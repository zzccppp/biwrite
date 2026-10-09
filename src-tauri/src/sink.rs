//! Delivers engine events to the webview.
//!
//! Payloads are segment states, usage counters and notices only; nothing that
//! passes through here may contain credentials.

use biwrite_engine::{EventSink, Fill, SegmentState, SegmentStatus, SessionUsage};
use biwrite_providers::RequestRecord;
use tauri::{AppHandle, Emitter};

use crate::request_log::LogSink;

pub const EVENT_SEGMENT_STATES: &str = "segment-states";
pub const EVENT_USAGE: &str = "usage";
pub const EVENT_NOTICE: &str = "notice";
pub const EVENT_REQUEST_LOG: &str = "request-log";
pub const EVENT_FILLS: &str = "segment-fills";

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
            log::warn!("failed to emit {event}: {e}");
        }
    }
}

impl EventSink for TauriSink {
    fn segment_states(&self, states: &[SegmentState]) {
        // Failed translations go to the log; the message is already redacted.
        for s in states.iter().filter(|s| s.status == SegmentStatus::Error) {
            if let Some(error) = &s.error {
                log::warn!("paragraph {} failed: {error}", s.id.0);
            }
        }
        self.emit(EVENT_SEGMENT_STATES, states);
    }

    fn usage(&self, usage: &SessionUsage) {
        self.emit(EVENT_USAGE, usage);
    }

    fn notice(&self, message: &str) {
        log::info!("{message}");
        self.emit(EVENT_NOTICE, message);
    }

    fn fills(&self, fills: &[Fill]) {
        self.emit(EVENT_FILLS, fills);
    }
}

impl LogSink for TauriSink {
    fn updated(&self, record: &RequestRecord) {
        self.emit(EVENT_REQUEST_LOG, record);
    }
}
