//! OpenAI Responses API with SSE streaming (`POST {base_url}/responses`).
//!
//! Besides OpenAI itself this serves Codex relays such as AnyRouter, which
//! offer GPT models only on this endpoint and accept a request only if it is
//! shaped like Codex's: stateless (`store: false`), asking for encrypted
//! reasoning (`include`), with a `prompt_cache_key`. All three are standard
//! Responses fields. `include` and `prompt_cache_key` are dropped if an
//! endpoint rejects them, like the other optional parameters.
//!
//! Errors may arrive inside a stream that started with HTTP 200, for example
//! `{"type":"error","error":{"code":"rate_limit_exceeded",...}}` followed by
//! `response.failed`. They are mapped like HTTP errors, so a rate limit is
//! retried with backoff (or another key).

use biwrite_core::ContentHash;
use biwrite_engine::{PartialFn, TokenUsage, TranslateError};
use serde_json::{Value, json};

use crate::chat::ImageInput;
use crate::clean::ThinkFilter;
use crate::http::{Core, Flow, describe_stream_error, invalid, read_sse, stream_error_object};
use crate::keys::random_u64;
use crate::observe::{Declared, Tap, UsageDetail};
use crate::provider::{Body, Streamed, TRANSLATE, cut_off, filtered, refused};

/// Optional body parameters, with the words that identify a complaint about
/// them in a 400 message.
pub(crate) const DROPPABLE: &[(&str, &[&str])] = &[
    (
        "include",
        &["include", "encrypted_content", "encrypted content"],
    ),
    (
        "reasoning",
        &[
            "reasoning.effort",
            "reasoning effort",
            "'reasoning'",
            "\"reasoning\"",
        ],
    ),
    ("service_tier", &["service_tier", "service tier"]),
    ("prompt_cache_key", &["prompt_cache_key"]),
];

/// Relays such as AnyRouter route a request by its `prompt_cache_key`, and
/// a route whose upstream is rate-limited keeps failing. Translation prompts
/// are short (no cache to gain), so every translation takes a fresh route.
/// Assistant prompts carry long writing rules worth caching, so they keep
/// the key's route until that key fails (see `KeyPool::report`).
fn prompt_cache_key(core: &Core, purpose: &str, route: u64) -> String {
    let id = ContentHash::of_raw(core.config.id.as_bytes()).to_hex();
    let route = if purpose == TRANSLATE {
        random_u64()
    } else {
        route
    };
    format!("biwrite-{purpose}-{}-{:08x}", &id[..8], route as u32)
}

pub(crate) fn body(
    core: &Core,
    system: &str,
    user: &str,
    images: &[ImageInput],
    purpose: &str,
    route: u64,
) -> Body {
    let config = &core.config;
    let mut sent = Vec::new();
    let mut content = vec![json!({"type": "input_text", "text": user})];
    content.extend(
        images
            .iter()
            .map(|i| json!({"type": "input_image", "image_url": i.data_url()})),
    );
    let mut json = json!({
        "model": config.model,
        "instructions": system,
        "input": [{
            "type": "message",
            "role": "user",
            "content": content,
        }],
        "stream": true,
        "store": false,
    });
    if !core.is_dropped("include") {
        json["include"] = json!(["reasoning.encrypted_content"]);
        sent.push("include");
    }
    if !core.is_dropped("prompt_cache_key") {
        json["prompt_cache_key"] = json!(prompt_cache_key(core, purpose, route));
        sent.push("prompt_cache_key");
    }
    let effort = config
        .effort
        .as_param()
        .filter(|_| !core.is_dropped("reasoning"));
    if let Some(effort) = effort {
        json["reasoning"] = json!({"effort": effort});
        sent.push("reasoning");
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
            effort: effort.map(str::to_owned),
            service_tier: tier.map(str::to_owned),
        },
    }
}

/// What a `response` object declares about itself.
fn declare(response: &Value, tap: &mut Tap) {
    tap.declared(
        response.get("model").and_then(Value::as_str),
        response
            .pointer("/reasoning/effort")
            .and_then(Value::as_str),
        response.get("service_tier").and_then(Value::as_str),
    );
}

fn usage(response: &Value, total: &mut TokenUsage, tap: &mut Tap) {
    let u = &response["usage"];
    if !u.is_object() {
        return;
    }
    let detail = UsageDetail {
        input_tokens: u["input_tokens"].as_u64(),
        cached_tokens: u
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
        output_tokens: u["output_tokens"].as_u64(),
        reasoning_tokens: u
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
    };
    total.input_tokens = detail.input_tokens.unwrap_or(total.input_tokens);
    total.output_tokens = detail.output_tokens.unwrap_or(total.output_tokens);
    tap.usage(detail);
}

pub(crate) async fn read_stream(
    resp: reqwest::Response,
    key: &str,
    purpose: &str,
    tap: &mut Tap,
    on_partial: PartialFn<'_>,
) -> Result<Streamed, TranslateError> {
    let mut text = String::new();
    let mut total = TokenUsage::default();
    let mut filter = ThinkFilter::default();
    let mut visible = String::new();
    let mut refusal = false;
    // Set by the event that ends the response.
    let mut end: Option<Result<(), TranslateError>> = None;
    read_sse(resp, key, |event| {
        let v: Value = serde_json::from_str(&event.data)
            .map_err(|e| invalid(format!("bad stream data: {e}")))?;
        let kind = v
            .get("type")
            .and_then(Value::as_str)
            .or(event.event.as_deref())
            .unwrap_or("");
        match kind {
            "response.created" | "response.in_progress" => declare(&v["response"], tap),
            "response.output_text.delta" => {
                let delta = v["delta"].as_str().unwrap_or("");
                if !delta.is_empty() {
                    tap.first_token();
                }
                text.push_str(delta);
                if let Some(more) = filter.push(delta) {
                    visible.push_str(&more);
                    on_partial(&visible);
                }
            }
            "response.refusal.delta" | "response.refusal.done" => refusal = true,
            "response.completed" => {
                declare(&v["response"], tap);
                usage(&v["response"], &mut total, tap);
                end = Some(Ok(()));
                return Ok(Flow::Stop);
            }
            "response.incomplete" => {
                declare(&v["response"], tap);
                usage(&v["response"], &mut total, tap);
                let reason = v
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                end = Some(Err(match reason {
                    "max_output_tokens" => cut_off(purpose),
                    "content_filter" => filtered(purpose),
                    other => invalid(format!("the response is incomplete ({other})")),
                }));
                return Ok(Flow::Stop);
            }
            "response.failed" => {
                declare(&v["response"], tap);
                let err = &v["response"]["error"];
                return Err(if err.is_object() {
                    tap.note(describe_stream_error(err, key));
                    stream_error_object(err, key)
                } else {
                    TranslateError::Server {
                        status: 500,
                        retry_after: None,
                    }
                });
            }
            // OpenAI puts code and message on the event, relays nest them
            // under `error`.
            "error" => {
                let err = v.get("error").filter(|e| e.is_object()).unwrap_or(&v);
                tap.note(describe_stream_error(err, key));
                return Err(stream_error_object(err, key));
            }
            _ => {} // output items, content parts, reasoning summaries
        }
        Ok(Flow::Continue)
    })
    .await?;

    match end {
        None => Err(TranslateError::Network("the response ended early".into())),
        Some(Err(e)) => Err(e),
        Some(Ok(())) if refusal && text.trim().is_empty() => Err(refused(purpose)),
        Some(Ok(())) => Ok(Streamed { text, usage: total }),
    }
}
