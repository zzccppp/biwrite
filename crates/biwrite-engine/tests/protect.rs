//! Placeholder protection end to end: math, citations and references reach
//! the model as `⟦n⟧` and come back byte for byte, or the segment fails.

mod common;

use std::time::Duration;

use biwrite_core::Mode;
use biwrite_core::protect::spans;
use biwrite_core::segment::segment;
use biwrite_engine::{EngineSettings, SegmentStatus};
use common::{Scripted, all_translated, harness, masked, settle, state_of, strip_placeholders};

const PAPER: &str = include_str!("../../../samples/paper.tex");

/// A model that rewrites every word and reverses their order, carrying the
/// placeholders along.
fn rewrite(masked: &str) -> String {
    masked
        .split(' ')
        .rev()
        .map(|w| match w.find('⟦') {
            Some(i) => format!("译{}", &w[i..]),
            None => "译".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn protected(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = spans(text, Mode::Latex)
        .into_iter()
        .map(|s| &text[s.range])
        .collect();
    v.sort_unstable();
    v
}

#[tokio::test(start_paused = true)]
async fn paper_translations_keep_every_protected_span() {
    let translator = Scripted::mapping(Duration::from_millis(200), rewrite);
    let engine = common::engine_with(translator.clone());
    engine.load(PAPER.to_owned(), Mode::Latex);
    settle(&engine).await;
    assert!(all_translated(&engine));
    // No placeholder problems, so no retries.
    assert_eq!(translator.calls(), 22);

    let states = engine.snapshot().states;
    let mut checked = 0;
    for (seg, state) in segment(PAPER, Mode::Latex).iter().zip(&states) {
        if !seg.kind.is_translatable() {
            continue;
        }
        let source = seg.content(PAPER);
        let translation = state.text.as_deref().unwrap();
        assert_eq!(protected(translation), protected(source), "{source}");
        checked += protected(source).len();
    }
    assert_eq!(checked, 14, "every $…$, \\cite, \\ref, \\eqref occurrence");
    // The model never saw them.
    for req in translator.requests() {
        assert!(!req.source.contains('$') && !req.source.contains("\\cite"));
    }
}

#[tokio::test(start_paused = true)]
async fn dropped_placeholder_is_retried_once_then_fails() {
    let translator = Scripted::mapping(Duration::from_millis(200), |s| {
        strip_placeholders(&common::reverse(s))
    });
    let engine = common::engine_with(translator.clone());
    let text = "See \\cite{kipf} for details.\n\nOther text.";
    engine.load(text.to_owned(), Mode::Latex);
    settle(&engine).await;
    assert_eq!(
        translator.calls(),
        3,
        "two attempts for the citation, one for the rest"
    );

    let id = engine.snapshot().layout[0].id;
    let state = state_of(&engine, id);
    assert_eq!(state.status, SegmentStatus::Error);
    let error = state.error.unwrap();
    assert!(error.contains("missing ⟦0⟧ `\\cite{kipf}`"), "{error}");

    // Sticky: an unrelated edit doesn't retry it, an explicit retry does.
    engine.update(text.replace("Other", "More"));
    settle(&engine).await;
    assert_eq!(translator.calls(), 4);
    engine.retranslate(id).unwrap();
    settle(&engine).await;
    assert_eq!(translator.calls(), 6);
    assert_eq!(engine.usage().requests, 6);
}

#[tokio::test(start_paused = true)]
async fn one_bad_answer_is_fixed_by_the_retry() {
    let h = harness(EngineSettings::default());
    h.translator.drop_placeholders_next(1);
    h.engine
        .load("Bound $f(x)$ by \\ref{a}.".into(), Mode::Latex);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), 2);
    let id = h.engine.snapshot().layout[0].id;
    let state = state_of(&h.engine, id);
    assert_eq!(state.status, SegmentStatus::Translated);
    assert_eq!(state.text.as_deref(), Some(".\\ref{a} yb $f(x)$ dnuoB"));
    // Both attempts are counted.
    let usage = h.engine.usage();
    assert_eq!((usage.requests, usage.input_tokens), (2, 20));
    let notices = h.sink.notices.lock().unwrap().clone();
    assert!(
        notices.iter().any(|n| n.contains("protected text")),
        "{notices:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn streamed_partials_show_restored_text() {
    let h = harness(EngineSettings::default());
    h.engine.load("Let $x$ be \\ref{a}.".into(), Mode::Latex);
    settle(&h.engine).await;
    let states = h.sink.states.lock().unwrap().clone();
    let partials: Vec<String> = states
        .iter()
        .filter(|s| s.partial)
        .filter_map(|s| s.text.clone())
        .collect();
    assert!(!partials.is_empty());
    for p in partials {
        assert!(!p.contains('⟦') && p.contains("$x$"), "{p}");
    }
}

#[tokio::test(start_paused = true)]
async fn revise_mode_shares_placeholder_numbers() {
    let h = harness(EngineSettings::default());
    let base = "We bound $f(x)$ using \\cite{a} and the lemma in \\ref{s}.";
    h.engine.load(base.into(), Mode::Latex);
    settle(&h.engine).await;
    let edited = "We tightly bound $f(x)$ using \\cite{a} and \\cite{b}.";
    h.engine.update(edited.into());
    settle(&h.engine).await;

    let req = h.translator.requests().pop().unwrap();
    assert_eq!(req.source, "We tightly bound ⟦0⟧ using ⟦1⟧ and ⟦2⟧.");
    let rev = req.revision.expect("small edit uses revise mode");
    assert_eq!(
        rev.old_source,
        "We bound ⟦0⟧ using ⟦1⟧ and the lemma in ⟦3⟧."
    );
    assert_eq!(
        rev.old_translation,
        ".⟦3⟧ ni ammel eht dna ⟦1⟧ gnisu ⟦0⟧ dnuob eW"
    );
    let id = h.engine.snapshot().layout[0].id;
    assert_eq!(
        state_of(&h.engine, id).text.as_deref(),
        Some(".\\cite{b} dna \\cite{a} gnisu $f(x)$ dnuob ylthgit eW")
    );
    assert_eq!(masked(edited, Mode::Latex), req.source);
}

#[tokio::test(start_paused = true)]
async fn a_rejected_answer_counts_even_if_the_retry_is_cancelled() {
    let h = harness(EngineSettings::default());
    h.translator.drop_placeholders_next(1);
    h.engine.load("Bound $f(x)$ here.".into(), Mode::Latex);
    // The first answer (700 ms) is rejected; the retry is in flight when
    // the user edits the paragraph.
    tokio::time::sleep(Duration::from_millis(900)).await;
    h.engine.update("Bound $f(x)$ there.".into());
    settle(&h.engine).await;
    let usage = h.engine.usage();
    // The rejected first answer and the edited paragraph; the aborted retry
    // never returned.
    assert_eq!((usage.requests, usage.input_tokens), (2, 20));
}
