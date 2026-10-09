//! Anthropic Messages API with SSE streaming (raw HTTP; there is no official
//! Rust SDK).
//!
//! Current Claude models reject sampling parameters, so `temperature` is only
//! sent to older models; `output_config.effort` defaults to `low` (translation
//! is a simple task). On the first-party API, Claude Opus 5.5 / Opus 5 /
//! Fable 5.1 / Sonnet 5.5 requests opt into server-side refusal fallbacks.

use biwrite_engine::{PartialFn, TokenUsage, TranslateError};
use reqwest::RequestBuilder;
use serde_json::{Value, json};

use crate::chat::ImageInput;
use crate::clean::ThinkFilter;
use crate::http::{Core, Flow, invalid, read_sse, redact, status_error};
use crate::observe::{Declared, Tap, UsageDetail};
use crate::provider::{Body, Streamed, cut_off, refused};

pub const API_VERSION: &str = "2023-06-01";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Output ceiling per request (includes thinking tokens on adaptive models).
const MAX_TOKENS: u32 = 32_000;

/// Model families that reject `temperature` (sampling parameters removed).
const NO_SAMPLING_PREFIXES: &[&str] = &[
    "claude-opus-5",
    "claude-sonnet-5",
    "claude-haiku-5",
    "claude-fable",
    "claude-mythos",
    "claude-opus-4-7",
    "claude-opus-4-8",
];

/// Models that accept `fallbacks: "default"` on the Claude API.
const FALLBACK_MODELS: &[&str] = &[
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-fable-5-1",
    "claude-sonnet-5-5",
];

pub(crate) const DROPPABLE: &[(&str, &[&str])] = &[
    ("temperature", &["temperature"]),
    ("effort", &["effort", "output_config"]),
    ("fallbacks", &["fallback", "anthropic-beta"]),
];

pub(crate) fn sends_temperature(model: &str) -> bool {
    !NO_SAMPLING_PREFIXES.iter().any(|p| model.starts_with(p))
}

pub(crate) fn uses_fallbacks(model: &str, base_url: &str) -> bool {
    let first_party =
        reqwest::Url::parse(base_url).is_ok_and(|u| u.host_str() == Some("api.anthropic.com"));
    first_party && FALLBACK_MODELS.contains(&model)
}

/// Request body and the optional parameters it includes.
pub(crate) fn body(core: &Core, system: &str, user: &str, images: &[ImageInput]) -> Body {
    let config = &core.config;
    let mut sent = Vec::new();
    // Images go before the text, as the Messages API recommends.
    let content = if images.is_empty() {
        json!(user)
    } else {
        let mut parts: Vec<Value> = images
            .iter()
            .map(|i| {
                json!({"type": "image", "source": {"type": "base64", "media_type": i.media_type, "data": i.data}})
            })
            .collect();
        parts.push(json!({"type": "text", "text": user}));
        json!(parts)
    };
    let mut json = json!({
        "model": config.model,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "messages": [{"role": "user", "content": content}],
        "stream": true,
    });
    if sends_temperature(&config.model) && !core.is_dropped("temperature") {
        json["temperature"] = json!(config.temperature);
        sent.push("temperature");
    }
    let effort = config
        .effort
        .as_param()
        .filter(|_| !core.is_dropped("effort"));
    if let Some(effort) = effort {
        json["output_config"] = json!({"effort": effort});
        sent.push("effort");
    }
    if !core.is_dropped("fallbacks") && uses_fallbacks(&config.model, &config.base_url) {
        json["fallbacks"] = json!("default");
        sent.push("fallbacks");
    }
    Body {
        json,
        sent,
        declared: Declared {
            model: Some(config.model.clone()),
            effort: effort.map(str::to_owned),
            service_tier: None,
        },
    }
}

/// Authentication and version headers (raw HTTP: there is no official Rust SDK).
pub(crate) fn headers(request: RequestBuilder, key: &str, sent: &[&'static str]) -> RequestBuilder {
    let request = request
        .header("x-api-key", key)
        .header("anthropic-version", API_VERSION);
    if sent.contains(&"fallbacks") {
        request.header("anthropic-beta", FALLBACK_BETA)
    } else {
        request
    }
}

pub(crate) async fn read_stream(
    resp: reqwest::Response,
    key: &str,
    purpose: &str,
    tap: &mut Tap,
    on_partial: PartialFn<'_>,
) -> Result<Streamed, TranslateError> {
    let mut text = String::new();
    let mut usage = TokenUsage::default();
    let mut filter = ThinkFilter::default();
    let mut visible = String::new();
    let mut stop_reason: Option<String> = None;
    let mut done = false;
    read_sse(resp, key, |event| {
        let v: Value = serde_json::from_str(&event.data)
            .map_err(|e| invalid(format!("bad stream data: {e}")))?;
        let kind = v
            .get("type")
            .and_then(Value::as_str)
            .or(event.event.as_deref())
            .unwrap_or("");
        match kind {
            "message_start" => {
                let u = &v["message"]["usage"];
                usage.input_tokens = [
                    "input_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                ]
                .iter()
                .filter_map(|f| u[*f].as_u64())
                .sum();
                usage.output_tokens = u["output_tokens"].as_u64().unwrap_or(0);
                tap.declared(
                    v.pointer("/message/model").and_then(Value::as_str),
                    None,
                    None,
                );
                tap.usage(UsageDetail {
                    input_tokens: Some(usage.input_tokens),
                    cached_tokens: u["cache_read_input_tokens"].as_u64(),
                    ..Default::default()
                });
            }
            "content_block_start" => {
                // A fallback block marks a switch to another model after a
                // refusal: text before it belongs to the declined attempt.
                if v.pointer("/content_block/type").and_then(Value::as_str) == Some("fallback") {
                    text.clear();
                    visible.clear();
                    filter = ThinkFilter::default();
                    on_partial("");
                    tap.note("server-side fallback to another model");
                    tap.declared(
                        v.pointer("/content_block/to/model").and_then(Value::as_str),
                        None,
                        None,
                    );
                }
            }
            "content_block_delta" => {
                if v.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta") {
                    let delta = v
                        .pointer("/delta/text")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !delta.is_empty() {
                        tap.first_token();
                    }
                    text.push_str(delta);
                    if let Some(more) = filter.push(delta) {
                        visible.push_str(&more);
                        on_partial(&visible);
                    }
                }
            }
            "message_delta" => {
                if let Some(reason) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    stop_reason = Some(reason.to_owned());
                }
                if let Some(out) = v.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    usage.output_tokens = out;
                    tap.usage(UsageDetail {
                        output_tokens: Some(out),
                        ..Default::default()
                    });
                }
            }
            "message_stop" => {
                done = true;
                return Ok(Flow::Stop);
            }
            "error" => return Err(stream_error(&v["error"], key)),
            _ => {} // ping, content_block_stop, thinking deltas
        }
        Ok(Flow::Continue)
    })
    .await?;

    if !done {
        return Err(TranslateError::Network("the response ended early".into()));
    }
    match stop_reason.as_deref() {
        Some("refusal") => Err(refused(purpose)),
        Some("max_tokens") => Err(cut_off(purpose)),
        _ => Ok(Streamed { text, usage }),
    }
}

/// An `event: error` inside the stream.
fn stream_error(err: &Value, key: &str) -> TranslateError {
    let message = redact(err["message"].as_str().unwrap_or("stream error"), key);
    let status = match err["type"].as_str() {
        Some("overloaded_error") => 529,
        Some("rate_limit_error") => 429,
        Some("authentication_error") => 401,
        Some("permission_error") => 403,
        Some("invalid_request_error") => 400,
        Some("billing_error") => 402,
        Some("not_found_error") => 404,
        Some("request_too_large") => 413,
        // api_error, timeout_error and anything new: transient, retry.
        _ => 500,
    };
    status_error(status, message, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_and_fallback_rules() {
        assert!(!sends_temperature("claude-opus-5-5"));
        assert!(!sends_temperature("claude-haiku-5-5"));
        assert!(!sends_temperature("claude-sonnet-5"));
        assert!(sends_temperature("claude-haiku-4-5"));
        assert!(sends_temperature("claude-sonnet-4-6"));
        assert!(uses_fallbacks(
            "claude-opus-5-5",
            "https://api.anthropic.com"
        ));
        assert!(!uses_fallbacks(
            "claude-haiku-5-5",
            "https://api.anthropic.com"
        ));
        assert!(!uses_fallbacks(
            "claude-opus-5-5",
            "https://proxy.example.com"
        ));
    }
}
