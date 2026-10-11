//! An edited paragraph is translated once it has stayed unchanged for the
//! edit delay. Typing on does not send a request after every pause, one
//! request goes out when the author stops, and a swap or a build that needs
//! the translation sends it at once.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use biwrite_core::Mode;
use biwrite_engine::{EngineSettings, SegmentStatus};
use common::{Harness, harness, join, loaded, paragraphs, settle};

const DELAY: Duration = Duration::from_secs(5);

async fn delayed() -> (Harness, Vec<String>) {
    let h = harness(EngineSettings {
        edit_delay: DELAY,
        ..EngineSettings::default()
    });
    let paras = paragraphs(3);
    // Opening a document translates it at once: nothing was edited.
    loaded(&h, &join(&paras)).await;
    (h, paras)
}

fn edit(paras: &[String], i: usize, extra: &str) -> String {
    let mut p = paras.to_vec();
    p[i] = format!("{} {extra}", p[i]);
    join(&p)
}

/// The status of the paragraph that starts as `Paragraph {i} ` (ASCII
/// text, so the layout's offsets are byte offsets).
fn status_of_paragraph(h: &Harness, i: usize) -> SegmentStatus {
    let text = h.engine.text();
    let snap = h.engine.snapshot();
    let start = format!("Paragraph {i} ");
    let id = snap
        .layout
        .iter()
        .find(|l| text[l.from..l.to].starts_with(&start))
        .expect("the paragraph")
        .id;
    snap.states.into_iter().find(|s| s.id == id).unwrap().status
}

#[tokio::test(start_paused = true)]
async fn typing_on_waits_and_one_request_goes_when_it_stops() {
    let (h, paras) = delayed().await;
    let calls = h.translator.calls();
    // Typing in paragraph 1, a pause of two seconds between changes.
    for (k, extra) in ["More", "More words", "More words here."]
        .into_iter()
        .enumerate()
    {
        h.engine.update(edit(&paras, 1, extra));
        if k < 2 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            assert_eq!(h.translator.calls(), calls, "no request while typing");
        }
    }
    assert_eq!(status_of_paragraph(&h, 1), SegmentStatus::Queued);
    assert!(
        h.engine.pending() > 0,
        "the held paragraph counts as pending"
    );
    // Four seconds after the last change: still quiet.
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(h.translator.calls(), calls);
    // Past five seconds: one request, for the final text.
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), calls + 1);
    assert_eq!(status_of_paragraph(&h, 1), SegmentStatus::Translated);
    let last = h.translator.requests().pop().unwrap();
    assert!(last.source.contains("More words here."), "{}", last.source);
}

#[tokio::test(start_paused = true)]
async fn a_held_paragraph_is_translated_at_once_when_asked() {
    let (h, paras) = delayed().await;
    let calls = h.translator.calls();
    h.engine.update(edit(&paras, 2, "Swapping soon."));
    assert_eq!(h.engine.translate_held(), 1);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), calls + 1);
    assert_eq!(status_of_paragraph(&h, 2), SegmentStatus::Translated);
    assert_eq!(h.engine.translate_held(), 0, "nothing left to release");
}

#[tokio::test(start_paused = true)]
async fn without_a_delay_an_edit_is_translated_at_once() {
    let h = harness(EngineSettings::default());
    let paras = paragraphs(2);
    loaded(&h, &join(&paras)).await;
    let calls = h.translator.calls();
    h.engine.update(edit(&paras, 0, "Now."));
    // The request is out before any delay could pass.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(h.translator.calls(), calls + 1);
    settle(&h.engine).await;
}

#[tokio::test(start_paused = true)]
async fn setting_the_delay_to_zero_releases_what_waits() {
    let (h, paras) = delayed().await;
    let calls = h.translator.calls();
    h.engine.update(edit(&paras, 0, "Changed."));
    h.engine.set_edit_delay(Duration::ZERO);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(h.translator.calls(), calls + 1);
    settle(&h.engine).await;
    // Later edits go at once too.
    h.engine.update(edit(&paras, 0, "Changed again."));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(h.translator.calls(), calls + 2);
    settle(&h.engine).await;
}

/// Retranslating the paragraph, or continuing with what is left, does not
/// wait for the edit delay.
#[tokio::test(start_paused = true)]
async fn asking_for_a_translation_skips_the_delay() {
    let (h, paras) = delayed().await;
    let calls = h.translator.calls();
    h.engine.update(edit(&paras, 0, "Retranslate me."));
    let id = {
        let text = h.engine.text();
        h.engine
            .snapshot()
            .layout
            .iter()
            .find(|l| text[l.from..l.to].starts_with("Paragraph 0 "))
            .unwrap()
            .id
    };
    h.engine.retranslate(id).unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(h.translator.calls(), calls + 1);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), calls + 1, "not sent again later");

    h.engine.update(edit(&paras, 2, "Continue with me."));
    assert_eq!(h.engine.continue_translation(false), 1);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(h.translator.calls(), calls + 2);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), calls + 2);
}

/// A new mode divides the text anew without anything typed, so its new
/// paragraphs are translated at once.
#[tokio::test(start_paused = true)]
async fn a_new_mode_is_translated_at_once() {
    let h = harness(EngineSettings {
        edit_delay: DELAY,
        ..EngineSettings::default()
    });
    let text = "# Methods\nWe build a graph from the table.\n\nWe train a model on it.";
    loaded(&h, text).await;
    let calls = h.translator.calls();
    h.engine.set_mode(Mode::Markdown, text.to_owned());
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(h.translator.calls() > calls, "no request went out");
    settle(&h.engine).await;
}
