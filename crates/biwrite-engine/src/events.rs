//! Data sent to the UI and the sink trait used to deliver it.
//!
//! Nothing in here may ever contain credentials: these structs are serialized
//! straight into IPC payloads.

use biwrite_core::{Direction, Mode, SegmentId, SegmentKind};
use serde::Serialize;

/// Display state of a segment's translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentStatus {
    /// `text` is the translation of the current source.
    Translated,
    /// Source changed and auto-translate is paused; `text` is the old
    /// translation, if any.
    Stale,
    /// Waiting for a free request slot; `text` is the old translation, if any.
    Queued,
    /// Request in flight; `text` is streamed output (`partial`) or the old
    /// translation.
    Translating,
    /// Translation failed; `error` explains why.
    Error,
    /// Not translatable (code, math, preamble, ...).
    Skipped,
}

/// Translation state of one segment. `version` increases on every change so
/// the UI can drop out-of-order updates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentState {
    pub id: SegmentId,
    pub version: u64,
    pub status: SegmentStatus,
    pub text: Option<String>,
    /// `text` is incomplete streamed output.
    pub partial: bool,
    pub error: Option<String>,
}

/// Position of a segment in the editor, in UTF-16 code units.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentLayout {
    pub id: SegmentId,
    pub kind: SegmentKind,
    pub from: usize,
    pub to: usize,
}

/// Session-wide counters for the status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsage {
    /// Completed provider requests (including failed and discarded ones).
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Segments served from the cache instead of the provider.
    pub cache_hits: u64,
}

/// Result of a document operation: the full layout plus the states that
/// changed (`full == true`: all states).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Increases with every text/mode change.
    pub revision: u64,
    pub mode: Mode,
    /// Which language is being edited.
    pub direction: Direction,
    pub full: bool,
    pub layout: Vec<SegmentLayout>,
    pub states: Vec<SegmentState>,
    pub usage: SessionUsage,
}

/// A paragraph still in the language of the translations (left so by an
/// early swap), now translated into the edited language: the editor
/// replaces the segment's text `old` (markup included) with `new`, unless
/// it changed meanwhile. Its translation stays the exact original.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fill {
    pub id: SegmentId,
    pub old: String,
    pub new: String,
}

/// Receives asynchronous engine events (from background translation jobs).
pub trait EventSink: Send + Sync {
    fn segment_states(&self, states: &[SegmentState]);
    fn usage(&self, usage: &SessionUsage);
    /// Non-fatal message for the status bar (retries, cache problems).
    fn notice(&self, message: &str);
    /// Paragraphs whose text in the editor is to be replaced (see [`Fill`]).
    fn fills(&self, _fills: &[Fill]) {}
}

/// Sink that drops everything (tests, headless use).
pub struct NullSink;

impl EventSink for NullSink {
    fn segment_states(&self, _: &[SegmentState]) {}
    fn usage(&self, _: &SessionUsage) {}
    fn notice(&self, _: &str) {}
}
