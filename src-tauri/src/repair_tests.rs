//! Fixes and checks against real TeX builds, with a model that answers
//! from a script: the errors reach the model, its changes are made and
//! built again, and the project's files never change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use biwrite_core::TextFile;
use biwrite_engine::{PartialFn, TokenUsage, TranslateError};
use biwrite_providers::{ChatModel, ChatOutput, ChatRequest};
use tauri::Manager;

use super::*;
use crate::pair_tests::{app_state, settle};
use crate::pairing;

/// A class-like color defined with the color package before xcolor is
/// loaded, as some journal templates do: TikZ cannot use it.
const DOC: &str = "\\documentclass{article}\n\
\\usepackage{color}\n\
\\definecolor{subsectioncolor}{rgb}{0,0,0}\n\
\\usepackage{xcolor}\n\
\\usepackage{tikz}\n\
\\begin{document}\n\
Text before the figure.\n\n\
\\begin{tikzpicture}\n\
\\node[fill=subsectioncolor] (a) {A};\n\
\\end{tikzpicture}\n\n\
Text after it.\n\
\\end{document}\n";

const CLEAN: &str = "\\documentclass{article}\n\
\\usepackage{color}\n\
\\definecolor{subsectioncolor}{rgb}{0,0,0}\n\
\\usepackage{xcolor}\n\
\\begin{document}\n\
Text before the figure.\n\n\
Text after it.\n\
\\end{document}\n";

/// Answers from a script, one reply per request, and keeps the prompts.
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

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
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
            let text = self
                .replies
                .lock()
                .unwrap()
                .pop()
                .expect("a scripted reply for every request");
            Ok(ChatOutput {
                text,
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
            })
        })
    }

    fn max_retries(&self) -> Option<u32> {
        Some(0)
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-repair-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

/// Every file of `dir` outside `.biwrite`, with its bytes.
fn project_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_file() {
            out.insert(
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&path).unwrap(),
            );
        }
    }
    out
}

fn open(state: &Document, path: &Path) {
    let file = TextFile::decode(std::fs::read(path).unwrap()).unwrap();
    pairing::open(state, path.to_owned(), file, None);
}

fn quiet() -> impl Fn(bool, u32) + Send + Sync {
    |_, _| {}
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_fix_gives_the_model_the_errors_and_offers_what_compiles() {
    let dir = temp("fix");
    let path = dir.join("main.tex");
    std::fs::write(&path, DOC).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let before = project_files(&dir);

    let model = Scripted::new(&[
        "<edit>\n<find>\n 10| \\node[fill=subsectioncolor] (a) {A};\n</find>\n<replace>\n\\node[fill=black!20] (a) {A};\n</replace>\n</edit>\n<changes_zh>\n模板的颜色在 xcolor 之前定义，TikZ 用不了，改用 black!20。\n</changes_zh>\n<changes_en>\nThe template's color predates xcolor; black!20 instead.\n</changes_en>",
    ]);
    let stages = Mutex::new(Vec::new());
    let on_stage = |fixing: bool, round: u32| stages.lock().unwrap().push((fixing, round));
    let fixed = fix_loop(&state, &model, DOC, "", None, &on_stage)
        .await
        .unwrap();
    let prompts = model.prompts();
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].contains("line 10: Package xcolor Error: Undefined color model"),
        "{}",
        prompts[0]
    );
    assert!(prompts[0].contains("defined before xcolor was loaded"));
    assert!(prompts[0].contains(" 10| \\node[fill=subsectioncolor]"));
    assert!(fixed.report.before > 0);
    assert!(fixed.report.errors.is_empty(), "{:?}", fixed.report.errors);
    assert_eq!(fixed.report.rounds, 1);
    assert_eq!(
        fixed.text,
        DOC.replace("subsectioncolor] (a)", "black!20] (a)")
    );
    assert_eq!(
        stages.into_inner().unwrap(),
        vec![(false, 0), (true, 1), (false, 1)]
    );
    let hunks = biwrite_core::repair::hunks(DOC, &fixed.text);
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].line, 10);
    assert_eq!(project_files(&dir), before, "the project is as it was");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_document_that_compiles_needs_no_model() {
    let dir = temp("clean");
    let path = dir.join("main.tex");
    std::fs::write(&path, CLEAN).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let model = Scripted::new(&[]);
    let fixed = fix_loop(&state, &model, CLEAN, "", None, &quiet())
        .await
        .unwrap();
    assert_eq!(fixed.report.before, 0);
    assert_eq!(fixed.report.rounds, 0);
    assert_eq!(fixed.text, CLEAN);
    assert!(model.prompts().is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_bibtex_failure_is_no_error_to_repair() {
    let dir = temp("bibtex");
    let path = dir.join("main.tex");
    let doc = CLEAN.replace(
        "Text after it.",
        "Text after it~\\cite{nobody}.\n\\bibliographystyle{plain}\n\\bibliography{nowhere}",
    );
    std::fs::write(&path, &doc).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let model = Scripted::new(&[]);
    let fixed = fix_loop(&state, &model, &doc, "", None, &quiet())
        .await
        .unwrap();
    assert_eq!(fixed.report.before, 0, "{:?}", fixed.report.errors);
    assert!(model.prompts().is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The figure of `DOC`, as an answer for `CLEAN`.
const FIGURE: &str =
    "\\begin{tikzpicture}\n\\node[fill=subsectioncolor] (a) {A};\n\\end{tikzpicture}";

/// The answer `revision` inserted after the first paragraph of `text`, its
/// preamble lines before `\begin{document}`.
fn insertion(id: u64, text: &str, revision: &str, preamble: &str) -> CheckStart {
    let at = text.find("Text before the figure.").unwrap() + "Text before the figure.".len();
    let begin = text.find("\\begin{document}").unwrap();
    CheckStart {
        id,
        text: text.to_owned(),
        from: at,
        to: at,
        lead: "\n\n".into(),
        trail: String::new(),
        revision: revision.into(),
        preamble_at: Some(begin),
        preamble: preamble.into(),
        root_lines: String::new(),
        fit: false,
    }
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn smart_apply_repairs_an_answer_that_brings_errors() {
    let dir = temp("check");
    let path = dir.join("main.tex");
    std::fs::write(&path, CLEAN).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let before = project_files(&dir);

    let model = Scripted::new(&[
        "<revision>\n\\begin{tikzpicture}\n\\node[fill=figA] (a) {A};\n\\end{tikzpicture}\n</revision>\n<preamble>\n\\usepackage{tikz}\n\\definecolor{figA}{RGB}{31,78,121}\n</preamble>\n<changes_zh>\n改用自定义颜色 figA。\n</changes_zh>\n<changes_en>\nOwn color figA.\n</changes_en>",
    ]);
    let start = insertion(7, CLEAN, FIGURE, "\\usepackage{tikz}\n");
    let result = check_loop(&state, &model, &start, &quiet()).await.unwrap();
    assert!(result.passed, "{:?}", result.errors);
    assert_eq!(result.rounds, 1);
    assert_eq!(result.before, 0);
    assert!(result.revision.contains("fill=figA"));
    assert_eq!(
        result.preamble,
        "\\usepackage{tikz}\n\\definecolor{figA}{RGB}{31,78,121}\n"
    );
    assert_eq!(result.changes_en, vec!["Own color figA."]);
    let prompt = &model.prompts()[0];
    assert!(prompt.contains("<inserted>"), "{prompt}");
    assert!(prompt.contains("\\node[fill=subsectioncolor]"));
    assert!(prompt.contains("were added for it"));
    assert_eq!(project_files(&dir), before, "the project is as it was");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn an_answer_that_compiles_is_applied_as_it_is() {
    let dir = temp("passes");
    let path = dir.join("main.tex");
    std::fs::write(&path, CLEAN).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let model = Scripted::new(&[]);
    let figure = FIGURE.replace("subsectioncolor", "blue!20");
    let start = insertion(8, CLEAN, &figure, "\\usepackage{tikz}\n");
    let result = check_loop(&state, &model, &start, &quiet()).await.unwrap();
    assert!(result.passed, "{:?}", result.errors);
    assert_eq!(result.rounds, 0);
    assert_eq!(result.revision, figure);
    assert!(model.prompts().is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn errors_the_document_had_are_not_the_answers() {
    let dir = temp("had");
    let path = dir.join("main.tex");
    let broken = CLEAN.replace("Text after it.", "Text after it \\undefinedmacro.");
    std::fs::write(&path, &broken).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let model = Scripted::new(&[]);
    let start = insertion(9, &broken, "A new paragraph of plain text.", "");
    let result = check_loop(&state, &model, &start, &quiet()).await.unwrap();
    assert!(result.passed, "{:?}", result.errors);
    assert_eq!(result.before, 1);
    assert!(model.prompts().is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_answer_is_composed_where_the_editor_would_put_it() {
    let start = insertion(1, CLEAN, "NEW", "\\usepackage{tikz}\n");
    let (text, answer, lines) = compose(&start, "NEW", "\\usepackage{tikz}\n");
    assert_eq!(&text[answer.clone()], "NEW");
    assert!(text.contains("Text before the figure.\n\nNEW\n\nText after it."));
    let lines = lines.unwrap();
    assert_eq!(&text[lines], "\\usepackage{tikz}\n");
    assert!(text.contains("\\usepackage{tikz}\n\\begin{document}"));
    // UTF-16 offsets past Chinese text.
    let zh = CLEAN.replace("Text before the figure.", "图前的文字。");
    let at16 = zh[..zh.find("图前的文字。").unwrap() + "图前的文字。".len()]
        .encode_utf16()
        .count();
    let start = CheckStart {
        id: 1,
        text: zh.clone(),
        from: at16,
        to: at16,
        lead: "\n\n".into(),
        revision: "X".into(),
        ..CheckStart::default()
    };
    let (text, answer, lines) = compose(&start, "X", "");
    assert!(text.contains("图前的文字。\n\nX\n\nText after it."));
    assert_eq!(&text[answer], "X");
    assert!(lines.is_none());
}

#[test]
fn only_errors_the_answer_brings_count() {
    let e = |line: Option<usize>, message: &str| TexError {
        line,
        message: message.into(),
        ..TexError::default()
    };
    let text = "a\nb\nANSWER\nc\n";
    let answer = 4..10;
    let before = [e(Some(4), "Undefined control sequence."), e(None, "Other.")];
    let after = [
        e(Some(5), "Undefined control sequence."),
        e(None, "Other."),
        e(Some(3), "Undefined control sequence."),
        e(None, "New one."),
    ];
    let new = new_errors(&after, &before, text, &answer, None);
    // The one in the answer, and the message the document never had.
    assert_eq!(
        new.iter()
            .map(|e| (e.line, e.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(Some(3), "Undefined control sequence."), (None, "New one.")]
    );
}

/// On a real paper: its edits build unsaved from a copy, an answer that
/// uses a color of its template is repaired by Smart apply, and a fix
/// repairs the same figure put in as it is. `BIWRITE_REAL_TEX` names the
/// main file of a copy (the test builds in its folder).
#[tokio::test]
#[ignore = "needs BIWRITE_REAL_TEX and a TeX distribution"]
async fn a_real_paper_builds_unsaved_and_repairs_a_figure() {
    let main = PathBuf::from(std::env::var("BIWRITE_REAL_TEX").unwrap());
    let dir = main.parent().unwrap().to_path_buf();
    let app = tauri::test::mock_app();
    app.manage(app_state(&temp("real-cfg")));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &main);
    settle(&state).await;
    let saved = std::fs::read_to_string(&main).unwrap();

    // As saved, in place.
    let view = crate::latex_commands::latex_compile(
        state.clone(),
        crate::latex_commands::Lang::En,
        saved.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(view.has_pdf, "{}", view.output);
    let before = project_files(&dir);

    // Unsaved: from a copy, figures and the bibliography found.
    let paragraph = saved
        .find("These observations point to three difficulties")
        .expect("a paragraph to put a figure after");
    let end = paragraph + saved[paragraph..].find('\n').unwrap();
    let edited = format!("{} Edited.{}", &saved[..end], &saved[end..]);
    let started = Instant::now();
    let view = crate::latex_commands::latex_compile(
        state.clone(),
        crate::latex_commands::Lang::En,
        edited,
        state.engine.document(),
    )
    .await
    .unwrap();
    eprintln!(
        "unsaved build: {:?} in {:?}",
        view.outcome,
        started.elapsed()
    );
    assert!(view.has_pdf, "{}", view.output);
    assert!(!view.unsaved_left_out);
    let errors: Vec<_> = view
        .issues
        .iter()
        .filter(|i| i.severity == biwrite_latex::Severity::Error)
        .map(|i| i.message.clone())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(project_files(&dir), before, "the paper is as it was");

    // Smart apply of a figure in the template's colors.
    let figure = "\\begin{figure}[t]\n\\centering\n\\begin{tikzpicture}\n\\node[draw, fill=subsectioncolor!20] (a) {Paired run};\n\\node[draw, right=2cm of a] (b) {Faulty run};\n\\draw[->] (a) -- (b);\n\\end{tikzpicture}\n\\caption{A faulty run and its paired run.}\n\\label{fig:test-paired}\n\\end{figure}";
    let at16 = saved[..end].encode_utf16().count();
    let begin = saved.find("\\begin{document}").unwrap();
    let start = CheckStart {
        id: 1,
        text: saved.clone(),
        from: at16,
        to: at16,
        lead: "\n\n".into(),
        trail: String::new(),
        revision: figure.into(),
        preamble_at: Some(saved[..begin].encode_utf16().count()),
        preamble: String::new(),
        root_lines: String::new(),
        fit: false,
    };
    let model = Scripted::new(&[
        "<revision>\n\\begin{figure}[t]\n\\centering\n\\begin{tikzpicture}\n\\node[draw, fill=figPaired!20] (a) {Paired run};\n\\node[draw, right=2cm of a] (b) {Faulty run};\n\\draw[->] (a) -- (b);\n\\end{tikzpicture}\n\\caption{A faulty run and its paired run.}\n\\label{fig:test-paired}\n\\end{figure}\n</revision>\n<preamble>\n\\usetikzlibrary{positioning}\n\\definecolor{figPaired}{RGB}{31,78,121}\n</preamble>\n<changes_zh>\n改用自定义颜色。\n</changes_zh>",
    ]);
    let started = Instant::now();
    let result = check_loop(&state, &model, &start, &quiet()).await.unwrap();
    eprintln!(
        "smart apply: {} rounds in {:?}",
        result.rounds,
        started.elapsed()
    );
    let prompt = &model.prompts()[0];
    assert!(prompt.contains("Undefined color model"), "{prompt}");
    assert!(result.passed, "{:?}", result.errors);
    assert_eq!(result.rounds, 1);

    // A fix of the same figure put in as it is.
    let with = format!("{}\n\n{figure}{}", &saved[..end], &saved[end..]);
    let model = Scripted::new(&[
        "<edit>\n<find>\n\\node[draw, fill=subsectioncolor!20] (a) {Paired run};\n</find>\n<replace>\n\\node[draw, fill=vioTableBand] (a) {Paired run};\n</replace>\n</edit>\n<edit>\n<find>\n\\usepackage{tikz}\n</find>\n<replace>\n\\usepackage{tikz}\n\\usetikzlibrary{positioning}\n</replace>\n</edit>",
    ]);
    let started = Instant::now();
    let fixed = fix_loop(&state, &model, &with, "", None, &quiet())
        .await
        .unwrap();
    eprintln!(
        "fix: {} errors, {} left after {} rounds in {:?}",
        fixed.report.before,
        fixed.report.errors.len(),
        fixed.report.rounds,
        started.elapsed()
    );
    assert!(fixed.report.before > 0);
    assert!(fixed.report.errors.is_empty(), "{:?}", fixed.report.errors);
    assert_eq!(project_files(&dir), before, "the paper is as it was");
}

/// A model that cannot be reached.
struct Down;

impl ChatModel for Down {
    fn label(&self) -> String {
        "down".into()
    }

    fn complete<'a>(
        &'a self,
        _request: &'a ChatRequest,
        _on_partial: PartialFn<'a>,
    ) -> biwrite_engine::BoxFuture<'a, Result<ChatOutput, TranslateError>> {
        Box::pin(async move {
            Err(TranslateError::Server {
                status: 503,
                retry_after: None,
                message: String::new(),
            })
        })
    }

    fn max_retries(&self) -> Option<u32> {
        Some(0)
    }
}

#[tokio::test]
async fn accepting_with_the_model_down_is_an_error_before_any_build() {
    // The frontend then applies the answer as it is.
    let app = tauri::test::mock_app();
    app.manage(app_state(&temp("fit-down")));
    let state = app.state::<Doc>().inner().clone();
    let start = CheckStart {
        fit: true,
        ..insertion(9, CLEAN, FIGURE, "\\usepackage{tikz}\n")
    };
    assert!(check_loop(&state, &Down, &start, &quiet()).await.is_err());
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn accepting_fits_the_answer_with_the_model_and_then_builds_it() {
    let dir = temp("fit");
    let path = dir.join("main.tex");
    std::fs::write(&path, CLEAN).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let before = project_files(&dir);

    // The model fits it, and it compiles: no repair round.
    let model = Scripted::new(&[
        "<revision>\n\\begin{tikzpicture}\n\\node[fill=figA] (a) {A};\n\\end{tikzpicture}\n</revision>\n<preamble>\n\\usepackage{tikz}\n\\definecolor{figA}{RGB}{31,78,121}\n</preamble>\n<changes_zh>\n改用自定义颜色 figA。\n</changes_zh>\n<changes_en>\nOwn color figA.\n</changes_en>",
    ]);
    let start = CheckStart {
        fit: true,
        ..insertion(7, CLEAN, FIGURE, "\\usepackage{tikz}\n")
    };
    let result = check_loop(&state, &model, &start, &quiet()).await.unwrap();
    assert!(result.passed, "{:?}", result.errors);
    assert!(result.fitted);
    assert_eq!(result.rounds, 0);
    assert!(result.revision.contains("fill=figA"));
    assert_eq!(result.changes_en, vec!["Own color figA."]);
    let prompts = model.prompts();
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].contains("Before it is applied, fit it to this document"),
        "{}",
        prompts[0]
    );
    assert!(!prompts[0].contains("<errors>"));

    // The model leaves it as it is: built, it fails, and the model repairs it.
    let model = Scripted::new(&[
        &format!(
            "<revision>\n{FIGURE}\n</revision>\n<preamble>\n\\usepackage{{tikz}}\n</preamble>"
        ),
        "<revision>\n\\begin{tikzpicture}\n\\node[fill=figB] (a) {A};\n\\end{tikzpicture}\n</revision>\n<preamble>\n\\usepackage{tikz}\n\\definecolor{figB}{RGB}{31,78,121}\n</preamble>\n<changes_zh>\n改用自定义颜色 figB。\n</changes_zh>",
    ]);
    let result = check_loop(&state, &model, &start, &quiet()).await.unwrap();
    assert!(result.passed, "{:?}", result.errors);
    assert!(!result.fitted, "unchanged by the fit");
    assert_eq!(result.rounds, 1);
    assert!(result.revision.contains("fill=figB"));
    assert!(model.prompts()[1].contains("<errors>"));
    assert_eq!(project_files(&dir), before, "the project is as it was");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A project of a main file and two sections, each with an error.
fn sectioned_with_errors(name: &str) -> PathBuf {
    let dir = temp(name);
    std::fs::create_dir_all(dir.join("sections")).unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\n\\input{sections/intro}\n\\include{sections/method}\n\\end{document}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("sections/intro.tex"),
        "\\section{Introduction}\n\\nosuchmacro We clean tables.\n",
    )
    .unwrap();
    std::fs::write(dir.join("sections/method.tex"), METHOD_WITH_ERROR).unwrap();
    dir
}

const METHOD_WITH_ERROR: &str =
    "\\section{Method}\n\\alsonosuchmacro Our method assigns one action to each row.\n";

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_fix_of_a_section_mends_its_own_errors_and_names_the_others() {
    let dir = sectioned_with_errors("sections");
    let path = dir.join("sections/method.tex");
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let model = Scripted::new(&[
        "<edit>\n<find>\n2| \\alsonosuchmacro Our method assigns one action to each row.\n</find>\n<replace>\nOur method assigns one action to each row.\n</replace>\n</edit>\n<changes_zh>\n删去未定义的命令。\n</changes_zh>\n<changes_en>\nThe undefined command goes.\n</changes_en>",
    ]);
    let fixed = fix_loop(&state, &model, METHOD_WITH_ERROR, "", None, &quiet())
        .await
        .unwrap();
    let prompts = model.prompts();
    assert_eq!(prompts.len(), 1, "one round: the section's error is mended");
    assert!(prompts[0].contains("alsonosuchmacro"), "{}", prompts[0]);
    assert!(
        !prompts[0].contains("nosuchmacro We clean"),
        "{}",
        prompts[0]
    );
    assert_eq!(fixed.report.before, 1, "{:?}", fixed.report);
    assert!(fixed.report.errors.is_empty(), "{:?}", fixed.report.errors);
    assert_eq!(
        fixed.report.elsewhere.len(),
        1,
        "{:?}",
        fixed.report.elsewhere
    );
    let other = &fixed.report.elsewhere[0];
    assert_eq!(other.file.as_deref(), Some("sections/intro.tex"));
    assert_eq!(other.file_line, Some(2));
    assert_eq!(other.line, None);

    // A section without errors of its own asks no model, and says where
    // the others are.
    let model = Scripted::new(&[]);
    let clean = METHOD_WITH_ERROR.replace("\\alsonosuchmacro ", "");
    let fixed = fix_loop(&state, &model, &clean, "", None, &quiet())
        .await
        .unwrap();
    assert!(model.prompts().is_empty());
    assert_eq!(fixed.report.before, 0);
    assert_eq!(fixed.report.elsewhere.len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn a_figure_of_a_section_is_checked_with_the_lines_the_main_file_needs() {
    let dir = temp("section-figure");
    std::fs::create_dir_all(dir.join("sections")).unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\n\\input{sections/method}\n\\end{document}\n",
    )
    .unwrap();
    let method = "\\section{Method}\nOur method assigns one action to each row.\n";
    let path = dir.join("sections/method.tex");
    std::fs::write(&path, method).unwrap();
    let app = tauri::test::mock_app();
    app.manage(app_state(&dir));
    let state = app.state::<Doc>().inner().clone();
    open(&state, &path);
    settle(&state).await;
    let at = method.encode_utf16().count();
    let start = CheckStart {
        id: 1,
        text: method.to_owned(),
        from: at,
        to: at,
        lead: "\n".into(),
        trail: String::new(),
        revision: "\\begin{tikzpicture}\n\\node[draw] {A};\n\\end{tikzpicture}\n".into(),
        preamble_at: None,
        preamble: String::new(),
        root_lines: "\\usepackage{tikz}\n".into(),
        fit: false,
    };
    // No model: the copy of the main file loads TikZ, so it compiles.
    let model = Scripted::new(&[]);
    let result = check_loop(&state, &model, &start, &quiet()).await.unwrap();
    assert!(result.passed, "{:?}", result.errors);
    assert_eq!(result.rounds, 0);
    assert!(model.prompts().is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.join("main.tex")).unwrap(),
        "\\documentclass{article}\n\\begin{document}\n\\input{sections/method}\n\\end{document}\n",
        "the main file is as it was"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
