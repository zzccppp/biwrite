//! BiWrite desktop app: wires the engine to Tauri (commands, events, dialogs).

mod assist_commands;
mod commands;
mod error;
mod files;
mod glossary_commands;
mod latex_commands;
mod latex_sync;
mod log_commands;
mod logging;
mod pairing;
mod provider_state;
mod request_log;
mod secrets;
mod settings;
mod settings_commands;
mod sink;
mod skills;
mod state;
mod storage_commands;
mod tray;
mod updater;

#[cfg(test)]
mod home_tests;
#[cfg(test)]
mod key_tests;
#[cfg(test)]
mod leak_tests;
#[cfg(test)]
mod pair_tests;
#[cfg(test)]
mod workflow_tests;

use std::path::PathBuf;
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
    // First, so even failures before setup (e.g. no WebView2) are reported.
    logging::init();
    let builder = tauri::Builder::default();
    // First, so a second launch goes no further than handing over.
    #[cfg(not(target_os = "macos"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        tray::show_window(app);
    }));
    let app = builder
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
            let sink = Arc::new(TauriSink::new(app.handle().clone()));
            let request_log = Arc::new(RequestLog::new(
                settings.request_log,
                request_log_file(app),
                sink.clone(),
            ));
            let built = initial_translator(&settings, &secrets, &paths, request_log.clone());
            log::info!("provider: {}", settings::label(&settings.active().config));
            let (cache, cache_path) = open_cache(app);
            paths.cache = cache_path;
            let engine = Engine::new(
                built.translator,
                cache.clone(),
                sink,
                EngineSettings {
                    concurrency: settings.effective_concurrency(),
                    batch_size: settings.batch_size,
                    ..EngineSettings::default()
                },
                tauri::async_runtime::handle().inner().clone(),
            );
            engine.set_glossary(settings.glossary.clone());
            let active_id = settings.active().config.id.clone();
            let skills = skills::SkillStore::new(
                app.path()
                    .resource_dir()
                    .ok()
                    .map(|d| d.join("resources/skills/research-builder")),
                app.path()
                    .app_data_dir()
                    .ok()
                    .map(|d| d.join("skills/research-builder")),
            );
            let latex = latex_commands::LatexState::new(
                app.path()
                    .resource_dir()
                    .map(|d| d.join("resources/templates"))
                    .unwrap_or_default(),
                app.path()
                    .app_data_dir()
                    .map(|d| d.join("templates"))
                    .unwrap_or_default(),
            );
            let state = AppState::new(
                engine,
                cache,
                settings,
                paths,
                secrets,
                request_log,
                skills,
                latex,
            );
            let close_to_tray = state.settings().close_to_tray;
            match tray::Tray::build(app, close_to_tray) {
                Ok(tray) => {
                    app.manage(tray);
                }
                Err(e) => log::error!("no tray icon: {e}"),
            }
            state.set_translation_http(active_id, built.http);
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
            commands::retarget_language,
            commands::continue_translation,
            commands::open_link,
            commands::open_manual,
            settings_commands::get_settings,
            settings_commands::save_provider,
            settings_commands::delete_provider,
            settings_commands::set_api_key,
            settings_commands::add_api_keys,
            settings_commands::export_api_keys,
            settings_commands::import_api_keys,
            settings_commands::remove_api_key,
            settings_commands::clear_api_key,
            settings_commands::key_status,
            settings_commands::list_api_keys,
            settings_commands::rename_api_key,
            settings_commands::test_api_key,
            settings_commands::set_batch_size,
            settings_commands::set_match_pool,
            settings_commands::set_assistant_provider,
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
            assist_commands::assist_start,
            assist_commands::assist_cancel,
            assist_commands::assist_offer,
            assist_commands::attach_image,
            assist_commands::load_reference,
            assist_commands::drop_attachment,
            assist_commands::get_skill,
            assist_commands::update_skill,
            assist_commands::choose_skill_folder,
            assist_commands::reset_skill_folder,
            assist_commands::reveal_skill,
            latex_commands::latex_status,
            latex_commands::latex_set_compile_on_save,
            latex_commands::latex_choose_bin,
            latex_commands::latex_reset_bin,
            latex_commands::latex_project,
            latex_commands::latex_open,
            latex_commands::latex_open_folder,
            latex_commands::latex_compile,
            latex_commands::latex_cancel,
            latex_commands::latex_pdf,
            latex_commands::latex_reveal_pdf,
            latex_commands::latex_save_pdf,
            latex_commands::latex_export_tex,
            latex_commands::latex_inverse,
            latex_commands::latex_forward,
            latex_commands::latex_locate,
            latex_commands::latex_goto,
            latex_commands::latex_templates,
            latex_commands::latex_new_paper,
            latex_commands::latex_import_template,
            latex_commands::latex_export_template,
            latex_commands::latex_delete_template,
            latex_commands::latex_reveal_templates,
            pairing::import_mirror,
            pairing::close_mirror,
            pairing::write_mirror,
            updater::list_releases,
            updater::install_release,
            updater::relaunch,
            settings_commands::set_check_updates,
            settings_commands::set_close_to_tray,
            settings_commands::set_tray_language,
            log_commands::get_request_log,
            log_commands::set_request_log,
            log_commands::clear_request_log,
            log_commands::reveal_request_log,
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
    observer: Arc<dyn RequestObserver>,
) -> Built {
    let prompts = Arc::new(PromptFiles {
        dir: paths.prompts.clone(),
    });
    provider_state::translator_for(settings.active(), secrets, prompts, observer).unwrap_or_else(
        |e| {
            log::warn!("provider unavailable ({e}); using the mock translator");
            Built {
                translator: Arc::new(MockTranslator::default()),
                http: None,
            }
        },
    )
}

/// `requests.jsonl` in the app data folder, if the folder is available.
fn request_log_file(app: &App) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("requests.jsonl"))
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

/// Closing the window hides it in the tray (the setting), or asks first
/// about unsaved changes and quits.
fn on_window_event(window: &Window, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    let to_tray = window.state::<AppState>().settings().close_to_tray
        && window.try_state::<tray::Tray>().is_some();
    if to_tray {
        api.prevent_close();
        tray::hide_window(window);
        return;
    }
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

/// Ask before quitting (e.g. Cmd+Q) with unsaved changes, with the window
/// shown even if it was hidden in the tray. A click on the Dock icon brings
/// a hidden window back.
fn on_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        RunEvent::ExitRequested { api, .. } => {
            if app
                .try_state::<AppState>()
                .is_some_and(|s| s.needs_close_confirmation())
            {
                api.prevent_exit();
                tray::quit_app(app);
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => tray::show_window(app),
        _ => {}
    }
}
