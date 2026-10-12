use std::path::PathBuf;
use std::sync::Mutex;

use biwrite_core::TextFile;
use biwrite_engine::{PartialFn, TokenUsage, TranslateError};
use biwrite_providers::ChatOutput;
use tauri::Manager;

use super::*;
use crate::pair_tests::{app_state, settle};

const HEAD: &str = "\\documentclass{article}\n\\usepackage{graphicx}\n";

/// A model with one reply ready, or none: then every call fails, as when
/// the provider cannot be reached.
struct Scripted {
    reply: Option<String>,
    prompts: Mutex<Vec<String>>,
}

impl Scripted {
    fn says(reply: &str) -> Self {
        Self {
            reply: Some(reply.to_owned()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn down() -> Self {
        Self {
            reply: None,
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
            match &self.reply {
                Some(text) => Ok(ChatOutput {
                    text: text.clone(),
                    usage: TokenUsage::default(),
                }),
                None => Err(TranslateError::Server {
                    status: 503,
                    retry_after: None,
                    message: String::new(),
                }),
            }
        })
    }

    fn max_retries(&self) -> Option<u32> {
        Some(0)
    }
}

/// A project `top/main.tex` holding `text`, opened.
async fn opened(name: &str, text: &str) -> (tauri::App<tauri::test::MockRuntime>, PathBuf) {
    let top = std::env::temp_dir().join(format!("biwrite-tidy-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&top);
    std::fs::create_dir_all(&top).unwrap();
    let top = std::fs::canonicalize(&top).unwrap();
    let main = top.join("main.tex");
    std::fs::write(&main, text).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&top.join("cfg")));
    let state = app.state::<AppState>();
    crate::pairing::open(
        &state,
        main,
        TextFile::decode(text.as_bytes().to_vec()).unwrap(),
        None,
    );
    settle(&state).await;
    (app, top)
}

fn err(message: &str) -> TexError {
    TexError {
        line: None,
        file: None,
        message: message.into(),
        context: String::new(),
    }
}

#[test]
fn only_errors_beyond_those_the_document_had_count() {
    let before = [
        err("Undefined control sequence."),
        err("Missing $ inserted."),
    ];
    let after = [
        err("Undefined control sequence."),
        err("Undefined control sequence."),
        err("Missing $ inserted."),
    ];
    assert_eq!(new_errors(&after, &before), 1);
    assert_eq!(new_errors(&before, &before), 0);
}

#[test]
fn the_model_reads_the_document_and_the_numbered_lines() {
    let lines = [
        "\\usepackage{pifont}".to_owned(),
        "\\tikzset{\n a/.style={}\n}".to_owned(),
    ];
    let (system, user) = prompt("DOC", &lines);
    assert!(system.contains("Keep a line when anything else needs it or when you are not sure."));
    assert!(system.contains("A line needed only by other lines of the list is not needed"));
    assert!(user.contains("<document>\nDOC\n</document>"));
    assert!(user.contains("1. \\usepackage{pifont}\n2. \\tikzset{  a/.style={} }"));
}

#[test]
fn a_verdict_names_lines_by_number() {
    assert_eq!(parse("1 keep\n2 remove\n3 remove", 3), Some(vec![2, 3]));
    assert_eq!(parse("- 1: Remove\n* 2. keep", 2), Some(vec![1]));
    assert_eq!(parse("1 keep\n2 keep", 2), Some(vec![]));
    // Numbers out of range are not lines, and a reply naming none is no
    // verdict.
    assert_eq!(parse("7 remove\n1 keep", 2), Some(vec![]));
    assert_eq!(parse("I cannot tell.", 2), None);
}

/// A state with no project: nothing can be built.
fn no_project() -> tauri::App<tauri::test::MockRuntime> {
    let app = tauri::test::mock_app();
    app.manage(app_state(
        &std::env::temp_dir().join(format!("biwrite-tidy-none-{}", std::process::id())),
    ));
    app
}

#[tokio::test]
async fn the_model_decides_and_a_model_that_cannot_answer_keeps_every_line() {
    let app = no_project();
    let state = app.state::<AppState>();
    let added = "\\usepackage{tikz}\n\\usetikzlibrary{arrows.meta}\n\\definecolor{figBlue}{RGB}{31,78,121}\n";
    let text = format!("{HEAD}{added}\\begin{{document}}\nText.\n\\end{{document}}\n");
    // No build can be done here: the model's choice stands.
    let model = Scripted::says("1 remove\n2 remove\n3 keep");
    let t = tidy(&state, &model, &text, added).await.unwrap();
    assert_eq!(
        t.remove,
        ["\\usepackage{tikz}", "\\usetikzlibrary{arrows.meta}"]
    );
    assert_eq!(t.kept, ["\\definecolor{figBlue}{RGB}{31,78,121}"]);
    let asked = model.prompts.lock().unwrap()[0].clone();
    assert!(
        asked.contains("1. \\usepackage{tikz}\n2. \\usetikzlibrary{arrows.meta}\n3. \\definecolor"),
        "{asked}"
    );
    // The model down, or a reply naming no line: the caller keeps them all.
    assert!(tidy(&state, &Scripted::down(), &text, added).await.is_err());
    assert!(
        tidy(&state, &Scripted::says("Sure."), &text, added)
            .await
            .is_err()
    );
    // A line edited since is not asked about and stays.
    let edited = text.replace("{arrows.meta}", "{arrows.meta,positioning}");
    let model = Scripted::says("1 remove\n2 remove");
    let t = tidy(&state, &model, &edited, added).await.unwrap();
    assert_eq!(t.kept, ["\\usetikzlibrary{arrows.meta}"]);
    assert_eq!(t.remove.len(), 2);
}

/// A paper whose other paragraph uses pifont's \ding, with an unused colour
/// the figure added.
fn ding_paper() -> String {
    format!(
        "{HEAD}\\usepackage{{xcolor}}\n\\usepackage{{pifont}}\n\\definecolor{{figRed}}{{RGB}}{{192,0,0}}\n\\begin{{document}}\nA tick \\ding{{51}}.\n\\end{{document}}\n"
    )
}

const PIFONT: &str = "\\usepackage{pifont}\n\\definecolor{figRed}{RGB}{192,0,0}\n";

#[tokio::test]
#[ignore = "needs TeX"]
async fn what_the_model_picks_goes_once_a_build_without_it_passes() {
    let text = ding_paper();
    let (app, top) = opened("model", &text).await;
    let state = app.state::<AppState>();
    let t = tidy(&state, &Scripted::says("1 keep\n2 remove"), &text, PIFONT)
        .await
        .unwrap();
    assert_eq!(t.remove, ["\\definecolor{figRed}{RGB}{192,0,0}"]);
    assert_eq!(t.kept, ["\\usepackage{pifont}"]);
    assert_eq!(
        std::fs::read_to_string(top.join("main.tex")).unwrap(),
        text,
        "nothing saved"
    );
    std::fs::remove_dir_all(&top).unwrap();
}

#[tokio::test]
#[ignore = "needs TeX"]
async fn a_choice_that_breaks_the_build_keeps_every_line() {
    let text = ding_paper();
    let (app, top) = opened("wrong", &text).await;
    let state = app.state::<AppState>();
    // Wrong: \ding needs pifont.
    let t = tidy(&state, &Scripted::says("1 remove\n2 remove"), &text, PIFONT)
        .await
        .unwrap();
    assert!(t.remove.is_empty(), "{t:?}");
    assert_eq!(
        t.kept,
        [
            "\\usepackage{pifont}",
            "\\definecolor{figRed}{RGB}{192,0,0}"
        ]
    );
    // Without \ding both go.
    let plain = text.replace("A tick \\ding{51}.", "No tick.");
    let t = tidy(
        &state,
        &Scripted::says("1 remove\n2 remove"),
        &plain,
        PIFONT,
    )
    .await
    .unwrap();
    assert_eq!(t.remove.len(), 2);
    std::fs::remove_dir_all(&top).unwrap();
}
