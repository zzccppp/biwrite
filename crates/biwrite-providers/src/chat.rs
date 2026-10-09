//! A plain model call for the writing assistant: one system text and one user
//! text in, streamed text out. The prompts are built by the caller (see
//! `biwrite_core::assist`), unlike translation, whose prompts are built here.

use biwrite_engine::{BoxFuture, PartialFn, TokenUsage, TranslateError};

/// One assistant request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatRequest {
    /// What the request is for (`polish`, `edit`, `ask`), shown in the
    /// request log.
    pub purpose: String,
    pub system: String,
    pub user: String,
}

/// The model's answer, with any leading reasoning block removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatOutput {
    pub text: String,
    pub usage: TokenUsage,
}

/// A model that answers free-form requests.
pub trait ChatModel: Send + Sync {
    /// "Name · model", for the UI.
    fn label(&self) -> String;

    /// Send the request, calling `on_partial` with the whole visible output
    /// so far. Dropping the future cancels the request.
    fn complete<'a>(
        &'a self,
        request: &'a ChatRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<ChatOutput, TranslateError>>;
}
