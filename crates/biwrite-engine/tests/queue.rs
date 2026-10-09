//! Queue behaviour: cancellation, concurrency, retries, pause, revise mode.

mod common;

use std::time::Duration;

use common::{
    Scripted, all_translated, harness, join, loaded, paragraphs, reverse, settle, state_of,
};

use biwrite_core::{Mode, SegmentId};
use biwrite_engine::{
    EngineError, EngineSettings, MemoryCache, MockTranslator, NullSink, SegmentStatus,
    TranslateError,
};

#[tokio::test(start_paused = true)]
async fn editing_during_flight_discards_stale_result() {
    let h = harness(EngineSettings::default());
    h.engine
        .load("First version of the text.".into(), Mode::Plain);
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.engine.update("Second version of the text.".into());
    settle(&h.engine).await;

    let id = h.engine.snapshot().layout[0].id;
    let state = state_of(&h.engine, id);
    assert_eq!(state.status, SegmentStatus::Translated);
    assert_eq!(state.text, Some(reverse("Second version of the text.")));
    // The first request was aborted; only one result was applied.
    assert_eq!(h.translator.calls(), 2);
    let applied: Vec<_> = h
        .sink
        .states
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.status == SegmentStatus::Translated)
        .filter_map(|s| s.text.clone())
        .collect();
    assert_eq!(applied, vec![reverse("Second version of the text.")]);
}

#[tokio::test(start_paused = true)]
async fn concurrency_limit_is_respected() {
    let mock = std::sync::Arc::new(MockTranslator::with_delay(
        Duration::from_millis(500),
        Duration::from_millis(2000),
    ));
    let engine = biwrite_engine::Engine::new(
        mock.clone(),
        std::sync::Arc::new(MemoryCache::default()),
        std::sync::Arc::new(NullSink),
        EngineSettings {
            concurrency: 3,
            ..EngineSettings::default()
        },
        tokio::runtime::Handle::current(),
    );
    engine.load(join(&paragraphs(20)), Mode::Plain);
    settle(&engine).await;
    assert_eq!(mock.calls(), 20);
    assert_eq!(mock.max_in_flight(), 3);
    assert!(all_translated(&engine));
}

#[tokio::test(start_paused = true)]
async fn rate_limits_are_retried_with_backoff() {
    let h = harness(EngineSettings::default());
    h.translator.fail_next([
        TranslateError::RateLimited { retry_after: None },
        TranslateError::Server {
            status: 503,
            retry_after: Some(Duration::from_secs(2)),
        },
    ]);
    loaded(&h, "Only paragraph.").await;
    assert_eq!(h.translator.calls(), 3);
    assert_eq!(h.engine.usage().requests, 3);
    assert_eq!(h.sink.notices.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn permanent_errors_are_sticky_until_retry() {
    let h = harness(EngineSettings::default());
    h.translator.fail_next([TranslateError::Rejected {
        status: 400,
        message: "bad request".into(),
    }]);
    h.engine
        .load("Broken paragraph.\n\nGood one.".into(), Mode::Plain);
    settle(&h.engine).await;
    let id = h.engine.snapshot().layout[0].id;
    assert_eq!(state_of(&h.engine, id).status, SegmentStatus::Error);
    assert!(
        state_of(&h.engine, id)
            .error
            .unwrap()
            .contains("bad request")
    );

    // An unrelated edit does not retry the failed segment.
    h.engine
        .update("Broken paragraph.\n\nGood one, edited.".into());
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 3);
    assert_eq!(state_of(&h.engine, id).status, SegmentStatus::Error);

    // Explicit retry does.
    h.engine.retranslate(id).unwrap();
    settle(&h.engine).await;
    assert_eq!(state_of(&h.engine, id).status, SegmentStatus::Translated);
    assert_eq!(h.translator.calls(), 4);
}

#[tokio::test(start_paused = true)]
async fn retranslate_bypasses_cache() {
    let h = harness(EngineSettings::default());
    loaded(&h, "One.\n\nTwo.").await;
    let id = h.engine.snapshot().layout[1].id;
    h.engine.retranslate(id).unwrap();
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 3);
    h.engine.retranslate_all();
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 5);
    assert_eq!(
        h.engine.retranslate(SegmentId(999)),
        Err(EngineError::UnknownSegment(SegmentId(999)))
    );
}

#[tokio::test(start_paused = true)]
async fn pause_marks_stale_and_resume_translates() {
    let h = harness(EngineSettings::default());
    let mut paras = paragraphs(3);
    loaded(&h, &join(&paras)).await;
    h.engine.set_auto_translate(false);

    paras[1] = "A brand new middle paragraph.".into();
    let snap = h.engine.update(join(&paras));
    settle(&h.engine).await;
    let id = snap.layout[1].id;
    assert_eq!(h.translator.calls(), 3);
    let state = state_of(&h.engine, id);
    assert_eq!(state.status, SegmentStatus::Stale);

    h.engine.set_auto_translate(true);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 4);
    assert_eq!(state_of(&h.engine, id).status, SegmentStatus::Translated);
}

#[tokio::test(start_paused = true)]
async fn revise_mode_for_small_edits_only() {
    let h = harness(EngineSettings::default());
    let base = "The model learns graph structure from context examples at inference time.";
    loaded(&h, base).await;

    h.engine.update(base.replace("learns", "infers"));
    settle(&h.engine).await;
    let small = h.translator.requests().pop().unwrap();
    let rev = small.revision.expect("small edit uses revise mode");
    assert_eq!(rev.old_source, base);
    assert_eq!(rev.old_translation, reverse(base));

    h.engine
        .update("Completely different sentence about other things.".into());
    settle(&h.engine).await;
    assert!(h.translator.requests().pop().unwrap().revision.is_none());
}

#[tokio::test(start_paused = true)]
async fn stand_in_output_is_not_revised() {
    let h = harness(EngineSettings::default());
    h.translator.set_stand_in(true);
    let base = "The model learns graph structure from context examples at inference time.";
    loaded(&h, base).await;
    h.engine.update(base.replace("learns", "infers"));
    settle(&h.engine).await;
    assert!(h.translator.requests().pop().unwrap().revision.is_none());
}

#[tokio::test(start_paused = true)]
async fn switching_from_the_mock_translates_from_scratch() {
    let h = harness(EngineSettings::default());
    h.translator.set_stand_in(true);
    loaded(&h, "The model learns graph structure.\n\nSecond paragraph.").await;

    let real = Scripted::other_provider(Duration::from_millis(300));
    h.engine.set_translator(real.clone());
    settle(&h.engine).await;
    let requests = real.requests();
    assert_eq!(requests.len(), 2);
    // The mock's reversed text is never offered as a translation to revise.
    assert!(
        requests.iter().all(|r| r.revision.is_none()),
        "{requests:?}"
    );
    assert!(all_translated(&h.engine));
}

#[tokio::test(start_paused = true)]
async fn requests_carry_neighbor_context_and_note() {
    let h = harness(EngineSettings::default());
    h.engine.set_doc_note(Some("ML paper".into()));
    loaded(&h, "Alpha.\n\nBeta.\n\nGamma.").await;
    let beta = h
        .translator
        .requests()
        .into_iter()
        .find(|r| r.source == "Beta.")
        .unwrap();
    assert_eq!(beta.context_before.as_deref(), Some("Alpha."));
    assert_eq!(beta.context_after.as_deref(), Some("Gamma."));
    assert_eq!(beta.doc_note.as_deref(), Some("ML paper"));
}

#[tokio::test(start_paused = true)]
async fn streaming_updates_are_published() {
    let h = harness(EngineSettings::default());
    loaded(&h, "Stream me.").await;
    let states = h.sink.states.lock().unwrap().clone();
    assert!(
        states
            .iter()
            .any(|s| s.status == SegmentStatus::Translating && s.partial)
    );
    // Versions increase monotonically per segment.
    let versions: Vec<u64> = states.iter().map(|s| s.version).collect();
    assert!(versions.windows(2).all(|w| w[0] < w[1]));
}

#[tokio::test(start_paused = true)]
async fn transient_errors_are_retried_on_the_next_pass() {
    let h = harness(EngineSettings {
        max_retries: 1,
        concurrency: 1,
        ..EngineSettings::default()
    });
    let outage = || TranslateError::Network("connection reset".into());
    h.translator.fail_next([outage(), outage()]);
    h.engine
        .load("Flaky paragraph.\n\nOther.".into(), Mode::Plain);
    settle(&h.engine).await;
    let id = h.engine.snapshot().layout[0].id;
    assert_eq!(state_of(&h.engine, id).status, SegmentStatus::Error);

    // Network is back: an unrelated edit re-queues the failed segment.
    h.engine.update("Flaky paragraph.\n\nOther, edited.".into());
    settle(&h.engine).await;
    assert_eq!(state_of(&h.engine, id).status, SegmentStatus::Translated);
    assert!(all_translated(&h.engine));
}

#[tokio::test(start_paused = true)]
async fn failed_attempt_partial_is_cleared_during_backoff() {
    let h = harness(EngineSettings::default());
    h.translator
        .fail_next([TranslateError::RateLimited { retry_after: None }]);
    loaded(&h, "Paragraph.").await;
    let states = h.sink.states.lock().unwrap().clone();
    let partial_at = states
        .iter()
        .position(|s| s.partial)
        .expect("streamed partial");
    assert!(
        states[partial_at + 1..]
            .iter()
            .any(|s| s.status == SegmentStatus::Translating && !s.partial),
        "partial output must be cleared before retrying"
    );
}

#[tokio::test(start_paused = true)]
async fn edits_jump_the_initial_backlog() {
    let h = harness(EngineSettings {
        concurrency: 1,
        ..EngineSettings::default()
    });
    let mut paras = paragraphs(30);
    h.engine.load(join(&paras), Mode::Plain);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Edit a paragraph deep in the not-yet-translated backlog.
    paras[25] = "An urgent rewrite of paragraph twenty-five.".into();
    h.engine.update(join(&paras));
    settle(&h.engine).await;

    let order: Vec<String> = h
        .translator
        .requests()
        .into_iter()
        .map(|r| r.source)
        .collect();
    // First request was already in flight; the edit goes right after it.
    assert_eq!(order[1], paras[25]);
    assert_eq!(order.len(), 30);
}

#[tokio::test(start_paused = true)]
async fn retranslating_an_empty_heading_sends_nothing() {
    let h = harness(EngineSettings::default());
    h.engine.load("# \n\nBody.".into(), Mode::Markdown);
    settle(&h.engine).await;
    let id = h.engine.snapshot().layout[0].id;
    h.engine.retranslate(id).unwrap();
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 1);
    assert_eq!(state_of(&h.engine, id).status, SegmentStatus::Translated);
}

#[tokio::test(start_paused = true)]
async fn switching_translator_retries_failed_segments() {
    let h = harness(EngineSettings::default());
    h.translator
        .fail_next([TranslateError::Config("No API key".into())]);
    loaded_with_error(&h).await;
    let id = h.engine.snapshot().layout[0].id;
    assert_eq!(state_of(&h.engine, id).status, SegmentStatus::Error);

    let fresh = common::Scripted::new(Duration::from_millis(100));
    h.engine.set_translator(fresh.clone());
    settle(&h.engine).await;
    assert_eq!(state_of(&h.engine, id).status, SegmentStatus::Translated);
    assert_eq!(fresh.calls(), 1);
}

async fn loaded_with_error(h: &common::Harness) {
    h.engine.load("Needs a key.".into(), Mode::Plain);
    settle(&h.engine).await;
}

#[tokio::test(start_paused = true)]
async fn switching_translator_redoes_in_flight_and_mock_output() {
    let mock = std::sync::Arc::new(MockTranslator::with_delay(
        Duration::from_millis(500),
        Duration::from_millis(500),
    ));
    let engine = biwrite_engine::Engine::new(
        mock.clone(),
        std::sync::Arc::new(MemoryCache::default()),
        std::sync::Arc::new(NullSink),
        EngineSettings {
            concurrency: 2,
            ..EngineSettings::default()
        },
        tokio::runtime::Handle::current(),
    );
    let paras = paragraphs(6);
    engine.load(join(&paras), Mode::Plain);
    tokio::time::sleep(Duration::from_millis(600)).await; // first two done by the mock
    let real = common::Scripted::new(Duration::from_millis(100));
    engine.set_translator(real.clone());
    settle(&engine).await;

    // Every paragraph (mock-translated, in flight or queued) ends up real.
    assert_eq!(real.calls(), 6);
    let snap = engine.snapshot();
    for (layout, para) in snap.layout.iter().zip(&paras) {
        assert_eq!(state_of(&engine, layout.id).text, Some(reverse(para)));
    }

    // Switching between real translators keeps existing translations.
    engine.set_translator(common::Scripted::new(Duration::from_millis(100)));
    settle(&engine).await;
    assert!(all_translated(&engine));
    assert_eq!(real.calls(), 6);
}
