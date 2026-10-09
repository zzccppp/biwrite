//! Persistent translation cache in SQLite, shared across files and sessions.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::cache::{CacheError, CacheKey, TranslationCache};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS translations (
    hash             BLOB    NOT NULL,
    direction        TEXT    NOT NULL,
    provider         TEXT    NOT NULL,
    model            TEXT    NOT NULL,
    glossary_version INTEGER NOT NULL,
    translation      TEXT    NOT NULL,
    created_at       INTEGER NOT NULL,
    PRIMARY KEY (hash, direction, provider, model, glossary_version)
) WITHOUT ROWID;
";

impl From<rusqlite::Error> for CacheError {
    fn from(e: rusqlite::Error) -> Self {
        Self(e.to_string())
    }
}

pub struct SqliteCache {
    conn: Mutex<Connection>,
}

impl SqliteCache {
    /// Open (or create) the cache database at `path`.
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        let conn = Connection::open(path)?;
        // Lookups run under the engine lock: never wait long on another process.
        conn.busy_timeout(Duration::from_millis(250))?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    /// A private in-memory database (tests, fallback).
    pub fn in_memory() -> Result<Self, CacheError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, CacheError> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Number of cached translations.
    pub fn len(&self) -> Result<u64, CacheError> {
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM translations", [], |row| row.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    pub fn is_empty(&self) -> Result<bool, CacheError> {
        Ok(self.len()? == 0)
    }
}

fn glossary_version(key: &CacheKey) -> i64 {
    i64::try_from(key.glossary_version).unwrap_or(i64::MAX)
}

impl TranslationCache for SqliteCache {
    fn get(&self, key: &CacheKey) -> Result<Option<String>, CacheError> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT translation FROM translations
             WHERE hash = ?1 AND direction = ?2 AND provider = ?3 AND model = ?4
               AND glossary_version = ?5",
        )?;
        let found = stmt
            .query_row(
                params![
                    key.hash.as_bytes().as_slice(),
                    key.direction.as_str(),
                    key.provider,
                    key.model,
                    glossary_version(key)
                ],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found)
    }

    fn put(&self, key: &CacheKey, translation: &str) -> Result<(), CacheError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "INSERT INTO translations
                 (hash, direction, provider, model, glossary_version, translation, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (hash, direction, provider, model, glossary_version)
             DO UPDATE SET translation = excluded.translation, created_at = excluded.created_at",
        )?;
        stmt.execute(params![
            key.hash.as_bytes().as_slice(),
            key.direction.as_str(),
            key.provider,
            key.model,
            glossary_version(key),
            translation,
            now
        ])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use biwrite_core::{ContentHash, Direction};

    use super::*;

    fn key(text: &str) -> CacheKey {
        CacheKey {
            hash: ContentHash::of(text),
            direction: Direction::EnZh,
            provider: "p".into(),
            model: "m".into(),
            glossary_version: 0,
        }
    }

    #[test]
    fn get_put_and_overwrite() {
        let cache = SqliteCache::in_memory().unwrap();
        assert_eq!(cache.get(&key("a")).unwrap(), None);
        cache.put(&key("a"), "甲").unwrap();
        assert_eq!(cache.get(&key("a")).unwrap().as_deref(), Some("甲"));
        cache.put(&key("a"), "甲二").unwrap();
        assert_eq!(cache.get(&key("a")).unwrap().as_deref(), Some("甲二"));
        assert_eq!(cache.len().unwrap(), 1);
    }

    #[test]
    fn every_key_part_matters() {
        let cache = SqliteCache::in_memory().unwrap();
        let base = key("a");
        cache.put(&base, "x").unwrap();
        let variants = [
            CacheKey {
                direction: Direction::ZhEn,
                ..base.clone()
            },
            CacheKey {
                provider: "q".into(),
                ..base.clone()
            },
            CacheKey {
                model: "n".into(),
                ..base.clone()
            },
            CacheKey {
                glossary_version: 1,
                ..base.clone()
            },
            key("b"),
        ];
        for v in variants {
            assert_eq!(cache.get(&v).unwrap(), None, "{v:?}");
        }
        // Whitespace-only differences share an entry (normalized hash).
        assert_eq!(cache.get(&key(" a\n")).unwrap().as_deref(), Some("x"));
    }

    #[test]
    fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("biwrite-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.sqlite3");
        {
            let cache = SqliteCache::open(&path).unwrap();
            cache.put(&key("kept"), "保留").unwrap();
        }
        let cache = SqliteCache::open(&path).unwrap();
        assert_eq!(cache.get(&key("kept")).unwrap().as_deref(), Some("保留"));
        drop(cache);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
