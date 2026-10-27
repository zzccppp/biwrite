//! Commands from several windows, through IPC as the webviews send them:
//! each reaches the document of its own window, settings for every window
//! reach every window's engine, and a document's events go to its window.
//! Files opened go to the window that has them, to an empty window, or to
//! a new one; a window gone stops its document's work; builds into one
//! folder take turns.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use biwrite_core::Mode;
use biwrite_engine::EventSink;
use serde_json::{Value, json};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{
    INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
};
use tauri::webview::{InvokeRequest, WebviewWindow, WebviewWindowBuilder};
use tauri::{Listener, Manager, WindowEvent};

use crate::latex_commands::LatexState;
use crate::pair_tests::app_state;
use crate::sink::TauriSink;
use crate::state::{AppState, Doc};
use crate::windows::open_from;

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-win-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An app with windows `labels`, all sharing the state of `main`.
fn app_with(
    main: &Doc,
    labels: &[&str],
) -> (tauri::App<MockRuntime>, Vec<WebviewWindow<MockRuntime>>) {
    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![
            crate::commands::get_session,
            crate::settings_commands::set_batch_size,
        ])
        .build(mock_context(noop_assets()))
        .unwrap();
    app.manage(Arc::clone(main.app()));
    let windows = labels
        .iter()
        .map(|label| {
            WebviewWindowBuilder::new(&app, *label, Default::default())
                .build()
                .unwrap()
        })
        .collect();
    (app, windows)
}

fn invoke(window: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
    get_ipc_response(
        window,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|body| body.deserialize::<Value>().unwrap())
}

/// The second window's document, opened as BiWrite opens one.
fn second(main: &Doc, app: &tauri::App<MockRuntime>) -> Doc {
    main.app().open_document(app.handle(), "doc-2")
}

#[tokio::test(flavor = "multi_thread")]
async fn each_window_reaches_its_own_document() {
    let dir = temp("own");
    let main = app_state(&dir);
    let (app, windows) = app_with(&main, &["main", "doc-2", "doc-3"]);
    let second = second(&main, &app);
    main.engine
        .load("The first window's text.".into(), Mode::Plain);
    second
        .engine
        .load("The second window's text.".into(), Mode::Plain);

    let text = |w| invoke(w, "get_session", json!({})).map(|v| v["text"].clone());
    assert_eq!(text(&windows[0]), Ok(json!("The first window's text.")));
    assert_eq!(text(&windows[1]), Ok(json!("The second window's text.")));
    // A window without a document is refused, not given another's.
    let refused = text(&windows[2]).unwrap_err();
    assert!(refused.to_string().contains("doc-3"), "{refused}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_setting_for_every_window_reaches_every_engine() {
    let dir = temp("every");
    let main = app_state(&dir);
    let (app, windows) = app_with(&main, &["main", "doc-2"]);
    let second = second(&main, &app);

    invoke(&windows[1], "set_batch_size", json!({ "size": 4 })).unwrap();
    assert_eq!(main.engine.settings().batch_size, 4);
    assert_eq!(second.engine.settings().batch_size, 4);
    assert_eq!(main.settings().batch_size, 4);
    // A provider applied sets every engine's concurrency.
    main.settings().concurrency = 7;
    main.apply_active_provider().unwrap();
    let effective = main.settings().effective_concurrency();
    assert_eq!(main.engine.settings().concurrency, effective);
    assert_eq!(second.engine.settings().concurrency, effective);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_window_translates_with_the_current_settings() {
    let dir = temp("new");
    let main = app_state(&dir);
    let (app, _windows) = app_with(&main, &["main", "doc-2"]);
    {
        let mut s = main.settings();
        s.batch_size = 3;
        s.translate_delay = 9;
    }
    let second = second(&main, &app);
    let engine = second.engine.settings();
    assert_eq!(engine.batch_size, 3);
    assert_eq!(engine.edit_delay.as_secs(), 9);
    assert_eq!(engine.concurrency, main.settings().effective_concurrency());
    assert_eq!(main.app().documents().len(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_documents_events_go_to_its_window_only() {
    let dir = temp("events");
    let main = app_state(&dir);
    let (app, windows) = app_with(&main, &["main", "doc-2"]);
    let heard: Vec<Arc<AtomicUsize>> = windows
        .iter()
        .map(|w| {
            let n = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&n);
            w.listen("notice", move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            });
            n
        })
        .collect();
    let count = || {
        heard
            .iter()
            .map(|n| n.load(Ordering::SeqCst))
            .collect::<Vec<_>>()
    };
    TauriSink::for_window(app.handle().clone(), "doc-2").notice("for the second window");
    assert_eq!(count(), [0, 1]);
    // The request log's sink speaks to every window.
    TauriSink::new(app.handle().clone()).notice("for everyone");
    assert_eq!(count(), [1, 2]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn quitting_asks_about_any_window_with_changes() {
    let dir = temp("quit");
    let main = app_state(&dir);
    let (app, _windows) = app_with(&main, &["main", "doc-2"]);
    let second = second(&main, &app);
    let state: &Arc<AppState> = main.app();
    assert!(!state.needs_quit_confirmation());
    second.file().dirty = true;
    assert!(state.any_unsaved());
    assert!(state.needs_quit_confirmation());
    // Discarding one window's changes (closing it) leaves the quit question
    // to the others.
    second.confirm_discard_on_close();
    assert!(!state.needs_quit_confirmation());
    main.file().dirty = true;
    assert!(state.needs_quit_confirmation());
    // A quit cancelled at the second question: the first answer is
    // withdrawn too.
    state.withdraw_discards();
    assert!(second.needs_close_confirmation());
    main.confirm_discard_on_close();
    second.confirm_discard_on_close();
    assert!(!state.needs_quit_confirmation());
    // A window gone takes its document along.
    assert!(state.forget("doc-2").is_some());
    assert_eq!(state.documents().len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn write(dir: &Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn path_of(doc: &Doc) -> Option<std::path::PathBuf> {
    doc.file().path.clone()
}

/// Where `path`, opened from `window` (with document `doc`), went: loaded
/// here (its view), or elsewhere (`None`).
async fn open(
    app: &tauri::App<MockRuntime>,
    window: &WebviewWindow<MockRuntime>,
    doc: &Doc,
    path: &Path,
) -> Option<crate::state::SessionView> {
    let opened = open_from(app.handle(), window, doc, path.to_path_buf())
        .await
        .unwrap();
    assert!(opened.opened);
    opened.view
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_opens_here_only_in_an_empty_window() {
    let dir = temp("open");
    let a = write(&dir, "a.md", "First file.\n");
    let b = write(&dir, "b.md", "Second file.\n");
    let main = app_state(&dir);
    let (app, windows) = app_with(&main, &["main"]);

    // Nothing written in the window yet: the file takes its place.
    assert!(open(&app, &windows[0], &main, &a).await.is_some());
    assert_eq!(path_of(&main), Some(a.clone()));
    // Now it holds a file: the next one gets a window of its own.
    assert!(open(&app, &windows[0], &main, &b).await.is_none());
    assert_eq!(path_of(&main), Some(a.clone()));
    let docs = main.app().documents();
    assert_eq!(docs.len(), 2);
    let second: Doc = docs.into_iter().find(|d| d.label != "main").unwrap().into();
    assert_eq!(path_of(&second), Some(b.clone()));
    let there = app.get_webview_window(&second.label).expect("its window");
    // A file open in another window is shown there, not opened again.
    assert!(open(&app, &there, &second, &a).await.is_none());
    assert_eq!(main.app().documents().len(), 2);
    assert_eq!(path_of(&second), Some(b.clone()));
    // Its own file, opened again in its window, is read again there.
    std::fs::write(&b, "Second file, changed.\n").unwrap();
    let again = open(&app, &there, &second, &b).await.unwrap();
    assert_eq!(again.text, "Second file, changed.\n");
    assert_eq!(main.app().documents().len(), 2);
    // Nor is a file another window has open written over by a save.
    assert!(crate::commands::not_open_elsewhere(&second, [&a]).is_err());
    assert!(crate::commands::not_open_elsewhere(&second, [&b]).is_ok());
    // A file being opened at this moment, elsewhere, is not opened again.
    let c = write(&dir, "c.md", "Third file.\n");
    let claim = main.app().start_opening(&c).unwrap();
    assert!(open(&app, &there, &second, &c).await.is_none());
    assert_eq!(main.app().documents().len(), 2);
    drop(claim);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_paired_in_one_window_is_not_opened_in_another() {
    let dir = temp("paired");
    let en = write(&dir, "paper.tex", crate::pair_tests::EN);
    let zh = write(&dir, "paper_zh.tex", crate::pair_tests::ZH);
    let main = app_state(&dir);
    let (app, windows) = app_with(&main, &["main"]);
    open(&app, &windows[0], &main, &en).await.unwrap();
    assert!(main.file().pair.is_some(), "paper_zh.tex is its pair");

    let empty = crate::windows::open_window(app.handle(), Some(&windows[0]), None)
        .await
        .unwrap();
    let blank: Doc = main.app().document(empty.label()).unwrap().into();
    assert!(blank.is_blank());
    // Opened from the empty window, the paired file stays with its pair.
    assert!(open(&app, &empty, &blank, &zh).await.is_none());
    assert!(blank.is_blank());
    assert_eq!(main.app().documents().len(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_whose_pair_another_window_has_opens_alone() {
    let dir = temp("held");
    let en = write(&dir, "paper.tex", crate::pair_tests::EN);
    let zh = write(&dir, "paper_zh.tex", crate::pair_tests::ZH);
    let main = app_state(&dir);
    let (app, windows) = app_with(&main, &["main"]);
    // The Chinese file open on its own (its English one written later, say).
    crate::commands::open_path(&main, zh.clone()).await.unwrap();
    main.file().pair = None;
    assert!(open(&app, &windows[0], &main, &en).await.is_none());
    let english: Doc = main
        .app()
        .documents()
        .into_iter()
        .find(|d| d.label != "main")
        .unwrap()
        .into();
    assert_eq!(path_of(&english), Some(en.clone()));
    assert!(english.file().pair.is_none(), "not paired with main's file");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_whose_pair_is_being_opened_elsewhere_opens_alone() {
    let dir = temp("being");
    let en = write(&dir, "paper.tex", crate::pair_tests::EN);
    let zh = write(&dir, "paper_zh.tex", crate::pair_tests::ZH);
    let main = app_state(&dir);
    let (app, windows) = app_with(&main, &["main"]);
    // Another window is opening the Chinese file at this moment.
    let claim = main.app().start_opening(&zh).unwrap();
    open(&app, &windows[0], &main, &en).await.unwrap();
    assert_eq!(path_of(&main), Some(en.clone()));
    assert!(
        main.file().pair.is_none(),
        "not paired with a file being opened"
    );
    drop(claim);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_windows_number_serves_the_next_one() {
    let dir = temp("numbers");
    let main = app_state(&dir);
    let (app, _windows) = app_with(&main, &["main"]);
    let state = main.app();
    let (two, three) = (
        state.open_new_document(app.handle()),
        state.open_new_document(app.handle()),
    );
    assert_eq!(
        (two.label.as_str(), three.label.as_str()),
        ("doc-2", "doc-3")
    );
    state.forget("doc-2");
    // Its build folders (`.biwrite/zh-doc-2`, …) serve the next window.
    assert_eq!(state.open_new_document(app.handle()).label, "doc-2");
    assert_eq!(state.open_new_document(app.handle()).label, "doc-4");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_gone_stops_its_documents_work() {
    let dir = temp("gone");
    let main = app_state(&dir);
    let (app, windows) = app_with(&main, &["main", "doc-2"]);
    // Slow translations, so the window's are still on their way.
    *main.app().translator.lock().unwrap() = Arc::new(biwrite_engine::MockTranslator::with_delay(
        Duration::from_secs(5),
        Duration::from_secs(10),
    ));
    let second = second(&main, &app);
    second.engine.load(
        (0..40).map(|i| format!("Paragraph {i}.\n\n")).collect(),
        Mode::Plain,
    );
    assert!(second.engine.pending() > 0);
    let job = tokio::spawn(tokio::time::sleep(Duration::from_secs(3600)));
    second.assist.jobs().insert(7, job.abort_handle());

    let window = windows[1].as_ref().window();
    crate::on_window_event(&window, &WindowEvent::Focused(true));
    assert_eq!(main.app().focused().as_deref(), Some("doc-2"));
    crate::on_window_event(&window, &WindowEvent::Destroyed);
    assert!(main.app().document("doc-2").is_none());
    assert_eq!(main.app().focused(), None);
    assert_eq!(second.engine.pending(), 0);
    let ended = tokio::time::timeout(Duration::from_secs(5), job).await;
    assert!(ended.expect("the job stopped").unwrap_err().is_cancelled());
    assert!(second.assist.jobs().is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn each_window_builds_in_its_own_folders_and_in_place_builds_take_turns() {
    let dir = temp("builds");
    let base = LatexState::new(dir.join("t"), dir.join("u"));
    let (one, other) = (base.for_window("main"), base.for_window("doc-2"));
    // Copies of unsaved text and translations: each window its own PDFs.
    assert_eq!(one.folder(".biwrite/zh"), ".biwrite/zh");
    assert_eq!(other.folder(".biwrite/zh"), ".biwrite/zh-doc-2");
    let wait = |within: &'static str| {
        tokio::time::timeout(
            Duration::from_millis(100),
            other.build_alone(&dir, Path::new(within)),
        )
    };
    // In place, TeX writes next to the root: another window's build of the
    // same root waits ...
    let held = one.build_alone(&dir, Path::new("main.tex")).await;
    assert!(wait("main.tex").await.is_err());
    // ... one of another root, or into its own folder, does not.
    assert!(wait("main_zh.tex").await.is_ok());
    assert!(wait(".biwrite/zh-doc-2").await.is_ok());
    drop(held);
    assert!(wait("main.tex").await.is_ok());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Two windows on two files of one paper, both edited and not saved, build
/// at once: each its own PDF, in its own folder.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a TeX distribution"]
async fn two_windows_on_one_paper_build_their_own_pdfs() {
    use crate::latex_commands::{Lang, latex_compile, pdf_of};
    let dir = temp("two-builds");
    std::fs::create_dir_all(dir.join("sections")).unwrap();
    let main_tex = write(
        &dir,
        "main.tex",
        "\\documentclass{article}\n\\begin{document}\nThe main file.\n\n\\input{sections/intro}\n\\end{document}\n",
    );
    let intro = write(&dir, "sections/intro.tex", "The introduction.\n");
    let main = app_state(&dir);
    let (app, _windows) = app_with(&main, &["main", "doc-2"]);
    let second = second(&main, &app);
    crate::commands::open_path(&main, main_tex).await.unwrap();
    crate::commands::open_path(&second, intro).await.unwrap();
    let edited = |doc: &Doc, extra: &str| (doc.engine.text() + extra, doc.engine.document());
    let (a, da) = edited(&main, "\nEdited in the first window.\n");
    let (b, db) = edited(&second, "Edited in the second window.\n");
    let (one, two) = tokio::join!(
        latex_compile(main.clone(), Lang::En, a, da),
        latex_compile(second.clone(), Lang::En, b, db),
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert!(one.has_pdf, "{}", one.output);
    assert!(two.has_pdf, "{}", two.output);
    let (pdf_one, pdf_two) = (
        pdf_of(&main, Lang::En).unwrap(),
        pdf_of(&second, Lang::En).unwrap(),
    );
    assert_ne!(pdf_one, pdf_two);
    assert!(
        pdf_one.to_string_lossy().contains("/.biwrite/en/"),
        "{}",
        pdf_one.display()
    );
    assert!(
        pdf_two.to_string_lossy().contains("/.biwrite/en-doc-2/"),
        "{}",
        pdf_two.display()
    );
    assert!(pdf_one.is_file() && pdf_two.is_file());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn only_files_on_the_command_line_are_opened() {
    let dir = temp("args");
    write(&dir, "paper.tex", "x");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let elsewhere = write(&dir.join("sub"), "notes.md", "y");
    let args = ["-psn_0_1234", "paper.tex", "missing.tex", "sub"]
        .map(std::ffi::OsString::from)
        .into_iter()
        .chain([elsewhere.clone().into_os_string()]);
    assert_eq!(
        crate::windows::file_args(args, &dir),
        [dir.join("paper.tex"), elsewhere]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Files the system opens with BiWrite: the first into the empty window
/// (whose page hears of it), the next into a window of its own, one open
/// already shown where it is.
#[tokio::test(flavor = "multi_thread")]
async fn files_the_system_opens_go_to_an_empty_window_then_new_ones() {
    use crate::windows::{EVENT_OPENED, open_from_system};
    let dir = temp("system");
    let a = write(&dir, "a.md", "First file.\n");
    let b = write(&dir, "b.md", "Second file.\n");
    let main = app_state(&dir);
    let (app, windows) = app_with(&main, &["main"]);
    let heard = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&heard);
    windows[0].listen(EVENT_OPENED, move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    open_from_system(app.handle(), a.clone()).await.unwrap();
    assert_eq!(path_of(&main), Some(a.clone()));
    assert_eq!(heard.load(Ordering::SeqCst), 1, "the page loads it");
    open_from_system(app.handle(), b.clone()).await.unwrap();
    assert_eq!(path_of(&main), Some(a.clone()));
    assert_eq!(main.app().documents().len(), 2);
    // Again: shown where it is open.
    open_from_system(app.handle(), b.clone()).await.unwrap();
    open_from_system(app.handle(), a.clone()).await.unwrap();
    assert_eq!(main.app().documents().len(), 2);
    assert_eq!(heard.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn files_sent_before_setup_wait() {
    let pending = crate::windows::Pending::default();
    pending.add(vec!["a.tex".into()]);
    pending.add(vec!["b.md".into()]);
    assert_eq!(
        pending.take(),
        [std::path::PathBuf::from("a.tex"), "b.md".into()]
    );
    assert!(pending.take().is_empty());
}
