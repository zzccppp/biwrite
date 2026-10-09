//! Writing assistant commands. A polish, edit, ask or figure job runs in the
//! background: `assist_start` answers at once with the job id and the
//! resolved target, and the job reports through `assist` events (partial
//! text, the result, or a failure). The editor applies an approved revision
//! itself; `assist_offer` first gives the engine the approved translation.
//!
//! Payloads carry document text, answers and attachment previews, never
//! keys. Reference images are read in Rust from files picked in a native
//! dialog; the webview only names them by id.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use biwrite_core::assist::{self, Action, Answer, DiffPart, Scope};
use biwrite_core::glossary;
use biwrite_core::utf16::{byte_to_utf16, utf16_to_byte};
use biwrite_core::{Mode, Protector};
use biwrite_engine::TokenUsage;
use biwrite_providers::{ChatModel, ChatRequest, ImageInput};
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
/// Minimum interval between streamed updates of one job.
const PARTIAL_EVERY: Duration = Duration::from_millis(80);

/// Running jobs and attached reference images.
#[derive(Default)]
pub struct AssistState {
    next_job: AtomicU64,
    jobs: Mutex<HashMap<u64, AbortHandle>>,
    next_image: AtomicU64,
    images: Mutex<HashMap<u64, Attachment>>,
}

struct Attachment {
    image: ImageInput,
}

impl AssistState {
    fn jobs(&self) -> std::sync::MutexGuard<'_, HashMap<u64, AbortHandle>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
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
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssistEvent {
    Partial { id: u64, text: String },
    Done { id: u64, result: AssistResult },
    Failed { id: u64, message: String },
}

fn emit(app: &AppHandle, event: &AssistEvent) {
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

/// Resolve the cursor or selection (byte offsets) into a target.
fn resolve(text: &str, mode: Mode, action: Action, a: usize, b: usize) -> Result<Resolved, String> {
    let (a, b) = (a.min(b), a.max(b));
    if a == b {
        return match (assist::paragraph_at(text, mode, a), action) {
            (Some(p), Action::Figure) => Ok(Resolved {
                range: p.end..p.end,
                text: text[p].to_owned(),
                whole_paragraph: false,
                insert: true,
            }),
            (Some(p), _) => Ok(Resolved {
                text: text[p.clone()].to_owned(),
                range: p,
                whole_paragraph: true,
                insert: false,
            }),
            (None, Action::Figure) => Ok(Resolved {
                range: a..a,
                text: String::new(),
                whole_paragraph: false,
                insert: true,
            }),
            (None, _) => Err("put the cursor in a paragraph or select text first".into()),
        };
    }
    let selected = &text[a..b];
    let start = a + (selected.len() - selected.trim_start().len());
    let end = b - (selected.len() - selected.trim_end().len());
    if start >= end {
        return Err("the selection is empty".into());
    }
    if action == Action::Figure {
        return Ok(Resolved {
            range: end..end,
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
    let target = resolve(text, mode, request.action, a, b).map_err(CommandError::Settings)?;
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
        protector: prompt.protector,
        model,
        chat,
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
    protector: Protector,
    model: Arc<dyn ChatModel>,
    chat: ChatRequest,
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
    let outcome = job.model.complete(&job.chat, &on_partial).await;
    let event = match outcome {
        Err(e) => AssistEvent::Failed {
            id: job.id,
            message: e.to_string(),
        },
        Ok(out) => match assist::parse(job.action, &out.text, &job.protector) {
            Err(e) => AssistEvent::Failed {
                id: job.id,
                message: e.to_string(),
            },
            Ok(answer) => {
                let diff = match (&answer.revision, job.action) {
                    (Some(revision), Action::Polish | Action::Edit | Action::Ask) => {
                        assist::diff(&job.original, revision)
                    }
                    _ => Vec::new(),
                };
                AssistEvent::Done {
                    id: job.id,
                    result: AssistResult {
                        answer,
                        diff,
                        usage: out.usage,
                        duration_ms: started.elapsed().as_millis() as u64,
                    },
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
}
