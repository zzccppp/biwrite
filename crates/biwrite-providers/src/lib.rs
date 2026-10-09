//! LLM providers for BiWrite. No Tauri dependency.
//!
//! - [`HttpProvider`]: one provider for three wire formats, OpenAI-compatible
//!   chat completions (OpenAI, DeepSeek, Qwen, Kimi, OpenRouter, local
//!   servers), the OpenAI Responses API (OpenAI, Codex relays such as
//!   AnyRouter) and the Anthropic Messages API. It implements both
//!   [`Translator`] and [`ChatModel`].
//!
//! Requests stream over SSE. Translation prompts are built with
//! [`prompt::build`] and output is cleaned with [`clean::clean_output`].
//! Keys come from a pool fetched lazily through a [`KeyFn`], rotate per
//! request and never leave this crate except in request headers. Every error
//! message is redacted. Each HTTP request is reported to a
//! [`RequestObserver`] as metadata (model, effort, tier, tokens, timing).

mod anthropic;
mod chat;
mod http;
mod keys;
mod models;
mod observe;
mod openai;
mod provider;
mod responses;

pub mod clean;
pub mod config;
pub mod prompt;
pub mod sse;

use std::sync::Arc;

use biwrite_engine::{MockTranslator, TranslateError, Translator};

pub use chat::{ChatModel, ChatOutput, ChatRequest};
pub use config::{
    ConfigError, Effort, Preset, ProviderConfig, ProviderKind, ServiceTier, WireApi, presets,
};
pub use http::redact;
pub use keys::{KeyFn, KeyStatus, tail as key_tail};
pub use models::list_models;
pub use observe::{
    Declared, KeyUse, NoObserver, RecordState, RequestObserver, RequestRecord, UsageDetail,
};
pub use prompt::{DefaultPrompts, PromptFiles, PromptSource};
pub use provider::{HttpProvider, TRANSLATE};

/// Build the translator for a (validated) provider configuration, without a
/// request log.
pub fn build_translator(
    config: &ProviderConfig,
    keys: KeyFn,
    prompts: Arc<dyn PromptSource>,
) -> Result<Arc<dyn Translator>, TranslateError> {
    build_translator_with(config, keys, prompts, Arc::new(NoObserver))
}

/// Build the translator for a (validated) provider configuration, reporting
/// its requests to `observer`.
pub fn build_translator_with(
    config: &ProviderConfig,
    keys: KeyFn,
    prompts: Arc<dyn PromptSource>,
    observer: Arc<dyn RequestObserver>,
) -> Result<Arc<dyn Translator>, TranslateError> {
    Ok(match config.kind {
        ProviderKind::Mock => Arc::new(MockTranslator::default()),
        _ => build_http(config, keys, prompts, observer)?,
    })
}

/// The HTTP provider for a (validated) configuration. The mock has none.
pub fn build_http(
    config: &ProviderConfig,
    keys: KeyFn,
    prompts: Arc<dyn PromptSource>,
    observer: Arc<dyn RequestObserver>,
) -> Result<Arc<HttpProvider>, TranslateError> {
    if config.kind == ProviderKind::Mock {
        return Err(TranslateError::Config(
            "the offline mock cannot answer requests: choose a model provider".into(),
        ));
    }
    Ok(Arc::new(HttpProvider::new(
        config.clone(),
        keys,
        prompts,
        observer,
    )?))
}
