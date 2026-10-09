//! IPC commands. Payloads carry document text, segment layout/state and file
//! names only — never credentials.

use std::path::PathBuf;

use biwrite_core::{Direction, Mode, SegmentId};
use biwrite_engine::Snapshot;
use tauri::{AppHandle, State, WebviewWindow};

use crate::error::CommandResult;
use crate::files;
use crate::pairing;
use crate::state::{AppState, SavedView, SessionView, display_name};

/// Cosmetic, so a failure is logged rather than failing the command (which
/// could leave frontend and backend on different documents).
fn refresh_title(window: &WebviewWindow, state: &AppState) {
    let title = state.file().window_title();
    if let Err(e) = window.set_title(&title) {
        log::warn!("failed to set window title: {e}");
    }
}

/// Current document and all segment states (used on startup and reload).
#[tauri::command]
pub async fn get_session(state: State<'_, AppState>) -> CommandResult<SessionView> {
    let snapshot = state.engine.snapshot();
    Ok(state.session_view(snapshot))
}

/// Show the Open dialog and load the chosen file. `None` if cancelled.
#[tauri::command]
pub async fn open_file(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CommandResult<Option<SessionView>> {
    if state.is_dirty() && !files::confirm_discard(&app, &window).await {
        return Ok(None);
    }
    let Some(path) = files::pick_open(&app, &window).await else {
        return Ok(None);
    };
    load(&window, &state, path).await.map(Some)
}

/// Make `path` the open document (the caller has dealt with unsaved
/// changes).
pub(crate) async fn load(
    window: &WebviewWindow,
    state: &AppState,
    path: PathBuf,
) -> CommandResult<SessionView> {
    let file = files::read_text_file(path.clone()).await?;
    // A file in the other language by the same name is its mirror.
    let mirror = match pairing::counterpart(&path) {
        Some(m) => files::read_text_file(m.clone()).await.ok().map(|f| (m, f)),
        None => None,
    };
    let snapshot = pairing::open(state, path, file, mirror);
    refresh_title(window, state);
    Ok(state.session_view(snapshot))
}

/// Save to the current path (asks for one if the document is untitled).
#[tauri::command]
pub async fn save_file(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
) -> CommandResult<Option<SavedView>> {
    let english = saved_text(&state, text)?;
    let current = state.file().path.clone();
    let path = match current {
        Some(path) => path,
        None => match files::pick_save(&app, &window, None).await {
            Some(path) => path,
            None => return Ok(None),
        },
    };
    save_to(&window, &state, path, english).await.map(Some)
}

/// Save under a new name.
#[tauri::command]
pub async fn save_file_as(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
) -> CommandResult<Option<SavedView>> {
    let english = saved_text(&state, text)?;
    let current = state.file().path.clone();
    let Some(path) = files::pick_save(&app, &window, current.as_deref()).await else {
        return Ok(None);
    };
    save_to(&window, &state, path, english).await.map(Some)
}

/// What is written to the open file. Without a pair the file is always
/// English: while editing Chinese, it is composed from the current
/// translations (and refused if any paragraph is still pending). With a
/// pair, each file holds its own language: the editor's text is written,
/// and the mirror follows.
fn saved_text(state: &AppState, editor_text: String) -> CommandResult<String> {
    if state.file().pair.is_some() {
        if state.engine.text() != editor_text {
            state.engine.update(editor_text.clone());
        }
        return Ok(editor_text);
    }
    match state.engine.direction() {
        Direction::EnZh => Ok(editor_text),
        Direction::ZhEn => {
            // Normally a no-op: the frontend flushes edits before saving.
            state.engine.update(editor_text);
            Ok(state.engine.compose_target()?)
        }
    }
}

async fn save_to(
    window: &WebviewWindow,
    state: &AppState,
    path: PathBuf,
    text: String,
) -> CommandResult<SavedView> {
    let _one_save_at_a_time = state.save_lock.lock().await;
    let note = state.doc_note();
    // Only the English source is written; unedited text yields the original bytes.
    let bytes = state.file().file.encode(&text);
    files::write_file_atomic(path.clone(), bytes.clone()).await?;
    {
        let mut fs = state.file();
        fs.file = fs.file.saved(text, bytes);
        fs.path = Some(path.clone());
        fs.dirty = false;
    }
    // Keep the document note when an untitled draft is saved or renamed;
    // otherwise use the note stored for the new path.
    if !note.is_empty() && state.doc_note().is_empty() {
        state.set_doc_note(note)?;
    } else {
        state.apply_doc_note();
    }
    refresh_title(window, state);
    log::info!("saved {}", display_name(Some(&path)));
    let mirror = pairing::save_mirror(state).await?;
    Ok(SavedView {
        name: display_name(Some(&path)),
        suggested_mode: Mode::from_path(&path),
        path: path.display().to_string(),
        mirror,
    })
}

/// Swap languages: the translations become the editable text and the
/// current text becomes their translation. Unchanged paragraphs round-trip
/// exactly. Fails if any paragraph is not translated yet, unless `keep`
/// asks to swap now with those paragraphs unchanged.
#[tauri::command]
pub async fn swap_languages(
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
    keep: Option<bool>,
) -> CommandResult<SessionView> {
    if state.file().pair.is_some() {
        let view = pairing::swap(&state, text)?;
        refresh_title(&window, &state);
        return Ok(view);
    }
    // `keep`: swap now; untranslated paragraphs keep their text.
    let swapped = if keep.unwrap_or(false) {
        state.engine.swap_keeping_untranslated(text.clone())?
    } else {
        state.engine.swap(text.clone())?
    };
    log::info!(
        "swapped languages: now {}",
        swapped.snapshot.direction.as_str()
    );
    // Dirty means "the English differs from the file on disk".
    let english = match swapped.snapshot.direction {
        Direction::EnZh => &swapped.text,
        Direction::ZhEn => &text,
    };
    {
        let mut fs = state.file();
        fs.dirty = english != fs.file.text();
    }
    refresh_title(&window, &state);
    Ok(state.session_view(swapped.snapshot))
}

/// New editor text after the debounce: re-segment, align, schedule work.
#[tauri::command]
pub async fn update_document(state: State<'_, AppState>, text: String) -> CommandResult<Snapshot> {
    Ok(state.engine.update(text))
}

#[tauri::command]
pub async fn set_mode(
    state: State<'_, AppState>,
    mode: Mode,
    text: String,
) -> CommandResult<Snapshot> {
    Ok(state.engine.set_mode(mode, text))
}

#[tauri::command]
pub async fn retranslate_segment(state: State<'_, AppState>, id: u64) -> CommandResult<()> {
    state.engine.retranslate(SegmentId(id))?;
    Ok(())
}

#[tauri::command]
pub async fn retranslate_all(state: State<'_, AppState>) -> CommandResult<()> {
    state.engine.retranslate_all();
    Ok(())
}

#[tauri::command]
pub async fn set_auto_translate(state: State<'_, AppState>, on: bool) -> CommandResult<()> {
    state.engine.set_auto_translate(on);
    Ok(())
}

/// The frontend reports when the editor content starts/stops differing from
/// the saved file (drives the title marker and close confirmation).
#[tauri::command]
pub async fn set_dirty(
    window: WebviewWindow,
    state: State<'_, AppState>,
    dirty: bool,
) -> CommandResult<()> {
    state.file().dirty = dirty;
    refresh_title(&window, &state);
    Ok(())
}

/// Open one of BiWrite's known links (see `files::known_link`).
#[tauri::command]
pub async fn open_link(name: String) -> CommandResult<()> {
    match files::known_link(&name) {
        Some(url) => files::open_url(url),
        None => Err(crate::error::CommandError::Settings(format!(
            "unknown link {name}"
        ))),
    }
}
