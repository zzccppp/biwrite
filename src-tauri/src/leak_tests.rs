//! Acceptance: the API key never appears in IPC payloads or on disk outside
//! the OS credential store.

use std::sync::Arc;

use biwrite_core::Mode;
use biwrite_engine::{Engine, EngineSettings, MemoryCache, MockTranslator, NullSink};
use biwrite_providers::{Effort, ProviderConfig, ProviderKind};

use crate::secrets::{MemoryStore, SecretStore};
use crate::settings::{AppSettings, Paths, ProviderEntry};
use crate::state::AppState;

const KEY: &str = "sk-leaktest-0123456789abcdefghij";

#[tokio::test]
async fn key_never_appears_in_views_or_settings_file() {
    let dir = std::env::temp_dir().join(format!("biwrite-leak-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let paths = Paths::in_dir(&dir);
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    secrets.set("p-1", KEY).unwrap();

    let mut settings = AppSettings::default();
    settings.providers.push(ProviderEntry {
        config: ProviderConfig {
            id: "p-1".into(),
            name: "DeepSeek".into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            temperature: 0.0,
            effort: Effort::Low,
        },
        has_key: true,
    });
    settings.active_provider = "p-1".into();

    let cache = Arc::new(MemoryCache::default());
    let engine = Engine::new(
        Arc::new(MockTranslator::default()),
        cache.clone(),
        Arc::new(NullSink),
        EngineSettings::default(),
        tokio::runtime::Handle::current(),
    );
    let state = AppState::new(engine, cache, settings, paths.clone(), secrets);
    state.apply_active_provider().unwrap();
    let snapshot = state.engine.load("Hello world.".into(), Mode::Plain);

    let payloads = [
        serde_json::to_string(&state.settings_view()).unwrap(),
        serde_json::to_string(&state.session_view(snapshot)).unwrap(),
    ];
    for json in &payloads {
        assert!(!json.contains(KEY), "key leaked into IPC payload: {json}");
        assert!(!json.contains("0123456789abcdef"));
    }
    assert!(payloads[0].contains("\"hasKey\":true"));

    state.persist(&state.settings()).unwrap();
    let on_disk = std::fs::read_to_string(&paths.settings).unwrap();
    assert!(!on_disk.contains(KEY));
    std::fs::remove_dir_all(&dir).unwrap();
}
