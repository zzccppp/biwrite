//! Pairing end to end through the app state: open a document with its
//! mirror, edit a paragraph, save, swap, edit the other side, save again.
//! Each save may change only the paragraphs that were edited.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use biwrite_core::{Direction, TextFile};
use biwrite_engine::{Engine, EngineSettings, MemoryCache, MockTranslator, NullSink};

use crate::pairing;
use crate::request_log::{LogSettings, NoSink, RequestLog};
use crate::secrets::{MemoryStore, SecretStore};
use crate::settings::{AppSettings, Paths};
use crate::state::AppState;

pub(crate) fn app_state(dir: &Path) -> AppState {
    let engine = Engine::new(
        Arc::new(MockTranslator::with_delay(
            Duration::from_millis(1),
            Duration::from_millis(3),
        )),
        Arc::new(MemoryCache::default()),
        Arc::new(NullSink),
        EngineSettings::default(),
        tokio::runtime::Handle::current(),
    );
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    AppState::new(
        engine,
        Arc::new(MemoryCache::default()),
        AppSettings::default(),
        Paths::in_dir(dir),
        secrets,
        Arc::new(RequestLog::new(
            LogSettings::default(),
            None,
            Arc::new(NoSink),
        )),
        crate::skills::SkillStore::new(None, None),
        crate::latex_commands::LatexState::new(dir.join("t"), dir.join("u")),
    )
}

pub(crate) async fn settle(state: &AppState) {
    for _ in 0..2000 {
        if state.engine.pending() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("translations did not finish");
}

fn read(path: &Path) -> TextFile {
    TextFile::decode(std::fs::read(path).unwrap()).unwrap()
}

/// The paragraphs (units) of `a` and `b` that differ.
fn changed_units(a: &str, b: &str) -> Vec<usize> {
    let ua = biwrite_core::pair::units(a, biwrite_core::Mode::Latex);
    let ub = biwrite_core::pair::units(b, biwrite_core::Mode::Latex);
    assert_eq!(ua.len(), ub.len(), "the number of paragraphs changed");
    ua.iter()
        .zip(&ub)
        .enumerate()
        .filter(|(_, (x, y))| a[x.content.clone()] != b[y.content.clone()])
        .map(|(i, _)| i)
        .collect()
}

const EN: &str = "\\section{Method}\\label{sec:method}\n\
Our method prepares tables for TabPFN \\cite{hollmann2025} in three steps.\n\n\
First, it profiles each column and records $k=5$ statistics per value.\n\n\
\\begin{equation}\n  s(v) = \\sum_i w_i f_i(v)\n\\end{equation}\n\n\
Second, it decides one action per record (Table~\\ref{tab:actions}).\n";

const ZH: &str = "\\section{方法}\\label{sec:method}\n\
我们的方法分三步为 TabPFN \\cite{hollmann2025} 准备表格。\n\n\
第一步，逐列画像，并为每个取值记录 $k=5$ 个统计量。\n\n\
\\begin{equation}\n  s(v) = \\sum_i w_i f_i(v)\n\\end{equation}\n\n\
第二步，为每条记录决定一个动作（表~\\ref{tab:actions}）。\n";

fn project(name: &str, en: &str, zh: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("biwrite-pairs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sections_en")).unwrap();
    std::fs::create_dir_all(dir.join("sections_zh")).unwrap();
    let (a, b) = (
        dir.join("sections_en/method.tex"),
        dir.join("sections_zh/method.tex"),
    );
    std::fs::write(&a, en).unwrap();
    std::fs::write(&b, zh).unwrap();
    (dir, a, b)
}

async fn round_trip(dir: &Path, en_path: &Path, zh_path: &Path) {
    let state = app_state(dir);
    let en = std::fs::read_to_string(en_path).unwrap();
    let zh = std::fs::read_to_string(zh_path).unwrap();

    // Opening finds the mirror by its folder and takes its paragraphs.
    let mirror = pairing::counterpart(en_path).expect("a counterpart");
    assert_eq!(mirror, zh_path);
    pairing::open(
        &state,
        en_path.to_path_buf(),
        read(en_path),
        Some((mirror, read(zh_path))),
    );
    settle(&state).await;
    let units = state.engine.translations();
    let paired = state.file().pair.as_ref().unwrap().links.len();
    assert!(
        paired * 10 >= units.len() * 9,
        "{paired} of {}",
        units.len()
    );

    // Nothing edited: saving leaves the mirror byte for byte.
    let saved = pairing::save_mirror(&state).await.unwrap().unwrap();
    assert!(!saved.written, "an unchanged pair rewrote the mirror");
    assert_eq!(std::fs::read_to_string(zh_path).unwrap(), zh);

    // Edit the second English paragraph: only its Chinese changes.
    let ua = biwrite_core::pair::units(&en, biwrite_core::Mode::Latex);
    let second = &en[ua[2].content.clone()];
    let edited = en.replacen(second, &format!("{second} Edited."), 1);
    state.engine.update(edited.clone());
    settle(&state).await;
    // Saving the document writes the mirror after it.
    let saved =
        crate::commands::write_document(&state, en_path.to_path_buf(), edited.clone(), None)
            .await
            .unwrap()
            .mirror
            .unwrap();
    assert!(saved.written && saved.changed == 1, "{saved:?}");
    let zh_after = std::fs::read_to_string(zh_path).unwrap();
    let links = state.file().pair.as_ref().unwrap().links.clone();
    let id = state.engine.translations()[2].0;
    let unit = links[&id];
    assert_eq!(changed_units(&zh, &zh_after), vec![unit]);

    // Swap: edit the Chinese, and only the linked English paragraph changes.
    let view = pairing::swap(&state, edited.clone()).unwrap();
    assert_eq!(view.text, zh_after);
    assert_eq!(state.engine.direction(), Direction::ZhEn);
    settle(&state).await;
    let uz = biwrite_core::pair::units(&zh_after, biwrite_core::Mode::Latex);
    let first = &zh_after[uz[1].content.clone()];
    let zh_edited = zh_after.replacen(first, &format!("{first}（改）"), 1);
    state.engine.update(zh_edited.clone());
    settle(&state).await;
    let saved = crate::commands::write_document(&state, zh_path.to_path_buf(), zh_edited, None)
        .await
        .unwrap()
        .mirror
        .unwrap();
    assert!(saved.written && saved.changed == 1, "{saved:?}");
    let en_after = std::fs::read_to_string(en_path).unwrap();
    assert_eq!(changed_units(&edited, &en_after).len(), 1);
}

#[tokio::test]
async fn a_pair_saves_only_edited_paragraphs_both_ways() {
    let (dir, en, zh) = project("sample", EN, ZH);
    round_trip(&dir, &en, &zh).await;
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The same on a real paper: `BIWRITE_PAIR_PROJECT` names a folder with
/// `sections_en/` and `sections_zh/` (a copy: the test writes into it).
#[tokio::test]
#[ignore = "needs BIWRITE_PAIR_PROJECT"]
async fn a_real_paper_pair_saves_only_edited_paragraphs() {
    let root = PathBuf::from(std::env::var("BIWRITE_PAIR_PROJECT").unwrap());
    let mut sections: Vec<String> = std::fs::read_dir(root.join("sections_en"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tex") && root.join("sections_zh").join(n).is_file())
        .collect();
    sections.sort();
    for name in sections {
        let en = std::fs::read_to_string(root.join("sections_en").join(&name)).unwrap();
        let zh = std::fs::read_to_string(root.join("sections_zh").join(&name)).unwrap();
        if biwrite_core::pair::units(&en, biwrite_core::Mode::Latex).len() < 3 {
            continue;
        }
        eprintln!("{name}");
        let (dir, a, b) = project(&name.replace(".tex", ""), &en, &zh);
        round_trip(&dir, &a, &b).await;
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// A copy in the same language (a translation just started with
/// `cp paper.tex paper_zh.tex`) is not taken for the translation, so saving
/// never writes into the original.
#[tokio::test]
async fn a_copy_in_the_same_language_is_not_paired() {
    for (name, text) in [("same-en", EN), ("same-zh", ZH)] {
        let dir = std::env::temp_dir().join(format!("biwrite-pairs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (original, copy) = (dir.join("paper.tex"), dir.join("paper_zh.tex"));
        std::fs::write(&original, text).unwrap();
        std::fs::write(&copy, text).unwrap();
        let state = app_state(&dir);
        let mirror = pairing::counterpart(&copy).expect("found by name");
        assert_eq!(mirror, original);
        pairing::open(
            &state,
            copy.clone(),
            read(&copy),
            Some((mirror, read(&original))),
        );
        assert!(state.file().pair.is_none(), "{name}: paired with itself");
        settle(&state).await;
        assert!(pairing::save_mirror(&state).await.unwrap().is_none());
        assert_eq!(std::fs::read_to_string(&original).unwrap(), text);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// Swapping a pair with unsaved edits: they now live in the side swapped
/// away from, and stay unsaved (closing asks) until a save writes them.
#[tokio::test]
async fn swapping_a_pair_keeps_its_unsaved_edits_unsaved() {
    let (dir, en_path, zh_path) = project("unsaved", EN, ZH);
    let state = app_state(&dir);
    pairing::open(
        &state,
        en_path.clone(),
        read(&en_path),
        Some((zh_path.clone(), read(&zh_path))),
    );
    settle(&state).await;
    // Only the equation changes: no translation changes with it.
    let edited = EN.replace("w_i f_i(v)", "w_i g_i(v)");
    state.engine.update(edited.clone());
    state.file().dirty = true; // as the editor reports it
    settle(&state).await;
    let view = pairing::swap(&state, edited.clone()).unwrap();
    assert!(view.dirty, "the English edit is not on disk");
    // The editor now holds the untouched Chinese file and reports it clean.
    state.file().dirty = false;
    assert!(state.is_dirty() && state.needs_close_confirmation());
    assert!(state.file().window_title().starts_with('•'));

    // Saving writes both, and everything is clean.
    crate::commands::write_document(&state, zh_path.clone(), view.text.clone(), None)
        .await
        .unwrap();
    assert!(!state.is_dirty());
    assert_eq!(std::fs::read_to_string(&en_path).unwrap(), edited);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Chinese dense with English names (under half Chinese by share) still
/// pairs with its English, either file opened.
#[tokio::test]
async fn chinese_dense_with_english_names_still_pairs() {
    let en = "\\section{Setup}\n\
We train with AdamW, a learning rate of 1e-4 and a weight decay of 0.01.\n\n\
We build on PyTorch Lightning 2.4 and HuggingFace Transformers 4.46.\n";
    let zh = "\\section{Setup}\n\
使用 AdamW optimizer，learning rate 为 1e-4，weight decay 为 0.01。\n\n\
基于 PyTorch Lightning 2.4 和 HuggingFace Transformers 4.46 实现。\n";
    assert_ne!(
        biwrite_core::lang::written_in(zh, biwrite_core::Mode::Latex),
        Some(Direction::ZhEn),
        "reads as English on its own"
    );
    let (dir, en_path, zh_path) = project("dense", en, zh);
    for (open, mirror, direction) in [
        (&en_path, &zh_path, Direction::EnZh),
        (&zh_path, &en_path, Direction::ZhEn),
    ] {
        let state = app_state(&dir);
        pairing::open(
            &state,
            open.clone(),
            read(open),
            Some((mirror.clone(), read(mirror))),
        );
        assert!(
            state.file().pair.is_some(),
            "{} did not pair",
            open.display()
        );
        assert_eq!(state.engine.direction(), direction);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

// ── Phase 2 of the review fixes: saving a pair ──────────────────────────

use tauri::Manager;

use crate::commands::write_document;
use crate::pairing::MirrorProblem;

/// The app around [`app_state`], so commands can be called.
fn app(dir: &Path) -> tauri::App<tauri::test::MockRuntime> {
    let app = tauri::test::mock_app();
    app.manage(app_state(dir));
    app
}

fn open_pair(state: &AppState, en_path: &Path, zh_path: &Path) {
    pairing::open(
        state,
        en_path.to_path_buf(),
        read(en_path),
        Some((zh_path.to_path_buf(), read(zh_path))),
    );
    assert!(state.file().pair.is_some());
}

/// Changes made to the paired file elsewhere are never overwritten.
#[tokio::test]
async fn the_paired_file_is_not_overwritten_after_changes_on_disk() {
    let (dir, en_path, zh_path) = project("disk", EN, ZH);
    let state = app_state(&dir);
    open_pair(&state, &en_path, &zh_path);
    settle(&state).await;
    // A sentence of the Chinese is fixed in another editor.
    let fixed = ZH.replace("准备表格", "整理表格");
    std::fs::write(&zh_path, &fixed).unwrap();
    let edited = EN.replace("in three steps", "in three short steps");
    state.engine.update(edited.clone());
    settle(&state).await;
    let saved = write_document(&state, en_path.clone(), edited.clone(), None)
        .await
        .unwrap();
    let mirror = saved.mirror.unwrap();
    assert!(!mirror.written);
    assert_eq!(mirror.problem, Some(MirrorProblem::ChangedOnDisk));
    assert_eq!(std::fs::read_to_string(&zh_path).unwrap(), fixed);
    assert_eq!(std::fs::read_to_string(&en_path).unwrap(), edited);
    assert!(state.is_dirty(), "the paired file is behind");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A paired file waiting for translations follows the saved document, not
/// edits made after the save.
#[tokio::test]
async fn a_waiting_paired_file_follows_the_saved_text() {
    let (dir, en_path, zh_path) = project("waiting", EN, ZH);
    let app = app(&dir);
    let state = app.state::<AppState>();
    open_pair(&state, &en_path, &zh_path);
    settle(&state).await;
    // Save while the edited paragraph's translation is still to come.
    state.engine.set_auto_translate(false);
    let edited = EN.replace("in three steps", "in three short steps");
    state.engine.update(edited.clone());
    let saved = write_document(&state, en_path.clone(), edited.clone(), None)
        .await
        .unwrap();
    assert_eq!(saved.mirror.unwrap().pending, 1);
    assert!(state.is_dirty());
    // Then, unsaved, the last paragraph is deleted, and the translation arrives.
    let last = "Second, it decides one action per record (Table~\\ref{tab:actions}).\n";
    let later = edited.replace(last, "");
    state.engine.update(later.clone());
    state.engine.set_auto_translate(true);
    settle(&state).await;
    let done = pairing::write_mirror(state.clone()).await.unwrap().unwrap();
    assert!(!done.written, "not with edits that aren't saved");
    assert_eq!(std::fs::read_to_string(&zh_path).unwrap(), ZH);
    assert!(state.is_dirty());
    // The next save writes both.
    let saved = write_document(&state, en_path.clone(), later.clone(), None)
        .await
        .unwrap();
    assert!(saved.mirror.unwrap().written);
    let zh_now = std::fs::read_to_string(&zh_path).unwrap();
    assert!(!zh_now.contains("第二步"), "{zh_now}");
    assert!(
        !zh_now.contains("准备表格"),
        "the edited paragraph changed: {zh_now}"
    );
    assert!(!state.is_dirty());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A new paragraph right under a heading becomes a paragraph of its own in
/// the paired file, and later saves still change the right paragraphs.
#[tokio::test]
async fn a_new_paragraph_under_a_heading_stays_its_own() {
    let (dir, en_path, zh_path) = project("insert", EN, ZH);
    let state = app_state(&dir);
    open_pair(&state, &en_path, &zh_path);
    settle(&state).await;
    let heading = "\\section{Method}\\label{sec:method}\n";
    let added = EN.replace(heading, &format!("{heading}A new opening paragraph.\n\n"));
    state.engine.update(added.clone());
    settle(&state).await;
    let saved = write_document(&state, en_path.clone(), added.clone(), None)
        .await
        .unwrap();
    assert!(saved.mirror.unwrap().written);
    let zh_after = std::fs::read_to_string(&zh_path).unwrap();
    let mode = biwrite_core::Mode::Latex;
    let (before, after) = (
        biwrite_core::pair::units(ZH, mode),
        biwrite_core::pair::units(&zh_after, mode),
    );
    assert_eq!(after.len(), before.len() + 1, "{zh_after}");
    assert_eq!(
        &zh_after[after[2].content.clone()],
        &ZH[before[1].content.clone()]
    );

    // Editing the paragraph after it changes that one, not a neighbour.
    let edited = added.replace("in three steps", "in three short steps");
    state.engine.update(edited.clone());
    settle(&state).await;
    write_document(&state, en_path.clone(), edited, None)
        .await
        .unwrap();
    let zh_again = std::fs::read_to_string(&zh_path).unwrap();
    assert_eq!(changed_units(&zh_after, &zh_again), vec![2]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A new heading has no place of its own in the paired file: it is left
/// out and reported, not written as a paragraph.
#[tokio::test]
async fn new_headings_are_left_out_of_the_paired_file() {
    let (dir, en_path, zh_path) = project("heading", EN, ZH);
    let state = app_state(&dir);
    open_pair(&state, &en_path, &zh_path);
    settle(&state).await;
    let last = "Second, it decides one action per record (Table~\\ref{tab:actions}).\n";
    let added = EN.replace(last, &format!("{last}\n\\subsection{{Details}}\n"));
    state.engine.update(added.clone());
    settle(&state).await;
    let saved = write_document(&state, en_path.clone(), added, None)
        .await
        .unwrap();
    let mirror = saved.mirror.unwrap();
    assert_eq!(mirror.left_out, 1);
    assert_eq!(std::fs::read_to_string(&zh_path).unwrap(), ZH);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A pair keeps its mode, and its own translations can't be retranslated.
#[tokio::test]
async fn a_pair_keeps_its_mode_and_its_own_translations() {
    let (dir, en_path, zh_path) = project("guards", EN, ZH);
    let app = app(&dir);
    let state = app.state::<AppState>();
    open_pair(&state, &en_path, &zh_path);
    settle(&state).await;
    let document = state.engine.document();
    let mode = biwrite_core::Mode::Markdown;
    assert!(
        crate::commands::set_mode(state.clone(), mode, EN.into(), document)
            .await
            .is_err()
    );
    assert_eq!(state.engine.mode(), biwrite_core::Mode::Latex);
    assert!(
        crate::commands::retranslate_all(state.clone())
            .await
            .is_err()
    );
    let id = state.engine.translations()[1].0;
    assert!(state.engine.has_exact_translation(id));
    assert!(
        crate::commands::retranslate_segment(state.clone(), id.0)
            .await
            .is_err()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A folder linked to its counterpart is the same file, not a pair.
#[cfg(unix)]
#[test]
fn a_file_is_never_its_own_counterpart() {
    let dir = std::env::temp_dir().join(format!("biwrite-pairs-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("en")).unwrap();
    std::os::unix::fs::symlink(dir.join("en"), dir.join("zh")).unwrap();
    std::fs::write(dir.join("en/x.tex"), EN).unwrap();
    assert_eq!(pairing::counterpart(&dir.join("en/x.tex")), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Swapping makes the paired file the edited one, saved as such: not over
/// changes made to it elsewhere.
#[tokio::test]
async fn swapping_refuses_a_paired_file_changed_on_disk() {
    let (dir, en_path, zh_path) = project("swapdisk", EN, ZH);
    let state = app_state(&dir);
    open_pair(&state, &en_path, &zh_path);
    settle(&state).await;
    std::fs::write(&zh_path, ZH.replace("准备表格", "整理表格")).unwrap();
    let Err(err) = pairing::swap(&state, EN.into()) else {
        panic!("swapped onto a file changed elsewhere");
    };
    assert!(err.to_string().contains("changed on disk"), "{err}");
    assert_eq!(state.engine.direction(), Direction::EnZh);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A new paragraph with no paragraph before it goes before the next one,
/// outside a list the file starts with, never into it.
#[tokio::test]
async fn a_new_first_paragraph_stays_out_of_a_leading_list() {
    let en = "\\begin{itemize}\n\\item One item about TabPFN.\n\\end{itemize}\n\n\
              First paragraph with $k=5$ values.\n\nSecond paragraph at 0.86.\n";
    let zh = "\\begin{itemize}\n\\item 关于 TabPFN 的一项。\n\\end{itemize}\n\n\
              第一段，取 $k=5$ 个值。\n\n第二段，0.86。\n";
    let (dir, en_path, zh_path) = project("leading", en, zh);
    let state = app_state(&dir);
    open_pair(&state, &en_path, &zh_path);
    settle(&state).await;
    let added = en.replace(
        "\\end{itemize}\n\n",
        "\\end{itemize}\n\nA new paragraph.\n\n",
    );
    state.engine.update(added.clone());
    settle(&state).await;
    let saved = write_document(&state, en_path.clone(), added, None)
        .await
        .unwrap()
        .mirror
        .unwrap();
    assert!(saved.written, "{saved:?}");
    let zh_after = std::fs::read_to_string(&zh_path).unwrap();
    let (list_end, first) = (
        zh_after.find("\\end{itemize}").unwrap(),
        zh_after.find("第一段").unwrap(),
    );
    let new = zh_after
        .find(".hpargarap wen A")
        .expect("the new paragraph (the mock reverses)");
    assert!(list_end < new && new < first, "{zh_after}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A body wrapped in an environment (`CJK*`, `multicols`) takes new
/// paragraphs inside it, next to their neighbour: never refused for good.
#[tokio::test]
async fn a_wrapped_body_still_takes_new_paragraphs() {
    let en = "\\section{Intro}\nFirst paragraph with $k=5$.\n\nSecond paragraph at 0.86.\n";
    let zh = "\\begin{CJK*}{UTF8}{gbsn}\n\\section{引言}\n第一段，$k=5$。\n\n第二段，0.86。\n\\end{CJK*}\n";
    let (dir, en_path, zh_path) = project("wrapped", en, zh);
    let state = app_state(&dir);
    open_pair(&state, &en_path, &zh_path);
    settle(&state).await;
    let added = en.replace("$k=5$.\n\n", "$k=5$.\n\nA new paragraph.\n\n");
    state.engine.update(added.clone());
    settle(&state).await;
    let mirror = write_document(&state, en_path.clone(), added, None)
        .await
        .unwrap()
        .mirror
        .unwrap();
    assert!(mirror.written && mirror.problem.is_none(), "{mirror:?}");
    let zh_after = std::fs::read_to_string(&zh_path).unwrap();
    let (first, new, second, end) = (
        zh_after.find("第一段").unwrap(),
        zh_after.find(".hpargarap wen A").unwrap(),
        zh_after.find("第二段").unwrap(),
        zh_after.find("\\end{CJK*}").unwrap(),
    );
    assert!(first < new && new < second && second < end, "{zh_after}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A new paragraph after a list, with no paragraph of its own next to it,
/// is left out: never put before the list or into it.
#[tokio::test]
async fn a_new_paragraph_after_a_list_is_left_out() {
    let en = "First paragraph about TabPFN.\n\n\\begin{itemize}\n\\item One item at 0.86.\n\\end{itemize}\n";
    let zh = "关于 TabPFN 的第一段。\n\n\\begin{itemize}\n\\item 一项，0.86。\n\\end{itemize}\n";
    let (dir, en_path, zh_path) = project("afterlist", en, zh);
    let state = app_state(&dir);
    open_pair(&state, &en_path, &zh_path);
    settle(&state).await;
    let added = format!("{en}\nA closing paragraph.\n");
    state.engine.update(added.clone());
    settle(&state).await;
    let mirror = write_document(&state, en_path.clone(), added, None)
        .await
        .unwrap()
        .mirror
        .unwrap();
    assert_eq!(mirror.left_out, 1, "{mirror:?}");
    assert_eq!(std::fs::read_to_string(&zh_path).unwrap(), zh);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// When Save As can't write the paired file, the next save creates it
/// under the new name; the old one stays untouched.
#[tokio::test]
async fn a_paired_file_that_failed_to_save_as_is_created_later() {
    let (dir, en_path, zh_path) = project("saveasfail", EN, ZH);
    let state = app_state(&dir);
    open_pair(&state, &en_path, &zh_path);
    settle(&state).await;
    let dest = dir.join("missing/method_zh.tex");
    assert!(pairing::save_mirror_as(&state, dest.clone()).await.is_err());
    assert!(
        state.is_dirty(),
        "the new paired file is still to be written"
    );
    std::fs::create_dir_all(dir.join("missing")).unwrap();
    let mirror = pairing::save_mirror(&state).await.unwrap().unwrap();
    assert!(mirror.written, "{mirror:?}");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), ZH);
    assert_eq!(std::fs::read_to_string(&zh_path).unwrap(), ZH);
    assert!(!state.is_dirty());
    std::fs::remove_dir_all(&dir).unwrap();
}
