//! Making LaTeX compile with the model's help.
//!
//! A fix job builds the open document as it is edited (in `.biwrite/check`,
//! apart from the PDFs shown), gives the model TeX's errors, makes the
//! changes it answers with and builds again, a few rounds at most. Its card
//! then offers the changes as hunks to accept.
//!
//! A check runs when an answer is accepted with the model's help: the model
//! first fits the answer to the document (when asked to), the document is
//! built with the answer put in, and when that brings errors the document
//! did not have, the model corrects the answer and the lines it adds before
//! `\begin{document}`, a few rounds at most. The editor then applies what
//! passed. The project's files are never written.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use biwrite_core::Mode;
use biwrite_core::assist::{self, Action, Answer};
use biwrite_core::repair::{self, TexError};
use biwrite_core::utf16::utf16_to_byte;
use biwrite_engine::{TokenUsage, TranslateError};
use biwrite_providers::{ChatModel, ChatOutput, ChatRequest};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::assist_commands::{
    AssistEvent, AssistResult, AssistStarted, AssistTarget, PARTIAL_EVERY, complete_with_retries,
    emit, retry_wait,
};
use crate::error::{CommandError, CommandResult};
use crate::latex_commands::{check_build, check_build_with};
use crate::state::{Doc, Document};

/// Most rounds of repairs by the model.
const MAX_ROUNDS: u32 = 3;
/// Where a fix or a check builds the document with its changes.
const CHECK_FOLDER: &str = biwrite_latex::CHECK_DIR;
/// Where a check builds the document as it was, to tell the errors it had.
const BEFORE_FOLDER: &str = ".biwrite/check-before";

/// How a fix went.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixReport {
    /// Errors of the document before the fix.
    pub before: usize,
    /// Errors of the document left in the last build.
    pub errors: Vec<TexError>,
    /// Errors of the last build in other files of the project, which a fix
    /// of those files mends.
    pub elsewhere: Vec<TexError>,
    /// Rounds of repairs the model made.
    pub rounds: u32,
    /// Changes the model gave whose lines were not found.
    pub missed: usize,
}

/// An answer checked for applying.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    /// The answer to apply, corrected when the model repaired it.
    pub revision: String,
    /// The lines to add before `\begin{document}` for it (empty for none).
    pub preamble: String,
    /// The document compiles without errors it did not have before.
    pub passed: bool,
    /// Errors the answer brings that are left.
    pub errors: Vec<TexError>,
    /// Errors the document had before, which are not the answer's.
    pub before: usize,
    /// Rounds of repairs the model made.
    pub rounds: u32,
    /// The model fitted the answer to the document before it was built,
    /// and changed it.
    pub fitted: bool,
    pub changes_zh: Vec<String>,
    pub changes_en: Vec<String>,
    pub usage: TokenUsage,
}

/// Change notes gathered over rounds, each once.
#[derive(Default)]
struct Notes {
    zh: Vec<String>,
    en: Vec<String>,
}

impl Notes {
    fn add(&mut self, zh: Vec<String>, en: Vec<String>) {
        for l in zh {
            if !self.zh.contains(&l) {
                self.zh.push(l);
            }
        }
        for l in en {
            if !self.en.contains(&l) {
                self.en.push(l);
            }
        }
    }
}

fn add_usage(total: &mut TokenUsage, more: TokenUsage) {
    total.input_tokens += more.input_tokens;
    total.output_tokens += more.output_tokens;
}

/// What a repair loop reports as it goes: building (`false`) or asking
/// the model (`true`), and the round.
type OnStage<'a> = &'a (dyn Fn(bool, u32) + Send + Sync);

/// The model's answer to `chat`, retried after passing failures. With an
/// app, its text as it arrives and the retries go to the card of job `id`.
pub(crate) async fn ask(
    stream: Option<(&AppHandle, &str)>,
    id: u64,
    model: &dyn ChatModel,
    chat: &ChatRequest,
) -> Result<ChatOutput, TranslateError> {
    let last = Mutex::new(None::<Instant>);
    let on_partial = |text: &str| {
        let Some((app, label)) = stream else { return };
        let due = {
            let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            let due = last.is_none_or(|t| now.duration_since(t) >= PARTIAL_EVERY);
            if due {
                *last = Some(now);
            }
            due
        };
        if due {
            emit(
                app,
                label,
                &AssistEvent::Partial {
                    id,
                    text: assist::streaming_preview(text),
                },
            );
        }
    };
    let on_retry = |attempt: u32, of: u32, err: &TranslateError| {
        log::info!("assistant {}: {err}; retry {attempt} of {of}", chat.purpose);
        if let Some((app, label)) = stream {
            emit(
                app,
                label,
                &AssistEvent::Retrying {
                    id,
                    attempt,
                    of,
                    message: err.to_string(),
                },
            );
        }
    };
    complete_with_retries(model, chat, &on_partial, &on_retry, retry_wait).await
}

/// What a fix made of the document.
pub(crate) struct Fixed {
    pub(crate) text: String,
    pub(crate) report: FixReport,
    notes: Notes,
    usage: TokenUsage,
}

/// Build `start`, and while TeX reports errors, have the model repair them
/// and build again, at most [`MAX_ROUNDS`] times.
pub(crate) async fn fix_loop(
    state: &Document,
    model: &dyn ChatModel,
    start: &str,
    instruction: &str,
    stream: Option<(&AppHandle, u64)>,
    on_stage: OnStage<'_>,
) -> CommandResult<Fixed> {
    let packages = crate::latex_commands::document_packages(state, start)
        .map(|(p, _)| p.into_iter().collect::<Vec<_>>());
    on_stage(false, 0);
    // Only the open document's errors go to the model: those of the
    // project's other files are not in the text it changes.
    let (mut errors, mut elsewhere) = check_build(state, start, CHECK_FOLDER).await?.split();
    let before = errors.len();
    let mut text = start.to_owned();
    let mut notes = Notes::default();
    let mut usage = TokenUsage::default();
    let mut rounds = 0;
    let mut missed = 0;
    while !errors.is_empty() && rounds < MAX_ROUNDS {
        rounds += 1;
        on_stage(true, rounds);
        let (system, user) = repair::fix_prompt(&repair::FixRequest {
            text: &text,
            errors: &errors,
            instruction,
            packages: packages.as_deref(),
        });
        let chat = ChatRequest {
            purpose: Action::Fix.purpose().to_owned(),
            system,
            user,
            images: Vec::new(),
        };
        let out = ask(
            stream.map(|(app, _)| (app, state.label.as_str())),
            stream.map_or(0, |s| s.1),
            model,
            &chat,
        )
        .await
        .map_err(|e| CommandError::Settings(e.to_string()))?;
        add_usage(&mut usage, out.usage);
        notes.add(
            assist::note_lines(&out.text, "changes_zh"),
            assist::note_lines(&out.text, "changes_en"),
        );
        let replacements = repair::parse_replacements(&out.text);
        let near: Vec<usize> = errors.iter().filter_map(|e| e.line).collect();
        let (next, miss) = repair::apply_replacements(&text, &replacements, &near);
        missed += miss.len();
        if next == text {
            break;
        }
        text = next;
        on_stage(false, rounds);
        (errors, elsewhere) = check_build(state, &text, CHECK_FOLDER).await?.split();
    }
    Ok(Fixed {
        text,
        report: FixReport {
            before,
            errors,
            elsewhere,
            rounds,
            missed,
        },
        notes,
        usage,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixStart {
    /// The editor's text when the fix starts.
    pub text: String,
    #[serde(default)]
    pub instruction: String,
}

/// Start a fix of the open document as it is edited.
#[tauri::command]
pub async fn assist_fix(
    app: AppHandle,
    state: Doc,
    request: FixStart,
) -> CommandResult<AssistStarted> {
    if state.engine.mode() != Mode::Latex {
        return Err(CommandError::Settings(
            "only a LaTeX document can be compiled".into(),
        ));
    }
    // A saved .tex file: its project is where the check builds.
    crate::latex_commands::project(&state)?;
    let model: Arc<dyn ChatModel> = state.assistant_model()?;
    let id = state.assist.next_job.fetch_add(1, Ordering::Relaxed) + 1;
    let started = AssistStarted {
        id,
        target: AssistTarget {
            from: 0,
            to: 0,
            text: String::new(),
            whole_paragraph: false,
            insert: false,
        },
        model: model.label(),
        skill: String::new(),
    };
    let handle = tauri::async_runtime::spawn(run_fix(app, state.clone(), id, request, model));
    state
        .assist
        .jobs()
        .insert(id, handle.inner().abort_handle());
    Ok(started)
}

async fn run_fix(
    app: AppHandle,
    state: Doc,
    id: u64,
    request: FixStart,
    model: Arc<dyn ChatModel>,
) {
    let started = Instant::now();
    let event = {
        let on_stage = |fixing: bool, round: u32| {
            emit(
                &app,
                &state.label,
                &AssistEvent::Stage { id, fixing, round },
            );
        };
        let fixed = fix_loop(
            &state,
            model.as_ref(),
            &request.text,
            &request.instruction,
            Some((&app, id)),
            &on_stage,
        )
        .await;
        state.assist.jobs().remove(&id);
        match fixed {
            Err(e) => AssistEvent::Failed {
                id,
                message: e.to_string(),
            },
            Ok(fixed) => AssistEvent::Done {
                id,
                result: Box::new(AssistResult {
                    answer: Answer {
                        changes_zh: fixed.notes.zh,
                        changes_en: fixed.notes.en,
                        ..Answer::default()
                    },
                    diff: Vec::new(),
                    usage: fixed.usage,
                    duration_ms: started.elapsed().as_millis() as u64,
                    missing_packages: Vec::new(),
                    libraries: Vec::new(),
                    preamble_here: false,
                    hunks: repair::hunks(&request.text, &fixed.text),
                    fix: Some(fixed.report),
                    figure: None,
                }),
            },
        }
    };
    if let AssistEvent::Failed { message, .. } = &event {
        log::warn!("the fix failed: {message}");
    }
    emit(&app, &state.label, &event);
}

/// An answer about to be applied, as the editor would put it in.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckStart {
    /// The job whose answer it is.
    pub id: u64,
    /// The document without the answer (UTF-16 offsets below).
    pub text: String,
    /// What the answer replaces (an empty range where it is inserted).
    pub from: usize,
    pub to: usize,
    /// What goes around the answer there, such as the blank lines that make
    /// it a paragraph.
    #[serde(default)]
    pub lead: String,
    #[serde(default)]
    pub trail: String,
    pub revision: String,
    /// Where lines go before `\begin{document}`, when the preamble is in
    /// the open file.
    pub preamble_at: Option<usize>,
    /// The lines put there for the answer.
    #[serde(default)]
    pub preamble: String,
    /// Lines the answer needs in the root's preamble when the open file is
    /// a part: the builds of the check have them in the root's copy, and
    /// the card asks the author to add them.
    #[serde(default)]
    pub root_lines: String,
    /// The model fits the answer to the document before the first build
    /// (Accept with the model's help). A model that cannot be reached is an
    /// error, and the answer is applied as it is.
    #[serde(default)]
    pub fit: bool,
}

/// The document of `start` with `revision` and `preamble` in it, and where
/// the answer and the preamble lines are in it (bytes).
fn compose(
    start: &CheckStart,
    revision: &str,
    preamble: &str,
) -> (String, Range<usize>, Option<Range<usize>>) {
    let text = &start.text;
    let from = utf16_to_byte(text, start.from);
    let to = utf16_to_byte(text, start.to).max(from);
    let at = start
        .preamble_at
        .map(|p| utf16_to_byte(text, p))
        .filter(|p| *p <= from);
    let mut out = String::with_capacity(text.len() + revision.len() + preamble.len() + 8);
    let mut lines = None;
    match at {
        Some(p) => {
            out.push_str(&text[..p]);
            lines = Some(p..p + preamble.len());
            out.push_str(preamble);
            out.push_str(&text[p..from]);
        }
        None => out.push_str(&text[..from]),
    }
    out.push_str(&start.lead);
    let answer_start = out.len();
    out.push_str(revision);
    let answer = answer_start..out.len();
    out.push_str(&start.trail);
    out.push_str(&text[to..]);
    (out, answer, lines)
}

/// `after`'s errors that `before` did not have: those in the answer or the
/// lines added for it, and the others beyond as many with the same message
/// as `before` had.
fn new_errors(
    after: &[TexError],
    before: &[TexError],
    text: &str,
    answer: &Range<usize>,
    lines: Option<&Range<usize>>,
) -> Vec<TexError> {
    let mut had: HashMap<&str, usize> = HashMap::new();
    for e in before {
        *had.entry(e.message.as_str()).or_insert(0) += 1;
    }
    let ours = |line: usize| {
        repair::lines_of(text, answer).contains(&line)
            || lines.is_some_and(|r| r.end > r.start && repair::lines_of(text, r).contains(&line))
    };
    after
        .iter()
        .filter(|e| {
            if e.line.is_some_and(ours) {
                return true;
            }
            match had.get_mut(e.message.as_str()) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    false
                }
                _ => true,
            }
        })
        .cloned()
        .collect()
}

/// Build the document with the answer of `start` in it, and while that
/// brings errors the document did not have, have the model correct the
/// answer and its preamble lines and build again, at most [`MAX_ROUNDS`]
/// times.
pub(crate) async fn check_loop(
    state: &Document,
    model: &dyn ChatModel,
    start: &CheckStart,
    on_stage: OnStage<'_>,
) -> CommandResult<CheckResult> {
    let editable = start.preamble_at.is_some();
    let mut revision = start.revision.clone();
    let mut preamble = start.preamble.clone();
    let packages = crate::latex_commands::document_packages(state, &start.text)
        .map(|(p, _)| p.into_iter().collect::<Vec<_>>());
    let mut notes = Notes::default();
    let mut usage = TokenUsage::default();
    // First the model fits the answer to the document.
    let mut fitted = false;
    if start.fit {
        on_stage(true, 0);
        let (text, answer, lines) = compose(start, &revision, &preamble);
        let (system, user) = repair::answer_fix_prompt(&repair::AnswerFixRequest {
            text: &text,
            answer: answer.clone(),
            preamble: if editable {
                Some(lines.clone().unwrap_or(answer.start..answer.start))
            } else {
                None
            },
            errors: &[],
            packages: packages.as_deref(),
        });
        let chat = ChatRequest {
            purpose: "check".to_owned(),
            system,
            user,
            images: Vec::new(),
        };
        let out = ask(None, start.id, model, &chat)
            .await
            .map_err(|e| CommandError::Settings(e.to_string()))?;
        add_usage(&mut usage, out.usage);
        let fit = repair::parse_answer_fix(&out.text);
        let next_revision = fit.revision.unwrap_or_else(|| revision.clone());
        let next_preamble = match fit.preamble {
            Some(p) if editable => p,
            _ => preamble.clone(),
        };
        if next_revision != revision || next_preamble != preamble {
            fitted = true;
            notes.add(fit.changes_zh, fit.changes_en);
            revision = next_revision;
            preamble = next_preamble;
        }
    }
    on_stage(false, 0);
    let (mut text, mut answer, mut lines) = compose(start, &revision, &preamble);
    let mut checked = check_build_with(state, &text, CHECK_FOLDER, &start.root_lines).await?;
    // The errors the document had without the answer, read only when
    // there are errors at all.
    let before = if checked.errors.is_empty() {
        Vec::new()
    } else {
        check_build(state, &start.text, BEFORE_FOLDER).await?.errors
    };
    let mut new = new_errors(&checked.errors, &before, &text, &answer, lines.as_ref());
    let mut rounds = 0;
    while !new.is_empty() && rounds < MAX_ROUNDS {
        rounds += 1;
        on_stage(true, rounds);
        let (system, user) = repair::answer_fix_prompt(&repair::AnswerFixRequest {
            text: &text,
            answer: answer.clone(),
            preamble: if editable {
                Some(lines.clone().unwrap_or(answer.start..answer.start))
            } else {
                None
            },
            errors: &new,
            packages: packages.as_deref(),
        });
        let chat = ChatRequest {
            purpose: "check".to_owned(),
            system,
            user,
            images: Vec::new(),
        };
        let out = ask(None, start.id, model, &chat)
            .await
            .map_err(|e| CommandError::Settings(e.to_string()))?;
        add_usage(&mut usage, out.usage);
        let fix = repair::parse_answer_fix(&out.text);
        notes.add(fix.changes_zh, fix.changes_en);
        let next_revision = fix.revision.unwrap_or_else(|| revision.clone());
        let next_preamble = match fix.preamble {
            Some(p) if editable => p,
            _ => preamble.clone(),
        };
        if next_revision == revision && next_preamble == preamble {
            break;
        }
        revision = next_revision;
        preamble = next_preamble;
        (text, answer, lines) = compose(start, &revision, &preamble);
        on_stage(false, rounds);
        checked = check_build_with(state, &text, CHECK_FOLDER, &start.root_lines).await?;
        new = new_errors(&checked.errors, &before, &text, &answer, lines.as_ref());
    }
    Ok(CheckResult {
        revision,
        preamble,
        passed: new.is_empty(),
        errors: new,
        before: before.len(),
        rounds,
        fitted,
        changes_zh: notes.zh,
        changes_en: notes.en,
        usage,
    })
}

/// Check the answer of job `request.id` before it is applied. The result
/// comes as a `checked` event (or `check_failed`).
#[tauri::command]
pub async fn assist_check(app: AppHandle, state: Doc, request: CheckStart) -> CommandResult<()> {
    if state.engine.mode() != Mode::Latex {
        return Err(CommandError::Settings(
            "only a LaTeX document can be compiled".into(),
        ));
    }
    crate::latex_commands::project(&state)?;
    let model: Arc<dyn ChatModel> = state.assistant_model()?;
    let id = request.id;
    let (app, task_state) = (app.clone(), state.clone());
    let handle = tauri::async_runtime::spawn(async move {
        let state = task_state;
        let on_stage = |fixing: bool, round: u32| {
            emit(
                &app,
                &state.label,
                &AssistEvent::Stage { id, fixing, round },
            );
        };
        let checked = check_loop(&state, model.as_ref(), &request, &on_stage).await;
        state.assist.checks().remove(&id);
        let event = match checked {
            Ok(result) => AssistEvent::Checked {
                id,
                result: Box::new(result),
            },
            Err(e) => {
                log::warn!("the check of an answer failed: {e}");
                AssistEvent::CheckFailed {
                    id,
                    message: e.to_string(),
                }
            }
        };
        emit(&app, &state.label, &event);
    });
    if let Some(previous) = state
        .assist
        .checks()
        .insert(id, handle.inner().abort_handle())
    {
        previous.abort();
    }
    Ok(())
}

/// Stop the check of job `id`'s answer.
#[tauri::command]
pub async fn assist_check_cancel(app: AppHandle, state: Doc, id: u64) -> CommandResult<()> {
    if let Some(handle) = state.assist.checks().remove(&id) {
        handle.abort();
        emit(
            &app,
            &state.label,
            &AssistEvent::CheckFailed {
                id,
                message: "cancelled".into(),
            },
        );
    }
    Ok(())
}

/// A picture of its own (a PDF figure): its document, and where the
/// picture and the lines before `\begin{document}` are in it (bytes).
fn standalone(picture: &str, preamble: &str) -> (String, Range<usize>, Range<usize>) {
    let mut doc = String::from("\\documentclass[border=3pt]{standalone}\n");
    if !preamble.contains("{pgfplots}") && !preamble.contains("{tikz}") {
        doc.push_str("\\usepackage{tikz}\n");
    }
    let pre_start = doc.len();
    if !preamble.trim().is_empty() {
        doc.push_str(preamble.trim());
        doc.push('\n');
    }
    let pre = pre_start..doc.len();
    doc.push_str("\\begin{document}\n");
    let start = doc.len();
    doc.push_str(picture.trim());
    let picture = start..doc.len();
    doc.push_str("\n\\end{document}\n");
    (doc, picture, pre)
}

/// A picture built as a PDF.
pub(crate) struct Picture {
    pub(crate) pdf: Vec<u8>,
    pub(crate) changes_zh: Vec<String>,
    pub(crate) changes_en: Vec<String>,
    pub(crate) usage: TokenUsage,
}

/// Build `picture` as a PDF of its own in `work`, and while TeX reports
/// errors, have the model repair the picture and its lines before
/// `\begin{document}` and build again, at most [`MAX_ROUNDS`] times.
pub(crate) async fn picture_loop(
    tc: &biwrite_latex::Toolchain,
    model: &dyn ChatModel,
    id: u64,
    work: &std::path::Path,
    picture: &str,
    preamble: &str,
) -> Result<Picture, String> {
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let mut picture = picture.trim().to_owned();
    let mut preamble = preamble.trim().to_owned();
    let mut notes = Notes::default();
    let mut usage = TokenUsage::default();
    let mut rounds = 0;
    loop {
        let (doc, at, pre) = standalone(&picture, &preamble);
        std::fs::write(work.join("figure.tex"), &doc).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(work.join("figure.aux"));
        let engine = if doc.chars().any(biwrite_core::lang::is_cjk) {
            biwrite_latex::Engine::Xelatex
        } else {
            biwrite_latex::Engine::Pdflatex
        };
        let job = biwrite_latex::Job {
            dir: work.to_path_buf(),
            root: "figure.tex".into(),
            engine,
            out_dir: None,
            timeout: std::time::Duration::from_secs(120),
        };
        let built = biwrite_latex::compile(tc, &job)
            .await
            .map_err(|e| e.to_string())?;
        let log = std::fs::read(work.join("figure.log"))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let errors = crate::latex_commands::tex_errors(
            &built,
            &log,
            |f| f.trim_start_matches("./").to_owned(),
            |f| f.is_none_or(|f| f == "figure.tex"),
        );
        if errors.is_empty()
            && let Some(pdf) = built.pdf.as_deref().and_then(|p| std::fs::read(p).ok())
        {
            return Ok(Picture {
                pdf,
                changes_zh: notes.zh,
                changes_en: notes.en,
                usage,
            });
        }
        let first = errors
            .first()
            .map_or_else(|| "no PDF".to_owned(), |e| e.message.clone());
        if rounds >= MAX_ROUNDS {
            return Err(format!("the picture does not compile: {first}"));
        }
        rounds += 1;
        let (system, user) = repair::answer_fix_prompt(&repair::AnswerFixRequest {
            text: &doc,
            answer: at,
            preamble: Some(pre),
            errors: &errors,
            packages: None,
        });
        let chat = ChatRequest {
            purpose: "figure".to_owned(),
            system,
            user,
            images: Vec::new(),
        };
        let out = ask(None, id, model, &chat)
            .await
            .map_err(|e| e.to_string())?;
        add_usage(&mut usage, out.usage);
        let fix = repair::parse_answer_fix(&out.text);
        notes.add(fix.changes_zh, fix.changes_en);
        let next = fix.revision.unwrap_or_else(|| picture.clone());
        let next_pre = fix.preamble.unwrap_or_else(|| preamble.clone());
        if next.trim() == picture && next_pre.trim() == preamble {
            return Err(format!("the picture does not compile: {first}"));
        }
        picture = next.trim().to_owned();
        preamble = next_pre.trim().to_owned();
    }
}

#[cfg(test)]
#[path = "repair_tests.rs"]
mod tests;
