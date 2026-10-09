//! Test doubles shared by the engine integration tests.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use biwrite_core::{Mode, SegmentId};
use biwrite_engine::{
    BoxFuture, Engine, EngineSettings, EventSink, MemoryCache, PartialFn, SegmentState,
    SegmentStatus, SessionUsage, TokenUsage, TranslateError, TranslationOutput, TranslationRequest,
    Translator,
};

/// Reverses the source after a fixed delay; records every request and can be
/// scripted to fail.
pub struct Scripted {
    pub delay: Duration,
    transform: fn(&str) -> String,
    calls: AtomicU64,
    requests: Mutex<Vec<TranslationRequest>>,
    failures: Mutex<VecDeque<TranslateError>>,
}

impl Scripted {
    pub fn new(delay: Duration) -> Arc<Self> {
        Self::mapping(delay, reverse)
    }

    /// A translator applying `transform` instead of reversing.
    pub fn mapping(delay: Duration, transform: fn(&str) -> String) -> Arc<Self> {
        Arc::new(Self {
            delay,
            transform,
            calls: AtomicU64::new(0),
            requests: Mutex::new(Vec::new()),
            failures: Mutex::new(VecDeque::new()),
        })
    }

    pub fn fail_next(&self, errors: impl IntoIterator<Item = TranslateError>) {
        self.failures.lock().unwrap().extend(errors);
    }

    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn requests(&self) -> Vec<TranslationRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Translator for Scripted {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn model(&self) -> &str {
        "reverse"
    }

    fn translate<'a>(
        &'a self,
        request: &'a TranslationRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<TranslationOutput, TranslateError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request.clone());
            tokio::time::sleep(self.delay).await;
            if let Some(err) = self.failures.lock().unwrap().pop_front() {
                // Stream some output before failing, like a dropped connection.
                on_partial("half an answ");
                return Err(err);
            }
            let text = (self.transform)(&request.source);
            on_partial(&text);
            Ok(TranslationOutput {
                text,
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
            })
        })
    }
}

#[derive(Default)]
pub struct RecordingSink {
    pub states: Mutex<Vec<SegmentState>>,
    pub notices: Mutex<Vec<String>>,
    pub usage: Mutex<Option<SessionUsage>>,
}

impl EventSink for RecordingSink {
    fn segment_states(&self, states: &[SegmentState]) {
        self.states.lock().unwrap().extend_from_slice(states);
    }
    fn usage(&self, usage: &SessionUsage) {
        *self.usage.lock().unwrap() = Some(*usage);
    }
    fn notice(&self, message: &str) {
        self.notices.lock().unwrap().push(message.to_owned());
    }
}

pub struct Harness {
    pub engine: Engine,
    pub translator: Arc<Scripted>,
    pub sink: Arc<RecordingSink>,
    pub cache: Arc<MemoryCache>,
}

pub fn harness(settings: EngineSettings) -> Harness {
    let translator = Scripted::new(Duration::from_millis(700));
    let sink = Arc::new(RecordingSink::default());
    let cache = Arc::new(MemoryCache::default());
    let engine = Engine::new(
        translator.clone(),
        cache.clone(),
        sink.clone(),
        settings,
        tokio::runtime::Handle::current(),
    );
    Harness {
        engine,
        translator,
        sink,
        cache,
    }
}

pub fn reverse(s: &str) -> String {
    s.chars().rev().collect()
}

/// `n` distinct paragraphs separated by blank lines.
pub fn paragraphs(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("Paragraph {i} explains finding number {i} of the study in plain words."))
        .collect()
}

pub fn join(paras: &[String]) -> String {
    paras.join("\n\n")
}

/// Wait (in virtual time) until no job is queued or running.
pub async fn settle(engine: &Engine) {
    for _ in 0..100_000 {
        if engine.pending() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("engine did not settle");
}

pub fn state_of(engine: &Engine, id: SegmentId) -> SegmentState {
    engine
        .snapshot()
        .states
        .into_iter()
        .find(|s| s.id == id)
        .expect("segment state")
}

pub fn all_translated(engine: &Engine) -> bool {
    engine
        .snapshot()
        .states
        .iter()
        .all(|s| s.status == SegmentStatus::Translated || s.status == SegmentStatus::Skipped)
}

pub async fn loaded(h: &Harness, text: &str) {
    h.engine.load(text.to_owned(), Mode::Plain);
    settle(&h.engine).await;
    assert!(all_translated(&h.engine));
}
