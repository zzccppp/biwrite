//! IPC commands. Payloads carry document text, segment layout/state and file
//! names only — never credentials.

use std::path::PathBuf;

use biwrite_core::lang::written_in;
use biwrite_core::{Mode, SegmentId};
use biwrite_engine::Snapshot;

use crate::error::CommandError;
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
    let _no_save_meanwhile = state.save_lock.lock().await;
    let view = open_path(state, path).await?;
    refresh_title(window, state);
    Ok(view)
}

/// [`load`] without the window title.
pub(crate) async fn open_path(state: &AppState, path: PathBuf) -> CommandResult<SessionView> {
    let file = files::read_text_file(path.clone()).await?;
    // A file in the other language by the same name is its mirror.
    let mirror = match pairing::counterpart(&path) {
        Some(m) => files::read_text_file(m.clone()).await.ok().map(|f| (m, f)),
        None => None,
    };
    let snapshot = pairing::open(state, path, file, mirror);
    Ok(state.session_view(snapshot))
}

/// Save to the current path (asks for one if the document is untitled).
/// `text` is the editor's, for `document` (see [`AppState::sync_text`]).
#[tauri::command]
pub async fn save_file(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
    document: u64,
) -> CommandResult<Option<SavedView>> {
    let _one_at_a_time = state.save_lock.lock().await;
    state.sync_text(document, &text)?;
    let own = home_text(&state, text)?;
    let current = state.file().path.clone();
    let path = match current {
        Some(path) => path,
        None => match files::pick_save(&app, &window, None).await {
            Some(path) => path,
            None => return Ok(None),
        },
    };
    save_to(&window, &state, path, own, None).await.map(Some)
}

/// Save under a new name.
#[tauri::command]
pub async fn save_file_as(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
    document: u64,
) -> CommandResult<Option<SavedView>> {
    let _one_at_a_time = state.save_lock.lock().await;
    state.sync_text(document, &text)?;
    let own = home_text(&state, text)?;
    let current = state.file().path.clone();
    let Some(path) = files::pick_save(&app, &window, current.as_deref()).await else {
        return Ok(None);
    };
    // A pair keeps its old files: the mirror follows under the new name.
    let mirror_to = {
        let fs = state.file();
        match (&current, fs.pair.as_ref()) {
            (Some(old), Some(pair)) if *old != path => {
                Some(pairing::mirror_path_for(old, &path, &pair.path))
            }
            _ => None,
        }
    };
    save_to(&window, &state, path, own, mirror_to)
        .await
        .map(Some)
}

/// The open file's own text, what saving writes: the editor's, or, while
/// the other language is edited (after a swap), composed from the
/// translations, refused while a paragraph is still pending. With a pair,
/// the editor holds the file's own language and the mirror follows.
///
/// The engine holds `editor_text` already ([`AppState::sync_text`]).
pub(crate) fn home_text(state: &AppState, editor_text: String) -> CommandResult<String> {
    let own = {
        let fs = state.file();
        fs.pair.is_some() || state.engine.direction() == fs.home
    };
    if own {
        Ok(editor_text)
    } else {
        Ok(state.engine.compose_target()?)
    }
}

async fn save_to(
    window: &WebviewWindow,
    state: &AppState,
    path: PathBuf,
    text: String,
    mirror_to: Option<PathBuf>,
) -> CommandResult<SavedView> {
    let saved = write_document(state, path, text, mirror_to).await;
    refresh_title(window, state);
    saved
}

/// Write the document's own `text` to `path` (and the paired file to
/// `mirror_to`, or where it is); [`save_to`] without the window title. The
/// caller holds `save_lock`, so the document can't change meanwhile.
pub(crate) async fn write_document(
    state: &AppState,
    path: PathBuf,
    text: String,
    mirror_to: Option<PathBuf>,
) -> CommandResult<SavedView> {
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
    log::info!("saved {}", display_name(Some(&path)));
    let mirror = match mirror_to {
        Some(dest) => pairing::save_mirror_as(state, dest).await?,
        None => pairing::save_mirror(state).await?,
    };
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
    document: u64,
    keep: Option<bool>,
) -> CommandResult<SessionView> {
    let _no_save_meanwhile = state.save_lock.lock().await;
    state.sync_text(document, &text)?;
    let view = swap(&state, text, keep.unwrap_or(false));
    refresh_title(&window, &state);
    view
}

/// [`swap_languages`] without the window title.
pub(crate) fn swap(state: &AppState, text: String, keep: bool) -> CommandResult<SessionView> {
    if state.file().pair.is_some() {
        return pairing::swap(state, text);
    }
    // `keep`: swap now; untranslated paragraphs keep their text. Not back
    // to the file's own language: those paragraphs would put the other
    // language into the file, so that swap waits for every translation.
    let keep = keep && state.engine.direction().flipped() != state.file().home;
    let swapped = if keep {
        state.engine.swap_keeping_untranslated(text.clone())?
    } else {
        state.engine.swap(text.clone())?
    };
    log::info!(
        "swapped languages: now {}",
        swapped.snapshot.direction.as_str()
    );
    // Dirty means "the file's own language differs from the file on disk".
    {
        let mut fs = state.file();
        let own = if swapped.snapshot.direction == fs.home {
            &swapped.text
        } else {
            &text
        };
        fs.dirty = own != fs.file.text();
    }
    Ok(state.session_view(swapped.snapshot))
}

/// Take the open file to be written in the other language than assumed:
/// its own text becomes the source of the other direction (a Chinese file
/// opened as English is translated into English from then on). While the
/// other language is edited, the file's own text comes back first, exactly.
/// With `only_if_needed`, this happens only when the text plainly reads as
/// the other language, and `None` means nothing changed. Not for a pair,
/// whose files name their languages.
#[tauri::command]
pub async fn retarget_language(
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
    document: u64,
    only_if_needed: bool,
) -> CommandResult<Option<SessionView>> {
    let _no_save_meanwhile = state.save_lock.lock().await;
    state.sync_text(document, &text)?;
    let Some(snapshot) = retarget(&state, text, only_if_needed)? else {
        return Ok(None);
    };
    refresh_title(&window, &state);
    Ok(Some(state.session_view(snapshot)))
}

/// See [`retarget_language`].
pub(crate) fn retarget(
    state: &AppState,
    text: String,
    only_if_needed: bool,
) -> CommandResult<Option<Snapshot>> {
    if state.file().pair.is_some() {
        if only_if_needed {
            return Ok(None);
        }
        return Err(CommandError::Settings(
            "This document is paired with its translation, and each file keeps its own language."
                .into(),
        ));
    }
    let home = state.file().home;
    let mode = state.engine.mode();
    if only_if_needed {
        let editing_own = state.engine.direction() == home;
        if !editing_own || written_in(&text, mode) != Some(home.flipped()) {
            return Ok(None);
        }
    }
    let own = home_text(state, text)?;
    let new_home = home.flipped();
    let snapshot = state
        .engine
        .load_known(own.clone(), mode, new_home, Vec::new());
    {
        let mut fs = state.file();
        fs.home = new_home;
        fs.dirty = own != fs.file.text();
    }
    log::info!("the document is read as {} now", new_home.as_str());
    Ok(Some(snapshot))
}

/// Translate what is left (see [`biwrite_engine::Engine::continue_translation`]):
/// while the other language is edited, also the paragraphs still written in
/// the file's language. Returns how many paragraphs were taken up.
#[tauri::command]
pub async fn continue_translation(
    state: State<'_, AppState>,
    text: String,
    document: u64,
) -> CommandResult<usize> {
    state.sync_text(document, &text)?;
    let other_language = {
        let fs = state.file();
        fs.pair.is_none() && state.engine.direction() != fs.home
    };
    Ok(state.engine.continue_translation(other_language))
}

/// New editor text after the debounce: re-segment, align, schedule work.
/// `None` if the editor sent it for an earlier document (an update that
/// crossed a swap or an open): it is dropped.
#[tauri::command]
pub async fn update_document(
    state: State<'_, AppState>,
    text: String,
    document: u64,
) -> CommandResult<Option<Snapshot>> {
    Ok(state.engine.update_in(document, text).ok())
}

/// Change the mode; `None` as for [`update_document`].
#[tauri::command]
pub async fn set_mode(
    state: State<'_, AppState>,
    mode: Mode,
    text: String,
    document: u64,
) -> CommandResult<Option<Snapshot>> {
    Ok(state.engine.set_mode_in(document, mode, text).ok())
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

/// Open the user guide, in `lang` ("zh" or "en"), on GitHub in the browser
/// (where it can be read or downloaded; the app does not carry it).
#[tauri::command]
pub async fn open_manual(lang: String) -> CommandResult<()> {
    files::open_url(files::manual_url(&lang))
}

/// Open one of BiWrite's known links (see `files::known_link`).
#[tauri::command]
pub async fn open_link(name: String) -> CommandResult<()> {
    match files::known_link(&name) {
        Some(url) => files::open_url(url),
        None => Err(CommandError::Settings(format!("unknown link {name}"))),
    }
}
