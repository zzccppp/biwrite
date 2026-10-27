//! BiWrite's windows, each with a document of its own. A file opened goes
//! to the window that has it open already (as its document or the paired
//! file), to the window it is opened from if that holds nothing yet, or to
//! a new window. Files the system opens with BiWrite (Finder's Open With,
//! Explorer's Open with, a second launch with files) go the same way, an
//! empty window taking the first.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow, WebviewWindowBuilder};

use crate::commands;
use crate::error::{CommandError, CommandResult};
use crate::files;
use crate::state::{AppState, Doc, Document, SessionView};

/// How far a new window sits below and to the right of the one it was
/// opened from (logical pixels).
const CASCADE: f64 = 28.0;

/// A window's document was replaced from outside, by a file the system
/// opened in it: the new view (the page loads it).
pub const EVENT_OPENED: &str = "document-opened";

/// Files the system asked BiWrite to open before it was set up (macOS sends
/// them before the app has finished launching): opened once it is.
#[derive(Default)]
pub struct Pending(Mutex<Vec<PathBuf>>);

impl Pending {
    pub fn add(&self, paths: Vec<PathBuf>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(paths);
    }

    pub fn take(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// The files among command-line `args` (after the program), relative ones
/// taken from `cwd`; options, folders and anything else left out.
pub fn file_args(args: impl IntoIterator<Item = OsString>, cwd: &Path) -> Vec<PathBuf> {
    args.into_iter()
        .map(|a| cwd.join(a))
        .filter(|p| p.is_file())
        .collect()
}

/// Open `paths`, which the system asked BiWrite to open, one after the
/// other (see [`open_from_system`]), in the background.
pub(crate) fn open_all<R: Runtime>(app: &AppHandle<R>, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for path in paths {
            if let Err(e) = open_from_system(&app, path.clone()).await {
                log::error!("could not open {}: {e}", path.display());
            }
        }
    });
}

/// Open `path`, which the system asked BiWrite to open: shown in the window
/// that has it, else in an empty window (the one used last first), else in
/// a new window.
pub(crate) async fn open_from_system<R: Runtime>(
    app: &AppHandle<R>,
    path: PathBuf,
) -> CommandResult<()> {
    let state = Arc::clone(app.state::<Arc<AppState>>().inner());
    // Asked twice at once (two launches with the file): opened once.
    let Some(_opening) = state.start_opening(&path) else {
        return Ok(());
    };
    // No window is excluded: any that has it shows it.
    if let Some(there) = state
        .window_with(&path, "")
        .and_then(|label| app.get_webview_window(&label))
    {
        bring_forward(&there);
        return Ok(());
    }
    let blank = |w: &WebviewWindow<R>| state.document(w.label()).is_some_and(|d| d.is_blank());
    let empty = current_window(app)
        .filter(blank)
        .or_else(|| document_windows(app).into_iter().find(blank));
    let Some(window) = empty else {
        let window = open_window(app, current_window(app).as_ref(), Some(path)).await?;
        bring_forward(&window);
        return Ok(());
    };
    let Some(doc) = state.document(window.label()) else {
        return Ok(());
    };
    let view = commands::load(&window, &Doc::from(doc), path).await?;
    // A page still starting reads it with `get_session` instead.
    if let Err(e) = app.emit_to(window.label(), EVENT_OPENED, &view) {
        log::warn!("failed to emit {EVENT_OPENED}: {e}");
    }
    bring_forward(&window);
    Ok(())
}

/// Open a new window with an untitled document.
#[tauri::command]
pub async fn new_window(app: AppHandle, window: WebviewWindow) -> CommandResult<()> {
    open_window(&app, Some(&window), None).await.map(|_| ())
}

/// What became of a file the author opened.
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Opened {
    /// Loaded in the window it was opened from.
    pub view: Option<SessionView>,
    /// Opened, here or in another window (not when the author cancelled).
    pub opened: bool,
}

/// Open `path`, chosen in `window` (whose document is `state`): shown in the
/// window that has it, else here when this window holds nothing yet or has
/// it itself (then read again, asking first about unsaved changes), else in
/// a new window.
pub(crate) async fn open_from<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    state: &Doc,
    path: PathBuf,
) -> CommandResult<Opened> {
    let elsewhere = Opened {
        view: None,
        opened: true,
    };
    if shown_elsewhere(app, state, &path) {
        return Ok(elsewhere);
    }
    let here = state.is_blank() || state.holds(&path);
    // Read again over unsaved changes (this window's own file) only if the
    // author agrees.
    if here && state.is_dirty() && !files::confirm_discard(app, window).await {
        return Ok(Opened::default());
    }
    // Opened in another window at the same moment (or while the author
    // was asked): it shows there.
    let Some(_opening) = state.start_opening(&path) else {
        return Ok(elsewhere);
    };
    if shown_elsewhere(app, state, &path) {
        return Ok(elsewhere);
    }
    if !here {
        open_window(app, Some(window), Some(path)).await?;
        return Ok(elsewhere);
    }
    let view = commands::load(window, state, path).await?;
    Ok(Opened {
        view: Some(view),
        opened: true,
    })
}

/// `path` is open in a window other than `state`'s (as its document or the
/// paired file): that window is brought forward.
pub(crate) fn shown_elsewhere<R: Runtime>(
    app: &AppHandle<R>,
    state: &Document,
    path: &Path,
) -> bool {
    let Some(label) = state.window_with(path, &state.label) else {
        return false;
    };
    if let Some(there) = app.get_webview_window(&label) {
        bring_forward(&there);
    }
    true
}

/// Open a window with a document of its own: the file at `path`, or
/// untitled. Opened from `from`, it sits a little below and to the right,
/// as large, and a part of a project it opens stays in that window's project.
pub(crate) async fn open_window<R: Runtime>(
    app: &AppHandle<R>,
    from: Option<&WebviewWindow<R>>,
    path: Option<PathBuf>,
) -> CommandResult<WebviewWindow<R>> {
    let state = Arc::clone(app.state::<Arc<AppState>>().inner());
    let doc = state.open_new_document(app);
    let label = doc.label.clone();
    if let Some(parent) = from.and_then(|w| state.document(w.label())) {
        doc.latex.follow(&parent.latex);
    }
    let made = async {
        if let Some(path) = path {
            // Read before the window shows: its first `get_session` has it.
            let _no_save_meanwhile = doc.save_lock.lock().await;
            commands::open_path(&doc, path).await?;
        }
        let title = doc.file().window_title();
        build(app, &label, &title, from)
    }
    .await;
    if made.is_err()
        && let Some(doc) = state.forget(&label)
    {
        doc.shut_down();
    }
    made
}

/// The window itself, as the main window is configured.
fn build<R: Runtime>(
    app: &AppHandle<R>,
    label: &str,
    title: &str,
    from: Option<&WebviewWindow<R>>,
) -> CommandResult<WebviewWindow<R>> {
    let mut config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == crate::MAIN_WINDOW)
        .cloned()
        .unwrap_or_default();
    config.label = label.to_owned();
    config.title = title.to_owned();
    if let Some(place) = from.and_then(next_to) {
        (config.x, config.y) = (Some(place.x), Some(place.y));
        if let Some((width, height)) = place.size {
            (config.width, config.height) = (width, height);
        }
        config.center = false;
    }
    WebviewWindowBuilder::from_config(app, &config)
        .and_then(|builder| builder.build())
        .map_err(|e| CommandError::Window(e.to_string()))
}

/// Where a new window goes, and its size if not the default (logical
/// pixels).
struct Place {
    x: f64,
    y: f64,
    size: Option<(f64, f64)>,
}

/// Where a window opened from `from` goes, and its size: `from`'s, unless
/// that fills the screen.
fn next_to<R: Runtime>(from: &WebviewWindow<R>) -> Option<Place> {
    let scale = from.scale_factor().ok()?;
    let at = from.outer_position().ok()?.to_logical::<f64>(scale);
    let full = from.is_maximized().unwrap_or(false) || from.is_fullscreen().unwrap_or(false);
    let size = if full {
        None
    } else {
        from.inner_size()
            .ok()
            .map(|s| s.to_logical::<f64>(scale))
            .map(|s| (s.width, s.height))
    };
    Some(Place {
        x: at.x + CASCADE,
        y: at.y + CASCADE,
        size,
    })
}

/// Show `window` in front (it may be minimized, or hidden in the tray).
pub(crate) fn bring_forward<R: Runtime>(window: &WebviewWindow<R>) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

/// The windows that show a document, in the order they were opened.
pub(crate) fn document_windows<R: Runtime>(app: &AppHandle<R>) -> Vec<WebviewWindow<R>> {
    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return Vec::new();
    };
    let mut labels: Vec<String> = state.documents().iter().map(|d| d.label.clone()).collect();
    // `main`, then `doc-2`, `doc-3`, … `doc-10`.
    labels.sort_by_key(|label| {
        label
            .strip_prefix("doc-")
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0)
    });
    labels
        .iter()
        .filter_map(|label| app.get_webview_window(label))
        .collect()
}

/// The window to show or to ask the author's attention in: the one that
/// had the focus last, else any.
pub(crate) fn current_window<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    app.try_state::<Arc<AppState>>()
        .and_then(|s| s.focused())
        .and_then(|label| app.get_webview_window(&label))
        .or_else(|| document_windows(app).into_iter().next())
}
