//! Provider- and settings-related parts of [`AppState`]: building the active
//! translator, key access, document notes, persistence.

use std::path::Path;
use std::sync::atomic::Ordering;
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
use crate::settings::{self, AppSettings, ProviderEntry, SettingsView, same_destination};
use crate::state::AppState;

/// Key source for provider `entry`, reading the OS credential store lazily.
pub fn key_fn(store: &Arc<dyn SecretStore>, entry: &ProviderEntry) -> KeyFn {
    let store = Arc::clone(store);
    let id = entry.config.id.clone();
    let vouched = entry.vouched();
    Arc::new(move || secrets::get_keys(store.as_ref(), &id, vouched))
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
    let http = build_http(&entry.config, key_fn(secrets, entry), prompts, observer)?;
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
        self.apply_provider_of(&settings)
    }

    /// [`Self::apply_active_provider`] with the settings lock held.
    fn apply_provider_of(&self, settings: &AppSettings) -> CommandResult<()> {
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

    /// Keys were added to provider `id`: the providers in use read them
    /// again for their next request. Requests in flight go on (a new
    /// translator would cancel and redo them), failed paragraphs are tried
    /// again.
    pub fn reload_keys(&self, id: &str) -> CommandResult<()> {
        // Held throughout, as in `apply_active_provider`: the active
        // provider cannot change meanwhile.
        let s = self.settings();
        let Some(entry) = s.provider(id) else {
            return Ok(());
        };
        let (concurrency, active) = (s.effective_concurrency(), s.active_provider == id);
        let keys = key_fn(&self.secrets, entry);
        // Only a provider built for this host gets the keys (one built from
        // settings read just before a move is not).
        let in_use = |slot: &std::sync::Mutex<crate::state::HttpInUse>| {
            slot.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .filter(|(used, http)| used == id && same_destination(http.config(), &entry.config))
                .map(|(_, http)| http)
        };
        {
            let cached = self
                .assistant_http
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some((_, http)) = cached
                .as_ref()
                .filter(|(used, http)| used == id && same_destination(http.config(), &entry.config))
            {
                http.reload_keys(Arc::clone(&keys));
            }
            // An assistant provider being built now read the old keys.
            self.assistant_epoch.fetch_add(1, Ordering::AcqRel);
        }
        if !active {
            return Ok(());
        }
        match in_use(&self.translation_http) {
            Some(http) => {
                http.reload_keys(keys);
                self.engine.set_concurrency(concurrency);
                self.engine.retry_failed();
                Ok(())
            }
            None => self.apply_provider_of(&s),
        }
    }

    /// Settings can be saved. Checked before keychain changes, so keys are
    /// not changed when the settings recording them cannot be.
    pub fn settings_writable(&self) -> CommandResult<()> {
        if self.settings().keep_file {
            return Err(CommandError::Settings(format!(
                "{} could not be read, so settings are not saved: fix or remove it, then restart BiWrite",
                self.paths.settings.display()
            )));
        }
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
        // Settings or keys may change while a provider is built: it is
        // then built again (bounded), never kept for later requests.
        for _ in 0..3 {
            let epoch = self.assistant_epoch.load(Ordering::Acquire);
            let entry = self.settings().assistant().clone();
            if entry.config.kind == ProviderKind::Mock {
                return Err(CommandError::Settings(
                    "choose a model for the writing assistant in Settings (the offline mock cannot write)".into(),
                ));
            }
            // A provider built from other settings (read before a change) is
            // not used: its host may not be the provider's any more.
            let built_for = |id: &String, http: &HttpProvider| {
                *id == entry.config.id && *http.config() == entry.config
            };
            let shared = self
                .translation_http
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .filter(|(id, http)| built_for(id, http));
            if let Some((_, http)) = shared {
                return Ok(http);
            }
            let cached = self
                .assistant_http
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .filter(|(id, http)| built_for(id, http));
            if let Some((_, http)) = cached {
                return Ok(http);
            }
            let http = build_http(
                &entry.config,
                key_fn(&self.secrets, &entry),
                Arc::new(DefaultPrompts),
                self.request_log.clone(),
            )?;
            // No other lock is held while reading the settings: they come
            // before the providers in use.
            if self.settings().assistant().config != entry.config {
                continue;
            }
            if self.keep_assistant(epoch, &entry.config.id, &http) {
                return Ok(http);
            }
        }
        Err(CommandError::Settings(
            "the assistant's provider changed meanwhile; try again".into(),
        ))
    }

    /// Keep `http`, built for provider `id` when the assistant's epoch was
    /// `epoch`, as the assistant's provider, unless its settings or keys
    /// changed since. Returns whether it was kept.
    pub(crate) fn keep_assistant(&self, epoch: u64, id: &str, http: &Arc<HttpProvider>) -> bool {
        let mut cached = self
            .assistant_http
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.assistant_epoch.load(Ordering::Acquire) != epoch {
            return false;
        }
        *cached = Some((id.to_owned(), Arc::clone(http)));
        true
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
        let mut cached = self
            .assistant_http
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *cached = None;
        self.assistant_epoch.fetch_add(1, Ordering::AcqRel);
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
