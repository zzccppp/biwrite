//! API key pool. A provider may hold several keys (for example accounts at a
//! relay), and requests spread over them.
//!
//! - A key may carry a limited number of requests at a time (relays often
//!   allow two). A request waits for a free slot when every key is full.
//! - Among keys with a free slot, the least busy ready key goes first.
//! - A key that hits a rate limit cools down, one that fails with a server
//!   or network error pauses briefly, and one the server rejects (invalid,
//!   no permission, out of quota) moves to the back.
//! - When no ready key has room, the pool still hands out the key most
//!   likely to work, so a real request goes out and the server decides.
//!   With one key and no limit this is a provider without a pool.

use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use biwrite_core::ContentHash;
use biwrite_engine::TranslateError;
use serde::Serialize;
use tokio::sync::Notify;

use crate::observe::KeyUse;

/// Fetches the provider's keys (from the OS keychain in the app), in pool
/// order. Called at most once per translator, lazily, on a blocking thread.
pub type KeyFn = Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;

/// Cooldown after a rate limit that came without a `Retry-After` hint.
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(5);
/// Longest cooldown, whatever the server asks for.
const MAX_COOLDOWN: Duration = Duration::from_secs(120);
/// Pause after a server or network error, so the next request tries
/// another key first.
const TRANSIENT_PAUSE: Duration = Duration::from_secs(2);

/// The last four characters of a key.
pub fn tail(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    chars[chars.len().saturating_sub(4)..].iter().collect()
}

/// A short stable identifier of a key that does not reveal it (12 hex
/// digits of its BLAKE3 hash). Key names in the settings refer to it.
pub fn fingerprint(key: &str) -> String {
    ContentHash::of_raw(key.trim().as_bytes()).to_hex()[..12].to_owned()
}

/// State of one key, for the settings view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub number: usize,
    pub fingerprint: String,
    pub tail: String,
    /// `ready`, `cooling` or `rejected`.
    pub state: &'static str,
    /// Requests in flight with this key.
    pub in_flight: u32,
    /// Why the key was set aside (the server's message, redacted).
    pub detail: Option<String>,
}

#[derive(Clone, Debug)]
struct Slot {
    cool_until: Option<Instant>,
    rejected: Option<(Instant, String)>,
    in_flight: u32,
    /// Routing salt for this key's `prompt_cache_key`. Relays route by that
    /// key, so it changes after a failure to leave a congested upstream.
    route: u64,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            cool_until: None,
            rejected: None,
            in_flight: 0,
            route: random_u64(),
        }
    }
}

/// A random number (std's per-instance hasher keys; no extra dependency).
pub(crate) fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
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
    /// Requests one key may carry at a time.
    limit: Option<u32>,
    loaded: tokio::sync::Mutex<Option<Result<Arc<[String]>, TranslateError>>>,
    state: Mutex<PoolState>,
    /// Signalled whenever a lease ends.
    freed: Notify,
}

/// A key handed out for one request. Dropping it frees the key's slot.
pub(crate) struct Lease<'a> {
    pool: &'a KeyPool,
    pub index: usize,
    pub count: usize,
    pub key: String,
    /// The key's routing salt when it was handed out.
    pub route: u64,
}

impl Lease<'_> {
    pub fn key_use(&self) -> KeyUse {
        KeyUse {
            number: self.index + 1,
            count: self.count,
            tail: tail(&self.key),
            fingerprint: fingerprint(&self.key),
        }
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(slot) = self.pool.state().slots.get_mut(self.index) {
            slot.in_flight = slot.in_flight.saturating_sub(1);
        }
        self.pool.freed.notify_waiters();
    }
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
        TranslateError::Server { .. } | TranslateError::Network(_) => {
            Fault::Cooldown(TRANSIENT_PAUSE)
        }
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
    pub fn new(name: &str, fetch: KeyFn, limit: Option<u32>) -> Self {
        Self {
            name: name.to_owned(),
            fetch,
            limit: limit.filter(|&n| n > 0),
            loaded: tokio::sync::Mutex::new(None),
            state: Mutex::new(PoolState::default()),
            freed: Notify::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, PoolState> {
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

    /// The key for the next request: waits while every key is at its limit.
    pub async fn acquire(&self) -> Result<Lease<'_>, TranslateError> {
        let keys = self.keys().await?;
        let count = keys.len();
        loop {
            // Registered before looking, so a slot freed meanwhile wakes us.
            let mut freed = pin!(self.freed.notified());
            freed.as_mut().enable();
            if let Some((index, route)) = self.pick(count) {
                return Ok(Lease {
                    pool: self,
                    index,
                    count,
                    key: keys[index].clone(),
                    route,
                });
            }
            freed.await;
        }
    }

    /// Choose a key with a free slot and take the slot. Returns the key's
    /// index and routing salt.
    fn pick(&self, count: usize) -> Option<(usize, u64)> {
        let mut st = self.state();
        if st.slots.len() != count {
            st.slots = vec![Slot::default(); count];
        }
        let now = Instant::now();
        let limit = self.limit;
        let has_room = |s: &Slot| limit.is_none_or(|l| s.in_flight < l);
        let ready = |s: &Slot| s.rejected.is_none() && s.cool_until.is_none_or(|t| t <= now);
        let start = st.next % count;
        let order: Vec<usize> = (0..count).map(|k| (start + k) % count).collect();
        let index = order
            .iter()
            .copied()
            .filter(|&i| has_room(&st.slots[i]) && ready(&st.slots[i]))
            .min_by_key(|&i| st.slots[i].in_flight)
            // Nothing ready: the key whose pause ends first, then the key
            // rejected longest ago.
            .or_else(|| {
                order
                    .iter()
                    .copied()
                    .filter(|&i| has_room(&st.slots[i]) && st.slots[i].rejected.is_none())
                    .min_by_key(|&i| st.slots[i].cool_until)
            })
            .or_else(|| {
                order
                    .iter()
                    .copied()
                    .filter(|&i| has_room(&st.slots[i]))
                    .min_by_key(|&i| st.slots[i].rejected.as_ref().map(|r| r.0))
            })?;
        st.slots[index].in_flight += 1;
        st.next = index + 1;
        Some((index, st.slots[index].route))
    }

    /// Record a failed request. Returns `true` if another key is ready and
    /// has room now, so the caller should retry with it at once.
    pub fn report(&self, lease: &Lease<'_>, err: &TranslateError) -> bool {
        let fault = fault(err);
        if fault == Fault::None {
            return false;
        }
        let mut st = self.state();
        let now = Instant::now();
        if let Some(slot) = st.slots.get_mut(lease.index) {
            match fault {
                Fault::Cooldown(d) => {
                    slot.cool_until = Some(slot.cool_until.map_or(now + d, |t| t.max(now + d)));
                }
                Fault::Rejected => slot.rejected = Some((now, err.to_string())),
                Fault::None => {}
            }
            slot.route = random_u64();
        }
        let limit = self.limit;
        st.slots.iter().enumerate().any(|(i, s)| {
            i != lease.index
                && s.rejected.is_none()
                && s.cool_until.is_none_or(|t| t <= now)
                && limit.is_none_or(|l| s.in_flight < l)
        })
    }

    /// A request with this key succeeded: it is usable again.
    pub fn succeeded(&self, lease: &Lease<'_>) {
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
                        fingerprint: fingerprint(key),
                        tail: tail(key),
                        state,
                        in_flight: slot.in_flight,
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

    fn pool(keys: &[&str], limit: Option<u32>) -> KeyPool {
        let keys: Vec<String> = keys.iter().map(|k| (*k).to_owned()).collect();
        KeyPool::new("Relay", Arc::new(move || Ok(keys.clone())), limit)
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
        let p = pool(&["k-aaaa", "k-bbbb", "k-cccc"], None);
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(p.acquire().await.unwrap().key.clone());
        }
        assert_eq!(seen, ["k-aaaa", "k-bbbb", "k-cccc", "k-aaaa"]);
    }

    #[tokio::test]
    async fn the_least_busy_key_goes_first() {
        let p = pool(&["k-aaaa", "k-bbbb"], None);
        let a1 = p.acquire().await.unwrap();
        let b1 = p.acquire().await.unwrap();
        let a2 = p.acquire().await.unwrap();
        assert_eq!((a1.index, b1.index, a2.index), (0, 1, 0));
        drop(b1);
        // b has no request in flight now, a has two.
        assert_eq!(p.acquire().await.unwrap().index, 1);
    }

    #[tokio::test]
    async fn requests_wait_for_a_free_slot() {
        let p = Arc::new(pool(&["k-aaaa", "k-bbbb"], Some(2)));
        let held: Vec<Lease<'_>> = vec![
            p.acquire().await.unwrap(),
            p.acquire().await.unwrap(),
            p.acquire().await.unwrap(),
            p.acquire().await.unwrap(),
        ];
        let status = p.status().unwrap();
        assert_eq!(
            status.iter().map(|s| s.in_flight).collect::<Vec<_>>(),
            [2, 2]
        );
        // A fifth request has to wait until one of the four ends.
        let waiting = tokio::time::timeout(Duration::from_millis(100), p.acquire()).await;
        assert!(waiting.is_err(), "every key is at its limit");
        let freed_index = held[1].index;
        let mut held = held;
        held.remove(1);
        let next = tokio::time::timeout(Duration::from_millis(500), p.acquire())
            .await
            .expect("a slot was freed")
            .unwrap();
        assert_eq!(next.index, freed_index);
    }

    #[tokio::test]
    async fn rate_limited_and_rejected_keys_are_skipped() {
        let p = pool(&["k-aaaa", "k-bbbb", "k-cccc"], None);
        let a = p.acquire().await.unwrap();
        assert!(p.report(&a, &rate_limited()), "another key is ready");
        drop(a);
        let b = p.acquire().await.unwrap();
        assert_eq!(b.key, "k-bbbb");
        assert!(p.report(&b, &invalid_key()));
        drop(b);
        // Only c is ready now.
        assert_eq!(p.acquire().await.unwrap().key, "k-cccc");
        assert_eq!(p.acquire().await.unwrap().key, "k-cccc");
        let status = p.status().unwrap();
        assert_eq!(
            status.iter().map(|s| s.state).collect::<Vec<_>>(),
            ["cooling", "rejected", "ready"]
        );
        assert_eq!(status[1].tail, "bbbb");
        assert_eq!(status[1].fingerprint, fingerprint("k-bbbb"));
        assert!(status[1].detail.as_deref().unwrap().contains("invalid key"));
    }

    #[tokio::test]
    async fn server_errors_hand_over_to_another_key() {
        let p = pool(&["k-aaaa", "k-bbbb"], None);
        let a = p.acquire().await.unwrap();
        let busy = TranslateError::Server {
            status: 500,
            retry_after: None,
        };
        assert!(p.report(&a, &busy));
        drop(a);
        assert_eq!(p.acquire().await.unwrap().key, "k-bbbb");
        assert_eq!(p.status().unwrap()[0].state, "cooling");
    }

    #[tokio::test]
    async fn without_ready_keys_a_request_still_goes_out() {
        let p = pool(&["k-aaaa", "k-bbbb"], None);
        let a = p.acquire().await.unwrap();
        assert!(p.report(&a, &invalid_key()));
        drop(a);
        let b = p.acquire().await.unwrap();
        assert!(!p.report(&b, &rate_limited()), "no other key is ready");
        drop(b);
        // b is cooling but not rejected, so it is preferred over a.
        assert_eq!(p.acquire().await.unwrap().key, "k-bbbb");
        // Both rejected: the one rejected longest ago comes back first.
        let b = p.acquire().await.unwrap();
        p.report(&b, &invalid_key());
        drop(b);
        assert_eq!(p.acquire().await.unwrap().key, "k-aaaa");
    }

    #[tokio::test]
    async fn a_single_key_behaves_like_no_pool() {
        let p = pool(&["only-key"], None);
        let k = p.acquire().await.unwrap();
        assert!(!p.report(&k, &invalid_key()));
        drop(k);
        assert_eq!(p.acquire().await.unwrap().key, "only-key");
        let k = p.acquire().await.unwrap();
        assert!(!p.report(&k, &rate_limited()));
        drop(k);
        assert_eq!(p.acquire().await.unwrap().key, "only-key");
    }

    #[tokio::test]
    async fn a_failure_moves_the_key_to_another_route() {
        let p = pool(&["k-aaaa"], None);
        let first = p.acquire().await.unwrap();
        let route = first.route;
        drop(first);
        let again = p.acquire().await.unwrap();
        assert_eq!(again.route, route, "a healthy key keeps its route");
        p.report(&again, &rate_limited());
        drop(again);
        assert_ne!(p.acquire().await.unwrap().route, route);
    }

    #[tokio::test]
    async fn success_clears_a_rejection() {
        let p = pool(&["k-aaaa"], None);
        let k = p.acquire().await.unwrap();
        p.report(&k, &invalid_key());
        p.succeeded(&k);
        drop(k);
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
        assert_eq!(
            fault(&TranslateError::Network("reset".into())),
            Fault::Cooldown(TRANSIENT_PAUSE)
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
    }

    #[test]
    fn fingerprints_are_short_stable_and_not_the_key() {
        let f = fingerprint("sk-test-0123456789");
        assert_eq!(f.len(), 12);
        assert_eq!(f, fingerprint(" sk-test-0123456789 "));
        assert_ne!(f, fingerprint("sk-test-0123456780"));
        assert!(!"sk-test-0123456789".contains(&f));
    }

    #[tokio::test]
    async fn empty_or_unreadable_keys_are_config_errors() {
        let empty = pool(&["  "], None);
        assert!(matches!(
            empty.acquire().await,
            Err(TranslateError::Config(m)) if m.contains("No API key")
        ));
        let broken = KeyPool::new("Relay", Arc::new(|| Err("denied".into())), None);
        assert!(matches!(
            broken.acquire().await,
            Err(TranslateError::Config(m)) if m.contains("denied")
        ));
    }
}
