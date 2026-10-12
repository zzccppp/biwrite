//! Open files changed on disk by other programs (another editor, git, a
//! sync service). The document and its paired file are compared with what
//! the app last read or wrote of them. When the window gains focus, the
//! frontend asks whether to load a changed file again; not loaded, nothing
//! changes (and that version is not asked about again). Saving never writes
//! over a change made elsewhere unless the author says so
//! ([`CommandError::ChangedOnDisk`]; the paired file is left alone, see
//! `pairing::save_mirror_with`).

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use biwrite_core::TextFile;
use serde::{Deserialize, Serialize};
use tauri::{State, WebviewWindow};

use crate::commands::{open_path, refresh_title};
use crate::error::{CommandError, CommandResult};
use crate::files;
use crate::pairing;
use crate::state::{AppState, SessionView};

/// The bytes of the file at `path`, `None` if it can't be read (gone).
async fn read_bytes(path: &Path) -> Option<Vec<u8>> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || std::fs::read(path).ok())
        .await
        .ok()
        .flatten()
}

/// Whether the file at `path` is neither `known` (its text as the app last
/// read or wrote it) nor `ignored` (a version the author chose not to load).
/// A file that is gone or can't be read is not (saving puts it back, or
/// reports why it can't); one that is no longer UTF-8 is.
pub(crate) async fn changed_on_disk(path: &Path, known: &str, ignored: Option<&[u8]>) -> bool {
    let Some(bytes) = read_bytes(path).await else {
        return false;
    };
    if ignored == Some(bytes.as_slice()) {
        return false;
    }
    !TextFile::decode(bytes).is_ok_and(|disk| disk.text() == known)
}

/// Which of the open files changed on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskView {
    /// The open document.
    pub document: bool,
    /// Its paired file.
    pub mirror: bool,
    /// Stamps of the versions reported (see [`stamp`]), so that ignoring one
    /// never ignores a later one the author was not asked about.
    #[serde(default)]
    pub document_stamp: String,
    #[serde(default)]
    pub mirror_stamp: String,
}

/// A stamp of `bytes`, to tell the version on disk reported from a later one.
fn stamp(bytes: &[u8]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The stamp of the file at `path` if it changed (see [`changed_on_disk`]).
async fn changed_stamp(path: &Path, known: &str, ignored: Option<&[u8]>) -> Option<String> {
    let bytes = read_bytes(path).await?;
    changed_on_disk(path, known, ignored)
        .await
        .then(|| stamp(&bytes))
}

/// The open document's path and paired file's, with what the app knows of
/// each (see [`changed_on_disk`]). Only a paired file that exists counts.
type Known = (PathBuf, String, Option<Vec<u8>>);

fn known(state: &AppState) -> (Option<Known>, Option<Known>) {
    let fs = state.file();
    let document = fs
        .path
        .clone()
        .map(|p| (p, fs.file.text().to_owned(), fs.ignored.clone()));
    let mirror = fs
        .pair
        .as_ref()
        .filter(|p| p.exists)
        .map(|p| (p.path.clone(), p.file.text().to_owned(), p.ignored.clone()));
    (document, mirror)
}

/// Which open files changed on disk since the app last read or wrote them
/// (to a version the author did not choose to ignore). `None` if
/// `document` is no longer the open one.
#[tauri::command]
pub async fn check_disk(
    state: State<'_, AppState>,
    document: u64,
) -> CommandResult<Option<DiskView>> {
    check(&state, document).await
}

/// See [`check_disk`].
pub(crate) async fn check(state: &AppState, document: u64) -> CommandResult<Option<DiskView>> {
    // Never halfway through a write of ours.
    let _no_save_meanwhile = state.save_lock.lock().await;
    if state.engine.document() != document {
        return Ok(None);
    }
    let (doc, mirror) = known(state);
    let mut view = DiskView::default();
    if let Some((path, known, ignored)) = doc
        && let Some(stamp) = changed_stamp(&path, &known, ignored.as_deref()).await
    {
        view.document = true;
        view.document_stamp = stamp;
    }
    if let Some((path, known, ignored)) = mirror
        && let Some(stamp) = changed_stamp(&path, &known, ignored.as_deref()).await
    {
        view.mirror = true;
        view.mirror_stamp = stamp;
    }
    Ok(Some(view))
}

/// The author does not load the versions on disk of the files in `which`
/// that changed (as [`check_disk`] reported them, by their stamps): nothing
/// changes, and they are not reported again (a later change is). Saving
/// still asks before writing over them.
#[tauri::command]
pub async fn ignore_disk(
    state: State<'_, AppState>,
    document: u64,
    which: DiskView,
) -> CommandResult<()> {
    ignore(&state, document, which).await
}

/// See [`ignore_disk`].
pub(crate) async fn ignore(state: &AppState, document: u64, which: DiskView) -> CommandResult<()> {
    let _no_save_meanwhile = state.save_lock.lock().await;
    if state.engine.document() != document {
        return Err(biwrite_engine::EngineError::Stale.into());
    }
    let (doc, mirror) = known(state);
    // Only the version reported, if it is still there.
    let reported = |bytes: &Option<Vec<u8>>, wanted: &str| {
        bytes.as_deref().is_some_and(|b| stamp(b) == wanted)
    };
    let doc = match doc.filter(|_| which.document) {
        Some((path, ..)) => {
            let bytes = read_bytes(&path).await;
            Some((path, bytes))
        }
        None => None,
    }
    .filter(|(_, bytes)| reported(bytes, &which.document_stamp));
    let mirror = match mirror.filter(|_| which.mirror) {
        Some((path, ..)) => {
            let bytes = read_bytes(&path).await;
            Some((path, bytes))
        }
        None => None,
    }
    .filter(|(_, bytes)| reported(bytes, &which.mirror_stamp));
    let mut fs = state.file();
    if let Some((path, bytes)) = doc
        && fs.path.as_ref() == Some(&path)
    {
        fs.ignored = bytes;
    }
    if let Some((path, bytes)) = mirror
        && let Some(pair) = fs.pair.as_mut().filter(|p| p.path == path)
    {
        pair.ignored = bytes;
    }
    Ok(())
}

/// The open document's text as it is on disk now, to put in place of the
/// editor's text while the file's own language is edited. Nothing changes
/// here until the editor holds it ([`adopt_disk`]). `None` if `document` is
/// no longer the open one or is untitled.
#[tauri::command]
pub async fn reload_document(
    state: State<'_, AppState>,
    document: u64,
) -> CommandResult<Option<String>> {
    reload(&state, document).await
}

/// See [`reload_document`].
pub(crate) async fn reload(state: &AppState, document: u64) -> CommandResult<Option<String>> {
    let _no_save_meanwhile = state.save_lock.lock().await;
    if state.engine.document() != document {
        return Ok(None);
    }
    if !state.editing_own_language() {
        return Err(CommandError::Settings(
            "The other language is edited: open the file again instead.".into(),
        ));
    }
    let Some(path) = state.file().path.clone() else {
        return Ok(None);
    };
    let disk = files::read_text_file(path).await?;
    Ok(Some(disk.text().to_owned()))
}

/// The editor holds `text`, read by [`reload_document`]: if the file on
/// disk still reads so, it is what the app knows of the file from now on.
/// Returns whether it was (else it changed again, and is reported again).
#[tauri::command]
pub async fn adopt_disk(
    state: State<'_, AppState>,
    document: u64,
    text: String,
) -> CommandResult<bool> {
    adopt(&state, document, &text).await
}

/// See [`adopt_disk`].
pub(crate) async fn adopt(state: &AppState, document: u64, text: &str) -> CommandResult<bool> {
    let _no_save_meanwhile = state.save_lock.lock().await;
    if state.engine.document() != document {
        return Ok(false);
    }
    let Some(path) = state.file().path.clone() else {
        return Ok(false);
    };
    let Ok(disk) = files::read_text_file(path.clone()).await else {
        return Ok(false);
    };
    if disk.text() != text {
        return Ok(false);
    }
    let mut fs = state.file();
    if fs.path.as_ref() != Some(&path) {
        return Ok(false);
    }
    fs.file = disk;
    fs.ignored = None;
    log::info!("loaded {} again from disk", fs.display_name());
    Ok(true)
}

/// Open the document again from disk, with its paired file, as Open does
/// (unsaved changes are let go). `None` if `document` is no longer the open
/// one or is untitled.
#[tauri::command]
pub async fn reopen_file(
    window: WebviewWindow,
    state: State<'_, AppState>,
    document: u64,
) -> CommandResult<Option<SessionView>> {
    let view = reopen(&state, document).await?;
    refresh_title(&window, &state);
    Ok(view)
}

/// See [`reopen_file`].
pub(crate) async fn reopen(state: &AppState, document: u64) -> CommandResult<Option<SessionView>> {
    let _no_save_meanwhile = state.save_lock.lock().await;
    if state.engine.document() != document {
        return Ok(None);
    }
    let Some(path) = state.file().path.clone() else {
        return Ok(None);
    };
    open_path(state, path).await.map(Some)
}

/// Pair the open document again with its paired file as it is on disk now;
/// the editor keeps its text (`text`, for `document`). The document is
/// paired as saved, and its unsaved edits go on top, so their paragraphs are
/// translated afresh; so are the saved paragraphs whose translations the
/// paired file lacked (a refused write). Changes of the paired file not
/// written yet are let go. `None` without a pair, or if `document` is no
/// longer the open one.
#[tauri::command]
pub async fn reload_mirror(
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
    document: u64,
) -> CommandResult<Option<SessionView>> {
    let view = repair(&state, text, document).await?;
    refresh_title(&window, &state);
    Ok(view)
}

/// See [`reload_mirror`].
pub(crate) async fn repair(
    state: &AppState,
    text: String,
    document: u64,
) -> CommandResult<Option<SessionView>> {
    let Some(mirror_path) = state.file().pair.as_ref().map(|p| p.path.clone()) else {
        return Ok(None);
    };
    let mirror = files::read_text_file(mirror_path.clone()).await?;
    let _no_save_meanwhile = state.save_lock.lock().await;
    if state.engine.document() != document {
        return Ok(None);
    }
    state.sync_text(document, &text)?;
    if !state.editing_own_language() {
        return Ok(None);
    }
    let (path, saved, fresh) = {
        let fs = state.file();
        match (
            fs.path.clone(),
            fs.pair.as_ref().filter(|p| p.path == mirror_path),
        ) {
            (Some(path), Some(pair)) => (
                path,
                fs.file.text().to_owned(),
                pairing::behind_paragraphs(state, pair),
            ),
            _ => return Ok(None),
        }
    };
    let mode = state.engine.mode();
    // The engine changes only if the two pair up.
    let (snapshot, pair) =
        pairing::load_paired_except(state, &path, &saved, mode, mirror_path, mirror, &fresh)?;
    let dirty = text != saved;
    let mut pair = pair;
    // What it lacked goes in once translated.
    pair.behind = !fresh.is_empty();
    {
        let mut fs = state.file();
        fs.pair = Some(pair);
        fs.home = snapshot.direction;
        fs.dirty = dirty;
    }
    let snapshot = if dirty {
        state.engine.update(text);
        state.engine.snapshot()
    } else {
        snapshot
    };
    log::info!("paired again with the paired file as on disk");
    let mut view = state.session_view(snapshot);
    view.dirty = dirty;
    Ok(Some(view))
}
