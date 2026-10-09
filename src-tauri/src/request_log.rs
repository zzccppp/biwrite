//! The request log behind the Log panel: every model request as metadata,
//! namely the model, reasoning effort and service tier it asked for, what the
//! response declared, status, timing and token counts. Prompts, outputs and
//! keys are never part of a record (see `biwrite_providers::RequestRecord`).
//!
//! Recording can be paused. While paused, new requests are not recorded, and
//! requests recorded before the pause still get their outcome. The newest
//! records stay in memory for the panel. Finished records are also appended
//! to `requests.jsonl` in the app data folder (owner-only permissions), which
//! is rotated once it grows large.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use biwrite_providers::{RecordState, RequestObserver, RequestRecord};
use serde::{Deserialize, Serialize};

/// Records kept in memory.
const MAX_RECORDS: usize = 500;
/// The log file is moved to `requests.jsonl.1` at this size.
const MAX_FILE_BYTES: u64 = 5_000_000;

/// Receives every recorded change (the app forwards it to the webview).
pub trait LogSink: Send + Sync {
    fn updated(&self, record: &RequestRecord);
}

#[cfg(test)]
pub struct NoSink;

#[cfg(test)]
impl LogSink for NoSink {
    fn updated(&self, _: &RequestRecord) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogSettings {
    /// Record new requests.
    pub enabled: bool,
    /// Also append finished records to the log file.
    pub persist: bool,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            persist: true,
        }
    }
}

struct Inner {
    settings: LogSettings,
    records: VecDeque<RequestRecord>,
}

pub struct RequestLog {
    sink: Arc<dyn LogSink>,
    file: Option<PathBuf>,
    inner: Mutex<Inner>,
    /// Serializes appends and rotation of the file.
    writing: Mutex<()>,
}

impl RequestLog {
    pub fn new(settings: LogSettings, file: Option<PathBuf>, sink: Arc<dyn LogSink>) -> Self {
        Self {
            sink,
            file,
            inner: Mutex::new(Inner {
                settings,
                records: VecDeque::new(),
            }),
            writing: Mutex::new(()),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn settings(&self) -> LogSettings {
        self.inner().settings
    }

    pub fn set_settings(&self, settings: LogSettings) {
        self.inner().settings = settings;
    }

    /// Records in memory, newest first.
    pub fn records(&self) -> Vec<RequestRecord> {
        self.inner().records.iter().rev().cloned().collect()
    }

    /// Forget the records in memory (the file keeps them).
    pub fn clear(&self) {
        self.inner().records.clear();
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    fn append(&self, record: &RequestRecord) {
        let Some(path) = &self.file else {
            return;
        };
        let Ok(mut line) = serde_json::to_vec(record) else {
            return;
        };
        line.push(b'\n');
        let _one_writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
            let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options.open(path).and_then(|mut f| f.write_all(&line));
        if let Err(e) = written {
            eprintln!("BiWrite: cannot write {}: {e}", path.display());
        }
    }
}

impl RequestObserver for RequestLog {
    fn record(&self, record: &RequestRecord) {
        let persist = {
            let mut inner = self.inner();
            match inner.records.iter().position(|r| r.id == record.id) {
                Some(i) => inner.records[i] = record.clone(),
                // Only a request's first report (in flight) starts a record,
                // so requests that began while paused stay out.
                None if inner.settings.enabled && record.state == RecordState::InFlight => {
                    inner.records.push_back(record.clone());
                    while inner.records.len() > MAX_RECORDS {
                        inner.records.pop_front();
                    }
                }
                None => return,
            }
            inner.settings.persist && record.state != RecordState::InFlight
        };
        if persist {
            self.append(record);
        }
        self.sink.updated(record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Count(Mutex<usize>);

    impl LogSink for Count {
        fn updated(&self, _: &RequestRecord) {
            *self.0.lock().unwrap() += 1;
        }
    }

    fn rec(id: u64, state: RecordState) -> RequestRecord {
        RequestRecord {
            id,
            state,
            purpose: "translate".into(),
            ..Default::default()
        }
    }

    #[test]
    fn pausing_keeps_running_requests_and_skips_new_ones() {
        let sink = Arc::new(Count::default());
        let log = RequestLog::new(LogSettings::default(), None, sink.clone());
        log.record(&rec(1, RecordState::InFlight));
        log.set_settings(LogSettings {
            enabled: false,
            persist: true,
        });
        log.record(&rec(2, RecordState::InFlight));
        log.record(&rec(1, RecordState::Ok));
        log.set_settings(LogSettings::default());
        // Began while paused: its outcome is not recorded either.
        log.record(&rec(2, RecordState::Ok));
        let records = log.records();
        assert_eq!(records.len(), 1);
        assert_eq!((records[0].id, records[0].state), (1, RecordState::Ok));
        assert_eq!(*sink.0.lock().unwrap(), 2);
    }

    #[test]
    fn memory_is_bounded_and_newest_first() {
        let log = RequestLog::new(LogSettings::default(), None, Arc::new(NoSink));
        for id in 0..(MAX_RECORDS as u64 + 20) {
            log.record(&rec(id, RecordState::InFlight));
        }
        let records = log.records();
        assert_eq!(records.len(), MAX_RECORDS);
        assert_eq!(records[0].id, MAX_RECORDS as u64 + 19);
        log.clear();
        assert!(log.records().is_empty());
    }

    #[test]
    fn finished_records_go_to_a_private_file() {
        let dir = std::env::temp_dir().join(format!("biwrite-reqlog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("requests.jsonl");
        let log = RequestLog::new(LogSettings::default(), Some(path.clone()), Arc::new(NoSink));
        log.record(&rec(7, RecordState::InFlight));
        log.record(&rec(7, RecordState::Ok));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().count(),
            1,
            "only the finished state is written"
        );
        assert!(text.contains("\"id\":7"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        log.set_settings(LogSettings {
            enabled: true,
            persist: false,
        });
        log.record(&rec(8, RecordState::InFlight));
        log.record(&rec(8, RecordState::Error));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
