//! Provider- and settings-related parts of [`AppState`]: building the active
//! translator, key access, document notes, persistence.

use std::path::Path;
use std::sync::{Arc, MutexGuard, PoisonError};

use biwrite_engine::Translator;
use biwrite_providers::{KeyFn, PromptFiles, PromptSource, build_translator};

use crate::error::{CommandError, CommandResult};
use crate::secrets::SecretStore;
use crate::settings::{self, AppSettings, ProviderEntry, SettingsView};
use crate::state::AppState;

/// Key source for provider `id`, reading the OS credential store lazily.
pub fn key_fn(secrets: &Arc<dyn SecretStore>, id: &str) -> KeyFn {
    let secrets = Arc::clone(secrets);
    let id = id.to_owned();
    Arc::new(move || secrets.get(&id))
}

/// The translator for `entry`, or the mock if it cannot be built.
pub fn translator_for(
    entry: &ProviderEntry,
    secrets: &Arc<dyn SecretStore>,
    prompts: Arc<dyn PromptSource>,
) -> Result<Arc<dyn Translator>, CommandError> {
    Ok(build_translator(
        &entry.config,
        key_fn(secrets, &entry.config.id),
        prompts,
    )?)
}

impl AppState {
    pub fn settings(&self) -> MutexGuard<'_, AppSettings> {
        self.settings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn prompts(&self) -> Arc<dyn PromptSource> {
        Arc::new(PromptFiles {
            dir: self.paths.prompts.clone(),
        })
    }

    /// Write settings to disk.
    pub fn persist(&self, settings: &AppSettings) -> CommandResult<()> {
        settings::save(&self.paths.settings, settings)
            .map_err(|e| CommandError::Settings(format!("could not save settings: {e}")))
    }

    /// Point the engine at the active provider (fresh translator, so a new
    /// key or model takes effect).
    pub fn apply_active_provider(&self) -> CommandResult<()> {
        // Held across `set_translator` so two quick switches can't leave the
        // engine on a different provider than settings. (The engine never
        // takes the settings lock, so this can't deadlock.)
        let settings = self.settings();
        let translator = translator_for(settings.active(), &self.secrets, self.prompts())?;
        self.engine.set_translator(translator);
        Ok(())
    }

    fn doc_key(&self) -> Option<String> {
        self.file().path.as_ref().map(|p| p.display().to_string())
    }

    /// Note stored for `path` (the untitled note if `None`).
    pub fn note_for(&self, path: Option<&Path>) -> String {
        match path {
            Some(path) => self
                .settings()
                .doc_notes
                .get(&path.display().to_string())
                .cloned()
                .unwrap_or_default(),
            None => self
                .untitled_note
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }

    /// Note for the open document.
    pub fn doc_note(&self) -> String {
        let path = self.file().path.clone();
        self.note_for(path.as_deref())
    }

    /// Hand the open document's note to the engine (after open/save as).
    pub fn apply_doc_note(&self) {
        self.engine.set_doc_note(Some(self.doc_note()));
    }

    pub fn set_doc_note(&self, note: String) -> CommandResult<()> {
        let note = note.trim().to_owned();
        match self.doc_key() {
            Some(key) => {
                let mut s = self.settings();
                if note.is_empty() {
                    s.doc_notes.remove(&key);
                } else {
                    s.doc_notes.insert(key, note);
                }
                self.persist(&s)?;
            }
            None => {
                *self
                    .untitled_note
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = note;
            }
        }
        self.apply_doc_note();
        Ok(())
    }

    pub fn settings_view(&self) -> SettingsView {
        let note = self.doc_note();
        settings::view(&self.settings(), note, &self.paths.prompts)
    }
}
