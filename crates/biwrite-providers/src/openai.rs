//! OpenAI-compatible chat completions with SSE streaming
//! (OpenAI, DeepSeek, Qwen/DashScope, Kimi/Moonshot, OpenRouter, local servers).

use biwrite_engine::{PartialFn, TokenUsage, TranslateError};
use serde_json::{Value, json};

use crate::chat::ImageInput;
use crate::clean::ThinkFilter;
use crate::http::{Core, Flow, describe_stream_error, invalid, read_sse, stream_error_object};
use crate::observe::{Declared, Tap, UsageDetail};
use crate::provider::{Body, Streamed, cut_off, filtered};

/// Optional body parameters some compatible servers reject, with the words
/// that identify the complaint in a 400 message.
pub(crate) const DROPPABLE: &[(&str, &[&str])] = &[
    ("temperature", &["temperature"]),
    ("stream_options", &["stream_options", "include_usage"]),
    ("service_tier", &["service_tier", "service tier"]),
];

/// Request body and the optional parameters it includes.
pub(crate) fn body(core: &Core, system: &str, user: &str, images: &[ImageInput]) -> Body {
    let config = &core.config;
    let mut sent = Vec::new();
    // Plain text stays a string (what every compatible server accepts);
    // images need the content-part form.
    let user_content = if images.is_empty() {
        json!(user)
    } else {
        let mut parts = vec![json!({"type": "text", "text": user})];
        parts.extend(
            images
                .iter()
                .map(|i| json!({"type": "image_url", "image_url": {"url": i.data_url()}})),
        );
        json!(parts)
    };
    let mut json = json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user_content},
        ],
        "stream": true,
    });
    if !core.is_dropped("temperature") {
        json["temperature"] = json!(config.temperature);
        sent.push("temperature");
    }
    if !core.is_dropped("stream_options") {
        json["stream_options"] = json!({"include_usage": true});
        sent.push("stream_options");
    }
    let tier = config
        .service_tier
        .filter(|_| !core.is_dropped("service_tier"))
        .map(|t| t.as_param());
    if let Some(tier) = tier {
        json["service_tier"] = json!(tier);
        sent.push("service_tier");
    }
    Body {
        json,
        sent,
        declared: Declared {
            model: Some(config.model.clone()),
            effort: None,
            service_tier: tier.map(str::to_owned),
        },
    }
}

fn usage_detail(u: &Value) -> UsageDetail {
    UsageDetail {
        input_tokens: u["prompt_tokens"].as_u64(),
        cached_tokens: u
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
        output_tokens: u["completion_tokens"].as_u64(),
        reasoning_tokens: u
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
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
            tap.note(describe_stream_error(err, key));
            return Err(stream_error_object(err, key));
        }
        tap.declared(
            v.get("model").and_then(Value::as_str),
            None,
            v.get("service_tier").and_then(Value::as_str),
        );
        let u = v
            .get("usage")
            .filter(|u| u.is_object())
            .or_else(|| v.pointer("/choices/0/usage"));
        if let Some(u) = u {
            usage.input_tokens = u["prompt_tokens"].as_u64().unwrap_or(usage.input_tokens);
            usage.output_tokens = u["completion_tokens"]
                .as_u64()
                .unwrap_or(usage.output_tokens);
            tap.usage(usage_detail(u));
        }
        if let Some(delta) = v
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            if !delta.is_empty() {
                tap.first_token();
            }
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
        Some("length") => Err(cut_off(purpose)),
        Some("content_filter") => Err(filtered(purpose)),
        _ => Ok(Streamed { text, usage }),
    }
}
