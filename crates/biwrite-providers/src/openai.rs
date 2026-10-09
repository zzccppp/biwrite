//! OpenAI-compatible chat completions with SSE streaming
//! (OpenAI, DeepSeek, Qwen/DashScope, Kimi/Moonshot, OpenRouter, local servers).

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

/// Optional body parameters some compatible servers reject, with the words
/// that identify the complaint in a 400 message.
const DROPPABLE: &[(&str, &[&str])] = &[
    ("temperature", &["temperature"]),
    ("stream_options", &["stream_options", "include_usage"]),
];

pub struct OpenAiTranslator {
    core: Core,
    endpoint: String,
}

impl OpenAiTranslator {
    pub fn new(
        config: ProviderConfig,
        key: KeyFn,
        prompts: Arc<dyn PromptSource>,
    ) -> Result<Self, TranslateError> {
        let endpoint = format!("{}/chat/completions", config.base_url);
        Ok(Self {
            core: Core::new(config, key, prompts)?,
            endpoint,
        })
    }

    /// Request body and the optional parameters it includes.
    fn body(&self, m: &Messages) -> (Value, Vec<&'static str>) {
        let mut sent = Vec::new();
        let mut body = json!({
            "model": self.core.config.model,
            "messages": [
                {"role": "system", "content": m.system},
                {"role": "user", "content": m.user},
            ],
            "stream": true,
        });
        if !self.core.is_dropped("temperature") {
            body["temperature"] = json!(self.core.config.temperature);
            sent.push("temperature");
        }
        if !self.core.is_dropped("stream_options") {
            body["stream_options"] = json!({"include_usage": true});
            sent.push("stream_options");
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
            let request = self
                .core
                .client
                .post(&self.endpoint)
                .bearer_auth(&key)
                .header(ACCEPT, "text/event-stream")
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string());
            let resp = send(request, &key).await?;
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
    let mut finish: Option<String> = None;
    let mut done = false;
    read_sse(resp, key, |event| {
        let data = event.data.trim();
        if data == "[DONE]" {
            done = true;
            return Ok(Flow::Stop);
        }
        let v: Value =
            serde_json::from_str(data).map_err(|e| invalid(format!("bad stream data: {e}")))?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            return Err(stream_error(err, key));
        }
        let u = v
            .get("usage")
            .filter(|u| u.is_object())
            .or_else(|| v.pointer("/choices/0/usage"));
        if let Some(u) = u {
            usage.input_tokens = u["prompt_tokens"].as_u64().unwrap_or(usage.input_tokens);
            usage.output_tokens = u["completion_tokens"]
                .as_u64()
                .unwrap_or(usage.output_tokens);
        }
        if let Some(delta) = v
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            text.push_str(delta);
            if let Some(more) = filter.push(delta) {
                visible.push_str(&more);
                on_partial(&visible);
            }
        }
        if let Some(reason) = v
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            finish = Some(reason.to_owned());
        }
        Ok(Flow::Continue)
    })
    .await?;

    match finish.as_deref() {
        None if !done => Err(TranslateError::Network("the response ended early".into())),
        Some("length") => Err(invalid(
            "the translation was cut off (output limit reached)",
        )),
        Some("content_filter") => Err(TranslateError::Rejected {
            status: 200,
            message: "the provider's content filter blocked this paragraph".into(),
        }),
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

/// An error object sent inside the stream (e.g. OpenRouter upstream errors).
fn stream_error(err: &Value, key: &str) -> TranslateError {
    let message = redact(
        err.get("message")
            .and_then(Value::as_str)
            .unwrap_or("stream error"),
        key,
    );
    let code = err
        .get("code")
        .and_then(Value::as_u64)
        .and_then(|c| u16::try_from(c).ok());
    match code {
        Some(code) => status_error(code, message, None),
        None => TranslateError::Server {
            status: 500,
            retry_after: None,
        },
    }
}

impl Translator for OpenAiTranslator {
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
