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

/// A pasted key with the name written before it, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedKey {
    pub name: Option<String>,
    pub key: String,
}

/// Longest key name kept.
const MAX_NAME_CHARS: usize = 40;

/// Looks like an API key rather than a name: key characters only, and
/// either the usual `sk-` prefix or long with letters and digits mixed.
fn is_key_token(token: &str) -> bool {
    let key_chars = token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    let prefixed = token.starts_with("sk-") || token.starts_with("sk_");
    key_chars
        && if prefixed {
            token.len() >= 16
        } else {
            token.len() >= 24
                && token.chars().any(|c| c.is_ascii_digit())
                && token.chars().any(|c| c.is_ascii_alphabetic())
        }
}

/// Split pasted text into keys with optional names. Accepted shapes, mixed
/// freely: a key per line; a name on one line and its key on the next;
/// `name key`, `name: key`, `name,key` or `name=key` on one line. Keys
/// may also be separated by spaces or commas. Duplicates are dropped (the
/// first name wins) and the order is kept.
pub fn parse_named_keys(text: &str) -> Result<Vec<NamedKey>, String> {
    let mut out: Vec<NamedKey> = Vec::new();
    let mut pending_name: Option<String> = None;
    for line in text.lines() {
        // Comments (the header of an exported pool).
        if line.trim_start().starts_with('#') {
            continue;
        }
        let tokens: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '='))
            .map(|t| t.trim().trim_matches(['"', '\'']).trim_end_matches(':'))
            .filter(|t| !t.is_empty())
            .collect();
        let mut words: Vec<&str> = Vec::new();
        for token in tokens {
            if token.chars().any(char::is_control) {
                return Err("that does not look like an API key".into());
            }
            if is_key_token(token) {
                if token.len() > 1024 {
                    return Err("that does not look like an API key".into());
                }
                let name = if words.is_empty() {
                    pending_name.take()
                } else {
                    Some(words.join(" "))
                };
                words.clear();
                let name = name
                    .map(|n| n.chars().take(MAX_NAME_CHARS).collect::<String>())
                    .filter(|n| !n.trim().is_empty());
                if !out.iter().any(|k| k.key == token) {
                    out.push(NamedKey {
                        name,
                        key: token.to_owned(),
                    });
                }
            } else {
                words.push(token);
            }
        }
        if !words.is_empty() {
            pending_name = Some(words.join(" "));
        }
    }
    if out.is_empty() {
        return Err("no API key found in the text".into());
    }
    if out.len() > MAX_KEYS {
        return Err(format!("at most {MAX_KEYS} keys per provider"));
    }
    Ok(out)
}

/// Split pasted text into keys, ignoring names (see [`parse_named_keys`]).
#[cfg(test)]
pub fn parse_keys(text: &str) -> Result<Vec<String>, String> {
    Ok(parse_named_keys(text)?.into_iter().map(|k| k.key).collect())
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

    const A: &str = "sk-aaaaaaaaaaaaaaaaaaaaaaaa1";
    const B: &str = "sk-bbbbbbbbbbbbbbbbbbbbbbbb2";
    const C: &str = "sk-cccccccccccccccccccccccc3";

    #[test]
    fn pasted_keys_are_split_and_deduplicated() {
        assert_eq!(
            parse_keys(&format!(" {A}\n{B}, {A}\r\n\"{C}\"  ")).unwrap(),
            [A, B, C]
        );
        assert!(parse_keys(" \n ").is_err());
        assert!(parse_keys("just some words").is_err());
        assert!(parse_keys(&format!("{A}\u{7}")).is_err());
    }

    #[test]
    fn names_come_from_the_line_before_or_the_same_line() {
        let text = format!("main\n{A}\nlab: {B}\n{C}\n");
        let keys = parse_named_keys(&text).unwrap();
        let named: Vec<(Option<&str>, &str)> = keys
            .iter()
            .map(|k| (k.name.as_deref(), k.key.as_str()))
            .collect();
        assert_eq!(named, [(Some("main"), A), (Some("lab"), B), (None, C)]);
        let one_line = parse_named_keys(&format!("main account, {A}")).unwrap();
        assert_eq!(one_line[0].name.as_deref(), Some("main account"));
        let digits_in_name = parse_named_keys(&format!("account2024\n{B}")).unwrap();
        assert_eq!(digits_in_name[0].name.as_deref(), Some("account2024"));
    }

    #[test]
    fn the_first_key_stays_where_older_versions_look() {
        let store = MemoryStore::default();
        let keys: Vec<String> = [A, B, C].map(String::from).to_vec();
        set_keys(&store, "p-1", &keys).unwrap();
        assert_eq!(store.get("p-1").unwrap().as_deref(), Some(A));
        assert_eq!(get_keys(&store, "p-1").unwrap(), keys);

        set_keys(&store, "p-1", &keys[1..2]).unwrap();
        assert_eq!(get_keys(&store, "p-1").unwrap(), [B]);
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
