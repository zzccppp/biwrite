//! The HTTP provider shared by all three wire formats (OpenAI chat
//! completions, OpenAI Responses, Anthropic Messages).
//!
//! One exchange sends a system and a user text and streams the answer back.
//! It picks a key from the pool, records the request for the request log,
//! drops optional parameters the endpoint rejects, and moves on to another
//! key when the failure was the key's (rate limit, invalid, out of quota).
//! Translation and the writing assistant both go through it.

use std::sync::Arc;

use biwrite_engine::{
    BoxFuture, PartialFn, TokenUsage, TranslateError, TranslationOutput, TranslationRequest,
    Translator,
};
use reqwest::RequestBuilder;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::Value;

use crate::chat::{ChatModel, ChatOutput, ChatRequest};
use crate::clean::{clean_output, strip_reasoning};
use crate::config::{ProviderConfig, ProviderKind, WireApi};
use crate::http::{Core, error_from_response, invalid, send};
use crate::keys::{KeyFn, KeyStatus, Lease};
use crate::observe::{Declared, RequestObserver, RequestRecord, Tap, endpoint_label};
use crate::prompt::{self, PromptSource};
use crate::{anthropic, openai, responses};

/// Purpose of translation requests (part of every record).
pub const TRANSLATE: &str = "translate";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wire {
    Chat,
    Responses,
    Anthropic,
}

/// A request body and what it declares.
pub(crate) struct Body {
    pub json: Value,
    /// Optional parameters included (candidates for dropping).
    pub sent: Vec<&'static str>,
    pub declared: Declared,
}

/// Text streamed back by one request, before any cleanup.
pub(crate) struct Streamed {
    pub text: String,
    pub usage: TokenUsage,
}

/// The output was cut off by the output limit.
pub(crate) fn cut_off(purpose: &str) -> TranslateError {
    invalid(if purpose == TRANSLATE {
        "the translation was cut off (output limit reached)"
    } else {
        "the answer was cut off (output limit reached)"
    })
}

/// The provider's content filter blocked the output.
pub(crate) fn filtered(purpose: &str) -> TranslateError {
    TranslateError::Rejected {
        status: 200,
        message: if purpose == TRANSLATE {
            "the provider's content filter blocked this paragraph".into()
        } else {
            "the provider's content filter blocked this request".into()
        },
    }
}

/// The model refused to answer.
pub(crate) fn refused(purpose: &str) -> TranslateError {
    TranslateError::Rejected {
        status: 200,
        message: if purpose == TRANSLATE {
            "the model declined to translate this paragraph".into()
        } else {
            "the model declined this request".into()
        },
    }
}

pub struct HttpProvider {
    core: Core,
    wire: Wire,
    endpoint: String,
}

impl HttpProvider {
    pub fn new(
        config: ProviderConfig,
        keys: KeyFn,
        prompts: Arc<dyn PromptSource>,
        observer: Arc<dyn RequestObserver>,
    ) -> Result<Self, TranslateError> {
        let wire = match (config.kind, config.wire_api) {
            (ProviderKind::Anthropic, _) => Wire::Anthropic,
            (_, WireApi::Responses) => Wire::Responses,
            _ => Wire::Chat,
        };
        let endpoint = match wire {
            Wire::Chat => format!("{}/chat/completions", config.base_url),
            Wire::Responses => format!("{}/responses", config.base_url),
            Wire::Anthropic => format!("{}/v1/messages", config.base_url),
        };
        Ok(Self {
            core: Core::new(config, keys, prompts, observer)?,
            wire,
            endpoint,
        })
    }

    /// Per-key state of the pool, once the keys have been read.
    pub fn key_status(&self) -> Option<Vec<KeyStatus>> {
        self.core.keys.status()
    }

    fn droppable(&self) -> &'static [(&'static str, &'static [&'static str])] {
        match self.wire {
            Wire::Chat => openai::DROPPABLE,
            Wire::Responses => responses::DROPPABLE,
            Wire::Anthropic => anthropic::DROPPABLE,
        }
    }

    fn body(&self, system: &str, user: &str, purpose: &str) -> Body {
        match self.wire {
            Wire::Chat => openai::body(&self.core, system, user),
            Wire::Responses => responses::body(&self.core, system, user, purpose),
            Wire::Anthropic => anthropic::body(&self.core, system, user),
        }
    }

    fn request(&self, key: &str, sent: &[&'static str]) -> RequestBuilder {
        let request = self
            .core
            .client
            .post(&self.endpoint)
            .header(ACCEPT, "text/event-stream")
            .header(CONTENT_TYPE, "application/json");
        match self.wire {
            Wire::Chat | Wire::Responses => request.bearer_auth(key),
            Wire::Anthropic => anthropic::headers(request, key, sent),
        }
    }

    async fn attempt(
        &self,
        lease: &Lease,
        body: &Body,
        purpose: &str,
        tap: &mut Tap,
        on_partial: PartialFn<'_>,
    ) -> Result<Streamed, TranslateError> {
        let key = &lease.key;
        let request = self.request(key, &body.sent).body(body.json.to_string());
        let resp = send(request, key).await?;
        tap.status(resp.status().as_u16());
        if !resp.status().is_success() {
            return Err(error_from_response(resp, key).await);
        }
        match self.wire {
            Wire::Chat => openai::read_stream(resp, key, purpose, tap, on_partial).await,
            Wire::Responses => responses::read_stream(resp, key, purpose, tap, on_partial).await,
            Wire::Anthropic => anthropic::read_stream(resp, key, purpose, tap, on_partial).await,
        }
    }

    /// Send one prompt and stream the answer, rotating keys and dropping
    /// rejected optional parameters as needed.
    pub(crate) async fn exchange(
        &self,
        purpose: &str,
        system: &str,
        user: &str,
        on_partial: PartialFn<'_>,
    ) -> Result<Streamed, TranslateError> {
        let droppable = self.droppable();
        let mut param_retries = 0;
        let mut key_retries = 0;
        loop {
            let lease = self.core.keys.acquire().await?;
            let body = self.body(system, user, purpose);
            let mut tap = Tap::start(
                &self.core.observer,
                RequestRecord {
                    purpose: purpose.to_owned(),
                    provider: self.core.config.name.clone(),
                    wire: self.core.config.wire_label().to_owned(),
                    endpoint: endpoint_label(&self.endpoint),
                    key: Some(lease.key_use()),
                    request: body.declared.clone(),
                    prompt_chars: system.chars().count() + user.chars().count(),
                    ..Default::default()
                },
            );
            let result = self
                .attempt(&lease, &body, purpose, &mut tap, on_partial)
                .await;
            let err = match result {
                Ok(streamed) => {
                    tap.finish(&Ok::<(), TranslateError>(()), streamed.text.chars().count());
                    self.core.keys.succeeded(&lease);
                    return Ok(streamed);
                }
                Err(err) => err,
            };
            if param_retries < droppable.len()
                && let Some(param) = self.core.retry_without(&err, droppable, &body.sent)
            {
                param_retries += 1;
                tap.note(format!(
                    "the endpoint rejected `{param}`: retrying without it"
                ));
                tap.finish(&Err::<(), _>(err), 0);
                on_partial("");
                continue;
            }
            let other_key = self.core.keys.report(&lease, &err);
            if other_key && key_retries + 1 < lease.count {
                key_retries += 1;
                tap.note(format!(
                    "key {} of {} set aside: retrying with another key",
                    lease.index + 1,
                    lease.count
                ));
                tap.finish(&Err::<(), _>(err), 0);
                on_partial("");
                continue;
            }
            tap.finish(&Err::<(), _>(err.clone()), 0);
            return Err(err);
        }
    }

    async fn translate_one(
        &self,
        req: &TranslationRequest,
        on_partial: PartialFn<'_>,
    ) -> Result<TranslationOutput, TranslateError> {
        let messages = prompt::build(req, self.core.prompts.system_prompt(req.direction));
        let out = self
            .exchange(TRANSLATE, &messages.system, &messages.user, on_partial)
            .await?;
        let cleaned = clean_output(&out.text, &req.source);
        if cleaned.is_empty() {
            return Err(invalid("the model returned no translation"));
        }
        Ok(TranslationOutput {
            text: cleaned,
            usage: out.usage,
        })
    }

    async fn complete_one(
        &self,
        req: &ChatRequest,
        on_partial: PartialFn<'_>,
    ) -> Result<ChatOutput, TranslateError> {
        let out = self
            .exchange(&req.purpose, &req.system, &req.user, on_partial)
            .await?;
        let text = strip_reasoning(&out.text).trim().to_owned();
        if text.is_empty() {
            return Err(invalid("the model returned an empty answer"));
        }
        Ok(ChatOutput {
            text,
            usage: out.usage,
        })
    }
}

impl Translator for HttpProvider {
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
        Box::pin(self.translate_one(request, on_partial))
    }
}

impl ChatModel for HttpProvider {
    fn label(&self) -> String {
        format!("{} · {}", self.core.config.name, self.core.config.model)
    }

    fn complete<'a>(
        &'a self,
        request: &'a ChatRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<ChatOutput, TranslateError>> {
        Box::pin(self.complete_one(request, on_partial))
    }
}
