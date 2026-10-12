//! Paragraphs left in the other language by an early swap are translated
//! afterwards and put in place in the editor (fills), while their
//! translation stays the exact original; "continue" takes up what is left.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use biwrite_core::{Direction, Mode, SegmentId};
use biwrite_engine::{
    BatchOutput, BatchPartialFn, BoxFuture, EngineSettings, Fill, PartialFn, SegmentStatus,
    TokenUsage, TranslateError, TranslationOutput, TranslationRequest, Translator,
};
use common::{Harness, Scripted, all_translated, harness, join, paragraphs, settle};

/// A toy translator between English and "Chinese": lowercase letters map
/// to Han characters and back, so the language of every text is plain and
/// translating twice gives the original.
const HAN: [char; 26] = [
    '阿', '波', '次', '得', '鹅', '佛', '哥', '喝', '衣', '鸡', '科', '乐', '摸', '呢', '哦', '坡',
    '期', '日', '思', '特', '乌', '维', '窝', '西', '鸭', '子',
];

fn toy(s: &str) -> String {
    if s.chars().any(|c| HAN.contains(&c)) {
        s.chars()
            .map(|c| match HAN.iter().position(|&h| h == c) {
                Some(i) => (b'a' + i as u8) as char,
                None => c,
            })
            .collect()
    } else {
        s.chars()
            .map(|c| match c {
                'a'..='z' => HAN[(c as u8 - b'a') as usize],
                _ => c,
            })
            .collect()
    }
}

fn toy_harness(settings: EngineSettings) -> Harness {
    let mut h = harness(settings);
    let translator = Scripted::mapping(Duration::from_millis(700), toy);
    h.engine.set_translator(translator.clone());
    h.translator = translator;
    h
}

fn fills(h: &Harness) -> Vec<Fill> {
    h.sink.fills.lock().unwrap().clone()
}

/// What the editor does with fills: each replaces its paragraph if the
/// paragraph still reads as it did.
fn apply(text: &str, fills: &[Fill]) -> String {
    let mut out = text.to_owned();
    for f in fills {
        if let Some(at) = out.find(&f.old) {
            out.replace_range(at..at + f.old.len(), &f.new);
        }
    }
    out
}

/// Four paragraphs, two at a time: after a second the first two are
/// translated and the other two are on their way.
async fn half_translated() -> (Harness, Vec<String>) {
    let h = toy_harness(EngineSettings {
        concurrency: 2,
        ..EngineSettings::default()
    });
    let paras = paragraphs(4);
    h.engine.load(join(&paras), Mode::Plain);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    (h, paras)
}

#[tokio::test(start_paused = true)]
async fn an_early_swap_fills_in_the_paragraphs_not_translated_yet() {
    let (h, paras) = half_translated().await;
    let english = join(&paras);
    let swapped = h.engine.swap_keeping_untranslated(english.clone()).unwrap();
    let blocks: Vec<&str> = swapped.text.split("\n\n").collect();
    assert_eq!(blocks[0], toy(&paras[0]));
    assert_eq!(blocks[1], toy(&paras[1]));
    assert_eq!(blocks[2], paras[2], "kept in English for now");
    assert_eq!(blocks[3], paras[3]);
    settle(&h.engine).await;

    // Both kept paragraphs were translated English to Chinese and sent.
    let fills = fills(&h);
    assert_eq!(fills.len(), 2);
    let sent: Vec<Direction> = h
        .translator
        .requests()
        .iter()
        .map(|r| r.direction)
        .collect();
    assert_eq!(sent.iter().filter(|d| **d == Direction::EnZh).count(), 6);
    // Their translations on the right stayed the exact originals throughout.
    let ids: Vec<_> = fills.iter().map(|f| f.id).collect();
    for state in h
        .sink
        .states
        .lock()
        .unwrap()
        .iter()
        .filter(|s| ids.contains(&s.id))
    {
        assert!(!state.partial, "no streamed text on the right for a fill");
        if let Some(text) = &state.text {
            assert!(
                paras.contains(text),
                "the right side kept its English: {text}"
            );
        }
    }

    // The editor puts them in; that costs no request and swaps back exactly.
    let filled = apply(&swapped.text, &fills);
    assert_eq!(
        filled,
        join(&paras.iter().map(|p| toy(p)).collect::<Vec<_>>())
    );
    let calls = h.translator.calls();
    h.engine.update(filled.clone());
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), calls);
    assert!(all_translated(&h.engine));
    let back = h.engine.swap(filled).unwrap();
    assert_eq!(back.text, english);
}

#[tokio::test(start_paused = true)]
async fn a_paused_swap_waits_for_continue() {
    let (h, paras) = half_translated().await;
    h.engine.set_auto_translate(false);
    let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    settle(&h.engine).await;
    assert!(fills(&h).is_empty(), "nothing is translated while paused");

    assert_eq!(h.engine.continue_translation(true), 2);
    settle(&h.engine).await;
    let fills = fills(&h);
    assert_eq!(fills.len(), 2);
    let filled = apply(&swapped.text, &fills);
    assert!(!filled.contains("Paragraph 2"), "{filled}");
    // Nothing is left now.
    h.engine.update(filled);
    settle(&h.engine).await;
    assert_eq!(h.engine.continue_translation(true), 0);
}

#[tokio::test(start_paused = true)]
async fn a_paragraph_edited_before_its_fill_keeps_the_edit() {
    let (h, paras) = half_translated().await;
    let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    // The author rewrites the third paragraph before its translation is back.
    let edited = swapped.text.replace(&paras[2], "My own words here.");
    h.engine.update(edited.clone());
    settle(&h.engine).await;
    let fills = fills(&h);
    assert_eq!(fills.len(), 1, "only the untouched paragraph is filled");
    assert_eq!(fills[0].old, paras[3]);
    // Rewritten from scratch, the paragraph is a new one, translated like
    // any edit of the Chinese side.
    let filled = apply(&edited, &fills);
    assert!(filled.contains("My own words here."));
    h.engine.update(filled.clone());
    settle(&h.engine).await;
    assert!(h.engine.compose_target().is_ok());
}

#[tokio::test(start_paused = true)]
async fn swapping_back_before_the_fills_restores_the_original() {
    let (h, paras) = half_translated().await;
    let english = join(&paras);
    let swapped = h.engine.swap_keeping_untranslated(english.clone()).unwrap();
    // Straight back, while the fills are still on their way.
    let back = h.engine.swap(swapped.text).unwrap();
    assert_eq!(back.text, english);
    settle(&h.engine).await;
    assert!(
        fills(&h).is_empty(),
        "fills of the old document are dropped"
    );
    assert_eq!(h.engine.direction(), Direction::EnZh);
}

/// The paragraphs a fill is on its way for went over in English and have
/// themselves as their translation, a stand-in. Swapped straight back, they
/// are no translations of themselves: they are translated afresh, and the
/// Chinese has no English left in it.
#[tokio::test(start_paused = true)]
async fn swapped_back_before_their_fills_paragraphs_are_translated_afresh() {
    let (h, paras) = half_translated().await;
    let english = join(&paras);
    let swapped = h.engine.swap_keeping_untranslated(english.clone()).unwrap();
    assert_eq!(h.engine.standins().len(), 2);
    let back = h.engine.swap(swapped.text).unwrap();
    assert_eq!(back.text, english);
    assert!(h.engine.standins().is_empty());
    settle(&h.engine).await;
    let chinese = join(&paras.iter().map(|p| toy(p)).collect::<Vec<_>>());
    assert_eq!(h.engine.compose_target().unwrap(), chinese);
}

/// A fill that fails leaves its paragraph in English, still a stand-in:
/// swapped back, it is translated afresh too.
#[tokio::test(start_paused = true)]
async fn a_failed_fill_leaves_a_stand_in() {
    let (h, paras) = half_translated().await;
    let english = join(&paras);
    let rejected = || TranslateError::Rejected {
        status: 400,
        message: "bad request".into(),
    };
    h.translator.fail_next([rejected(), rejected()]);
    let swapped = h.engine.swap_keeping_untranslated(english.clone()).unwrap();
    settle(&h.engine).await;
    assert!(fills(&h).is_empty(), "both fills failed");
    assert_eq!(h.engine.standins().len(), 2);
    h.engine.swap(swapped.text).unwrap();
    settle(&h.engine).await;
    let chinese = join(&paras.iter().map(|p| toy(p)).collect::<Vec<_>>());
    assert_eq!(h.engine.compose_target().unwrap(), chinese);
}

/// Paused, the stand-ins wait for their fills; swapped back, their
/// counterparts wait for a translation instead of taking them for one.
#[tokio::test(start_paused = true)]
async fn a_paused_swap_back_waits_for_translations() {
    let (h, paras) = half_translated().await;
    h.engine.set_auto_translate(false);
    let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    h.engine.swap(swapped.text).unwrap();
    settle(&h.engine).await;
    assert!(
        h.engine.compose_target().is_err(),
        "two paragraphs wait for their translations"
    );
    assert_eq!(h.engine.continue_translation(false), 2);
    settle(&h.engine).await;
    let chinese = join(&paras.iter().map(|p| toy(p)).collect::<Vec<_>>());
    assert_eq!(h.engine.compose_target().unwrap(), chinese);
}

/// Continue takes up stand-ins whatever side is edited (it is told
/// `other_language: false` with a pair or back home), a failed fill too.
#[tokio::test(start_paused = true)]
async fn continue_takes_up_a_stand_in_whose_fill_failed() {
    let (h, paras) = half_translated().await;
    let rejected = || TranslateError::Rejected {
        status: 400,
        message: "bad request".into(),
    };
    h.translator.fail_next([rejected(), rejected()]);
    let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    settle(&h.engine).await;
    assert!(fills(&h).is_empty());
    assert_eq!(h.engine.continue_translation(false), 2);
    settle(&h.engine).await;
    let fills = fills(&h);
    assert_eq!(fills.len(), 2);
    h.engine.update(apply(&swapped.text, &fills));
    assert!(h.engine.standins().is_empty());
}

/// A stand-in edited before its fill, still in English, stays one: its
/// edited English is its translation exactly, nothing is sent for it, and
/// swapped back it is translated afresh.
#[tokio::test(start_paused = true)]
async fn an_edited_stand_in_stays_one() {
    let (h, paras) = half_translated().await;
    h.engine.set_auto_translate(false);
    let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    let edited = swapped
        .text
        .replace("finding number 2", "finding number two");
    h.engine.update(edited.clone());
    assert_eq!(h.engine.standins().len(), 2);
    h.engine.set_auto_translate(true);
    settle(&h.engine).await;
    let english = h.engine.compose_target().unwrap();
    assert!(english.contains("finding number two"), "{english}");
    let back = h.engine.swap(edited).unwrap();
    assert_eq!(back.text, english);
    settle(&h.engine).await;
    // Both in Chinese (the untouched one from its fill's cached answer).
    assert_eq!(h.engine.compose_target().unwrap(), toy(&english));
}

/// Continue sends the fill, then the author edits the paragraph, still in
/// English, before it lands: it stays a stand-in, its edit its translation.
#[tokio::test(start_paused = true)]
async fn a_stand_in_edited_after_continue_stays_one() {
    let (h, paras) = half_translated().await;
    h.engine.set_auto_translate(false);
    let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    assert_eq!(h.engine.continue_translation(false), 2);
    let edited = swapped
        .text
        .replace("finding number 2", "finding number two");
    h.engine.update(edited);
    assert_eq!(h.engine.standins().len(), 2);
    settle(&h.engine).await;
    assert_eq!(fills(&h).len(), 1, "only the untouched one is filled");
    assert!(
        h.engine.standins().len() == 2,
        "the filled one until the editor puts it in"
    );
    let english = h.engine.compose_target().unwrap();
    assert!(english.contains("finding number two"), "{english}");
}

/// The offline translator's answer to a fill is no translation: the
/// paragraph stays as it is, a stand-in, for a real translator to fill.
#[tokio::test(start_paused = true)]
async fn the_offline_translator_leaves_stand_ins_as_they_are() {
    let (h, paras) = half_translated().await;
    h.translator.set_stand_in(true);
    let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    settle(&h.engine).await;
    assert!(fills(&h).is_empty(), "{:?}", fills(&h));
    assert_eq!(h.engine.standins().len(), 2);
    h.engine.swap(swapped.text).unwrap();
    assert!(h.engine.standins().is_empty());
}

/// Retranslating a stand-in whose fill failed puts it into the edited
/// language: its translation stays its own text.
#[tokio::test(start_paused = true)]
async fn retranslating_a_stand_in_fills_it() {
    let (h, paras) = half_translated().await;
    let rejected = || TranslateError::Rejected {
        status: 400,
        message: "bad request".into(),
    };
    h.translator.fail_next([rejected(), rejected()]);
    h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    settle(&h.engine).await;
    assert!(fills(&h).is_empty(), "both fills failed");
    let id = *h.engine.standins().iter().next().unwrap();
    h.engine.retranslate(id).unwrap();
    settle(&h.engine).await;
    let fills = fills(&h);
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(fills[0].id, id);
    assert!(h.engine.has_exact_translation(id));
}

/// Chinese only partly put into English: the first half of its Han
/// characters, the rest as they are. English is put into Chinese whole.
fn partly(s: &str) -> String {
    if !s.chars().any(|c| HAN.contains(&c)) {
        return toy(s);
    }
    let mut left = s.chars().filter(|c| HAN.contains(c)).count() / 2;
    s.chars()
        .map(|c| match HAN.iter().position(|&h| h == c) {
            Some(i) if left > 0 => {
                left -= 1;
                (b'a' + i as u8) as char
            }
            _ => c,
        })
        .collect()
}

/// A fill that comes back only partly in English is put in, and the
/// paragraph's translation is the author's Chinese, its original: the
/// half-translated text, still mostly Chinese, is no stand-in.
#[tokio::test(start_paused = true)]
async fn a_partly_translated_fill_keeps_the_original() {
    let mut h = harness(EngineSettings::default());
    let translator = Scripted::mapping(Duration::from_millis(700), partly);
    h.engine.set_translator(translator.clone());
    h.translator = translator;
    let paras = paragraphs(2);
    h.engine.load(join(&paras), Mode::Plain);
    settle(&h.engine).await;
    let swapped = h.engine.swap(join(&paras)).unwrap();
    h.engine.set_auto_translate(false);
    let zh = toy(&paras[1]);
    let zh_edited = format!("{zh}{}", toy(" and some more words"));
    let edited = swapped.text.replace(&zh, &zh_edited);
    h.engine.update(edited.clone());
    let back = h.engine.swap_keeping_untranslated(edited).unwrap();
    assert_eq!(h.engine.standins().len(), 1);
    h.engine.set_auto_translate(true);
    settle(&h.engine).await;
    let fills = fills(&h);
    assert_eq!(fills.len(), 1, "{fills:?}");
    h.engine.update(apply(&back.text, &fills));
    settle(&h.engine).await;
    assert!(h.engine.standins().is_empty());
    let chinese = h.engine.compose_target().unwrap();
    assert!(chinese.contains(&zh_edited), "{chinese}");
}

/// A stand-in edited and then undone (or typed back as it was) is still a
/// stand-in: swapped back, it is translated afresh.
#[tokio::test(start_paused = true)]
async fn a_stand_in_edited_and_undone_stays_one() {
    let (h, paras) = half_translated().await;
    h.engine.set_auto_translate(false);
    let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    let edited = swapped
        .text
        .replace("finding number 2", "finding number two");
    h.engine.update(edited);
    h.engine.update(swapped.text.clone());
    assert_eq!(h.engine.standins().len(), 2);
    h.engine.set_auto_translate(true);
    h.engine.swap(swapped.text).unwrap();
    settle(&h.engine).await;
    let chinese = join(&paras.iter().map(|p| toy(p)).collect::<Vec<_>>());
    assert_eq!(h.engine.compose_target().unwrap(), chinese);
}

/// The stand-in is deleted before its fill lands, and the fill's text is
/// the paragraph after it: that paragraph keeps its own original.
#[tokio::test(start_paused = true)]
async fn a_deleted_stand_in_leaves_its_twin_alone() {
    let mut h = harness(EngineSettings::default());
    // "The abc def." and "abc def." both come out as the Chinese of "abc def.".
    let translator = Scripted::mapping(Duration::from_millis(700), |s: &str| {
        toy(&s.replace("The ", ""))
    });
    h.engine.set_translator(translator.clone());
    h.translator = translator;
    let english = "abc def.\n\nxyz.".to_owned();
    h.engine.load(english.clone(), Mode::Plain);
    settle(&h.engine).await;
    let swapped = h.engine.swap(english.clone()).unwrap();
    let twin = toy("abc def.");
    let typed = swapped
        .text
        .replacen(&twin, &format!("The abc def.\n\n{twin}"), 1);
    h.engine.update(typed.clone());
    settle(&h.engine).await;
    assert_eq!(h.engine.continue_translation(true), 1);
    settle(&h.engine).await;
    let fills = fills(&h);
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].new.trim(), twin, "the fill is its twin's text");
    h.engine.update(typed.replacen("The abc def.\n\n", "", 1));
    settle(&h.engine).await;
    assert_eq!(h.engine.compose_target().unwrap(), english);
}

/// Stand-ins the offline translator could not fill are filled once a real
/// translator is set (a key was added), and so are those whose fill
/// failed once keys are tried again.
#[tokio::test(start_paused = true)]
async fn stand_ins_are_filled_once_a_real_translator_can() {
    let (h, paras) = half_translated().await;
    h.translator.set_stand_in(true);
    h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    settle(&h.engine).await;
    assert!(fills(&h).is_empty());
    h.translator.set_stand_in(false);
    h.engine.set_translator(h.translator.clone());
    settle(&h.engine).await;
    assert_eq!(fills(&h).len(), 2);

    let (h, paras) = half_translated().await;
    let rejected = || TranslateError::Rejected {
        status: 401,
        message: "no key".into(),
    };
    h.translator.fail_next([rejected(), rejected()]);
    h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    settle(&h.engine).await;
    assert!(fills(&h).is_empty());
    h.engine.retry_failed();
    settle(&h.engine).await;
    assert_eq!(fills(&h).len(), 2);
}

/// A paragraph typed in the other language whose fill fails has no
/// translation at all: it is translated the usual way round.
#[tokio::test(start_paused = true)]
async fn a_failed_fill_of_a_typed_paragraph_is_translated_the_usual_way() {
    let h = toy_harness(EngineSettings::default());
    let english = "abc def.".to_owned();
    h.engine.load(english.clone(), Mode::Plain);
    settle(&h.engine).await;
    let swapped = h.engine.swap(english).unwrap();
    let rejected = || TranslateError::Rejected {
        status: 400,
        message: "bad request".into(),
    };
    // Its translation fails, then its fill.
    h.translator.fail_next([rejected(), rejected()]);
    h.engine.update(format!("{}\n\nxyz uvw.", swapped.text));
    settle(&h.engine).await;
    assert_eq!(h.engine.continue_translation(true), 1);
    settle(&h.engine).await;
    assert!(fills(&h).is_empty());
    assert!(h.engine.compose_target().is_ok(), "translated after all");
}

/// A stand-in put into Chinese by hand and then undone, or deleted and
/// put back (a new ID), is a stand-in again: swapped back, it is
/// translated afresh.
#[tokio::test(start_paused = true)]
async fn a_stand_in_back_after_an_undo_is_one_again() {
    for undo in ["hand-translated", "deleted"] {
        let (h, paras) = half_translated().await;
        h.engine.set_auto_translate(false);
        let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
        let meanwhile = match undo {
            "hand-translated" => swapped.text.replace(&paras[2], &toy(&paras[2])),
            _ => swapped.text.replace(&format!("{}\n\n", paras[2]), ""),
        };
        h.engine.update(meanwhile);
        assert_eq!(h.engine.standins().len(), 1, "{undo}");
        h.engine.update(swapped.text.clone());
        assert_eq!(h.engine.standins().len(), 2, "{undo}");
        h.engine.set_auto_translate(true);
        h.engine.swap(swapped.text).unwrap();
        settle(&h.engine).await;
        let chinese = join(&paras.iter().map(|p| toy(p)).collect::<Vec<_>>());
        assert_eq!(h.engine.compose_target().unwrap(), chinese, "{undo}");
    }
}

/// Paused, a paragraph typed in the other language whose fill fails on
/// Continue is translated the usual way round at once: it was asked for.
#[tokio::test(start_paused = true)]
async fn a_failed_fill_asked_for_while_paused_is_translated_the_usual_way() {
    let h = toy_harness(EngineSettings::default());
    let english = "abc def.".to_owned();
    h.engine.load(english.clone(), Mode::Plain);
    settle(&h.engine).await;
    let swapped = h.engine.swap(english).unwrap();
    h.engine.set_auto_translate(false);
    h.engine.update(format!("{}\n\nxyz uvw.", swapped.text));
    h.translator.fail_next([TranslateError::Rejected {
        status: 400,
        message: "bad request".into(),
    }]);
    assert_eq!(h.engine.continue_translation(true), 1);
    settle(&h.engine).await;
    assert!(fills(&h).is_empty());
    assert!(h.engine.compose_target().is_ok(), "translated after all");
}

/// Back home, a Chinese stand-in the author starts putting into English by
/// hand is no stand-in any more, though still more Chinese than English: it
/// is translated the usual way, not taken as the exact Chinese. A small
/// edit keeps it one.
#[tokio::test(start_paused = true)]
async fn a_stand_in_being_translated_by_hand_is_none() {
    let h = toy_harness(EngineSettings::default());
    let english = "abc def.".to_owned();
    h.engine.load(english.clone(), Mode::Plain);
    settle(&h.engine).await;
    let swapped = h.engine.swap(english).unwrap();
    h.engine.set_auto_translate(false);
    let zh = toy("a new paragraph about the results of the study here");
    let added = format!("{}\n\n{zh}", swapped.text);
    h.engine.update(added.clone());
    let back = h.engine.swap_keeping_untranslated(added).unwrap();
    assert_eq!(h.engine.standins().len(), 1);
    let typo = back.text.replace(&zh, &format!("{zh}{}", toy("s")));
    h.engine.update(typo);
    assert_eq!(h.engine.standins().len(), 1, "a small edit keeps it one");
    let half = back.text.replace(&zh, &partly(&zh));
    h.engine.update(half);
    assert!(h.engine.standins().is_empty());
}

/// A paragraph put in Chinese by its fill keeps its ID, so what is tied to
/// it (a paired file's paragraph) stays tied, and it is a stand-in no more.
#[tokio::test(start_paused = true)]
async fn a_filled_paragraph_keeps_its_id() {
    let (h, paras) = half_translated().await;
    let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    settle(&h.engine).await;
    let fills = fills(&h);
    assert_eq!(fills.len(), 2);
    let ids = |h: &Harness| -> Vec<SegmentId> {
        h.engine.snapshot().layout.iter().map(|s| s.id).collect()
    };
    let before = ids(&h);
    h.engine.update(apply(&swapped.text, &fills));
    assert_eq!(ids(&h), before);
    assert!(fills.iter().all(|f| before.contains(&f.id)));
    assert!(h.engine.standins().is_empty());
}

/// A kept paragraph that reads the same in both languages (its fill is
/// refused) is its own translation rightly, no stand-in.
#[tokio::test(start_paused = true)]
async fn a_kept_paragraph_the_same_in_both_languages_is_no_stand_in() {
    let h = toy_harness(EngineSettings::default());
    h.engine.set_auto_translate(false);
    let english = "Paragraph one.\n\nICLR 2026.".to_owned();
    h.engine.load(english.clone(), Mode::Plain);
    h.engine.swap_keeping_untranslated(english).unwrap();
    assert_eq!(h.engine.standins().len(), 2);
    h.engine.set_auto_translate(true);
    settle(&h.engine).await;
    let fills = fills(&h);
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(
        h.engine.standins().into_iter().collect::<Vec<_>>(),
        vec![fills[0].id],
        "the other one until the editor puts its fill in"
    );
}

#[tokio::test(start_paused = true)]
async fn continue_takes_up_failed_and_paused_paragraphs_only() {
    let h = toy_harness(EngineSettings::default());
    h.translator.fail_next([TranslateError::Rejected {
        status: 400,
        message: "bad request".into(),
    }]);
    let paras = paragraphs(3);
    h.engine.load(join(&paras), Mode::Plain);
    settle(&h.engine).await;
    let failed = h
        .engine
        .snapshot()
        .states
        .iter()
        .filter(|s| s.status == SegmentStatus::Error)
        .count();
    assert_eq!(failed, 1);
    // Paused, an edit waits.
    h.engine.set_auto_translate(false);
    let edited = join(&paras).replace("number 2", "number two");
    h.engine.update(edited);
    settle(&h.engine).await;
    let calls = h.translator.calls();
    assert_eq!(h.engine.continue_translation(false), 2);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), calls + 2);
    assert!(all_translated(&h.engine));
    assert_eq!(h.engine.continue_translation(false), 0);
}

#[tokio::test(start_paused = true)]
async fn continue_while_swapped_fills_paragraphs_still_in_english_from_the_cache() {
    let h = toy_harness(EngineSettings::default());
    let paras = paragraphs(3);
    let english = join(&paras);
    h.engine.load(english.clone(), Mode::Plain);
    settle(&h.engine).await;
    let swapped = h.engine.swap(english).unwrap();
    // The author pastes the English of the second paragraph over its Chinese.
    let pasted = swapped.text.replace(&toy(&paras[1]), &paras[1]);
    h.engine.update(pasted.clone());
    settle(&h.engine).await;
    let calls = h.translator.calls();
    assert_eq!(h.engine.continue_translation(true), 1);
    settle(&h.engine).await;
    // Its Chinese was known from the first translation: no request.
    assert_eq!(h.translator.calls(), calls);
    let fills = fills(&h);
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].old, paras[1]);
    assert_eq!(fills[0].new, toy(&paras[1]));
    let filled = apply(&pasted, &fills);
    h.engine.update(filled);
    settle(&h.engine).await;
    // The English file still reads as it was.
    assert_eq!(h.engine.compose_target().unwrap(), join(&paras));
}

/// Translates a batch in one call, as a provider does, and records the
/// direction of every request of each call.
struct ToyBatcher {
    calls: Mutex<Vec<Vec<Direction>>>,
}

impl Translator for ToyBatcher {
    fn provider(&self) -> &str {
        "toy-batcher"
    }

    fn model(&self) -> &str {
        "toy"
    }

    fn translate<'a>(
        &'a self,
        request: &'a TranslationRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<TranslationOutput, TranslateError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(vec![request.direction]);
            tokio::time::sleep(Duration::from_millis(700)).await;
            let text = toy(&request.source);
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
        _on_partial: BatchPartialFn<'a>,
    ) -> BoxFuture<'a, Result<BatchOutput, TranslateError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(requests.iter().map(|r| r.direction).collect());
            tokio::time::sleep(Duration::from_millis(700)).await;
            Ok(BatchOutput {
                texts: requests.iter().map(|r| Some(toy(&r.source))).collect(),
                usage: TokenUsage::default(),
            })
        })
    }
}

#[tokio::test(start_paused = true)]
async fn fills_go_out_in_batches_of_their_own() {
    let h = harness(EngineSettings {
        concurrency: 1,
        batch_size: 4,
        ..EngineSettings::default()
    });
    let batcher = Arc::new(ToyBatcher {
        calls: Mutex::new(Vec::new()),
    });
    h.engine.set_translator(batcher.clone());
    let paras = paragraphs(7);
    h.engine.load(join(&paras), Mode::Plain);
    // The first four are back, the other three on their way.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let swapped = h.engine.swap_keeping_untranslated(join(&paras)).unwrap();
    // A new Chinese paragraph meanwhile: a fresh translation the usual way
    // round, which must not share a request with the fills.
    let added = format!("{}\n\n{}", swapped.text, toy("a new paragraph of my own"));
    h.engine.update(added.clone());
    settle(&h.engine).await;

    let fills = h.sink.fills.lock().unwrap().clone();
    assert_eq!(fills.len(), 3);
    let calls = batcher.calls.lock().unwrap().clone();
    for call in &calls {
        assert!(
            call.iter().all(|d| *d == call[0]),
            "a request mixes directions: {calls:?}"
        );
    }
    assert!(
        calls
            .iter()
            .any(|c| c.len() == 3 && c[0] == Direction::EnZh),
        "the three fills share one request: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c == &vec![Direction::ZhEn]),
        "{calls:?}"
    );
    let filled = apply(&added, &fills);
    h.engine.update(filled);
    settle(&h.engine).await;
    assert!(all_translated(&h.engine));
    assert_eq!(
        h.engine.compose_target().unwrap(),
        format!("{}\n\na new paragraph of my own", join(&paras))
    );
}

// ── Chinese that reads as English (review fix) ─────────────────────────

const EN_TOOLS: &str = "Implemented with PyTorch Lightning and HuggingFace Transformers.";
/// Chinese dense with English names: under half Chinese by share.
const ZH_TOOLS: &str = "使用 PyTorch Lightning 和 HuggingFace Transformers 实现。";
const EN_STUDY: &str = "We study translation quality in detail.";
const ZH_STUDY: &str = "我们详细研究翻译质量。";
const EN_NEW: &str = "Trained with DeepSpeed ZeRO Offload on NVIDIA A100 Tensor Core GPUs.";
/// Reads as English too.
const ZH_NEW: &str = "基于 DeepSpeed ZeRO Offload 和 NVIDIA A100 Tensor Core GPUs 训练。";

/// Edited, still dense with names (reads as English on its own).
const ZH_TOOLS_EDITED: &str = "使用 PyTorch Lightning 和 HuggingFace Transformers 完成实现。";
const EN_TOOLS_EDITED: &str =
    "Implemented in full with PyTorch Lightning and HuggingFace Transformers.";

/// Knows a few sentences each way; asked to put Chinese "into Chinese" it
/// only tidies the spacing, as a model does (or, for the edited sentence,
/// puts its English names into Chinese too).
struct Phrasebook {
    calls: Mutex<Vec<(Direction, String)>>,
}

fn phrase(direction: Direction, source: &str) -> String {
    let s = source.trim();
    let known = match (direction, s) {
        (Direction::EnZh, EN_TOOLS) => ZH_TOOLS,
        (Direction::EnZh, EN_STUDY) => ZH_STUDY,
        (Direction::EnZh, EN_NEW) => ZH_NEW,
        (Direction::ZhEn, ZH_TOOLS) => EN_TOOLS,
        (Direction::ZhEn, ZH_STUDY) => EN_STUDY,
        (Direction::ZhEn, ZH_NEW) => EN_NEW,
        (Direction::ZhEn, ZH_TOOLS_EDITED) => EN_TOOLS_EDITED,
        (Direction::EnZh, ZH_TOOLS_EDITED) => "使用 PyTorch 闪电框架和抱脸变换器库完成实现。",
        (Direction::EnZh, other) => return other.replace(' ', ""),
        (Direction::ZhEn, other) => other,
    };
    known.to_owned()
}

impl Translator for Phrasebook {
    fn provider(&self) -> &str {
        "phrasebook"
    }

    fn model(&self) -> &str {
        "fixed"
    }

    fn translate<'a>(
        &'a self,
        request: &'a TranslationRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<TranslationOutput, TranslateError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((request.direction, request.source.clone()));
            tokio::time::sleep(Duration::from_millis(500)).await;
            let text = phrase(request.direction, &request.source);
            on_partial(&text);
            Ok(TranslationOutput {
                text,
                usage: TokenUsage::default(),
            })
        })
    }
}

fn phrasebook_harness() -> (Harness, Arc<Phrasebook>) {
    let h = harness(EngineSettings::default());
    let book = Arc::new(Phrasebook {
        calls: Mutex::new(Vec::new()),
    });
    h.engine.set_translator(book.clone());
    (h, book)
}

#[tokio::test(start_paused = true)]
async fn continue_never_puts_chinese_into_the_english_file() {
    let (h, _) = phrasebook_harness();
    let english = format!("{EN_TOOLS}\n\n{EN_STUDY}");
    h.engine.load(english.clone(), Mode::Plain);
    settle(&h.engine).await;
    let swapped = h.engine.swap(english.clone()).unwrap();
    assert_eq!(swapped.text, format!("{ZH_TOOLS}\n\n{ZH_STUDY}"));

    // Continue while editing Chinese: the first paragraph reads as English
    // on its own, but it has its English original.
    assert_eq!(h.engine.continue_translation(true), 0);
    settle(&h.engine).await;
    assert!(fills(&h).is_empty(), "{:?}", fills(&h));
    assert_eq!(h.engine.compose_target().unwrap(), english);
    // An edit and its undo bring the exact original back too.
    h.engine.update(swapped.text.replace("实现", "完成"));
    settle(&h.engine).await;
    h.engine.update(swapped.text.clone());
    settle(&h.engine).await;
    assert_eq!(h.engine.compose_target().unwrap(), english);
}

#[tokio::test(start_paused = true)]
async fn a_fill_that_keeps_the_language_is_refused() {
    let (h, book) = phrasebook_harness();
    let english = EN_STUDY.to_owned();
    h.engine.load(english.clone(), Mode::Plain);
    settle(&h.engine).await;
    let swapped = h.engine.swap(english).unwrap();
    // Paused, the author writes a new paragraph in Chinese that reads as
    // English, then asks to continue.
    h.engine.set_auto_translate(false);
    let added = format!("{}\n\n{ZH_NEW}", swapped.text);
    h.engine.update(added.clone());
    settle(&h.engine).await;
    assert_eq!(h.engine.continue_translation(true), 1);
    settle(&h.engine).await;

    // Taken for English, it was sent to be put into Chinese, came back
    // Chinese, so it is not replaced in the editor...
    assert!(fills(&h).is_empty(), "{:?}", fills(&h));
    // ...and is translated into English like any Chinese paragraph.
    let calls = book.calls.lock().unwrap().clone();
    assert!(
        calls.contains(&(Direction::ZhEn, ZH_NEW.to_owned())),
        "{calls:?}"
    );
    assert_eq!(
        h.engine.compose_target().unwrap(),
        format!("{EN_STUDY}\n\n{EN_NEW}")
    );
}

#[tokio::test(start_paused = true)]
async fn english_pasted_into_the_chinese_is_still_filled() {
    let (h, _) = phrasebook_harness();
    let english = EN_STUDY.to_owned();
    h.engine.load(english.clone(), Mode::Plain);
    settle(&h.engine).await;
    let swapped = h.engine.swap(english).unwrap();
    let pasted = format!("{}\n\n{EN_TOOLS}", swapped.text);
    h.engine.update(pasted.clone());
    settle(&h.engine).await;
    assert_eq!(h.engine.continue_translation(true), 1);
    settle(&h.engine).await;
    let fills = fills(&h);
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].new, ZH_TOOLS);
    h.engine.update(apply(&pasted, &fills));
    settle(&h.engine).await;
    // The pasted English is the English file's text, exactly.
    assert_eq!(
        h.engine.compose_target().unwrap(),
        format!("{EN_STUDY}\n\n{EN_TOOLS}")
    );
}

#[tokio::test(start_paused = true)]
async fn an_edited_paragraph_dense_with_names_is_not_taken_for_english() {
    let (h, _) = phrasebook_harness();
    let english = format!("{EN_TOOLS}\n\n{EN_STUDY}");
    h.engine.load(english.clone(), Mode::Plain);
    settle(&h.engine).await;
    let swapped = h.engine.swap(english).unwrap();
    // Paused, the author edits the Chinese; it has no translation now.
    h.engine.set_auto_translate(false);
    let edited = swapped.text.replace(ZH_TOOLS, ZH_TOOLS_EDITED);
    h.engine.update(edited.clone());
    settle(&h.engine).await;
    assert_eq!(h.engine.continue_translation(true), 1);
    settle(&h.engine).await;
    // Not "filled" into more Chinese (which would make the Chinese its
    // exact English), but translated into English as an edit.
    assert!(fills(&h).is_empty(), "{:?}", fills(&h));
    assert_eq!(
        h.engine.compose_target().unwrap(),
        format!("{EN_TOOLS_EDITED}\n\n{EN_STUDY}")
    );
}
