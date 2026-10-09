//! Anthropic Messages API with SSE streaming (raw HTTP; there is no official
//! Rust SDK).
//!
//! Current Claude models reject sampling parameters, so `temperature` is only
//! sent to older models; `output_config.effort` defaults to `low` (translation
//! is a simple task). On the first-party API, Claude Opus 5.5 / Opus 5 /
//! Fable 5.1 / Sonnet 5.5 requests opt into server-side refusal fallbacks.

use std::sync::Arc;

use biwrite_engine::{
    BoxFuture, PartialFn, TokenUsage, TranslateError, TranslationOutput, TranslationRequest,
    Translator,
};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::{Value, json};

use crate::clean::{ThinkFilter, clean_output};
use crate::config::ProviderConfig;
use crate::http::{
    Core, Flow, KeyFn, error_from_response, invalid, read_sse, redact, send, status_error,
};
use crate::prompt::{self, Messages, PromptSource};

pub const API_VERSION: &str = "2023-06-01";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Output ceiling per paragraph (includes thinking tokens on adaptive models).
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

const DROPPABLE: &[(&str, &[&str])] = &[
    ("temperature", &["temperature"]),
    ("effort", &["effort", "output_config"]),
    ("fallbacks", &["fallback", "anthropic-beta"]),
];

pub struct AnthropicTranslator {
    core: Core,
    endpoint: String,
}

pub(crate) fn sends_temperature(model: &str) -> bool {
    !NO_SAMPLING_PREFIXES.iter().any(|p| model.starts_with(p))
}

pub(crate) fn uses_fallbacks(model: &str, base_url: &str) -> bool {
    let first_party =
        reqwest::Url::parse(base_url).is_ok_and(|u| u.host_str() == Some("api.anthropic.com"));
    first_party && FALLBACK_MODELS.contains(&model)
}

impl AnthropicTranslator {
    pub fn new(
        config: ProviderConfig,
        key: KeyFn,
        prompts: Arc<dyn PromptSource>,
    ) -> Result<Self, TranslateError> {
        let endpoint = format!("{}/v1/messages", config.base_url);
        Ok(Self {
            core: Core::new(config, key, prompts)?,
            endpoint,
        })
    }

    fn fallbacks(&self) -> bool {
        !self.core.is_dropped("fallbacks")
            && uses_fallbacks(&self.core.config.model, &self.core.config.base_url)
    }

    /// Request body and the optional parameters it includes.
    fn body(&self, m: &Messages) -> (Value, Vec<&'static str>) {
        let config = &self.core.config;
        let mut sent = Vec::new();
        let mut body = json!({
            "model": config.model,
            "max_tokens": MAX_TOKENS,
            "system": m.system,
            "messages": [{"role": "user", "content": m.user}],
            "stream": true,
        });
        if sends_temperature(&config.model) && !self.core.is_dropped("temperature") {
            body["temperature"] = json!(config.temperature);
            sent.push("temperature");
        }
        if let Some(effort) = config
            .effort
            .as_param()
            .filter(|_| !self.core.is_dropped("effort"))
        {
            body["output_config"] = json!({"effort": effort});
            sent.push("effort");
        }
        if self.fallbacks() {
            body["fallbacks"] = json!("default");
            sent.push("fallbacks");
        }
        (body, sent)
    }

    async fn run(
        &self,
        req: &TranslationRequest,
        on_partial: PartialFn<'_>,
    ) -> Result<TranslationOutput, TranslateError> {
        let key = self.core.key().await?;
        let messages = prompt::build(req, self.core.prompts.system_prompt(req.direction));
        for _ in 0..=DROPPABLE.len() {
            let (body, sent) = self.body(&messages);
            let mut request = self
                .core
                .client
                .post(&self.endpoint)
                .header("x-api-key", &key)
                .header("anthropic-version", API_VERSION)
                .header(ACCEPT, "text/event-stream")
                .header(CONTENT_TYPE, "application/json");
            if sent.contains(&"fallbacks") {
                request = request.header("anthropic-beta", FALLBACK_BETA);
            }
            let resp = send(request.body(body.to_string()), &key).await?;
            if !resp.status().is_success() {
                let err = error_from_response(resp, &key).await;
                if self.core.retry_without(&err, DROPPABLE, &sent) {
                    continue;
                }
                return Err(err);
            }
            return read_stream(resp, req, on_partial, &key).await;
        }
        Err(invalid("the provider kept rejecting the request"))
    }
}

async fn read_stream(
    resp: reqwest::Response,
    req: &TranslationRequest,
    on_partial: PartialFn<'_>,
    key: &str,
) -> Result<TranslationOutput, TranslateError> {
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
            }
            "content_block_start" => {
                // A fallback block marks a switch to another model after a
                // refusal: text before it belongs to the declined attempt.
                if v.pointer("/content_block/type").and_then(Value::as_str) == Some("fallback") {
                    text.clear();
                    visible.clear();
                    filter = ThinkFilter::default();
                    on_partial("");
                }
            }
            "content_block_delta" => {
                if v.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta") {
                    let delta = v
                        .pointer("/delta/text")
                        .and_then(Value::as_str)
                        .unwrap_or("");
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
        Some("refusal") => Err(TranslateError::Rejected {
            status: 200,
            message: "the model declined to translate this paragraph".into(),
        }),
        Some("max_tokens") => Err(invalid(
            "the translation was cut off (output limit reached)",
        )),
        _ => {
            let cleaned = clean_output(&text, &req.source);
            if cleaned.is_empty() {
                return Err(invalid("the model returned no translation"));
            }
            Ok(TranslationOutput {
                text: cleaned,
                usage,
            })
        }
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

impl Translator for AnthropicTranslator {
    fn provider(&self) -> &str {
        &self.core.identity
    }

    fn model(&self) -> &str {
        &self.core.config.model
    }

    fn translate<'a>(
        &'a self,
        request: &'a TranslationRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<TranslationOutput, TranslateError>> {
        Box::pin(self.run(request, on_partial))
    }
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
