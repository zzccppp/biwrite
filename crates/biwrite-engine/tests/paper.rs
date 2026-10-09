//! Engine behaviour on the sample LaTeX paper: what triggers requests, the
//! persistent cache, and swapping languages.

mod common;

use std::sync::Arc;

use biwrite_core::{Direction, Mode};
use biwrite_engine::{
    Engine, EngineError, EngineSettings, MemoryCache, NullSink, SegmentStatus, SqliteCache,
};
use common::{
    Scripted, all_translated, engine_with, harness, masked, reverse, reversed_translation, settle,
    state_of,
};

const PAPER: &str = include_str!("../../../samples/paper.tex");
/// Translatable segments in the sample paper (see the core segmenter test).
const TRANSLATABLE: u64 = 22;

async fn paper_loaded() -> common::Harness {
    let h = harness(EngineSettings::default());
    h.engine.load(PAPER.to_owned(), Mode::Latex);
    settle(&h.engine).await;
    assert!(all_translated(&h.engine));
    assert_eq!(h.translator.calls(), TRANSLATABLE);
    h
}

async fn edit(h: &common::Harness, from: &str, to: &str) -> String {
    let current = h.engine.text();
    assert!(current.contains(from), "pattern {from:?} not found");
    let text = current.replacen(from, to, 1);
    h.engine.update(text.clone());
    settle(&h.engine).await;
    text
}

#[tokio::test(start_paused = true)]
async fn only_prose_edits_trigger_requests() {
    let h = paper_loaded().await;
    // Preamble, math, table body, comment, markup: no requests.
    edit(&h, "\\usepackage{hyperref}", "\\usepackage{hyperref,url}").await;
    edit(&h, "c_v)/\\tau)", "c_v)/\\tau')").await;
    edit(&h, "71.2 & 63.5", "71.3 & 63.5").await;
    edit(&h, "% TODO: add ablation", "% TODO: add an ablation").await;
    edit(&h, "\\bibliography{refs}", "\\bibliography{refs,more}").await;
    assert_eq!(h.translator.calls(), TRANSLATABLE);

    // A caption and a heading do.
    edit(&h, "the query node attends", "the query node then attends").await;
    edit(
        &h,
        "\\subsection{Training Objective}",
        "\\subsection{Pretraining Objective}",
    )
    .await;
    assert_eq!(h.translator.calls(), TRANSLATABLE + 2);
    let sources: Vec<String> = h
        .translator
        .requests()
        .into_iter()
        .map(|r| r.source)
        .collect();
    assert_eq!(
        sources.last().map(String::as_str),
        Some("Pretraining Objective")
    );
}

#[tokio::test(start_paused = true)]
async fn persistent_cache_survives_restart() {
    let dir = std::env::temp_dir().join(format!("biwrite-engine-cache-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cache.sqlite3");
    let session = |translator: Arc<Scripted>| {
        Engine::new(
            translator,
            Arc::new(SqliteCache::open(&path).unwrap()),
            Arc::new(NullSink),
            EngineSettings::default(),
            tokio::runtime::Handle::current(),
        )
    };

    let first = Scripted::new(std::time::Duration::from_millis(300));
    let engine = session(first.clone());
    engine.load(PAPER.to_owned(), Mode::Latex);
    settle(&engine).await;
    assert_eq!(first.calls(), TRANSLATABLE);
    drop(engine);

    // "Restart": a new engine and connection on the same file.
    let second = Scripted::new(std::time::Duration::from_millis(300));
    let engine = session(second.clone());
    engine.load(PAPER.to_owned(), Mode::Latex);
    settle(&engine).await;
    assert_eq!(second.calls(), 0);
    assert_eq!(engine.usage().cache_hits, TRANSLATABLE);
    assert!(all_translated(&engine));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(start_paused = true)]
async fn swap_round_trip_is_exact_and_free() {
    let h = paper_loaded().await;
    let swapped = h.engine.swap(PAPER.to_owned()).unwrap();
    assert_eq!(swapped.snapshot.direction, Direction::ZhEn);
    settle(&h.engine).await;
    // Every paragraph's English is known: no requests, all translated.
    assert_eq!(h.translator.calls(), TRANSLATABLE);
    assert!(all_translated(&h.engine));
    // The English file composed from the Chinese side is the original.
    assert_eq!(h.engine.compose_target().unwrap(), PAPER);

    let back = h.engine.swap(swapped.text).unwrap();
    assert_eq!(back.text, PAPER);
    assert_eq!(back.snapshot.direction, Direction::EnZh);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), TRANSLATABLE);
    assert!(all_translated(&h.engine));
}

#[tokio::test(start_paused = true)]
async fn editing_chinese_revises_only_that_paragraph() {
    let h = paper_loaded().await;
    let intro_en = "In contrast, large language models perform new tasks when shown a few input-output pairs in context \\cite{brown2020gpt3}. This raises a natural question: can a graph model be prompted in the same way?";
    let intro_zh = reversed_translation(intro_en, Mode::Latex);
    assert!(
        intro_zh.contains("\\cite{brown2020gpt3}"),
        "citation kept whole"
    );
    let swapped = h.engine.swap(PAPER.to_owned()).unwrap();
    settle(&h.engine).await;
    assert!(swapped.text.contains(&intro_zh));

    // Edit the Chinese of one paragraph.
    let edited_zh = format!("{intro_zh} 补充一句。");
    let zh_text = swapped.text.replacen(&intro_zh, &edited_zh, 1);
    h.engine.update(zh_text.clone());
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), TRANSLATABLE + 1);

    // Revise mode: old Chinese + the original English + new Chinese.
    let req = h.translator.requests().pop().unwrap();
    assert_eq!(req.direction, Direction::ZhEn);
    // The citation is the same placeholder in all three texts.
    assert_eq!(req.source, masked(&edited_zh, Mode::Latex));
    assert!(req.source.contains("⟦0⟧"));
    let rev = req.revision.expect("small edit uses revise mode");
    assert_eq!(rev.old_source, masked(&intro_zh, Mode::Latex));
    assert_eq!(rev.old_translation, masked(intro_en, Mode::Latex));

    // The English file changes in that paragraph only.
    let english = h.engine.compose_target().unwrap();
    assert_eq!(
        english,
        PAPER.replacen(intro_en, &reversed_translation(&edited_zh, Mode::Latex), 1)
    );

    // Swapping back shows the user's own Chinese for that paragraph, free.
    let back = h.engine.swap(zh_text).unwrap();
    assert_eq!(back.text, english);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), TRANSLATABLE + 1);
    assert!(all_translated(&h.engine));
    let texts: Vec<String> = h
        .engine
        .snapshot()
        .states
        .into_iter()
        .filter_map(|s| s.text)
        .collect();
    assert!(
        texts.contains(&edited_zh),
        "right pane shows the user's own Chinese"
    );
}

#[tokio::test(start_paused = true)]
async fn undoing_a_chinese_edit_restores_the_original_english() {
    let h = paper_loaded().await;
    let swapped = h.engine.swap(PAPER.to_owned()).unwrap();
    settle(&h.engine).await;
    let zh = swapped.text;
    let edited = zh.replacen("noitcudortnI", "引言", 1);
    h.engine.update(edited);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), TRANSLATABLE + 1);
    h.engine.update(zh);
    settle(&h.engine).await;
    assert_eq!(h.translator.calls(), TRANSLATABLE + 1);
    assert_eq!(h.engine.compose_target().unwrap(), PAPER);
}

#[tokio::test(start_paused = true)]
async fn swap_requires_every_paragraph_translated() {
    let h = harness(EngineSettings::default());
    h.engine.load(PAPER.to_owned(), Mode::Latex);
    let err = h.engine.swap(PAPER.to_owned()).unwrap_err();
    assert_eq!(
        err,
        EngineError::NotReady {
            missing: TRANSLATABLE as usize
        }
    );
    assert_eq!(h.engine.direction(), Direction::EnZh);
    settle(&h.engine).await;

    // Unsent edits count too: they make their paragraph pending.
    let edited = PAPER.replacen("Future work", "In future work", 1);
    assert!(matches!(
        h.engine.swap(edited),
        Err(EngineError::NotReady { missing: 1 })
    ));
    settle(&h.engine).await;
    assert!(h.engine.swap(h.engine.text()).is_ok());
}

#[tokio::test(start_paused = true)]
async fn empty_document_swaps_to_chinese_drafting() {
    let h = harness(EngineSettings::default());
    let swapped = h.engine.swap(String::new()).unwrap();
    assert_eq!(swapped.text, "");
    assert_eq!(h.engine.direction(), Direction::ZhEn);
    h.engine.update("我们研究图上的上下文学习。".into());
    settle(&h.engine).await;
    assert_eq!(
        h.engine.compose_target().unwrap(),
        reverse("我们研究图上的上下文学习。")
    );
    let id = h.engine.snapshot().layout[0].id;
    assert_eq!(state_of(&h.engine, id).status, SegmentStatus::Translated);
    // Opening a file returns to English editing.
    h.engine.load("Hello.".into(), Mode::Plain);
    assert_eq!(h.engine.direction(), Direction::EnZh);
}

#[tokio::test(start_paused = true)]
async fn cache_keys_include_direction() {
    let cache = Arc::new(MemoryCache::default());
    let translator = Scripted::new(std::time::Duration::from_millis(100));
    let engine = Engine::new(
        translator.clone(),
        cache.clone(),
        Arc::new(NullSink),
        EngineSettings::default(),
        tokio::runtime::Handle::current(),
    );
    engine.load("Same text.".into(), Mode::Plain);
    settle(&engine).await;
    engine.swap(engine.text()).unwrap();
    // Draft the same characters in the other direction: not a cache hit.
    engine.update(".txet emaS\n\nSame text.".into());
    settle(&engine).await;
    assert_eq!(translator.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn identical_translations_keep_distinct_originals() {
    // "Methods" and "Method" (and two wrappings of one sentence) translate
    // to the same Chinese; each must still come back as its own English.
    fn zh(s: &str) -> String {
        if s.starts_with("Method") {
            "方法".into()
        } else {
            format!("译：{}", s.split_whitespace().collect::<Vec<_>>().join(" "))
        }
    }
    let text =
        "\\section{Methods}\nSame sentence here.\n\n\\subsection{Method}\nSame sentence\nhere.\n";
    let translator = Scripted::mapping(std::time::Duration::from_millis(100), zh);
    let engine = engine_with(translator.clone());
    engine.load(text.to_owned(), Mode::Latex);
    settle(&engine).await;

    let swapped = engine.swap(text.to_owned()).unwrap();
    settle(&engine).await;
    assert_eq!(engine.compose_target().unwrap(), text);
    let back = engine.swap(swapped.text).unwrap();
    assert_eq!(back.text, text);
}

#[tokio::test(start_paused = true)]
async fn originals_are_restored_verbatim() {
    // Shapes that machine output would be normalized from.
    let text = "\\section{ Introduction }\nBody.\n\\begin{figure}\n  \\caption{\n    Accuracy on Cora.\n\n    Second line.\n  }\n\\end{figure}\n";
    let translator = Scripted::mapping(std::time::Duration::from_millis(100), |s| {
        format!("译{}", s.split_whitespace().collect::<Vec<_>>().join(" "))
    });
    let engine = engine_with(translator);
    engine.load(text.to_owned(), Mode::Latex);
    settle(&engine).await;
    let swapped = engine.swap(text.to_owned()).unwrap();
    settle(&engine).await;
    assert_eq!(engine.compose_target().unwrap(), text);
    assert_eq!(engine.swap(swapped.text).unwrap().text, text);
}

#[tokio::test(start_paused = true)]
async fn percent_from_the_model_cannot_break_the_english_file() {
    let text = "\\section{结果}\n准确率为95\\%。\n";
    let translator = Scripted::mapping(std::time::Duration::from_millis(100), |s| {
        if s == "结果" {
            "Results".into()
        } else {
            "Accuracy is 95%.".into()
        }
    });
    let engine = engine_with(translator);
    // Draft in Chinese from scratch, then save (compose) the English.
    engine.load(String::new(), Mode::Latex);
    engine.swap(String::new()).unwrap();
    engine.update(text.to_owned());
    settle(&engine).await;
    assert_eq!(
        engine.compose_target().unwrap(),
        "\\section{Results}\nAccuracy is 95\\%.\n"
    );
}

#[tokio::test(start_paused = true)]
async fn bilingual_export_is_the_same_from_either_side() {
    let h = harness(EngineSettings::default());
    h.engine.load(PAPER.to_owned(), Mode::Latex);
    let pending = h.engine.bilingual_markdown();
    assert_eq!(pending.missing, TRANSLATABLE as usize);
    assert_eq!(
        pending.text.matches("*（尚未翻译）*").count(),
        pending.missing
    );
    settle(&h.engine).await;

    let english_side = h.engine.bilingual_markdown().text;
    assert!(!english_side.contains("尚未翻译"));
    // Each paragraph is followed by its translation.
    let intro = PAPER
        .lines()
        .find(|l| l.starts_with("We answer this question"))
        .unwrap();
    let pair = format!(
        "{intro}\n\n{}\n\n",
        reversed_translation(intro, Mode::Latex)
    );
    assert!(english_side.contains(&pair));

    let swapped = h.engine.swap(PAPER.to_owned()).unwrap();
    settle(&h.engine).await;
    assert_eq!(h.engine.bilingual_markdown().text, english_side);
    assert!(!swapped.text.is_empty());
}
