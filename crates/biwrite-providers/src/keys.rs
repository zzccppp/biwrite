//! API key pool. A provider may hold several keys (for example accounts at a
//! relay), and requests rotate through them.
//!
//! A key that hits a rate limit cools down for a while, and a key the server
//! rejects (invalid, no permission, out of quota) moves to the back. Rotation
//! prefers keys in neither state. When no such key is left, the pool still
//! hands out the key most likely to work, so a real request always goes out
//! and the server decides. With a single key this is exactly the behaviour of
//! a provider without a pool.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use biwrite_engine::TranslateError;
use serde::Serialize;

use crate::observe::KeyUse;

/// Fetches the provider's keys (from the OS keychain in the app), in pool
/// order. Called at most once per translator, lazily, on a blocking thread.
pub type KeyFn = Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;

/// Cooldown after a rate limit that came without a `Retry-After` hint.
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(5);
/// Longest cooldown, whatever the server asks for.
const MAX_COOLDOWN: Duration = Duration::from_secs(120);

/// A key handed out for one request.
#[derive(Clone, Debug)]
pub(crate) struct Lease {
    pub index: usize,
    pub count: usize,
    pub key: String,
}

impl Lease {
    pub fn key_use(&self) -> KeyUse {
        KeyUse {
            number: self.index + 1,
            count: self.count,
            tail: tail(&self.key),
        }
    }
}

/// The last four characters of a key.
pub fn tail(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    chars[chars.len().saturating_sub(4)..].iter().collect()
}

/// State of one key, for the settings view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub number: usize,
    pub tail: String,
    /// `ready`, `cooling` or `rejected`.
    pub state: &'static str,
    /// Why the key was set aside (the server's message, redacted).
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Slot {
    cool_until: Option<Instant>,
    rejected: Option<(Instant, String)>,
}

#[derive(Debug, Default)]
struct PoolState {
    next: usize,
    slots: Vec<Slot>,
}

pub(crate) struct KeyPool {
    /// Provider name, for messages.
    name: String,
    fetch: KeyFn,
    loaded: tokio::sync::Mutex<Option<Result<Arc<[String]>, TranslateError>>>,
    state: Mutex<PoolState>,
}

/// How a failed request reflects on the key it used.
#[derive(Debug, PartialEq, Eq)]
enum Fault {
    None,
    Cooldown(Duration),
    Rejected,
}

fn fault(err: &TranslateError) -> Fault {
    match err {
        TranslateError::RateLimited { retry_after } => Fault::Cooldown(
            retry_after
                .unwrap_or(DEFAULT_COOLDOWN)
                .clamp(Duration::from_millis(200), MAX_COOLDOWN),
        ),
        // 429 that was not mapped to `RateLimited` is an exhausted quota.
        TranslateError::Rejected {
            status: 401 | 402 | 403 | 429,
            ..
        } => Fault::Rejected,
        TranslateError::Rejected { message, .. } if mentions_quota(message) => Fault::Rejected,
        _ => Fault::None,
    }
}

fn mentions_quota(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "insufficient_quota",
        "quota",
        "insufficient balance",
        "billing",
        "credit",
        "余额",
        "额度",
    ]
    .iter()
    .any(|n| lower.contains(n))
}

impl KeyPool {
    pub fn new(name: &str, fetch: KeyFn) -> Self {
        Self {
            name: name.to_owned(),
            fetch,
            loaded: tokio::sync::Mutex::new(None),
            state: Mutex::new(PoolState::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The keys, read from the key source on first use. A failure to read
    /// them is remembered, so a denied keychain prompt is not shown again for
    /// every paragraph. (A new translator is built when the keys change.)
    async fn keys(&self) -> Result<Arc<[String]>, TranslateError> {
        let mut loaded = self.loaded.lock().await;
        if let Some(result) = loaded.as_ref() {
            return result.clone();
        }
        let fetch = Arc::clone(&self.fetch);
        let fetched = tokio::task::spawn_blocking(move || fetch())
            .await
            .map_err(|e| TranslateError::Config(format!("reading the API key failed: {e}")))?;
        let name = &self.name;
        let result = match fetched {
            Ok(keys) => {
                let keys: Vec<String> = keys
                    .iter()
                    .map(|k| k.trim().to_owned())
                    .filter(|k| !k.is_empty())
                    .collect();
                if keys.is_empty() {
                    Err(TranslateError::Config(format!(
                        "No API key for “{name}”. Add one in Settings."
                    )))
                } else {
                    Ok(Arc::from(keys))
                }
            }
            Err(e) => Err(TranslateError::Config(format!(
                "Could not read the API key for “{name}”: {e}"
            ))),
        };
        *loaded = Some(result.clone());
        result
    }

    /// Pick the key for the next request.
    pub async fn acquire(&self) -> Result<Lease, TranslateError> {
        let keys = self.keys().await?;
        let count = keys.len();
        let mut st = self.state();
        if st.slots.len() != count {
            st.slots = vec![Slot::default(); count];
        }
        let now = Instant::now();
        let ready = |s: &Slot| s.rejected.is_none() && s.cool_until.is_none_or(|t| t <= now);
        let start = st.next % count;
        let index = (0..count)
            .map(|k| (start + k) % count)
            .find(|&i| ready(&st.slots[i]))
            // Nothing ready: the key whose cooldown ends first, then the key
            // rejected longest ago.
            .or_else(|| {
                (0..count)
                    .filter(|&i| st.slots[i].rejected.is_none())
                    .min_by_key(|&i| st.slots[i].cool_until)
            })
            .or_else(|| (0..count).min_by_key(|&i| st.slots[i].rejected.as_ref().map(|r| r.0)))
            .unwrap_or(0);
        st.next = index + 1;
        Ok(Lease {
            index,
            count,
            key: keys[index].clone(),
        })
    }

    /// Record a failed request. Returns `true` if the failure was the key's
    /// and another key is ready now, so the caller should retry with it.
    pub fn report(&self, lease: &Lease, err: &TranslateError) -> bool {
        let fault = fault(err);
        if fault == Fault::None {
            return false;
        }
        let mut st = self.state();
        let now = Instant::now();
        if let Some(slot) = st.slots.get_mut(lease.index) {
            match fault {
                Fault::Cooldown(d) => slot.cool_until = Some(now + d),
                Fault::Rejected => slot.rejected = Some((now, err.to_string())),
                Fault::None => {}
            }
        }
        st.slots.iter().enumerate().any(|(i, s)| {
            i != lease.index && s.rejected.is_none() && s.cool_until.is_none_or(|t| t <= now)
        })
    }

    /// A request with this key succeeded: it is usable again.
    pub fn succeeded(&self, lease: &Lease) {
        if let Some(slot) = self.state().slots.get_mut(lease.index) {
            slot.rejected = None;
            slot.cool_until = None;
        }
    }

    /// Per-key state, once the keys have been read (`None` before).
    pub fn status(&self) -> Option<Vec<KeyStatus>> {
        let keys = self
            .loaded
            .try_lock()
            .ok()?
            .as_ref()?
            .as_ref()
            .ok()?
            .clone();
        let st = self.state();
        let now = Instant::now();
        Some(
            keys.iter()
                .enumerate()
                .map(|(i, key)| {
                    let slot = st.slots.get(i).cloned().unwrap_or_default();
                    let (state, detail) = match (&slot.rejected, slot.cool_until) {
                        (Some((_, why)), _) => ("rejected", Some(why.clone())),
                        (None, Some(t)) if t > now => ("cooling", None),
                        _ => ("ready", None),
                    };
                    KeyStatus {
                        number: i + 1,
                        tail: tail(key),
                        state,
                        detail,
                    }
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(keys: &[&str]) -> KeyPool {
        let keys: Vec<String> = keys.iter().map(|k| (*k).to_owned()).collect();
        KeyPool::new("Relay", Arc::new(move || Ok(keys.clone())))
    }

    fn rate_limited() -> TranslateError {
        TranslateError::RateLimited { retry_after: None }
    }

    fn invalid_key() -> TranslateError {
        TranslateError::Rejected {
            status: 401,
            message: "invalid key".into(),
        }
    }

    #[tokio::test]
    async fn rotates_through_ready_keys() {
        let p = pool(&["k-aaaa", "k-bbbb", "k-cccc"]);
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(p.acquire().await.unwrap().key);
        }
        assert_eq!(seen, ["k-aaaa", "k-bbbb", "k-cccc", "k-aaaa"]);
    }

    #[tokio::test]
    async fn rate_limited_and_rejected_keys_are_skipped() {
        let p = pool(&["k-aaaa", "k-bbbb", "k-cccc"]);
        let a = p.acquire().await.unwrap();
        assert!(p.report(&a, &rate_limited()), "another key is ready");
        let b = p.acquire().await.unwrap();
        assert_eq!(b.key, "k-bbbb");
        assert!(p.report(&b, &invalid_key()));
        // Only c is ready now.
        assert_eq!(p.acquire().await.unwrap().key, "k-cccc");
        assert_eq!(p.acquire().await.unwrap().key, "k-cccc");
        let status = p.status().unwrap();
        assert_eq!(
            status.iter().map(|s| s.state).collect::<Vec<_>>(),
            ["cooling", "rejected", "ready"]
        );
        assert_eq!(status[1].tail, "bbbb");
        assert!(status[1].detail.as_deref().unwrap().contains("invalid key"));
    }

    #[tokio::test]
    async fn without_ready_keys_a_request_still_goes_out() {
        let p = pool(&["k-aaaa", "k-bbbb"]);
        let a = p.acquire().await.unwrap();
        assert!(p.report(&a, &invalid_key()));
        let b = p.acquire().await.unwrap();
        assert!(!p.report(&b, &rate_limited()), "no other key is ready");
        // b is cooling but not rejected, so it is preferred over a.
        assert_eq!(p.acquire().await.unwrap().key, "k-bbbb");
        // Both rejected: the one rejected longest ago comes back first.
        let b = p.acquire().await.unwrap();
        p.report(&b, &invalid_key());
        assert_eq!(p.acquire().await.unwrap().key, "k-aaaa");
    }

    #[tokio::test]
    async fn a_single_key_behaves_like_no_pool() {
        let p = pool(&["only-key"]);
        let k = p.acquire().await.unwrap();
        assert!(!p.report(&k, &invalid_key()));
        assert_eq!(p.acquire().await.unwrap().key, "only-key");
        let k = p.acquire().await.unwrap();
        assert!(!p.report(&k, &rate_limited()));
        assert_eq!(p.acquire().await.unwrap().key, "only-key");
    }

    #[tokio::test]
    async fn success_clears_a_rejection() {
        let p = pool(&["k-aaaa"]);
        let k = p.acquire().await.unwrap();
        p.report(&k, &invalid_key());
        p.succeeded(&k);
        assert_eq!(p.status().unwrap()[0].state, "ready");
    }

    #[test]
    fn faults() {
        assert_eq!(fault(&rate_limited()), Fault::Cooldown(DEFAULT_COOLDOWN));
        assert_eq!(
            fault(&TranslateError::RateLimited {
                retry_after: Some(Duration::from_secs(900))
            }),
            Fault::Cooldown(MAX_COOLDOWN)
        );
        assert_eq!(fault(&invalid_key()), Fault::Rejected);
        assert_eq!(
            fault(&TranslateError::Rejected {
                status: 400,
                message: "您的额度已用尽".into()
            }),
            Fault::Rejected
        );
        assert_eq!(
            fault(&TranslateError::Rejected {
                status: 400,
                message: "bad model".into()
            }),
            Fault::None
        );
        assert_eq!(
            fault(&TranslateError::Server {
                status: 500,
                retry_after: None
            }),
            Fault::None
        );
    }

    #[tokio::test]
    async fn empty_or_unreadable_keys_are_config_errors() {
        let empty = pool(&["  "]);
        assert!(matches!(
            empty.acquire().await,
            Err(TranslateError::Config(m)) if m.contains("No API key")
        ));
        let broken = KeyPool::new("Relay", Arc::new(|| Err("denied".into())));
        assert!(matches!(
            broken.acquire().await,
            Err(TranslateError::Config(m)) if m.contains("denied")
        ));
    }
}
