//! Translation cache keyed by (content hash, direction, provider, model,
//! glossary fingerprint).
//!
//! The cache is shared across files: an identical paragraph in another
//! document is never translated twice. [`crate::SqliteCache`] persists it;
//! [`MemoryCache`] is for tests and as a fallback.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use biwrite_core::{ContentHash, Direction};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub hash: ContentHash,
    pub direction: Direction,
    pub provider: String,
    pub model: String,
    /// [`biwrite_core::glossary::fingerprint`] of the glossary entries sent
    /// with the request (0: none), so glossary edits only invalidate the
    /// paragraphs that mention the edited terms.
    pub glossary: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("translation cache error: {0}")]
pub struct CacheError(pub String);

/// What the cache holds, for the settings UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStats {
    pub entries: u64,
    /// Approximate size in bytes (database pages, or text in memory).
    pub bytes: u64,
    /// Entries per provider, model and direction, largest first.
    pub groups: Vec<CacheGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheGroup {
    pub provider: String,
    pub model: String,
    pub direction: Direction,
    pub entries: u64,
}

pub trait TranslationCache: Send + Sync {
    fn get(&self, key: &CacheKey) -> Result<Option<String>, CacheError>;
    fn put(&self, key: &CacheKey, translation: &str) -> Result<(), CacheError>;
    fn stats(&self) -> Result<CacheStats, CacheError>;
    /// Delete every entry (and give the space back). Returns how many there
    /// were.
    fn clear(&self) -> Result<u64, CacheError>;
}

/// Sort groups largest first, then by name, for a stable display.
pub(crate) fn sort_groups(groups: &mut [CacheGroup]) {
    groups.sort_by(|a, b| {
        b.entries
            .cmp(&a.entries)
            .then_with(|| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)))
            .then_with(|| a.direction.as_str().cmp(b.direction.as_str()))
    });
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

    fn stats(&self) -> Result<CacheStats, CacheError> {
        let map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        let mut counts: HashMap<(&str, &str, Direction), u64> = HashMap::new();
        let mut bytes = 0;
        for (key, text) in map.iter() {
            *counts
                .entry((&key.provider, &key.model, key.direction))
                .or_default() += 1;
            bytes += (32 + key.provider.len() + key.model.len() + text.len()) as u64;
        }
        let mut groups: Vec<CacheGroup> = counts
            .into_iter()
            .map(|((provider, model, direction), entries)| CacheGroup {
                provider: provider.to_owned(),
                model: model.to_owned(),
                direction,
                entries,
            })
            .collect();
        sort_groups(&mut groups);
        Ok(CacheStats {
            entries: map.len() as u64,
            bytes,
            groups,
        })
    }

    fn clear(&self) -> Result<u64, CacheError> {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        let n = map.len() as u64;
        map.clear();
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str, model: &str, direction: Direction) -> CacheKey {
        CacheKey {
            hash: ContentHash::of(text),
            direction,
            provider: "p".into(),
            model: model.into(),
            glossary: 0,
        }
    }

    #[test]
    fn memory_stats_and_clear() {
        let cache = MemoryCache::default();
        cache.put(&key("a", "m1", Direction::EnZh), "甲").unwrap();
        cache.put(&key("b", "m1", Direction::EnZh), "乙").unwrap();
        cache.put(&key("c", "m2", Direction::ZhEn), "C").unwrap();
        let stats = cache.stats().unwrap();
        assert_eq!(stats.entries, 3);
        assert!(stats.bytes > 0);
        assert_eq!(
            stats
                .groups
                .iter()
                .map(|g| (g.model.as_str(), g.direction, g.entries))
                .collect::<Vec<_>>(),
            [("m1", Direction::EnZh, 2), ("m2", Direction::ZhEn, 1)]
        );
        assert_eq!(cache.clear().unwrap(), 3);
        assert_eq!(cache.stats().unwrap(), CacheStats::default());
    }
}
