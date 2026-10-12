//! Writing assistant commands. A polish, edit, ask or figure job runs in the
//! background: `assist_start` answers at once with the job id and the
//! resolved target, and the job reports through `assist` events (partial
//! text, the result, or a failure). The editor applies an approved revision
//! itself; `assist_offer` first gives the engine the approved translation.
//!
//! Payloads carry document text, answers and attachment previews, never
//! keys. Reference images are read in Rust from files picked in a native
//! dialog; the webview only names them by id.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use biwrite_core::assist::{self, Action, Answer, DiffPart, FigureFormat, Scope};
use biwrite_core::glossary;
use biwrite_core::repair::Hunk;
use biwrite_core::utf16::{byte_to_utf16, utf16_to_byte};
use biwrite_core::{Mode, Protector};
use biwrite_engine::{PartialFn, TokenUsage, TranslateError};
use biwrite_providers::{ChatModel, ChatOutput, ChatRequest, ImageInput};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use crate::error::{CommandError, CommandResult};
use crate::skills::SkillInfo;
use crate::state::AppState;

pub const EVENT_ASSIST: &str = "assist";

/// Longest document sent with the "whole document" scope, in characters.
const MAX_DOCUMENT_CHARS: usize = 300_000;
/// Largest reference image.
const MAX_IMAGE_BYTES: u64 = 8_000_000;
/// Longest reference text from a file, in characters (a long paper's body).
const MAX_REFERENCE_CHARS: usize = 120_000;
/// Largest reference file read (it is then cut to `MAX_REFERENCE_CHARS`).
const MAX_REFERENCE_BYTES: u64 = 8_000_000;
/// Minimum interval between streamed updates of one job.
pub(crate) const PARTIAL_EVERY: Duration = Duration::from_millis(80);
/// Retries of a request after a transient failure (a relay at capacity, a
/// dropped connection) when the provider sets no number.
const DEFAULT_RETRIES: u32 = 5;
/// The longest wait between two tries. The author is waiting for the
/// answer, and a relay at capacity frees up at any moment.
const RETRY_WAIT_MAX: Duration = Duration::from_secs(5);
/// The longest wait a rate limit may ask for before the next try.
const RETRY_HINT_MAX: Duration = Duration::from_secs(30);

/// Running jobs and attached reference images.
#[derive(Default)]
pub struct AssistState {
    pub(crate) next_job: AtomicU64,
    jobs: Mutex<HashMap<u64, AbortHandle>>,
    /// Checks of answers being applied, by the job whose answer it is.
    checks: Mutex<HashMap<u64, AbortHandle>>,
    next_image: AtomicU64,
    images: Mutex<HashMap<u64, Attachment>>,
}

struct Attachment {
    image: ImageInput,
}

impl AssistState {
    pub(crate) fn jobs(&self) -> std::sync::MutexGuard<'_, HashMap<u64, AbortHandle>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn checks(&self) -> std::sync::MutexGuard<'_, HashMap<u64, AbortHandle>> {
        self.checks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn images(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Attachment>> {
        self.images.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistStart {
    pub action: Action,
    #[serde(default)]
    pub scope: Scope,
    /// The editor's text when the job starts.
    pub text: String,
    /// Selection or cursor, in UTF-16 offsets of `text`.
    pub from: usize,
    pub to: usize,
    #[serde(default)]
    pub instruction: String,
    #[serde(default)]
    pub references: Vec<String>,
    /// Attachment ids of reference images.
    #[serde(default)]
    pub images: Vec<u64>,
    /// Earlier questions and answers of an ask conversation.
    #[serde(default)]
    pub history: Vec<(String, String)>,
    /// What a figure becomes (LaTeX documents).
    #[serde(default)]
    pub figure: FigureFormat,
}

/// What a job works on, in the editor's coordinates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistTarget {
    /// UTF-16 range in the text the job started with.
    pub from: usize,
    pub to: usize,
    /// The target text (for a figure: the paragraph it follows).
    pub text: String,
    /// The range is exactly one paragraph's content, so an approved
    /// translation can stand as that paragraph's translation.
    pub whole_paragraph: bool,
    /// The answer is inserted at `from` instead of replacing the range.
    pub insert: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistStarted {
    pub id: u64,
    pub target: AssistTarget,
    /// "Name · model" of the assistant's provider.
    pub model: String,
    /// Name and version of the skill the rules come from.
    pub skill: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistResult {
    #[serde(flatten)]
    pub answer: Answer,
    /// Word-level diff from the target to the revision.
    pub diff: Vec<DiffPart>,
    pub usage: TokenUsage,
    pub duration_ms: u64,
    /// Packages a figure needs that the document does not load.
    pub missing_packages: Vec<String>,
    /// TikZ libraries a figure loads that the preamble does not. They go
    /// there with the packages, and the figure's own `\usetikzlibrary`
    /// lines are taken out of the revision.
    pub libraries: Vec<String>,
    /// The open file holds the preamble, so the packages can be added there.
    pub preamble_here: bool,
    /// The changes of a fix, in whole lines of the text it started from.
    pub hunks: Vec<Hunk>,
    /// How a fix went.
    pub fix: Option<crate::repair_commands::FixReport>,
    /// A figure that is a file: what was made.
    pub figure: Option<crate::figures::FigureView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssistEvent {
    Partial {
        id: u64,
        text: String,
    },
    /// The request failed for a passing reason and is sent again: try
    /// `attempt` of `of` retries.
    Retrying {
        id: u64,
        attempt: u32,
        of: u32,
        message: String,
    },
    Done {
        id: u64,
        result: Box<AssistResult>,
    },
    Failed {
        id: u64,
        message: String,
    },
    /// A fix, or the check of an answer being applied, moved on: building
    /// the document (`fixing` false) or asking the model to repair it, in
    /// round `round` (0 before any repair).
    Stage {
        id: u64,
        fixing: bool,
        round: u32,
    },
    /// The answer of job `id`, checked (and repaired) for applying.
    Checked {
        id: u64,
        result: Box<crate::repair_commands::CheckResult>,
    },
    /// The check of job `id`'s answer could not finish.
    CheckFailed {
        id: u64,
        message: String,
    },
}

pub(crate) fn emit(app: &AppHandle, event: &AssistEvent) {
    if let Err(e) = app.emit(EVENT_ASSIST, event) {
        log::warn!("failed to emit {EVENT_ASSIST}: {e}");
    }
}

/// The byte range a job works on, its text, and how the answer is applied.
#[derive(Debug, PartialEq, Eq)]
struct Resolved {
    range: Range<usize>,
    text: String,
    whole_paragraph: bool,
    insert: bool,
}

/// Resolve the cursor or selection (byte offsets) into a target. New text
/// (a figure, new paragraphs) goes after the whole segment of the cursor
/// or of the selection's end, never inside it (see
/// [`assist::insertion_point`]).
fn resolve(text: &str, mode: Mode, action: Action, a: usize, b: usize) -> Result<Resolved, String> {
    let (a, b) = (a.min(b), a.max(b));
    let insert_at = |pos: usize| {
        assist::insertion_point(text, mode, pos).ok_or_else(|| {
            "nothing after \\end{document} is typeset: put the cursor in the document".to_owned()
        })
    };
    if a == b {
        return match (assist::paragraph_at(text, mode, a), action.inserts()) {
            (Some(p), true) => {
                let at = insert_at(a)?;
                Ok(Resolved {
                    range: at..at,
                    text: text[p].to_owned(),
                    whole_paragraph: false,
                    insert: true,
                })
            }
            (Some(p), false) => Ok(Resolved {
                text: text[p.clone()].to_owned(),
                range: p,
                whole_paragraph: true,
                insert: false,
            }),
            (None, true) => {
                let at = insert_at(a)?;
                Ok(Resolved {
                    range: at..at,
                    text: String::new(),
                    whole_paragraph: false,
                    insert: true,
                })
            }
            (None, false) => Err("put the cursor in a paragraph or select text first".into()),
        };
    }
    let selected = &text[a..b];
    let start = a + (selected.len() - selected.trim_start().len());
    let end = b - (selected.len() - selected.trim_end().len());
    if start >= end {
        return Err("the selection is empty".into());
    }
    if action.inserts() {
        let at = insert_at(end)?;
        return Ok(Resolved {
            range: at..at,
            text: text[start..end].to_owned(),
            whole_paragraph: false,
            insert: true,
        });
    }
    let whole = assist::paragraph_at(text, mode, start).is_some_and(|p| p == (start..end));
    Ok(Resolved {
        range: start..end,
        text: text[start..end].to_owned(),
        whole_paragraph: whole,
        insert: false,
    })
}

/// The document for the "whole document" scope, cut to a window of
/// `MAX_DOCUMENT_CHARS` characters around the target when it is longer.
fn document_window(text: &str, range: &Range<usize>) -> String {
    let total = text.chars().count();
    if total <= MAX_DOCUMENT_CHARS {
        return text.to_owned();
    }
    let at = text[..range.start.min(text.len())].chars().count();
    let last = (at.saturating_sub(MAX_DOCUMENT_CHARS / 2) + MAX_DOCUMENT_CHARS).min(total);
    let first = last - MAX_DOCUMENT_CHARS;
    let byte = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(b, _)| b);
    format!("[…]\n{}\n[…]", &text[byte(first)..byte(last)])
}

/// Start a polish, edit, ask or figure job.
#[tauri::command]
pub async fn assist_start(
    app: AppHandle,
    state: State<'_, AppState>,
    request: AssistStart,
) -> CommandResult<AssistStarted> {
    let mode = state.engine.mode();
    let direction = state.engine.direction();
    let text = &request.text;
    let a = utf16_to_byte(text, request.from);
    let b = utf16_to_byte(text, request.to);
    if request.action == Action::Fix {
        return Err(CommandError::Settings(
            "a fix starts with assist_fix".into(),
        ));
    }
    let target = resolve(text, mode, request.action, a, b).map_err(CommandError::Settings)?;
    if request.action == Action::Mirror && request.instruction.trim().is_empty() {
        return Err(CommandError::Settings(
            "write the new translation of the paragraph first".into(),
        ));
    }
    let model = state.assistant_model()?;
    let (skill, info) = state.skill()?;

    let (before, after) = match request.scope {
        Scope::Neighbors => assist::neighbors(text, mode, &target.range, 2),
        _ => (None, None),
    };
    let document = (request.scope == Scope::Document).then(|| document_window(text, &target.range));
    let settings = state.settings().clone();
    let masked = Protector::new(mode).mask(&target.text);
    let terms = glossary::relevant(&masked, direction, &settings.glossary);
    let note = state.doc_note();
    // What a figure can rely on in this document.
    let packages = (request.action == Action::Figure)
        .then(|| crate::latex_commands::document_packages(&state, text))
        .flatten();
    let package_list: Option<Vec<String>> =
        packages.as_ref().map(|(p, _)| p.iter().cloned().collect());
    // TikZ libraries the preamble here loads (a figure's own go there).
    let libraries = packages
        .as_ref()
        .filter(|(_, here)| *here)
        .map(|_| biwrite_latex::packages::loaded_libraries(text));
    // A figure that is a file follows the document's figures.
    let figure = if request.action == Action::Figure && mode == Mode::Latex {
        request.figure
    } else {
        FigureFormat::Latex
    };
    let place = (figure != FigureFormat::Latex).then(|| {
        let project = crate::latex_commands::project(&state).ok();
        let (root_text, root_dir) = match &project {
            Some(p) => (p.root_text.clone(), Some(p.dir.clone())),
            None => (text.clone(), None),
        };
        (
            crate::figures::place(&root_text, text, root_dir.as_deref()),
            root_dir,
        )
    });
    let images: Vec<ImageInput> = {
        let all = state.assist.images();
        request
            .images
            .iter()
            .filter_map(|id| all.get(id).map(|a| a.image.clone()))
            .collect()
    };
    let prompt = assist::build(
        &assist::Request {
            action: request.action,
            mode,
            direction,
            target: &target.text,
            instruction: &request.instruction,
            before: before.as_deref(),
            after: after.as_deref(),
            document: document.as_deref(),
            doc_note: Some(note.as_str()),
            glossary: &terms,
            references: &request.references,
            images: images.len(),
            history: &request.history,
            packages: package_list.as_deref(),
            figure,
        },
        &skill,
    );

    let bounds = byte_to_utf16(text, [target.range.start, target.range.end]);
    let started = AssistStarted {
        id: state.assist.next_job.fetch_add(1, Ordering::Relaxed) + 1,
        target: AssistTarget {
            from: bounds[0],
            to: bounds[1],
            text: target.text.clone(),
            whole_paragraph: target.whole_paragraph,
            insert: target.insert,
        },
        model: model.label(),
        skill: format!("{} {}", info.name, info.version),
    };
    let id = started.id;
    let chat = ChatRequest {
        purpose: request.action.purpose().to_owned(),
        system: prompt.system,
        user: prompt.user,
        images,
    };
    let job = Job {
        app: app.clone(),
        id,
        action: request.action,
        original: target.text,
        instruction: request.instruction.clone(),
        protector: prompt.protector,
        model,
        chat,
        packages,
        libraries,
        figure,
        place,
    };
    let handle = tauri::async_runtime::spawn(run(job));
    state
        .assist
        .jobs()
        .insert(id, handle.inner().abort_handle());
    Ok(started)
}

struct Job {
    app: AppHandle,
    id: u64,
    action: Action,
    original: String,
    /// The instruction (for `Mirror`, the author's new translation).
    instruction: String,
    protector: Protector,
    model: Arc<dyn ChatModel>,
    chat: ChatRequest,
    /// For a figure in LaTeX: the packages the document loads, and whether
    /// the open file holds the preamble.
    packages: Option<(BTreeSet<String>, bool)>,
    /// For a figure whose preamble is in the open file: the TikZ libraries
    /// it loads.
    libraries: Option<BTreeSet<String>>,
    figure: FigureFormat,
    /// For a figure that is a file: how the document includes figures, and
    /// the folder TeX runs in (unknown before the first save).
    place: Option<(crate::figures::Place, Option<PathBuf>)>,
}

async fn run(job: Job) {
    let started = Instant::now();
    let last = Mutex::new(None::<Instant>);
    let on_partial = |text: &str| {
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
                &job.app,
                &AssistEvent::Partial {
                    id: job.id,
                    text: assist::streaming_preview(text),
                },
            );
        }
    };
    let on_retry = |attempt: u32, of: u32, err: &TranslateError| {
        log::info!(
            "assistant {}: {err}; retry {attempt} of {of}",
            job.action.purpose()
        );
        emit(
            &job.app,
            &AssistEvent::Retrying {
                id: job.id,
                attempt,
                of,
                message: err.to_string(),
            },
        );
    };
    let outcome = complete_with_retries(
        job.model.as_ref(),
        &job.chat,
        &on_partial,
        &on_retry,
        retry_wait,
    )
    .await;
    let event = match outcome {
        Err(e) => AssistEvent::Failed {
            id: job.id,
            message: e.to_string(),
        },
        Ok(out) if job.place.is_some() => match figure_file(&job, &out.text, out.usage).await {
            Ok(mut result) => {
                result.duration_ms = started.elapsed().as_millis() as u64;
                AssistEvent::Done {
                    id: job.id,
                    result: Box::new(result),
                }
            }
            Err(message) => AssistEvent::Failed {
                id: job.id,
                message,
            },
        },
        Ok(out) => match assist::parse_for(job.action, &out.text, &job.protector, &job.original) {
            Err(e) => AssistEvent::Failed {
                id: job.id,
                message: e.to_string(),
            },
            Ok(mut answer) => {
                if job.action == Action::Mirror && answer.revision.is_some() {
                    // The author's own text is the paragraph's translation.
                    answer.translation = Some(job.instruction.trim().to_owned());
                    answer.translation_matches = true;
                }
                let diff = match (&answer.revision, job.action) {
                    (
                        Some(revision),
                        Action::Polish | Action::Edit | Action::Ask | Action::Mirror,
                    ) => assist::diff(&job.original, revision),
                    _ => Vec::new(),
                };
                // The figure's TikZ libraries go to the preamble.
                let mut libraries = Vec::new();
                if let (Some(loaded), Some(revision)) = (&job.libraries, answer.revision.as_mut()) {
                    let (wanted, rest) = biwrite_latex::packages::hoist_libraries(revision);
                    if !wanted.is_empty() {
                        *revision = rest.trim().to_owned();
                        libraries = wanted.into_iter().filter(|l| !loaded.contains(l)).collect();
                    }
                }
                let (mut missing_packages, preamble_here) = match (&job.packages, &answer.revision)
                {
                    (Some((loaded, here)), Some(revision)) => {
                        (biwrite_latex::packages::missing(revision, loaded), *here)
                    }
                    _ => (Vec::new(), false),
                };
                if !libraries.is_empty()
                    && let Some((loaded, _)) = &job.packages
                    && !loaded.contains("tikz")
                    && !loaded.contains("pgfplots")
                    && !missing_packages
                        .iter()
                        .any(|p| p == "tikz" || p == "pgfplots")
                {
                    missing_packages.push("tikz".to_owned());
                }
                AssistEvent::Done {
                    id: job.id,
                    result: Box::new(AssistResult {
                        answer,
                        diff,
                        usage: out.usage,
                        duration_ms: started.elapsed().as_millis() as u64,
                        missing_packages,
                        libraries,
                        preamble_here,
                        hunks: Vec::new(),
                        fix: None,
                        figure: None,
                    }),
                }
            }
        },
    };
    if let AssistEvent::Failed { message, .. } = &event {
        log::warn!("assistant {} failed: {message}", job.action.purpose());
    }
    if let Some(state) = tauri::Manager::try_state::<AppState>(&job.app) {
        state.assist.jobs().remove(&job.id);
    }
    emit(&job.app, &event);
}

/// A figure that is a file, made from the model's `reply`: the PowerPoint
/// file and its PDF (through LibreOffice, when it is installed), or the PDF
/// of the picture (repaired by the model while it does not compile). The
/// files wait for the document's save; the answer is the figure
/// environment that includes them.
async fn figure_file(job: &Job, reply: &str, usage: TokenUsage) -> Result<AssistResult, String> {
    let state = tauri::Manager::try_state::<AppState>(&job.app).ok_or("the app is closing")?;
    let place = job.place.clone().ok_or("not a figure file")?;
    make_figure(
        &state,
        job.model.as_ref(),
        job.id,
        job.figure,
        place,
        job.packages.as_ref(),
        reply,
        usage,
    )
    .await
}

/// [`figure_file`] for job `id`, whose format is `format` and whose
/// document includes figures as `place` says.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn make_figure(
    state: &AppState,
    model: &dyn ChatModel,
    id: u64,
    format: FigureFormat,
    (place, root_dir): (crate::figures::Place, Option<PathBuf>),
    packages: Option<&(BTreeSet<String>, bool)>,
    reply: &str,
    usage: TokenUsage,
) -> Result<AssistResult, String> {
    use crate::figures::{self, FigureView, Made, Waiting};
    let answer = assist::parse_figure(format, reply).map_err(|e| e.to_string())?;
    let work = std::env::temp_dir().join(format!("biwrite-figure-{}-{id}", std::process::id()));
    let stem = figures::free_stem(&state.figures, &place, root_dir.as_deref(), &answer, "pdf");
    let label = stem
        .strip_prefix(&place.stem_prefix)
        .unwrap_or(&stem)
        .to_owned();
    let pdf_file = format!("{}{stem}.pdf", place.dir);
    let reference = format!("{}{stem}.pdf", place.prefix);
    let mut usage = usage;
    let mut changes_zh = answer.changes_zh.clone();
    let mut changes_en = answer.changes_en.clone();
    let (environment, view, made, files) = match format {
        FigureFormat::Pptx => {
            // The icons of the skill in use.
            let icons = state
                .skill()
                .map(|(s, _)| s.icons.clone())
                .unwrap_or_default();
            let laid = figures::laid(&answer, &icons)?;
            let pptx = laid.pptx()?;
            let pptx_file = format!("{}{stem}.pptx", place.dir);
            let pdf = match figures::soffice() {
                Some(soffice) => match figures::convert(&soffice, &work, &stem, &pptx).await {
                    Ok(pdf) => Some(pdf),
                    Err(e) => {
                        log::warn!("the figure was not converted: {e}");
                        None
                    }
                },
                None => None,
            };
            let environment = figures::environment(
                &place,
                &reference,
                &answer.caption,
                &label,
                laid.wide || figures::wide(laid.width),
                pdf.is_none().then_some(pptx_file.as_str()),
            );
            let mut files = vec![(pptx_file.clone(), pptx.clone())];
            if let Some(pdf) = &pdf {
                files.insert(0, (pdf_file.clone(), pdf.clone()));
            }
            let view = FigureView {
                format: FigureFormat::Pptx,
                file: pptx_file,
                pdf: pdf.is_some(),
                pptx: true,
                svg: pdf.is_none().then(|| laid.svg()),
                export: pdf.is_none(),
            };
            let made = Made {
                stem: stem.clone(),
                pdf,
                pptx: Some(pptx),
            };
            (environment, view, made, files)
        }
        _ => {
            let tc = crate::latex_commands::require_toolchain(state)
                .await
                .map_err(|e| e.to_string())?;
            let picture = crate::repair_commands::picture_loop(
                &tc,
                model,
                id,
                &work,
                &answer.body,
                &answer.preamble,
            )
            .await?;
            usage.input_tokens += picture.usage.input_tokens;
            usage.output_tokens += picture.usage.output_tokens;
            changes_zh.extend(picture.changes_zh);
            changes_en.extend(picture.changes_en);
            let wide = figures::pdf_width(&picture.pdf).is_some_and(figures::wide);
            let environment =
                figures::environment(&place, &reference, &answer.caption, &label, wide, None);
            let view = FigureView {
                format: FigureFormat::Pdf,
                file: pdf_file.clone(),
                pdf: true,
                pptx: false,
                svg: None,
                export: false,
            };
            let made = Made {
                stem: stem.clone(),
                pdf: Some(picture.pdf.clone()),
                pptx: None,
            };
            (
                environment,
                view,
                made,
                vec![(pdf_file.clone(), picture.pdf)],
            )
        }
    };
    let _ = std::fs::remove_dir_all(&work);
    state.figures.wait(Waiting {
        reference: reference.clone(),
        files,
    });
    state.figures.made().insert(id, made);
    let (missing_packages, preamble_here) = match packages {
        Some((loaded, here)) => (
            biwrite_latex::packages::missing(&environment, loaded),
            *here,
        ),
        None => (Vec::new(), false),
    };
    Ok(AssistResult {
        answer: Answer {
            revision: Some(environment),
            translation: answer.translation,
            changes_zh,
            changes_en,
            ..Answer::default()
        },
        diff: Vec::new(),
        usage,
        duration_ms: 0,
        missing_packages,
        libraries: Vec::new(),
        preamble_here,
        hunks: Vec::new(),
        fix: None,
        figure: Some(view),
    })
}

/// The PDF a figure job made, for its preview.
#[tauri::command]
pub async fn figure_pdf(
    state: State<'_, AppState>,
    id: u64,
) -> CommandResult<tauri::ipc::Response> {
    let pdf = state
        .figures
        .made()
        .get(&id)
        .and_then(|m| m.pdf.clone())
        .ok_or_else(|| CommandError::Settings("this figure has no PDF".into()))?;
    Ok(tauri::ipc::Response::new(pdf))
}

/// Save a copy of what a figure job made, where the author picks: its
/// PowerPoint file, its PDF, or a PNG the preview drew (`png`).
#[tauri::command]
pub async fn figure_save(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: u64,
    kind: String,
    png: Option<Vec<u8>>,
) -> CommandResult<Option<String>> {
    let made = state
        .figures
        .made()
        .get(&id)
        .cloned()
        .ok_or_else(|| CommandError::Settings("this figure is no longer there".into()))?;
    let (bytes, label) = match kind.as_str() {
        "pptx" => (made.pptx, "PowerPoint"),
        "pdf" => (made.pdf, "PDF"),
        "png" => (png, "PNG"),
        _ => (None, ""),
    };
    let bytes =
        bytes.ok_or_else(|| CommandError::Settings(format!("this figure has no {kind} file")))?;
    let dir = state
        .file()
        .path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    let name = format!("{}.{kind}", made.stem);
    let Some(dest) = crate::files::pick_save_kind(
        &app,
        &window,
        label,
        &[kind.as_str()],
        dir.as_deref(),
        &name,
    )
    .await
    else {
        return Ok(None);
    };
    crate::files::write_file_atomic(dest.clone(), bytes).await?;
    Ok(Some(dest.display().to_string()))
}

/// The wait before retry number `attempt` (from 1) after `err`: a rate
/// limit's hint, else one second doubling up to [`RETRY_WAIT_MAX`].
pub(crate) fn retry_wait(attempt: u32, err: &TranslateError) -> Duration {
    if let Some(hint) = err.retry_after() {
        return hint.min(RETRY_HINT_MAX);
    }
    Duration::from_secs(1)
        .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
        .min(RETRY_WAIT_MAX)
}

/// The model's answer to `chat`. A transient failure (a server error, a
/// rate limit, the network) sends the request again, up to the provider's
/// retry count (or [`DEFAULT_RETRIES`]), `wait` apart. Each retry is told to
/// `on_retry` (the retry, the count, the failure) and clears the streamed
/// output.
pub(crate) async fn complete_with_retries(
    model: &dyn ChatModel,
    chat: &ChatRequest,
    on_partial: PartialFn<'_>,
    on_retry: &(dyn Fn(u32, u32, &TranslateError) + Send + Sync),
    wait: impl Fn(u32, &TranslateError) -> Duration,
) -> Result<ChatOutput, TranslateError> {
    let retries = model.max_retries().unwrap_or(DEFAULT_RETRIES);
    let mut attempt = 0;
    loop {
        match model.complete(chat, on_partial).await {
            Err(err) if err.is_retryable() && attempt < retries => {
                attempt += 1;
                on_retry(attempt, retries, &err);
                on_partial("");
                tokio::time::sleep(wait(attempt, &err)).await;
            }
            outcome => return outcome,
        }
    }
}

/// Stop a running job.
#[tauri::command]
pub async fn assist_cancel(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
) -> CommandResult<()> {
    if let Some(handle) = state.assist.jobs().remove(&id) {
        handle.abort();
        emit(
            &app,
            &AssistEvent::Failed {
                id,
                message: "cancelled".into(),
            },
        );
    }
    Ok(())
}

/// Give the engine the approved translation of a paragraph about to become
/// `source`, so it shows without a new request.
#[tauri::command]
pub async fn assist_offer(
    state: State<'_, AppState>,
    source: String,
    translation: String,
) -> CommandResult<()> {
    state.engine.offer_translation(&source, &translation);
    Ok(())
}

/// A reference text read from a file.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceView {
    pub name: String,
    pub text: String,
    /// Characters kept, and whether the file had more.
    pub chars: usize,
    pub truncated: bool,
}

/// The prose of a reference paper: for LaTeX, the document body (or the
/// whole file of a section), without comments; at most `MAX_REFERENCE_CHARS`.
pub(crate) fn reference_text(name: &str, raw: &str) -> (String, bool) {
    let latex = name.to_ascii_lowercase().ends_with(".tex");
    let body = if latex {
        let start = raw
            .find("\\begin{document}")
            .map_or(0, |i| i + "\\begin{document}".len());
        let end = raw
            .rfind("\\end{document}")
            .filter(|e| *e >= start)
            .unwrap_or(raw.len());
        let mut out = String::with_capacity(end - start);
        for line in raw[start..end].lines() {
            // A comment starts at an unescaped %.
            let bytes = line.as_bytes();
            let mut cut = line.len();
            let mut i = 0;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'%' => {
                        cut = i;
                        break;
                    }
                    _ => i += 1,
                }
            }
            let kept = &line[..cut.min(line.len())];
            if kept.trim().is_empty() && !line.trim().is_empty() {
                continue; // a comment line
            }
            out.push_str(kept.trim_end());
            out.push('\n');
        }
        out
    } else {
        raw.to_owned()
    };
    // At most one blank line in a row.
    let mut tidy = String::with_capacity(body.len());
    let mut blank = 0;
    for line in body.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        tidy.push_str(line);
        tidy.push('\n');
    }
    let tidy = tidy.trim().to_owned();
    match tidy.char_indices().nth(MAX_REFERENCE_CHARS) {
        Some((cut, _)) => (tidy[..cut].to_owned(), true),
        None => (tidy, false),
    }
}

/// Pick a reference paper or text (`.tex`, `.md`, `.txt`) whose writing to
/// imitate. `None` if cancelled.
#[tauri::command]
pub async fn load_reference(
    app: AppHandle,
    window: WebviewWindow,
) -> CommandResult<Option<ReferenceView>> {
    let Some(path) = crate::files::pick_open_kind(
        &app,
        &window,
        "Reference text",
        &["tex", "md", "markdown", "txt"],
    )
    .await
    else {
        return Ok(None);
    };
    // Checked before reading: a reference is prose, never this large.
    let bytes = crate::files::read_small_file(path.clone(), MAX_REFERENCE_BYTES).await?;
    let file = biwrite_core::TextFile::decode(bytes).map_err(|source| CommandError::Decode {
        path: path.display().to_string(),
        source,
    })?;
    let name = crate::state::display_name(Some(&path));
    let (text, truncated) = reference_text(&name, file.text());
    if text.trim().is_empty() {
        return Err(CommandError::Settings(format!(
            "{name} has no text to imitate"
        )));
    }
    Ok(Some(ReferenceView {
        chars: text.chars().count(),
        name,
        text,
        truncated,
    }))
}

/// A reference image as the webview sees it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    pub id: u64,
    pub name: String,
    /// For the thumbnail (CSP allows `data:` images).
    pub data_url: String,
}

fn media_type(path: &std::path::Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// Pick a reference image in a native dialog. `None` if cancelled.
#[tauri::command]
pub async fn attach_image(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CommandResult<Option<AttachmentView>> {
    let Some(path) = crate::files::pick_open_kind(
        &app,
        &window,
        "Images",
        &["png", "jpg", "jpeg", "webp", "gif"],
    )
    .await
    else {
        return Ok(None);
    };
    let media = media_type(&path)
        .ok_or_else(|| CommandError::Settings("choose a PNG, JPEG, WebP or GIF image".into()))?;
    let bytes = crate::files::read_small_file(path.clone(), MAX_IMAGE_BYTES).await?;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let image = ImageInput {
        media_type: media.to_owned(),
        data,
    };
    let view = AttachmentView {
        id: state.assist.next_image.fetch_add(1, Ordering::Relaxed) + 1,
        name: crate::state::display_name(Some(&path)),
        data_url: image.data_url(),
    };
    state.assist.images().insert(view.id, Attachment { image });
    Ok(Some(view))
}

#[tauri::command]
pub async fn drop_attachment(state: State<'_, AppState>, id: u64) -> CommandResult<()> {
    state.assist.images().remove(&id);
    Ok(())
}

/// The skill the assistant follows.
#[tauri::command]
pub async fn get_skill(state: State<'_, AppState>) -> CommandResult<SkillInfo> {
    Ok(state.skill()?.1)
}

/// Download the latest skill from GitHub.
#[tauri::command]
pub async fn update_skill(state: State<'_, AppState>) -> CommandResult<SkillInfo> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("BiWrite/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| CommandError::Settings(e.to_string()))?;
    state
        .skills
        .update(&client)
        .await
        .map_err(|e| CommandError::Settings(format!("could not update the skill: {e}")))?;
    Ok(state.skill()?.1)
}

/// Use a skill folder of the author's own (it must hold the writing rules).
#[tauri::command]
pub async fn choose_skill_folder(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CommandResult<Option<SkillInfo>> {
    let (tx, rx) = oneshot::channel();
    app.dialog()
        .file()
        .set_parent(&window)
        .pick_folder(move |folder| {
            let _ = tx.send(folder);
        });
    let Some(folder) = rx.await.ok().flatten().and_then(|f| f.into_path().ok()) else {
        return Ok(None);
    };
    if !crate::skills::is_skill_dir(&folder) {
        return Err(CommandError::Settings(format!(
            "{} has no writing-deai.md and writing-playbook.md",
            folder.display()
        )));
    }
    {
        let mut s = state.settings();
        s.skill_folder = Some(folder.display().to_string());
        state.persist(&s)?;
    }
    state.skills.invalidate();
    Ok(Some(state.skill()?.1))
}

/// Go back to the bundled or downloaded skill.
#[tauri::command]
pub async fn reset_skill_folder(state: State<'_, AppState>) -> CommandResult<SkillInfo> {
    {
        let mut s = state.settings();
        s.skill_folder = None;
        state.persist(&s)?;
    }
    state.skills.invalidate();
    Ok(state.skill()?.1)
}

/// Show the skill's folder (the chosen or downloaded one) or open nothing.
#[tauri::command]
pub async fn reveal_skill(state: State<'_, AppState>) -> CommandResult<()> {
    let folder = state
        .settings()
        .skill_folder
        .clone()
        .map(PathBuf::from)
        .or_else(|| state.skills.downloaded_dir());
    match folder {
        Some(dir) => crate::files::reveal(&dir),
        None => Err(CommandError::Settings(
            "the bundled skill has no folder to show".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "First paragraph.\n\nSecond one here.\n\nThird.";

    /// Fails with `error` for the first `fails` requests, then answers.
    struct Flaky {
        fails: u32,
        error: TranslateError,
        retries: Option<u32>,
        calls: std::sync::atomic::AtomicU32,
    }

    impl Flaky {
        fn new(fails: u32, error: TranslateError, retries: Option<u32>) -> Self {
            Self {
                fails,
                error,
                retries,
                calls: std::sync::atomic::AtomicU32::new(0),
            }
        }

        fn calls(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ChatModel for Flaky {
        fn label(&self) -> String {
            "flaky".into()
        }

        fn complete<'a>(
            &'a self,
            _request: &'a ChatRequest,
            on_partial: PartialFn<'a>,
        ) -> biwrite_engine::BoxFuture<'a, Result<ChatOutput, TranslateError>> {
            Box::pin(async move {
                let call = self.calls.fetch_add(1, Ordering::SeqCst);
                on_partial("half an answer");
                if call < self.fails {
                    return Err(self.error.clone());
                }
                Ok(ChatOutput {
                    text: "answer".into(),
                    usage: TokenUsage::default(),
                })
            })
        }

        fn max_retries(&self) -> Option<u32> {
            self.retries
        }
    }

    fn chat() -> ChatRequest {
        ChatRequest {
            purpose: "ask".into(),
            system: String::new(),
            user: "question".into(),
            images: Vec::new(),
        }
    }

    const BUSY: TranslateError = TranslateError::Server {
        status: 500,
        retry_after: None,
    };

    /// A relay at capacity answers once a try gets through: each failed try
    /// is reported, its half answer cleared, and the answer comes back.
    #[tokio::test]
    async fn a_busy_relay_is_asked_again_until_it_answers() {
        let model = Flaky::new(7, BUSY, Some(20));
        let retries = Mutex::new(Vec::new());
        let cleared = std::sync::atomic::AtomicU32::new(0);
        let on_partial = |text: &str| {
            if text.is_empty() {
                cleared.fetch_add(1, Ordering::SeqCst);
            }
        };
        let on_retry = |attempt: u32, of: u32, _: &TranslateError| {
            retries.lock().unwrap().push((attempt, of));
        };
        let out = complete_with_retries(&model, &chat(), &on_partial, &on_retry, |_, _| {
            Duration::ZERO
        })
        .await
        .unwrap();
        assert_eq!(out.text, "answer");
        assert_eq!(model.calls(), 8);
        let retries = retries.into_inner().unwrap();
        assert_eq!(retries.len(), 7);
        assert_eq!(retries[0], (1, 20));
        assert_eq!(retries[6], (7, 20));
        assert_eq!(cleared.load(Ordering::SeqCst), 7);
    }

    /// The provider's retry count bounds the tries, the default without
    /// one, and a rejected request is not sent again.
    #[tokio::test]
    async fn retries_stop_at_the_count_and_skip_rejections() {
        let quiet = |_: &str| {};
        let none = |_: u32, _: u32, _: &TranslateError| {};
        let zero = |_: u32, _: &TranslateError| Duration::ZERO;
        let model = Flaky::new(100, BUSY, Some(3));
        let err = complete_with_retries(&model, &chat(), &quiet, &none, zero)
            .await
            .unwrap_err();
        assert_eq!(err, BUSY);
        assert_eq!(model.calls(), 4, "one try and three retries");
        let model = Flaky::new(100, BUSY, None);
        assert!(
            complete_with_retries(&model, &chat(), &quiet, &none, zero)
                .await
                .is_err()
        );
        assert_eq!(model.calls(), DEFAULT_RETRIES + 1);
        let rejected = TranslateError::Rejected {
            status: 401,
            message: "bad key".into(),
        };
        let model = Flaky::new(100, rejected, Some(20));
        assert!(
            complete_with_retries(&model, &chat(), &quiet, &none, zero)
                .await
                .is_err()
        );
        assert_eq!(model.calls(), 1);
    }

    #[test]
    fn retry_waits_stay_short() {
        assert_eq!(retry_wait(1, &BUSY), Duration::from_secs(1));
        assert_eq!(retry_wait(2, &BUSY), Duration::from_secs(2));
        assert_eq!(retry_wait(3, &BUSY), Duration::from_secs(4));
        assert_eq!(retry_wait(40, &BUSY), RETRY_WAIT_MAX);
        let limited = TranslateError::RateLimited {
            retry_after: Some(Duration::from_secs(600)),
        };
        assert_eq!(retry_wait(1, &limited), RETRY_HINT_MAX);
    }

    #[test]
    fn a_cursor_targets_its_paragraph() {
        let pos = DOC.find("one").unwrap();
        let r = resolve(DOC, Mode::Plain, Action::Polish, pos, pos).unwrap();
        assert_eq!(r.text, "Second one here.");
        assert!(r.whole_paragraph && !r.insert);
        assert!(
            resolve(DOC, Mode::Plain, Action::Edit, 17, 17).is_err(),
            "blank line"
        );
    }

    #[test]
    fn selections_are_trimmed_and_may_span_paragraphs() {
        let a = DOC.find("Second").unwrap() - 2;
        let b = DOC.find("here").unwrap() + 4;
        let r = resolve(DOC, Mode::Plain, Action::Edit, b, a).unwrap();
        assert_eq!(r.text, "Second one here");
        assert!(!r.whole_paragraph, "a part of the paragraph");
        let whole = resolve(DOC, Mode::Plain, Action::Edit, 0, DOC.len()).unwrap();
        assert!(whole.text.starts_with("First") && whole.text.ends_with("Third."));
        assert!(!whole.whole_paragraph);
        assert!(resolve(DOC, Mode::Plain, Action::Edit, 16, 18).is_err());
    }

    #[test]
    fn figures_go_after_the_paragraph() {
        let pos = DOC.find("one").unwrap();
        let r = resolve(DOC, Mode::Plain, Action::Figure, pos, pos).unwrap();
        assert!(r.insert);
        assert_eq!(
            r.range.start,
            DOC.find("Second one here.").unwrap() + "Second one here.".len()
        );
        assert_eq!(r.range.start, r.range.end);
    }

    #[test]
    fn long_documents_are_cut_around_the_target() {
        let long = "word ".repeat(MAX_DOCUMENT_CHARS / 4);
        let mid = long.len() / 2;
        let window = document_window(&long, &(mid..mid + 4));
        assert!(window.starts_with("[…]") && window.len() < long.len());
        assert_eq!(
            window.chars().count(),
            MAX_DOCUMENT_CHARS + "[…]\n\n[…]".chars().count()
        );
        assert_eq!(document_window(DOC, &(0..5)), DOC);
    }

    #[test]
    fn new_text_goes_after_the_paragraph_or_at_an_empty_line() {
        let pos = DOC.find("one").unwrap();
        let r = resolve(DOC, Mode::Plain, Action::Write, pos, pos).unwrap();
        assert!(r.insert && !r.whole_paragraph);
        assert_eq!(r.text, "Second one here.");
        assert_eq!(r.range, r.range.end..r.range.end);
        assert_eq!(r.range.end, DOC.find("\n\nThird").unwrap());
        // On an empty line: there, with nothing before it as the target.
        let blank = "First.\n\n\n\nLast.";
        let at = blank.find("\n\n\n").unwrap() + 2;
        let r = resolve(blank, Mode::Plain, Action::Write, at, at).unwrap();
        assert!(r.insert && r.text.is_empty() && r.range == (at..at));
        // A selection: after its paragraph, not inside it.
        let r = resolve(DOC, Mode::Plain, Action::Write, 0, 6).unwrap();
        assert_eq!(
            (r.text.as_str(), r.range.start),
            ("First", "First paragraph.".len())
        );
    }

    #[test]
    fn new_text_never_goes_inside_a_heading_or_a_figure() {
        let doc = "\\section{Introduction}\\label{sec:intro}\nText.\n\n\\begin{figure}\n\\centering\n\
                   \\caption{A plot.}\n\\end{figure}\n\nAfter.\n\n\\begin{equation}\nx = 1\n\\end{equation}\n\
                   \\end{document}\ntrailing notes\n";
        let mode = Mode::Latex;
        // In a heading's title: after its whole line, closing brace included.
        let pos = doc.find("Intro").unwrap();
        let r = resolve(doc, mode, Action::Write, pos, pos).unwrap();
        assert_eq!(r.range.start, doc.find("\nText.").unwrap());
        let r = resolve(doc, mode, Action::Figure, pos, pos + 5).unwrap();
        assert_eq!(r.range.start, doc.find("\nText.").unwrap());
        // In a figure (its caption or another line): after the whole float.
        let after_float = doc.find("\\end{figure}").unwrap() + "\\end{figure}".len();
        for word in ["plot", "centering"] {
            let pos = doc.find(word).unwrap();
            let r = resolve(doc, mode, Action::Figure, pos, pos).unwrap();
            assert_eq!(r.range.start, after_float, "{word}");
        }
        // In math: after the block.
        let pos = doc.find("x = 1").unwrap();
        let r = resolve(doc, mode, Action::Write, pos, pos).unwrap();
        assert_eq!(
            &doc[r.range.start - "\\end{equation}".len()..r.range.start],
            "\\end{equation}"
        );
        // After \end{document}: refused, nothing there is typeset, even at
        // the very end of the file.
        let pos = doc.find("trailing").unwrap();
        assert!(resolve(doc, mode, Action::Write, pos, pos).is_err());
        assert!(resolve(doc, mode, Action::Write, doc.len(), doc.len()).is_err());
    }

    #[test]
    fn a_float_ends_at_its_own_end() {
        let doc = "Text.\n\n\\begin{figure}\n\\centering\n\\caption{A.}\n\\end{figure}\n\n\
                   \\begin{table}\n\\caption{B.}\n\\begin{tabular}{l}\nx\\\\\n\\end{tabular}\n\\end{table}\n\n\
                   \\begin{tabular}{l}\ny\\\\\n\\end{tabular}\n\n\\begin{figure}\n\\caption{C.}\n\\end{figure}\n\nEnd.\n";
        let mode = Mode::Latex;
        let end = |env_end: &str, nth: usize| {
            doc.match_indices(env_end)
                .nth(nth)
                .map(|(at, _)| at + env_end.len())
                .unwrap()
        };
        let at = |word: &str| {
            let pos = doc.find(word).unwrap();
            resolve(doc, mode, Action::Figure, pos, pos)
                .unwrap()
                .range
                .start
        };
        // The first figure, not past the table after it.
        assert_eq!(at("centering"), end("\\end{figure}", 0));
        assert_eq!(at("A."), end("\\end{figure}", 0));
        // The table, from its caption or its tabular.
        assert_eq!(at("B."), end("\\end{table}", 0));
        assert_eq!(at("x\\\\"), end("\\end{table}", 0));
        // A tabular of its own: after it, not after the next figure.
        assert_eq!(at("y\\\\"), end("\\end{tabular}", 1));
    }

    #[test]
    fn a_reference_paper_gives_its_prose() {
        let paper = "\\documentclass{article}\n\\usepackage{x}\n% preamble note\n\\begin{document}\n\
                     \\section{Introduction}\n% TODO cut this\n\
                     Cleaning costs 50\\% of the time. % why\n\n\n\n\
                     We present UniClean.\n\\end{document}\nafter\n";
        let (text, truncated) = reference_text("uniclean.tex", paper);
        assert!(!truncated);
        assert_eq!(
            text,
            "\\section{Introduction}\nCleaning costs 50\\% of the time.\n\nWe present UniClean."
        );
        // A section file without a preamble, and plain text, as they are.
        let (text, _) = reference_text("intro.tex", "Plain prose.\n% note\nMore.");
        assert_eq!(text, "Plain prose.\nMore.");
        let (text, _) = reference_text("notes.md", "# Notes\n100% sure.");
        assert_eq!(text, "# Notes\n100% sure.");
        // A very long paper is cut.
        let long = "word ".repeat(MAX_REFERENCE_CHARS);
        let (text, truncated) = reference_text("long.txt", &long);
        assert!(truncated && text.chars().count() == MAX_REFERENCE_CHARS);
    }
}

#[cfg(test)]
#[path = "figure_tests.rs"]
mod figure_tests;
