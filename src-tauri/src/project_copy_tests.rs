//! Save As and the translation's export into a new folder with the files
//! the paper needs. The original folder stays as it was, byte for byte.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use biwrite_core::TextFile;

use crate::pair_tests::{app_state, settle};
use crate::pairing;
use crate::project_copy::{export_into, paper_bundle, save_into};
use crate::state::Document;

const EN: &str = "\\documentclass{ieeecolor}\n\\newcommand{\\figdir}{../figures/}\n\\begin{document}\n\\section{Method}\nOur method prepares tables in three steps.\n\n\\includegraphics{\\figdir fig_a.pdf}\n\\bibliographystyle{IEEEtran}\n\\bibliography{../refs}\n\\end{document}\n";

const ZH: &str = "\\documentclass{ieeecolor}\n\\usepackage[UTF8]{ctex}\n\\newcommand{\\figdir}{../figures/}\n\\begin{document}\n\\section{方法}\n我们的方法分三步准备表格。\n\n\\includegraphics{\\figdir fig_a.pdf}\n\\bibliographystyle{IEEEtran}\n\\bibliography{../refs}\n\\end{document}\n";

/// A paper with `en/` and `zh/` folders beside the figures and the
/// bibliography they share, the figures named through a macro (so only the
/// build's record names them).
fn paper(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-copy-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (file, text) in [
        ("en/paper.tex", EN),
        ("en/ieeecolor.cls", "class"),
        ("en/IEEEtran.bst", "bst"),
        ("en/paper.aux", "aux"),
        ("en/paper.pdf", "pdf"),
        (
            "en/paper.fls",
            "INPUT ../figures/fig_a.pdf\nINPUT ./ieeecolor.cls\n",
        ),
        ("zh/paper_zh.tex", ZH),
        ("zh/ieeecolor.cls", "class"),
        ("zh/paper_zh.fls", "INPUT ../figures/fig_a.pdf\n"),
        ("figures/fig_a.pdf", "fig a"),
        ("refs.bib", "@article{x}"),
        ("notes/todo.txt", "not the paper's"),
    ] {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

/// Every file under `dir` with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    out
}

fn read(path: &Path) -> TextFile {
    TextFile::decode(std::fs::read(path).unwrap()).unwrap()
}

fn open(state: &Document, path: &Path, mirror: Option<&Path>) {
    pairing::open(
        state,
        path.to_path_buf(),
        read(path),
        mirror.map(|m| (m.to_path_buf(), read(m))),
    );
}

#[tokio::test]
async fn saving_with_the_files_copies_what_the_paper_needs_and_changes_nothing_here() {
    let root = std::env::temp_dir().join(format!("biwrite-copy-dest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = paper("save");
    let before = snapshot(&dir);
    let state = app_state(&dir);
    let main = dir.join("en/paper.tex");
    open(&state, &main, None);
    settle(&state).await;
    let edited = EN.replace("three steps.", "three steps, edited.");
    state.engine.update(edited.clone());
    state.file().dirty = true;

    let bundle = paper_bundle(&state).await.unwrap();
    let dest = root.join("paper-copy");
    let copy = save_into(&state, edited.clone(), &bundle, dest.clone())
        .await
        .unwrap();
    let after = snapshot(&dest);
    let names: Vec<String> = after.keys().map(|p| p.display().to_string()).collect();
    assert_eq!(
        names,
        [
            "en/IEEEtran.bst",
            "en/ieeecolor.cls",
            "en/paper.tex",
            "figures/fig_a.pdf",
            "refs.bib",
        ]
    );
    assert_eq!(copy.copied, 4, "the document itself is written, not copied");
    assert_eq!(after[Path::new("en/paper.tex")], edited.as_bytes());
    assert_eq!(after[Path::new("figures/fig_a.pdf")], b"fig a");
    // The document goes on from the new folder, saved.
    let new_main = std::fs::canonicalize(dest.join("en/paper.tex")).unwrap();
    assert_eq!(
        state
            .file()
            .path
            .as_deref()
            .map(|p| std::fs::canonicalize(p).unwrap()),
        Some(new_main)
    );
    assert!(!state.file().dirty);
    // The original folder is byte for byte as it was.
    assert_eq!(snapshot(&dir), before);

    // A folder with files in it is refused, and nothing is written there.
    let busy = root.join("busy");
    std::fs::create_dir_all(&busy).unwrap();
    std::fs::write(busy.join("mine.txt"), "mine").unwrap();
    let bundle = paper_bundle(&state).await.unwrap();
    assert!(
        save_into(&state, edited.clone(), &bundle, busy.clone())
            .await
            .is_err()
    );
    assert_eq!(snapshot(&busy).len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn a_pair_is_saved_with_both_folders() {
    let root = std::env::temp_dir().join(format!("biwrite-copy-out-pair-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = paper("pair");
    let before = snapshot(&dir);
    let state = app_state(&dir);
    open(
        &state,
        &dir.join("en/paper.tex"),
        Some(&dir.join("zh/paper_zh.tex")),
    );
    settle(&state).await;
    assert!(state.file().pair.is_some(), "paired");
    let bundle = paper_bundle(&state).await.unwrap();
    let dest = root.join("pair-copy");
    let copy = save_into(&state, EN.to_owned(), &bundle, dest.clone())
        .await
        .unwrap();
    let after = snapshot(&dest);
    assert_eq!(after[Path::new("en/paper.tex")], EN.as_bytes());
    assert_eq!(after[Path::new("zh/paper_zh.tex")], ZH.as_bytes());
    assert!(after.contains_key(Path::new("zh/ieeecolor.cls")));
    assert!(after.contains_key(Path::new("figures/fig_a.pdf")));
    assert!(copy.saved.is_some_and(|s| s.mirror.is_some()));
    let pair = state.file().pair.as_ref().unwrap().path.clone();
    assert_eq!(
        std::fs::canonicalize(pair).unwrap(),
        std::fs::canonicalize(dest.join("zh/paper_zh.tex")).unwrap(),
        "the pair goes on with the new Chinese file"
    );
    assert_eq!(snapshot(&dir), before);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn the_translation_is_exported_with_the_files() {
    let root = std::env::temp_dir().join(format!("biwrite-copy-out-export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = paper("export");
    let before = snapshot(&dir);
    let state = app_state(&dir);
    open(&state, &dir.join("en/paper.tex"), None);
    settle(&state).await;
    let document = state.engine.document();
    let (name, body, _) =
        crate::latex_commands::translation_tex(&state, EN.to_owned(), document).unwrap();
    assert_eq!(name, "paper_zh.tex");
    let bundle = paper_bundle(&state).await.unwrap();
    let dest = root.join("export");
    let copy = export_into(&state, &name, body.clone(), false, &bundle, dest.clone())
        .await
        .unwrap();
    let after = snapshot(&dest);
    assert_eq!(after[Path::new("en/paper_zh.tex")], body.as_bytes());
    assert_eq!(
        after[Path::new("en/paper.tex")],
        EN.as_bytes(),
        "as on disk"
    );
    assert!(after.contains_key(Path::new("refs.bib")));
    assert!(copy.exported.is_some());
    // The open document is still the original.
    assert_eq!(
        state
            .file()
            .path
            .as_deref()
            .map(|p| std::fs::canonicalize(p).unwrap()),
        Some(std::fs::canonicalize(dir.join("en/paper.tex")).unwrap())
    );
    assert_eq!(snapshot(&dir), before);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

/// A real paper saved with its files builds in the new folder as it does
/// in its own. `BIWRITE_BUNDLE_PAPER` names the main file of a copy built
/// once (its records name the files outside its folder).
#[tokio::test]
#[ignore = "needs BIWRITE_BUNDLE_PAPER and a TeX distribution"]
async fn a_real_paper_saved_with_its_files_builds_there() {
    let main = PathBuf::from(std::env::var("BIWRITE_BUNDLE_PAPER").unwrap());
    let dir = main.parent().unwrap().parent().unwrap().to_path_buf();
    let before = snapshot(&dir);
    let state = app_state(
        &std::env::temp_dir().join(format!("biwrite-copy-real-cfg-{}", std::process::id())),
    );
    open(&state, &main, None);
    settle(&state).await;
    let text = state.engine.text();
    let bundle = paper_bundle(&state).await.unwrap();
    eprintln!(
        "base {} with {} files, left out {:?}",
        bundle.base.display(),
        bundle.files.len(),
        bundle.left_out
    );
    let dest = std::env::temp_dir().join(format!("biwrite-copy-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    let copy = save_into(&state, text, &bundle, dest.clone())
        .await
        .unwrap();
    let new_main = PathBuf::from(&copy.saved.as_ref().unwrap().path);
    let tc = biwrite_latex::detect(biwrite_latex::find_bin(None).unwrap()).await;
    let rel = new_main.file_name().unwrap().to_string_lossy().into_owned();
    let job = biwrite_latex::Job {
        dir: new_main.parent().unwrap().to_path_buf(),
        root: PathBuf::from(rel),
        engine: biwrite_latex::engine_for(&std::fs::read_to_string(&new_main).unwrap()),
        out_dir: None,
        timeout: std::time::Duration::from_secs(300),
    };
    let built = biwrite_latex::compile(&tc, &job).await.unwrap();
    let errors: Vec<_> = built
        .issues
        .iter()
        .filter(|i| i.severity == biwrite_latex::Severity::Error)
        .collect();
    eprintln!(
        "{:?} in {} ms with {}: {errors:?}",
        built.outcome, built.duration_ms, built.tool
    );
    assert_eq!(
        built.outcome,
        biwrite_latex::Outcome::Ok,
        "{}",
        built.output
    );
    let log = std::fs::read_to_string(new_main.with_extension("log")).unwrap();
    assert!(!log.contains("not found"), "a file was not copied");
    assert!(
        !log.contains("Citation") || !log.contains("undefined"),
        "the bibliography was not copied"
    );
    assert_eq!(snapshot(&dir), before, "the original folder changed");
    std::fs::remove_dir_all(&dest).unwrap();
}
