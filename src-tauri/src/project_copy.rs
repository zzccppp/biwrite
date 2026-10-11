//! Saving the paper, or exporting its translation, into a new folder
//! together with the files it needs to build there (its template with
//! class, styles and bibliography style, its figures and bibliography), as
//! [`biwrite_latex::bundle`] finds them. The original folder is only read.

use std::path::{Path, PathBuf};

use biwrite_latex as latex;
use serde::Serialize;
use tauri::{AppHandle, State, WebviewWindow};

use crate::commands::home_text;
use crate::error::{CommandError, CommandResult};
use crate::files;
use crate::latex_commands::{canonical, project, template_err, translation_tex};
use crate::pairing;
use crate::state::{AppState, SavedView};

/// What a copy into a new folder did.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCopy {
    /// The new folder.
    pub folder: String,
    /// Files copied besides the ones written from the editor.
    pub copied: usize,
    /// Files the paper reads from too far above its folder to take along.
    pub left_out: Vec<String>,
    /// The document as saved there (Save As).
    pub saved: Option<SavedView>,
    /// The translation written there (export).
    pub exported: Option<String>,
}

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::Latex(message.into())
}

/// The root of the paired file at `path` (itself when it is a main file).
fn root_of(path: &Path) -> PathBuf {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    latex::root_for(path, &text).map_or_else(|_| path.to_path_buf(), |r| canonical(&r))
}

async fn bundle_of(roots: Vec<PathBuf>) -> CommandResult<latex::Bundle> {
    tokio::task::spawn_blocking(move || {
        let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
        latex::bundle(&roots)
    })
    .await
    .map_err(|e| CommandError::Task(e.to_string()))?
    .map_err(template_err)
}

async fn copy(bundle: latex::Bundle, dest: PathBuf, skip: Vec<PathBuf>) -> CommandResult<usize> {
    tokio::task::spawn_blocking(move || latex::copy_bundle(&bundle, &dest, &skip))
        .await
        .map_err(|e| CommandError::Task(e.to_string()))?
        .map_err(template_err)
}

/// A name for the new folder, the folder copied with `-copy`.
fn suggested(base: &Path) -> String {
    let name = base
        .file_name()
        .map_or_else(|| "paper".to_owned(), |n| n.to_string_lossy().into_owned());
    format!("{name}-copy")
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|p| p.display().to_string()).collect()
}

/// Save As into a new folder (`title` heads the folder dialog). The files
/// the paper needs are copied there at the same places relative to each
/// other, the document is written there from the editor, a paired file
/// too, and the document goes on from the new folder. Nothing is replaced,
/// since the folder must be new or empty.
#[tauri::command]
pub async fn save_project_as(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
    document: u64,
    title: String,
) -> CommandResult<Option<ProjectCopy>> {
    let _one_at_a_time = state.save_lock.lock().await;
    state.sync_text(document, &text)?;
    let own = home_text(&state, text)?;
    let bundle = paper_bundle(&state).await?;
    let Some(dest) = files::pick_new_folder(&app, &window, &title, &suggested(&bundle.base)).await
    else {
        return Ok(None);
    };
    let copy = save_into(&state, own, &bundle, dest).await?;
    crate::commands::refresh_title(&window, &state);
    Ok(Some(copy))
}

/// The files the open paper needs, its paired file's included.
pub(crate) async fn paper_bundle(state: &AppState) -> CommandResult<latex::Bundle> {
    let project = project(state)?;
    let mirror = state.file().pair.as_ref().map(|p| canonical(&p.path));
    let mut roots = vec![project.root.clone()];
    roots.extend(mirror.as_deref().map(root_of));
    bundle_of(roots).await
}

/// [`save_project_as`] into `dest`, the document's own text being `own`.
/// The caller holds `save_lock`.
pub(crate) async fn save_into(
    state: &AppState,
    own: String,
    bundle: &latex::Bundle,
    dest: PathBuf,
) -> CommandResult<ProjectCopy> {
    let current = project(state)?.path;
    let mirror = state.file().pair.as_ref().map(|p| canonical(&p.path));
    let rel_doc = current
        .strip_prefix(&bundle.base)
        .map_err(|_| fail("The document is outside its project folder."))?
        .to_path_buf();
    let rel_mirror = mirror
        .as_deref()
        .and_then(|m| m.strip_prefix(&bundle.base).ok())
        .map(Path::to_path_buf);
    let mut skip = vec![rel_doc.clone()];
    skip.extend(rel_mirror.clone());
    let copied = copy(bundle.clone(), dest.clone(), skip).await?;
    let new_path = dest.join(&rel_doc);
    let mirror_to = match (rel_mirror, mirror) {
        (Some(rel), _) => Some(dest.join(rel)),
        // Far from the paper, it goes next to it, named as Save As names it.
        (None, Some(m)) => Some(pairing::mirror_path_for(&current, &new_path, &m)),
        (None, None) => None,
    };
    for file in std::iter::once(&new_path).chain(mirror_to.as_ref()) {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CommandError::io(parent, e))?;
        }
    }
    let saved = crate::commands::write_document(state, new_path, own, mirror_to).await?;
    log::info!("saved with {copied} project files into {}", dest.display());
    Ok(ProjectCopy {
        folder: dest.display().to_string(),
        copied,
        left_out: names(&bundle.left_out),
        saved: Some(saved),
        exported: None,
    })
}

/// Export the translation of the open document (see
/// [`crate::latex_commands::latex_export_tex`]) into a new folder with the
/// files the paper needs. The folder is copied as it is on disk, and the
/// translation goes next to the document under the name the export gives
/// it.
#[tauri::command]
pub async fn latex_export_project(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
    document: u64,
    title: String,
) -> CommandResult<Option<ProjectCopy>> {
    let (name, body) = translation_tex(&state, text, document)?;
    let bundle = bundle_of(vec![project(&state)?.root]).await?;
    let Some(dest) = files::pick_new_folder(&app, &window, &title, &suggested(&bundle.base)).await
    else {
        return Ok(None);
    };
    export_into(&state, &name, body, &bundle, dest)
        .await
        .map(Some)
}

/// [`latex_export_project`] into `dest`, the translation `body` as `name`.
pub(crate) async fn export_into(
    state: &AppState,
    name: &str,
    body: String,
    bundle: &latex::Bundle,
    dest: PathBuf,
) -> CommandResult<ProjectCopy> {
    let rel_dir = project(state)?
        .path
        .parent()
        .and_then(|d| d.strip_prefix(&bundle.base).ok())
        .ok_or_else(|| fail("The document is outside its project folder."))?
        .to_path_buf();
    // Not over a file of the paper's own with that name.
    let mut rel = rel_dir.join(name);
    let stem = name.trim_end_matches(".tex").to_owned();
    let mut n = 2;
    while bundle.files.contains(&rel) {
        rel = rel_dir.join(format!("{stem}-{n}.tex"));
        n += 1;
    }
    let copied = copy(bundle.clone(), dest.clone(), Vec::new()).await?;
    let out = dest.join(&rel);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CommandError::io(parent, e))?;
    }
    files::write_file_atomic(out.clone(), body.into_bytes()).await?;
    log::info!(
        "exported with {copied} project files into {}",
        dest.display()
    );
    Ok(ProjectCopy {
        folder: dest.display().to_string(),
        copied,
        left_out: names(&bundle.left_out),
        saved: None,
        exported: Some(out.display().to_string()),
    })
}
