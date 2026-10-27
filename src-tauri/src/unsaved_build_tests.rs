//! Building a document with edits not saved yet: the PDF shows the text as
//! edited, built from copies in the project's `.biwrite` folder, and the
//! project's files stay exactly as they were. Once the text is as saved,
//! the PDF is built from the files again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use biwrite_core::TextFile;
use tauri::Manager;

use crate::latex_commands::{Lang, latex_compile, latex_forward, latex_inverse, pdf_of};
use crate::pair_tests::{app_state, settle};
use crate::pairing;
use crate::state::{Doc, Document};

const MAIN: &str = "\\documentclass{article}\n\
\\begin{document}\n\
\\section{Introduction}\n\
Tabular foundation models do well on small tasks, but errors in the data cost accuracy.\n\n\
We assign one cleaning action to each row and test it on twelve datasets.\n\
\\end{document}\n";

const ROOT: &str = "\\documentclass{article}\n\
\\begin{document}\n\
\\input{sections/intro}\n\
\\end{document}\n";

const INTRO: &str = "% !TEX root = ../main.tex\n\
\\section{Introduction}\n\
We assign one cleaning action to each row and test it on twelve datasets.\n";

const ZH: &str = "\\documentclass{article}\n\
\\usepackage[UTF8]{ctex}\n\
\\begin{document}\n\
\\section{引言}\n\
表格基础模型在小样本任务上表现突出，但数据中的错误会降低其精度。\n\n\
我们为每一行分配一个清洗动作，并在十二个数据集上检验。\n\
\\end{document}\n";

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-unsaved-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // As the builds name it (the temporary folder may be behind a link).
    std::fs::canonicalize(dir).unwrap()
}

/// Every file of `dir` outside `.biwrite`, with its bytes.
fn project_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == ".biwrite") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn open(state: &Document, path: &Path) {
    let file = TextFile::decode(std::fs::read(path).unwrap()).unwrap();
    let mirror = pairing::counterpart(path).map(|m| {
        (
            m.clone(),
            TextFile::decode(std::fs::read(m).unwrap()).unwrap(),
        )
    });
    pairing::open(state, path.to_owned(), file, mirror);
}

fn utf16(text: &str, needle: &str) -> usize {
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} in {text}"));
    text[..at].encode_utf16().count()
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn an_unsaved_document_is_built_from_a_copy_and_its_file_stays_as_saved() {
    let dir = temp("doc");
    let path = dir.join("main.tex");
    std::fs::write(&path, MAIN).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let before = project_files(&dir);

    let edited = MAIN.replace("twelve datasets", "fourteen datasets");
    let view = latex_compile(
        state.clone(),
        Lang::En,
        edited.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(view.has_pdf, "{}", view.output);
    assert!(!view.unsaved_left_out);
    assert_eq!(
        project_files(&dir),
        before,
        "nothing of the project changed"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(".biwrite/en/main.tex")).unwrap(),
        edited
    );
    let pdf = pdf_of(&state, Lang::En).unwrap();
    assert!(
        pdf.starts_with(dir.join(".biwrite/en")),
        "{}",
        pdf.display()
    );

    // The edited sentence is found in the PDF, and a click on it lands on it.
    let at = utf16(&edited, "We assign");
    let boxes = latex_forward(
        state.clone(),
        Lang::En,
        at,
        edited.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    let b = boxes.first().expect("a box for the edited paragraph");
    let span = "We assign one cleaning action";
    let hit = latex_inverse(
        state.clone(),
        Lang::En,
        b.page,
        b.left + b.width / 2.0,
        b.top + b.height / 2.0,
        span.to_owned(),
        4,
        edited.clone(),
        state.engine.document(),
    )
    .await
    .unwrap()
    .expect("a hit");
    let r = hit.range.expect("a range");
    let units: Vec<u16> = edited.encode_utf16().collect();
    let picked = String::from_utf16_lossy(&units[r.from..r.to]);
    assert!(picked.contains("fourteen datasets"), "{picked}");

    // As saved, the PDF is built from the file again.
    let view = latex_compile(
        state.clone(),
        Lang::En,
        MAIN.to_owned(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(view.has_pdf, "{}", view.output);
    assert_eq!(pdf_of(&state, Lang::En).unwrap(), dir.join("main.pdf"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn an_unsaved_part_is_built_through_a_copy_of_its_root() {
    let dir = temp("part");
    std::fs::create_dir_all(dir.join("sections")).unwrap();
    std::fs::write(dir.join("main.tex"), ROOT).unwrap();
    let path = dir.join("sections/intro.tex");
    std::fs::write(&path, INTRO).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let before = project_files(&dir);

    let edited = INTRO.replace("twelve datasets", "fourteen datasets");
    let view = latex_compile(
        state.clone(),
        Lang::En,
        edited.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(view.has_pdf, "{}", view.output);
    assert!(!view.unsaved_left_out);
    assert_eq!(
        project_files(&dir),
        before,
        "nothing of the project changed"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(".biwrite/en/sections/intro.tex")).unwrap(),
        edited
    );
    let root = std::fs::read_to_string(dir.join(".biwrite/en/main.tex")).unwrap();
    assert!(
        root.contains("\\input{.biwrite/en/sections/intro}"),
        "{root}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_part_its_root_does_not_include_directly_is_built_as_saved() {
    let dir = temp("indirect");
    std::fs::create_dir_all(dir.join("sections")).unwrap();
    // Through a macro: the copy could not be put in its place.
    let root = ROOT.replace(
        "\\input{sections/intro}",
        "\\newcommand{\\part}[1]{\\input{sections/#1}}\\part{intro}",
    );
    std::fs::write(dir.join("main.tex"), root).unwrap();
    let path = dir.join("sections/intro.tex");
    std::fs::write(&path, INTRO).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let before = project_files(&dir);

    let edited = INTRO.replace("twelve datasets", "fourteen datasets");
    let view = latex_compile(state.clone(), Lang::En, edited, state.engine.document())
        .await
        .unwrap();
    assert!(view.unsaved_left_out);
    assert!(view.has_pdf, "{}", view.output);
    // Built in place, as before: only the build's own files are new.
    let after = project_files(&dir);
    for (name, bytes) in &before {
        assert_eq!(after.get(name), Some(bytes), "{name}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_pairs_unsaved_translation_is_built_from_a_copy_and_clicks_find_it() {
    let dir = temp("pair");
    let (en, zh) = (dir.join("paper.tex"), dir.join("paper_zh.tex"));
    std::fs::write(&en, MAIN).unwrap();
    std::fs::write(&zh, ZH).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &en);
    assert!(state.file().pair.is_some());
    settle(&state).await;
    let before = project_files(&dir);

    // An English paragraph changes: its new translation is not saved.
    let edited = MAIN.replace("twelve datasets", "fourteen datasets");
    state.engine.update(edited.clone());
    settle(&state).await;
    let view = latex_compile(
        state.clone(),
        Lang::Zh,
        edited.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(view.has_pdf, "{}", view.output);
    assert!(!view.unsaved_left_out);
    assert_eq!(
        project_files(&dir),
        before,
        "nothing of the project changed"
    );
    let copy = std::fs::read_to_string(dir.join(".biwrite/zh/paper_zh.tex")).unwrap();
    assert_ne!(copy, ZH, "the copy has the new translation");
    assert!(copy.contains("\\usepackage[UTF8]{ctex}"), "{copy}");

    // The edited paragraph is found in the Chinese PDF, and a click on it
    // comes back to it.
    let at = utf16(&edited, "We assign");
    let boxes = latex_forward(
        state.clone(),
        Lang::Zh,
        at,
        edited.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    let b = boxes.first().expect("a box for the edited paragraph");
    let hit = latex_inverse(
        state.clone(),
        Lang::Zh,
        b.page,
        b.left + b.width / 2.0,
        b.top + b.height / 2.0,
        String::new(),
        0,
        edited.clone(),
        state.engine.document(),
    )
    .await
    .unwrap()
    .expect("a hit");
    let r = hit.range.expect("a range");
    let units: Vec<u16> = edited.encode_utf16().collect();
    let picked = String::from_utf16_lossy(&units[r.from..r.to]);
    assert!(picked.contains("fourteen datasets"), "{picked}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_bibliography_beside_the_project_folder_is_found_by_an_unsaved_build() {
    let top = temp("bib");
    let dir = top.join("paper");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        top.join("refs.bib"),
        "@article{smith2020,\n  author = {Smith, John},\n  title = {A Study},\n  journal = {Journal},\n  year = {2020}\n}\n",
    )
    .unwrap();
    let paper = MAIN.replace(
        "twelve datasets.\n\\end{document}",
        "twelve datasets~\\cite{smith2020}.\n\\bibliographystyle{plain}\n\\bibliography{../refs}\n\\end{document}",
    );
    let path = dir.join("main.tex");
    std::fs::write(&path, &paper).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&top.join("cfg")));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let before = project_files(&top);

    // Never built in its folder: BibTeX runs in the build's own.
    let edited = paper.replace("twelve datasets", "fourteen datasets");
    let view = latex_compile(state.clone(), Lang::En, edited, state.engine.document())
        .await
        .unwrap();
    assert!(view.has_pdf, "{}", view.output);
    let bbl = std::fs::read_to_string(dir.join(".biwrite/en/main.bbl")).unwrap_or_default();
    assert!(
        bbl.contains("\\bibitem{smith2020}"),
        "{bbl}\n{}",
        view.output
    );
    let log = std::fs::read_to_string(dir.join(".biwrite/en/main.log")).unwrap();
    assert!(
        !log.contains("Citation `smith2020' on page 1 undefined"),
        "{log}"
    );
    // The copy BibTeX read is BiWrite's, beside its builds.
    assert!(dir.join(".biwrite/refs.bib").is_file());
    let after: BTreeMap<String, Vec<u8>> = project_files(&top)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("cfg"))
        .collect();
    assert_eq!(after, before, "nothing outside .biwrite changed");
    std::fs::remove_dir_all(&top).unwrap();
}

/// The editor as saved, but the file changed elsewhere since (the change
/// ignored): the PDF shows what the editor shows, built from a copy, not
/// the other version on disk.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_file_changed_elsewhere_is_built_as_the_editor_shows_it() {
    let dir = temp("elsewhere");
    let path = dir.join("main.tex");
    std::fs::write(&path, MAIN).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let theirs = MAIN.replace("twelve datasets", "ELSEWHERE datasets");
    std::fs::write(&path, &theirs).unwrap();
    let view = latex_compile(
        state.clone(),
        Lang::En,
        MAIN.to_owned(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(view.has_pdf, "{}", view.output);
    assert_eq!(
        std::fs::read_to_string(dir.join(".biwrite/en/main.tex")).unwrap(),
        MAIN
    );
    assert!(
        pdf_of(&state, Lang::En)
            .unwrap()
            .starts_with(dir.join(".biwrite/en"))
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        theirs,
        "left alone"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A pair's paragraph not translated yet goes into the Chinese build as it
/// is, and the build says so.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_pairs_paragraph_not_translated_yet_is_counted_in_its_build() {
    let dir = temp("pair-pending");
    let (en, zh) = (dir.join("paper.tex"), dir.join("paper_zh.tex"));
    std::fs::write(&en, MAIN).unwrap();
    std::fs::write(&zh, ZH).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &en);
    settle(&state).await;
    state.engine.set_auto_translate(false);
    let edited = MAIN.replace("twelve datasets", "fourteen datasets");
    state.engine.update(edited.clone());
    let view = latex_compile(state.clone(), Lang::Zh, edited, state.engine.document())
        .await
        .unwrap();
    assert!(view.has_pdf, "{}", view.output);
    assert_eq!(view.untranslated, 1);
    std::fs::remove_dir_all(&dir).unwrap();
}
