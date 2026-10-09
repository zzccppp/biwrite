//! Acceptance tests from the spec, run against the engine with a scripted
//! translator in virtual time.

mod common;

use common::{all_translated, harness, join, loaded, paragraphs, reverse, settle, state_of};

use biwrite_engine::{EngineSettings, SegmentStatus};

#[tokio::test(start_paused = true)]
async fn editing_one_of_fifty_paragraphs_makes_exactly_one_call() {
    let h = harness(EngineSettings::default());
    let mut paras = paragraphs(50);
    loaded(&h, &join(&paras)).await;
    assert_eq!(h.translator.calls(), 50);

    paras[17] = paras[17].replace("plain words", "simple words");
    h.engine.update(join(&paras));
    settle(&h.engine).await;

    assert_eq!(h.translator.calls(), 51);
    assert!(all_translated(&h.engine));
    let snap = h.engine.snapshot();
    let id = snap.layout[17].id;
    assert_eq!(state_of(&h.engine, id).text, Some(reverse(&paras[17])));
}

#[tokio::test(start_paused = true)]
async fn undoing_an_edit_is_a_cache_hit() {
    let h = harness(EngineSettings::default());
    let original = paragraphs(50);
    loaded(&h, &join(&original)).await;

    let mut edited = original.clone();
    edited[3].push_str(" With an extra sentence.");
    h.engine.update(join(&edited));
    settle(&h.engine).await;
    let calls = h.translator.calls();

    h.engine.update(join(&original));
    settle(&h.engine).await;
    assert_eq!(
        h.translator.calls(),
        calls,
        "undo must not call the provider"
    );
    let id = h.engine.snapshot().layout[3].id;
    assert_eq!(state_of(&h.engine, id).text, Some(reverse(&original[3])));
}

#[tokio::test(start_paused = true)]
async fn undo_while_request_in_flight_makes_no_new_call() {
    let h = harness(EngineSettings::default());
    let original = paragraphs(10);
    loaded(&h, &join(&original)).await;

    let mut edited = original.clone();
    edited[2] = "Totally rewritten paragraph.".into();
    h.engine.update(join(&edited));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    h.engine.update(join(&original));
    settle(&h.engine).await;

    // One call for the edit (cancelled), none for the undo.
    assert_eq!(h.translator.calls(), 11);
    assert!(all_translated(&h.engine));
}

#[tokio::test(start_paused = true)]
async fn moving_a_paragraph_makes_no_calls() {
    let h = harness(EngineSettings::default());
    let mut paras = paragraphs(50);
    loaded(&h, &join(&paras)).await;
    let ids_before = h.engine.snapshot().layout;

    let moved = paras.remove(5);
    paras.insert(40, moved);
    let snap = h.engine.update(join(&paras));
    settle(&h.engine).await;

    assert_eq!(h.translator.calls(), 50);
    assert!(all_translated(&h.engine));
    // The moved block keeps its identity.
    assert_eq!(snap.layout[40].id, ids_before[5].id);
}

#[tokio::test(start_paused = true)]
async fn whitespace_only_edits_make_no_calls() {
    let h = harness(EngineSettings::default());
    let paras = paragraphs(5);
    loaded(&h, &join(&paras)).await;
    let rewrapped = join(&paras).replace(" of the ", "\nof   the ");
    h.engine.update(rewrapped);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 5);
}

#[tokio::test(start_paused = true)]
async fn cache_is_shared_across_documents() {
    let h = harness(EngineSettings::default());
    let paras = paragraphs(5);
    loaded(&h, &join(&paras)).await;
    // "Open" another file that shares two paragraphs.
    let other = join(&[paras[1].clone(), "Something new.".into(), paras[4].clone()]);
    h.engine.load(other, biwrite_core::Mode::Plain);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 6);
    assert_eq!(h.engine.usage().cache_hits, 2);
}

#[tokio::test(start_paused = true)]
async fn skipped_segments_are_never_sent() {
    let h = harness(EngineSettings::default());
    let text = "# Title\n\nBody text.\n\n```\ncode here\n```\n";
    h.engine.load(text.to_owned(), biwrite_core::Mode::Markdown);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 2);
    let snap = h.engine.snapshot();
    assert_eq!(snap.states[2].status, SegmentStatus::Skipped);
    let sources: Vec<String> = h
        .translator
        .requests()
        .into_iter()
        .map(|r| r.source)
        .collect();
    assert_eq!(sources, vec!["Title", "Body text."]);
}
