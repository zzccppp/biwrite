//! LLM translation providers for BiWrite. No Tauri dependency.
//!
//! - [`OpenAiTranslator`]: OpenAI-compatible chat completions (OpenAI,
//!   DeepSeek, Qwen, Kimi, OpenRouter, local servers).
//! - [`AnthropicTranslator`]: Anthropic Messages API.
//!
//! Both stream over SSE, build prompts with [`prompt::build`], clean output
//! with [`clean::clean_output`], back off via the engine on 429/5xx, and
//! redact the API key from every error message. Keys are fetched lazily
//! through a [`KeyFn`] and never leave this crate except in request headers.

mod anthropic;
mod http;
mod models;
mod openai;

pub mod clean;
pub mod config;
pub mod prompt;
pub mod sse;

use std::sync::Arc;

use biwrite_engine::{MockTranslator, TranslateError, Translator};

pub use anthropic::AnthropicTranslator;
pub use config::{ConfigError, Effort, Preset, ProviderConfig, ProviderKind, presets};
pub use http::{KeyFn, redact};
pub use models::list_models;
pub use openai::OpenAiTranslator;
pub use prompt::{DefaultPrompts, PromptFiles, PromptSource};

/// Build the translator for a (validated) provider configuration.
pub fn build_translator(
    config: &ProviderConfig,
    key: KeyFn,
    prompts: Arc<dyn PromptSource>,
) -> Result<Arc<dyn Translator>, TranslateError> {
    Ok(match config.kind {
        ProviderKind::Mock => Arc::new(MockTranslator::default()),
        ProviderKind::OpenaiCompatible => {
            Arc::new(OpenAiTranslator::new(config.clone(), key, prompts)?)
        }
        ProviderKind::Anthropic => {
            Arc::new(AnthropicTranslator::new(config.clone(), key, prompts)?)
        }
    })
}
