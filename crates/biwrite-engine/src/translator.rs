//! The `Translator` abstraction implemented by every provider.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use biwrite_core::Direction;
use serde::Serialize;

/// Boxed future so `Translator` stays object-safe (`Arc<dyn Translator>`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Callback receiving the *whole* output streamed so far (each call replaces
/// the previous partial, so a provider can also restart it, e.g. after a
/// server-side model fallback).
pub type PartialFn<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// A glossary entry relevant to the segment being translated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlossaryEntry {
    pub term: String,
    /// Chinese rendering; `None` means "keep in English".
    pub translation: Option<String>,
}

/// Previous source and translation for revise mode: the model is asked to
/// revise `old_translation` minimally so wording stays stable across edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub old_source: String,
    pub old_translation: String,
}

/// Everything a provider needs to translate one segment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranslationRequest {
    /// Source and target language.
    pub direction: Direction,
    /// Text to translate.
    pub source: String,
    /// Source of the previous segment, for context only (not translated).
    pub context_before: Option<String>,
    /// Source of the next segment, for context only (not translated).
    pub context_after: Option<String>,
    pub glossary: Vec<GlossaryEntry>,
    /// Document-level note, e.g. "ML paper on in-context learning".
    pub doc_note: Option<String>,
    pub revision: Option<Revision>,
}

/// Token counts reported by a provider for one request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Final result of a translation request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationOutput {
    pub text: String,
    pub usage: TokenUsage,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TranslateError {
    #[error("rate limited by the provider")]
    RateLimited { retry_after: Option<Duration> },
    #[error("provider server error (HTTP {status})")]
    Server {
        status: u16,
        retry_after: Option<Duration>,
    },
    #[error("network error: {0}")]
    Network(String),
    #[error("request rejected (HTTP {status}): {message}")]
    Rejected { status: u16, message: String },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    /// Misconfiguration the user must fix (e.g. no API key). Not retried.
    #[error("{0}")]
    Config(String),
}

impl TranslateError {
    /// Whether the queue should retry with exponential backoff.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::Server { .. } | Self::Network(_)
        )
    }

    /// Server-provided `Retry-After`, if any.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } | Self::Server { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// A translation backend.
pub trait Translator: Send + Sync {
    /// Provider identifier; part of the cache key.
    fn provider(&self) -> &str;

    /// Model identifier; part of the cache key.
    fn model(&self) -> &str;

    /// Output is a stand-in (the mock), to be redone once a real
    /// translator is selected.
    fn is_placeholder(&self) -> bool {
        false
    }

    /// Translate one segment, calling `on_partial` with the output so far.
    /// The returned text is the complete output. Dropping the future cancels
    /// the request.
    fn translate<'a>(
        &'a self,
        request: &'a TranslationRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<TranslationOutput, TranslateError>>;
}
