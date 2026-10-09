//! Provider- and settings-related parts of [`AppState`]: building the active
//! translator, key access, document notes, persistence.

use std::path::Path;
use std::sync::{Arc, MutexGuard, PoisonError};

use biwrite_core::assist::Skill;
use biwrite_engine::{MockTranslator, Translator};
use biwrite_providers::{
    DefaultPrompts, HttpProvider, KeyFn, KeyStatus, PromptFiles, PromptSource, ProviderKind,
    RequestObserver, build_http,
};

use crate::skills::SkillInfo;

use crate::error::{CommandError, CommandResult};
use crate::secrets::{self, SecretStore};
use crate::settings::{self, AppSettings, ProviderEntry, SettingsView};
use crate::state::AppState;

/// Key source for provider `id`, reading the OS credential store lazily.
pub fn key_fn(store: &Arc<dyn SecretStore>, id: &str) -> KeyFn {
    let store = Arc::clone(store);
    let id = id.to_owned();
    Arc::new(move || secrets::get_keys(store.as_ref(), &id))
}

/// A translator and, unless it is the mock, the HTTP provider behind it.
pub struct Built {
    pub translator: Arc<dyn Translator>,
    pub http: Option<Arc<HttpProvider>>,
}

/// The translator for `entry`, reporting its requests to `observer`.
pub fn translator_for(
    entry: &ProviderEntry,
    secrets: &Arc<dyn SecretStore>,
    prompts: Arc<dyn PromptSource>,
    observer: Arc<dyn RequestObserver>,
) -> Result<Built, CommandError> {
    if entry.config.kind == ProviderKind::Mock {
        return Ok(Built {
            translator: Arc::new(MockTranslator::default()),
            http: None,
        });
    }
    let http = build_http(
        &entry.config,
        key_fn(secrets, &entry.config.id),
        prompts,
        observer,
    )?;
    Ok(Built {
        translator: http.clone(),
        http: Some(http),
    })
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
        let built = translator_for(
            settings.active(),
            &self.secrets,
            self.prompts(),
            self.request_log.clone(),
        )?;
        self.engine.set_translator(built.translator);
        self.engine
            .set_concurrency(settings.effective_concurrency());
        self.set_translation_http(settings.active().config.id.clone(), built.http);
        log::info!("provider: {}", settings::label(&settings.active().config));
        Ok(())
    }

    /// Remember the HTTP provider the engine translates with (for key status).
    pub fn set_translation_http(&self, id: String, http: Option<Arc<HttpProvider>>) {
        *self
            .translation_http
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = http.map(|h| (id, h));
    }

    /// The writing assistant's model. When the assistant uses the
    /// translation provider, it shares that provider's key pool, so per-key
    /// limits hold across both.
    pub fn assistant_model(&self) -> CommandResult<Arc<HttpProvider>> {
        let entry = self.settings().assistant().clone();
        if entry.config.kind == ProviderKind::Mock {
            return Err(CommandError::Settings(
                "choose a model for the writing assistant in Settings (the offline mock cannot write)".into(),
            ));
        }
        let shared = self
            .translation_http
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .filter(|(id, _)| *id == entry.config.id);
        if let Some((_, http)) = shared {
            return Ok(http);
        }
        let mut cached = self
            .assistant_http
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((id, http)) = cached.as_ref()
            && *id == entry.config.id
        {
            return Ok(Arc::clone(http));
        }
        let http = build_http(
            &entry.config,
            key_fn(&self.secrets, &entry.config.id),
            Arc::new(DefaultPrompts),
            self.request_log.clone(),
        )?;
        *cached = Some((entry.config.id.clone(), Arc::clone(&http)));
        Ok(http)
    }

    /// The skill the assistant follows.
    pub fn skill(&self) -> CommandResult<(Arc<Skill>, SkillInfo)> {
        let folder = self
            .settings()
            .skill_folder
            .clone()
            .map(std::path::PathBuf::from);
        self.skills.current(folder.as_deref()).ok_or_else(|| {
            CommandError::Settings("the writing skill is missing from this installation".into())
        })
    }

    /// Drop the assistant's provider, so the next request builds it again
    /// from the current settings and keys.
    pub fn invalidate_assistant(&self) {
        *self
            .assistant_http
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// State of each key of provider `id`, if a provider in use has read them.
    pub fn key_status(&self, id: &str) -> Option<Vec<KeyStatus>> {
        let translation = self
            .translation_http
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let assistant = self
            .assistant_http
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        [translation, assistant]
            .into_iter()
            .flatten()
            .filter(|(owner, _)| owner == id)
            .find_map(|(_, http)| http.key_status())
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
        settings::view(&self.settings(), note, &self.paths)
    }
}
