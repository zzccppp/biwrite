//! Figures that are files, from the model's answer to the saved project:
//! a PowerPoint diagram and its PDF, or a TikZ picture repaired until it
//! compiles; unsaved builds read them from `.biwrite`, and a save writes
//! them next to the document's other figures.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use biwrite_core::TextFile;
use biwrite_engine::{PartialFn, TokenUsage, TranslateError};
use biwrite_providers::{ChatModel, ChatOutput, ChatRequest};
use tauri::Manager;

use super::*;
use crate::latex_commands::{Lang, latex_compile};
use crate::pair_tests::{app_state, settle};

/// A paper that keeps its figures in `../figures` through `\figdir`.
const PAPER: &str = "\\documentclass{article}\n\
\\usepackage{graphicx}\n\
\\newcommand{\\figdir}{../figures/}\n\
\\begin{document}\n\
First paragraph of the paper.\n\n\
\\begin{figure}[t]\n\
\\centering\n\
\\includegraphics[width=\\columnwidth]{\\figdir fig_old.pdf}\n\
\\caption{An old figure.}\n\
\\end{figure}\n\n\
\\begin{figure}[t]\n\
\\centering\n\
\\includegraphics[width=\\columnwidth]{\\figdir fig_other.pdf}\n\
\\caption{Another figure.}\n\
\\end{figure}\n\n\
Last paragraph.\n\
\\end{document}\n";

struct Scripted {
    replies: Mutex<Vec<String>>,
    prompts: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(replies: &[&str]) -> Self {
        Self {
            replies: Mutex::new(replies.iter().rev().map(|r| (*r).to_owned()).collect()),
            prompts: Mutex::new(Vec::new()),
        }
    }
}

impl ChatModel for Scripted {
    fn label(&self) -> String {
        "scripted".into()
    }

    fn complete<'a>(
        &'a self,
        request: &'a ChatRequest,
        _on_partial: PartialFn<'a>,
    ) -> biwrite_engine::BoxFuture<'a, Result<ChatOutput, TranslateError>> {
        Box::pin(async move {
            self.prompts.lock().unwrap().push(request.user.clone());
            let text = self.replies.lock().unwrap().pop().expect("a reply");
            Ok(ChatOutput {
                text,
                usage: TokenUsage::default(),
            })
        })
    }

    fn max_retries(&self) -> Option<u32> {
        Some(0)
    }
}

/// A project `top/en/main.tex` with figures in `top/figures`.
fn project(name: &str) -> PathBuf {
    let top = std::env::temp_dir().join(format!("biwrite-figfile-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&top);
    std::fs::create_dir_all(top.join("en")).unwrap();
    std::fs::create_dir_all(top.join("figures")).unwrap();
    std::fs::canonicalize(&top).unwrap()
}

/// A small PDF made with TeX, for the paper's existing figures.
async fn tiny_pdf(dir: &Path, name: &str) {
    let tc = biwrite_latex::detect(biwrite_latex::find_bin(None).expect("TeX")).await;
    std::fs::write(
        dir.join("tiny.tex"),
        "\\documentclass[border=2pt]{standalone}\\begin{document}x\\end{document}",
    )
    .unwrap();
    let job = biwrite_latex::Job {
        dir: dir.to_path_buf(),
        root: "tiny.tex".into(),
        engine: biwrite_latex::Engine::Pdflatex,
        out_dir: None,
        timeout: std::time::Duration::from_secs(60),
    };
    let built = biwrite_latex::compile(&tc, &job).await.unwrap();
    std::fs::copy(built.pdf.unwrap(), dir.join(name)).unwrap();
    for ext in [
        "tex",
        "pdf",
        "aux",
        "log",
        "fls",
        "fdb_latexmk",
        "synctex.gz",
    ] {
        let _ = std::fs::remove_file(dir.join(format!("tiny.{ext}")));
    }
}

/// Make the figure of job `id` from `reply`, as the job would.
async fn make(
    state: &AppState,
    id: u64,
    format: FigureFormat,
    model: &dyn ChatModel,
    reply: &str,
) -> Result<AssistResult, String> {
    let text = state.engine.text();
    let p = crate::latex_commands::project(state).unwrap();
    let place = (
        crate::figures::place(&p.root_text, &text, Some(&p.dir)),
        Some(p.dir.clone()),
    );
    let packages = crate::latex_commands::document_packages(state, &text);
    make_figure(
        state,
        model,
        id,
        format,
        place,
        packages.as_ref(),
        reply,
        TokenUsage::default(),
    )
    .await
}

/// `text` with `figure` after its first paragraph.
fn with_figure(text: &str, figure: &str) -> String {
    let at =
        text.find("First paragraph of the paper.").unwrap() + "First paragraph of the paper.".len();
    format!("{}\n\n{figure}{}", &text[..at], &text[at..])
}

const SCENE_REPLY: &str = "<name>\npaired runs\n</name>\n<scene>\n{\"wide\": false, \"nodes\": [{\"id\": \"a\", \"x\": 0, \"y\": 0, \"w\": 2.6, \"h\": 0.8, \"text\": \"Paired run\", \"fill\": \"#DCE6F2\"}, {\"id\": \"b\", \"x\": 4, \"y\": 0, \"w\": 2.6, \"h\": 0.8, \"text\": \"故障运行\"}], \"edges\": [{\"from\": \"a\", \"to\": \"b\", \"label\": \"minus\"}]}\n</scene>\n<caption>\nA faulty run and its paired run.\n</caption>\n<translation>\n故障运行与配对运行。\n</translation>";

#[tokio::test]
#[ignore = "needs TeX and LibreOffice"]
async fn a_powerpoint_figure_builds_unsaved_and_is_saved_beside_the_others() {
    let top = project("pptx");
    tiny_pdf(&top.join("figures"), "fig_old.pdf").await;
    tiny_pdf(&top.join("figures"), "fig_other.pdf").await;
    let main = top.join("en/main.tex");
    std::fs::write(&main, PAPER).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&top.join("cfg")));
    let state = app.state::<AppState>();
    crate::pairing::open(
        &state,
        main.clone(),
        TextFile::decode(PAPER.as_bytes().to_vec()).unwrap(),
        None,
    );
    settle(&state).await;

    let model = Scripted::new(&[]);
    let result = make(&state, 41, FigureFormat::Pptx, &model, SCENE_REPLY)
        .await
        .unwrap();
    let figure = result.figure.clone().unwrap();
    assert!(figure.pdf, "LibreOffice made the PDF");
    assert_eq!(figure.file, "../figures/fig_paired-runs.pptx");
    let env = result.answer.revision.clone().unwrap();
    assert_eq!(
        env,
        "\\begin{figure}[t]\n\\centering\n\\includegraphics[width=\\columnwidth]{\\figdir fig_paired-runs.pdf}\n\\caption{A faulty run and its paired run.}\n\\label{fig:paired-runs}\n\\end{figure}"
    );
    assert_eq!(
        result.answer.translation.as_deref(),
        Some("故障运行与配对运行。")
    );
    assert!(result.missing_packages.is_empty(), "graphicx is loaded");
    assert!(
        !top.join("figures/fig_paired-runs.pdf").exists(),
        "nothing written yet"
    );

    // Inserted and not saved: the build reads the waiting PDF.
    let edited = with_figure(PAPER, &env);
    let view = latex_compile(
        state.clone(),
        Lang::En,
        edited.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(view.has_pdf, "{}", view.output);
    let errors: Vec<_> = view
        .issues
        .iter()
        .filter(|i| i.severity == biwrite_latex::Severity::Error)
        .map(|i| i.message.clone())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert!(
        top.join("en/.biwrite/figures/fig_paired-runs.pdf")
            .is_file()
    );
    assert!(
        !top.join("figures/fig_paired-runs.pdf").exists(),
        "still nothing written"
    );

    // Saved: the PDF and the PowerPoint file go beside the other figures.
    state.sync_text(state.engine.document(), &edited).unwrap();
    let saved = crate::commands::write_document(&state, main.clone(), edited, None)
        .await
        .unwrap();
    assert_eq!(
        saved.figures,
        [
            "../figures/fig_paired-runs.pdf",
            "../figures/fig_paired-runs.pptx"
        ]
    );
    assert!(saved.figures_kept.is_empty());
    let pdf = std::fs::read(top.join("figures/fig_paired-runs.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    let pptx = std::fs::read(top.join("figures/fig_paired-runs.pptx")).unwrap();
    assert!(pptx.starts_with(b"PK"));
    // A second figure of the same name takes the next one.
    let again = make(&state, 42, FigureFormat::Pptx, &model, SCENE_REPLY)
        .await
        .unwrap();
    assert_eq!(
        again.figure.unwrap().file,
        "../figures/fig_paired-runs-2.pptx"
    );
    std::fs::remove_dir_all(&top).unwrap();
}

#[tokio::test]
#[ignore = "needs TeX"]
async fn a_picture_that_does_not_compile_is_repaired_into_a_pdf() {
    let top = project("pdf");
    let main = top.join("en/main.tex");
    let plain = PAPER.replace("\\usepackage{graphicx}\n", "");
    std::fs::write(&main, &plain).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&top.join("cfg")));
    let state = app.state::<AppState>();
    crate::pairing::open(
        &state,
        main.clone(),
        TextFile::decode(plain.clone().into_bytes()).unwrap(),
        None,
    );
    settle(&state).await;

    let first = "<name>\nloss\n</name>\n<revision>\n\\begin{tikzpicture}\n\\draw[-{Stealth}] (0,0) -- (3,0) node[right] {epoch};\n\\end{tikzpicture}\n</revision>\n<preamble>\n</preamble>\n<caption>\nThe loss.\n</caption>";
    let repaired = "<revision>\n\\begin{tikzpicture}\n\\draw[-{Stealth}] (0,0) -- (3,0) node[right] {epoch};\n\\end{tikzpicture}\n</revision>\n<preamble>\n\\usetikzlibrary{arrows.meta}\n</preamble>\n<changes_zh>\n加载 arrows.meta。\n</changes_zh>";
    let model = Scripted::new(&[repaired]);
    let result = make(&state, 43, FigureFormat::Pdf, &model, first)
        .await
        .unwrap();
    let prompts = model.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 1, "one repair");
    assert!(
        prompts[0].contains("Unknown arrow tip kind"),
        "{}",
        prompts[0]
    );
    let figure = result.figure.unwrap();
    assert_eq!((figure.pdf, figure.pptx), (true, false));
    assert_eq!(figure.file, "../figures/fig_loss.pdf");
    assert!(
        result
            .answer
            .changes_zh
            .contains(&"加载 arrows.meta。".to_owned())
    );
    // The document does not load graphicx: the insert adds it.
    assert_eq!(result.missing_packages, ["graphicx"]);
    let made = state.figures.made().get(&43).cloned().unwrap();
    assert!(made.pdf.unwrap().starts_with(b"%PDF"));
    std::fs::remove_dir_all(&top).unwrap();
}
