//! Persistent app settings (`<config dir>/settings.json`) and the view sent
//! to the settings UI. Never contains API keys: only whether one is stored.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use biwrite_core::{Direction, GlossaryEntry};
use biwrite_providers::prompt::{default_prompt, prompt_file_name};
use biwrite_providers::{Effort, Preset, ProviderConfig, ProviderKind, WireApi, presets};
use serde::{Deserialize, Serialize};

use crate::request_log::LogSettings;

pub const MOCK_ID: &str = "mock";
const DEFAULT_CONCURRENCY: usize = 4;
/// Most parallel requests (a large key pool can carry this many).
pub const MAX_CONCURRENCY: usize = 32;

/// A configured provider plus non-secret key status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    #[serde(flatten)]
    pub config: ProviderConfig,
    /// A key is stored in the OS keychain for this provider.
    #[serde(default)]
    pub has_key: bool,
    /// How many keys the provider's pool holds. Versions without key pools
    /// ignore it and read `has_key`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub key_count: usize,
    /// Names of the pool's keys, by key fingerprint (never the key itself).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub key_names: BTreeMap<String, String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub providers: Vec<ProviderEntry>,
    pub active_provider: String,
    pub concurrency: usize,
    /// Document note per file path.
    pub doc_notes: BTreeMap<String, String>,
    /// Preferred renderings of terms, for every document.
    pub glossary: Vec<GlossaryEntry>,
    /// Provider for the writing assistant; empty means the translation
    /// provider.
    pub assistant_provider: String,
    pub request_log: LogSettings,
    /// Paragraphs translated together in one request (1: one per request).
    pub batch_size: usize,
    /// With a per-key limit, run as many requests as the pool carries
    /// (keys × limit) instead of `concurrency`.
    pub match_pool: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            providers: vec![mock_entry()],
            active_provider: MOCK_ID.to_owned(),
            concurrency: DEFAULT_CONCURRENCY,
            doc_notes: BTreeMap::new(),
            glossary: Vec::new(),
            assistant_provider: String::new(),
            request_log: LogSettings::default(),
            batch_size: 1,
            match_pool: true,
        }
    }
}

fn mock_entry() -> ProviderEntry {
    ProviderEntry {
        config: ProviderConfig {
            id: MOCK_ID.to_owned(),
            name: "Mock (offline, reverses text)".to_owned(),
            kind: ProviderKind::Mock,
            base_url: String::new(),
            model: "reverse".to_owned(),
            temperature: 0.0,
            effort: Effort::Default,
            wire_api: WireApi::Chat,
            service_tier: None,
            key_concurrency: None,
            max_retries: None,
        },
        has_key: false,
        key_count: 0,
        key_names: BTreeMap::new(),
    }
}

impl AppSettings {
    /// Repair invariants after loading: the mock provider exists, the active
    /// provider exists, concurrency is sane, key counts agree with the key
    /// flags (a key stored by an older version counts as one).
    fn normalized(mut self) -> Self {
        if !self.providers.iter().any(|p| p.config.id == MOCK_ID) {
            self.providers.insert(0, mock_entry());
        }
        if !self
            .providers
            .iter()
            .any(|p| p.config.id == self.active_provider)
        {
            self.active_provider = MOCK_ID.to_owned();
        }
        if !self.assistant_provider.is_empty()
            && self
                .provider(&self.assistant_provider)
                .is_none_or(|p| p.config.kind == ProviderKind::Mock)
        {
            self.assistant_provider.clear();
        }
        for p in &mut self.providers {
            p.key_count = match (p.has_key, p.key_count) {
                (false, _) => 0,
                (true, 0) => 1,
                (true, n) => n,
            };
        }
        self.concurrency = self.concurrency.clamp(1, MAX_CONCURRENCY);
        self.batch_size = self.batch_size.clamp(1, biwrite_engine::MAX_BATCH);
        self
    }

    /// Parallel requests for the active provider: its whole key pool
    /// (keys × per-key limit) when it has a limit and `match_pool` is on,
    /// else the `concurrency` setting.
    pub fn effective_concurrency(&self) -> usize {
        let active = self.active();
        match (self.match_pool, active.config.key_concurrency) {
            (true, Some(per_key)) if active.key_count > 0 => {
                (active.key_count * per_key as usize).clamp(1, MAX_CONCURRENCY)
            }
            _ => self.concurrency,
        }
    }

    /// Record how many keys provider `id` now has.
    pub fn set_key_count(&mut self, id: &str, count: usize) -> bool {
        match self.provider_mut(id) {
            Some(p) => {
                p.key_count = count;
                p.has_key = count > 0;
                true
            }
            None => false,
        }
    }

    /// The provider the writing assistant uses: the chosen one, else the
    /// translation provider.
    pub fn assistant(&self) -> &ProviderEntry {
        self.provider(&self.assistant_provider)
            .unwrap_or_else(|| self.active())
    }

    pub fn active(&self) -> &ProviderEntry {
        self.provider(&self.active_provider)
            .or_else(|| self.providers.first())
            .unwrap_or_else(|| unreachable_mock())
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderEntry> {
        self.providers.iter().find(|p| p.config.id == id)
    }

    pub fn provider_mut(&mut self, id: &str) -> Option<&mut ProviderEntry> {
        self.providers.iter_mut().find(|p| p.config.id == id)
    }
}

/// `normalized()` guarantees the mock entry; this only satisfies the types.
fn unreachable_mock() -> &'static ProviderEntry {
    static MOCK: std::sync::OnceLock<ProviderEntry> = std::sync::OnceLock::new();
    MOCK.get_or_init(mock_entry)
}

/// Load settings; a missing file gives defaults, a corrupt one is moved
/// aside (`settings.json.bad`) so the user's data isn't silently lost.
pub fn load(path: &Path) -> AppSettings {
    let Ok(text) = std::fs::read_to_string(path) else {
        return AppSettings::default();
    };
    match serde_json::from_str::<AppSettings>(&text) {
        Ok(s) => s.normalized(),
        Err(e) => {
            log::warn!(
                "{} is invalid ({e}); starting with defaults",
                path.display()
            );
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            AppSettings::default()
        }
    }
}

pub fn save(path: &Path, settings: &AppSettings) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    crate::files::write_atomic_blocking(path, &json).map_err(|e| e.to_string())
}

/// Create missing prompt files with the defaults so users can find and edit them.
pub fn ensure_prompt_files(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!("cannot create {}: {e}", dir.display());
        return;
    }
    for direction in [Direction::EnZh, Direction::ZhEn] {
        let path = dir.join(prompt_file_name(direction));
        if !path.exists()
            && let Err(e) = std::fs::write(&path, format!("{}\n", default_prompt(direction)))
        {
            log::warn!("cannot write {}: {e}", path.display());
        }
    }
}

/// A provider as shown in settings.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    #[serde(flatten)]
    pub config: ProviderConfig,
    pub has_key: bool,
    pub key_count: usize,
    /// Key names by fingerprint.
    pub key_names: BTreeMap<String, String>,
    pub builtin: bool,
    pub needs_key: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub providers: Vec<ProviderView>,
    pub active_provider: String,
    /// "Name · model" for the status bar.
    pub active_label: String,
    pub concurrency: usize,
    /// Note for the open document.
    pub doc_note: String,
    pub presets: Vec<Preset>,
    pub prompt_dir: String,
    /// Log folder, if logs are written to files.
    pub log_dir: Option<String>,
    /// Chosen assistant provider (empty: the translation provider).
    pub assistant_provider: String,
    /// "Name · model" of the provider the assistant uses.
    pub assistant_label: String,
    /// The assistant has a real model (not the offline mock).
    pub assistant_ready: bool,
    pub request_log: LogSettings,
    pub batch_size: usize,
    pub match_pool: bool,
    /// Parallel requests in effect (see `AppSettings::effective_concurrency`).
    pub effective_concurrency: usize,
}

pub fn view(settings: &AppSettings, doc_note: String, paths: &Paths) -> SettingsView {
    let active = settings.active();
    SettingsView {
        providers: settings
            .providers
            .iter()
            .map(|p| ProviderView {
                config: p.config.clone(),
                has_key: p.has_key,
                key_count: p.key_count,
                key_names: p.key_names.clone(),
                builtin: p.config.id == MOCK_ID,
                needs_key: p.config.kind.needs_key(),
            })
            .collect(),
        active_provider: active.config.id.clone(),
        active_label: label(&active.config),
        concurrency: settings.concurrency,
        doc_note,
        presets: presets(),
        prompt_dir: paths.prompts.display().to_string(),
        log_dir: paths.logs.as_ref().map(|d| d.display().to_string()),
        assistant_provider: settings.assistant_provider.clone(),
        assistant_label: label(&settings.assistant().config),
        assistant_ready: settings.assistant().config.kind != ProviderKind::Mock,
        request_log: settings.request_log,
        batch_size: settings.batch_size,
        match_pool: settings.match_pool,
        effective_concurrency: settings.effective_concurrency(),
    }
}

pub fn label(config: &ProviderConfig) -> String {
    match config.kind {
        ProviderKind::Mock => "Mock".to_owned(),
        _ => format!("{} · {}", config.name, config.model),
    }
}

/// Where BiWrite keeps its files.
#[derive(Clone, Debug)]
pub struct Paths {
    pub settings: PathBuf,
    pub prompts: PathBuf,
    /// Log folder (`None`: logging to stderr only).
    pub logs: Option<PathBuf>,
    /// Translation cache database (`None`: in-memory cache).
    pub cache: Option<PathBuf>,
}

impl Paths {
    pub fn in_dir(config_dir: &Path) -> Self {
        Self {
            settings: config_dir.join("settings.json"),
            prompts: config_dir.join("prompts"),
            logs: None,
            cache: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deepseek() -> ProviderEntry {
        ProviderEntry {
            config: ProviderConfig {
                id: "p-1".into(),
                name: "DeepSeek".into(),
                kind: ProviderKind::OpenaiCompatible,
                base_url: "https://api.deepseek.com/v1".into(),
                model: "deepseek-chat".into(),
                temperature: 0.1,
                effort: Effort::Low,
                wire_api: WireApi::Chat,
                service_tier: None,
                key_concurrency: None,
                max_retries: None,
            },
            has_key: true,
            key_count: 2,
            key_names: BTreeMap::new(),
        }
    }

    #[test]
    fn round_trip_and_repair() {
        let dir = std::env::temp_dir().join(format!("biwrite-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        assert_eq!(load(&path), AppSettings::default());

        let mut s = AppSettings::default();
        s.providers.push(deepseek());
        s.active_provider = "p-1".into();
        s.doc_notes.insert("/a/paper.tex".into(), "ML paper".into());
        s.glossary = vec![
            GlossaryEntry::new("GNN", None),
            GlossaryEntry::new("prompt", Some("提示")),
        ];
        save(&path, &s).unwrap();
        assert_eq!(load(&path), s);
        // Files from before the glossary existed still load.
        std::fs::write(&path, r#"{"activeProvider":"mock","concurrency":2}"#).unwrap();
        assert!(load(&path).glossary.is_empty());

        // Missing mock and dangling active provider are repaired.
        std::fs::write(
            &path,
            r#"{"providers":[],"activeProvider":"gone","concurrency":99}"#,
        )
        .unwrap();
        let repaired = load(&path);
        assert_eq!(repaired.active_provider, MOCK_ID);
        assert_eq!(repaired.concurrency, MAX_CONCURRENCY);

        // Corrupt files are moved aside.
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load(&path), AppSettings::default());
        assert!(path.with_extension("json.bad").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prompt_files_are_created_once() {
        let dir = std::env::temp_dir().join(format!("biwrite-prompt-init-{}", std::process::id()));
        ensure_prompt_files(&dir);
        let en = dir.join("en-zh.txt");
        assert!(
            std::fs::read_to_string(&en)
                .unwrap()
                .starts_with("You translate academic English")
        );
        std::fs::write(&en, "mine").unwrap();
        ensure_prompt_files(&dir);
        assert_eq!(std::fs::read_to_string(&en).unwrap(), "mine");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The settings types of v0.1.0, frozen. Users can install an older
    /// version from the update dialog, so the file this version writes must
    /// still load there: older versions move a file they cannot parse aside
    /// and start from defaults.
    #[allow(dead_code)] // fields exist to be parsed, as v0.1 does
    mod v0_1 {
        use std::collections::BTreeMap;

        use biwrite_core::GlossaryEntry;
        use serde::Deserialize;

        #[derive(Debug, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum ProviderKind {
            OpenaiCompatible,
            Anthropic,
            Mock,
        }

        #[derive(Debug, Default, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Effort {
            #[default]
            Low,
            Medium,
            High,
            Default,
        }

        #[derive(Debug, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct ProviderConfig {
            pub id: String,
            pub name: String,
            pub kind: ProviderKind,
            pub base_url: String,
            pub model: String,
            #[serde(default)]
            pub temperature: f32,
            #[serde(default)]
            pub effort: Effort,
        }

        #[derive(Debug, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct ProviderEntry {
            #[serde(flatten)]
            pub config: ProviderConfig,
            #[serde(default)]
            pub has_key: bool,
        }

        #[derive(Debug, Default, Deserialize)]
        #[serde(rename_all = "camelCase", default)]
        pub struct AppSettings {
            pub providers: Vec<ProviderEntry>,
            pub active_provider: String,
            pub concurrency: usize,
            pub doc_notes: BTreeMap<String, String>,
            pub glossary: Vec<GlossaryEntry>,
        }
    }

    #[test]
    fn settings_written_now_load_in_v0_1() {
        let mut s = AppSettings::default();
        let mut relay = deepseek();
        relay.config.id = "p-relay".into();
        relay.config.wire_api = WireApi::Responses;
        relay.config.service_tier = Some(biwrite_providers::ServiceTier::Priority);
        relay.config.effort = Effort::High;
        relay.key_count = 3;
        s.providers.push(relay);
        s.assistant_provider = "p-relay".into();
        s.request_log.persist = false;
        let json = serde_json::to_string_pretty(&s).unwrap();
        let old: v0_1::AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(old.providers.len(), 2);
        assert!(matches!(
            old.providers[1].config.kind,
            v0_1::ProviderKind::OpenaiCompatible
        ));
        assert!(old.providers[1].has_key);
        assert_eq!(old.providers[1].config.id, "p-relay");
    }

    #[test]
    fn parallel_requests_follow_the_key_pool() {
        let mut s = AppSettings::default();
        let mut relay = deepseek();
        relay.config.key_concurrency = Some(2);
        relay.key_count = 6;
        s.providers.push(relay);
        s.active_provider = "p-1".into();
        assert_eq!(s.effective_concurrency(), 12);
        s.match_pool = false;
        assert_eq!(s.effective_concurrency(), DEFAULT_CONCURRENCY);
        s.match_pool = true;
        s.provider_mut("p-1").unwrap().key_count = 40;
        assert_eq!(s.effective_concurrency(), MAX_CONCURRENCY);
        s.provider_mut("p-1").unwrap().config.key_concurrency = None;
        assert_eq!(s.effective_concurrency(), DEFAULT_CONCURRENCY);
    }

    #[test]
    fn keys_stored_by_v0_1_count_as_one() {
        let dir = std::env::temp_dir().join(format!("biwrite-settings-v01-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"providers":[{"id":"p-1","name":"DeepSeek","kind":"openai_compatible","baseUrl":"https://api.deepseek.com/v1","model":"deepseek-chat","temperature":0.0,"effort":"low","hasKey":true}],"activeProvider":"p-1","concurrency":4}"#,
        )
        .unwrap();
        let s = load(&path);
        let p = s.provider("p-1").unwrap();
        assert_eq!((p.has_key, p.key_count), (true, 1));
        assert_eq!(p.config.wire_api, WireApi::Chat);
        assert_eq!(
            s.assistant().config.id,
            "p-1",
            "the assistant follows translation"
        );
        assert_eq!(s.request_log, LogSettings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
