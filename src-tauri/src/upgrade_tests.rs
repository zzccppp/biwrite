//! Upgrading from 0.2.2: its settings file and its keychain items are read
//! as they are. Every key of a pool comes back in order with its name, and
//! reading writes nothing to the keychain.

use std::sync::{Arc, Mutex, PoisonError};

use biwrite_providers::key_fingerprint;

use crate::secrets::{MemoryStore, SecretStore};
use crate::{provider_state, settings};

const KEYS: [&str; 3] = [
    "sk-test-0001-aaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "sk-test-0002-bbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "sk-test-0003-cccccccccccccccccccccccccccc",
];
const NAMES: [&str; 3] = ["main", "lab", "backup"];

/// A keychain that counts writes.
#[derive(Default)]
struct Watched {
    inner: MemoryStore,
    writes: Mutex<usize>,
}

impl Watched {
    fn writes(&self) -> usize {
        *self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn count(&self) {
        *self.writes.lock().unwrap_or_else(PoisonError::into_inner) += 1;
    }
}

impl SecretStore for Watched {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        self.inner.get(account)
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        self.count();
        self.inner.set(account, secret)
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.count();
        self.inner.delete(account)
    }
}

/// `settings.json` as 0.2.2 wrote it: a pool of three named keys, its count
/// and no `keyParts`, names keyed by the keys' fingerprints.
fn settings_0_2_2() -> String {
    format!(
        r#"{{
  "providers": [
    {{"id": "mock", "name": "Mock (offline, reverses text)", "kind": "mock", "baseUrl": "", "model": "reverse", "temperature": 0.0, "effort": "low", "hasKey": false}},
    {{"id": "p-1", "name": "Pool", "kind": "openai_compatible", "baseUrl": "https://api.example.com/v1", "model": "model-x", "temperature": 0.0, "effort": "high", "wireApi": "responses", "serviceTier": "priority", "keyConcurrency": 2, "maxRetries": 10, "hasKey": true, "keyCount": 3, "keyNames": {{"{}": "{}", "{}": "{}", "{}": "{}"}}}}
  ],
  "activeProvider": "p-1",
  "concurrency": 6
}}"#,
        key_fingerprint(KEYS[0]),
        NAMES[0],
        key_fingerprint(KEYS[1]),
        NAMES[1],
        key_fingerprint(KEYS[2]),
        NAMES[2],
    )
}

#[test]
fn settings_and_keys_saved_by_0_2_2_load_as_they_are() {
    let dir = std::env::temp_dir().join(format!("biwrite-upgrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");
    std::fs::write(&path, settings_0_2_2()).unwrap();

    // The keychain as 0.2.2 left it: the first key under the provider's id,
    // the others in its pool item.
    let store = Arc::new(Watched::default());
    store.inner.set("p-1", KEYS[0]).unwrap();
    store.inner.set("p-1#pool", &KEYS[1..].join("\n")).unwrap();

    let s = settings::load(&path);
    assert_eq!(s.active_provider, "p-1");
    let entry = s
        .providers
        .iter()
        .find(|p| p.config.id == "p-1")
        .expect("the provider");
    assert_eq!(entry.key_count, 3);
    let keychain: Arc<dyn SecretStore> = store.clone();
    let keys = provider_state::key_fn(&keychain, entry)().unwrap();
    assert_eq!(keys, KEYS);
    for (key, name) in KEYS.iter().zip(NAMES) {
        assert_eq!(
            entry
                .key_names
                .get(&key_fingerprint(key))
                .map(String::as_str),
            Some(name)
        );
    }
    assert_eq!(store.writes(), 0, "reading wrote to the keychain");
    // The settings file itself is left as it was.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), settings_0_2_2());
    std::fs::remove_dir_all(&dir).unwrap();
}
