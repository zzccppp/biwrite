//! File IO and native dialogs. All file access happens here, in Rust; the
//! webview never sees or chooses paths directly.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use biwrite_core::TextFile;
use tauri::{AppHandle, Runtime, WebviewWindow, Window};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::sync::oneshot;

use crate::error::{CommandError, CommandResult};

const DOC_EXTENSIONS: &[&str] = &["tex", "md", "markdown", "txt"];

/// Native "Open" dialog. `None` if cancelled.
pub async fn pick_open<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
) -> Option<PathBuf> {
    let (tx, rx) = oneshot::channel();
    app.dialog()
        .file()
        .set_parent(window)
        .add_filter("Documents (.tex, .md, .txt)", DOC_EXTENSIONS)
        .pick_file(move |path| {
            // The receiver only disappears if the command was cancelled.
            let _ = tx.send(path);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// Native "Save As" dialog. `None` if cancelled.
pub async fn pick_save<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    current: Option<&Path>,
) -> Option<PathBuf> {
    let (tx, rx) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_parent(window)
        .add_filter("Documents (.tex, .md, .txt)", DOC_EXTENSIONS);
    match current {
        Some(path) => {
            if let Some(dir) = path.parent() {
                dialog = dialog.set_directory(dir);
            }
            if let Some(name) = path.file_name() {
                dialog = dialog.set_file_name(name.to_string_lossy());
            }
        }
        None => dialog = dialog.set_file_name("Untitled.md"),
    }
    dialog.save_file(move |path| {
        let _ = tx.send(path);
    });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// Native "Open" dialog for one kind of file (CSV, ...). `None` if cancelled.
pub async fn pick_open_kind<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    label: &str,
    extensions: &[&str],
) -> Option<PathBuf> {
    let (tx, rx) = oneshot::channel();
    app.dialog()
        .file()
        .set_parent(window)
        .add_filter(label, extensions)
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// Native "Save" dialog for an exported file, suggesting `file_name` in
/// `dir`. `None` if cancelled.
pub async fn pick_save_kind<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    label: &str,
    extensions: &[&str],
    dir: Option<&Path>,
    file_name: &str,
) -> Option<PathBuf> {
    let (tx, rx) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_parent(window)
        .add_filter(label, extensions)
        .set_file_name(file_name);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    dialog.save_file(move |path| {
        let _ = tx.send(path);
    });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// Read a small file whole (blocking IO off the async runtime), refusing
/// anything over `max_bytes`.
pub async fn read_small_file(path: PathBuf, max_bytes: u64) -> CommandResult<Vec<u8>> {
    tokio::task::spawn_blocking(move || {
        let len = fs::metadata(&path)
            .map_err(|e| CommandError::io(&path, e))?
            .len();
        if len > max_bytes {
            return Err(CommandError::Settings(format!(
                "{}: the file is too large ({} MB)",
                path.display(),
                len / 1_000_000
            )));
        }
        fs::read(&path).map_err(|e| CommandError::io(&path, e))
    })
    .await
    .map_err(|e| CommandError::Task(e.to_string()))?
}

/// Ask whether unsaved changes may be discarded.
pub fn ask_discard<R: Runtime>(
    app: &AppHandle<R>,
    parent: Option<&Window<R>>,
    on_answer: impl FnOnce(bool) + Send + 'static,
) {
    let mut dialog = app
        .dialog()
        .message("Your document has unsaved changes. Discard them?")
        .title("Unsaved changes")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Discard".into(),
            "Cancel".into(),
        ));
    if let Some(parent) = parent {
        dialog = dialog.parent(parent);
    }
    dialog.show(on_answer);
}

pub async fn confirm_discard<R: Runtime>(app: &AppHandle<R>, window: &WebviewWindow<R>) -> bool {
    let (tx, rx) = oneshot::channel();
    ask_discard(app, Some(&window.as_ref().window()), move |ok| {
        let _ = tx.send(ok);
    });
    rx.await.unwrap_or(false)
}

/// Read and decode a text file (blocking IO off the async runtime).
pub async fn read_text_file(path: PathBuf) -> CommandResult<TextFile> {
    tokio::task::spawn_blocking(move || {
        let bytes = fs::read(&path).map_err(|e| CommandError::io(&path, e))?;
        TextFile::decode(bytes).map_err(|source| CommandError::Decode {
            path: path.display().to_string(),
            source,
        })
    })
    .await
    .map_err(|e| CommandError::Task(e.to_string()))?
}

/// Distinguishes temp files of saves that might overlap.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` atomically: unique temp file in the same directory, fsync,
/// rename, fsync the directory. Symlinks are resolved so the link target is
/// updated, and the original file's permissions are kept.
pub async fn write_file_atomic(path: PathBuf, bytes: Vec<u8>) -> CommandResult<()> {
    tokio::task::spawn_blocking(move || write_atomic_blocking(&path, &bytes))
        .await
        .map_err(|e| CommandError::Task(e.to_string()))?
}

pub(crate) fn write_atomic_blocking(path: &Path, bytes: &[u8]) -> CommandResult<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".into());
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}-{n}.biwrite-tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Ok(meta) = fs::metadata(&target) {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        fs::rename(&tmp, &target)?;
        sync_dir(dir)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| CommandError::io(&target, e))
}

/// Persist the rename itself (directory entry) on Unix.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_content_byte_for_byte() {
        let dir = std::env::temp_dir().join(format!("biwrite-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.tex");
        fs::write(&path, b"old").unwrap();
        let bytes = "\u{feff}new\r\ncontent\r\n".as_bytes().to_vec();
        write_atomic_blocking(&path, &bytes).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".biwrite-tmp"))
            .collect();
        assert!(leftovers.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_then_save_unedited_is_byte_identical() {
        let dir = std::env::temp_dir().join(format!("biwrite-test-rt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("paper.tex");
        let original = b"\\section{Intro}\r\nMixed\nendings\r\n\r\nand trailing   \n".to_vec();
        fs::write(&path, &original).unwrap();
        let file = TextFile::decode(fs::read(&path).unwrap()).unwrap();
        // The editor round-trips the normalized text unchanged.
        let editor_text = file.text().to_owned();
        write_atomic_blocking(&path, &file.encode(&editor_text)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_dir_all(&dir).unwrap();
    }
}
