//! Translation cache keyed by (content hash, direction, provider, model,
//! glossary version).
//!
//! The cache is shared across files: an identical paragraph in another
//! document is never translated twice. [`crate::SqliteCache`] persists it;
//! [`MemoryCache`] is for tests and as a fallback.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use biwrite_core::{ContentHash, Direction};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub hash: ContentHash,
    pub direction: Direction,
    pub provider: String,
    pub model: String,
    pub glossary_version: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("translation cache error: {0}")]
pub struct CacheError(pub String);

pub trait TranslationCache: Send + Sync {
    fn get(&self, key: &CacheKey) -> Result<Option<String>, CacheError>;
    fn put(&self, key: &CacheKey, translation: &str) -> Result<(), CacheError>;
}

/// Process-local cache.
#[derive(Default)]
pub struct MemoryCache {
    map: Mutex<HashMap<CacheKey, String>>,
}

impl MemoryCache {
    pub fn len(&self) -> usize {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl TranslationCache for MemoryCache {
    fn get(&self, key: &CacheKey) -> Result<Option<String>, CacheError> {
        let map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(map.get(key).cloned())
    }

    fn put(&self, key: &CacheKey, translation: &str) -> Result<(), CacheError> {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        map.insert(key.clone(), translation.to_owned());
        Ok(())
    }
}
