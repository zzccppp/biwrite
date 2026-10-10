//! The open file's own language: a Chinese file is the Chinese side, saving
//! writes the file's language whichever side is edited, a file taken for
//! the wrong language is read again as the other one, and the PDF of the
//! file's language is built from disk, the other from the translation.

use std::path::{Path, PathBuf};

use biwrite_core::{Direction, TextFile};

use crate::commands::{home_text, retarget};
use crate::latex_commands::{Lang, latex_compile, latex_forward};
use crate::pair_tests::{app_state, settle};
use crate::pairing;
use crate::state::AppState;

const ZH_PAPER: &str = "\\documentclass{article}\n\
\\usepackage[UTF8]{ctex}\n\
\\begin{document}\n\
\\section{引言}\n\
表格基础模型在小样本任务上表现突出，但数据中的错误会降低其精度 \\cite{tabpfn}。\
我们研究如何在训练之前修复这些错误。\n\n\
本文提出一种逐行分配清洗动作的方法，并在十二个数据集上验证其效果。\n\
\\end{document}\n";

const EN_PAPER: &str = "\\documentclass{article}\n\
\\begin{document}\n\
\\section{Introduction}\n\
Tabular foundation models do well on small tasks, but errors in the data cost accuracy.\n\n\
We assign one cleaning action to each row and test it on twelve datasets.\n\
\\end{document}\n";

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-home-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(state: &AppState, path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    let file = TextFile::decode(std::fs::read(path).unwrap()).unwrap();
    let mirror = pairing::counterpart(path).map(|m| {
        (
            m.clone(),
            TextFile::decode(std::fs::read(m).unwrap()).unwrap(),
        )
    });
    pairing::open(state, path.to_owned(), file, mirror);
}

#[tokio::test]
async fn a_chinese_file_is_the_chinese_side() {
    let dir = temp("zh");
    let state = app_state(&dir);
    open(&state, &dir.join("paper.tex"), ZH_PAPER);
    assert_eq!(state.engine.direction(), Direction::ZhEn);
    assert_eq!(state.file().home, Direction::ZhEn);
    settle(&state).await;
    // Saving writes the editor's Chinese as it is.
    assert_eq!(home_text(&state, ZH_PAPER.into()).unwrap(), ZH_PAPER);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn an_english_file_is_the_english_side() {
    let dir = temp("en");
    let state = app_state(&dir);
    open(&state, &dir.join("paper.tex"), EN_PAPER);
    assert_eq!(state.engine.direction(), Direction::EnZh);
    assert_eq!(state.file().home, Direction::EnZh);
    settle(&state).await;
    assert_eq!(home_text(&state, EN_PAPER.into()).unwrap(), EN_PAPER);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn saving_while_swapped_writes_the_files_own_language() {
    for (name, paper) in [("swap-zh", ZH_PAPER), ("swap-en", EN_PAPER)] {
        let dir = temp(name);
        let state = app_state(&dir);
        open(&state, &dir.join("paper.tex"), paper);
        settle(&state).await;
        let home = state.file().home;
        let swapped = state.engine.swap(paper.to_owned()).unwrap();
        assert_ne!(swapped.text, paper);
        assert_eq!(state.engine.direction(), home.flipped());
        // The other language is edited; the file gets its own back, exactly.
        assert_eq!(home_text(&state, swapped.text.clone()).unwrap(), paper);
        settle(&state).await;
        let back = state.engine.swap(swapped.text).unwrap();
        assert_eq!(back.text, paper);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[tokio::test]
async fn a_chinese_file_taken_for_english_is_read_as_chinese() {
    let dir = temp("wrong");
    let state = app_state(&dir);
    open(&state, &dir.join("paper.tex"), ZH_PAPER);
    // As before this version: every file was taken for English.
    state.engine.load_known(
        ZH_PAPER.into(),
        biwrite_core::Mode::Latex,
        Direction::EnZh,
        Vec::new(),
    );
    state.file().home = Direction::EnZh;
    settle(&state).await;

    let snapshot = retarget(&state, ZH_PAPER.into(), true).unwrap().unwrap();
    assert_eq!(snapshot.direction, Direction::ZhEn);
    assert_eq!(state.file().home, Direction::ZhEn);
    assert_eq!(state.engine.text(), ZH_PAPER, "the text stays as it is");
    assert!(!state.file().dirty);
    // Right now: nothing more to do.
    assert!(retarget(&state, ZH_PAPER.into(), true).unwrap().is_none());
    settle(&state).await;
    assert_eq!(home_text(&state, ZH_PAPER.into()).unwrap(), ZH_PAPER);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn the_language_changes_only_when_plain_or_asked_for() {
    let dir = temp("asked");
    let state = app_state(&dir);
    open(&state, &dir.join("paper.tex"), EN_PAPER);
    settle(&state).await;
    // English read as English: nothing to do.
    assert!(retarget(&state, EN_PAPER.into(), true).unwrap().is_none());
    // Asked for, it goes either way, the text untouched.
    let s = retarget(&state, EN_PAPER.into(), false).unwrap().unwrap();
    assert_eq!(s.direction, Direction::ZhEn);
    assert_eq!(state.engine.text(), EN_PAPER);
    let s = retarget(&state, EN_PAPER.into(), false).unwrap().unwrap();
    assert_eq!(s.direction, Direction::EnZh);
    assert_eq!(state.file().home, Direction::EnZh);
    // A mixed document mostly in English stays English.
    let mixed = EN_PAPER.replace("twelve datasets.", "twelve datasets（十二个数据集）.");
    assert!(retarget(&state, mixed, true).unwrap().is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn reading_again_while_swapped_starts_from_the_files_text() {
    let dir = temp("swapped");
    let state = app_state(&dir);
    open(&state, &dir.join("paper.tex"), EN_PAPER);
    settle(&state).await;
    let swapped = state.engine.swap(EN_PAPER.to_owned()).unwrap();
    // Not while the other language is edited, unless asked.
    assert!(
        retarget(&state, swapped.text.clone(), true)
            .unwrap()
            .is_none()
    );
    let s = retarget(&state, swapped.text, false).unwrap().unwrap();
    assert_eq!(
        state.engine.text(),
        EN_PAPER,
        "the file's own text comes back first"
    );
    assert_eq!(s.direction, Direction::ZhEn);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_pair_keeps_its_languages() {
    let dir = temp("pair");
    let state = app_state(&dir);
    std::fs::write(dir.join("paper_zh.tex"), ZH_PAPER).unwrap();
    open(&state, &dir.join("paper.tex"), EN_PAPER);
    assert!(state.file().pair.is_some());
    assert_eq!(state.file().home, Direction::EnZh);
    assert!(retarget(&state, EN_PAPER.into(), true).unwrap().is_none());
    assert!(retarget(&state, EN_PAPER.into(), false).is_err());
    // Opened from the Chinese side, the Chinese file is the edited one.
    let other = app_state(&dir);
    open(&other, &dir.join("paper_zh.tex"), ZH_PAPER);
    assert!(other.file().pair.is_some());
    assert_eq!(other.file().home, Direction::ZhEn);
    assert_eq!(other.engine.direction(), Direction::ZhEn);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn an_untitled_draft_in_chinese_can_be_read_as_chinese() {
    let dir = temp("untitled");
    let state = app_state(&dir);
    let draft = "我们研究表格数据清洗。\n\n第二段继续说明方法和实验设置。";
    state.engine.update(draft.into());
    assert_eq!(state.file().home, Direction::EnZh);
    let s = retarget(&state, draft.into(), true).unwrap().unwrap();
    assert_eq!(s.direction, Direction::ZhEn);
    assert_eq!(state.engine.text(), draft);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Build both PDFs of `paper` (needs a TeX distribution).
async fn build_both(paper: &str, name: &str) {
    let dir = temp(name);
    let app = tauri::test::mock_app();
    {
        use tauri::Manager;
        app.manage(app_state(&dir));
    }
    let state = tauri::Manager::state::<AppState>(&app);
    let path = dir.join("main.tex");
    open(&state, &path, paper);
    settle(&state).await;
    let own = if state.file().home == Direction::ZhEn {
        Lang::Zh
    } else {
        Lang::En
    };
    let other = if own == Lang::Zh { Lang::En } else { Lang::Zh };

    let disk = latex_compile(
        state.clone(),
        own,
        paper.to_owned(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(disk.has_pdf, "{own:?} PDF from disk: {}", disk.output);
    assert!(dir.join("main.pdf").is_file());
    assert_eq!(disk.untranslated, 0);

    let translated = latex_compile(
        state.clone(),
        other,
        paper.to_owned(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(translated.has_pdf, "{other:?} PDF: {}", translated.output);
    let folder = if other == Lang::En {
        ".biwrite/en"
    } else {
        ".biwrite/zh"
    };
    assert!(dir.join(folder).join("main.pdf").is_file(), "{folder}");
    // The translated PDF reads like the translation, not the file.
    let built = std::fs::read_to_string(dir.join(folder).join("main.tex")).unwrap();
    assert!(
        !built.contains("We assign one") && !built.contains("本文提出"),
        "{built}"
    );

    // The cursor's paragraph is found in both.
    let at = paper
        .find(if own == Lang::Zh {
            "本文提出"
        } else {
            "We assign"
        })
        .unwrap();
    let offset = paper[..at].encode_utf16().count();
    for lang in [own, other] {
        let boxes = latex_forward(
            state.clone(),
            lang,
            offset,
            paper.to_owned(),
            state.engine.document(),
        )
        .await
        .unwrap();
        assert!(!boxes.is_empty(), "{lang:?}: no box for the cursor");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_chinese_file_builds_its_english_pdf_from_the_translation() {
    build_both(ZH_PAPER, "tex-zh").await;
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn an_english_file_builds_its_chinese_pdf_from_the_translation() {
    build_both(EN_PAPER, "tex-en").await;
}

/// Text the editor sent before a swap (a save pressed during it) is
/// refused instead of being applied to the swapped document.
#[tokio::test]
async fn text_sent_before_a_swap_is_refused() {
    let dir = temp("stale");
    let state = app_state(&dir);
    open(&state, &dir.join("paper.tex"), EN_PAPER);
    settle(&state).await;
    let before = state.engine.document();
    let view = crate::commands::swap(&state, EN_PAPER.into(), false).unwrap();
    assert_eq!(view.snapshot.document, state.engine.document());
    assert!(state.sync_text(before, EN_PAPER).is_err());
    assert_eq!(
        state.engine.text(),
        view.text,
        "the swapped document is untouched"
    );
    assert_eq!(state.engine.direction(), Direction::ZhEn);
    assert!(state.sync_text(view.snapshot.document, &view.text).is_ok());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Swapping back to the file's own language never keeps paragraphs in the
/// other language: the file would be saved with them.
#[tokio::test]
async fn swapping_back_home_waits_for_every_translation() {
    let dir = temp("back");
    let state = app_state(&dir);
    open(&state, &dir.join("paper.tex"), EN_PAPER);
    settle(&state).await;
    let swapped = crate::commands::swap(&state, EN_PAPER.into(), false).unwrap();
    // A paragraph of the other language edited; its translation is on its way.
    let edited = format!(
        "{}\n",
        swapped
            .text
            .trim_end()
            .replacen("\\end{document}", "Extra words.\n\\end{document}", 1)
    );
    state.engine.update(edited.clone());
    assert!(state.engine.pending() > 0);
    let Err(err) = crate::commands::swap(&state, edited.clone(), true) else {
        panic!("swapped back with a paragraph still in Chinese");
    };
    assert!(err.to_string().contains("not translated yet"), "{err}");
    assert_eq!(state.engine.direction(), Direction::ZhEn, "still swapped");
    // Once translated, it swaps back, and the file reads in its language.
    settle(&state).await;
    let back = crate::commands::swap(&state, edited, true).unwrap();
    assert_eq!(state.engine.direction(), Direction::EnZh);
    assert!(back.text.contains("\\section{Introduction}"));
    std::fs::remove_dir_all(&dir).unwrap();
}
