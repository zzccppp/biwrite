//! Application state managed by Tauri: what the whole app shares
//! ([`AppState`]) and the document each window holds ([`Document`]).

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use biwrite_core::{Direction, LineEnding, Mode, TextFile};
use biwrite_engine::{Engine, EngineError, EngineSettings, Snapshot, TranslationCache, Translator};
use biwrite_providers::HttpProvider;
use serde::Serialize;
use tauri::ipc::{CommandArg, CommandItem, InvokeError};
use tauri::{AppHandle, Manager, Runtime};

use crate::assist_commands::AssistState;
use crate::error::CommandError;
use crate::latex_commands::LatexState;
use crate::pairing::{PairState, PairView};
use crate::request_log::RequestLog;
use crate::secrets::SecretStore;
use crate::settings::{AppSettings, Paths};
use crate::sink::TauriSink;
use crate::skills::SkillStore;
use crate::squeeze_commands::SqueezeState;
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
    /// The bytes the file had on disk, changed elsewhere, when the author
    /// chose not to load them: that version is not reported again (saving
    /// still asks before writing over it). `file` is what the app last read
    /// or wrote.
    pub ignored: Option<Vec<u8>>,
}

impl FileState {
    pub fn display_name(&self) -> String {
        display_name(self.path.as_deref())
    }

    /// Something isn't on disk: the editor's text, or the paired file
    /// (edits of it swapped away from, or a write it waits for). The
    /// frontend only reports the editor.
    pub fn unsaved(&self) -> bool {
        self.dirty || self.pair.as_ref().is_some_and(PairState::unsaved)
    }

    pub fn window_title(&self) -> String {
        let marker = if self.unsaved() { "• " } else { "" };
        format!("{marker}{} — BiWrite", self.display_name())
    }
}

pub fn display_name(path: Option<&Path>) -> String {
    path.and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into())
}

pub struct AppState {
    /// The engines' translation cache, for viewing and clearing it.
    pub cache: Arc<dyn TranslationCache>,
    pub(crate) settings: Mutex<AppSettings>,
    /// Changes of keys and providers (keychain, settings, providers in use)
    /// run one at a time, so keys read for one host are never written back
    /// after the provider moved to another. Held across keychain calls.
    pub keys_lock: tokio::sync::Mutex<()>,
    pub paths: Paths,
    pub secrets: Arc<dyn SecretStore>,
    pub request_log: Arc<RequestLog>,
    /// The translator every window's engine uses (a new window starts with
    /// it). Changed with the settings lock held.
    pub(crate) translator: Mutex<Arc<dyn Translator>>,
    /// The provider the engines translate with (for key status).
    pub(crate) translation_http: Mutex<HttpInUse>,
    /// The provider the writing assistant uses, built on first use.
    pub(crate) assistant_http: Mutex<HttpInUse>,
    /// Counts changes of the assistant's provider or its keys (bumped under
    /// the `assistant_http` lock): a provider built across one is not kept.
    pub(crate) assistant_epoch: AtomicU64,
    pub skills: SkillStore,
    /// What each window's LaTeX state starts from: the TeX toolchain found
    /// and the template folders, which all windows share.
    pub(crate) latex_base: LatexState,
    pub updates: UpdateState,
    /// Keys that got through to crowded relays, and squeezes running.
    pub squeeze: SqueezeState,
    /// The author's own papers and icons for the skill.
    pub materials: crate::materials::Materials,
    /// The document of each window, by window label.
    documents: Mutex<HashMap<String, Arc<Document>>>,
    /// The window that had the focus last.
    focused: Mutex<Option<String>>,
    /// Files being opened (canonical), not in a document yet.
    opening: Mutex<HashSet<PathBuf>>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        translator: Arc<dyn Translator>,
        cache: Arc<dyn TranslationCache>,
        settings: AppSettings,
        paths: Paths,
        secrets: Arc<dyn SecretStore>,
        request_log: Arc<RequestLog>,
        skills: SkillStore,
        latex: LatexState,
    ) -> Self {
        // The author's own materials, next to the settings.
        let materials = paths
            .settings
            .parent()
            .map(|d| d.join("materials"))
            .unwrap_or_default();
        let squeeze = SqueezeState::default();
        squeeze.set_keep_alive_failure_limit(settings.keep_alive_failure_limit);
        Self {
            cache,
            settings: Mutex::new(settings),
            keys_lock: tokio::sync::Mutex::new(()),
            paths,
            secrets,
            request_log,
            translator: Mutex::new(translator),
            translation_http: Mutex::new(None),
            assistant_http: Mutex::new(None),
            assistant_epoch: AtomicU64::new(0),
            skills,
            latex_base: latex,
            updates: UpdateState::default(),
            squeeze,
            materials: crate::materials::Materials::new(materials),
            documents: Mutex::new(HashMap::new()),
            focused: Mutex::new(None),
            opening: Mutex::new(HashSet::new()),
        }
    }

    fn documents_map(&self) -> MutexGuard<'_, HashMap<String, Arc<Document>>> {
        self.documents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The document of window `label`.
    pub fn document(&self, label: &str) -> Option<Arc<Document>> {
        self.documents_map().get(label).cloned()
    }

    /// Every window's document. (A copy: the registry is never locked
    /// while a document is worked on.)
    pub fn documents(&self) -> Vec<Arc<Document>> {
        self.documents_map().values().cloned().collect()
    }

    /// Drop the document of a window that is gone.
    pub fn forget(&self, label: &str) -> Option<Arc<Document>> {
        self.documents_map().remove(label)
    }

    /// The window, other than `except`, whose document is `path` or is
    /// paired with it.
    pub fn window_with(&self, path: &Path, except: &str) -> Option<String> {
        self.documents()
            .into_iter()
            .filter(|d| d.label != except)
            .find(|d| d.holds(path))
            .map(|d| d.label.clone())
    }

    /// `path` is about to be opened in some window: `None` if it is being
    /// opened already. No other window opens it while the claim is held.
    pub fn start_opening(&self, path: &Path) -> Option<Opening<'_>> {
        let path = crate::latex_commands::canonical(path);
        self.opening
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.clone())
            // Made only when claimed: dropped, it would give the claim up.
            .then(|| Opening { app: self, path })
    }

    /// `path` is being opened in some window (see [`Self::start_opening`]).
    pub fn is_opening(&self, path: &Path) -> bool {
        let path = crate::latex_commands::canonical(path);
        self.opening
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&path)
    }

    pub fn set_focused(&self, label: &str) {
        *self.focused.lock().unwrap_or_else(PoisonError::into_inner) = Some(label.to_owned());
    }

    /// The window that had the focus last, if it is still there.
    pub fn focused(&self) -> Option<String> {
        let label = self
            .focused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        self.document(&label).map(|d| d.label.clone())
    }

    /// The document of a new window `label`: untitled, its engine
    /// translating as every other window's does. Built with the settings
    /// lock held, as changes of the provider and of settings for every
    /// window are made: such a change comes before (and is in the new
    /// engine) or after (and reaches it).
    pub fn open_document<R: Runtime>(self: &Arc<Self>, app: &AppHandle<R>, label: &str) -> Doc {
        let settings = self.settings();
        self.document_with(&settings, app, label)
    }

    /// [`Self::open_document`] for a new window, labelled with the lowest
    /// number free (`doc-2`, `doc-3`, …): the build folders of a window
    /// closed serve the next one. Chosen with the settings lock held, so
    /// two windows opened at once get two labels.
    pub fn open_new_document<R: Runtime>(self: &Arc<Self>, app: &AppHandle<R>) -> Doc {
        let settings = self.settings();
        let label = (2..)
            .map(|n| format!("doc-{n}"))
            .find(|label| self.document(label).is_none() && app.get_webview_window(label).is_none())
            .unwrap_or_default();
        self.document_with(&settings, app, &label)
    }

    fn document_with<R: Runtime>(
        self: &Arc<Self>,
        settings: &AppSettings,
        app: &AppHandle<R>,
        label: &str,
    ) -> Doc {
        let translator = Arc::clone(
            &self
                .translator
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        let engine = Engine::new(
            translator,
            Arc::clone(&self.cache),
            Arc::new(TauriSink::for_window(app.clone(), label)),
            EngineSettings {
                concurrency: settings.effective_concurrency(),
                batch_size: settings.batch_size,
                edit_delay: Duration::from_secs(settings.translate_delay),
                ..EngineSettings::default()
            },
            tauri::async_runtime::handle().inner().clone(),
        );
        engine.set_glossary(settings.glossary.clone());
        Document::create(self, label, engine)
    }

    /// Run `f` on the engine of every window (settings that apply to all).
    pub fn each_engine(&self, f: impl Fn(&Engine)) {
        for doc in self.documents() {
            f(&doc.engine);
        }
    }

    /// Some window has changes that are not on disk.
    pub fn any_unsaved(&self) -> bool {
        self.documents().iter().any(|d| d.is_dirty())
    }

    /// Quitting must ask first: a window with unsaved changes the author
    /// did not agree to discard.
    pub fn needs_quit_confirmation(&self) -> bool {
        self.documents()
            .iter()
            .any(|d| d.needs_close_confirmation())
    }

    /// A quit was cancelled: changes agreed to be discarded for it (in a
    /// window asked about first) are asked about again next time.
    pub fn withdraw_discards(&self) {
        for doc in self.documents() {
            doc.discard_confirmed.store(false, Ordering::SeqCst);
        }
    }
}

/// A file being opened (see [`AppState::start_opening`]); dropped once it is
/// a window's document, or failed to open.
pub struct Opening<'a> {
    app: &'a AppState,
    path: PathBuf,
}

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        self.app
            .opening
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.path);
    }
}

/// The document a window holds, with its engine. Derefs to the
/// [`AppState`] all windows share.
pub struct Document {
    app: Arc<AppState>,
    /// The label of the window showing it.
    pub label: String,
    pub engine: Engine,
    file: Mutex<FileState>,
    /// Saves and changes of document (opening a file, swapping, reading
    /// the file as the other language, pairing) run one at a time, so a
    /// save never records its path on a document swapped in meanwhile.
    /// Held across the async write.
    pub save_lock: tokio::sync::Mutex<()>,
    /// Set once the user agreed to discard changes while closing/quitting.
    discard_confirmed: AtomicBool,
    /// Document note for an untitled document (not persisted).
    pub(crate) untitled_note: Mutex<String>,
    pub assist: AssistState,
    pub latex: LatexState,
    /// Figures that are files, waiting for the document's save.
    pub figures: crate::figures::FigureStore,
}

impl Deref for Document {
    type Target = AppState;

    fn deref(&self) -> &AppState {
        &self.app
    }
}

impl Document {
    /// An untitled document for window `label`, translated by `engine`,
    /// registered with `app`.
    pub fn create(app: &Arc<AppState>, label: &str, engine: Engine) -> Doc {
        let doc = Arc::new(Self {
            app: Arc::clone(app),
            label: label.to_owned(),
            engine,
            file: Mutex::new(FileState {
                file: TextFile::untitled(String::new()),
                ..FileState::default()
            }),
            save_lock: tokio::sync::Mutex::new(()),
            discard_confirmed: AtomicBool::new(false),
            untitled_note: Mutex::new(String::new()),
            assist: AssistState::default(),
            latex: app.latex_base.for_window(label),
            figures: crate::figures::FigureStore::default(),
        });
        let previous = app
            .documents_map()
            .insert(label.to_owned(), Arc::clone(&doc));
        debug_assert!(previous.is_none(), "window {label} had a document");
        Doc(doc)
    }

    /// The state all windows share.
    #[cfg(test)]
    pub fn app(&self) -> &Arc<AppState> {
        &self.app
    }

    /// Bring the engine to the editor's `text`, which the editor sent for
    /// `document` (see [`Snapshot::document`]). Text sent before another
    /// file was opened or the languages were swapped is refused, never
    /// applied to the new document.
    pub fn sync_text(&self, document: u64, text: &str) -> Result<(), CommandError> {
        if self.engine.document() != document {
            return Err(EngineError::Stale.into());
        }
        if self.engine.text() != text {
            self.engine.update_in(document, text.to_owned())?;
        }
        Ok(())
    }

    pub fn file(&self) -> MutexGuard<'_, FileState> {
        self.file.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn is_dirty(&self) -> bool {
        self.file().unsaved()
    }

    /// Closing or quitting must ask first.
    pub fn needs_close_confirmation(&self) -> bool {
        self.is_dirty() && !self.discard_confirmed.load(Ordering::SeqCst)
    }

    pub fn confirm_discard_on_close(&self) {
        self.discard_confirmed.store(true, Ordering::SeqCst);
    }

    /// The files the document holds: its own and the paired one.
    pub fn files(&self) -> Vec<PathBuf> {
        let fs = self.file();
        fs.path
            .iter()
            .cloned()
            .chain(fs.pair.as_ref().map(|p| p.path.clone()))
            .collect()
    }

    /// `path` is the document's file or the paired one.
    pub fn holds(&self, path: &Path) -> bool {
        use crate::latex_commands::canonical;
        let wanted = canonical(path);
        self.files().iter().any(|f| canonical(f) == wanted)
    }

    /// An untitled document nothing was written in: a file opened from its
    /// window takes its place, rather than a new window.
    pub fn is_blank(&self) -> bool {
        let fs = self.file();
        fs.path.is_none()
            && fs.pair.is_none()
            && !fs.unsaved()
            && self.engine.text().trim().is_empty()
    }

    /// Stop the document's work for good, its window gone: translations,
    /// the assistant's jobs and checks, builds.
    pub fn shut_down(&self) {
        self.engine.stop();
        for (_, job) in self.assist.jobs().drain() {
            job.abort();
        }
        for (_, check) in self.assist.checks().drain() {
            check.abort();
        }
        self.latex.stop();
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

    /// The editor holds the open file's own language, what saving writes
    /// as it is: always with a pair, else unless swapped.
    pub fn editing_own_language(&self) -> bool {
        let fs = self.file();
        fs.pair.is_some() || self.engine.direction() == fs.home
    }

    /// Paragraphs of the editor still in the other language since an early
    /// swap (their translations not there yet), which a save writes into the
    /// file as they are. None while the other language is edited: saving
    /// composes the file from the translations then.
    pub fn left_in_other_language(&self) -> usize {
        if self.editing_own_language() {
            self.engine.standins().len()
        } else {
            0
        }
    }

    pub fn session_view(&self, snapshot: Snapshot) -> SessionView {
        let units = self.engine.translations().len();
        let other_language = self.left_in_other_language();
        let fs = self.file();
        let settings = self.engine.settings();
        SessionView {
            path: fs.path.as_ref().map(|p| p.display().to_string()),
            name: fs.display_name(),
            text: self.engine.text(),
            dirty: fs.unsaved(),
            auto_translate: settings.auto_translate,
            line_ending: fs.file.line_ending(),
            bom: fs.file.has_bom(),
            pair: fs.pair.as_ref().map(|p| crate::pairing::view(p, units)),
            home: fs.home,
            other_language,
            snapshot,
        }
    }
}

/// The document of the window a command came from. Commands take it in
/// place of the app state; it derefs to the [`Document`] (and through it to
/// the [`AppState`]).
#[derive(Clone)]
pub struct Doc(Arc<Document>);

impl Deref for Doc {
    type Target = Document;

    fn deref(&self) -> &Document {
        &self.0
    }
}

impl From<Arc<Document>> for Doc {
    fn from(doc: Arc<Document>) -> Self {
        Self(doc)
    }
}

impl<'de, R: Runtime> CommandArg<'de, R> for Doc {
    fn from_command(command: CommandItem<'de, R>) -> Result<Self, InvokeError> {
        let webview = command.message.webview();
        let label = webview.label();
        let app = webview
            .try_state::<Arc<AppState>>()
            .ok_or_else(|| InvokeError::from("BiWrite is still starting"))?;
        app.document(label)
            .map(Doc)
            .ok_or_else(|| InvokeError::from(format!("window {label} has no document")))
    }
}

/// A window's document with an app state of its own (tests). The app's
/// translator, for windows opened later, is the mock.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn test_doc(
    engine: Engine,
    cache: Arc<dyn TranslationCache>,
    settings: AppSettings,
    paths: Paths,
    secrets: Arc<dyn SecretStore>,
    request_log: Arc<RequestLog>,
    skills: SkillStore,
    latex: LatexState,
) -> Doc {
    let app = Arc::new(AppState::new(
        Arc::new(biwrite_engine::MockTranslator::for_documents()),
        cache,
        settings,
        paths,
        secrets,
        request_log,
        skills,
        latex,
    ));
    Document::create(&app, crate::MAIN_WINDOW, engine)
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
    /// Paragraphs a save would write in the other language as they are
    /// ([`AppState::left_in_other_language`]).
    pub other_language: usize,
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
    /// Paragraphs written in the other language as they are, their
    /// translations not there yet ([`AppState::left_in_other_language`]).
    pub other_language: usize,
    /// Figure files the save wrote (the document includes them), relative
    /// to the folder TeX runs in.
    pub figures: Vec<String>,
    /// Figure files it left, because a file of that name is there.
    pub figures_kept: Vec<String>,
    /// A part saved under a new name: the main file that still includes
    /// the old one (its PDF leaves the new file out).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_in_root: Option<String>,
}
