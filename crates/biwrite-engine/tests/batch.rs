//! Several segments per request ("paragraphs per request" above one): fresh
//! translations share requests, edits keep their own, a segment without a
//! usable answer is retried alone, and editing one segment of a running
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

#[tokio::test(start_paused = true)]
async fn a_paragraph_without_an_answer_is_retried_alone() {
    let t = Batcher::new(Duration::from_millis(700));
    t.withhold.lock().unwrap().insert(1);
    let (engine, sink) = engine(t.clone(), 3, 4);
    let paras = paragraphs(3);
    engine.load(join(&paras), Mode::Plain);
    settle(&engine).await;
    let expected: Vec<Option<String>> = paras.iter().map(|p| Some(reverse(p))).collect();
    assert_eq!(texts(&engine), expected);
    assert_eq!(*t.batches.lock().unwrap(), vec![3]);
    assert_eq!(t.singles.load(Ordering::SeqCst), 1, "only the missing one");
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
