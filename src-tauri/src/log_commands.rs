//! Commands of the Log panel. Records are request metadata only (see
//! `request_log`), so these payloads carry no text and no keys.

use biwrite_providers::RequestRecord;
use serde::Serialize;
use tauri::State;

use crate::error::{CommandError, CommandResult};
use crate::request_log::LogSettings;
use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogView {
    pub settings: LogSettings,
    /// Newest first.
    pub records: Vec<RequestRecord>,
    /// The JSONL file finished records are appended to.
    pub file: Option<String>,
}

fn view(state: &AppState) -> LogView {
    LogView {
        settings: state.request_log.settings(),
        records: state.request_log.records(),
        file: state.request_log.file().map(|p| p.display().to_string()),
    }
}

#[tauri::command]
pub async fn get_request_log(state: State<'_, AppState>) -> CommandResult<LogView> {
    Ok(view(&state))
}

/// Pause or resume recording, and turn the log file on or off.
#[tauri::command]
pub async fn set_request_log(
    state: State<'_, AppState>,
    settings: LogSettings,
) -> CommandResult<LogView> {
    {
        let mut s = state.settings();
        s.request_log = settings;
        state.persist(&s)?;
    }
    state.request_log.set_settings(settings);
    Ok(view(&state))
}

/// Clear the panel (the log file keeps its records).
#[tauri::command]
pub async fn clear_request_log(state: State<'_, AppState>) -> CommandResult<LogView> {
    state.request_log.clear();
    Ok(view(&state))
}

/// Show the log file in Finder / Explorer.
#[tauri::command]
pub async fn reveal_request_log(state: State<'_, AppState>) -> CommandResult<()> {
    let Some(file) = state.request_log.file() else {
        return Err(CommandError::Settings("the request log has no file".into()));
    };
    let dir = file.parent().unwrap_or(file).to_path_buf();
    crate::files::reveal(&dir)
}
