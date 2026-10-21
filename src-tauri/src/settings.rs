//! Persistent app settings (`<config dir>/settings.json`) and the view sent
//! to the settings UI. Never contains API keys: only whether one is stored.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use biwrite_core::{Direction, GlossaryEntry};
use biwrite_providers::prompt::{default_prompt, prompt_file_name};
use biwrite_providers::{Effort, Preset, ProviderConfig, ProviderKind, WireApi, presets};
use serde::{Deserialize, Serialize};

use crate::request_log::LogSettings;
use crate::secrets::Vouched;

pub const MOCK_ID: &str = "mock";
const DEFAULT_CONCURRENCY: usize = 4;
/// Seconds an edited paragraph stays unchanged before it is translated.
const DEFAULT_TRANSLATE_DELAY: u64 = 5;
pub const MAX_TRANSLATE_DELAY: u64 = 60;
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
    /// When each key of the pool was last answered by a relay that turns
    /// keys away (Unix milliseconds), by key fingerprint: a key without one
    /// never got through.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub key_through: BTreeMap<String, u64>,
    /// Keychain items the keys after the first take (see `secrets`).
    /// Versions that store them in one item drop it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub key_parts: usize,
    /// Keys of the pool the author set aside, by fingerprint: no request,
    /// squeeze or keep-alive uses them until the author takes them back.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub key_ignored: BTreeSet<String>,
}

impl ProviderEntry {
    /// `keys` without those the author set aside.
    pub fn usable(&self, keys: Vec<String>) -> Vec<String> {
        if self.key_ignored.is_empty() {
            return keys;
        }
        keys.into_iter()
            .filter(|k| {
                !self
                    .key_ignored
                    .contains(&biwrite_providers::key_fingerprint(k))
            })
            .collect()
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Same API type and origin (scheme, host, port): the stored key may be
/// reused.
pub fn same_destination(a: &ProviderConfig, b: &ProviderConfig) -> bool {
    let origin = |c: &ProviderConfig| {
        reqwest::Url::parse(&c.base_url)
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_default()
    };
    a.kind == b.kind && origin(a) == origin(b)
}

/// The keys of a provider moved from `a` to `b` stay: the same host,
/// whatever its API type or model (a relay serves GPT models on the OpenAI
/// API and Claude models on the Messages API with the same keys).
pub fn keeps_keys(a: &ProviderConfig, b: &ProviderConfig) -> bool {
    let origin = |c: &ProviderConfig| {
        reqwest::Url::parse(&c.base_url)
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_default()
    };
    origin(a) == origin(b)
}

/// A relay that, when full, answers only the keys that got through.
fn is_crowded_relay(base_url: &str) -> bool {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host.contains("anyrouter"))
}

impl ProviderEntry {
    /// Getting through applies to it: a crowded relay, or a pool of
    /// several keys.
    pub fn squeezable(&self) -> bool {
        self.config.kind != ProviderKind::Mock && (self.key_count >= 2 || self.crowded_relay())
    }

    /// A relay that, when full, answers only the keys that got through
    /// (AnyRouter): the key pool sends to those keys first without a squeeze
    /// started for it.
    pub fn crowded_relay(&self) -> bool {
        is_crowded_relay(&self.config.base_url)
    }

    /// What a key that got through got through to. A key through for one
    /// host, model and API is not through for another.
    pub fn squeeze_target(&self) -> String {
        let c = &self.config;
        format!("{:?} {} {} {:?}", c.kind, c.base_url, c.model, c.wire_api)
    }

    /// How much of the stored keys may be read. Versions without key pools
    /// drop `key_count` when they save the settings (it then loads as 1),
    /// versions that keep a pool in one item drop `key_parts` (then the
    /// count tells where the keys end, as they keep it up to date), and
    /// both may have replaced or deleted keys or moved the provider to
    /// another host, leaving the rest of an older pool in the keychain.
    pub fn vouched(&self) -> Vouched {
        match (self.key_count, self.key_parts) {
            (0 | 1, _) => Vouched::FIRST,
            (keys, 0) => Vouched::counted(keys),
            (keys, parts) => Vouched { keys, parts },
        }
    }
}

/// Most keys a squeeze can be asked to get through before it stops.
pub const MAX_SQUEEZE_ENOUGH: usize = 99;

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
    /// Seconds an edited paragraph stays unchanged before it is translated
    /// (0 translates at once).
    pub translate_delay: u64,
    /// With a per-key limit, run as many requests as the pool carries
    /// (keys × limit) instead of `concurrency`.
    pub match_pool: bool,
    /// A skill folder of the author's own, used instead of the bundled or
    /// downloaded research-builder skill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_folder: Option<String>,
    pub latex: LatexSettings,
    /// The settings file on disk could not be read nor moved aside: these
    /// are defaults, and saving them would replace the user's file.
    #[serde(skip)]
    pub keep_file: bool,
    /// Look for a newer release at startup.
    pub check_updates: bool,
    /// Closing the window hides BiWrite in the menu bar or notification
    /// area instead of quitting.
    pub close_to_tray: bool,
    /// Keys that got through to a relay that turns keys away are sent a
    /// short translation when idle, so they keep their place.
    pub keep_alive: bool,
    /// They are kept alive however long the author is away; off, only
    /// within half an hour of the author's last use.
    pub keep_alive_forever: bool,
    /// Allow a failed keep-alive to rejoin a manual squeeze still running.
    pub keep_alive_rejoin: bool,
    /// Mean minutes between two keep-alives of a key; each wait is drawn
    /// within a quarter of it either way.
    pub keep_alive_minutes: f64,
    /// Mean seconds between two tries of a key in a squeeze; each pause is
    /// drawn within half of it either way.
    pub squeeze_seconds: f64,
    /// A squeeze stops once this many keys got through; those are kept
    /// alive, and one that loses its place is squeezed back in.
    pub squeeze_enough: usize,
    /// The reasoning effort of the tries of a squeeze and of keep-alives.
    pub squeeze_effort: Effort,
    /// API keys live in a file of the config folder only the user reads
    /// (no keychain prompt at start or after an update), else in the
    /// keychain.
    pub keys_in_file: bool,
    /// The keys kept alive when last noted (fingerprints, never the keys),
    /// by provider id: after a restart or an update they are squeezed back
    /// in. `None` from versions that did not note them.
    pub kept_keys: Option<BTreeMap<String, Vec<String>>>,
    /// Background work stopped after failed keep-alives. Only fingerprints
    /// and redacted error text are stored, tied to the provider target.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stopped_keys: BTreeMap<String, BTreeMap<String, crate::squeeze_commands::StoppedKey>>,
    /// Files paired by the author (Import translation), by the path of the
    /// document, with the path of its other-language file: the pair comes
    /// back when the document opens again, and builds of its project use
    /// it. Pairs by name (`x.tex` and `x_zh.tex`) need no entry.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub pairs: BTreeMap<String, String>,
    /// Accept and Revert in a LaTeX project go through the model and a
    /// build (the model fits an answer, judges the preamble lines a revert
    /// may take out); off, they apply and revert as they are.
    pub assisted_apply: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LatexSettings {
    /// Build the PDF after each save of a `.tex` file.
    pub compile_on_save: bool,
    /// The folder with the TeX programs, when it is not found by itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tex_bin: Option<String>,
}

impl Default for LatexSettings {
    fn default() -> Self {
        Self {
            compile_on_save: true,
            tex_bin: None,
        }
    }
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
            translate_delay: DEFAULT_TRANSLATE_DELAY,
            match_pool: true,
            skill_folder: None,
            latex: LatexSettings::default(),
            check_updates: true,
            close_to_tray: true,
            keep_alive: true,
            keep_alive_forever: true,
            keep_alive_rejoin: false,
            keys_in_file: true,
            keep_alive_minutes: 4.0,
            squeeze_seconds: 3.0,
            squeeze_enough: 2,
            squeeze_effort: Effort::Low,
            kept_keys: None,
            stopped_keys: BTreeMap::new(),
            pairs: BTreeMap::new(),
            assisted_apply: true,
            keep_file: false,
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
            long_context: None,
        },
        has_key: false,
        key_count: 0,
        key_names: BTreeMap::new(),
        key_through: Default::default(),
        key_parts: 0,
        key_ignored: Default::default(),
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
            // Names stored by versions that did not check them may hold a
            // key: they are dropped.
            p.key_names
                .retain(|_, name| !crate::secrets::may_be_secret(name));
            p.key_count = match (p.has_key, p.key_count) {
                (false, _) => 0,
                (true, 0) => 1,
                (true, n) => n,
            };
        }
        self.concurrency = self.concurrency.clamp(1, MAX_CONCURRENCY);
        self.batch_size = self.batch_size.clamp(1, biwrite_engine::MAX_BATCH);
        self.translate_delay = self.translate_delay.min(MAX_TRANSLATE_DELAY);
        self
    }

    /// Parallel requests for the active provider: its whole key pool
    /// (keys × per-key limit) when it has a limit and `match_pool` is on,
    /// else the `concurrency` setting.
    pub fn effective_concurrency(&self) -> usize {
        let active = self.active();
        let usable = active.key_count.saturating_sub(active.key_ignored.len());
        match (self.match_pool, active.config.key_concurrency) {
            (true, Some(per_key)) if usable > 0 => {
                (usable * per_key as usize).clamp(1, MAX_CONCURRENCY)
            }
            _ => self.concurrency,
        }
    }

    /// Record how many keys provider `id` now has, and in how many
    /// keychain items after the first.
    pub fn set_key_count(&mut self, id: &str, count: usize, parts: usize) -> bool {
        match self.provider_mut(id) {
            Some(p) => {
                p.key_count = count;
                p.key_parts = parts;
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
    let text = match read_patiently(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return AppSettings::default(),
        Err(e) => {
            // It may be fine (held by another program a moment longer):
            // keep it, and save nothing over it.
            log::error!(
                "cannot read {} ({e}); starting with defaults, which will not be saved",
                path.display()
            );
            return AppSettings {
                keep_file: true,
                ..AppSettings::default()
            };
        }
    };
    match serde_json::from_slice::<AppSettings>(&text) {
        Ok(s) => s.normalized(),
        Err(e) => {
            log::warn!(
                "{} is invalid ({e}); starting with defaults",
                path.display()
            );
            set_aside(path)
        }
    }
}

/// Read `path`, trying again for a moment if another program holds it
/// (antivirus or sync software on Windows).
fn read_patiently(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut attempt = 0;
    loop {
        match std::fs::read(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound && attempt < 5 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            result => return result,
        }
    }
}

/// Defaults in place of a settings file that cannot be used. The file is
/// moved aside, next to earlier ones rather than over them, so saving the
/// defaults does not destroy it; if it cannot be moved, saving is refused.
fn set_aside(path: &Path) -> AppSettings {
    let aside = (1..1000)
        .map(|n| match n {
            1 => path.with_extension("json.bad"),
            n => path.with_extension(format!("json.bad-{n}")),
        })
        .find(|p| !p.exists());
    let moved = match aside {
        Some(aside) => std::fs::rename(path, &aside)
            .map(|()| aside)
            .map_err(|e| e.to_string()),
        None => Err("too many earlier copies".to_owned()),
    };
    match moved {
        Ok(aside) => {
            log::warn!("the old settings are kept in {}", aside.display());
            AppSettings::default()
        }
        Err(e) => {
            log::error!(
                "cannot move {} aside ({e}); settings will not be saved",
                path.display()
            );
            AppSettings {
                keep_file: true,
                ..AppSettings::default()
            }
        }
    }
}

pub fn save(path: &Path, settings: &AppSettings) -> Result<(), String> {
    if settings.keep_file && path.exists() {
        return Err(format!(
            "{} could not be read, and is kept as it is: fix or remove it, then restart BiWrite",
            path.display()
        ));
    }
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
    /// Getting through applies (see `ProviderEntry::squeezable`).
    pub squeezable: bool,
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
    pub translate_delay: u64,
    pub match_pool: bool,
    /// Parallel requests in effect (see `AppSettings::effective_concurrency`).
    pub effective_concurrency: usize,
    pub latex: LatexSettings,
    pub check_updates: bool,
    pub close_to_tray: bool,
    pub keep_alive: bool,
    pub keep_alive_forever: bool,
    pub keep_alive_rejoin: bool,
    pub keys_in_file: bool,
    pub keep_alive_minutes: f64,
    pub squeeze_seconds: f64,
    pub squeeze_enough: usize,
    pub squeeze_effort: Effort,
    pub assisted_apply: bool,
    /// This build's version.
    pub version: String,
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
                squeezable: p.squeezable(),
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
        translate_delay: settings.translate_delay,
        match_pool: settings.match_pool,
        effective_concurrency: settings.effective_concurrency(),
        latex: settings.latex.clone(),
        check_updates: settings.check_updates,
        close_to_tray: settings.close_to_tray,
        keep_alive: settings.keep_alive,
        keep_alive_forever: settings.keep_alive_forever,
        keep_alive_rejoin: settings.keep_alive_rejoin,
        keys_in_file: settings.keys_in_file,
        keep_alive_minutes: settings.keep_alive_minutes,
        squeeze_seconds: settings.squeeze_seconds,
        squeeze_enough: settings.squeeze_enough,
        squeeze_effort: settings.squeeze_effort,
        assisted_apply: settings.assisted_apply,
        version: env!("CARGO_PKG_VERSION").to_owned(),
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

    #[test]
    fn keys_set_aside_take_no_requests_and_no_share_of_the_pool() {
        let mut s = AppSettings::default();
        let mut entry = s.active().clone();
        entry.config.id = "p-pool".into();
        entry.config.kind = biwrite_providers::ProviderKind::OpenaiCompatible;
        entry.config.key_concurrency = Some(2);
        entry.key_count = 6;
        entry
            .key_ignored
            .insert(biwrite_providers::key_fingerprint("sk-b"));
        assert_eq!(
            entry.usable(vec!["sk-a".into(), "sk-b".into(), "sk-c".into()]),
            ["sk-a", "sk-c"]
        );
        s.providers.push(entry);
        s.active_provider = "p-pool".into();
        s.match_pool = true;
        assert_eq!(s.effective_concurrency(), 10, "five keys of two each");
    }

    /// Settings written before these options read as on.
    #[test]
    fn keeping_alive_always_and_assisted_apply_are_on_unless_turned_off() {
        let old: AppSettings = serde_json::from_str(r#"{"keepAlive": true}"#).unwrap();
        assert!(old.keep_alive_forever && old.assisted_apply);
        assert!(
            !old.keep_alive_rejoin,
            "failed keys do not rejoin by default"
        );
        assert!(old.stopped_keys.is_empty());
        assert_eq!(old.kept_keys, None, "not noted by an older version");
        let off: AppSettings =
            serde_json::from_str(r#"{"keepAliveForever": false, "assistedApply": false}"#).unwrap();
        assert!(!off.keep_alive_forever && !off.assisted_apply);
        let paths = Paths::in_dir(&std::env::temp_dir());
        let v = view(&AppSettings::default(), String::new(), &paths);
        assert!(v.keep_alive_forever && v.assisted_apply);
        assert!(!v.keep_alive_rejoin);
    }

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
                long_context: None,
            },
            has_key: true,
            key_count: 2,
            key_names: BTreeMap::new(),
            key_through: Default::default(),
            key_parts: 0,
            key_ignored: Default::default(),
        }
    }

    #[test]
    fn the_translation_delay_is_five_seconds_unless_set_and_at_most_a_minute() {
        let dir =
            std::env::temp_dir().join(format!("biwrite-settings-delay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        // Files from before the delay existed.
        std::fs::write(&path, r#"{"concurrency": 3}"#).unwrap();
        assert_eq!(load(&path).translate_delay, 5);
        std::fs::write(&path, r#"{"translateDelay": 0}"#).unwrap();
        assert_eq!(load(&path).translate_delay, 0);
        std::fs::write(&path, r#"{"translateDelay": 3600}"#).unwrap();
        assert_eq!(load(&path).translate_delay, MAX_TRANSLATE_DELAY);
        let s = AppSettings {
            translate_delay: 12,
            ..AppSettings::default()
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path).translate_delay, 12);
        std::fs::remove_dir_all(&dir).unwrap();
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
        assert_eq!(
            p.vouched(),
            Vouched::FIRST,
            "a pool left by a newer version is not used"
        );
        assert_eq!(p.config.wire_api, WireApi::Chat);
        assert_eq!(
            s.assistant().config.id,
            "p-1",
            "the assistant follows translation"
        );
        assert_eq!(s.request_log, LogSettings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unusable_settings_file_is_kept() {
        let dir = std::env::temp_dir().join(format!("biwrite-settings-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        // Invalid twice: each copy is kept, the earlier one too.
        for text in ["{ not json", "[1, 2"] {
            std::fs::write(&path, text).unwrap();
            let s = load(&path);
            assert!(!s.keep_file);
            assert!(!path.exists());
            save(&path, &s).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("settings.json.bad")).unwrap(),
            "{ not json"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("settings.json.bad-2")).unwrap(),
            "[1, 2"
        );
        // Not UTF-8: unreadable as settings, also kept.
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        load(&path);
        assert_eq!(
            std::fs::read(dir.join("settings.json.bad-3")).unwrap(),
            [0xff, 0xfe, 0x00]
        );
        // A file that cannot be read stays where it is, and is not saved over.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&path, "{}").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            let s = load(&path);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(s.keep_file);
            assert!(save(&path, &s).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
            assert!(!dir.join("settings.json.bad-4").exists());
        }
        // A file that cannot be moved aside is never saved over.
        std::fs::write(&path, "{ mine").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |m| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(m));
            mode(0o555).unwrap();
            let s = load(&path);
            mode(0o755).unwrap();
            assert!(s.keep_file);
        }
        let s = AppSettings {
            keep_file: true,
            ..AppSettings::default()
        };
        assert!(save(&path, &s).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ mine");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stored_keys_are_read_as_far_as_the_settings_vouch() {
        let mut p = deepseek();
        let vouched = |p: &ProviderEntry| p.vouched();
        p.key_count = 1;
        assert_eq!(vouched(&p), Vouched::FIRST);
        // Written by this version: count and parts.
        (p.key_count, p.key_parts) = (40, 2);
        assert_eq!(vouched(&p), Vouched { keys: 40, parts: 2 });
        // Saved by 0.2.x, which drops the parts but keeps the count.
        p.key_parts = 0;
        assert_eq!(vouched(&p), Vouched::counted(40));
    }

    #[test]
    fn key_names_that_may_be_keys_are_dropped_on_load() {
        let mut s = AppSettings::default();
        let mut relay = deepseek();
        relay.key_names.insert("aaaaaaaaaaaa".into(), "main".into());
        relay.key_names.insert(
            "bbbbbbbbbbbb".into(),
            "“sk-proj-Ab3dE5gH7jK9mN1pQ3sT5vX7z".into(),
        );
        s.providers.push(relay);
        let s = s.normalized();
        let names = &s.provider("p-1").unwrap().key_names;
        assert_eq!(names.len(), 1);
        assert_eq!(names.get("aaaaaaaaaaaa").map(String::as_str), Some("main"));
    }
}
