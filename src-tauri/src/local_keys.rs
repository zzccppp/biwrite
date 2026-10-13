//! API keys in a file of the app's config folder that only the user's
//! account can read, by default: starting BiWrite or updating it asks for
//! no keychain password. The keys never leave this computer. A key the
//! keychain holds from an earlier version comes over the first time it is
//! read (the keychain asks that one last time), and the keychain is not
//! asked about it again. With the setting off, the keychain holds them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::secrets::SecretStore;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    keys: BTreeMap<String, String>,
    /// Accounts the keychain was asked about, or that were set or deleted
    /// here: the keychain is not read for them again.
    #[serde(default)]
    checked: BTreeSet<String>,
}

pub struct KeyFile {
    path: PathBuf,
    keychain: Arc<dyn SecretStore>,
    in_file: AtomicBool,
    /// The file as last read or written.
    stored: Mutex<Option<Stored>>,
}

impl KeyFile {
    pub fn new(path: PathBuf, keychain: Arc<dyn SecretStore>, in_file: bool) -> Self {
        Self {
            path,
            keychain,
            in_file: AtomicBool::new(in_file),
            stored: Mutex::new(None),
        }
    }

    fn in_file(&self) -> bool {
        self.in_file.load(Ordering::Acquire)
    }

    /// `change` applied to the stored keys (read first if need be), and the
    /// file written when `write`.
    fn with<T>(&self, write: bool, change: impl FnOnce(&mut Stored) -> T) -> Result<T, String> {
        let mut guard = self.stored.lock().unwrap_or_else(PoisonError::into_inner);
        let stored = match &mut *guard {
            Some(stored) => stored,
            empty => empty.insert(self.read()?),
        };
        let out = change(stored);
        if write {
            self.write(stored)?;
        }
        Ok(out)
    }

    fn read(&self) -> Result<Stored, String> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("{} could not be read: {e}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Stored::default()),
            Err(e) => Err(format!("{}: {e}", self.path.display())),
        }
    }

    /// Written whole to a new file readable only by the user, then put in
    /// place.
    fn write(&self, stored: &Stored) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(stored).map_err(|e| e.to_string())?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options.open(&tmp).and_then(|mut f| {
            use std::io::Write;
            f.write_all(&bytes)?;
            f.sync_all()
        });
        #[cfg(unix)]
        let written = written.and_then(|()| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
        });
        written
            .and_then(|()| std::fs::rename(&tmp, &self.path))
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

impl SecretStore for KeyFile {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        if !self.in_file() {
            return self.keychain.get(account);
        }
        let (found, checked) = self.with(false, |s| {
            (s.keys.get(account).cloned(), s.checked.contains(account))
        })?;
        if found.is_some() || checked {
            return Ok(found);
        }
        // Once: what the keychain holds comes over (refused, it is asked
        // again next time).
        let old = self.keychain.get(account)?;
        self.with(true, |s| {
            if let Some(key) = &old {
                s.keys.insert(account.to_owned(), key.clone());
            }
            s.checked.insert(account.to_owned());
        })?;
        Ok(old)
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        if !self.in_file() {
            return self.keychain.set(account, secret);
        }
        self.with(true, |s| {
            s.keys.insert(account.to_owned(), secret.to_owned());
            s.checked.insert(account.to_owned());
        })
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        if !self.in_file() {
            return self.keychain.delete(account);
        }
        self.with(true, |s| {
            s.keys.remove(account);
            s.checked.insert(account.to_owned());
        })
    }

    fn move_keys(&self, to_file: bool, accounts: &[String]) -> Result<(), String> {
        if to_file == self.in_file() {
            return Ok(());
        }
        if to_file {
            // Read through the keychain as the file is now in use.
            self.in_file.store(true, Ordering::Release);
            for account in accounts {
                if let Err(e) = self.get(account) {
                    self.in_file.store(false, Ordering::Release);
                    return Err(e);
                }
            }
            return Ok(());
        }
        let keys: Vec<(String, String)> = self.with(false, |s| {
            s.keys.iter().map(|(a, k)| (a.clone(), k.clone())).collect()
        })?;
        for (account, key) in &keys {
            self.keychain.set(account, key)?;
        }
        self.with(true, |s| {
            s.keys.clear();
            s.checked.clear();
        })?;
        self.in_file.store(false, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("biwrite-keys-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Counts the keychain's reads.
    #[derive(Default)]
    struct Counted {
        inner: MemoryStore,
        reads: Mutex<usize>,
    }

    impl SecretStore for Counted {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            *self.reads.lock().unwrap() += 1;
            self.inner.get(account)
        }
        fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            self.inner.set(account, secret)
        }
        fn delete(&self, account: &str) -> Result<(), String> {
            self.inner.delete(account)
        }
    }

    #[test]
    fn a_key_of_the_keychain_comes_over_once_and_is_read_from_the_file_after() {
        let dir = temp("over");
        let keychain = Arc::new(Counted::default());
        keychain.inner.set("p-1", "sk-old").unwrap();
        let path = dir.join("keys.json");
        let store = KeyFile::new(path.clone(), keychain.clone(), true);
        assert_eq!(store.get("p-1").unwrap().as_deref(), Some("sk-old"));
        assert_eq!(store.get("p-1").unwrap().as_deref(), Some("sk-old"));
        assert_eq!(store.get("p-2").unwrap(), None);
        assert_eq!(store.get("p-2").unwrap(), None);
        assert_eq!(
            *keychain.reads.lock().unwrap(),
            2,
            "each account asked once"
        );
        // After a restart, from the file only.
        let again = KeyFile::new(path.clone(), keychain.clone(), true);
        assert_eq!(again.get("p-1").unwrap().as_deref(), Some("sk-old"));
        assert_eq!(again.get("p-2").unwrap(), None);
        assert_eq!(*keychain.reads.lock().unwrap(), 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "only the user reads it");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keys_set_and_deleted_stay_in_the_file() {
        let dir = temp("set");
        let keychain = Arc::new(Counted::default());
        keychain.inner.set("p-1", "sk-old").unwrap();
        let store = KeyFile::new(dir.join("keys.json"), keychain.clone(), true);
        store.set("p-1", "sk-new").unwrap();
        assert_eq!(store.get("p-1").unwrap().as_deref(), Some("sk-new"));
        store.delete("p-1").unwrap();
        assert_eq!(
            store.get("p-1").unwrap(),
            None,
            "the keychain's copy is not read back"
        );
        assert_eq!(*keychain.reads.lock().unwrap(), 0);
        assert_eq!(
            keychain.inner.get("p-1").unwrap().as_deref(),
            Some("sk-old")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keys_move_to_the_keychain_and_back() {
        let dir = temp("move");
        let keychain = Arc::new(Counted::default());
        let store = KeyFile::new(dir.join("keys.json"), keychain.clone(), true);
        store.set("p-1", "sk-a").unwrap();
        store.set("p-1#pool", "sk-b\nsk-c").unwrap();
        let accounts = ["p-1".to_owned(), "p-1#pool".to_owned()];
        store.move_keys(false, &accounts).unwrap();
        assert_eq!(
            keychain.inner.get("p-1#pool").unwrap().as_deref(),
            Some("sk-b\nsk-c")
        );
        let text = std::fs::read_to_string(dir.join("keys.json")).unwrap();
        assert!(!text.contains("sk-"), "{text}");
        assert_eq!(store.get("p-1").unwrap().as_deref(), Some("sk-a"));
        store.move_keys(true, &accounts).unwrap();
        let text = std::fs::read_to_string(dir.join("keys.json")).unwrap();
        assert!(text.contains("sk-b"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
