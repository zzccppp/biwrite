//! Persistent app settings (`<config dir>/settings.json`) and the view sent
//! to the settings UI. Never contains API keys: only whether one is stored.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use biwrite_core::{Direction, GlossaryEntry};
use biwrite_providers::prompt::{default_prompt, prompt_file_name};
use biwrite_providers::{Effort, Preset, ProviderConfig, ProviderKind, presets};
use serde::{Deserialize, Serialize};

pub const MOCK_ID: &str = "mock";
const DEFAULT_CONCURRENCY: usize = 4;

/// A configured provider plus non-secret key status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    #[serde(flatten)]
    pub config: ProviderConfig,
    /// A key is stored in the OS keychain for this provider.
    #[serde(default)]
    pub has_key: bool,
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
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            providers: vec![mock_entry()],
            active_provider: MOCK_ID.to_owned(),
            concurrency: DEFAULT_CONCURRENCY,
            doc_notes: BTreeMap::new(),
            glossary: Vec::new(),
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
        },
        has_key: false,
    }
}

impl AppSettings {
    /// Repair invariants after loading: the mock provider exists, the active
    /// provider exists, concurrency is sane.
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
        self.concurrency = self.concurrency.clamp(1, 16);
        self
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
            eprintln!(
                "BiWrite: {} is invalid ({e}); starting with defaults",
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
        eprintln!("BiWrite: cannot create {}: {e}", dir.display());
        return;
    }
    for direction in [Direction::EnZh, Direction::ZhEn] {
        let path = dir.join(prompt_file_name(direction));
        if !path.exists()
            && let Err(e) = std::fs::write(&path, format!("{}\n", default_prompt(direction)))
        {
            eprintln!("BiWrite: cannot write {}: {e}", path.display());
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
}

pub fn view(settings: &AppSettings, doc_note: String, prompt_dir: &Path) -> SettingsView {
    let active = settings.active();
    SettingsView {
        providers: settings
            .providers
            .iter()
            .map(|p| ProviderView {
                config: p.config.clone(),
                has_key: p.has_key,
                builtin: p.config.id == MOCK_ID,
                needs_key: p.config.kind.needs_key(),
            })
            .collect(),
        active_provider: active.config.id.clone(),
        active_label: label(&active.config),
        concurrency: settings.concurrency,
        doc_note,
        presets: presets(),
        prompt_dir: prompt_dir.display().to_string(),
    }
}

pub fn label(config: &ProviderConfig) -> String {
    match config.kind {
        ProviderKind::Mock => "Mock".to_owned(),
        _ => format!("{} · {}", config.name, config.model),
    }
}

/// Paths of the settings file and prompt directory.
#[derive(Clone, Debug)]
pub struct Paths {
    pub settings: PathBuf,
    pub prompts: PathBuf,
}

impl Paths {
    pub fn in_dir(config_dir: &Path) -> Self {
        Self {
            settings: config_dir.join("settings.json"),
            prompts: config_dir.join("prompts"),
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
            },
            has_key: true,
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
        assert_eq!(repaired.concurrency, 16);

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
}
