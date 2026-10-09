//! BiWrite desktop app: wires the engine to Tauri (commands, events, dialogs).

mod commands;
mod error;
mod files;
mod glossary_commands;
mod log_commands;
mod provider_state;
mod request_log;
mod secrets;
mod settings;
mod settings_commands;
mod sink;
mod state;

#[cfg(test)]
mod leak_tests;

use std::sync::Arc;

use biwrite_engine::{
    Engine, EngineSettings, MemoryCache, MockTranslator, SqliteCache, TranslationCache,
};
use biwrite_providers::{PromptFiles, RequestObserver};
use tauri::{App, AppHandle, Manager, RunEvent, Window, WindowEvent};

use crate::provider_state::Built;
use crate::request_log::RequestLog;
use crate::secrets::{Keychain, SecretStore};
use crate::settings::Paths;
use crate::sink::TauriSink;
use crate::state::AppState;

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let paths = Paths::in_dir(&config_dir);
            settings::ensure_prompt_files(&paths.prompts);
            let settings = settings::load(&paths.settings);
            let secrets: Arc<dyn SecretStore> = Arc::new(Keychain);
            let sink = Arc::new(TauriSink::new(app.handle().clone()));
            let request_log = Arc::new(RequestLog::new(
                settings.request_log,
                request_log_file(app),
                sink.clone(),
            ));
            let built = initial_translator(&settings, &secrets, &paths, request_log.clone());
            let engine = Engine::new(
                built.translator,
                open_cache(app),
                sink,
                EngineSettings {
                    concurrency: settings.concurrency,
                    ..EngineSettings::default()
                },
                tauri::async_runtime::handle().inner().clone(),
            );
            engine.set_glossary(settings.glossary.clone());
            let active_id = settings.active().config.id.clone();
            let state = AppState::new(engine, settings, paths, secrets, request_log);
            state.set_translation_http(active_id, built.http);
            if let Some(path) = std::env::args_os().nth(1).map(std::path::PathBuf::from) {
                // `biwrite paper.tex`: open a file from the command line.
                match state.load_path_blocking(path) {
                    Ok(()) => {
                        if let Some(window) = app.get_webview_window("main") {
                            window.set_title(&state.file().window_title())?;
                        }
                    }
                    Err(e) => eprintln!("BiWrite: {e}"),
                }
            }
            app.manage(state);
            Ok(())
        })
        .on_window_event(on_window_event)
        .invoke_handler(tauri::generate_handler![
            commands::get_session,
            commands::open_file,
            commands::save_file,
            commands::save_file_as,
            commands::update_document,
            commands::set_mode,
            commands::retranslate_segment,
            commands::retranslate_all,
            commands::set_auto_translate,
            commands::set_dirty,
            commands::swap_languages,
            settings_commands::get_settings,
            settings_commands::save_provider,
            settings_commands::delete_provider,
            settings_commands::set_api_key,
            settings_commands::add_api_keys,
            settings_commands::remove_api_key,
            settings_commands::clear_api_key,
            settings_commands::key_status,
            settings_commands::set_assistant_provider,
            settings_commands::set_active_provider,
            settings_commands::test_provider,
            settings_commands::list_provider_models,
            settings_commands::set_concurrency,
            settings_commands::set_doc_note,
            settings_commands::get_prompt,
            settings_commands::save_prompt,
            settings_commands::reveal_prompts,
            glossary_commands::get_glossary,
            glossary_commands::save_glossary,
            glossary_commands::import_glossary,
            glossary_commands::export_glossary,
            glossary_commands::export_bilingual,
            log_commands::get_request_log,
            log_commands::set_request_log,
            log_commands::clear_request_log,
            log_commands::reveal_request_log,
        ])
        .build(tauri::generate_context!());

    match app {
        Ok(app) => app.run(on_run_event),
        Err(e) => {
            eprintln!("BiWrite failed to start: {e}");
            std::process::exit(1);
        }
    }
}

/// The active provider's translator (keys are read lazily, on first use);
/// the mock if it can't be built.
fn initial_translator(
    settings: &settings::AppSettings,
    secrets: &Arc<dyn SecretStore>,
    paths: &Paths,
    observer: Arc<dyn RequestObserver>,
) -> Built {
    let prompts = Arc::new(PromptFiles {
        dir: paths.prompts.clone(),
    });
    provider_state::translator_for(settings.active(), secrets, prompts, observer).unwrap_or_else(
        |e| {
            eprintln!("BiWrite: provider unavailable ({e}); using the mock translator");
            Built {
                translator: Arc::new(MockTranslator::default()),
                http: None,
            }
        },
    )
}

/// `requests.jsonl` in the app data folder, if the folder is available.
fn request_log_file(app: &App) -> Option<std::path::PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("requests.jsonl"))
}

/// The persistent translation cache in the app data folder; falls back to an
/// in-memory cache (with a warning) so the app still works if it can't open.
fn open_cache(app: &App) -> Arc<dyn TranslationCache> {
    let opened = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())
        .and_then(|dir| {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            SqliteCache::open(&dir.join("cache.sqlite3")).map_err(|e| e.to_string())
        });
    match opened {
        Ok(cache) => Arc::new(cache),
        Err(e) => {
            eprintln!("BiWrite: translation cache unavailable ({e}); using a temporary one");
            Arc::new(MemoryCache::default())
        }
    }
}

/// Ask before closing a window with unsaved changes.
fn on_window_event(window: &Window, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    if !window.state::<AppState>().needs_close_confirmation() {
        return;
    }
    api.prevent_close();
    let target = window.clone();
    files::ask_discard(window.app_handle(), Some(window), move |discard| {
        if discard {
            target.state::<AppState>().confirm_discard_on_close();
            if let Err(e) = target.destroy() {
                eprintln!("BiWrite: failed to close window: {e}");
            }
        }
    });
}

/// Ask before quitting (e.g. Cmd+Q) with unsaved changes.
fn on_run_event(app: &AppHandle, event: RunEvent) {
    let RunEvent::ExitRequested { api, .. } = event else {
        return;
    };
    if !app.state::<AppState>().needs_close_confirmation() {
        return;
    }
    api.prevent_exit();
    let handle = app.clone();
    let parent = app.get_webview_window("main").map(|w| w.as_ref().window());
    files::ask_discard(app, parent.as_ref(), move |discard| {
        if discard {
            handle.state::<AppState>().confirm_discard_on_close();
            handle.exit(0);
        }
    });
}
