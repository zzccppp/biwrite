//! LaTeX: the TeX toolchain, building the open document's project (the
//! English PDF, or the Chinese mirror PDF), SyncTeX between the PDF and the
//! editor, the project's files, and paper templates.
//!
//! The English PDF is built from the files on disk (the frontend saves
//! first). The Chinese PDF is built from the composed translation, written
//! to `.biwrite/zh/` inside the project with Chinese font support added.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use biwrite_core::assist::{paragraph_at, sentence_at};
use biwrite_core::pair::units;
use biwrite_core::utf16::{byte_to_utf16, utf16_to_byte};
use biwrite_core::{Direction, Mode, SegmentId};
use biwrite_latex::{
    self as latex, EN_MIRROR_DIR, Engine, Issue, Job, MIRROR_DIR, Outcome, PdfBox, Severity,
    Template, Toolchain, templates,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State, WebviewWindow};
use tokio::task::AbortHandle;

use crate::commands;
use crate::error::{CommandError, CommandResult};
use crate::files;
use crate::latex_sync::{Basis, Place, line_of, line_start, map_line};
use crate::settings;
use crate::state::{AppState, SessionView};

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

pub struct LatexState {
    tex: tokio::sync::Mutex<TexCache>,
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
}

impl LatexState {
    pub fn new(builtin: PathBuf, user: PathBuf) -> Self {
        Self {
            tex: tokio::sync::Mutex::new(TexCache::default()),
            builds: Mutex::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            last_root: Mutex::new(None),
            builtin,
            user,
        }
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
async fn toolchain(state: &AppState, refresh: bool) -> Option<Toolchain> {
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

async fn require_toolchain(state: &AppState) -> CommandResult<Toolchain> {
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

async fn status(state: &AppState, refresh: bool) -> TexStatus {
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
pub async fn latex_status(state: State<'_, AppState>, refresh: bool) -> CommandResult<TexStatus> {
    Ok(status(&state, refresh).await)
}

fn save_latex_settings(
    state: &AppState,
    edit: impl FnOnce(&mut settings::LatexSettings),
) -> CommandResult<()> {
    let mut s = state.settings();
    edit(&mut s.latex);
    settings::save(&state.paths.settings, &s).map_err(CommandError::Settings)
}

#[tauri::command]
pub async fn latex_set_compile_on_save(state: State<'_, AppState>, on: bool) -> CommandResult<()> {
    save_latex_settings(&state, |l| l.compile_on_save = on)
}

/// Choose the folder that holds `pdflatex` (and the other TeX programs).
#[tauri::command]
pub async fn latex_choose_bin(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
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
pub async fn latex_reset_bin(state: State<'_, AppState>) -> CommandResult<TexStatus> {
    save_latex_settings(&state, |l| l.tex_bin = None)?;
    Ok(status(&state, true).await)
}

// ── The project ──────────────────────────────────────────────────────

/// The open document's project: root file, folder and engine.
struct Project {
    /// The open `.tex` file.
    path: PathBuf,
    /// Its text on disk.
    saved: String,
    root: PathBuf,
    root_text: String,
    /// Folder of the root (canonical).
    dir: PathBuf,
}

fn is_tex(path: &Path) -> bool {
    path.extension()
        .is_some_and(|x| x.eq_ignore_ascii_case("tex"))
}

/// `path` relative to `dir`, `/`-separated.
fn relative(path: &Path, dir: &Path) -> Option<String> {
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

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| latex::clean(path))
}

fn project(state: &AppState) -> CommandResult<Project> {
    let (path, saved) = {
        let fs = state.file();
        (fs.path.clone(), fs.file.text().to_owned())
    };
    let path = path
        .filter(|p| is_tex(p))
        .ok_or_else(|| fail("Save the document as a .tex file first."))?;
    project_of(state, &path, saved)
}

/// The project of the `.tex` file at `path` whose text on disk is `saved`.
fn project_of(state: &AppState, path: &Path, saved: String) -> CommandResult<Project> {
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
    *state
        .latex
        .last_root
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(root.clone());
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
pub async fn latex_project(state: State<'_, AppState>) -> CommandResult<Option<ProjectView>> {
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
    state: State<'_, AppState>,
    file: String,
) -> CommandResult<Option<SessionView>> {
    let p = project(&state)?;
    let wanted = latex::tex_files(&p.dir)
        .into_iter()
        .map(|f| canonical(&f))
        .find(|f| relative(f, &p.dir).as_deref() == Some(file.as_str()))
        .ok_or_else(|| fail(format!("{file} is not a .tex file of this project")))?;
    if state.is_dirty() && !files::confirm_discard(&app, &window).await {
        return Ok(None);
    }
    commands::load(&window, &state, wanted).await.map(Some)
}

/// Open a LaTeX project folder: its main file.
#[tauri::command]
pub async fn latex_open_folder(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CommandResult<Option<SessionView>> {
    if state.is_dirty() && !files::confirm_discard(&app, &window).await {
        return Ok(None);
    }
    let Some(dir) = files::pick_folder(&app, &window, "Open a LaTeX project folder").await else {
        return Ok(None);
    };
    let main = latex::find_main(&dir).ok_or_else(|| {
        fail(format!(
            "{} has no main .tex file (one with \\documentclass and \\begin{{document}})",
            dir.display()
        ))
    })?;
    commands::load(&window, &state, main).await.map(Some)
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
}

/// A file named in the log (relative to the folder TeX ran in), as the
/// original file: the mirror's copies map back to what they mirror.
fn original(name: &str) -> String {
    let name = name.trim_start_matches("./");
    [MIRROR_DIR, EN_MIRROR_DIR]
        .iter()
        .find_map(|dir| name.strip_prefix(dir))
        .map(|rest| rest.trim_start_matches('/'))
        .unwrap_or(name)
        .to_owned()
}

fn issue_views(issues: Vec<Issue>, doc_rel: &str, doc_is_root: bool) -> Vec<IssueView> {
    issues
        .into_iter()
        .map(|i| {
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

/// Build the PDF of the file's own language (from the files on disk) or of
/// the other language (the Chinese PDF of an English paper, the English one
/// of a Chinese paper: from the translation). `text` is the editor's
/// current text. A new build of the same language replaces a running one.
#[tauri::command]
pub async fn latex_compile(
    state: State<'_, AppState>,
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
    // language as it is on disk.
    let mirror = {
        let fs = state.file();
        fs.pair
            .as_ref()
            .filter(|_| lang != source_lang(direction))
            .map(|pair| (pair.path.clone(), pair.file.text().to_owned()))
    };
    let p = match mirror {
        Some((path, saved)) => project_of(&state, &path, saved)?,
        None => project(&state)?,
    };
    let paired = state.file().pair.is_some();
    let doc_rel = relative(&p.path, &p.dir)
        .ok_or_else(|| fail("The document is outside its project folder."))?;
    let root_rel = relative(&p.root, &p.dir).unwrap_or_default();
    let doc_is_root = p.path == p.root;
    let mut untranslated = 0;
    let (job, doc_file, basis, source) = match lang {
        _ if paired || lang == file_lang => (
            Job {
                dir: p.dir.clone(),
                root: PathBuf::from(&root_rel),
                engine: latex::project_engine(&p.root, &p.root_text),
                out_dir: None,
                timeout: BUILD_TIMEOUT,
            },
            p.path.clone(),
            Basis {
                compiled: p.saved.clone(),
                editor: text,
                same_language: lang == source_lang(direction),
                mode: Mode::Latex,
            },
            Source::Files,
        ),
        _ => {
            // The editor's text if it is in this language, else composed
            // from the translations.
            let other = if lang == source_lang(direction) {
                text.clone()
            } else {
                let mirror = state.engine.compose_mirror();
                untranslated = mirror.untranslated;
                mirror.text
            };
            // Chinese needs font support; an English mirror keeps the
            // Chinese file's preamble, which has it.
            let (folder, prepare): (&str, fn(&str) -> String) = match lang {
                Lang::Zh => (MIRROR_DIR, latex::with_chinese),
                Lang::En => (EN_MIRROR_DIR, str::to_owned),
            };
            let out = p.dir.join(folder);
            let doc_out = out.join(&doc_rel);
            if doc_is_root {
                write_file(&doc_out, &prepare(&other))?;
            } else {
                write_file(&doc_out, &other)?;
                let from = doc_rel.trim_end_matches(".tex");
                let to = format!("{folder}/{from}");
                let root = latex::redirect_include(&p.root_text, from, &to).ok_or_else(|| {
                    fail(format!(
                        "The translated PDF needs {root_rel} to \\input or \\include {doc_rel} directly."
                    ))
                })?;
                write_file(&out.join(&root_rel), &prepare(&root))?;
            }
            let engine = match (lang, latex::engine_for(&p.root_text)) {
                (Lang::Zh, Engine::Lualatex) => Engine::Lualatex,
                (Lang::Zh, _) => Engine::Xelatex,
                (Lang::En, _) => latex::project_engine(&p.root, &p.root_text),
            };
            (
                Job {
                    dir: p.dir.clone(),
                    root: Path::new(folder).join(&root_rel),
                    engine,
                    out_dir: Some(PathBuf::from(folder)),
                    timeout: BUILD_TIMEOUT,
                },
                canonical(&doc_out),
                Basis {
                    compiled: other,
                    editor: text,
                    same_language: lang == source_lang(direction),
                    mode: Mode::Latex,
                },
                Source::Translation,
            )
        }
    };
    let engine = job.engine;

    let id = state.latex.next.fetch_add(1, Ordering::Relaxed);
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
        "built {lang:?} PDF of {root_rel}: {:?} in {} ms",
        built.outcome,
        built.duration_ms
    );
    let view = BuildView {
        id,
        lang,
        outcome: built.outcome,
        has_pdf: built.pdf.is_some(),
        stale: built.stale,
        issues: issue_views(built.issues, &doc_rel, doc_is_root),
        duration_ms: built.duration_ms,
        tool: built.tool,
        engine,
        root: root_rel,
        untranslated,
        output: built.output,
        project_rc: built.project_rc,
    };
    if let Some(rc) = &view.project_rc {
        log::warn!("the build ran the project's own {rc}");
    }
    state.latex.builds().insert(
        lang,
        Build {
            pdf: built.pdf,
            dir: p.dir,
            doc_file,
            doc_path: state
                .file()
                .path
                .as_deref()
                .map(canonical)
                .unwrap_or_default(),
            basis,
            source,
            direction,
        },
    );
    Ok(view)
}

/// What a build of `lang` is made from now (as [`latex_compile`] decides).
fn source_now(state: &AppState, lang: Lang) -> Source {
    let fs = state.file();
    if fs.pair.is_some() || lang == source_lang(fs.home) {
        Source::Files
    } else {
        Source::Translation
    }
}

/// The PDF of `lang` still belongs to the document as it is edited now.
/// It does not when a pairing or unpairing since the build changed what
/// the PDF is made from (the translation, or the paired file), nor after a
/// swap without a pair, which put the other language in the editor. Such a
/// PDF is built again before clicks or the cursor map through it.
fn build_is_current(state: &AppState, lang: Lang) -> bool {
    let builds = state.latex.builds();
    let Some(build) = builds.get(&lang) else {
        return true;
    };
    let paired = state.file().pair.is_some();
    build.source == source_now(state, lang)
        && (paired || build.direction == state.engine.direction())
}

/// The editor holds the open file's own language (as on disk, give or
/// take unsaved edits): always with a pair, else unless swapped.
fn editing_own_language(state: &AppState) -> bool {
    let fs = state.file();
    fs.pair.is_some() || state.engine.direction() == fs.home
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

/// A line of the paired mirror (as on disk) as the range of the linked
/// paragraph in the editor's `text`. `None` unless `file` is the mirror.
fn mirror_hit(
    state: &AppState,
    file: &Path,
    line: usize,
    text: &str,
    document: u64,
) -> Option<Range16> {
    let id = {
        let fs = state.file();
        let pair = fs.pair.as_ref().filter(|p| canonical(&p.path) == file)?;
        let mirror = pair.file.text();
        let at = line_start(mirror, line.saturating_sub(1));
        let ub = units(mirror, state.engine.mode());
        let j = ub.iter().rposition(|u| u.range.start <= at).unwrap_or(0);
        let by_unit: HashMap<usize, SegmentId> =
            pair.links.iter().map(|(id, u)| (*u, *id)).collect();
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

/// The mirror's file and line for the paragraph at `offset` (UTF-16) of
/// the editor's `text`.
fn mirror_line(
    state: &AppState,
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
    let fs = state.file();
    let pair = fs.pair.as_ref()?;
    let unit = ids
        .iter()
        .rev()
        .find_map(|id| pair.links.get(id).copied())?;
    let mirror = pair.file.text();
    let u = units(mirror, state.engine.mode()).into_iter().nth(unit)?;
    Some((canonical(&pair.path), line_of(mirror, u.content.start) + 1))
}

#[tauri::command]
pub async fn latex_cancel(state: State<'_, AppState>, lang: Lang) -> CommandResult<()> {
    if let Some((_, task)) = state.latex.running().remove(&lang) {
        task.abort();
    }
    Ok(())
}

fn pdf_of(state: &AppState, lang: Lang) -> CommandResult<PathBuf> {
    state
        .latex
        .builds()
        .get(&lang)
        .and_then(|b| b.pdf.clone())
        .ok_or_else(|| fail("There is no PDF yet: compile first."))
}

/// The PDF's bytes, for the viewer.
#[tauri::command]
pub async fn latex_pdf(
    state: State<'_, AppState>,
    lang: Lang,
) -> CommandResult<tauri::ipc::Response> {
    let pdf = pdf_of(&state, lang)?;
    let bytes = files::read_small_file(pdf, MAX_PDF_BYTES).await?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub async fn latex_reveal_pdf(state: State<'_, AppState>, lang: Lang) -> CommandResult<()> {
    files::reveal_file(&pdf_of(&state, lang)?)
}

/// Save a copy of the PDF (the Chinese one lives in a hidden folder).
#[tauri::command]
pub async fn latex_save_pdf(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
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
    state: State<'_, AppState>,
    text: String,
    document: u64,
) -> CommandResult<Option<String>> {
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
        state.engine.compose_mirror().text
    };
    let path = state.file().path.clone();
    let stem = path
        .as_deref()
        .and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "paper".into());
    let body = if target == Lang::Zh && latex::is_main(&translated) {
        latex::with_chinese(&translated)
    } else {
        translated
    };
    let dir = path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let name = match target {
        Lang::Zh => format!("{stem}_zh.tex"),
        Lang::En => format!("{}_en.tex", without_chinese_tag(&stem)),
    };
    let Some(dest) =
        files::pick_save_kind(&app, &window, "LaTeX", &["tex"], dir.as_deref(), &name).await
    else {
        return Ok(None);
    };
    files::write_file_atomic(dest.clone(), body.into_bytes()).await?;
    Ok(Some(dest.display().to_string()))
}

// ── SyncTeX ──────────────────────────────────────────────────────────

/// UTF-16 range in the editor's text.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Range16 {
    pub from: usize,
    pub to: usize,
}

fn range16(text: &str, from: usize, to: usize) -> Range16 {
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
    state: State<'_, AppState>,
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
    if let Some(range) = mirror_hit(&state, &file, line, &text, document) {
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
    let same_file_on_disk = editing_own_language(&state);
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
    state: &AppState,
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
    state: &AppState,
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
    let others = latex::tex_files(&build.dir)
        .into_iter()
        .map(|f| canonical(&f))
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
fn file_to_open(state: &AppState, file: &Path) -> Option<String> {
    let dir = project(state).ok()?.dir;
    let editing_zh = state.engine.direction() == Direction::ZhEn;
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let target = if crate::pairing::is_chinese(&text) != editing_zh {
        crate::pairing::counterpart(file).map(|c| canonical(&c))?
    } else {
        file.to_path_buf()
    };
    relative(&target, &dir).filter(|r| r.ends_with(".tex"))
}

/// Where the editor's position `offset` (UTF-16) is in the PDF.
#[tauri::command]
pub async fn latex_forward(
    state: State<'_, AppState>,
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
        && let Some((input, line)) = mirror_line(&state, offset, &text, document)
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
        } else if let Some(path) = current.filter(|_| editing_own_language(&state)) {
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
    state: State<'_, AppState>,
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

fn template_err(e: latex::TemplateError) -> CommandError {
    fail(e.to_string())
}

#[tauri::command]
pub async fn latex_templates(state: State<'_, AppState>) -> CommandResult<Vec<Template>> {
    let (builtin, user) = (state.latex.builtin.clone(), state.latex.user.clone());
    tokio::task::spawn_blocking(move || templates::list(&builtin, &user))
        .await
        .map_err(|e| CommandError::Task(e.to_string()))
}

/// A new paper from template `id`, in a folder the user names; opens its
/// main file.
#[tauri::command]
pub async fn latex_new_paper(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<Option<SessionView>> {
    let (dir, t) =
        templates::find(&state.latex.builtin, &state.latex.user, &id).map_err(template_err)?;
    if state.is_dirty() && !files::confirm_discard(&app, &window).await {
        return Ok(None);
    }
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
    commands::load(&window, &state, main).await.map(Some)
}

/// Import a template from a project folder or a `.zip`.
#[tauri::command]
pub async fn latex_import_template(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
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
    state: State<'_, AppState>,
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
pub async fn latex_delete_template(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    templates::delete(&state.latex.builtin, &state.latex.user, &id).map_err(template_err)
}

#[tauri::command]
pub async fn latex_reveal_templates(state: State<'_, AppState>) -> CommandResult<()> {
    files::reveal(&state.latex.user)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_of(source: Source, direction: Direction) -> Build {
        Build {
            pdf: None,
            dir: PathBuf::new(),
            doc_file: PathBuf::new(),
            doc_path: PathBuf::new(),
            basis: Basis {
                compiled: String::new(),
                editor: String::new(),
                same_language: true,
                mode: Mode::Latex,
            },
            source,
            direction,
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

    #[test]
    fn mirror_files_map_to_their_originals() {
        assert_eq!(original("./main.tex"), "main.tex");
        assert_eq!(
            original(".biwrite/zh/sections/intro.tex"),
            "sections/intro.tex"
        );
        assert_eq!(original("sections/intro.tex"), "sections/intro.tex");
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
        let views = issue_views(issues.clone(), "main.tex", true);
        assert!(views.iter().all(|v| v.here));
        assert_eq!(views[0].file.as_deref(), Some("main.tex"));
        let views = issue_views(issues, "sections/a.tex", false);
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
