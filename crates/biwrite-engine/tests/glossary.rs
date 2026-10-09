//! Glossary: only paragraphs that mention a changed term are retranslated,
//! and each request carries only the entries its paragraph mentions.

mod common;

use std::sync::Arc;
use std::time::Duration;

use biwrite_core::{GlossaryEntry, Mode};
use biwrite_engine::{Engine, EngineSettings, NullSink, SegmentStatus};
use common::{Scripted, all_translated, harness, settle, state_of};

const TEXT: &str =
    "Graph neural networks are strong.\n\nWe study prompting.\n\nGNNs and prompts meet here.";

fn entry(term: &str, translation: Option<&str>) -> GlossaryEntry {
    GlossaryEntry::new(term, translation)
}

#[tokio::test(start_paused = true)]
async fn changing_a_term_retranslates_only_paragraphs_that_use_it() {
    let h = harness(EngineSettings::default());
    h.engine.load(TEXT.into(), Mode::Plain);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 3);

    h.engine.set_glossary(vec![entry("prompt", Some("提示"))]);
    settle(&h.engine).await;
    // "prompting" is not "prompt"; "prompts" is.
    assert_eq!(h.translator.calls(), 4);
    let req = h.translator.requests().pop().unwrap();
    assert_eq!(req.source, "GNNs and prompts meet here.");
    assert_eq!(req.glossary, vec![entry("prompt", Some("提示"))]);
    // The source didn't change: the old translation is revised to follow
    // the glossary.
    let rev = req.revision.expect("revise the existing translation");
    assert_eq!(rev.old_source, req.source);

    // Adding an unrelated entry changes nothing; adding GNN touches the
    // first and last paragraphs. Each request carries only its own terms.
    h.engine.set_glossary(vec![
        entry("prompt", Some("提示")),
        entry("transformer", None),
        entry("GNN", None),
        entry("graph neural network", Some("图神经网络")),
    ]);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 6);
    let reqs = h.translator.requests();
    let first = reqs
        .iter()
        .rev()
        .find(|r| r.source.starts_with("Graph"))
        .unwrap();
    assert_eq!(
        first.glossary,
        vec![entry("graph neural network", Some("图神经网络"))]
    );
    let last = reqs
        .iter()
        .rev()
        .find(|r| r.source.starts_with("GNNs"))
        .unwrap();
    assert_eq!(
        last.glossary,
        vec![entry("GNN", None), entry("prompt", Some("提示"))]
    );
    assert!(all_translated(&h.engine));
}

#[tokio::test(start_paused = true)]
async fn removing_terms_returns_to_cached_translations() {
    let h = harness(EngineSettings::default());
    h.engine.load(TEXT.into(), Mode::Plain);
    settle(&h.engine).await;
    h.engine.set_glossary(vec![entry("GNN", None)]);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 4);
    // Without the entry, the earlier translation is in the cache.
    h.engine.set_glossary(Vec::new());
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 4);
    assert!(all_translated(&h.engine));
}

#[tokio::test(start_paused = true)]
async fn glossary_changes_respect_pause() {
    let h = harness(EngineSettings::default());
    h.engine.load(TEXT.into(), Mode::Plain);
    settle(&h.engine).await;
    h.engine.set_auto_translate(false);
    h.engine.set_glossary(vec![entry("prompt", Some("提示"))]);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 3);
    let id = h.engine.snapshot().layout[2].id;
    let state = state_of(&h.engine, id);
    assert_eq!(state.status, SegmentStatus::Stale);
    assert!(state.text.is_some(), "the old translation stays visible");
    h.engine.set_auto_translate(true);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 4);
}

#[tokio::test(start_paused = true)]
async fn in_flight_requests_with_an_old_glossary_are_redone() {
    let h = harness(EngineSettings::default());
    h.engine.load("GNNs are strong.".into(), Mode::Plain);
    // The first request is still running when the glossary changes.
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.engine.set_glossary(vec![entry("GNN", None)]);
    settle(&h.engine).await;
    let reqs = h.translator.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].glossary, vec![entry("GNN", None)]);
    assert!(all_translated(&h.engine));
}

#[tokio::test(start_paused = true)]
async fn unaffected_paragraphs_stay_cached_across_sessions() {
    let cache = Arc::new(biwrite_engine::MemoryCache::default());
    let session = |t: Arc<Scripted>, glossary: Vec<GlossaryEntry>| {
        let engine = Engine::new(
            t,
            cache.clone(),
            Arc::new(NullSink),
            EngineSettings::default(),
            tokio::runtime::Handle::current(),
        );
        engine.set_glossary(glossary);
        engine
    };
    let first = Scripted::new(Duration::from_millis(100));
    let engine = session(first.clone(), Vec::new());
    engine.load(TEXT.into(), Mode::Plain);
    settle(&engine).await;
    assert_eq!(first.calls(), 3);

    // Reopen with a glossary that mentions one paragraph.
    let second = Scripted::new(Duration::from_millis(100));
    let engine = session(second.clone(), vec![entry("prompt", Some("提示"))]);
    engine.load(TEXT.into(), Mode::Plain);
    settle(&engine).await;
    assert_eq!(second.calls(), 1);
    assert_eq!(engine.usage().cache_hits, 2);
}

#[tokio::test(start_paused = true)]
async fn chinese_drafting_keeps_the_users_english() {
    let h = harness(EngineSettings::default());
    h.engine.load(TEXT.into(), Mode::Plain);
    settle(&h.engine).await;
    // The "Chinese" is the reversed English; ".gnorts era ..." mentions the
    // rendering "gnorts".
    let swapped = h.engine.swap(TEXT.into()).unwrap();
    settle(&h.engine).await;
    let glossary = vec![entry("strong", Some("gnorts"))];
    h.engine.set_glossary(glossary);
    settle(&h.engine).await;
    // That paragraph's English is the user's own (an exact original): kept.
    assert_eq!(h.translator.calls(), 3);
    assert_eq!(h.engine.compose_target().unwrap(), TEXT);

    // New Chinese text is translated with the entry, shown as 中文 → English.
    h.engine
        .update(format!("{}\n\n新段落 gnorts。", swapped.text));
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 4);
    let req = h.translator.requests().pop().unwrap();
    assert_eq!(req.glossary, vec![entry("gnorts", Some("strong"))]);
}
