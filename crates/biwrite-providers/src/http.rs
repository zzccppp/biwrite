//! Shared HTTP plumbing: client, error mapping with key redaction, and the
//! SSE read loop.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use biwrite_engine::TranslateError;
use reqwest::header::HeaderMap;
use reqwest::{RequestBuilder, Response};
use serde_json::Value;

use crate::config::{ProviderConfig, is_loopback};
use crate::keys::{KeyFn, KeyPool};
use crate::observe::RequestObserver;
use crate::prompt::PromptSource;
use crate::sse::{SseEvent, SseParser};

/// Time allowed to establish a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Time allowed until response headers arrive.
const HEADERS_TIMEOUT: Duration = Duration::from_secs(90);
/// Longest silence tolerated mid-stream.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Error bodies are read up to this size.
const MAX_ERROR_BODY: usize = 16 * 1024;

/// State shared by the HTTP providers.
pub(crate) struct Core {
    pub config: ProviderConfig,
    pub client: reqwest::Client,
    pub prompts: Arc<dyn PromptSource>,
    pub identity: String,
    pub keys: KeyPool,
    pub observer: Arc<dyn RequestObserver>,
    /// Optional parameters this endpoint rejected; they are not sent again.
    dropped: Mutex<HashSet<&'static str>>,
    /// Optional features this endpoint asked for; they are sent from then on.
    wanted: Mutex<HashSet<&'static str>>,
}

impl Core {
    pub fn new(
        config: ProviderConfig,
        keys: KeyFn,
        prompts: Arc<dyn PromptSource>,
        observer: Arc<dyn RequestObserver>,
    ) -> Result<Self, TranslateError> {
        Ok(Self {
            client: client(&config.base_url)?,
            identity: config.cache_identity(),
            keys: KeyPool::new(&config.name, keys, config.key_concurrency),
            config,
            prompts,
            observer,
            dropped: Mutex::new(HashSet::new()),
            wanted: Mutex::new(HashSet::new()),
        })
    }

    pub fn is_wanted(&self, feature: &str) -> bool {
        self.wanted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(feature)
    }

    /// If `err` says the endpoint needs one of the optional features
    /// `candidates` that this request did not send, send it with every
    /// later request and return its name so the caller retries with it.
    pub fn retry_with(
        &self,
        err: &TranslateError,
        candidates: &[(&'static str, &[&str])],
        sent: &[&'static str],
    ) -> Option<&'static str> {
        let TranslateError::Rejected {
            status: 400 | 422,
            message,
        } = err
        else {
            return None;
        };
        let lower = message.to_lowercase();
        let (feature, _) = candidates.iter().find(|(feature, needles)| {
            !sent.contains(feature) && needles.iter().any(|n| lower.contains(n))
        })?;
        self.wanted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(feature);
        Some(feature)
    }

    pub fn is_dropped(&self, param: &str) -> bool {
        self.dropped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(param)
    }

    /// If `err` says one of the optional parameters `sent` with this request
    /// is unsupported, drop it (for all later requests too) and return its
    /// name so the caller retries without it. Concurrent requests that sent
    /// the same parameter all retry, even if another one dropped it first.
    pub fn retry_without(
        &self,
        err: &TranslateError,
        candidates: &[(&'static str, &[&str])],
        sent: &[&'static str],
    ) -> Option<&'static str> {
        let TranslateError::Rejected {
            status: 400 | 422,
            message,
        } = err
        else {
            return None;
        };
        let lower = message.to_lowercase();
        let (param, _) = candidates.iter().find(|(param, needles)| {
            sent.contains(param) && needles.iter().any(|n| lower.contains(n))
        })?;
        self.dropped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(param);
        Some(param)
    }
}

/// HTTP client for a provider. Redirects are never followed: reqwest strips
/// only `Authorization` on cross-origin redirects, so following one could
/// forward Anthropic's `x-api-key` to another host.
pub(crate) fn client(base_url: &str) -> Result<reqwest::Client, TranslateError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("BiWrite/", env!("CARGO_PKG_VERSION")));
    if reqwest::Url::parse(base_url).is_ok_and(|u| is_loopback(&u)) {
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|e| TranslateError::Config(format!("could not create the HTTP client: {e}")))
}

/// Send a request, mapping transport failures.
pub(crate) async fn send(request: RequestBuilder, key: &str) -> Result<Response, TranslateError> {
    match tokio::time::timeout(HEADERS_TIMEOUT, request.send()).await {
        Err(_) => Err(TranslateError::Network(
            "no response from the provider".into(),
        )),
        Ok(Err(e)) => Err(network(&e, key)),
        Ok(Ok(resp)) => Ok(resp),
    }
}

pub(crate) fn network(e: &reqwest::Error, key: &str) -> TranslateError {
    let what = if e.is_timeout() {
        "timed out".to_owned()
    } else if e.is_connect() {
        format!("could not connect: {e}")
    } else {
        e.to_string()
    };
    TranslateError::Network(redact(&what, key))
}

/// Map a non-success response to an error. The body is read (bounded) for
/// the provider's message, which is redacted.
pub(crate) async fn error_from_response(mut resp: Response, key: &str) -> TranslateError {
    let status = resp.status().as_u16();
    let retry_after = retry_after(resp.headers());
    let mut body = Vec::new();
    while body.len() < MAX_ERROR_BODY {
        match tokio::time::timeout(IDLE_TIMEOUT, resp.chunk()).await {
            Ok(Ok(Some(chunk))) => body.extend_from_slice(&chunk),
            _ => break,
        }
    }
    let body = String::from_utf8_lossy(&body);
    body_error(status, &body, key, retry_after)
}

fn body_error(status: u16, body: &str, key: &str, retry_after: Option<Duration>) -> TranslateError {
    let message = redact(&error_message(body), key);
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        let err = value
            .get("error")
            .filter(|e| e.is_object())
            .unwrap_or(&value);
        return object_error(status, err, message, retry_after);
    }
    status_error(status, message, retry_after)
}

pub(crate) fn status_error(
    status: u16,
    message: String,
    retry_after: Option<Duration>,
) -> TranslateError {
    let lower = message.to_lowercase();
    match status {
        300..=399 => TranslateError::Rejected {
            status,
            message: "the provider answered with a redirect; check the base URL".into(),
        },
        401 | 403 => TranslateError::Rejected {
            status,
            message: format!("{message} — check the API key"),
        },
        402 => TranslateError::Rejected { status, message },
        // Exhausted balances are permanent even when a relay wraps them in 5xx.
        _ if exhausted_quota_message(&lower) => TranslateError::Rejected { status, message },
        429 => TranslateError::RateLimited {
            retry_after,
            message,
        },
        // Some relays wrap upstream token limits in a generic server error.
        408 | 409 | 500..=599 if temporary_rate_limit_message(&lower) => {
            TranslateError::RateLimited {
                retry_after,
                message,
            }
        }
        408 | 409 | 500..=599 => TranslateError::Server {
            status,
            retry_after,
            message,
        },
        _ => TranslateError::Rejected { status, message },
    }
}

fn exhausted_quota_message(lower: &str) -> bool {
    [
        "insufficient_quota",
        "exceeded your current quota",
        "insufficient balance",
        "insufficient credit",
        "余额不足",
        "余额已用尽",
        "额度已用尽",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}

fn temporary_rate_limit_message(lower: &str) -> bool {
    lower == "rate limit exceeded"
        || lower.starts_with("rate limit exceeded:")
        || lower.starts_with("rate_limit_exceeded:")
        || lower.contains("have exceeded token rate limit")
}

/// Preserve upstream business errors when a relay supplies a generic status.
/// Authentication and exhausted balances take precedence over rate limits.
fn object_error(
    status: u16,
    err: &Value,
    message: String,
    retry_after: Option<Duration>,
) -> TranslateError {
    if matches!(status, 300..=399 | 401..=403) {
        return status_error(status, message, retry_after);
    }
    let code = err.get("code").and_then(Value::as_str).unwrap_or("");
    let kind = err.get("type").and_then(Value::as_str).unwrap_or("");
    let is = |names: &[&str]| names.contains(&code) || names.contains(&kind);
    if is(&["insufficient_quota"]) {
        return TranslateError::Rejected {
            status: 429,
            message,
        };
    }
    let status = if is(&["invalid_api_key", "authentication_error", "unauthorized"]) {
        401
    } else if is(&["permission_error", "permission_denied"]) {
        403
    } else if is(&[
        "billing_error",
        "insufficient_balance",
        "insufficient_credit",
    ]) {
        402
    } else if is(&[
        "rate_limit_exceeded",
        "rate_limit_error",
        "too_many_requests",
    ]) || (matches!(status, 408 | 409 | 500..=599)
        && temporary_rate_limit_message(&message.to_lowercase()))
    {
        429
    } else if is(&[
        "invalid_prompt",
        "invalid_request_error",
        "context_length_exceeded",
    ]) {
        400
    } else {
        status
    };
    status_error(status, message, retry_after)
}

/// A short description of an in-stream error object for the request log:
/// its code or type and its message, redacted.
pub(crate) fn describe_stream_error(err: &Value, key: &str) -> String {
    let field = |name: &str| match err.get(name) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    let code = field("code").or_else(|| field("type")).unwrap_or_default();
    let message = field("message").unwrap_or_default();
    truncate(
        &redact(&format!("stream error {code}: {message}"), key),
        300,
    )
}

/// An error object sent inside a stream (OpenAI style, also used by relays
/// and OpenRouter): a numeric HTTP-like `code`, or a symbolic `code` or
/// `type` such as `rate_limit_exceeded`. Unknown errors count as transient.
pub(crate) fn stream_error_object(err: &Value, key: &str) -> TranslateError {
    let message = redact(
        err.get("message")
            .and_then(Value::as_str)
            .unwrap_or("stream error"),
        key,
    );
    let status = err
        .get("code")
        .and_then(Value::as_u64)
        .and_then(|c| u16::try_from(c).ok())
        .unwrap_or(500);
    object_error(status, err, message, None)
}

/// The provider's error message from a JSON error body, else the raw body.
pub(crate) fn error_message(body: &str) -> String {
    let from_json = serde_json::from_str::<Value>(body).ok().and_then(|v| {
        let candidates = [
            v.pointer("/error/message"),
            v.get("message"),
            v.get("error").filter(|e| e.is_string()),
            v.get("detail"),
        ];
        candidates
            .into_iter()
            .flatten()
            .find_map(|m| m.as_str().map(str::to_owned))
    });
    let text = from_json.unwrap_or_else(|| body.trim().to_owned());
    let text = if text.is_empty() {
        "no details".to_owned()
    } else {
        text
    };
    truncate(&text, 300)
}

fn truncate(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", &s[..cut]),
        None => s.to_owned(),
    }
}

/// `retry-after` (seconds) or `retry-after-ms`.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    if let Some(ms) = get("retry-after-ms").and_then(|v| v.parse::<f64>().ok()) {
        return Some(Duration::from_millis(ms.max(0.0) as u64));
    }
    get("retry-after")
        .and_then(|v| v.parse::<f64>().ok())
        .map(|s| Duration::from_millis((s.max(0.0) * 1000.0) as u64))
}

/// Remove the API key (and anything that looks like a secret key) from text
/// that may reach the UI or logs.
pub fn redact(text: &str, key: &str) -> String {
    let text = if key.len() >= 8 {
        text.replace(key, "[redacted]")
    } else {
        text.to_owned()
    };
    redact_key_like(&text)
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Mask key-shaped tokens: `sk-…` / `sk_…` with at least 16 more characters.
fn redact_key_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut prev: Option<char> = None;
    while let Some(c) = text[i..].chars().next() {
        let rest = &text[i..];
        if (rest.starts_with("sk-") || rest.starts_with("sk_")) && !prev.is_some_and(is_key_char) {
            let run = rest
                .char_indices()
                .find(|(_, c)| !is_key_char(*c))
                .map_or(rest.len(), |(j, _)| j);
            if run >= 19 {
                out.push_str("[redacted]");
                i += run;
                prev = Some('x');
                continue;
            }
        }
        out.push(c);
        i += c.len_utf8();
        prev = Some(c);
    }
    out
}

pub(crate) enum Flow {
    Continue,
    Stop,
}

/// Read an SSE response, calling `handle` per event until it returns
/// [`Flow::Stop`] or the stream ends.
pub(crate) async fn read_sse(
    mut resp: Response,
    key: &str,
    mut handle: impl FnMut(SseEvent) -> Result<Flow, TranslateError>,
) -> Result<(), TranslateError> {
    let mut parser = SseParser::default();
    loop {
        let chunk = match tokio::time::timeout(IDLE_TIMEOUT, resp.chunk()).await {
            Err(_) => {
                return Err(TranslateError::Network(
                    "the provider stopped responding".into(),
                ));
            }
            Ok(Err(e)) => return Err(network(&e, key)),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(bytes))) => bytes,
        };
        for event in parser.feed(&chunk) {
            if let Flow::Stop = handle(event)? {
                return Ok(());
            }
        }
    }
    parser.finish();
    Ok(())
}

pub(crate) fn invalid(what: impl std::fmt::Display) -> TranslateError {
    TranslateError::InvalidResponse(what.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction() {
        let key = "sk-proj-AbCdEf0123456789xyz";
        assert_eq!(
            redact(&format!("bad key {key}!"), key),
            "bad key [redacted]!"
        );
        assert_eq!(
            redact(
                "Incorrect API key provided: sk-abcdefghijklmnopqrstuvwxyz",
                "other-key"
            ),
            "Incorrect API key provided: [redacted]"
        );
        assert_eq!(redact("ask skills sk- desk", "k"), "ask skills sk- desk");
        assert_eq!(redact("数据 sk", "k"), "数据 sk");
    }

    #[test]
    fn error_messages_from_json_bodies() {
        assert_eq!(
            error_message(r#"{"error":{"message":"bad model","type":"x"}}"#),
            "bad model"
        );
        assert_eq!(
            error_message(
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            ),
            "Overloaded"
        );
        assert_eq!(
            error_message("<html>gateway</html>"),
            "<html>gateway</html>"
        );
        assert_eq!(error_message(""), "no details");
        assert_eq!(error_message(&"x".repeat(400)).chars().count(), 301);
    }

    #[test]
    fn status_mapping() {
        assert!(matches!(
            status_error(429, "slow down".into(), None),
            TranslateError::RateLimited { .. }
        ));
        assert!(matches!(
            status_error(429, "insufficient_quota".into(), None),
            TranslateError::Rejected { .. }
        ));
        assert!(matches!(
            status_error(529, "overloaded".into(), None),
            TranslateError::Server { status: 529, .. }
        ));
        // A relay's words on a full upstream stay with the error.
        let full = error_message(
            r#"{"error":{"message":"当前模型 m 负载已经达到上限，请稍后重试 (request id: 1)","type":"new_api_error","code":"get_channel_failed"}}"#,
        );
        assert_eq!(
            status_error(500, full, None).to_string(),
            "provider server error (HTTP 500): 当前模型 m 负载已经达到上限，请稍后重试 (request id: 1)"
        );
        assert_eq!(
            status_error(429, "slow down".into(), None).to_string(),
            "rate limited by the provider: slow down"
        );
        assert!(matches!(
            status_error(400, "bad".into(), None),
            TranslateError::Rejected { status: 400, .. }
        ));
        let auth = status_error(401, "invalid x-api-key".into(), None);
        assert!(auth.to_string().contains("check the API key"));
    }

    const TOKEN_LIMIT: &str = "rate limit exceeded: Your requests to gpt-6-astra for gpt-6-astra in eastus2 have exceeded token rate limit.";

    #[test]
    fn a_server_wrapper_preserves_the_upstream_rate_limit_and_retry_hint() {
        for body in [
            TOKEN_LIMIT.to_owned(),
            serde_json::json!({"error": {"code": "rate_limit_exceeded", "message": "slow down"}})
                .to_string(),
            serde_json::json!({"type": "rate_limit_error", "message": "slow down"}).to_string(),
            serde_json::json!({"error": {"code": "server_error", "message": TOKEN_LIMIT}})
                .to_string(),
            serde_json::json!({"error": {"type": "invalid_request_error", "message": TOKEN_LIMIT}})
                .to_string(),
        ] {
            let err = body_error(500, &body, "", Some(Duration::from_secs(13)));
            assert!(
                matches!(err, TranslateError::RateLimited { retry_after: Some(delay), .. } if delay == Duration::from_secs(13)),
                "{err:?}"
            );
        }
    }

    #[test]
    fn stream_numeric_and_generic_errors_preserve_an_explicit_token_rate_limit() {
        for err in [
            serde_json::json!({"code": 500, "message": TOKEN_LIMIT}),
            serde_json::json!({"code": 500, "type": "rate_limit_exceeded", "message": "slow down"}),
            serde_json::json!({"code": "server_error", "message": TOKEN_LIMIT}),
            serde_json::json!({"message": TOKEN_LIMIT}),
        ] {
            let mapped = stream_error_object(&err, "");
            assert!(
                matches!(mapped, TranslateError::RateLimited { .. }),
                "{mapped:?}"
            );
        }
    }

    #[test]
    fn genuine_server_errors_do_not_become_rate_limits_from_loose_keywords() {
        for message in [
            "internal server error",
            "rate limit service unavailable",
            "token limit configuration failed",
            "current model load has reached its limit",
            "当前模型负载已经达到上限，请稍后重试",
        ] {
            let body = serde_json::json!({"error": {"code": "server_error", "message": message}})
                .to_string();
            assert!(matches!(
                body_error(500, &body, "", None),
                TranslateError::Server { status: 500, .. }
            ));
        }
    }

    #[test]
    fn exhausted_balances_and_refused_keys_take_precedence_over_rate_limits() {
        for code in [
            "insufficient_quota",
            "invalid_api_key",
            "permission_denied",
            "billing_error",
        ] {
            let err = serde_json::json!({"code": code, "type": "rate_limit_error", "message": TOKEN_LIMIT});
            let body = serde_json::json!({"error": err}).to_string();
            assert!(matches!(
                body_error(500, &body, "", None),
                TranslateError::Rejected { .. }
            ));
            assert!(matches!(
                stream_error_object(&err, ""),
                TranslateError::Rejected { .. }
            ));
        }
        for message in [
            "insufficient balance",
            "You exceeded your current quota",
            "余额不足",
        ] {
            let err = serde_json::json!({"code": "rate_limit_exceeded", "message": message});
            let body = serde_json::json!({"error": err}).to_string();
            assert!(matches!(
                body_error(500, &body, "", None),
                TranslateError::Rejected { .. }
            ));
            assert!(matches!(
                stream_error_object(&err, ""),
                TranslateError::Rejected { .. }
            ));
        }
        for status in [401, 402, 403] {
            let err = serde_json::json!({"code": status, "type": "rate_limit_error", "message": TOKEN_LIMIT});
            assert!(matches!(
                stream_error_object(&err, ""),
                TranslateError::Rejected { .. }
            ));
            let body = serde_json::json!({"error": {"code": "rate_limit_exceeded", "message": TOKEN_LIMIT}}).to_string();
            assert!(matches!(
                body_error(status, &body, "", None),
                TranslateError::Rejected { .. }
            ));
        }
    }
}
