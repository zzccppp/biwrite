//! Builds with the TeX distribution of this computer: every built-in
//! template compiles, SyncTeX answers in both directions, the Chinese
//! mirror compiles with XeLaTeX, and a timeout stops the engine.
//!
//! Needs TeX, so ignored by default:
//! `cargo test -p biwrite-latex --test tex -- --ignored --test-threads=1`

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use biwrite_latex::{
    Engine, Job, MIRROR_DIR, Outcome, Severity, Toolchain, compile, detect, engine_for, find_bin,
    locate, synctex, templates, with_chinese,
};

fn builtin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/resources/templates")
}

async fn toolchain() -> Toolchain {
    let bin = find_bin(None).expect("no TeX distribution found");
    detect(bin).await
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-tex-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn job(dir: &Path, root: &str, engine: Engine) -> Job {
    Job {
        dir: dir.to_path_buf(),
        root: PathBuf::from(root),
        engine,
        out_dir: None,
        timeout: Duration::from_secs(240),
    }
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn every_builtin_template_compiles_and_syncs() {
    let tc = toolchain().await;
    eprintln!("{} in {}", tc.distribution, tc.bin.display());
    let user = scratch("user");
    let list = templates::list(&builtin(), &user);
    let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["iclr2027", "ieee-tii", "pvldb"]);
    for t in &list {
        let dest = scratch(&t.id);
        let (dir, _) = templates::find(&builtin(), &user, &t.id).unwrap();
        let main = templates::instantiate(&dir, &dest).unwrap();
        let text = std::fs::read_to_string(&main).unwrap();
        let engine = engine_for(&text);
        let rel = main.file_name().unwrap().to_string_lossy().into_owned();
        let built = compile(&tc, &job(&dest, &rel, engine)).await.unwrap();
        let errors: Vec<_> = built
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        eprintln!(
            "{}: {:?} in {} ms with {}, {} issues",
            t.id,
            built.outcome,
            built.duration_ms,
            built.tool,
            built.issues.len()
        );
        assert_eq!(
            built.outcome,
            Outcome::Ok,
            "{}: {errors:?}\n{}",
            t.id,
            built.output
        );
        let pdf = built.pdf.unwrap();
        assert!(!built.stale);

        // Forward: a line of the first paragraph lands on page 1.
        let line = text
            .lines()
            .position(|l| l.starts_with("\\section"))
            .map(|i| i as u32 + 2)
            .unwrap();
        let boxes = synctex::forward(&tc, &pdf, &main, line, 0).await.unwrap();
        assert!(!boxes.is_empty(), "{}: no box for line {line}", t.id);
        // Inverse: the middle of the first box points back into the file.
        let b = &boxes[0];
        let point = synctex::inverse(
            &tc,
            &pdf,
            b.page,
            b.left + b.width / 2.0,
            b.top + b.height / 2.0,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(point.file.file_name(), main.file_name());
        assert!(
            point.line.abs_diff(line) <= 3,
            "{}: line {} for {line}",
            t.id,
            point.line
        );
        std::fs::remove_dir_all(&dest).unwrap();
    }
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn the_chinese_mirror_compiles_with_xelatex() {
    let tc = toolchain().await;
    let user = scratch("user-zh");
    for id in ["iclr2027", "ieee-tii", "pvldb"] {
        let dest = scratch(&format!("zh-{id}"));
        let (dir, t) = templates::find(&builtin(), &user, id).unwrap();
        let main = templates::instantiate(&dir, &dest).unwrap();
        let text = std::fs::read_to_string(&main).unwrap();
        // A Chinese paragraph after the first section heading.
        let at = text.find("\\section").unwrap();
        let end = at + text[at..].find('\n').unwrap() + 1;
        let zh = format!(
            "{}本文研究表格数据清洗中的行级修复问题，并给出可验证的预算分配方法。\n{}",
            &text[..end],
            &text[end..]
        );
        let mirror = with_chinese(&zh);
        let rel = Path::new(MIRROR_DIR).join(&t.manifest.main);
        std::fs::create_dir_all(dest.join(MIRROR_DIR)).unwrap();
        std::fs::write(dest.join(&rel), &mirror).unwrap();
        let mut j = job(&dest, &rel.to_string_lossy(), Engine::Xelatex);
        j.out_dir = Some(PathBuf::from(MIRROR_DIR));
        let built = compile(&tc, &j).await.unwrap();
        eprintln!("{id} zh: {:?} in {} ms", built.outcome, built.duration_ms);
        let errors: Vec<_> = built
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        assert_eq!(
            built.outcome,
            Outcome::Ok,
            "{id}: {errors:?}\n{}",
            built.output
        );
        let pdf = built.pdf.unwrap();
        assert!(pdf.starts_with(dest.join(MIRROR_DIR)));

        // Clicking the Chinese sentence in the mirror PDF finds it again.
        let line = zh[..end].lines().count() as u32 + 1;
        let boxes = synctex::forward(&tc, &pdf, &dest.join(&rel), line, 0)
            .await
            .unwrap();
        assert!(!boxes.is_empty(), "{id}: no box for the Chinese line");
        let b = &boxes[0];
        let point = synctex::inverse(&tc, &pdf, b.page, b.left + 10.0, b.top + b.height / 2.0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(point.line, line, "{id}");
        let span = "本文研究表格数据清洗中的行级修复问题";
        let offset = locate(&mirror, point.line, span, span.find("行级").unwrap());
        assert!(mirror[offset..].starts_with("行级"), "{id}");
        std::fs::remove_dir_all(&dest).unwrap();
    }
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn errors_are_reported_with_their_line() {
    let tc = toolchain().await;
    let dir = scratch("errors");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nText.\n\\undefinedmacro\nMore text \\cite{missing}.\n\\end{document}\n",
    )
    .unwrap();
    let built = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    assert_eq!(built.outcome, Outcome::Errors);
    let first = &built.issues[0];
    assert_eq!(first.severity, Severity::Error);
    assert_eq!(first.file.as_deref(), Some("main.tex"));
    assert_eq!(first.line, Some(4));
    assert!(
        built
            .issues
            .iter()
            .any(|i| i.severity == Severity::Warning && i.line == Some(5))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn xelatex_writes_a_pdf_despite_errors() {
    let tc = toolchain().await;
    let dir = scratch("xe-errors");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "\\documentclass{article}\n\\usepackage{graphicx}\n\\usepackage[UTF8]{ctex}\n\\begin{document}\n中文段落。\n\\includegraphics{missing.pdf}\n\\end{document}\n",
    )
    .unwrap();
    let built = compile(&tc, &job(&dir, "main.tex", Engine::Xelatex))
        .await
        .unwrap();
    assert_eq!(built.outcome, Outcome::Errors, "{}", built.output);
    assert!(built.pdf.is_some());
    assert!(
        built
            .issues
            .iter()
            .any(|i| i.severity == Severity::Error && i.line == Some(6))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_build_that_never_ends_is_stopped() {
    let tc = toolchain().await;
    let dir = scratch("loop");
    std::fs::create_dir_all(&dir).unwrap();
    // A name no other process has, to look for leftovers by.
    let name = format!("endless-{}.tex", std::process::id());
    std::fs::write(
        dir.join(&name),
        "\\documentclass{article}\n\\begin{document}\n\\def\\x{\\x}\\x\n\\end{document}\n",
    )
    .unwrap();
    let mut j = job(&dir, &name, Engine::Pdflatex);
    j.timeout = Duration::from_secs(3);
    let started = std::time::Instant::now();
    let built = compile(&tc, &j).await.unwrap();
    assert_eq!(built.outcome, Outcome::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(10));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let ps = std::process::Command::new("pgrep")
        .args(["-f", &name])
        .output()
        .unwrap();
    let running = String::from_utf8_lossy(&ps.stdout);
    assert!(running.trim().is_empty(), "still running: {running}");
    std::fs::remove_dir_all(&dir).unwrap();
}
