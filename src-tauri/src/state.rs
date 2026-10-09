//! Application state managed by Tauri.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use biwrite_core::{Direction, LineEnding, Mode, TextFile};
use biwrite_engine::{Engine, Snapshot, TranslationCache};
use biwrite_providers::HttpProvider;
use serde::Serialize;

use crate::assist_commands::AssistState;
use crate::error::CommandError;
use crate::latex_commands::LatexState;
use crate::pairing::{PairState, PairView};
use crate::request_log::RequestLog;
use crate::secrets::SecretStore;
use crate::settings::{AppSettings, Paths};
use crate::skills::SkillStore;
use crate::updater::UpdateState;

/// An HTTP provider in use, with the id of its settings entry.
pub type HttpInUse = Option<(String, Arc<HttpProvider>)>;

/// The open file as it is on disk.
#[derive(Default)]
pub struct FileState {
    pub path: Option<PathBuf>,
    pub file: TextFile,
    /// Editor content differs from what is on disk (reported by the frontend).
    pub dirty: bool,
    /// The file in the other language this one is paired with.
    pub pair: Option<PairState>,
    /// The direction in which the editor holds the file's own text: `EnZh`
    /// for an English file, `ZhEn` for a Chinese one. Editing the other
    /// language (after a swap), saving writes the file's language composed
    /// from the translations.
    pub home: Direction,
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
    /// The engine's translation cache, for viewing and clearing it.
    pub cache: Arc<dyn TranslationCache>,
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
    pub request_log: Arc<RequestLog>,
    /// The provider the engine translates with (for key status).
    pub(crate) translation_http: Mutex<HttpInUse>,
    /// The provider the writing assistant uses, built on first use.
    pub(crate) assistant_http: Mutex<HttpInUse>,
    pub assist: AssistState,
    pub skills: SkillStore,
    pub latex: LatexState,
    pub updates: UpdateState,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        engine: Engine,
        cache: Arc<dyn TranslationCache>,
        settings: AppSettings,
        paths: Paths,
        secrets: Arc<dyn SecretStore>,
        request_log: Arc<RequestLog>,
        skills: SkillStore,
        latex: LatexState,
    ) -> Self {
        Self {
            engine,
            cache,
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
            request_log,
            translation_http: Mutex::new(None),
            assistant_http: Mutex::new(None),
            assist: AssistState::default(),
            skills,
            latex,
            updates: UpdateState::default(),
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
        let read = |path: &PathBuf| -> Result<TextFile, CommandError> {
            let bytes = std::fs::read(path).map_err(|e| CommandError::io(path, e))?;
            TextFile::decode(bytes).map_err(|source| CommandError::Decode {
                path: path.display().to_string(),
                source,
            })
        };
        let file = read(&path)?;
        let mirror = crate::pairing::counterpart(&path).and_then(|m| read(&m).ok().map(|f| (m, f)));
        crate::pairing::open(self, path, file, mirror);
        Ok(())
    }

    pub fn session_view(&self, snapshot: Snapshot) -> SessionView {
        let units = self.engine.translations().len();
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
            pair: fs.pair.as_ref().map(|p| crate::pairing::view(p, units)),
            home: fs.home,
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
    /// The paired file in the other language.
    pub pair: Option<PairView>,
    /// The direction in which the editor holds the file's own language.
    pub home: Direction,
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
    /// What happened to the paired file.
    pub mirror: Option<crate::pairing::MirrorSaved>,
}
