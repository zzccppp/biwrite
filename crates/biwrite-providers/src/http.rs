//! Shared HTTP plumbing: client, lazily fetched API key, error mapping with
//! key redaction, and the SSE read loop.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use biwrite_engine::TranslateError;
use reqwest::header::HeaderMap;
use reqwest::{RequestBuilder, Response};
use serde_json::Value;

use crate::config::{ProviderConfig, is_loopback};
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

/// Fetches the API key (from the OS keychain in the app). Called at most
/// once per translator, lazily, on a blocking thread.
pub type KeyFn = Arc<dyn Fn() -> Result<Option<String>, String> + Send + Sync>;

/// State shared by the HTTP providers.
pub(crate) struct Core {
    pub config: ProviderConfig,
    pub client: reqwest::Client,
    pub prompts: Arc<dyn PromptSource>,
    pub identity: String,
    key_fn: KeyFn,
    /// The key, or the failure to read it (remembered so a denied keychain
    /// prompt isn't shown again for every paragraph; a new translator is
    /// built when the key changes).
    key: tokio::sync::Mutex<Option<Result<String, TranslateError>>>,
    /// Optional parameters this endpoint rejected; they are not sent again.
    dropped: Mutex<HashSet<&'static str>>,
}

impl Core {
    pub fn new(
        config: ProviderConfig,
        key_fn: KeyFn,
        prompts: Arc<dyn PromptSource>,
    ) -> Result<Self, TranslateError> {
        Ok(Self {
            client: client(&config.base_url)?,
            identity: config.cache_identity(),
            config,
            prompts,
            key_fn,
            key: tokio::sync::Mutex::new(None),
            dropped: Mutex::new(HashSet::new()),
        })
    }

    /// The API key, read from the key source on first use.
    pub async fn key(&self) -> Result<String, TranslateError> {
        let mut cached = self.key.lock().await;
        if let Some(result) = cached.as_ref() {
            return result.clone();
        }
        let fetch = Arc::clone(&self.key_fn);
        let fetched = tokio::task::spawn_blocking(move || fetch())
            .await
            .map_err(|e| TranslateError::Config(format!("reading the API key failed: {e}")))?;
        let name = &self.config.name;
        let result = match fetched {
            Ok(Some(key)) if !key.trim().is_empty() => Ok(key.trim().to_owned()),
            Ok(_) => Err(TranslateError::Config(format!(
                "No API key for “{name}”. Add one in Settings."
            ))),
            Err(e) => Err(TranslateError::Config(format!(
                "Could not read the API key for “{name}”: {e}"
            ))),
        };
        *cached = Some(result.clone());
        result
    }

    pub fn is_dropped(&self, param: &str) -> bool {
        self.dropped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(param)
    }

    /// If `err` says one of the optional parameters `sent` with this request
    /// is unsupported, drop it (for all later requests too) and return `true`
    /// so the caller retries without it. Concurrent requests that sent the
    /// same parameter all retry, even if another one dropped it first.
    pub fn retry_without(
        &self,
        err: &TranslateError,
        candidates: &[(&'static str, &[&str])],
        sent: &[&'static str],
    ) -> bool {
        let TranslateError::Rejected {
            status: 400 | 422,
            message,
        } = err
        else {
            return false;
        };
        let lower = message.to_lowercase();
        let rejected = candidates.iter().find(|(param, needles)| {
            sent.contains(param) && needles.iter().any(|n| lower.contains(n))
        });
        match rejected {
            Some((param, _)) => {
                self.dropped
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(param);
                true
            }
            None => false,
        }
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
    let message = redact(&error_message(&body), key);
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
        // OpenAI reports an exhausted balance as 429; retrying won't help.
        429 if lower.contains("insufficient_quota") || lower.contains("current quota") => {
            TranslateError::Rejected { status, message }
        }
        429 => TranslateError::RateLimited { retry_after },
        408 | 409 | 500..=599 => TranslateError::Server {
            status,
            retry_after,
        },
        401 | 403 => TranslateError::Rejected {
            status,
            message: format!("{message} — check the API key"),
        },
        _ => TranslateError::Rejected { status, message },
    }
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
        assert!(matches!(
            status_error(400, "bad".into(), None),
            TranslateError::Rejected { status: 400, .. }
        ));
        let auth = status_error(401, "invalid x-api-key".into(), None);
        assert!(auth.to_string().contains("check the API key"));
    }
}
