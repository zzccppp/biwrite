//! IPC commands. Payloads carry document text, segment layout/state and file
//! names only — never credentials.

use std::path::PathBuf;

use biwrite_core::lang::written_in;
use biwrite_core::{Mode, SegmentId};
use biwrite_engine::Snapshot;

use crate::error::CommandError;
use tauri::{AppHandle, Runtime, WebviewWindow};

use crate::error::CommandResult;
use crate::files;
use crate::pairing;
use crate::state::{Doc, Document, SavedView, SessionView, display_name};
use crate::windows;

/// Cosmetic, so a failure is logged rather than failing the command (which
/// could leave frontend and backend on different documents).
pub(crate) fn refresh_title<R: Runtime>(window: &WebviewWindow<R>, state: &Document) {
    let title = state.file().window_title();
    if let Err(e) = window.set_title(&title) {
        log::warn!("failed to set window title: {e}");
    }
}

/// Current document and all segment states (used on startup and reload).
#[tauri::command]
pub async fn get_session(state: Doc) -> CommandResult<SessionView> {
    let snapshot = state.engine.snapshot();
    Ok(state.session_view(snapshot))
}

/// Show the Open dialog and open the chosen file (see
/// [`windows::open_from`]). The view if it was loaded in this window.
#[tauri::command]
pub async fn open_file(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
) -> CommandResult<Option<SessionView>> {
    let Some(path) = files::pick_open(&app, &window).await else {
        return Ok(None);
    };
    Ok(windows::open_from(&app, &window, &state, path).await?.view)
}

/// Make `path` the open document (the caller has dealt with unsaved
/// changes).
pub(crate) async fn load<R: Runtime>(
    window: &WebviewWindow<R>,
    state: &Document,
    path: PathBuf,
) -> CommandResult<SessionView> {
    let _no_save_meanwhile = state.save_lock.lock().await;
    let view = open_path(state, path).await?;
    refresh_title(window, state);
    Ok(view)
}

/// [`load`] without the window title.
pub(crate) async fn open_path(state: &Document, path: PathBuf) -> CommandResult<SessionView> {
    let file = files::read_text_file(path.clone()).await?;
    // The file the author paired it with, or one in the other language by
    // the same name, is its mirror, unless another window has it open (or
    // is opening it).
    let mirror = match pairing::mirror_path(state, &path) {
        Some(m) if state.window_with(&m, &state.label).is_some() || state.is_opening(&m) => {
            log::info!(
                "{} is open in another window: not paired",
                display_name(Some(&m))
            );
            None
        }
        Some(m) => files::read_text_file(m.clone()).await.ok().map(|f| (m, f)),
        None => None,
    };
    let snapshot = pairing::open(state, path, file, mirror);
    Ok(state.session_view(snapshot))
}

/// Save to the current path (asks for one if the document is untitled).
/// `text` is the editor's, for `document` (see [`AppState::sync_text`]).
/// A file changed on disk since it was read is not written over unless
/// `overwrite` ([`CommandError::ChangedOnDisk`]), nor its paired file unless
/// `overwrite_mirror` (`MirrorProblem::ChangedOnDisk`).
#[tauri::command]
pub async fn save_file(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
    text: String,
    document: u64,
    overwrite: bool,
    overwrite_mirror: bool,
) -> CommandResult<Option<SavedView>> {
    let _one_at_a_time = state.save_lock.lock().await;
    state.sync_text(document, &text)?;
    let own = home_text(&state, text)?;
    let other_language = state.left_in_other_language();
    let current = state.file().path.clone();
    let path = match current {
        Some(path) => path,
        None => match files::pick_save(&app, &window, None).await {
            Some(path) => {
                not_open_elsewhere(&state, [&path])?;
                path
            }
            None => return Ok(None),
        },
    };
    let over = Overwrite {
        document: overwrite,
        mirror: overwrite_mirror,
    };
    save_to(&window, &state, path, own, None, other_language, over)
        .await
        .map(Some)
}

/// A save never writes over a file another window has open.
pub(crate) fn not_open_elsewhere<'a>(
    state: &Document,
    targets: impl IntoIterator<Item = &'a PathBuf>,
) -> CommandResult<()> {
    for target in targets {
        if state.window_with(target, &state.label).is_some() {
            return Err(CommandError::Settings(format!(
                "{} is open in another window: close it there first",
                display_name(Some(target))
            )));
        }
    }
    Ok(())
}

/// Save under a new name.
#[tauri::command]
pub async fn save_file_as(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
    text: String,
    document: u64,
) -> CommandResult<Option<SavedView>> {
    let _one_at_a_time = state.save_lock.lock().await;
    state.sync_text(document, &text)?;
    let own = home_text(&state, text)?;
    let other_language = state.left_in_other_language();
    let current = state.file().path.clone();
    // A part's root, which reads the part under its old name.
    let root = crate::latex_commands::project(&state)
        .ok()
        .filter(|p| p.root != p.path)
        .map(|p| (p.root, p.root_text));
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
    not_open_elsewhere(&state, std::iter::once(&path).chain(mirror_to.as_ref()))?;
    // Chosen in the dialog, which asked before replacing a file.
    let over = Overwrite {
        document: true,
        mirror: false,
    };
    let saved = save_to(
        &window,
        &state,
        path.clone(),
        own,
        mirror_to,
        other_language,
        over,
    )
    .await?;
    let written = std::fs::canonicalize(&path).unwrap_or(path);
    let not_in_root = root
        .filter(|(root, text)| !biwrite_latex::project::reaches(root, text, &written))
        .map(|(root, _)| display_name(Some(&root)));
    Ok(Some(SavedView {
        not_in_root,
        ..saved
    }))
}

/// What a save may write over though it changed on disk elsewhere (the
/// author chose to): the document, and its paired file.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Overwrite {
    pub document: bool,
    pub mirror: bool,
}

/// The open file's own text, what saving writes: the editor's, or, while
/// the other language is edited (after a swap), composed from the
/// translations, refused while a paragraph is still pending. With a pair,
/// the editor holds the file's own language and the mirror follows.
///
/// The engine holds `editor_text` already ([`AppState::sync_text`]).
pub(crate) fn home_text(state: &Document, editor_text: String) -> CommandResult<String> {
    if state.editing_own_language() {
        Ok(editor_text)
    } else {
        // Made from the translations, so what waits out the edit delay goes now.
        state.engine.translate_held();
        Ok(state.engine.compose_target()?)
    }
}

/// [`write_document`], with `other_language` as counted when `text` was
/// taken from the editor (fills may land during the dialog and the write).
async fn save_to(
    window: &WebviewWindow,
    state: &Document,
    path: PathBuf,
    text: String,
    mirror_to: Option<PathBuf>,
    other_language: usize,
    over: Overwrite,
) -> CommandResult<SavedView> {
    let saved = write_document_with(state, path, text, mirror_to, over).await;
    refresh_title(window, state);
    saved.map(|saved| SavedView {
        other_language,
        ..saved
    })
}

/// Write the document's own `text` to `path` (and the paired file to
/// `mirror_to`, or where it is); [`save_to`] without the window title. The
/// caller holds `save_lock`, so the document can't change meanwhile. Not
/// over the file if it changed on disk since it was read or written.
pub(crate) async fn write_document(
    state: &Document,
    path: PathBuf,
    text: String,
    mirror_to: Option<PathBuf>,
) -> CommandResult<SavedView> {
    write_document_with(state, path, text, mirror_to, Overwrite::default()).await
}

/// [`write_document`], over changes made on disk elsewhere as `over` says.
pub(crate) async fn write_document_with(
    state: &Document,
    path: PathBuf,
    text: String,
    mirror_to: Option<PathBuf>,
    over: Overwrite,
) -> CommandResult<SavedView> {
    // The file as the app knows it, when it is the one written.
    let known = {
        let fs = state.file();
        (fs.path.as_ref() == Some(&path)).then(|| fs.file.text().to_owned())
    };
    // Ours already (a write that reported a failure after landing) is no
    // change made elsewhere either.
    if let Some(known) = known
        && !over.document
        && crate::disk_commands::changed_on_disk(&path, &known, None).await
        && crate::disk_commands::changed_on_disk(&path, &text, None).await
    {
        log::warn!(
            "{} changed on disk; not overwritten",
            display_name(Some(&path))
        );
        return Err(CommandError::ChangedOnDisk);
    }
    let note = state.doc_note();
    let other_language = state.left_in_other_language();
    // Only the English source is written; unedited text yields the original bytes.
    let bytes = state.file().file.encode(&text);
    files::write_file_atomic(path.clone(), bytes.clone()).await?;
    {
        let mut fs = state.file();
        fs.file = fs.file.saved(text, bytes);
        fs.path = Some(path.clone());
        fs.dirty = false;
        fs.ignored = None;
    }
    // Keep the document note when an untitled draft is saved or renamed;
    // otherwise use the note stored for the new path.
    if !note.is_empty() && state.doc_note().is_empty() {
        state.set_doc_note(note)?;
    } else {
        state.apply_doc_note();
    }
    log::info!("saved {}", display_name(Some(&path)));
    // The figures it includes that wait for this save go next to the
    // document's other figures.
    let (figures, figures_kept) = match crate::latex_commands::project(state) {
        Ok(p) if state.engine.mode() == Mode::Latex => {
            let saved = state.file().file.text().to_owned();
            state.figures.save(&p.dir, &saved)
        }
        _ => (Vec::new(), Vec::new()),
    };
    for f in &figures {
        log::info!("wrote the figure {f}");
    }
    let mirror = match mirror_to {
        Some(dest) => pairing::save_mirror_as(state, dest).await?,
        None => pairing::save_mirror_with(state, over.mirror).await?,
    };
    Ok(SavedView {
        name: display_name(Some(&path)),
        suggested_mode: Mode::from_path(&path),
        path: path.display().to_string(),
        mirror,
        other_language,
        figures,
        figures_kept,
        not_in_root: None,
    })
}

/// Swap languages: the translations become the editable text and the
/// current text becomes their translation. Unchanged paragraphs round-trip
/// exactly. Fails if any paragraph is not translated yet, unless `keep`
/// asks to swap now with those paragraphs unchanged.
#[tauri::command]
pub async fn swap_languages(
    window: WebviewWindow,
    state: Doc,
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
pub(crate) fn swap(state: &Document, text: String, keep: bool) -> CommandResult<SessionView> {
    if state.file().pair.is_some() {
        return pairing::swap(state, text, keep);
    }
    // With `keep` the swap happens now. Untranslated paragraphs keep their
    // text and are put in the newly edited language once translated. Saved
    // before then, they go into the file as they are, a document of both
    // languages, which builds like any other; the window says so on the
    // swap and on the save (`other_language`).
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
    state: Doc,
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
    state: &Document,
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
        // Paragraphs an early swap left in the other language would tip
        // the scale: not while there are any.
        let left = !state.engine.standins().is_empty();
        if !editing_own || left || written_in(&text, mode) != Some(home.flipped()) {
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
pub async fn continue_translation(state: Doc, text: String, document: u64) -> CommandResult<usize> {
    continue_left(&state, text, document)
}

/// [`continue_translation`] without Tauri.
pub(crate) fn continue_left(state: &Document, text: String, document: u64) -> CommandResult<usize> {
    state.sync_text(document, &text)?;
    Ok(state
        .engine
        .continue_translation(!state.editing_own_language()))
}

/// New editor text after the debounce: re-segment, align, schedule work.
/// `None` if the editor sent it for an earlier document (an update that
/// crossed a swap or an open): it is dropped.
#[tauri::command]
pub async fn update_document(
    state: Doc,
    text: String,
    document: u64,
) -> CommandResult<Option<Snapshot>> {
    Ok(state.engine.update_in(document, text).ok())
}

/// Change the mode; `None` as for [`update_document`]. Not for a pair: its
/// links are paragraphs of the mode it was paired in.
#[tauri::command]
pub async fn set_mode(
    state: Doc,
    mode: Mode,
    text: String,
    document: u64,
) -> CommandResult<Option<Snapshot>> {
    if state.file().pair.is_some() && mode != state.engine.mode() {
        return Err(CommandError::Settings(
            "A document paired with its translation keeps its mode.".into(),
        ));
    }
    Ok(state.engine.set_mode_in(document, mode, text).ok())
}

/// The translations are text the user wrote and the file keeps: a pair's
/// other file, or the file's own language while the other is edited.
fn translations_are_own(state: &Document) -> bool {
    let fs = state.file();
    fs.pair.is_some() || state.engine.direction() != fs.home
}

/// Retranslate one paragraph (also retries a failed one). Never one whose
/// translation is the user's own text: it would replace it in the file.
#[tauri::command]
pub async fn retranslate_segment(state: Doc, id: u64) -> CommandResult<()> {
    let id = SegmentId(id);
    if translations_are_own(&state) && state.engine.has_exact_translation(id) {
        return Err(CommandError::Settings(
            "This paragraph's translation is your own text, which the file keeps: \
             edit it instead of retranslating."
                .into(),
        ));
    }
    state.engine.retranslate(id)?;
    Ok(())
}

/// A paragraph and its sentences, as UTF-16 offsets in the editor's text.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SentencesView {
    pub paragraph: crate::latex_commands::Range16,
    pub sentences: Vec<crate::latex_commands::Range16>,
}

/// The paragraph of `text` at `pos` (UTF-16) and its sentences, found as a
/// click in the PDF finds one: a sentence of the translation is matched to
/// the source sentence in the same place.
#[tauri::command]
pub async fn source_sentences(
    state: Doc,
    text: String,
    pos: usize,
) -> CommandResult<Option<SentencesView>> {
    Ok(sentences_view(&text, state.engine.mode(), pos))
}

pub(crate) fn sentences_view(text: &str, mode: Mode, pos: usize) -> Option<SentencesView> {
    use crate::latex_commands::range16;
    let at = biwrite_core::utf16::utf16_to_byte(text, pos);
    let (paragraph, sentences) = biwrite_core::assist::sentences_at(text, mode, at)?;
    Some(SentencesView {
        paragraph: range16(text, paragraph.start, paragraph.end),
        sentences: sentences
            .into_iter()
            .map(|r| range16(text, r.start, r.end))
            .collect(),
    })
}

/// Translate the edited paragraphs still waiting out the translation delay
/// now (a swap needs them). Returns how many there were.
#[tauri::command]
pub async fn translate_held(state: Doc) -> CommandResult<usize> {
    Ok(state.engine.translate_held())
}

#[tauri::command]
pub async fn retranslate_all(state: Doc) -> CommandResult<()> {
    if translations_are_own(&state) {
        return Err(CommandError::Settings(
            "The translations are your own text, which the file keeps: retranslating \
             them all would replace it."
                .into(),
        ));
    }
    state.engine.retranslate_all();
    Ok(())
}

#[tauri::command]
pub async fn set_auto_translate(state: Doc, on: bool) -> CommandResult<()> {
    state.engine.set_auto_translate(on);
    Ok(())
}

/// The frontend reports when the editor content starts/stops differing from
/// the saved file (drives the title marker and close confirmation).
#[tauri::command]
pub async fn set_dirty(window: WebviewWindow, state: Doc, dirty: bool) -> CommandResult<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Offsets are UTF-16, as the editor counts them, past Chinese text.
    #[test]
    fn sentences_come_in_editor_offsets() {
        let text = "中文标题\n\nFirst one. Second one.\n";
        let pos = text.encode_utf16().count() - 5;
        let view = sentences_view(text, Mode::Plain, pos).unwrap();
        let units: Vec<u16> = text.encode_utf16().collect();
        let read =
            |r: &crate::latex_commands::Range16| String::from_utf16(&units[r.from..r.to]).unwrap();
        assert_eq!(read(&view.paragraph), "First one. Second one.");
        let found: Vec<String> = view.sentences.iter().map(read).collect();
        assert_eq!(found, ["First one.", "Second one."]);
        assert!(sentences_view(text, Mode::Plain, 0).is_some_and(|v| v.sentences.len() == 1));
    }
}
