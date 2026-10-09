//! Request records for the request log, in the spirit of a wiretap: what each
//! HTTP request asked for (model, reasoning effort, service tier) next to what
//! the response declared, with status, timing and token counts.
//!
//! Records hold metadata only. Prompts, outputs and API keys never enter one.
//! A key is identified by its position in the pool and its last characters.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use biwrite_engine::TranslateError;
use serde::Serialize;

/// What one side of the exchange named: the request as sent, or the response
/// as declared by the server. `None` means not sent or not returned.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Declared {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
}

/// Which key of the provider's pool a request used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyUse {
    /// 1-based position in the pool.
    pub number: usize,
    pub count: usize,
    /// The last four characters, enough to tell keys apart.
    pub tail: String,
    /// Hash-based identifier that key names in the settings refer to.
    pub fingerprint: String,
}

/// Token counts as reported by the response. `None` means not reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDetail {
    pub input_tokens: Option<u64>,
    /// Part of the input served from the provider's prompt cache.
    pub cached_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Part of the output spent on reasoning.
    pub reasoning_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordState {
    #[default]
    InFlight,
    Ok,
    Error,
    /// The request was dropped before it finished (an edit superseded it).
    Cancelled,
}

/// One HTTP request to a model.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRecord {
    /// Unique within the process, increasing.
    pub id: u64,
    /// Unix time in milliseconds.
    pub started_at: u64,
    /// What the request was for: `translate`, `polish`, `edit`, `ask`, ...
    pub purpose: String,
    /// The provider's display name.
    pub provider: String,
    /// `chat`, `responses` or `anthropic`.
    pub wire: String,
    /// Host and path of the endpoint.
    pub endpoint: String,
    pub key: Option<KeyUse>,
    pub request: Declared,
    pub response: Declared,
    pub http_status: Option<u16>,
    pub state: RecordState,
    /// Redacted error message.
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
    /// Time until the first output text arrived.
    pub first_token_ms: Option<u64>,
    pub usage: UsageDetail,
    /// Size of the prompt (system and user text), in characters.
    pub prompt_chars: usize,
    /// Size of the output text, in characters.
    pub output_chars: usize,
    /// Events worth knowing, e.g. a parameter dropped after a rejection.
    pub notes: Vec<String>,
}

/// Receives request records. A record is reported when its request starts and
/// again when it ends. The later report replaces the earlier one (same `id`).
pub trait RequestObserver: Send + Sync {
    fn record(&self, record: &RequestRecord);
}

/// Observer that keeps nothing.
pub struct NoObserver;

impl RequestObserver for NoObserver {
    fn record(&self, _: &RequestRecord) {}
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Host and path of `url`, without scheme, credentials or query.
pub(crate) fn endpoint_label(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => {
            let host = u.host_str().unwrap_or("");
            match u.port() {
                Some(port) => format!("{host}:{port}{}", u.path()),
                None => format!("{host}{}", u.path()),
            }
        }
        Err(_) => String::new(),
    }
}

/// Tracks one HTTP request. Dropping it before [`finish`](Self::finish)
/// means the request future was cancelled, which is reported as such.
pub(crate) struct Tap {
    observer: Arc<dyn RequestObserver>,
    pub record: RequestRecord,
    start: Instant,
    done: bool,
}

impl Tap {
    pub fn start(observer: &Arc<dyn RequestObserver>, mut record: RequestRecord) -> Self {
        record.id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        record.started_at = now_ms();
        record.state = RecordState::InFlight;
        observer.record(&record);
        Self {
            observer: Arc::clone(observer),
            record,
            start: Instant::now(),
            done: false,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Note the arrival of output text (only the first call counts).
    pub fn first_token(&mut self) {
        if self.record.first_token_ms.is_none() {
            self.record.first_token_ms = Some(self.elapsed_ms());
        }
    }

    pub fn status(&mut self, status: u16) {
        self.record.http_status = Some(status);
    }

    /// Fill in what the response declared. Values already seen are kept
    /// unless the response states them again.
    pub fn declared(&mut self, model: Option<&str>, effort: Option<&str>, tier: Option<&str>) {
        let r = &mut self.record.response;
        let set = |slot: &mut Option<String>, v: Option<&str>| {
            if let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) {
                *slot = Some(v.to_owned());
            }
        };
        set(&mut r.model, model);
        set(&mut r.effort, effort);
        set(&mut r.service_tier, tier);
    }

    pub fn usage(&mut self, usage: UsageDetail) {
        let u = &mut self.record.usage;
        u.input_tokens = usage.input_tokens.or(u.input_tokens);
        u.cached_tokens = usage.cached_tokens.or(u.cached_tokens);
        u.output_tokens = usage.output_tokens.or(u.output_tokens);
        u.reasoning_tokens = usage.reasoning_tokens.or(u.reasoning_tokens);
    }

    pub fn note(&mut self, note: impl Into<String>) {
        self.record.notes.push(note.into());
    }

    /// Report the outcome. `output_chars` is the size of the text received.
    pub fn finish<T>(mut self, result: &Result<T, TranslateError>, output_chars: usize) {
        self.done = true;
        self.record.duration_ms = Some(self.elapsed_ms());
        self.record.output_chars = output_chars;
        match result {
            Ok(_) => self.record.state = RecordState::Ok,
            Err(e) => {
                self.record.state = RecordState::Error;
                self.record.error = Some(e.to_string());
            }
        }
        self.observer.record(&self.record);
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        if !self.done {
            self.record.duration_ms = Some(self.elapsed_ms());
            self.record.state = RecordState::Cancelled;
            self.observer.record(&self.record);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Collect(Mutex<Vec<RequestRecord>>);

    impl RequestObserver for Collect {
        fn record(&self, record: &RequestRecord) {
            self.0.lock().unwrap().push(record.clone());
        }
    }

    #[test]
    fn records_start_finish_and_cancellation() {
        let collect = Arc::new(Collect::default());
        let observer: Arc<dyn RequestObserver> = collect.clone();
        let mut tap = Tap::start(
            &observer,
            RequestRecord {
                purpose: "translate".into(),
                ..Default::default()
            },
        );
        tap.declared(Some("gpt-6-astra"), Some("high"), None);
        tap.declared(None, None, Some("default"));
        tap.usage(UsageDetail {
            input_tokens: Some(10),
            ..Default::default()
        });
        tap.usage(UsageDetail {
            output_tokens: Some(3),
            ..Default::default()
        });
        tap.finish(&Ok::<(), TranslateError>(()), 7);
        drop(Tap::start(&observer, RequestRecord::default()));

        let seen = collect.0.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert_eq!(seen[0].state, RecordState::InFlight);
        let done = &seen[1];
        assert_eq!(done.state, RecordState::Ok);
        assert_eq!(done.id, seen[0].id);
        assert_eq!(done.response.model.as_deref(), Some("gpt-6-astra"));
        assert_eq!(done.response.effort.as_deref(), Some("high"));
        assert_eq!(done.response.service_tier.as_deref(), Some("default"));
        assert_eq!(
            (done.usage.input_tokens, done.usage.output_tokens),
            (Some(10), Some(3))
        );
        assert_eq!(done.output_chars, 7);
        assert_eq!(seen[3].state, RecordState::Cancelled);
        assert!(seen[3].id > done.id);
    }

    #[test]
    fn endpoint_labels_drop_scheme_credentials_and_query() {
        assert_eq!(
            endpoint_label("https://anyrouter.top/v1/responses"),
            "anyrouter.top/v1/responses"
        );
        assert_eq!(
            endpoint_label("http://127.0.0.1:8080/v1/chat/completions?x=1"),
            "127.0.0.1:8080/v1/chat/completions"
        );
        assert_eq!(endpoint_label("not a url"), "");
    }
}
