//! Application state managed by Tauri.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use biwrite_core::{LineEnding, Mode, TextFile};
use biwrite_engine::{Engine, Snapshot};
use serde::Serialize;

use crate::error::CommandError;
use crate::secrets::SecretStore;
use crate::settings::{AppSettings, Paths};

/// The open file as it is on disk.
#[derive(Default)]
pub struct FileState {
    pub path: Option<PathBuf>,
    pub file: TextFile,
    /// Editor content differs from what is on disk (reported by the frontend).
    pub dirty: bool,
}

impl FileState {
    pub fn display_name(&self) -> String {
        display_name(self.path.as_deref())
    }

    pub fn window_title(&self) -> String {
        let marker = if self.dirty { "• " } else { "" };
        format!("{marker}{} — BiWrite", self.display_name())
    }
}

pub fn display_name(path: Option<&Path>) -> String {
    path.and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into())
}

pub struct AppState {
    pub engine: Engine,
    file: Mutex<FileState>,
    /// Saves run one at a time (held across the async write).
    pub save_lock: tokio::sync::Mutex<()>,
    /// Set once the user agreed to discard changes while closing/quitting.
    discard_confirmed: AtomicBool,
    pub(crate) settings: Mutex<AppSettings>,
    pub paths: Paths,
    pub secrets: Arc<dyn SecretStore>,
    /// Document note for an untitled document (not persisted).
    pub(crate) untitled_note: Mutex<String>,
}

impl AppState {
    pub fn new(
        engine: Engine,
        settings: AppSettings,
        paths: Paths,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            engine,
            file: Mutex::new(FileState {
                file: TextFile::untitled(String::new()),
                ..FileState::default()
            }),
            save_lock: tokio::sync::Mutex::new(()),
            discard_confirmed: AtomicBool::new(false),
            settings: Mutex::new(settings),
            paths,
            secrets,
            untitled_note: Mutex::new(String::new()),
        }
    }

    pub fn file(&self) -> MutexGuard<'_, FileState> {
        self.file.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn is_dirty(&self) -> bool {
        self.file().dirty
    }

    /// Closing or quitting must ask first.
    pub fn needs_close_confirmation(&self) -> bool {
        self.is_dirty() && !self.discard_confirmed.load(Ordering::SeqCst)
    }

    pub fn confirm_discard_on_close(&self) {
        self.discard_confirmed.store(true, Ordering::SeqCst);
    }

    /// Load a file synchronously (startup only, before the UI exists).
    pub fn load_path_blocking(&self, path: PathBuf) -> Result<(), CommandError> {
        let bytes = std::fs::read(&path).map_err(|e| CommandError::io(&path, e))?;
        let file = TextFile::decode(bytes).map_err(|source| CommandError::Decode {
            path: path.display().to_string(),
            source,
        })?;
        self.engine.set_doc_note(Some(self.note_for(Some(&path))));
        self.engine
            .load(file.text().to_owned(), Mode::from_path(&path));
        *self.file() = FileState {
            path: Some(path),
            file,
            dirty: false,
        };
        Ok(())
    }

    pub fn session_view(&self, snapshot: Snapshot) -> SessionView {
        let fs = self.file();
        let settings = self.engine.settings();
        SessionView {
            path: fs.path.as_ref().map(|p| p.display().to_string()),
            name: fs.display_name(),
            text: self.engine.text(),
            dirty: fs.dirty,
            auto_translate: settings.auto_translate,
            line_ending: fs.file.line_ending(),
            bom: fs.file.has_bom(),
            snapshot,
        }
    }
}

/// Everything the frontend needs to (re)build its view.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub path: Option<String>,
    pub name: String,
    pub text: String,
    pub dirty: bool,
    pub auto_translate: bool,
    pub line_ending: LineEnding,
    pub bom: bool,
    pub snapshot: Snapshot,
}

/// Result of a successful save.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedView {
    pub path: String,
    pub name: String,
    /// Mode implied by the (possibly new) file extension.
    pub suggested_mode: Mode,
}
