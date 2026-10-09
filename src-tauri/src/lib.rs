//! BiWrite desktop app: wires the engine to Tauri (commands, events, dialogs).

mod commands;
mod error;
mod files;
mod glossary_commands;
mod logging;
mod provider_state;
mod secrets;
mod settings;
mod settings_commands;
mod sink;
mod state;
mod storage_commands;

#[cfg(test)]
mod leak_tests;

use std::path::PathBuf;
use std::sync::Arc;

use biwrite_engine::{
    Engine, EngineSettings, MemoryCache, MockTranslator, SqliteCache, TranslationCache, Translator,
};
use biwrite_providers::PromptFiles;
use tauri::{App, AppHandle, Manager, RunEvent, Window, WindowEvent};

use crate::secrets::{Keychain, SecretStore};
use crate::settings::Paths;
use crate::sink::TauriSink;
use crate::state::AppState;

pub fn run() {
    // First, so even failures before setup (e.g. no WebView2) are reported.
    logging::init();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let log_dir = app.path().app_log_dir().ok();
            if let Some(dir) = &log_dir {
                logging::attach_dir(dir.clone());
            }
            log::info!(
                "BiWrite {} starting ({} {})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            let config_dir = app.path().app_config_dir()?;
            let mut paths = Paths::in_dir(&config_dir);
            paths.logs = log_dir;
            settings::ensure_prompt_files(&paths.prompts);
            let settings = settings::load(&paths.settings);
            let secrets: Arc<dyn SecretStore> = Arc::new(Keychain);
            let translator = initial_translator(&settings, &secrets, &paths);
            log::info!("provider: {}", settings::label(&settings.active().config));
            let (cache, cache_path) = open_cache(app);
            paths.cache = cache_path;
            let engine = Engine::new(
                translator,
                cache.clone(),
                Arc::new(TauriSink::new(app.handle().clone())),
                EngineSettings {
                    concurrency: settings.concurrency,
                    ..EngineSettings::default()
                },
                tauri::async_runtime::handle().inner().clone(),
            );
            engine.set_glossary(settings.glossary.clone());
            let state = AppState::new(engine, cache, settings, paths, secrets);
            if let Some(path) = std::env::args_os().nth(1).map(std::path::PathBuf::from) {
                // `biwrite paper.tex`: open a file from the command line.
                match state.load_path_blocking(path) {
                    Ok(()) => {
                        if let Some(window) = app.get_webview_window("main") {
                            window.set_title(&state.file().window_title())?;
                        }
                    }
                    Err(e) => log::error!("{e}"),
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
            settings_commands::clear_api_key,
            settings_commands::set_active_provider,
            settings_commands::test_provider,
            settings_commands::list_provider_models,
            settings_commands::set_concurrency,
            settings_commands::set_doc_note,
            settings_commands::get_prompt,
            settings_commands::save_prompt,
            settings_commands::reveal_prompts,
            storage_commands::get_cache,
            storage_commands::clear_cache,
            storage_commands::reveal_logs,
            glossary_commands::get_glossary,
            glossary_commands::save_glossary,
            glossary_commands::import_glossary,
            glossary_commands::export_glossary,
            glossary_commands::export_bilingual,
        ])
        .build(tauri::generate_context!());

    match app {
        Ok(app) => app.run(on_run_event),
        Err(e) => {
            log::error!("BiWrite failed to start: {e}");
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
) -> Arc<dyn Translator> {
    let prompts = Arc::new(PromptFiles {
        dir: paths.prompts.clone(),
    });
    provider_state::translator_for(settings.active(), secrets, prompts).unwrap_or_else(|e| {
        log::warn!("provider unavailable ({e}); using the mock translator");
        Arc::new(MockTranslator::default())
    })
}

/// The persistent translation cache in the app data folder (and its path);
/// falls back to an in-memory cache (with a warning) so the app still works
/// if it can't open.
fn open_cache(app: &App) -> (Arc<dyn TranslationCache>, Option<PathBuf>) {
    let opened = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())
        .and_then(|dir| {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = dir.join("cache.sqlite3");
            let cache = SqliteCache::open(&path).map_err(|e| e.to_string())?;
            Ok((cache, path))
        });
    match opened {
        Ok((cache, path)) => (Arc::new(cache), Some(path)),
        Err(e) => {
            log::warn!("translation cache unavailable ({e}); using a temporary one");
            (Arc::new(MemoryCache::default()), None)
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
                log::error!("failed to close window: {e}");
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
