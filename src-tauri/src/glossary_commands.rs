//! Glossary and export commands. The glossary lives in `settings.json`;
//! CSV and export files are chosen in native dialogs on the Rust side.

use std::path::Path;

use biwrite_core::GlossaryEntry;
use biwrite_core::glossary::{self, GlossaryError};
use serde::Serialize;
use tauri::{AppHandle, State, WebviewWindow};

use crate::error::{CommandError, CommandResult};
use crate::files;
use crate::state::{AppState, display_name};

/// Glossary CSVs are small (at most `MAX_ENTRIES` rows); refuse anything
/// this large.
const MAX_CSV_BYTES: u64 = 2_000_000;

#[tauri::command]
pub async fn get_glossary(state: State<'_, AppState>) -> CommandResult<Vec<GlossaryEntry>> {
    Ok(state.settings().glossary.clone())
}

/// Save the glossary (cleaned up: trimmed, empty terms dropped, duplicates
/// merged). Paragraphs whose terms changed are retranslated.
#[tauri::command]
pub async fn save_glossary(
    state: State<'_, AppState>,
    entries: Vec<GlossaryEntry>,
) -> CommandResult<Vec<GlossaryEntry>> {
    let entries = glossary::normalized(entries);
    glossary::check_size(&entries).map_err(|e| CommandError::Settings(e.to_string()))?;
    // Persist first, then commit to memory and the engine under one guard, so
    // a failed write or two overlapping saves can't leave settings and engine
    // disagreeing. (The engine never takes the settings lock.)
    let mut settings = state.settings();
    let mut next = settings.clone();
    next.glossary = entries.clone();
    state.persist(&next)?;
    *settings = next;
    state.engine.set_glossary(entries.clone());
    log::info!("glossary saved ({} terms)", entries.len());
    Ok(entries)
}

/// Read a CSV chosen in the Open dialog. The entries are returned for review
/// and only take effect when saved. `None` if cancelled.
#[tauri::command]
pub async fn import_glossary(
    app: AppHandle,
    window: WebviewWindow,
) -> CommandResult<Option<Vec<GlossaryEntry>>> {
    let Some(path) = files::pick_open_kind(&app, &window, "CSV (term,translation)", &["csv"]).await
    else {
        return Ok(None);
    };
    let bytes = files::read_small_file(path.clone(), MAX_CSV_BYTES).await?;
    let parsed = tokio::task::spawn_blocking(move || glossary::from_csv(&bytes))
        .await
        .map_err(|e| CommandError::Task(e.to_string()))?;
    let entries = parsed
        .map_err(|e: GlossaryError| CommandError::Settings(format!("{}: {e}", path.display())))?;
    log::info!(
        "read {} glossary terms from {}",
        entries.len(),
        display_name(Some(&path))
    );
    Ok(Some(entries))
}

/// Write the saved glossary to a CSV chosen in the Save dialog. Returns the
/// file name, or `None` if cancelled.
#[tauri::command]
pub async fn export_glossary(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CommandResult<Option<String>> {
    let csv = glossary::to_csv(&state.settings().glossary);
    let Some(path) =
        files::pick_save_kind(&app, &window, "CSV", &["csv"], None, "glossary.csv").await
    else {
        return Ok(None);
    };
    files::write_file_atomic(path.clone(), csv.into_bytes()).await?;
    log::info!("glossary exported to {}", display_name(Some(&path)));
    Ok(Some(display_name(Some(&path))))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportView {
    pub name: String,
    /// Paragraphs exported without a translation.
    pub missing: usize,
}

/// Export the document as bilingual Markdown (each paragraph in English,
/// then Chinese) to a file chosen in the Save dialog. `None` if cancelled.
#[tauri::command]
pub async fn export_bilingual(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
) -> CommandResult<Option<ExportView>> {
    // Normally a no-op: the frontend flushes edits first.
    state.engine.update(text);
    let current = state.file().path.clone();
    let suggested = format!("{}.bilingual.md", file_stem(current.as_deref()));
    let dir = current.as_deref().and_then(Path::parent);
    let Some(path) =
        files::pick_save_kind(&app, &window, "Markdown", &["md"], dir, &suggested).await
    else {
        return Ok(None);
    };
    // Built after the dialog, so translations that finished meanwhile count.
    let export = state.engine.bilingual_markdown();
    files::write_file_atomic(path.clone(), export.text.into_bytes()).await?;
    log::info!(
        "exported {} ({} paragraphs not translated yet)",
        display_name(Some(&path)),
        export.missing
    );
    Ok(Some(ExportView {
        name: display_name(Some(&path)),
        missing: export.missing,
    }))
}

fn file_stem(path: Option<&Path>) -> String {
    path.and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_names_follow_the_document() {
        assert_eq!(file_stem(Some(Path::new("/a/paper.tex"))), "paper");
        assert_eq!(file_stem(Some(Path::new("/a/notes.v2.md"))), "notes.v2");
        assert_eq!(file_stem(None), "Untitled");
    }
}
