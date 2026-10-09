//! API key storage in the OS credential store (macOS Keychain, Windows
//! Credential Manager). Keys are written from the settings form and read
//! only by the providers in Rust; no command ever returns one.

/// Keychain service name for all BiWrite secrets.
pub const SERVICE: &str = "app.biwrite.desktop";

pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, secret: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

/// The platform credential store. Calls may block (and may show an OS
/// prompt), so call them off the async runtime.
pub struct Keychain;

impl Keychain {
    fn entry(account: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, account).map_err(|e| e.to_string())
    }
}

impl SecretStore for Keychain {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        Self::entry(account)?
            .set_password(secret)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Most keys one provider may hold.
pub const MAX_KEYS: usize = 64;

/// Keychain account holding a provider's additional keys. The first key
/// stays under the provider id itself, which is all that versions without
/// key pools read, so they keep working with that key.
fn pool_account(id: &str) -> String {
    format!("{id}#pool")
}

/// Split pasted text into keys: one per line, or separated by spaces or
/// commas. Duplicates are dropped and the order is kept.
pub fn parse_keys(text: &str) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = Vec::new();
    for key in text.split(|c: char| c.is_whitespace() || c == ',') {
        let key = key.trim().trim_matches(['"', '\'']);
        if key.is_empty() || keys.iter().any(|k| k == key) {
            continue;
        }
        if key.len() > 1024 || key.chars().any(char::is_control) {
            return Err("that does not look like an API key".into());
        }
        keys.push(key.to_owned());
    }
    if keys.is_empty() {
        return Err("the key is empty".into());
    }
    if keys.len() > MAX_KEYS {
        return Err(format!("at most {MAX_KEYS} keys per provider"));
    }
    Ok(keys)
}

/// All keys of provider `id`, in pool order (blocking).
pub fn get_keys(store: &dyn SecretStore, id: &str) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = store.get(id)?.into_iter().collect();
    if let Some(rest) = store.get(&pool_account(id))? {
        for key in rest.lines().map(str::trim).filter(|k| !k.is_empty()) {
            if !keys.iter().any(|k| k == key) {
                keys.push(key.to_owned());
            }
        }
    }
    Ok(keys)
}

/// Store exactly `keys` for provider `id` (blocking). An empty list deletes them.
pub fn set_keys(store: &dyn SecretStore, id: &str, keys: &[String]) -> Result<(), String> {
    match keys.split_first() {
        None => delete_keys(store, id),
        Some((first, rest)) => {
            store.set(id, first)?;
            if rest.is_empty() {
                store.delete(&pool_account(id))
            } else {
                store.set(&pool_account(id), &rest.join("\n"))
            }
        }
    }
}

/// Delete every key of provider `id` (blocking).
pub fn delete_keys(store: &dyn SecretStore, id: &str) -> Result<(), String> {
    store.delete(id)?;
    store.delete(&pool_account(id))
}

#[cfg(test)]
pub use memory::MemoryStore;

#[cfg(test)]
mod pool_tests {
    use super::*;

    #[test]
    fn pasted_keys_are_split_and_deduplicated() {
        assert_eq!(
            parse_keys(" sk-a\nsk-b, sk-a\r\n\"sk-c\"  ").unwrap(),
            ["sk-a", "sk-b", "sk-c"]
        );
        assert!(parse_keys(" \n ").is_err());
        assert!(parse_keys("sk-\u{7}bad").is_err());
    }

    #[test]
    fn the_first_key_stays_where_older_versions_look() {
        let store = MemoryStore::default();
        let keys: Vec<String> = ["sk-a", "sk-b", "sk-c"].map(String::from).to_vec();
        set_keys(&store, "p-1", &keys).unwrap();
        assert_eq!(store.get("p-1").unwrap().as_deref(), Some("sk-a"));
        assert_eq!(get_keys(&store, "p-1").unwrap(), keys);

        set_keys(&store, "p-1", &keys[1..2]).unwrap();
        assert_eq!(get_keys(&store, "p-1").unwrap(), ["sk-b"]);
        assert_eq!(store.get("p-1#pool").unwrap(), None);

        // A key stored by an older version is a pool of one.
        store.set("p-2", "sk-old").unwrap();
        assert_eq!(get_keys(&store, "p-2").unwrap(), ["sk-old"]);

        delete_keys(&store, "p-1").unwrap();
        assert!(get_keys(&store, "p-1").unwrap().is_empty());
    }
}

#[cfg(test)]
mod memory {
    use std::collections::HashMap;
    use std::sync::{Mutex, PoisonError};

    use super::SecretStore;

    /// In-memory store (tests).
    #[derive(Default)]
    pub struct MemoryStore {
        map: Mutex<HashMap<String, String>>,
    }

    impl SecretStore for MemoryStore {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            Ok(self
                .map
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(account)
                .cloned())
        }

        fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            self.map
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(account.to_owned(), secret.to_owned());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<(), String> {
            self.map
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(account);
            Ok(())
        }
    }
}
