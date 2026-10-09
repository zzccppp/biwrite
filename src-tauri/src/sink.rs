//! Delivers engine events to the webview.
//!
//! Payloads are segment states, usage counters and notices only; nothing that
//! passes through here may contain credentials.

use biwrite_engine::{EventSink, SegmentState, SessionUsage};
use biwrite_providers::RequestRecord;
use tauri::{AppHandle, Emitter};

use crate::request_log::LogSink;

pub const EVENT_SEGMENT_STATES: &str = "segment-states";
pub const EVENT_USAGE: &str = "usage";
pub const EVENT_NOTICE: &str = "notice";
pub const EVENT_REQUEST_LOG: &str = "request-log";

pub struct TauriSink {
    app: AppHandle,
}

impl TauriSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) {
        // Events are best-effort: if the webview is gone or reloading, it
        // resynchronizes with `get_session` on startup.
        if let Err(e) = self.app.emit(event, payload) {
            eprintln!("biwrite: failed to emit {event}: {e}");
        }
    }
}

impl EventSink for TauriSink {
    fn segment_states(&self, states: &[SegmentState]) {
        self.emit(EVENT_SEGMENT_STATES, states);
    }

    fn usage(&self, usage: &SessionUsage) {
        self.emit(EVENT_USAGE, usage);
    }

    fn notice(&self, message: &str) {
        self.emit(EVENT_NOTICE, message);
    }
}

impl LogSink for TauriSink {
    fn updated(&self, record: &RequestRecord) {
        self.emit(EVENT_REQUEST_LOG, record);
    }
}
