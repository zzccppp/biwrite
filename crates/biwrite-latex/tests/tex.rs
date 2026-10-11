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

/// A project naming a file `-shell-escape` as its root must not get shell
/// escape: the name is refused before any tool runs.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_root_named_like_an_option_is_refused() {
    let tc = toolchain().await;
    let dir = scratch("option-root");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("-shell-escape"), "").unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "% !TEX root = -shell-escape\n\\documentclass{article}\n\\begin{document}\n\
         \\immediate\\write18{touch PWNED}\nhello\n\\end{document}\n",
    )
    .unwrap();
    let err = compile(&tc, &job(&dir, "-shell-escape", Engine::Pdflatex))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("can't be compiled"), "{err}");
    // latexmk runs the engine through the shell: a backtick runs a command.
    let quoted = "`touch PWNED`.tex";
    std::fs::write(
        dir.join(quoted),
        "\\documentclass{article}\\begin{document}x\\end{document}\n",
    )
    .unwrap();
    let err = compile(&tc, &job(&dir, quoted, Engine::Pdflatex))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("can't be compiled"), "{err}");
    assert!(!dir.join("PWNED").exists());
    assert!(!dir.join("main.pdf").exists(), "nothing ran");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A project's own latexmkrc runs with the build (latexmk reads it), so the
/// build says so, for the user to be warned.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_project_latexmkrc_is_reported() {
    let tc = toolchain().await;
    let dir = scratch("rc");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nhello\n\\end{document}\n",
    )
    .unwrap();
    let built = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    assert_eq!(built.project_rc, None);
    std::fs::write(dir.join("latexmkrc"), "$pdf_mode = 1;\n").unwrap();
    let built = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    if tc.latexmk {
        assert_eq!(built.project_rc.as_deref(), Some("latexmkrc"));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Build `root` in `dir` as an author's own latexmk run would, without
/// SyncTeX.
fn build_elsewhere(tc: &Toolchain, dir: &Path, flag: &str, root: &str) {
    let path = format!(
        "{}:{}",
        tc.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let status = std::process::Command::new(tc.tool("latexmk"))
        .args([flag, "-interaction=nonstopmode", root])
        .current_dir(dir)
        .env("PATH", path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

/// A PDF built elsewhere without SyncTeX is up to date for latexmk, which
/// would leave it so: the build runs again and writes the SyncTeX data that
/// clicks need. The next build, with nothing changed, runs nothing.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_pdf_built_elsewhere_without_synctex_gets_it() {
    let tc = toolchain().await;
    if !tc.latexmk {
        return;
    }
    let dir = scratch("nosynctex");
    std::fs::create_dir_all(&dir).unwrap();
    let source = "\\documentclass{article}\n\\begin{document}\nHello world, a sentence to click.\n\\end{document}\n";
    std::fs::write(dir.join("main.tex"), source).unwrap();
    build_elsewhere(&tc, &dir, "-pdf", "main.tex");
    assert!(dir.join("main.pdf").is_file());
    assert!(!dir.join("main.synctex.gz").exists());

    let built = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    assert_eq!(built.outcome, Outcome::Ok, "{}", built.output);
    assert!(!built.stale, "the PDF was built again");
    assert!(dir.join("main.synctex.gz").is_file());
    assert!(
        built.issues.iter().all(|i| !i.message.contains("SyncTeX")),
        "{:?}",
        built.issues
    );
    let point = synctex::inverse(&tc, &dir.join("main.pdf"), 1, 150.0, 130.0)
        .await
        .unwrap();
    assert!(point.is_some(), "SyncTeX answers for the rebuilt PDF");

    // Nothing changed: latexmk has nothing to do.
    let again = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    assert!(again.stale, "an up-to-date PDF is not built again");
    // The source was never written.
    assert_eq!(
        std::fs::read_to_string(dir.join("main.tex")).unwrap(),
        source
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// xdvipdfmx gives up on an EPS image it cannot convert (as when the app
/// could not find Ghostscript) after writing only the first bytes of the
/// PDF. The PDF that was there before stays, byte for byte, and the build
/// says so.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_build_that_breaks_its_pdf_keeps_the_previous_one() {
    let tc = toolchain().await;
    if !tc.has(Engine::Xelatex) {
        return;
    }
    let dir = scratch("keep-pdf");
    std::fs::create_dir_all(&dir).unwrap();
    let doc = |figure: &str| {
        format!(
            "\\documentclass{{article}}\n\\usepackage{{graphicx}}\n\\begin{{document}}\n\
             A paragraph to keep.\n{figure}\n\\end{{document}}\n"
        )
    };
    std::fs::write(dir.join("main.tex"), doc("")).unwrap();
    let first = compile(&tc, &job(&dir, "main.tex", Engine::Xelatex))
        .await
        .unwrap();
    assert_eq!(first.outcome, Outcome::Ok, "{}", first.output);
    let good = std::fs::read(dir.join("main.pdf")).unwrap();

    // An EPS file that is not PostScript. XeLaTeX takes its bounding box,
    // and xdvipdfmx cannot convert it.
    std::fs::write(
        dir.join("broken.eps"),
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 20 20\nthis is not postscript (\n",
    )
    .unwrap();
    std::fs::write(dir.join("main.tex"), doc("\\includegraphics{broken.eps}")).unwrap();
    let broken = compile(&tc, &job(&dir, "main.tex", Engine::Xelatex))
        .await
        .unwrap();
    assert_ne!(broken.outcome, Outcome::Ok, "{}", broken.output);
    assert_eq!(
        std::fs::read(dir.join("main.pdf")).unwrap(),
        good,
        "the PDF was replaced"
    );
    assert!(broken.stale, "the PDF on show is the previous one");
    assert!(
        broken
            .issues
            .iter()
            .any(|i| i.message.contains("previous PDF is kept")),
        "{:?}",
        broken.issues
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A run that failed while writing the PDF left only its first bytes, and
/// latexmk, the sources unchanged, would call that up to date. The next
/// build writes the whole PDF again.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn an_unfinished_pdf_is_built_again() {
    let tc = toolchain().await;
    if !tc.latexmk {
        return;
    }
    let dir = scratch("unfinished");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nA paragraph.\n\\end{document}\n",
    )
    .unwrap();
    let first = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    assert_eq!(first.outcome, Outcome::Ok, "{}", first.output);
    // What xdvipdfmx leaves when it stops early, the header alone.
    std::fs::write(dir.join("main.pdf"), b"%PDF-1.5\n%\xe4\xf0\xed\xf8\n").unwrap();
    let again = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    assert_eq!(again.outcome, Outcome::Ok, "{}", again.output);
    assert!(!again.stale, "built again");
    let pdf = std::fs::read(dir.join("main.pdf")).unwrap();
    assert!(
        pdf.len() > 1024 && pdf.ends_with(b"%%EOF\n"),
        "{} bytes",
        pdf.len()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Every file under `dir` with its bytes, to see that a build changed none.
fn sources(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "tex"))
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect();
    out.sort();
    out
}

/// A paper in English with Chinese in its title, a heading, a paragraph,
/// bold and italics, and no package for Chinese. It builds with each
/// engine, Latin text keeps the document's fonts, no file changes, and a
/// click on the Chinese paragraph finds its line.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn mixed_chinese_and_english_papers_build() {
    let tc = toolchain().await;
    let cases = [
        (
            "pdf",
            Engine::Pdflatex,
            "\\usepackage[T1]{fontenc}\n\\usepackage{times}\n\\usepackage{hyperref}\n",
        ),
        (
            "xe",
            Engine::Xelatex,
            "\\usepackage{fontspec}\n\\usepackage{hyperref}\n",
        ),
        ("lua", Engine::Lualatex, "\\usepackage{fontspec}\n"),
    ];
    for (name, engine, preamble) in cases {
        if !tc.has(engine) {
            continue;
        }
        let dir = scratch(&format!("mixed-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let text = format!(
            "\\documentclass{{article}}\n{preamble}\\title{{Mixed 中文标题}}\n\\author{{Author 作者}}\n\\begin{{document}}\n\\maketitle\n\\section{{Method 方法}}\nEnglish text, then 中文段落，带标点。\\textbf{{粗体中文}} and \\textit{{斜体中文}}.\n\nAn English paragraph.\n\\end{{document}}\n"
        );
        std::fs::write(dir.join("main.tex"), &text).unwrap();
        let before = sources(&dir);
        let built = compile(&tc, &job(&dir, "main.tex", engine)).await.unwrap();
        eprintln!(
            "{name}: {:?} in {} ms with {}, issues {:?}",
            built.outcome, built.duration_ms, built.tool, built.issues
        );
        assert_eq!(built.outcome, Outcome::Ok, "{name}: {}", built.output);
        assert!(built.tool.ends_with("+ CJK"), "{name}: {}", built.tool);
        assert!(
            built.issues.iter().all(|i| i.severity != Severity::Error),
            "{name}: {:?}",
            built.issues
        );
        let log = std::fs::read_to_string(dir.join("main.log")).unwrap();
        assert!(
            !log.contains("Missing character") && !log.contains("Unicode character"),
            "{name}: characters left out"
        );
        if engine == Engine::Pdflatex {
            // The Chinese font's other shapes come without a warning, and
            // the title keeps to one line.
            assert!(
                built.issues.iter().all(|i| i.severity == Severity::Box),
                "{name}: {:?}",
                built.issues
            );
            // The font files pdfTeX embedded, as its log lists them.
            assert!(log.contains("gbsnu"), "{name}: no Chinese font");
            assert!(log.contains("utmr8a"), "{name}: Times replaced");
        }
        assert_eq!(sources(&dir), before, "{name}: a file changed");

        let pdf = built.pdf.unwrap();
        let line = text
            .lines()
            .position(|l| l.starts_with("English text"))
            .unwrap() as u32
            + 1;
        let boxes = synctex::forward(&tc, &pdf, &dir.join("main.tex"), line, 0)
            .await
            .unwrap();
        assert!(!boxes.is_empty(), "{name}: no box for the mixed line");
        let b = &boxes[0];
        let point = synctex::inverse(&tc, &pdf, b.page, b.left + 10.0, b.top + b.height / 2.0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(point.file.file_name().unwrap(), "main.tex", "{name}");
        assert_eq!(point.line, line, "{name}");

        // Built again unchanged, still with the support.
        let again = compile(&tc, &job(&dir, "main.tex", engine)).await.unwrap();
        assert_eq!(again.outcome, Outcome::Ok, "{name}: {}", again.output);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// A mixed paper whose last build, made without the support, failed and
/// wrote no PDF. latexmk takes it for done, and the build with the support
/// runs anyway.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_mixed_paper_that_failed_before_builds() {
    let tc = toolchain().await;
    if !tc.latexmk {
        return;
    }
    let dir = scratch("mixed-failed");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nEnglish, then 中文。\n\\end{document}\n",
    )
    .unwrap();
    // The author's own build without the support stops at the Chinese.
    let path = format!(
        "{}:{}",
        tc.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let _ = std::process::Command::new(tc.tool("latexmk"))
        .args(["-pdf", "-interaction=nonstopmode", "main.tex"])
        .current_dir(&dir)
        .env("PATH", path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        std::fs::read_to_string(dir.join("main.log"))
            .unwrap()
            .contains("Unicode character"),
        "the outside build should fail"
    );
    let built = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    assert_eq!(built.outcome, Outcome::Ok, "{}", built.output);
    assert!(built.tool.ends_with("+ CJK"), "{}", built.tool);
    // Nothing changed since, so nothing is left to do.
    let again = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    assert_eq!(again.outcome, Outcome::Ok, "{}", again.output);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Chinese the scan cannot see (in a file named through a macro) shows in
/// the log, and the build is made again with the support.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn chinese_in_a_file_named_by_a_macro_is_found_in_the_log() {
    let tc = toolchain().await;
    let dir = scratch("mixed-macro");
    std::fs::create_dir_all(dir.join("sections")).unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "\\documentclass{article}\n\\newcommand{\\secdir}{sections}\n\\begin{document}\nEnglish.\n\n\\input{\\secdir/intro}\n\\end{document}\n",
    )
    .unwrap();
    std::fs::write(dir.join("sections/intro.tex"), "中文段落。\n").unwrap();
    let built = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    eprintln!(
        "{:?} with {}: {:?}",
        built.outcome, built.tool, built.issues
    );
    assert_eq!(built.outcome, Outcome::Ok, "{}", built.output);
    assert!(built.tool.ends_with("+ CJK"), "{}", built.tool);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A Chinese paper without a Chinese package builds with pdfLaTeX, and a
/// paper with one is built as it is.
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn chinese_papers_build_with_or_without_their_own_support() {
    let tc = toolchain().await;
    let dir = scratch("zh-plain");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\n\\section{引言}\n本文研究表格数据清洗中的行级修复问题，引用 English terms 也可以。\n\\end{document}\n",
    )
    .unwrap();
    let built = compile(&tc, &job(&dir, "main.tex", Engine::Pdflatex))
        .await
        .unwrap();
    assert_eq!(built.outcome, Outcome::Ok, "{}", built.output);
    assert!(built.tool.ends_with("+ CJK"), "{}", built.tool);

    std::fs::write(
        dir.join("own.tex"),
        "\\documentclass{article}\n\\usepackage[UTF8]{ctex}\n\\begin{document}\n本文 with English.\n\\end{document}\n",
    )
    .unwrap();
    let own = compile(&tc, &job(&dir, "own.tex", Engine::Xelatex))
        .await
        .unwrap();
    assert_eq!(own.outcome, Outcome::Ok, "{}", own.output);
    assert!(!own.tool.contains("CJK"), "{}", own.tool);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A real paper with Chinese in its English text, in the state its folder
/// is in (earlier failed builds included), builds without errors.
/// `BIWRITE_MIXED_PAPER` names its main file in a copy.
#[tokio::test]
#[ignore = "needs BIWRITE_MIXED_PAPER and a TeX distribution"]
async fn a_real_mixed_paper_builds() {
    let tc = toolchain().await;
    let main = PathBuf::from(std::env::var("BIWRITE_MIXED_PAPER").unwrap());
    let dir = main.parent().unwrap().to_path_buf();
    let rel = main.file_name().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read_to_string(&main).unwrap();
    let built = compile(&tc, &job(&dir, &rel, engine_for(&text)))
        .await
        .unwrap();
    let errors: Vec<_> = built
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    eprintln!(
        "{:?} in {} ms with {}: {errors:?}",
        built.outcome, built.duration_ms, built.tool
    );
    assert_eq!(built.outcome, Outcome::Ok, "{}", built.output);
    assert!(built.tool.ends_with("+ CJK"), "{}", built.tool);
    assert_eq!(
        std::fs::read_to_string(&main).unwrap(),
        text,
        "the file changed"
    );
}
