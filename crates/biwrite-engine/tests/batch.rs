//! Several segments per request ("paragraphs per request" above one): fresh
//! translations share requests, edits keep their own, a segment without a
//! usable answer is retried alone (the whole batch when an answer is
//! missing), and editing one segment of a running
//! batch does not cost the others their translation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use biwrite_core::Mode;
use biwrite_engine::{
    BatchOutput, BatchPartialFn, BoxFuture, Engine, EngineSettings, MemoryCache, PartialFn,
    SegmentStatus, TokenUsage, TranslateError, TranslationOutput, TranslationRequest, Translator,
};
use common::{RecordingSink, all_translated, join, paragraphs, reverse, settle, state_of};

/// Reverses text. Batches are one call; answers can be withheld per position.
struct Batcher {
    delay: Duration,
    singles: AtomicU64,
    batches: Mutex<Vec<usize>>,
    /// Positions left out of the next batch answer.
    withhold: Mutex<HashSet<usize>>,
    revisions: AtomicU64,
}

impl Batcher {
    fn new(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            delay,
            singles: AtomicU64::new(0),
            batches: Mutex::new(Vec::new()),
            withhold: Mutex::new(HashSet::new()),
            revisions: AtomicU64::new(0),
        })
    }
}

impl Translator for Batcher {
    fn provider(&self) -> &str {
        "batcher"
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
            self.singles.fetch_add(1, Ordering::SeqCst);
            if request.revision.is_some() {
                self.revisions.fetch_add(1, Ordering::SeqCst);
            }
            tokio::time::sleep(self.delay).await;
            let text = reverse(&request.source);
            on_partial(&text);
            Ok(TranslationOutput {
                text,
                usage: TokenUsage::default(),
            })
        })
    }

    fn translate_batch<'a>(
        &'a self,
        requests: &'a [TranslationRequest],
        on_partial: BatchPartialFn<'a>,
    ) -> BoxFuture<'a, Result<BatchOutput, TranslateError>> {
        Box::pin(async move {
            self.batches.lock().unwrap().push(requests.len());
            assert!(
                requests.iter().all(|r| r.revision.is_none()),
                "edits must not be batched"
            );
            tokio::time::sleep(self.delay).await;
            let withhold = std::mem::take(&mut *self.withhold.lock().unwrap());
            let texts = requests
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let text = reverse(&r.source);
                    on_partial(i, &text);
                    (!withhold.contains(&i)).then_some(text)
                })
                .collect();
            Ok(BatchOutput {
                texts,
                usage: TokenUsage::default(),
            })
        })
    }
}

fn engine(t: Arc<Batcher>, batch_size: usize, concurrency: usize) -> (Engine, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let engine = Engine::new(
        t,
        Arc::new(MemoryCache::default()),
        sink.clone(),
        EngineSettings {
            batch_size,
            concurrency,
            ..EngineSettings::default()
        },
        tokio::runtime::Handle::current(),
    );
    (engine, sink)
}

fn texts(engine: &Engine) -> Vec<Option<String>> {
    engine
        .snapshot()
        .states
        .into_iter()
        .map(|s| s.text)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn fresh_paragraphs_share_requests() {
    let t = Batcher::new(Duration::from_millis(700));
    let (engine, _) = engine(t.clone(), 3, 4);
    let paras = paragraphs(7);
    engine.load(join(&paras), Mode::Plain);
    settle(&engine).await;
    assert!(all_translated(&engine));
    let expected: Vec<Option<String>> = paras.iter().map(|p| Some(reverse(p))).collect();
    assert_eq!(texts(&engine), expected);
    // 3 + 3 in two batches; the last paragraph has nobody to share with.
    assert_eq!(*t.batches.lock().unwrap(), vec![3, 3]);
    assert_eq!(t.singles.load(Ordering::SeqCst), 1);
    // One request per batch in the usage counters.
    assert_eq!(engine.usage().requests, 3);
}

#[tokio::test(start_paused = true)]
async fn a_batch_counts_once_against_the_concurrency_limit() {
    let t = Batcher::new(Duration::from_millis(700));
    let (engine, _) = engine(t.clone(), 4, 2);
    engine.load(join(&paragraphs(8)), Mode::Plain);
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Two requests in flight, eight segments translating.
    let translating = engine
        .snapshot()
        .states
        .iter()
        .filter(|s| s.status == SegmentStatus::Translating)
        .count();
    assert_eq!(translating, 8);
    assert_eq!(*t.batches.lock().unwrap(), vec![4, 4]);
    settle(&engine).await;
    assert!(all_translated(&engine));
}

/// A missing answer means the model lost count: the answers around it may
/// be merged or shifted, so the whole batch goes again one by one.
#[tokio::test(start_paused = true)]
async fn a_batch_with_a_missing_answer_is_sent_again_one_by_one() {
    let t = Batcher::new(Duration::from_millis(700));
    t.withhold.lock().unwrap().insert(1);
    let (engine, sink) = engine(t.clone(), 3, 4);
    let paras = paragraphs(3);
    engine.load(join(&paras), Mode::Plain);
    settle(&engine).await;
    let expected: Vec<Option<String>> = paras.iter().map(|p| Some(reverse(p))).collect();
    assert_eq!(texts(&engine), expected);
    assert_eq!(*t.batches.lock().unwrap(), vec![3]);
    assert_eq!(t.singles.load(Ordering::SeqCst), 3);
    assert!(
        sink.notices
            .lock()
            .unwrap()
            .iter()
            .any(|n| n.contains("one by one"))
    );
}

#[tokio::test(start_paused = true)]
async fn editing_one_paragraph_keeps_the_rest_of_its_batch() {
    let t = Batcher::new(Duration::from_millis(700));
    let (engine, _) = engine(t.clone(), 3, 4);
    let mut paras = paragraphs(3);
    engine.load(join(&paras), Mode::Plain);
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Rewrite the middle paragraph completely while the batch is running.
    paras[1] = "A different sentence that shares nothing with the old one.".into();
    engine.update(join(&paras));
    settle(&engine).await;
    let expected: Vec<Option<String>> = paras.iter().map(|p| Some(reverse(p))).collect();
    assert_eq!(texts(&engine), expected);
    // The batch was not aborted: the first and last paragraph came from it.
    assert_eq!(*t.batches.lock().unwrap(), vec![3]);
    assert_eq!(t.singles.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn edits_keep_their_own_request() {
    let t = Batcher::new(Duration::from_millis(700));
    let (engine, _) = engine(t.clone(), 3, 4);
    let mut paras = paragraphs(4);
    engine.load(join(&paras), Mode::Plain);
    settle(&engine).await;
    let batches_before = t.batches.lock().unwrap().len();
    // Small edits: revise mode, which batches never carry.
    paras[0] = paras[0].replace("plain words", "simple words");
    paras[2] = paras[2].replace("plain words", "simple words");
    engine.update(join(&paras));
    settle(&engine).await;
    assert!(all_translated(&engine));
    assert_eq!(t.batches.lock().unwrap().len(), batches_before);
    assert_eq!(t.revisions.load(Ordering::SeqCst), 2);
    let id = engine.snapshot().layout[0].id;
    assert_eq!(state_of(&engine, id).text, Some(reverse(&paras[0])));
}

#[tokio::test(start_paused = true)]
async fn the_batch_size_can_change_at_any_time() {
    let t = Batcher::new(Duration::from_millis(700));
    let (engine, _) = engine(t.clone(), 1, 4);
    engine.set_batch_size(4);
    engine.load(join(&paragraphs(4)), Mode::Plain);
    settle(&engine).await;
    assert_eq!(*t.batches.lock().unwrap(), vec![4]);
    engine.set_batch_size(99);
    assert_eq!(engine.settings().batch_size, biwrite_engine::MAX_BATCH);
}

#[tokio::test(start_paused = true)]
async fn an_offered_translation_is_used_without_a_request() {
    let t = Batcher::new(Duration::from_millis(700));
    let (engine, _) = engine(t.clone(), 1, 4);
    let mut paras = paragraphs(2);
    engine.load(join(&paras), Mode::Plain);
    settle(&engine).await;
    let calls = t.singles.load(Ordering::SeqCst);
    // The assistant's approved revision of paragraph 1, with its translation.
    let revised = "An entirely new first paragraph written by the assistant.";
    engine.offer_translation(revised, "助手给出的译文。");
    paras[0] = revised.into();
    engine.update(join(&paras));
    settle(&engine).await;
    let id = engine.snapshot().layout[0].id;
    assert_eq!(
        state_of(&engine, id).text.as_deref(),
        Some("助手给出的译文。")
    );
    assert_eq!(t.singles.load(Ordering::SeqCst), calls, "no new request");
}

/// Answers "译 " + the source. Batches can come back with the first two
/// answers swapped, or fail with a given error.
struct Echo {
    swap: bool,
    batch_error: Option<TranslateError>,
    singles: AtomicU64,
    batches: AtomicU64,
}

impl Echo {
    fn new(swap: bool, batch_error: Option<TranslateError>) -> Arc<Self> {
        Arc::new(Self {
            swap,
            batch_error,
            singles: AtomicU64::new(0),
            batches: AtomicU64::new(0),
        })
    }
}

impl Translator for Echo {
    fn provider(&self) -> &str {
        "echo"
    }

    fn model(&self) -> &str {
        "echo"
    }

    fn translate<'a>(
        &'a self,
        request: &'a TranslationRequest,
        _on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<TranslationOutput, TranslateError>> {
        Box::pin(async move {
            self.singles.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(TranslationOutput {
                text: format!("译 {}", request.source),
                usage: TokenUsage::default(),
            })
        })
    }

    fn translate_batch<'a>(
        &'a self,
        requests: &'a [TranslationRequest],
        _on_partial: BatchPartialFn<'a>,
    ) -> BoxFuture<'a, Result<BatchOutput, TranslateError>> {
        Box::pin(async move {
            self.batches.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(300)).await;
            if let Some(e) = &self.batch_error {
                return Err(e.clone());
            }
            let mut texts: Vec<Option<String>> = requests
                .iter()
                .map(|r| Some(format!("译 {}", r.source)))
                .collect();
            if self.swap {
                texts.swap(0, 1);
            }
            Ok(BatchOutput {
                texts,
                usage: TokenUsage::default(),
            })
        })
    }
}

fn echo_engine(t: Arc<Echo>, mode: Mode, paras: &[&str]) -> Engine {
    let engine = Engine::new(
        t,
        Arc::new(MemoryCache::default()),
        Arc::new(RecordingSink::default()),
        EngineSettings {
            batch_size: 4,
            ..EngineSettings::default()
        },
        tokio::runtime::Handle::current(),
    );
    engine.load(paras.join("\n\n"), mode);
    engine
}

fn echoed(paras: &[&str]) -> Vec<Option<String>> {
    paras.iter().map(|p| Some(format!("译 {p}"))).collect()
}

#[tokio::test(start_paused = true)]
async fn swapped_answers_with_math_are_not_used() {
    let paras = ["We define $x$ here.", "We bound $y$ there.", "Plain words."];
    let t = Echo::new(true, None);
    let engine = echo_engine(t.clone(), Mode::Latex, &paras);
    settle(&engine).await;
    assert_eq!(texts(&engine), echoed(&paras));
    assert_eq!(t.batches.load(Ordering::SeqCst), 1);
    assert_eq!(t.singles.load(Ordering::SeqCst), 2, "the swapped two alone");
}

#[tokio::test(start_paused = true)]
async fn swapped_answers_with_names_are_not_used() {
    let paras = [
        "BERT is evaluated on GLUE.",
        "ResNet is trained on ImageNet.",
        "Plain words.",
    ];
    let t = Echo::new(true, None);
    let engine = echo_engine(t.clone(), Mode::Plain, &paras);
    settle(&engine).await;
    assert_eq!(texts(&engine), echoed(&paras));
    assert_eq!(t.singles.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn a_batch_the_answer_does_not_fit_is_sent_one_by_one() {
    let paras = ["First words.", "Second words.", "Third words."];
    let cut = TranslateError::InvalidResponse("the translation was cut off".into());
    let t = Echo::new(false, Some(cut));
    let engine = echo_engine(t.clone(), Mode::Plain, &paras);
    settle(&engine).await;
    assert_eq!(texts(&engine), echoed(&paras));
    assert_eq!(t.batches.load(Ordering::SeqCst), 1);
    assert_eq!(t.singles.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn a_rejected_key_fails_the_batch_without_splitting_it() {
    let paras = ["First words.", "Second words.", "Third words."];
    for invalid in [
        TranslateError::Rejected {
            status: 401,
            message: "invalid key".into(),
        },
        // As Gemini reports a bad key.
        TranslateError::Rejected {
            status: 400,
            message: "API key not valid. Please pass a valid API key.".into(),
        },
    ] {
        let t = Echo::new(false, Some(invalid));
        let engine = echo_engine(t.clone(), Mode::Plain, &paras);
        settle(&engine).await;
        assert!(
            engine
                .snapshot()
                .states
                .iter()
                .all(|s| s.status == SegmentStatus::Error)
        );
        assert_eq!(t.singles.load(Ordering::SeqCst), 0);
    }
}

/// Streams "半", starts over (another key) and then takes its time.
struct Restarts;

impl Translator for Restarts {
    fn provider(&self) -> &str {
        "restarts"
    }

    fn model(&self) -> &str {
        "restarts"
    }

    fn translate<'a>(
        &'a self,
        request: &'a TranslationRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<TranslationOutput, TranslateError>> {
        Box::pin(async move {
            on_partial("半");
            on_partial("");
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(TranslationOutput {
                text: format!("译 {}", request.source),
                usage: TokenUsage::default(),
            })
        })
    }

    fn translate_batch<'a>(
        &'a self,
        requests: &'a [TranslationRequest],
        on_partial: BatchPartialFn<'a>,
    ) -> BoxFuture<'a, Result<BatchOutput, TranslateError>> {
        Box::pin(async move {
            on_partial(0, "半");
            on_partial(1, "半");
            on_partial(0, "");
            on_partial(1, "");
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(BatchOutput {
                texts: requests
                    .iter()
                    .map(|r| Some(format!("译 {}", r.source)))
                    .collect(),
                usage: TokenUsage::default(),
            })
        })
    }
}

#[tokio::test(start_paused = true)]
async fn output_of_an_attempt_that_starts_over_is_cleared_at_once() {
    for batch_size in [1, 2] {
        let engine = Engine::new(
            Arc::new(Restarts),
            Arc::new(MemoryCache::default()),
            Arc::new(RecordingSink::default()),
            EngineSettings {
                batch_size,
                stream_throttle: Duration::from_secs(1),
                ..EngineSettings::default()
            },
            tokio::runtime::Handle::current(),
        );
        engine.load("One.\n\nTwo.".into(), Mode::Plain);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let states = engine.snapshot().states;
        assert!(
            states.iter().all(|s| !s.partial && s.text.is_none()),
            "batch size {batch_size}: {states:?}"
        );
        settle(&engine).await;
    }
}

#[tokio::test(start_paused = true)]
async fn placeholder_numbers_of_a_batch_do_not_match_glossary_terms() {
    // The second paragraph's placeholder is ⟦12⟧ in the batch: the term
    // "12" must not count as mentioned there.
    let first: String = (0..12).map(|i| format!("${i}x$ ")).collect();
    let text = format!("We list {first}here.\n\nThen $y$ follows.");
    let t = Echo::new(false, None);
    let engine = Engine::new(
        t.clone(),
        Arc::new(MemoryCache::default()),
        Arc::new(RecordingSink::default()),
        EngineSettings {
            batch_size: 4,
            ..EngineSettings::default()
        },
        tokio::runtime::Handle::current(),
    );
    let glossary = vec![biwrite_core::GlossaryEntry::new("12", Some("十二"))];
    engine.set_glossary(glossary.clone());
    engine.load(text, Mode::Latex);
    settle(&engine).await;
    assert_eq!(t.batches.load(Ordering::SeqCst), 1);
    let requests = t.batches.load(Ordering::SeqCst) + t.singles.load(Ordering::SeqCst);
    engine.set_glossary(glossary);
    settle(&engine).await;
    assert_eq!(
        t.batches.load(Ordering::SeqCst) + t.singles.load(Ordering::SeqCst),
        requests,
        "the same glossary sent a paragraph again"
    );
}
