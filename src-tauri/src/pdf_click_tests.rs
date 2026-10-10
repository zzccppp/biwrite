//! Clicks in both PDFs of a paper paired by hand with its Chinese version
//! (`en/paper.tex` and `zh/paper_zh.tex`, as in a real project) land on the
//! clicked paragraph, swapped or not. A PDF built before a pairing, an
//! unpairing or a swap without a pair maps nothing until it is built again.
//! Building and clicking never write the papers.

use std::path::{Path, PathBuf};

use biwrite_core::TextFile;
use biwrite_latex::PdfBox;
use tauri::Manager;

use crate::error::CommandError;
use crate::latex_commands::{Lang, SyncHit, latex_compile, latex_forward, latex_inverse};
use crate::pair_tests::{app_state, settle};
use crate::pairing;
use crate::state::AppState;

const EN: &str = "\\documentclass{article}\n\
\\begin{document}\n\
\\section{Introduction}\n\
Data cleaning repairs the errors of a table before a model reads it.\n\n\
We study when a repair helps the model and when it hurts the model.\n\n\
\\section{Method}\n\
Our method assigns one action to each row of the table.\n\n\
It checks every action on a held out set before keeping it.\n\
\\end{document}\n";

const ZH: &str = "\\documentclass{article}\n\
\\usepackage[UTF8]{ctex}\n\
\\begin{document}\n\
\\section{引言}\n\
数据清洗在模型读取表格之前修复表格中的错误。\n\n\
我们研究修复何时有助于模型，何时会损害模型。\n\n\
\\section{方法}\n\
我们的方法为表格的每一行分配一个动作。\n\n\
它在保留集上检查每个动作，确认有效后才保留。\n\
\\end{document}\n";

const EN_PARAGRAPH: &str = "Our method assigns one action to each row of the table.";
const ZH_PARAGRAPH: &str = "我们的方法为表格的每一行分配一个动作。";
const EN_SPAN: &str = "Our method assigns one action";
const ZH_SPAN: &str = "我们的方法为表格的每一行";

type AppStateRef<'a> = tauri::State<'a, AppState>;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-click-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("en")).unwrap();
    std::fs::create_dir_all(dir.join("zh")).unwrap();
    dir
}

/// The UTF-16 offset of `needle` in the editor's text.
fn offset_of(state: &AppState, needle: &str) -> usize {
    let text = state.engine.text();
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} in {text}"));
    text[..at].encode_utf16().count()
}

/// The first box of the editor's `offset` in the PDF of `lang`.
async fn box_at(
    state: &AppStateRef<'_>,
    lang: Lang,
    offset: usize,
) -> Result<PdfBox, CommandError> {
    let boxes = latex_forward(
        state.clone(),
        lang,
        offset,
        state.engine.text(),
        state.engine.document(),
    )
    .await?;
    Ok(boxes
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{lang:?}: no box at {offset}")))
}

/// Click the middle of `b` in the PDF of `lang`, `span` under the click.
async fn click_box(
    state: &AppStateRef<'_>,
    lang: Lang,
    b: &PdfBox,
    span: &str,
) -> Result<Option<SyncHit>, CommandError> {
    latex_inverse(
        state.clone(),
        lang,
        b.page,
        b.left + b.width / 2.0,
        b.top + b.height / 2.0,
        span.to_owned(),
        span.encode_utf16().count() / 2,
        state.engine.text(),
        state.engine.document(),
    )
    .await
}

/// The text a hit selects in the editor.
fn selected(state: &AppState, hit: Option<SyncHit>) -> String {
    let hit = hit.expect("a hit");
    assert!(hit.here, "{hit:?}");
    let r = hit.range.expect("a range");
    let units: Vec<u16> = state.engine.text().encode_utf16().collect();
    String::from_utf16_lossy(&units[r.from..r.to])
}

/// Click the paragraph `needle` of the editor in the PDF of `lang` and
/// return what the click selects.
async fn round_trip(state: &AppStateRef<'_>, lang: Lang, needle: &str, span: &str) -> String {
    let b = box_at(state, lang, offset_of(state, needle)).await.unwrap();
    selected(state, click_box(state, lang, &b, span).await.unwrap())
}

async fn build(state: &AppStateRef<'_>, lang: Lang) {
    let view = latex_compile(
        state.clone(),
        lang,
        state.engine.text(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(view.has_pdf, "{lang:?}: {}", view.output);
}

fn is_stale<T: std::fmt::Debug>(result: Result<T, CommandError>) -> bool {
    match result {
        Err(CommandError::StalePdf) => true,
        other => panic!("expected a stale PDF, got {other:?}"),
    }
}

/// Every `.tex` file under `dir`, hidden folders included.
fn tex_files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().is_some_and(|e| e == "tex") {
                out.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn clicks_in_both_pdfs_of_a_pair_land_on_the_clicked_paragraph() {
    let dir = temp("pair");
    let (en_path, zh_path) = (dir.join("en/paper.tex"), dir.join("zh/paper_zh.tex"));
    std::fs::write(&en_path, EN).unwrap();
    std::fs::write(&zh_path, ZH).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<AppState>();

    // The English paper alone: no counterpart by name. Its Chinese PDF is
    // built from the machine translation.
    let file = TextFile::decode(std::fs::read(&en_path).unwrap()).unwrap();
    pairing::open(&state, en_path.clone(), file, None);
    assert!(state.file().pair.is_none());
    settle(&state).await;
    build(&state, Lang::En).await;
    build(&state, Lang::Zh).await;
    assert_eq!(
        round_trip(&state, Lang::En, EN_PARAGRAPH, EN_SPAN).await,
        EN_PARAGRAPH
    );
    let translated = box_at(&state, Lang::Zh, offset_of(&state, EN_PARAGRAPH))
        .await
        .unwrap();
    assert_eq!(
        selected(
            &state,
            click_box(&state, Lang::Zh, &translated, "").await.unwrap()
        ),
        EN_PARAGRAPH
    );

    // Paired by hand with the Chinese paper. The Chinese PDF of the
    // translation maps nothing now: neither clicks nor the cursor.
    let view = pairing::pair_with(
        &state,
        state.engine.text(),
        state.engine.document(),
        en_path.clone(),
        zh_path.clone(),
    )
    .await
    .unwrap();
    let pair = view.pair.expect("paired");
    assert_eq!(pair.paired, pair.units, "every paragraph paired");
    settle(&state).await;
    assert!(is_stale(
        click_box(&state, Lang::Zh, &translated, ZH_SPAN).await
    ));
    assert!(is_stale(
        box_at(&state, Lang::Zh, offset_of(&state, EN_PARAGRAPH)).await
    ));
    // The English PDF is still the English file's.
    assert_eq!(
        round_trip(&state, Lang::En, EN_PARAGRAPH, EN_SPAN).await,
        EN_PARAGRAPH
    );

    // Built again: from the Chinese paper itself, in its folder.
    build(&state, Lang::Zh).await;
    assert!(dir.join("zh/paper_zh.pdf").is_file());
    assert_eq!(
        round_trip(&state, Lang::Zh, EN_PARAGRAPH, ZH_SPAN).await,
        EN_PARAGRAPH
    );

    // Swapped: the Chinese paper is edited. Both PDFs map into it.
    let swapped = crate::commands::swap(&state, state.engine.text(), false).unwrap();
    assert!(swapped.text.contains(ZH_PARAGRAPH));
    assert_eq!(state.file().path.as_deref(), Some(zh_path.as_path()));
    assert_eq!(
        round_trip(&state, Lang::Zh, ZH_PARAGRAPH, ZH_SPAN).await,
        ZH_PARAGRAPH
    );
    assert_eq!(
        round_trip(&state, Lang::En, ZH_PARAGRAPH, EN_SPAN).await,
        ZH_PARAGRAPH
    );

    // And back.
    crate::commands::swap(&state, state.engine.text(), false).unwrap();
    assert_eq!(state.file().path.as_deref(), Some(en_path.as_path()));
    assert_eq!(
        round_trip(&state, Lang::Zh, EN_PARAGRAPH, ZH_SPAN).await,
        EN_PARAGRAPH
    );
    assert_eq!(
        round_trip(&state, Lang::En, EN_PARAGRAPH, EN_SPAN).await,
        EN_PARAGRAPH
    );

    // Unpaired: the Chinese PDF of the paired file maps nothing, the
    // English one still does.
    let paired_box = box_at(&state, Lang::Zh, offset_of(&state, EN_PARAGRAPH))
        .await
        .unwrap();
    pairing::close_mirror(state.clone()).await.unwrap();
    assert!(is_stale(
        click_box(&state, Lang::Zh, &paired_box, ZH_SPAN).await
    ));
    assert_eq!(
        round_trip(&state, Lang::En, EN_PARAGRAPH, EN_SPAN).await,
        EN_PARAGRAPH
    );
    build(&state, Lang::Zh).await;
    assert_eq!(
        round_trip(&state, Lang::Zh, EN_PARAGRAPH, ZH_SPAN).await,
        EN_PARAGRAPH
    );

    // Swapped without a pair: the editor holds the translation, which both
    // PDFs built before map nothing into until they are built again.
    let en_box = box_at(&state, Lang::En, offset_of(&state, EN_PARAGRAPH))
        .await
        .unwrap();
    let zh_box = box_at(&state, Lang::Zh, offset_of(&state, EN_PARAGRAPH))
        .await
        .unwrap();
    crate::commands::swap(&state, state.engine.text(), false).unwrap();
    assert!(state.engine.text().contains(ZH_PARAGRAPH));
    assert!(is_stale(
        click_box(&state, Lang::En, &en_box, EN_SPAN).await
    ));
    assert!(is_stale(
        click_box(&state, Lang::Zh, &zh_box, ZH_SPAN).await
    ));
    build(&state, Lang::En).await;
    build(&state, Lang::Zh).await;
    assert_eq!(
        round_trip(&state, Lang::En, ZH_PARAGRAPH, EN_SPAN).await,
        ZH_PARAGRAPH
    );
    assert_eq!(
        round_trip(&state, Lang::Zh, ZH_PARAGRAPH, ZH_SPAN).await,
        ZH_PARAGRAPH
    );

    // Neither paper was written, and no other source file appeared next to
    // them (the translation's document lives in the mirror folder).
    assert_eq!(std::fs::read(&en_path).unwrap(), EN.as_bytes());
    assert_eq!(std::fs::read(&zh_path).unwrap(), ZH.as_bytes());
    assert_eq!(
        tex_files(&dir),
        [
            "en/.biwrite/zh/paper.tex",
            "en/paper.tex",
            "zh/paper_zh.tex"
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The Chinese paper as its author left it: built with their own latexmk,
/// without SyncTeX, so latexmk finds it up to date. The Chinese PDF BiWrite
/// builds after the pairing still carries SyncTeX, and its clicks land on
/// the English paragraph.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_chinese_paper_built_without_synctex_still_maps_clicks() {
    let dir = temp("built");
    let (en_path, zh_path) = (dir.join("en/paper.tex"), dir.join("zh/paper_zh.tex"));
    std::fs::write(&en_path, EN).unwrap();
    std::fs::write(&zh_path, ZH).unwrap();
    let tc = biwrite_latex::detect(biwrite_latex::find_bin(None).expect("TeX")).await;
    if !tc.latexmk {
        return;
    }
    let status = std::process::Command::new(tc.tool("latexmk"))
        .args(["-xelatex", "-interaction=nonstopmode", "paper_zh.tex"])
        .current_dir(dir.join("zh"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                tc.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(dir.join("zh/paper_zh.pdf").is_file());
    assert!(!dir.join("zh/paper_zh.synctex.gz").exists());

    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<AppState>();
    let file = TextFile::decode(std::fs::read(&en_path).unwrap()).unwrap();
    pairing::open(&state, en_path.clone(), file, None);
    settle(&state).await;
    build(&state, Lang::En).await;
    pairing::pair_with(
        &state,
        state.engine.text(),
        state.engine.document(),
        en_path.clone(),
        zh_path.clone(),
    )
    .await
    .unwrap();
    settle(&state).await;
    build(&state, Lang::Zh).await;
    assert!(dir.join("zh/paper_zh.synctex.gz").is_file());
    assert_eq!(
        round_trip(&state, Lang::Zh, EN_PARAGRAPH, ZH_SPAN).await,
        EN_PARAGRAPH
    );
    assert_eq!(
        round_trip(&state, Lang::En, EN_PARAGRAPH, EN_SPAN).await,
        EN_PARAGRAPH
    );
    assert_eq!(std::fs::read(&en_path).unwrap(), EN.as_bytes());
    assert_eq!(std::fs::read(&zh_path).unwrap(), ZH.as_bytes());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The boxes of the editor's `offset` in the PDF of `lang`.
async fn boxes_at(state: &AppStateRef<'_>, lang: Lang, offset: usize) -> Vec<PdfBox> {
    latex_forward(
        state.clone(),
        lang,
        offset,
        state.engine.text(),
        state.engine.document(),
    )
    .await
    .unwrap_or_default()
}

/// `text` holds only lines that open what follows (`\\begin{…}`, `\\label{…}`).
fn opens(text: &str) -> bool {
    text.lines().all(|line| {
        let line = line.trim();
        line.is_empty() || line.starts_with("\\begin{") || line.starts_with("\\label{")
    })
}

/// Each paired body paragraph of the editor that the cursor finds in a PDF
/// is selected again by a click there, in both PDFs.
async fn check_round_trips(state: &AppStateRef<'_>, editor: &str) {
    let text = state.engine.text();
    let units: Vec<u16> = text.encode_utf16().collect();
    let linked: Vec<_> = state
        .file()
        .pair
        .as_ref()
        .unwrap()
        .links
        .keys()
        .copied()
        .collect();
    let paragraphs: Vec<(usize, usize)> = state
        .engine
        .snapshot()
        .layout
        .iter()
        .filter(|s| linked.contains(&s.id))
        .map(|s| (s.from, s.to))
        .filter(|&(from, to)| {
            let body = String::from_utf16_lossy(&units[from..to]);
            body.chars().count() >= 60 && !body.trim_start().starts_with('\\')
        })
        .collect();
    for lang in [Lang::Zh, Lang::En] {
        let (mut found, mut back, mut misses) = (0, 0, Vec::new());
        for &(from, to) in &paragraphs {
            // A line of the paragraph: SyncTeX also names the column box
            // that holds it, whose middle is in another paragraph.
            let Some(b) = boxes_at(state, lang, from + 10)
                .await
                .into_iter()
                .find(|b| b.height < 40.0)
            else {
                continue;
            };
            found += 1;
            let hit = click_box(state, lang, &b, "").await.unwrap();
            match hit.and_then(|h| h.range) {
                Some(r) if r.from < to && r.to > from => back += 1,
                // The paragraph's opening line, such as the
                // `\\begin{theorem}[Title]` of a click on the title.
                Some(r)
                    if r.from < from && opens(&String::from_utf16_lossy(&units[r.from..from])) =>
                {
                    back += 1
                }
                other => misses.push((from, other)),
            }
        }
        eprintln!(
            "{editor}, {lang:?} PDF: {found} of {} paragraphs found, {back} clicked back, misses {misses:?}",
            paragraphs.len()
        );
        assert!(
            found * 10 >= paragraphs.len() * 9,
            "{editor}, {lang:?}: too few found"
        );
        assert_eq!(
            back, found,
            "{editor}, {lang:?}: clicks that missed their paragraph"
        );
    }
}

/// The author's own paper paired by hand (copies: `BIWRITE_REAL_PAIR_EN`
/// and `BIWRITE_REAL_PAIR_ZH` name the two main files, the Chinese one as
/// its author built it). Every paired paragraph of the English paper that
/// the cursor finds in a PDF is selected again by a click there.
#[tokio::test]
#[ignore = "needs a TeX distribution; uses BIWRITE_REAL_PAIR_EN and BIWRITE_REAL_PAIR_ZH"]
async fn a_real_paper_paired_by_hand_maps_clicks_both_ways() {
    let (Ok(en), Ok(zh)) = (
        std::env::var("BIWRITE_REAL_PAIR_EN"),
        std::env::var("BIWRITE_REAL_PAIR_ZH"),
    ) else {
        eprintln!("skipped: BIWRITE_REAL_PAIR_EN and BIWRITE_REAL_PAIR_ZH name no papers");
        return;
    };
    let (en_path, zh_path) = (PathBuf::from(en), PathBuf::from(zh));
    let (en_before, zh_before) = (
        std::fs::read(&en_path).unwrap(),
        std::fs::read(&zh_path).unwrap(),
    );
    let scratch = temp("real-pair");
    let app = tauri::test::mock_app();
    app.manage(app_state(&scratch));
    let state = app.state::<AppState>();
    let file = TextFile::decode(en_before.clone()).unwrap();
    pairing::open(&state, en_path.clone(), file, None);
    settle(&state).await;
    let view = pairing::pair_with(
        &state,
        state.engine.text(),
        state.engine.document(),
        en_path.clone(),
        zh_path.clone(),
    )
    .await
    .unwrap();
    let pair = view.pair.expect("paired");
    eprintln!("paired {} of {} paragraphs", pair.paired, pair.units);
    settle(&state).await;
    build(&state, Lang::Zh).await;
    build(&state, Lang::En).await;
    let synctex = zh_path.with_extension("synctex.gz");
    assert!(synctex.is_file(), "{}", synctex.display());

    // Every paired body paragraph of the English paper, then, swapped, of
    // the Chinese one, round trips through both PDFs.
    check_round_trips(&state, "English editor").await;
    crate::commands::swap(&state, state.engine.text(), false).unwrap();
    assert_eq!(state.file().path.as_deref(), Some(zh_path.as_path()));
    check_round_trips(&state, "Chinese editor").await;
    crate::commands::swap(&state, state.engine.text(), false).unwrap();
    assert_eq!(state.file().path.as_deref(), Some(en_path.as_path()));
    // Building and clicking wrote neither paper.
    assert_eq!(std::fs::read(&en_path).unwrap(), en_before);
    assert_eq!(std::fs::read(&zh_path).unwrap(), zh_before);
    std::fs::remove_dir_all(&scratch).unwrap();
}
