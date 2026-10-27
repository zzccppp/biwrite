//! LaTeX: the TeX toolchain, building the open document's project (the
//! English PDF, or the Chinese mirror PDF), SyncTeX between the PDF and the
//! editor, the project's files, and paper templates.
//!
//! The English PDF is built from the files on disk while the document is
//! saved, and from a copy of the editor's text in `.biwrite/en/` inside the
//! project while it is not, so the files change only when the author saves.
//! The Chinese PDF is built from the composed translation, written to
//! `.biwrite/zh/` with Chinese font support added.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use biwrite_core::assist::{paragraph_at, sentence_at};
use biwrite_core::pair::units;
use biwrite_core::repair::TexError;
use biwrite_core::utf16::{byte_to_utf16, utf16_to_byte};
use biwrite_core::{Direction, Mode, SegmentId};
use biwrite_latex::{
    self as latex, CHECK_DIR, EN_MIRROR_DIR, Engine, Issue, Job, MIRROR_DIR, Outcome, PdfBox,
    Severity, Template, Toolchain, templates,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, WebviewWindow};
use tokio::task::AbortHandle;

use crate::commands;
use crate::error::{CommandError, CommandResult};
use crate::files;
use crate::latex_sync::{Basis, Place, line_of, line_start, map_line};
use crate::settings;
use crate::state::{Doc, Document, SessionView};

/// Longest build before it is stopped.
const BUILD_TIMEOUT: Duration = Duration::from_secs(300);
/// Largest PDF sent to the viewer.
const MAX_PDF_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    En,
    Zh,
}

/// A finished build.
struct Build {
    pdf: Option<PathBuf>,
    /// When the PDF was written. Built again since (by another window, of
    /// the same root in place), it maps no more.
    written: Option<std::time::SystemTime>,
    /// The project folder TeX ran in (canonical).
    dir: PathBuf,
    /// The file TeX read for the open document (canonical).
    doc_file: PathBuf,
    /// The open document's path when the build started.
    doc_path: PathBuf,
    basis: Basis,
    /// What TeX read: the files on disk, or a document composed from the
    /// translations.
    source: Source,
    /// The editing direction when the build started.
    direction: Direction,
    /// The paired file, built from a copy with translations not saved yet.
    mirror: Option<MirrorCopy>,
    /// Other parts of the project built from copies in this language (see
    /// [`stage_parts`]).
    parts: Vec<PartCopy>,
}

/// A part of the project (not the open document) built in the other
/// language from a copy composed of the translations in the cache: the copy
/// TeX read, the part's own file (both canonical), and the line of the part
/// where each paragraph of the copy starts (`(line in the copy, line in the
/// part)`, from 1, in order), so a click in the copy opens the part there.
#[derive(Clone)]
struct PartCopy {
    copy: PathBuf,
    file: PathBuf,
    lines: Vec<(usize, usize)>,
}

impl PartCopy {
    /// The line of the part for `line` of the copy: where its paragraph
    /// starts.
    fn line(&self, line: usize) -> usize {
        self.lines
            .iter()
            .rev()
            .find(|(copy, _)| *copy <= line)
            .map_or(line, |(_, part)| *part)
    }

    /// The line of the copy for `line` of the part: where its paragraph
    /// starts there.
    fn copy_line(&self, line: usize) -> usize {
        self.lines
            .iter()
            .rev()
            .find(|(_, part)| *part <= line)
            .map_or(line, |(copy, _)| *copy)
    }
}

/// A copy of the paired file in the `.biwrite` folder, with the current
/// translations: the copy TeX read, its text, and segment of the open
/// document → unit of the text.
#[derive(Clone)]
struct MirrorCopy {
    file: PathBuf,
    text: String,
    links: HashMap<SegmentId, usize>,
}

/// What a PDF is built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The project's files as saved (the file's own language, or either
    /// language of a pair).
    Files,
    /// A document composed from the translations, in a mirror folder.
    Translation,
}

#[derive(Default)]
struct TexCache {
    checked: bool,
    found: Option<Toolchain>,
}

/// A window's builds, with what all windows share: the TeX toolchain found
/// and the template folders.
pub struct LatexState {
    tex: Arc<tokio::sync::Mutex<TexCache>>,
    builds: Mutex<HashMap<Lang, Build>>,
    running: Mutex<HashMap<Lang, (u64, AbortHandle)>>,
    next: AtomicU64,
    /// The root of the project last opened. A file of its folder that no
    /// main file inputs (a figure snippet, a table) stays in that project.
    last_root: Mutex<Option<PathBuf>>,
    /// Templates shipped with the app.
    builtin: PathBuf,
    /// Templates the user imported.
    user: PathBuf,
    /// Builds into one folder run one at a time, whichever window starts
    /// them (shared by all windows).
    folders: Arc<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
    /// Added to the names of the folders this window builds in inside
    /// `.biwrite` (`.biwrite/zh-doc-2`), so each window's PDFs are its own;
    /// none for the main window.
    suffix: String,
    /// The build of each language started last: one waiting for its folder
    /// meanwhile gives up.
    latest: Mutex<HashMap<Lang, u64>>,
    /// The window is gone: builds waiting give up.
    stopped: AtomicBool,
    /// Wakes builds waiting for their turn when one is replaced or
    /// cancelled, or the window closes.
    changed: tokio::sync::Notify,
}

impl LatexState {
    pub fn new(builtin: PathBuf, user: PathBuf) -> Self {
        Self {
            tex: Arc::new(tokio::sync::Mutex::new(TexCache::default())),
            builds: Mutex::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            last_root: Mutex::new(None),
            builtin,
            user,
            folders: Arc::default(),
            suffix: String::new(),
            latest: Mutex::new(HashMap::new()),
            stopped: AtomicBool::new(false),
            changed: tokio::sync::Notify::new(),
        }
    }

    /// The state of window `label`: no builds, the same toolchain,
    /// templates and build locks.
    pub fn for_window(&self, label: &str) -> Self {
        Self {
            tex: Arc::clone(&self.tex),
            folders: Arc::clone(&self.folders),
            suffix: if label == crate::MAIN_WINDOW {
                String::new()
            } else {
                format!("-{label}")
            },
            ..Self::new(self.builtin.clone(), self.user.clone())
        }
    }

    /// This window's own folder for `base` (`.biwrite/zh` →
    /// `.biwrite/zh-doc-2`), as deep in the project.
    pub(crate) fn folder(&self, base: &str) -> String {
        format!("{base}{}", self.suffix)
    }

    /// A build of `lang` starts: its id, and the one to finish.
    fn begin(&self, lang: Lang) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(lang, id);
        self.changed.notify_waiters();
        id
    }

    /// Build `id` of `lang` was cancelled, replaced by a newer one or its
    /// window closed while it waited.
    fn superseded(&self, lang: Lang, id: u64) -> bool {
        self.stopped.load(Ordering::SeqCst)
            || self
                .latest
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&lang)
                != Some(&id)
    }

    /// A window opened from the one with `other`: its project for a part
    /// no main file reaches is that window's.
    pub fn follow(&self, other: &LatexState) {
        let root = other
            .last_root
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        *self
            .last_root
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = root;
    }

    /// Stop the builds running and waiting (the window is gone).
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
        for (_, (_, task)) in self.running().drain() {
            task.abort();
        }
    }

    /// Wait until no other build writes to `within` the project in `dir`
    /// (a folder of `.biwrite`, or the root TeX writes next to): held while
    /// this one stages its files and runs. Another window's build of the
    /// same root in place is waited for this way.
    pub(crate) async fn build_alone(
        &self,
        dir: &Path,
        within: &Path,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        self.lock_for(dir, within).lock_owned().await
    }

    /// [`Self::build_alone`] for build `id` of `lang`, given up (`None`) as
    /// soon as it is replaced or cancelled or its window closes, so it never
    /// holds this window's folder while it waits for nothing.
    pub(crate) async fn turn(
        &self,
        dir: &Path,
        within: &Path,
        lang: Lang,
        id: u64,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let lock = self.lock_for(dir, within);
        loop {
            // Listening before the check: a change in between still wakes.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.superseded(lang, id) {
                return None;
            }
            tokio::select! {
                guard = Arc::clone(&lock).lock_owned() => {
                    return (!self.superseded(lang, id)).then_some(guard);
                }
                () = &mut changed => {}
            }
        }
    }

    fn lock_for(&self, dir: &Path, within: &Path) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            self.folders
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(canonical(dir).join(within))
                .or_default(),
        )
    }

    fn builds(&self) -> MutexGuard<'_, HashMap<Lang, Build>> {
        self.builds.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn running(&self) -> MutexGuard<'_, HashMap<Lang, (u64, AbortHandle)>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::Latex(message.into())
}

/// The toolchain, detected once (again when `refresh`).
async fn toolchain(state: &Document, refresh: bool) -> Option<Toolchain> {
    let mut cache = state.latex.tex.lock().await;
    if refresh || !cache.checked {
        let preferred = state.settings().latex.tex_bin.clone().map(PathBuf::from);
        let bin = tokio::task::spawn_blocking(move || latex::find_bin(preferred.as_deref()))
            .await
            .ok()
            .flatten();
        cache.found = match bin {
            Some(bin) => Some(latex::detect(bin).await),
            None => None,
        };
        cache.checked = true;
        match &cache.found {
            Some(tc) => log::info!("TeX: {} in {}", tc.distribution, tc.bin.display()),
            None => log::info!("TeX: not found"),
        }
    }
    cache.found.clone()
}

pub(crate) async fn require_toolchain(state: &Document) -> CommandResult<Toolchain> {
    toolchain(state, false).await.ok_or_else(|| {
        fail("No TeX distribution was found. Install TeX Live or MiKTeX (MacTeX on macOS), or choose its folder in Settings.")
    })
}

// ── Status and settings ──────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TexStatus {
    pub found: bool,
    pub distribution: String,
    pub bin: String,
    pub latexmk: bool,
    pub synctex: bool,
    pub engines: Vec<Engine>,
    /// A folder the user chose for the TeX programs.
    pub custom_bin: Option<String>,
    pub compile_on_save: bool,
}

async fn status(state: &Document, refresh: bool) -> TexStatus {
    let tc = toolchain(state, refresh).await;
    let s = state.settings().latex.clone();
    TexStatus {
        found: tc.is_some(),
        distribution: tc
            .as_ref()
            .map(|t| t.distribution.clone())
            .unwrap_or_default(),
        bin: tc
            .as_ref()
            .map(|t| t.bin.display().to_string())
            .unwrap_or_default(),
        latexmk: tc.as_ref().is_some_and(|t| t.latexmk),
        synctex: tc.as_ref().is_some_and(|t| t.synctex),
        engines: tc.map(|t| t.engines).unwrap_or_default(),
        custom_bin: s.tex_bin,
        compile_on_save: s.compile_on_save,
    }
}

#[tauri::command]
pub async fn latex_status(state: Doc, refresh: bool) -> CommandResult<TexStatus> {
    Ok(status(&state, refresh).await)
}

fn save_latex_settings(
    state: &Document,
    edit: impl FnOnce(&mut settings::LatexSettings),
) -> CommandResult<()> {
    let mut s = state.settings();
    edit(&mut s.latex);
    settings::save(&state.paths.settings, &s).map_err(CommandError::Settings)
}

#[tauri::command]
pub async fn latex_set_compile_on_save(state: Doc, on: bool) -> CommandResult<()> {
    save_latex_settings(&state, |l| l.compile_on_save = on)
}

/// Choose the folder that holds `pdflatex` (and the other TeX programs).
#[tauri::command]
pub async fn latex_choose_bin(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
) -> CommandResult<TexStatus> {
    if let Some(dir) = files::pick_folder(&app, &window, "Folder with pdflatex").await {
        let found = latex::find_bin(Some(&dir));
        if found.as_deref() != Some(dir.as_path()) {
            return Err(fail(format!(
                "{} has no pdflatex, xelatex or lualatex",
                dir.display()
            )));
        }
        save_latex_settings(&state, |l| l.tex_bin = Some(dir.display().to_string()))?;
    }
    Ok(status(&state, true).await)
}

#[tauri::command]
pub async fn latex_reset_bin(state: Doc) -> CommandResult<TexStatus> {
    save_latex_settings(&state, |l| l.tex_bin = None)?;
    Ok(status(&state, true).await)
}

// ── The project ──────────────────────────────────────────────────────

/// The open document's project: root file, folder and engine.
pub(crate) struct Project {
    /// The open `.tex` file.
    pub(crate) path: PathBuf,
    /// Its text on disk.
    saved: String,
    pub(crate) root: PathBuf,
    pub(crate) root_text: String,
    /// Folder of the root (canonical).
    pub(crate) dir: PathBuf,
}

/// The text of the file at `path` as it is on disk, else (unreadable)
/// `known`, what the app last read or wrote of it.
pub(crate) fn as_on_disk(path: &Path, known: &str) -> String {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| biwrite_core::TextFile::decode(bytes).ok())
        .map_or_else(|| known.to_owned(), |file| file.text().to_owned())
}

fn is_tex(path: &Path) -> bool {
    path.extension()
        .is_some_and(|x| x.eq_ignore_ascii_case("tex"))
}

/// `path` relative to `dir`, `/`-separated.
pub(crate) fn relative(path: &Path, dir: &Path) -> Option<String> {
    let rel = path.strip_prefix(dir).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| match c {
            Component::Normal(p) => Some(p.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    Some(parts.join("/"))
}

pub(crate) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| latex::clean(path))
}

pub(crate) fn project(state: &Document) -> CommandResult<Project> {
    let (path, saved) = {
        let fs = state.file();
        (fs.path.clone(), fs.file.text().to_owned())
    };
    let path = path
        .filter(|p| is_tex(p))
        .ok_or_else(|| fail("Save the document as a .tex file first."))?;
    let p = project_of(state, &path, saved)?;
    // The open document's root, for a part no main file reaches later (a
    // paired file's or another language's root is not).
    *state
        .latex
        .last_root
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(p.root.clone());
    Ok(p)
}

/// The project of the `.tex` file at `path` whose text on disk is `saved`.
fn project_of(state: &Document, path: &Path, saved: String) -> CommandResult<Project> {
    let path = canonical(path);
    let last = state
        .latex
        .last_root
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let root = match latex::root_for(&path, &saved) {
        Ok(root) => canonical(&root),
        // Not reached from any main file: the project it was opened from.
        Err(e) => last
            .filter(|r| r.is_file() && r.parent().is_some_and(|d| path.starts_with(d)))
            .ok_or_else(|| fail(e.to_string()))?,
    };
    let root_text = if root == path {
        saved.clone()
    } else {
        std::fs::read_to_string(&root).map_err(|e| CommandError::io(&root, e))?
    };
    let dir = root.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(Project {
        path,
        saved,
        root,
        root_text,
        dir,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    /// The project folder's name.
    pub folder: String,
    /// The root file, relative to the folder.
    pub root: String,
    /// The open document, relative to the folder.
    pub current: String,
    pub engine: Engine,
    /// The project's `.tex` files, relative to the folder.
    pub files: Vec<String>,
}

#[tauri::command]
pub async fn latex_project(state: Doc) -> CommandResult<Option<ProjectView>> {
    let Ok(p) = project(&state) else {
        return Ok(None);
    };
    let dir = p.dir.clone();
    let files = tokio::task::spawn_blocking(move || latex::tex_files(&dir))
        .await
        .map_err(|e| CommandError::Task(e.to_string()))?;
    Ok(Some(ProjectView {
        folder: p
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        root: relative(&p.root, &p.dir).unwrap_or_default(),
        current: relative(&p.path, &p.dir).unwrap_or_default(),
        engine: latex::engine_for(&p.root_text),
        files: files
            .iter()
            .filter_map(|f| relative(&canonical(f), &p.dir))
            .collect(),
    }))
}

/// Open another `.tex` file of the project, named relative to its folder.
#[tauri::command]
pub async fn latex_open(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
    file: String,
) -> CommandResult<Option<SessionView>> {
    let p = project(&state)?;
    let wanted = latex::tex_files(&p.dir)
        .into_iter()
        .map(|f| canonical(&f))
        .find(|f| relative(f, &p.dir).as_deref() == Some(file.as_str()))
        .ok_or_else(|| fail(format!("{file} is not a .tex file of this project")))?;
    // Open in another window: shown there. The project's files are gone
    // through in this window otherwise, as before.
    if crate::windows::shown_elsewhere(&app, &state, &wanted) {
        return Ok(None);
    }
    if state.is_dirty() && !files::confirm_discard(&app, &window).await {
        return Ok(None);
    }
    // Opened elsewhere while the author was asked.
    let Some(_opening) = state.start_opening(&wanted) else {
        return Ok(None);
    };
    if crate::windows::shown_elsewhere(&app, &state, &wanted) {
        return Ok(None);
    }
    commands::load(&window, &state, wanted).await.map(Some)
}

/// Open a LaTeX project folder: its main file (see
/// [`crate::windows::open_from`]).
#[tauri::command]
pub async fn latex_open_folder(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
) -> CommandResult<Option<crate::windows::Opened>> {
    let Some(dir) = files::pick_folder(&app, &window, "Open a LaTeX project folder").await else {
        return Ok(None);
    };
    let main = latex::find_main(&dir).ok_or_else(|| {
        fail(format!(
            "{} has no main .tex file (one with \\documentclass and \\begin{{document}})",
            dir.display()
        ))
    })?;
    crate::windows::open_from(&app, &window, &state, main)
        .await
        .map(Some)
}

// ── Building ─────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueView {
    pub severity: Severity,
    /// Relative to the project folder (the original file for the mirror).
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
    /// The line is in the open document.
    pub here: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildView {
    pub id: u64,
    pub lang: Lang,
    pub outcome: Outcome,
    pub has_pdf: bool,
    /// The PDF predates the build (it stopped before writing one).
    pub stale: bool,
    pub issues: Vec<IssueView>,
    pub duration_ms: u64,
    pub tool: String,
    pub engine: Engine,
    pub root: String,
    /// Paragraphs of the Chinese PDF still in English.
    pub untranslated: usize,
    /// The end of the console output, for failures without a log.
    pub output: String,
    /// The project's own `latexmkrc`, which ran with the build (a warning).
    pub project_rc: Option<String>,
    /// Edits not saved yet are not in the PDF: it was built from the files
    /// (a part its root does not include directly, a paired file the
    /// translations do not fit).
    pub unsaved_left_out: bool,
}

/// A file named in the log (relative to the folder TeX ran in), as the
/// original file: the mirror's copies map back to what they mirror.
fn original(name: &str) -> String {
    let name = name.trim_start_matches("./");
    [MIRROR_DIR, EN_MIRROR_DIR, CHECK_DIR]
        .iter()
        .find_map(|dir| {
            let rest = name.strip_prefix(dir)?;
            // `.biwrite/zh/…`, or another window's `.biwrite/zh-doc-2/…`
            // (`\` on Windows).
            let slash = |c: char| c == '/' || c == '\\';
            match rest.strip_prefix('-') {
                Some(own) => own.split_once(slash).map(|(_, rest)| rest),
                None => rest.strip_prefix(slash),
            }
        })
        .unwrap_or(name)
        .to_owned()
}

fn issue_views(
    issues: Vec<Issue>,
    doc_rel: &str,
    doc_is_root: bool,
    dir: &Path,
    parts: &[PartCopy],
) -> Vec<IssueView> {
    issues
        .into_iter()
        .map(|mut i| {
            // A part built from a copy of its translations: the line of the
            // part where the paragraph starts.
            if let (Some(name), Some(line)) = (i.file.as_deref(), i.line) {
                let copy = canonical(&dir.join(name.trim_start_matches("./")));
                if let Some(part) = parts.iter().find(|p| p.copy == copy) {
                    i.line = Some(part.line(line as usize) as u32);
                }
            }
            let file = i.file.as_deref().map(original);
            // Warnings name no file: in a one-file project they are ours.
            let here = match &file {
                Some(f) => f == doc_rel,
                None => doc_is_root && i.line.is_some(),
            };
            IssueView {
                severity: i.severity,
                file,
                line: i.line,
                message: i.message,
                here,
            }
        })
        .collect()
}

fn write_file(path: &Path, text: &str) -> CommandResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| CommandError::io(dir, e))?;
    }
    std::fs::write(path, text).map_err(|e| CommandError::io(path, e))
}

/// The folder of the project where the PDF of `lang` is built from copies.
fn copies_dir(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => MIRROR_DIR,
        Lang::En => EN_MIRROR_DIR,
    }
}

/// A copy of the open document holding `text` in `folder` of the project,
/// and of its root pointed at the copy when the document is a part, so TeX
/// builds `text` with the project's other files while the document's file
/// stays as it is. The root to build (relative to the project folder) and
/// the copy of the document; `None` when the root does not `\input` or
/// `\include` the document directly.
fn stage(
    state: &Document,
    p: &Project,
    folder: &str,
    text: &str,
    prepare: fn(&str) -> String,
    redirects: &[(String, String)],
) -> CommandResult<Option<(PathBuf, PathBuf)>> {
    let doc_rel = relative(&p.path, &p.dir)
        .ok_or_else(|| fail("The document is outside its project folder."))?;
    let root_rel = relative(&p.root, &p.dir).unwrap_or_default();
    let out = p.dir.join(folder);
    let doc_out = out.join(&doc_rel);
    // Figures waiting for a save are read from copies in `.biwrite`.
    let text = &state.figures.staged(&p.dir, text);
    if p.path == p.root {
        write_file(&doc_out, &prepare(text))?;
    } else {
        let from = doc_rel.trim_end_matches(".tex");
        let to = format!("{folder}/{from}");
        let Some(root) = latex::redirect_include(&p.root_text, from, &to) else {
            return Ok(None);
        };
        // The other parts read from where `redirects` says.
        let root = redirect_all(&root, redirects);
        let root = state.figures.staged(&p.dir, &root);
        write_file(&doc_out, text)?;
        write_file(&out.join(&root_rel), &prepare(&root))?;
    }
    stage_bibliographies(p, folder, &[text, &p.root_text]);
    Ok(Some((
        Path::new(folder).join(root_rel),
        canonical(&doc_out),
    )))
}

/// The `.bib` files a `\bibliography` of `texts` names with `./` or `../`,
/// copied to where that name leads from `folder`: latexmk runs BibTeX in
/// the folder it builds in, and BibTeX takes such a name from there. Only
/// a place inside `.biwrite` gets a copy.
fn stage_bibliographies(p: &Project, folder: &str, texts: &[&str]) {
    let cache = p.dir.join(".biwrite");
    let mut done = Vec::new();
    for name in texts.iter().flat_map(|t| latex::bibliographies(t)) {
        if !(name.starts_with("./") || name.starts_with("../")) || done.contains(&name) {
            continue;
        }
        let file = if name.ends_with(".bib") {
            name.clone()
        } else {
            format!("{name}.bib")
        };
        let source = latex::clean(&p.dir.join(&file));
        let target = latex::clean(&p.dir.join(folder).join(&file));
        if target.starts_with(&cache) && source.is_file() {
            let copied = target
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::copy(&source, &target).map(|_| ()));
            if let Err(e) = copied {
                log::warn!("could not copy {} for BibTeX: {e}", source.display());
            }
        }
        done.push(name);
    }
}

/// The text of a file as edited now (see [`text_now`]).
struct Now {
    text: String,
    /// A paired file's links (segment of the document → unit of `text`).
    links: Option<HashMap<SegmentId, usize>>,
    /// Paragraphs in it still in the other language.
    untranslated: usize,
}

/// What the file of `lang` holds as edited now, as a save would write it:
/// the editor's text when it is in that language; else the paired file
/// with the current translations (paragraphs still being translated as
/// they are) and its links, or `None` when they would not fit its
/// structure; else, without a pair, the document composed from the
/// translations. With the number of paragraphs in it still in the other
/// language (their translations not there yet).
fn text_now(state: &Document, lang: Lang, direction: Direction, text: &str) -> Option<Now> {
    if lang == source_lang(direction) {
        return Some(Now {
            text: text.to_owned(),
            links: None,
            untranslated: 0,
        });
    }
    // Paragraphs waiting out the translation delay go now.
    state.engine.translate_held();
    {
        let fs = state.file();
        if let Some(pair) = fs.pair.as_ref() {
            return match crate::pairing::patched(state, pair, None, true) {
                // Kept as they are, and stand-ins: in the other language.
                crate::pairing::Patched::Ready {
                    text, links, kept, ..
                } => Some(Now {
                    text,
                    links: Some(links),
                    untranslated: kept.len() + state.engine.standins().len(),
                }),
                _ => None,
            };
        }
    }
    let mirror = state.engine.compose_mirror();
    Some(Now {
        text: mirror.text,
        links: None,
        untranslated: mirror.untranslated,
    })
}

/// Build the PDF of the file's own language (as edited) or of the other
/// language (the Chinese PDF of an English paper, the English one of a
/// Chinese paper: from the translation). `text` is the editor's current
/// text. What is not saved is built from copies in the project's
/// `.biwrite` folder, so the PDF shows the document as edited and its
/// files stay as they are until the author saves. A new build of the same
/// language replaces a running one.
#[tauri::command]
pub async fn latex_compile(
    state: Doc,
    lang: Lang,
    text: String,
    document: u64,
) -> CommandResult<BuildView> {
    let tc = require_toolchain(&state).await?;
    state.sync_text(document, &text)?;
    let direction = state.engine.direction();
    // The language of the file on disk.
    let file_lang = source_lang(state.file().home);
    // A pair keeps each language in its own files: build the file of this
    // language.
    let mirror = {
        let fs = state.file();
        fs.pair
            .as_ref()
            .filter(|_| lang != source_lang(direction))
            .map(|pair| (pair.path.clone(), pair.file.text().to_owned()))
    };
    let paired = state.file().pair.is_some();
    // The project's root in this language, built as saved whichever part
    // is open (see [`root_in`]).
    let other_root = if !paired && lang != file_lang {
        root_in(&state, lang)
    } else {
        None
    };
    let p = match (mirror, &other_root) {
        (Some((path, saved)), _) => project_of(&state, &path, saved)?,
        (None, Some(root)) => {
            let saved = std::fs::read_to_string(root).map_err(|e| CommandError::io(root, e))?;
            project_of(&state, root, saved)?
        }
        (None, None) => project(&state)?,
    };
    let doc_rel = relative(&p.path, &p.dir)
        .ok_or_else(|| fail("The document is outside its project folder."))?;
    let root_rel = relative(&p.root, &p.dir).unwrap_or_default();
    let doc_is_root = p.path == p.root;
    let same_language = lang == source_lang(direction);
    let mut untranslated = 0;
    let mut unsaved_left_out = false;
    let mut unsaved_built = false;
    let mut mirror_copy = None;
    let mut parts = Vec::new();
    // A newer build of this language replaces this window's older one at
    // once; one still waiting for its folder gives up when it gets it.
    let id = state.latex.begin(lang);
    if let Some((_, previous)) = state.latex.running().remove(&lang) {
        previous.abort();
    }
    let folder = state.latex.folder(copies_dir(lang));
    let folder = folder.as_str();
    let Some(_alone) = state.latex.turn(&p.dir, Path::new(folder), lang, id).await else {
        return Err(CommandError::Cancelled);
    };
    let (job, doc_file, basis, source) = match lang {
        _ if paired || lang == file_lang => {
            let now = text_now(&state, lang, direction, &text);
            // What TeX reads of the file as saved: as it is on disk now,
            // changed elsewhere perhaps since the app read or wrote it.
            let disk = as_on_disk(&p.path, &p.saved);
            let unsaved = now.as_ref().is_none_or(|now| now.text != disk);
            let staged = match &now {
                Some(now) if unsaved => stage(&state, &p, folder, &now.text, str::to_owned, &[])?,
                _ => None,
            };
            unsaved_left_out = unsaved && staged.is_none();
            match (staged, now) {
                (
                    Some((root, copy)),
                    Some(Now {
                        text: now,
                        links,
                        untranslated: left,
                    }),
                ) => {
                    // Paragraphs still in the other language are noted.
                    untranslated = left;
                    // A changed `% !TEX program` counts as it is now.
                    let root_text = if doc_is_root { &now } else { &p.root_text };
                    let engine = latex::project_engine(&p.root, root_text);
                    unsaved_built = true;
                    mirror_copy = links.map(|links| MirrorCopy {
                        file: copy.clone(),
                        text: now.clone(),
                        links,
                    });
                    (
                        Job {
                            dir: p.dir.clone(),
                            root,
                            engine,
                            out_dir: Some(PathBuf::from(folder)),
                            timeout: BUILD_TIMEOUT,
                        },
                        copy,
                        Basis {
                            compiled: now,
                            editor: text,
                            same_language,
                            mode: Mode::Latex,
                        },
                        Source::Files,
                    )
                }
                _ => (
                    Job {
                        dir: p.dir.clone(),
                        root: PathBuf::from(&root_rel),
                        engine: latex::project_engine(&p.root, &p.root_text),
                        out_dir: None,
                        timeout: BUILD_TIMEOUT,
                    },
                    p.path.clone(),
                    Basis {
                        compiled: disk,
                        editor: text,
                        same_language,
                        mode: Mode::Latex,
                    },
                    Source::Files,
                ),
            }
        }
        // Not the open document's file: no line of it stands for one of the
        // editor's (a part both roots read is found as another file).
        _ if other_root.is_some() => (
            Job {
                dir: p.dir.clone(),
                root: PathBuf::from(&root_rel),
                engine: latex::project_engine(&p.root, &p.root_text),
                out_dir: None,
                timeout: BUILD_TIMEOUT,
            },
            PathBuf::new(),
            Basis {
                compiled: as_on_disk(&p.path, &p.saved),
                editor: text,
                same_language,
                mode: Mode::Latex,
            },
            Source::Files,
        ),
        _ => {
            // The editor's text if it is in this language, else composed
            // from the translations.
            let other = if same_language {
                text.clone()
            } else {
                // Paragraphs waiting out the translation delay go now.
                state.engine.translate_held();
                let mirror = state.engine.compose_mirror();
                untranslated = mirror.untranslated;
                mirror.text
            };
            // Chinese needs font support; an English mirror keeps the
            // Chinese file's preamble, which has it.
            let prepare: fn(&str) -> String = match lang {
                Lang::Zh => latex::with_chinese,
                Lang::En => str::to_owned,
            };
            // The project's other parts in this language too, where they
            // have it.
            let toward = match lang {
                Lang::Zh => Direction::EnZh,
                Lang::En => Direction::ZhEn,
            };
            let (redirects, staged) = stage_parts(&state, &p, folder, lang, toward);
            parts = staged;
            let other = redirect_all(&other, &redirects);
            // The root's own words (title, abstract) in this language too,
            // when a part is open.
            let root_mirror = (p.path != p.root)
                .then(|| {
                    state
                        .engine
                        .compose_cached(&p.root_text, Mode::Latex, toward)
                })
                .filter(|m| m.pairs.len() > m.untranslated);
            let translated = root_mirror.as_ref().map(|m| Project {
                path: p.path.clone(),
                saved: p.saved.clone(),
                root: p.root.clone(),
                root_text: m.text.clone(),
                dir: p.dir.clone(),
            });
            if let Some(m) = &root_mirror {
                parts.push(PartCopy {
                    copy: p.dir.join(folder).join(&root_rel),
                    file: canonical(&p.root),
                    lines: part_lines(&p.root_text, m),
                });
            }
            let staged = stage(
                &state,
                translated.as_ref().unwrap_or(&p),
                folder,
                &other,
                prepare,
                &redirects,
            )?;
            if let Some(part) = parts.last_mut().filter(|_| root_mirror.is_some()) {
                part.copy = canonical(&part.copy);
            }
            let (root, copy) = staged.ok_or_else(|| {
                fail(format!(
                    "The translated PDF needs {root_rel} to \\input or \\include {doc_rel} directly."
                ))
            })?;
            let engine = match (lang, latex::engine_for(&p.root_text)) {
                (Lang::Zh, Engine::Lualatex) => Engine::Lualatex,
                (Lang::Zh, _) => Engine::Xelatex,
                (Lang::En, _) => latex::project_engine(&p.root, &p.root_text),
            };
            (
                Job {
                    dir: p.dir.clone(),
                    root,
                    engine,
                    out_dir: Some(PathBuf::from(folder)),
                    timeout: BUILD_TIMEOUT,
                },
                copy,
                Basis {
                    compiled: other,
                    editor: text,
                    same_language,
                    mode: Mode::Latex,
                },
                Source::Translation,
            )
        }
    };
    let engine = job.engine;

    // Built in place, TeX writes next to the root, as another window's
    // build of the same root does.
    let _in_place = match job.out_dir {
        None => Some(
            state
                .latex
                .turn(&p.dir, &job.root, lang, id)
                .await
                .ok_or(CommandError::Cancelled)?,
        ),
        Some(_) => None,
    };
    let task = tauri::async_runtime::spawn(async move { latex::compile(&tc, &job).await });
    if let Some((_, previous)) = state
        .latex
        .running()
        .insert(lang, (id, task.inner().abort_handle()))
    {
        previous.abort();
    }
    let result = task.await;
    {
        let mut running = state.latex.running();
        if running.get(&lang).is_some_and(|(i, _)| *i == id) {
            running.remove(&lang);
        }
    }
    let built = match result {
        Ok(built) => built.map_err(|e| fail(e.to_string()))?,
        Err(_) => return Err(CommandError::Cancelled),
    };
    log::info!(
        "built {lang:?} PDF of {root_rel}{}: {:?} in {} ms",
        if unsaved_built { " as edited" } else { "" },
        built.outcome,
        built.duration_ms
    );
    let view = BuildView {
        id,
        lang,
        outcome: built.outcome,
        has_pdf: built.pdf.is_some(),
        stale: built.stale,
        issues: if other_root.is_some() {
            // The open document is in this build only as a part both roots
            // read.
            let open = state.file().path.as_deref().map(canonical);
            let open_rel = open.and_then(|o| relative(&o, &p.dir)).unwrap_or_default();
            issue_views(built.issues, &open_rel, false, &p.dir, &parts)
        } else {
            issue_views(built.issues, &doc_rel, doc_is_root, &p.dir, &parts)
        },
        duration_ms: built.duration_ms,
        tool: built.tool,
        engine,
        root: root_rel,
        untranslated,
        output: built.output,
        project_rc: built.project_rc,
        unsaved_left_out,
    };
    if let Some(rc) = &view.project_rc {
        log::warn!("the build ran the project's own {rc}");
    }
    state.latex.builds().insert(
        lang,
        Build {
            written: built.pdf.as_deref().and_then(modified),
            pdf: built.pdf,
            dir: p.dir,
            doc_file,
            doc_path: if other_root.is_some() {
                PathBuf::new()
            } else {
                state
                    .file()
                    .path
                    .as_deref()
                    .map(canonical)
                    .unwrap_or_default()
            },
            basis,
            source,
            direction,
            mirror: mirror_copy,
            parts,
        },
    );
    Ok(view)
}

/// Longest check build before it is stopped.
const CHECK_TIMEOUT: Duration = Duration::from_secs(180);
/// Most errors read from a check build's log.
const CHECK_ERRORS: usize = 40;

/// What TeX said about the open document built as some text.
pub(crate) struct Checked {
    /// Its errors; the line is that of the text when TeX found the error
    /// in it.
    pub(crate) errors: Vec<TexError>,
    /// The open document, relative to the project folder (as errors name
    /// their files).
    pub(crate) doc: String,
}

impl Checked {
    /// The errors of the open document, and those TeX found in other files
    /// of the project, which only those files can fix.
    pub(crate) fn split(self) -> (Vec<TexError>, Vec<TexError>) {
        let doc = self.doc;
        self.errors
            .into_iter()
            .partition(|e| e.file.as_deref().is_none_or(|f| f == doc))
    }
}

/// Build the open document as `text` in `folder` of its project (a folder
/// of `.biwrite` apart from the PDFs shown) and read TeX's errors, to see
/// whether it compiles. The project's files stay as they are. `text` is in
/// the editor's language, built as the file is when that is the file's own
/// and as the translated PDF is otherwise.
pub(crate) async fn check_build(
    state: &Document,
    text: &str,
    folder: &str,
) -> CommandResult<Checked> {
    check_build_with(state, text, folder, "").await
}

/// [`check_build`] with `root_lines` before `\begin{document}` of the
/// root's copy when the open document is a part (what an answer of it
/// needs there).
pub(crate) async fn check_build_with(
    state: &Document,
    text: &str,
    folder: &str,
    root_lines: &str,
) -> CommandResult<Checked> {
    let tc = require_toolchain(state).await?;
    let lang = source_lang(state.engine.direction());
    let own = {
        let fs = state.file();
        fs.pair.is_some() || lang == source_lang(fs.home)
    };
    let p = project(state)?;
    let folder = state.latex.folder(folder);
    let folder = folder.as_str();
    let _alone = state.latex.build_alone(&p.dir, Path::new(folder)).await;
    let doc_rel = relative(&p.path, &p.dir)
        .ok_or_else(|| fail("The document is outside its project folder."))?;
    let root_rel = relative(&p.root, &p.dir).unwrap_or_default();
    let prepare: fn(&str) -> String = if !own && lang == Lang::Zh {
        latex::with_chinese
    } else {
        str::to_owned
    };
    let (root, _) = stage(state, &p, folder, text, prepare, &[])?.ok_or_else(|| {
        fail(format!(
            "{root_rel} includes {doc_rel} through a macro, so it cannot be built on its own."
        ))
    })?;
    if !root_lines.trim().is_empty() && p.path != p.root {
        let copy = p.dir.join(&root);
        let staged = std::fs::read_to_string(&copy).map_err(|e| CommandError::io(&copy, e))?;
        if let Some(at) = staged.find("\\begin{document}") {
            let lines = if root_lines.ends_with('\n') {
                root_lines.to_owned()
            } else {
                format!("{root_lines}\n")
            };
            write_file(&copy, &format!("{}{lines}{}", &staged[..at], &staged[at..]))?;
        }
    }
    let engine = if own {
        latex::project_engine(&p.root, if p.path == p.root { text } else { &p.root_text })
    } else {
        match (lang, latex::engine_for(&p.root_text)) {
            (Lang::Zh, Engine::Lualatex) => Engine::Lualatex,
            (Lang::Zh, _) => Engine::Xelatex,
            (Lang::En, _) => latex::project_engine(&p.root, &p.root_text),
        }
    };
    let out = p.dir.join(folder);
    let stem = root
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A build an error stopped may have left its auxiliary file cut short,
    // which would read as an error of the next one.
    let _ = std::fs::remove_file(out.join(format!("{stem}.aux")));
    let job = Job {
        dir: p.dir.clone(),
        root,
        engine,
        out_dir: Some(PathBuf::from(folder)),
        timeout: CHECK_TIMEOUT,
    };
    let built = latex::compile(&tc, &job)
        .await
        .map_err(|e| fail(e.to_string()))?;
    let log = std::fs::read(out.join(format!("{stem}.log")))
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    // A file of the build as the project names it.
    let named = |file: &str| {
        let file = file.trim_start_matches("./");
        file.strip_prefix(folder)
            .map(|rest| rest.trim_start_matches('/'))
            .unwrap_or(file)
            .to_owned()
    };
    let one_file = p.path == p.root;
    let errors = tex_errors(&built, &log, named, |f| match f {
        Some(f) => f == doc_rel,
        None => one_file,
    });
    log::info!(
        "checked {doc_rel}: {:?}, {} errors in {} ms",
        built.outcome,
        errors.len(),
        built.duration_ms
    );
    Ok(Checked {
        errors,
        doc: doc_rel,
    })
}

/// The errors of a build to repair, from its log: each with what TeX
/// printed after it, its file as `named` gives it, and its line when
/// `here` (of that file) says it is in the text built. A tool's failure
/// (BibTeX with a database it cannot open) or a PDF left unfinished is no
/// line of the source; a build that stopped without an error in its log
/// says so.
pub(crate) fn tex_errors(
    built: &latex::Compiled,
    log: &str,
    named: impl Fn(&str) -> String,
    here: impl Fn(Option<&str>) -> bool,
) -> Vec<TexError> {
    let contexts = latex::log::error_contexts(log, CHECK_ERRORS);
    let mut errors: Vec<TexError> = built
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .filter(|i| {
            i.file.is_some() || i.line.is_some() || contexts.iter().any(|c| c.message == i.message)
        })
        .map(|i| {
            let file = i.file.as_deref().map(&named);
            let context = contexts
                .iter()
                .find(|c| c.message == i.message && c.line == i.line)
                .map(|c| c.context.clone())
                .unwrap_or_default();
            let here = here(file.as_deref());
            TexError {
                line: i.line.filter(|_| here).map(|l| l as usize),
                file_line: i.line.filter(|_| !here).map(|l| l as usize),
                file,
                message: i.message.clone(),
                context,
            }
        })
        .collect();
    if errors.is_empty() && matches!(built.outcome, Outcome::Failed | Outcome::TimedOut) {
        let lines: Vec<&str> = built.output.lines().collect();
        errors.push(TexError {
            line: None,
            file: None,
            file_line: None,
            message: match built.outcome {
                Outcome::TimedOut => "The build did not finish in time.".to_owned(),
                _ => "No PDF was produced.".to_owned(),
            },
            context: lines[lines.len().saturating_sub(20)..].join("\n"),
        });
    }
    errors
}

/// What a build of `lang` is made from now (as [`latex_compile`] decides).
fn source_now(state: &Document, lang: Lang) -> Source {
    let own = {
        let fs = state.file();
        fs.pair.is_some() || lang == source_lang(fs.home)
    };
    if own || root_in(state, lang).is_some() {
        Source::Files
    } else {
        Source::Translation
    }
}

/// The root of the project in `lang`, when a part of it is open, its root
/// has a file in that language (`paper_zh.tex` for `paper.tex`, or one the
/// author paired it with), and that file includes the open part, or the
/// part's own file in that language: that root, as saved, makes the PDF
/// of `lang`. Otherwise (the root itself open, a part the other root
/// lacks) the PDF is built from the translations.
fn root_in(state: &Document, lang: Lang) -> Option<PathBuf> {
    let p = project(state).ok()?;
    if p.path == p.root {
        return None;
    }
    let other = canonical(&crate::pairing::mirror_path(state, &p.root)?);
    let text = std::fs::read_to_string(&other).ok()?;
    // Chinese by its words, or, for a main file of hardly more than its
    // `\input`s, by loading Chinese support where the root does not.
    let chinese = crate::pairing::is_chinese(&text)
        || (latex::cjk::supported(&text) && !latex::cjk::supported(&p.root_text));
    if chinese != (lang == Lang::Zh) || !latex::is_main(&text) {
        return None;
    }
    let reads = |file: &Path| latex::project::reaches(&other, &text, &canonical(file));
    let part_there =
        reads(&p.path) || crate::pairing::mirror_path(state, &p.path).is_some_and(|m| reads(&m));
    part_there.then_some(other)
}

/// `text` with each part `(from, to)` it includes read from `to` instead
/// (names relative to the project folder, without `.tex`).
fn redirect_all(text: &str, redirects: &[(String, String)]) -> String {
    let mut text = text.to_owned();
    for (from, to) in redirects {
        if let Some(t) = latex::redirect_include(&text, from, to) {
            text = t;
        }
    }
    text
}

/// The line (from 1) of byte `at` in `text`.
fn line_number(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// Where each paragraph of `mirror` (composed from `source`) starts in
/// both: `(line in the mirror, line in the source)`.
fn part_lines(source: &str, mirror: &biwrite_engine::Mirror) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut in_mirror, mut in_source) = (0, 0);
    for (put, from) in &mirror.pairs {
        if put.is_empty() || from.is_empty() {
            continue;
        }
        let (Some(m), Some(s)) = (
            mirror.text[in_mirror..].find(put.as_str()),
            source[in_source..].find(from.as_str()),
        ) else {
            continue;
        };
        let (m, s) = (in_mirror + m, in_source + s);
        out.push((line_number(&mirror.text, m), line_number(source, s)));
        in_mirror = m + put.len();
        in_source = s + from.len();
    }
    out
}

/// The other parts of the project in `lang`, for a build of the open
/// document's translation in `folder`: a part paired with a file in that
/// language is read from that file, else from a copy in `folder` holding
/// the translations the cache has (`toward` that language; none are
/// requested), else as it is. Returns the includes to point elsewhere
/// (`(part, file to read)`, relative to the project folder, without `.tex`)
/// and the copies written, which point their own includes the same way.
fn stage_parts(
    state: &Document,
    p: &Project,
    folder: &str,
    lang: Lang,
    toward: Direction,
) -> (Vec<(String, String)>, Vec<PartCopy>) {
    let open = canonical(&p.path);
    let root = canonical(&p.root);
    let in_lang = |text: &str| crate::pairing::is_chinese(text) == (lang == Lang::Zh);
    let stem = |f: &Path| relative(f, &p.dir).map(|r| r.trim_end_matches(".tex").to_owned());
    let mut redirects = Vec::new();
    // Each copy to write, with its text.
    let mut copies: Vec<(PartCopy, String)> = Vec::new();
    for file in latex::tex_files(&p.dir) {
        let file = canonical(&file);
        if file == open || file == root {
            continue;
        }
        let Some(from) = stem(&file) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        // Already in that language (a part both roots share), or a main file.
        if in_lang(&text) || latex::is_main(&text) {
            continue;
        }
        // Its own file in that language.
        let paired = crate::pairing::mirror_path(state, &file)
            .filter(|m| std::fs::read_to_string(m).is_ok_and(|t| in_lang(&t)))
            .and_then(|m| stem(&canonical(&m)));
        if let Some(to) = paired {
            redirects.push((from, to));
            continue;
        }
        let mirror = state.engine.compose_cached(&text, Mode::Latex, toward);
        if mirror.pairs.len() <= mirror.untranslated {
            continue;
        }
        let to = format!("{folder}/{from}");
        let copy = PartCopy {
            copy: p.dir.join(format!("{to}.tex")),
            file,
            lines: part_lines(&text, &mirror),
        };
        redirects.push((from, to));
        copies.push((copy, mirror.text));
    }
    let mut parts = Vec::new();
    for (mut part, text) in copies {
        // Its own includes too (sections of a chapter).
        if write_file(&part.copy, &redirect_all(&text, &redirects)).is_ok() {
            part.copy = canonical(&part.copy);
            parts.push(part);
        }
    }
    (redirects, parts)
}

/// Most bytes of the files put into [`whole_paper`].
const WHOLE_PAPER_BYTES: usize = 4 << 20;

/// The whole paper the open document belongs to, for the writing
/// assistant: its root with each file it includes (`\input`, `\include`,
/// `\subfile`) put in its place, the open document as the editor has it
/// (`text`), each file between comments naming it. While the other language
/// is edited without a pair, the other files are as the cache has them in
/// that language. Also where byte `target` of `text` is in it. `None`
/// outside a LaTeX project of several files, or when the root does not
/// include the document where it can be found.
pub(crate) fn whole_paper(state: &Document, text: &str, target: usize) -> Option<(String, usize)> {
    let toward = (!state.editing_own_language()).then(|| state.file().home);
    let (paper, at) = put_together(state, text, target, toward, true)?;
    Some((paper, at?))
}

/// The whole paper as one `.tex` file: its root with each file it includes
/// in its place (`\include`d ones between `\clearpage`s, as TeX sets
/// them, and no `\includeonly`), the open document as `text`. With
/// `toward`, the other files are in that direction's target language:
/// their own file in it (a pair), else the translations the cache has.
/// `None` outside a LaTeX project of several files.
pub(crate) fn flat_paper(
    state: &Document,
    text: &str,
    toward: Option<Direction>,
) -> Option<String> {
    put_together(state, text, 0, toward, false).map(|(paper, _)| paper)
}

/// The paper of the open document's project put together (see
/// [`whole_paper`] for `marked`, [`flat_paper`] otherwise), and where byte
/// `target` of `text` is in it.
fn put_together(
    state: &Document,
    text: &str,
    target: usize,
    toward: Option<Direction>,
    marked: bool,
) -> Option<(String, Option<usize>)> {
    if state.engine.mode() != Mode::Latex {
        return None;
    }
    let p = project(state).ok()?;
    let mut paper = Paper {
        state,
        dir: canonical(&p.dir),
        open: canonical(&p.path),
        editor: text,
        target,
        toward,
        marked,
        at: None,
        budget: WHOLE_PAPER_BYTES,
        seen: std::collections::HashSet::new(),
        parts: 0,
    };
    let root = canonical(&p.root);
    let root_text = paper.text_of(&root, &p.root_text);
    let mut out = String::with_capacity(root_text.len() * 4);
    paper.seen.insert(root.clone());
    paper.put(&mut out, &root, &root_text, 4);
    (paper.parts > 0).then_some((out, paper.at))
}

/// The state of [`whole_paper`] as it puts files in.
struct Paper<'a> {
    state: &'a Document,
    dir: PathBuf,
    open: PathBuf,
    editor: &'a str,
    target: usize,
    toward: Option<Direction>,
    /// Each file between comments naming it (for the assistant), else as
    /// TeX sets it (for a file of its own).
    marked: bool,
    /// Where the target is in the paper, once put in.
    at: Option<usize>,
    budget: usize,
    seen: std::collections::HashSet<PathBuf>,
    /// Files put in.
    parts: usize,
}

impl Paper<'_> {
    /// The text of `file` (as saved: `saved`) for the paper.
    fn text_of(&self, file: &Path, saved: &str) -> String {
        if *file == self.open {
            return self.editor.to_owned();
        }
        let Some(toward) = self.toward else {
            return saved.to_owned();
        };
        // Its own file in that language, else the translations the cache has.
        let zh = toward == Direction::EnZh;
        crate::pairing::mirror_path(self.state, file)
            .and_then(|m| std::fs::read(m).ok())
            .and_then(|b| biwrite_core::TextFile::decode(b).ok())
            .map(|f| f.text().to_owned())
            .filter(|t| crate::pairing::is_chinese(t) == zh)
            .unwrap_or_else(|| {
                self.state
                    .engine
                    .compose_cached(saved, Mode::Latex, toward)
                    .text
            })
    }

    /// `slice` (bytes `from`.. of `file`'s text) onto `out`, noting where
    /// the target is when it is in it.
    fn copy(&mut self, out: &mut String, file: &Path, slice: &str, from: usize) {
        if *file == self.open
            && self.at.is_none()
            && (from..=from + slice.len()).contains(&self.target)
        {
            self.at = Some(out.len() + self.target - from);
        }
        out.push_str(slice);
    }

    /// `text` of `file` onto `out` with the files it includes in their
    /// place, to `depth` levels.
    fn put(&mut self, out: &mut String, file: &Path, text: &str, depth: usize) {
        const COMMANDS: [&str; 3] = ["\\input{", "\\include{", "\\subfile{"];
        let mut start = 0;
        for line in text.split_inclusive('\n') {
            let end = latex::project::uncommented(line).len().min(line.len());
            // A file of its own holds every part.
            if !self.marked && line[..end].contains("\\includeonly{") {
                out.push_str("% ");
            }
            let mut at = 0;
            while let Some((i, len)) = COMMANDS
                .iter()
                .filter_map(|c| line[at..end].find(c).map(|i| (at + i, c.len())))
                .min_by_key(|(i, _)| *i)
            {
                let Some(close) = line[i + len..end].find('}') else {
                    break;
                };
                let name = line[i + len..i + len + close].trim();
                let after = i + len + close + 1;
                let Some((sub, saved)) = (depth > 0).then(|| self.read(name)).flatten() else {
                    self.copy(out, file, &line[at..after], start + at);
                    at = after;
                    continue;
                };
                self.copy(out, file, &line[at..i], start + at);
                let rel = relative(&sub, &self.dir).unwrap_or_else(|| name.to_owned());
                let here = if sub == self.open {
                    " (the file open in the editor)"
                } else {
                    ""
                };
                // `\include` starts and ends a page.
                let page = line[i..].starts_with("\\include{");
                if self.marked {
                    out.push_str(&format!("\n%% ===== {rel}{here} =====\n"));
                } else if page {
                    out.push_str("\\clearpage\n");
                }
                let sub_text = self.text_of(&sub, &saved);
                self.parts += 1;
                self.put(out, &sub, &sub_text, depth - 1);
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                if self.marked {
                    out.push_str(&format!("%% ===== end of {rel} =====\n"));
                } else if page {
                    out.push_str("\\clearpage\n");
                }
                at = after;
            }
            self.copy(out, file, &line[at..], start + at);
            start += line.len();
        }
    }

    /// The project file `name` names (as TeX reads it from the root's
    /// folder), with its saved text, once.
    fn read(&mut self, name: &str) -> Option<(PathBuf, String)> {
        if name.is_empty() || name.contains('\\') || name.contains('#') {
            return None;
        }
        let plain = self.dir.join(name);
        let path = if plain.extension().is_some() && plain.is_file() {
            plain
        } else {
            self.dir.join(format!("{name}.tex"))
        };
        let path = canonical(&path);
        if !path.starts_with(&self.dir) || !path.is_file() || !self.seen.insert(path.clone()) {
            return None;
        }
        let saved = if path == self.open {
            String::new()
        } else {
            let bytes = std::fs::read(&path).ok()?;
            biwrite_core::TextFile::decode(bytes)
                .ok()?
                .text()
                .to_owned()
        };
        let size = if path == self.open {
            self.editor.len()
        } else {
            saved.len()
        };
        self.budget = self.budget.checked_sub(size)?;
        Some((path, saved))
    }
}

/// The PDF of `lang` still belongs to the document as it is edited now.
/// It does not when a pairing or unpairing since the build changed what
/// the PDF is made from (the translation, or the paired file), nor after a
/// swap without a pair, which put the other language in the editor, nor
/// once the document is saved outside the project it was built for (Save
/// As into another folder), when its lines would lead to the old files.
/// Such a PDF is built again before clicks or the cursor map through it.
fn build_is_current(state: &Document, lang: Lang) -> bool {
    let builds = state.latex.builds();
    let Some(build) = builds.get(&lang) else {
        return true;
    };
    let (here, pair) = {
        let fs = state.file();
        (
            fs.path.as_deref().map(canonical).unwrap_or_default(),
            fs.pair.as_ref().map(|p| canonical(&p.path)),
        )
    };
    // The document built, another file of its project, or either file of a
    // pair (a pair's swap keeps both PDFs).
    let of_build = |path: &Path| {
        (!build.doc_path.as_os_str().is_empty() && *path == build.doc_path)
            || (!build.dir.as_os_str().is_empty() && path.starts_with(&build.dir))
    };
    let same_project = of_build(&here) || pair.as_deref().is_some_and(of_build);
    let paired = state.file().pair.is_some();
    // The PDF is still the one this window built.
    let its_own = build
        .pdf
        .as_deref()
        .is_none_or(|pdf| modified(pdf) == build.written);
    same_project
        && its_own
        && build.source == source_now(state, lang)
        && (paired || build.direction == state.engine.direction())
}

/// When the file at `path` was last written.
fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// `paper_zh` → `paper`: the name of a Chinese file without its language tag.
fn without_chinese_tag(stem: &str) -> &str {
    ["_zh", "-zh", ".zh", "_cn", "-cn", "_chinese"]
        .iter()
        .find_map(|tag| {
            stem.len()
                .checked_sub(tag.len())
                .filter(|&at| stem.is_char_boundary(at) && stem[at..].eq_ignore_ascii_case(tag))
                .map(|at| &stem[..at])
        })
        .filter(|s| !s.is_empty())
        .unwrap_or(stem)
}

/// The language of the edited text.
fn source_lang(direction: Direction) -> Lang {
    match direction {
        Direction::EnZh => Lang::En,
        Direction::ZhEn => Lang::Zh,
    }
}

/// The paired file as the build of `lang` read it, when `file` is that:
/// its text and its links (segment of the open document → unit of the
/// text). A copy with translations not saved yet, or the file as saved.
fn paired_source(
    state: &Document,
    lang: Lang,
    file: &Path,
) -> Option<(String, HashMap<SegmentId, usize>)> {
    let copy = state
        .latex
        .builds()
        .get(&lang)
        .and_then(|b| b.mirror.clone());
    if let Some(copy) = copy {
        return (copy.file == file).then_some((copy.text, copy.links));
    }
    let fs = state.file();
    let pair = fs.pair.as_ref().filter(|p| canonical(&p.path) == file)?;
    Some((pair.file.text().to_owned(), pair.links.clone()))
}

/// A line of the paired mirror (as the build of `lang` read it) as the
/// range of the linked paragraph in the editor's `text`. `None` unless
/// `file` is the mirror.
fn mirror_hit(
    state: &Document,
    lang: Lang,
    file: &Path,
    line: usize,
    text: &str,
    document: u64,
) -> Option<Range16> {
    let id = {
        let (mirror, links) = paired_source(state, lang, file)?;
        let at = line_start(&mirror, line.saturating_sub(1));
        let ub = units(&mirror, state.engine.mode());
        let ranges: Vec<_> = ub.iter().map(|u| u.range.clone()).collect();
        let j = crate::latex_sync::index_at_line(&ranges, &mirror, at).unwrap_or(0);
        let by_unit: HashMap<usize, SegmentId> = links.iter().map(|(id, u)| (*u, *id)).collect();
        (0..=j).rev().find_map(|k| by_unit.get(&k).copied())?
    };
    // A click resolved after another file was opened or a swap: no place.
    state.sync_text(document, text).ok()?;
    let snapshot = state.engine.snapshot();
    let seg = snapshot.layout.iter().find(|s| s.id == id)?;
    Some(Range16 {
        from: seg.from,
        to: seg.to,
    })
}

/// The mirror's file and line (as the build of `lang` read it) for the
/// paragraph at `offset` (UTF-16) of the editor's `text`.
fn mirror_line(
    state: &Document,
    lang: Lang,
    offset: usize,
    text: &str,
    document: u64,
) -> Option<(PathBuf, usize)> {
    state.sync_text(document, text).ok()?;
    let snapshot = state.engine.snapshot();
    let ids: Vec<SegmentId> = snapshot
        .layout
        .iter()
        .filter(|s| s.from <= offset)
        .map(|s| s.id)
        .collect();
    let copy = state
        .latex
        .builds()
        .get(&lang)
        .and_then(|b| b.mirror.clone());
    let (file, mirror, links) = match copy {
        Some(copy) => (copy.file, copy.text, copy.links),
        None => {
            let fs = state.file();
            let pair = fs.pair.as_ref()?;
            (
                canonical(&pair.path),
                pair.file.text().to_owned(),
                pair.links.clone(),
            )
        }
    };
    let unit = ids.iter().rev().find_map(|id| links.get(id).copied())?;
    let u = units(&mirror, state.engine.mode()).into_iter().nth(unit)?;
    Some((file, line_of(&mirror, u.content.start) + 1))
}

#[tauri::command]
pub async fn latex_cancel(state: Doc, lang: Lang) -> CommandResult<()> {
    // A build still waiting for its folder gives up too.
    state.latex.begin(lang);
    if let Some((_, task)) = state.latex.running().remove(&lang) {
        task.abort();
    }
    Ok(())
}

pub(crate) fn pdf_of(state: &Document, lang: Lang) -> CommandResult<PathBuf> {
    state
        .latex
        .builds()
        .get(&lang)
        .and_then(|b| b.pdf.clone())
        .ok_or_else(|| fail("There is no PDF yet: compile first."))
}

/// The PDF's bytes, for the viewer.
#[tauri::command]
pub async fn latex_pdf(state: Doc, lang: Lang) -> CommandResult<tauri::ipc::Response> {
    let pdf = pdf_of(&state, lang)?;
    let bytes = files::read_small_file(pdf, MAX_PDF_BYTES).await?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub async fn latex_reveal_pdf(state: Doc, lang: Lang) -> CommandResult<()> {
    files::reveal_file(&pdf_of(&state, lang)?)
}

/// Save a copy of the PDF (the Chinese one lives in a hidden folder).
#[tauri::command]
pub async fn latex_save_pdf(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
    lang: Lang,
) -> CommandResult<Option<String>> {
    let pdf = pdf_of(&state, lang)?;
    let stem = state
        .file()
        .path
        .as_deref()
        .and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "paper".into());
    let name = match lang {
        Lang::En => format!("{stem}.pdf"),
        Lang::Zh => format!("{stem}.zh.pdf"),
    };
    let dir = state
        .file()
        .path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let Some(dest) =
        files::pick_save_kind(&app, &window, "PDF", &["pdf"], dir.as_deref(), &name).await
    else {
        return Ok(None);
    };
    std::fs::copy(&pdf, &dest).map_err(|e| CommandError::io(&dest, e))?;
    Ok(Some(dest.display().to_string()))
}

/// Save the translation of the open document (the Chinese version of an
/// English file, the English version of a Chinese one) as a `.tex` file; a
/// Chinese main file gets font support so it compiles on its own.
/// Paragraphs not translated yet stay in the file's language. With a pair,
/// the translation is the paired file already.
#[tauri::command]
pub async fn latex_export_tex(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
    text: String,
    document: u64,
) -> CommandResult<Option<String>> {
    let (name, body, root_dir) = translation_tex(&state, text, document)?;
    let dir = root_dir.or_else(|| {
        state
            .file()
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    });
    let Some(dest) =
        files::pick_save_kind(&app, &window, "LaTeX", &["tex"], dir.as_deref(), &name).await
    else {
        return Ok(None);
    };
    commands::not_open_elsewhere(&state, [&dest])?;
    files::write_file_atomic(dest.clone(), body.into_bytes()).await?;
    Ok(Some(dest.display().to_string()))
}

/// Save the whole paper of a project of several files as one `.tex` file
/// (see [`flat_paper`]), in the language of its files, the open document
/// as edited. The project's files stay as they are, and the document stays
/// open.
#[tauri::command]
pub async fn latex_save_flat(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
    text: String,
    document: u64,
) -> CommandResult<Option<String>> {
    state.sync_text(document, &text)?;
    let own = crate::commands::home_text(&state, text)?;
    let paper = flat_paper(&state, &own, None)
        .ok_or_else(|| fail("The document is not part of a project of several files."))?;
    let p = project(&state)?;
    let stem = p
        .root
        .file_stem()
        .map_or_else(|| "paper".into(), |s| s.to_string_lossy().into_owned());
    let Some(dest) = files::pick_save_kind(
        &app,
        &window,
        "LaTeX",
        &["tex"],
        Some(&p.dir),
        &format!("{stem}-merged.tex"),
    )
    .await
    else {
        return Ok(None);
    };
    commands::not_open_elsewhere(&state, [&dest])?;
    // Never over a file of the paper.
    if latex::tex_files(&p.dir)
        .iter()
        .any(|f| crate::pairing::same_file(f, &dest))
    {
        return Err(fail(format!(
            "{} is a file of the paper: choose a new name.",
            crate::state::display_name(Some(&dest))
        )));
    }
    files::write_file_atomic(dest.clone(), paper.into_bytes()).await?;
    log::info!("saved the paper as one file, {}", dest.display());
    Ok(Some(dest.display().to_string()))
}

/// The file name and text of the translation [`latex_export_tex`] writes.
/// In a project of several files it is the whole paper in one file (see
/// [`flat_paper`]), named after the root and meant for the root's folder,
/// which comes third.
pub(crate) fn translation_tex(
    state: &Document,
    text: String,
    document: u64,
) -> CommandResult<(String, String, Option<PathBuf>)> {
    if let Some(pair) = state.file().pair.as_ref() {
        return Err(fail(format!(
            "The translation is the paired file {}.",
            crate::state::display_name(Some(&pair.path))
        )));
    }
    state.sync_text(document, &text)?;
    let target = match source_lang(state.file().home) {
        Lang::En => Lang::Zh,
        Lang::Zh => Lang::En,
    };
    let translated = if source_lang(state.engine.direction()) == target {
        text
    } else {
        state.engine.translate_held();
        state.engine.compose_mirror().text
    };
    let toward = state.file().home;
    let whole = flat_paper(state, &translated, Some(toward))
        .and_then(|paper| project(state).ok().map(|p| (paper, p.root)));
    let stem_of = |path: &Path| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "paper".into())
    };
    let (translated, stem, root_dir) = match whole {
        Some((paper, root)) => (paper, stem_of(&root), root.parent().map(Path::to_path_buf)),
        None => (
            translated,
            state
                .file()
                .path
                .as_deref()
                .map_or_else(|| "paper".into(), stem_of),
            None,
        ),
    };
    let body = if target == Lang::Zh && latex::is_main(&translated) {
        latex::with_chinese(&translated)
    } else {
        translated
    };
    // The whole paper is no version of the root (which `main_zh.tex` would
    // be), but of `main-merged.tex`, what Save as one .tex writes.
    let merged = if root_dir.is_some() { "-merged" } else { "" };
    let name = match target {
        Lang::Zh => format!("{stem}{merged}_zh.tex"),
        Lang::En => format!("{}{merged}_en.tex", without_chinese_tag(&stem)),
    };
    Ok((name, body, root_dir))
}

// ── SyncTeX ──────────────────────────────────────────────────────────

/// UTF-16 range in the editor's text.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Range16 {
    pub from: usize,
    pub to: usize,
}

pub(crate) fn range16(text: &str, from: usize, to: usize) -> Range16 {
    let r = byte_to_utf16(text, [from, to]);
    Range16 {
        from: r[0],
        to: r[1],
    }
}

/// The sentence at `at` (bytes), else its paragraph, else line `line`.
fn sentence_or_line(text: &str, at: usize, line: usize) -> Range16 {
    if let Some(r) =
        sentence_at(text, Mode::Latex, at).or_else(|| paragraph_at(text, Mode::Latex, at))
    {
        return range16(text, r.start, r.end);
    }
    let from = line_start(text, line.saturating_sub(1));
    let to = text[from..].find('\n').map_or(text.len(), |i| from + i);
    range16(text, from, to)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncHit {
    /// Relative to the project folder (the original file for the mirror).
    pub file: String,
    pub line: u32,
    /// In the open document: `range` is what to select.
    pub here: bool,
    pub range: Option<Range16>,
    /// `range` is a whole paragraph (the PDF is in the other language).
    pub paragraph: bool,
    /// Not in the open document: the project file to open for it
    /// (relative to the project folder), after which the click resolves.
    pub open: Option<String>,
}

/// The source under a click in the PDF: page (1-based) and point in PDF
/// points from the top left; `span` is the text-layer run under the
/// click and `click` the click's UTF-16 offset in it.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn latex_inverse(
    state: Doc,
    lang: Lang,
    page: u32,
    x: f64,
    y: f64,
    span: String,
    click: usize,
    text: String,
    document: u64,
) -> CommandResult<Option<SyncHit>> {
    let tc = require_toolchain(&state).await?;
    if !build_is_current(&state, lang) {
        return Err(CommandError::StalePdf);
    }
    let pdf = pdf_of(&state, lang)?;
    let Some(point) = latex::synctex::inverse(&tc, &pdf, page, x, y)
        .await
        .map_err(|e| fail(e.to_string()))?
    else {
        return Ok(None);
    };
    let click = utf16_to_byte(&span, click);
    let point = checked_point(&state, lang, point, &span, click);
    let file = canonical(&point.file);
    let line = point.line as usize;
    let current = state.file().path.as_deref().map(canonical);
    if let Some(range) = mirror_hit(&state, lang, &file, line, &text, document) {
        return Ok(Some(SyncHit {
            file: file.display().to_string(),
            line: point.line,
            here: true,
            range: Some(range),
            paragraph: true,
            open: None,
        }));
    }
    let open = file_to_open(&state, &file);
    let builds = state.latex.builds();
    let Some(build) = builds.get(&lang) else {
        return Ok(None);
    };
    // A part built from a copy of its translations: the part opens where
    // the clicked paragraph is.
    if let Some(part) = build.parts.iter().find(|c| c.copy == file) {
        let rel = relative(&part.file, &build.dir).unwrap_or_default();
        // Open now (the click opened it): its paragraph.
        if current.as_ref() == Some(&part.file) && state.editing_own_language() {
            let saved = state.file().file.text().to_owned();
            let l = map_line(&saved, &text, part.line(line));
            let ranges: Vec<_> = units(&text, Mode::Latex)
                .iter()
                .map(|u| u.range.clone())
                .collect();
            let at = line_start(&text, l.saturating_sub(1));
            if let Some(r) =
                crate::latex_sync::index_at_line(&ranges, &text, at).map(|j| &ranges[j])
            {
                return Ok(Some(SyncHit {
                    file: rel,
                    line: l as u32,
                    here: true,
                    range: Some(range16(&text, r.start, r.end)),
                    paragraph: true,
                    open: None,
                }));
            }
        }
        return Ok(Some(SyncHit {
            file: rel.clone(),
            line: part.line(line) as u32,
            here: false,
            range: None,
            paragraph: false,
            open: Some(rel).filter(|r| r.ends_with(".tex")),
        }));
    }
    let rel = relative(&file, &build.dir)
        .map(|r| original(&r))
        .unwrap_or_else(|| file.display().to_string());
    let here = |range: Range16, line: usize, paragraph: bool| SyncHit {
        file: rel.clone(),
        line: line as u32,
        here: true,
        range: Some(range),
        paragraph,
        open: None,
    };
    if file == build.doc_file
        && current.as_ref() == Some(&build.doc_path)
        && build.direction == state.engine.direction()
    {
        return Ok(match build.basis.to_editor(line, &text) {
            Some(Place::Line(l)) => {
                let at = latex::locate(&text, l as u32, &span, click);
                Some(here(sentence_or_line(&text, at, l), l, false))
            }
            Some(Place::Paragraph(r)) => Some(here(range16(&text, r.start, r.end), line, true)),
            None => None,
        });
    }
    // Another file of the project that is open now (the build was made
    // while a different one was): its text on disk is what TeX read.
    let same_file_on_disk = state.editing_own_language();
    if current.as_ref() == Some(&file) && same_file_on_disk {
        let saved = state.file().file.text().to_owned();
        let l = map_line(&saved, &text, line);
        let at = latex::locate(&text, l as u32, &span, click);
        return Ok(Some(here(sentence_or_line(&text, at, l), l, false)));
    }
    Ok(Some(SyncHit {
        file: rel,
        line: point.line,
        here: false,
        range: None,
        paragraph: false,
        open,
    }))
}

/// For a figure: the packages the document loads, and whether the open
/// file holds the preamble (then missing ones can be added to it). `None`
/// outside LaTeX, or for a part of a project whose main file is unknown.
pub(crate) fn document_packages(
    state: &Document,
    editor_text: &str,
) -> Option<(std::collections::BTreeSet<String>, bool)> {
    if state.engine.mode() != Mode::Latex {
        return None;
    }
    if latex::is_main(editor_text) {
        let dir = state
            .file()
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        return Some((latex::packages::loaded(&dir, editor_text), true));
    }
    let p = project(state).ok()?;
    Some((latex::packages::loaded(&p.dir, &p.root_text), false))
}

/// SyncTeX names the box on top at the click, which may be drawn over the
/// text: the line-number ruler of a submission, a running header. When the
/// clicked words are not near the line it names, they are looked up in the
/// document as compiled and then in the project's other files.
fn checked_point(
    state: &Document,
    lang: Lang,
    point: latex::synctex::SourcePoint,
    span: &str,
    click: usize,
) -> latex::synctex::SourcePoint {
    let builds = state.latex.builds();
    let Some(build) = builds.get(&lang) else {
        return point;
    };
    let source = |file: &Path| -> Option<String> {
        if file == build.doc_file {
            Some(build.basis.compiled.clone())
        } else {
            std::fs::read_to_string(file).ok()
        }
    };
    let named = canonical(&point.file);
    if source(&named)
        .is_some_and(|text| latex::find_words(&text, Some(point.line), span, click).is_some())
    {
        return point;
    }
    // The copies of other parts too, which the project's files leave out.
    let others = build
        .parts
        .iter()
        .map(|part| part.copy.clone())
        .chain(
            latex::tex_files(&build.dir)
                .into_iter()
                .map(|f| canonical(&f)),
        )
        .filter(|f| *f != build.doc_file);
    for file in std::iter::once(build.doc_file.clone())
        .chain(others)
        .take(400)
    {
        let Some(text) = source(&file) else {
            continue;
        };
        if let Some(at) = latex::find_words(&text, None, span, click) {
            log::info!(
                "the click is in {} rather than at {}:{}",
                file.display(),
                named.display(),
                point.line
            );
            return latex::synctex::SourcePoint {
                line: (line_of(&text, at) + 1) as u32,
                file,
            };
        }
    }
    point
}

/// The project file to open for a hit in `file`: the file itself, or, for
/// a file in the other language, its counterpart in the edited language
/// (which opens paired with it). Relative to the open document's project.
fn file_to_open(state: &Document, file: &Path) -> Option<String> {
    let dir = project(state).ok()?.dir;
    let editing_zh = state.engine.direction() == Direction::ZhEn;
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let target = if crate::pairing::is_chinese(&text) != editing_zh {
        crate::pairing::mirror_path(state, file).map(|c| canonical(&c))?
    } else {
        file.to_path_buf()
    };
    relative(&target, &dir).filter(|r| r.ends_with(".tex"))
}

/// Where the editor's position `offset` (UTF-16) is in the PDF.
#[tauri::command]
pub async fn latex_forward(
    state: Doc,
    lang: Lang,
    offset: usize,
    text: String,
    document: u64,
) -> CommandResult<Vec<PdfBox>> {
    let tc = require_toolchain(&state).await?;
    if !build_is_current(&state, lang) {
        return Err(CommandError::StalePdf);
    }
    let at = utf16_to_byte(&text, offset);
    let current = state.file().path.as_deref().map(canonical);
    if lang != source_lang(state.engine.direction())
        && let Some((input, line)) = mirror_line(&state, lang, offset, &text, document)
    {
        let pdf = pdf_of(&state, lang)?;
        return latex::synctex::forward(&tc, &pdf, &input, line as u32, 0)
            .await
            .map_err(|e| fail(e.to_string()));
    }
    let (pdf, input, line) = {
        let builds = state.latex.builds();
        let Some(build) = builds.get(&lang) else {
            return Ok(Vec::new());
        };
        let Some(pdf) = build.pdf.clone() else {
            return Ok(Vec::new());
        };
        if current.as_ref() == Some(&build.doc_path) && build.direction == state.engine.direction()
        {
            let Some(line) = build.basis.to_compiled(&text, at) else {
                return Ok(Vec::new());
            };
            (pdf, build.doc_file.clone(), line)
        } else if let Some(part) = build
            .parts
            .iter()
            .find(|c| current.as_ref() == Some(&c.file))
            .filter(|_| state.editing_own_language())
        {
            // A part built from a copy of its translations: its paragraph
            // there.
            let saved = state.file().file.text().to_owned();
            let line = map_line(&text, &saved, line_of(&text, at) + 1);
            (pdf, part.copy.clone(), part.copy_line(line))
        } else if let Some(path) = current.filter(|_| state.editing_own_language()) {
            // Another file of the project: as saved.
            let saved = state.file().file.text().to_owned();
            let line = map_line(&text, &saved, line_of(&text, at) + 1);
            (pdf, path, line)
        } else {
            return Ok(Vec::new());
        }
    };
    latex::synctex::forward(&tc, &pdf, &input, line as u32, 0)
        .await
        .map_err(|e| fail(e.to_string()))
}

/// The sentence at `line` (1-based) of a freshly opened file that matches
/// the clicked words (after a click in the PDF opened it).
#[tauri::command]
pub fn latex_locate(text: String, line: u32, span: String, click: usize) -> Range16 {
    let click = utf16_to_byte(&span, click);
    let at = latex::locate(&text, line, &span, click);
    sentence_or_line(&text, at, line as usize)
}

/// A compiled line (an issue in the log) as a range of the editor's text.
#[tauri::command]
pub async fn latex_goto(
    state: Doc,
    lang: Lang,
    line: u32,
    text: String,
) -> CommandResult<Option<Range16>> {
    if !build_is_current(&state, lang) {
        return Err(CommandError::StalePdf);
    }
    let current = state.file().path.as_deref().map(canonical);
    let direction = state.engine.direction();
    let builds = state.latex.builds();
    let Some(build) = builds
        .get(&lang)
        .filter(|b| current.as_ref() == Some(&b.doc_path) && b.direction == direction)
    else {
        return Ok(None);
    };
    Ok(match build.basis.to_editor(line as usize, &text) {
        Some(Place::Line(l)) => {
            let from = line_start(&text, l - 1);
            let to = text[from..].find('\n').map_or(text.len(), |i| from + i);
            Some(range16(&text, from, to))
        }
        Some(Place::Paragraph(r)) => Some(range16(&text, r.start, r.end)),
        None => None,
    })
}

// ── Templates ────────────────────────────────────────────────────────

pub(crate) fn template_err(e: latex::TemplateError) -> CommandError {
    fail(e.to_string())
}

#[tauri::command]
pub async fn latex_templates(state: Doc) -> CommandResult<Vec<Template>> {
    let (builtin, user) = (state.latex.builtin.clone(), state.latex.user.clone());
    tokio::task::spawn_blocking(move || templates::list(&builtin, &user))
        .await
        .map_err(|e| CommandError::Task(e.to_string()))
}

/// A new paper from template `id`, in a folder the user names; opens its
/// main file (here if this window holds nothing yet, else in a new one).
#[tauri::command]
pub async fn latex_new_paper(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
    id: String,
) -> CommandResult<Option<crate::windows::Opened>> {
    let (dir, t) =
        templates::find(&state.latex.builtin, &state.latex.user, &id).map_err(template_err)?;
    let suggested = format!("{}-paper", t.id);
    let Some(dest) = files::pick_new_folder(&app, &window, "New paper folder", &suggested).await
    else {
        return Ok(None);
    };
    let main = tokio::task::spawn_blocking(move || templates::instantiate(&dir, &dest))
        .await
        .map_err(|e| CommandError::Task(e.to_string()))?
        .map_err(template_err)?;
    log::info!("new paper from template {id}");
    crate::windows::open_from(&app, &window, &state, main)
        .await
        .map(Some)
}

/// Import a template from a project folder or a `.zip`.
#[tauri::command]
pub async fn latex_import_template(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
    zip: bool,
) -> CommandResult<Option<Template>> {
    let picked = if zip {
        files::pick_open_kind(&app, &window, "Template archive (.zip)", &["zip"]).await
    } else {
        files::pick_folder(&app, &window, "Import a LaTeX project folder as a template").await
    };
    let Some(src) = picked else {
        return Ok(None);
    };
    let user = state.latex.user.clone();
    let t = tokio::task::spawn_blocking(move || {
        if zip {
            templates::import_zip(&src, &user)
        } else {
            templates::import_folder(&src, &user, None)
        }
    })
    .await
    .map_err(|e| CommandError::Task(e.to_string()))?
    .map_err(template_err)?;
    log::info!("imported template {}", t.id);
    Ok(Some(t))
}

/// Export the open project as a template archive.
#[tauri::command]
pub async fn latex_export_template(
    app: AppHandle,
    window: WebviewWindow,
    state: Doc,
) -> CommandResult<Option<String>> {
    let p = project(&state)?;
    let name = format!(
        "{}.zip",
        p.dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "paper".into())
    );
    let parent = p.dir.parent().map(Path::to_path_buf);
    let Some(dest) = files::pick_save_kind(
        &app,
        &window,
        "Template archive",
        &["zip"],
        parent.as_deref(),
        &name,
    )
    .await
    else {
        return Ok(None);
    };
    let out = dest.clone();
    let count = tokio::task::spawn_blocking(move || templates::export_zip(&p.dir, &p.root, &out))
        .await
        .map_err(|e| CommandError::Task(e.to_string()))?
        .map_err(template_err)?;
    log::info!("exported {count} files as a template");
    Ok(Some(dest.display().to_string()))
}

#[tauri::command]
pub async fn latex_delete_template(state: Doc, id: String) -> CommandResult<()> {
    templates::delete(&state.latex.builtin, &state.latex.user, &id).map_err(template_err)
}

#[tauri::command]
pub async fn latex_reveal_templates(state: Doc) -> CommandResult<()> {
    files::reveal(&state.latex.user)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_of(source: Source, direction: Direction) -> Build {
        Build {
            pdf: None,
            written: None,
            dir: PathBuf::new(),
            doc_file: PathBuf::new(),
            doc_path: PathBuf::from("/papers/x/paper.tex"),
            basis: Basis {
                compiled: String::new(),
                editor: String::new(),
                same_language: true,
                mode: Mode::Latex,
            },
            source,
            direction,
            mirror: None,
            parts: Vec::new(),
        }
    }

    /// A PDF stops mapping once it would be built from something else (a
    /// pairing, an unpairing) or its lines are in the other language than
    /// the editor's (a swap without a pair). A swapped pair keeps both.
    #[tokio::test]
    async fn a_pdf_is_stale_once_it_would_be_built_from_something_else() {
        let dir = std::env::temp_dir().join(format!("biwrite-stale-{}", std::process::id()));
        let state = crate::pair_tests::app_state(&dir);
        let set = |lang, source, direction| {
            state
                .latex
                .builds()
                .insert(lang, build_of(source, direction));
        };
        let edit = |direction| {
            state
                .engine
                .load_known("x".to_owned(), Mode::Latex, direction, Vec::new());
        };
        let current = |lang| build_is_current(&state, lang);
        state.file().path = Some(PathBuf::from("/papers/x/paper.tex"));

        // An English file: its PDF from the file, the Chinese one from
        // the translation.
        set(Lang::En, Source::Files, Direction::EnZh);
        set(Lang::Zh, Source::Translation, Direction::EnZh);
        assert!(current(Lang::En) && current(Lang::Zh));

        // Paired: the Chinese PDF is now the paired file's.
        state.file().pair = Some(crate::pairing::PairState {
            path: dir.join("paper_zh.tex"),
            file: biwrite_core::TextFile::decode(b"x".to_vec()).unwrap(),
            text: "x".to_owned(),
            links: HashMap::new(),
            unpaired: Default::default(),
            behind: false,
            exists: true,
            ignored: None,
        });
        assert!(current(Lang::En) && !current(Lang::Zh));
        set(Lang::Zh, Source::Files, Direction::EnZh);
        assert!(current(Lang::Zh));

        // A swap of the pair: the Chinese file is edited, both still map.
        edit(Direction::ZhEn);
        state.file().home = Direction::ZhEn;
        assert!(current(Lang::En) && current(Lang::Zh));

        // Unpaired while editing the Chinese file: the English PDF should
        // come from the translation, and the Chinese one was built for the
        // English editor.
        state.file().pair = None;
        assert!(!current(Lang::En) && !current(Lang::Zh));
        set(Lang::En, Source::Translation, Direction::ZhEn);
        set(Lang::Zh, Source::Files, Direction::ZhEn);
        assert!(current(Lang::En) && current(Lang::Zh));

        // A swap without a pair: the editor holds the translation.
        edit(Direction::EnZh);
        assert!(!current(Lang::En) && !current(Lang::Zh));
        // No PDF at all is not a stale one.
        state.latex.builds().clear();
        assert!(current(Lang::En) && current(Lang::Zh));
    }

    /// A PDF written again after this window built it (another window built
    /// the same root in place) maps no more: its SyncTeX is the other build's.
    #[tokio::test]
    async fn a_pdf_built_again_elsewhere_is_stale() {
        let dir = std::env::temp_dir().join(format!("biwrite-stale-again-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state = crate::pair_tests::app_state(&dir);
        let (paper, pdf) = (dir.join("paper.tex"), dir.join("paper.pdf"));
        std::fs::write(&paper, "x").unwrap();
        std::fs::write(&pdf, "%PDF this window's").unwrap();
        state.file().path = Some(paper.clone());
        let mut build = build_of(Source::Files, Direction::EnZh);
        build.doc_path = canonical(&paper);
        build.written = modified(&pdf);
        build.pdf = Some(pdf.clone());
        state.latex.builds().insert(Lang::En, build);
        assert!(build_is_current(&state, Lang::En));
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&pdf)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(!build_is_current(&state, Lang::En), "built again");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Saved under another path (Save As), the document's PDF is no longer
    /// its own, and clicks would lead to the old file. Another file of the
    /// project built, and either file of a pair, still count.
    #[tokio::test]
    async fn a_pdf_built_for_another_path_is_stale() {
        let dir = std::env::temp_dir().join(format!("biwrite-stale-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state = crate::pair_tests::app_state(&dir);
        let (old, new) = (dir.join("paper.tex"), dir.join("copy/paper.tex"));
        std::fs::write(&old, "x").unwrap();
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&new, "x").unwrap();
        state.file().path = Some(old.clone());
        let mut build = build_of(Source::Files, Direction::EnZh);
        build.doc_path = canonical(&old);
        state.latex.builds().insert(Lang::En, build);
        assert!(build_is_current(&state, Lang::En));
        state.file().path = Some(new.clone());
        assert!(
            !build_is_current(&state, Lang::En),
            "built for the old path"
        );
        // A file of the project the PDF was built for still maps.
        let section = dir.join("sections/intro.tex");
        std::fs::create_dir_all(section.parent().unwrap()).unwrap();
        std::fs::write(&section, "x").unwrap();
        let mut project = build_of(Source::Files, Direction::EnZh);
        project.doc_path = canonical(&old);
        project.dir = canonical(&dir);
        state.latex.builds().insert(Lang::En, project);
        state.file().path = Some(section);
        assert!(
            build_is_current(&state, Lang::En),
            "a section of the project"
        );
        let elsewhere =
            std::env::temp_dir().join(format!("biwrite-stale-else-{}", std::process::id()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("paper.tex"), "x").unwrap();
        state.file().path = Some(elsewhere.join("paper.tex"));
        assert!(
            !build_is_current(&state, Lang::En),
            "saved into another folder"
        );
        std::fs::remove_dir_all(&elsewhere).unwrap();
        state.file().path = Some(new.clone());
        let mut build = build_of(Source::Files, Direction::EnZh);
        build.doc_path = canonical(&old);
        state.latex.builds().insert(Lang::En, build);
        // The other file of a pair, as after a pair's swap.
        state.file().pair = Some(crate::pairing::PairState {
            path: old.clone(),
            file: biwrite_core::TextFile::decode(b"x".to_vec()).unwrap(),
            text: "x".to_owned(),
            links: HashMap::new(),
            unpaired: Default::default(),
            behind: false,
            exists: true,
            ignored: None,
        });
        assert!(build_is_current(&state, Lang::En));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mirror_files_map_to_their_originals() {
        assert_eq!(original("./main.tex"), "main.tex");
        assert_eq!(
            original(".biwrite/zh/sections/intro.tex"),
            "sections/intro.tex"
        );
        assert_eq!(original("sections/intro.tex"), "sections/intro.tex");
        assert_eq!(
            original(".biwrite/zh-doc-2/sections/intro.tex"),
            "sections/intro.tex"
        );
        assert_eq!(original(".biwrite/english/a.tex"), ".biwrite/english/a.tex");
        assert_eq!(original(".biwrite/zh-doc-2\\main.tex"), "main.tex");
    }

    /// A build waiting for another window's build of the same root gives
    /// up at once when it is cancelled or its window closes, and gets its
    /// turn otherwise.
    #[tokio::test]
    async fn a_build_waiting_for_its_turn_gives_up_at_once() {
        let dir = std::env::temp_dir().join("biwrite-turns");
        let root = Path::new("main.tex");
        let base = LatexState::new(PathBuf::new(), PathBuf::new());
        let (one, other) = (base.for_window("main"), base.for_window("doc-2"));
        let held = one.build_alone(&dir, root).await;
        let soon = |f| tokio::time::timeout(std::time::Duration::from_secs(5), f);
        // Cancelled (`latex_cancel` starts no build but replaces the id).
        let id = other.begin(Lang::Zh);
        let cancel = async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            other.begin(Lang::Zh);
        };
        let (waited, ()) = tokio::join!(soon(other.turn(&dir, root, Lang::Zh, id)), cancel);
        assert!(waited.expect("woken at once").is_none());
        // Its window closed.
        let id = other.begin(Lang::En);
        let close = async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            other.stop();
        };
        let (waited, ()) = tokio::join!(soon(other.turn(&dir, root, Lang::En, id)), close);
        assert!(waited.expect("woken at once").is_none());
        // Neither: it gets its turn once the other build is done.
        let third = base.for_window("doc-3");
        let id = third.begin(Lang::Zh);
        let free = async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            drop(held);
        };
        let (waited, ()) = tokio::join!(soon(third.turn(&dir, root, Lang::Zh, id)), free);
        assert!(waited.expect("its turn").is_some());
    }

    /// A build waiting for its folder gives up when a newer one of its
    /// language starts, it is cancelled, or its window closes.
    #[test]
    fn a_waiting_build_gives_up_when_replaced_cancelled_or_closed() {
        let state = LatexState::new(PathBuf::new(), PathBuf::new());
        let first = state.begin(Lang::Zh);
        let en = state.begin(Lang::En);
        assert!(!state.superseded(Lang::Zh, first));
        let second = state.begin(Lang::Zh);
        assert!(state.superseded(Lang::Zh, first));
        assert!(!state.superseded(Lang::Zh, second));
        assert!(!state.superseded(Lang::En, en));
        state.stop();
        assert!(state.superseded(Lang::Zh, second));
        assert!(state.superseded(Lang::En, en));
    }

    #[test]
    fn issues_without_a_file_belong_to_a_lone_root() {
        let issues = vec![
            Issue {
                severity: Severity::Error,
                file: Some(".biwrite/zh/main.tex".into()),
                line: Some(3),
                message: "Undefined control sequence.".into(),
            },
            Issue {
                severity: Severity::Warning,
                file: None,
                line: Some(9),
                message: "Citation undefined".into(),
            },
        ];
        let views = issue_views(issues.clone(), "main.tex", true, Path::new(""), &[]);
        assert!(views.iter().all(|v| v.here));
        assert_eq!(views[0].file.as_deref(), Some("main.tex"));
        let views = issue_views(issues, "sections/a.tex", false, Path::new(""), &[]);
        assert!(views.iter().all(|v| !v.here));
    }

    #[test]
    fn sentences_are_selected_inside_paragraphs() {
        let text = "\\section{Intro}\nFirst sentence here. Second one follows.\n";
        let at = text.find("Second").unwrap();
        let r = sentence_or_line(text, at, 2);
        assert_eq!(&text[r.from..r.to], "Second one follows.");
    }
}
