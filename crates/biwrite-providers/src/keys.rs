//! API key pool. A provider may hold several keys (for example accounts at a
//! relay), and requests spread over them.
//!
//! - A key may carry a limited number of requests at a time (relays often
//!   allow two). A request waits for a free slot when every key is full.
//! - Among keys with a free slot, the least busy ready key goes first.
//! - A key that hits a rate limit cools down, one that fails with a server
//!   or network error pauses briefly, and one the server rejects (invalid,
//!   no permission, out of quota) moves to the back.
//! - While some key is ready, requests go to ready keys only: when those are
//!   full, a request waits for a free slot, or for a paused key's pause to
//!   end, rather than going to a paused or rejected key.
//! - When no key is ready, the pool still hands out the key most likely to
//!   work, so a real request goes out and the server decides. With one key
//!   and no limit this is a provider without a pool.
//! - The keys can be read again ([`KeyPool::reload`]) while requests are in
//!   flight: a key that stays keeps its state and its requests in flight.

use std::pin::pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use biwrite_core::ContentHash;
use biwrite_engine::TranslateError;
use serde::Serialize;
use tokio::sync::Notify;

use crate::observe::KeyUse;

/// Fetches the provider's keys (from the OS keychain in the app), in pool
/// order. Called lazily on a blocking thread: once per translator, and again
/// after [`KeyPool::reload`].
pub type KeyFn = Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;

/// Cooldown after a rate limit that came without a `Retry-After` hint.
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(5);
/// Longest cooldown, whatever the server asks for.
const MAX_COOLDOWN: Duration = Duration::from_secs(120);
/// Pause after a server or network error, so the next request tries
/// another key first.
const TRANSIENT_PAUSE: Duration = Duration::from_secs(2);
/// A rejected key is tried again after this long (an account topped up, a
/// rejection that was not about the key after all).
const REJECTED_RETRY: Duration = Duration::from_secs(600);

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
    /// When the request that set the latest cooldown was sent.
    cooled_at: Option<Instant>,
    /// When the request the key was rejected for was sent, and why.
    rejected: Option<(Instant, String)>,
    /// When the latest request that succeeded with this key was sent: a
    /// failure of a request sent before it is old news.
    ok_at: Option<Instant>,
    in_flight: u32,
    /// Routing salt for this key's `prompt_cache_key`. Relays route by that
    /// key, so it changes after a failure to leave a congested upstream.
    route: u64,
}

impl Slot {
    /// Not paused, and not rejected (or rejected long enough ago to try
    /// again).
    fn ready(&self, now: Instant) -> bool {
        self.rejected
            .as_ref()
            .is_none_or(|(at, _)| now.saturating_duration_since(*at) >= REJECTED_RETRY)
            && self.cool_until.is_none_or(|t| t <= now)
    }
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            cool_until: None,
            cooled_at: None,
            rejected: None,
            ok_at: None,
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

#[derive(Debug)]
struct PoolState {
    next: usize,
    /// The keys `slots` describe, in pool order, and the reading
    /// ([`KeyPool::epoch`]) they come from.
    keys: Arc<[String]>,
    epoch: u64,
    slots: Vec<Slot>,
}

impl Default for PoolState {
    fn default() -> Self {
        Self {
            next: 0,
            keys: Arc::from(Vec::new()),
            epoch: 0,
            slots: Vec::new(),
        }
    }
}

impl PoolState {
    fn slot(&self, key: &str) -> Option<&Slot> {
        let i = self.keys.iter().position(|k| k == key)?;
        self.slots.get(i)
    }

    fn slot_mut(&mut self, key: &str) -> Option<&mut Slot> {
        let i = self.keys.iter().position(|k| k == key)?;
        self.slots.get_mut(i)
    }

    /// Describe `keys`, read under `epoch`. A key that was already in the
    /// pool keeps its state, including its requests in flight.
    fn adopt(&mut self, epoch: u64, keys: &Arc<[String]>) {
        let slots = keys
            .iter()
            .map(|k| self.slot(k).cloned().unwrap_or_default())
            .collect();
        self.slots = slots;
        self.keys = Arc::clone(keys);
        self.epoch = epoch;
    }
}

/// The keys as last read, with the reading they belong to.
type Loaded = Option<(u64, Result<Arc<[String]>, TranslateError>)>;

pub(crate) struct KeyPool {
    /// Provider name, for messages.
    name: String,
    /// Where the keys come from; [`reload`](Self::reload) may replace it.
    fetch: Mutex<KeyFn>,
    /// Requests one key may carry at a time.
    limit: Option<u32>,
    /// Counts [`reload`](Self::reload)s: keys read before the latest one are
    /// read again.
    epoch: AtomicU64,
    loaded: tokio::sync::Mutex<Loaded>,
    state: Mutex<PoolState>,
    /// Signalled whenever a lease ends or the keys change.
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
    /// When it was handed out.
    started: Instant,
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
        if let Some(slot) = self.pool.state().slot_mut(&self.key) {
            slot.in_flight = slot.in_flight.saturating_sub(1);
        }
        self.pool.freed.notify_waiters();
    }
}

/// How a failed request reflects on the key it used.
#[derive(Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// A pause, and whether it is the key's doing (a rate limit) rather
    /// than the server's or the network's.
    Cooldown(Duration, bool),
    Rejected,
}

fn fault(err: &TranslateError) -> Fault {
    match err {
        TranslateError::RateLimited { retry_after } => Fault::Cooldown(
            retry_after
                .unwrap_or(DEFAULT_COOLDOWN)
                .clamp(Duration::from_millis(200), MAX_COOLDOWN),
            true,
        ),
        TranslateError::Server { .. } | TranslateError::Network(_) => {
            Fault::Cooldown(TRANSIENT_PAUSE, false)
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

/// What a failed request means for the next attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failover {
    /// No other key would do better now.
    No,
    /// Another key is ready and has room.
    Now,
    /// The keys were read again since the request started: try the new ones.
    KeysChanged,
}

/// What [`KeyPool::pick`] found.
enum Pick {
    /// The key's index and routing salt; its slot is taken.
    Key(usize, u64),
    /// No key to use now: wait for a lease to end, or until the instant
    /// when a paused key with room becomes ready.
    Wait(Option<Instant>),
    /// The keys were read again meanwhile: look again with the new ones.
    Stale,
}

impl KeyPool {
    pub fn new(name: &str, fetch: KeyFn, limit: Option<u32>) -> Self {
        Self {
            name: name.to_owned(),
            fetch: Mutex::new(fetch),
            limit: limit.filter(|&n| n > 0),
            epoch: AtomicU64::new(0),
            loaded: tokio::sync::Mutex::new(None),
            state: Mutex::new(PoolState::default()),
            freed: Notify::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Read the keys again before the next request, from `fetch` (the keys
    /// changed). Requests in flight go on with their keys; a key that stays
    /// keeps its state, so per-key limits still count its requests.
    pub fn reload(&self, fetch: KeyFn) {
        *self.fetch.lock().unwrap_or_else(PoisonError::into_inner) = fetch;
        self.epoch.fetch_add(1, Ordering::AcqRel);
        // Waiting requests look at the new keys.
        self.freed.notify_waiters();
    }

    /// The keys, read from the key source on first use and after a reload,
    /// with the reading they belong to. A failure to read them is
    /// remembered, so a denied keychain prompt is not shown again for every
    /// paragraph.
    async fn keys(&self) -> Result<(u64, Arc<[String]>), TranslateError> {
        let mut loaded = self.loaded.lock().await;
        let epoch = self.epoch.load(Ordering::Acquire);
        if let Some((at, result)) = loaded.as_ref()
            && *at == epoch
        {
            return result.clone().map(|keys| (epoch, keys));
        }
        let fetch = Arc::clone(&self.fetch.lock().unwrap_or_else(PoisonError::into_inner));
        let fetched = tokio::task::spawn_blocking(move || fetch())
            .await
            .map_err(|e| TranslateError::Config(format!("reading the API key failed: {e}")))?;
        let name = &self.name;
        let result = match fetched {
            Ok(fetched) => {
                let mut keys: Vec<String> = Vec::new();
                for key in fetched.iter().map(|k| k.trim()).filter(|k| !k.is_empty()) {
                    if !keys.iter().any(|k| k == key) {
                        keys.push(key.to_owned());
                    }
                }
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
        *loaded = Some((epoch, result.clone()));
        result.map(|keys| (epoch, keys))
    }

    /// The key for the next request: waits while every usable key is at its
    /// limit.
    pub async fn acquire(&self) -> Result<Lease<'_>, TranslateError> {
        loop {
            let (epoch, keys) = self.keys().await?;
            // Registered before looking, so a slot freed meanwhile wakes us.
            let mut freed = pin!(self.freed.notified());
            freed.as_mut().enable();
            match self.pick(epoch, &keys) {
                Pick::Key(index, route) => {
                    return Ok(Lease {
                        pool: self,
                        index,
                        count: keys.len(),
                        key: keys[index].clone(),
                        route,
                        started: Instant::now(),
                    });
                }
                Pick::Stale => {}
                Pick::Wait(None) => freed.await,
                Pick::Wait(Some(until)) => {
                    let until = tokio::time::Instant::from_std(until);
                    let _ = tokio::time::timeout_at(until, freed).await;
                }
            }
        }
    }

    /// Choose a key with a free slot and take the slot.
    fn pick(&self, epoch: u64, keys: &Arc<[String]>) -> Pick {
        let mut st = self.state();
        if epoch < st.epoch {
            return Pick::Stale;
        }
        if epoch > st.epoch || st.slots.len() != keys.len() {
            st.adopt(epoch, keys);
        }
        let count = keys.len();
        let now = Instant::now();
        let limit = self.limit;
        let has_room = |s: &Slot| limit.is_none_or(|l| s.in_flight < l);
        let ready = |s: &Slot| s.ready(now);
        let start = st.next % count;
        let order: Vec<usize> = (0..count).map(|k| (start + k) % count).collect();
        let index = if st.slots.iter().any(ready) {
            // Full ready keys are waited for: a paused or rejected key
            // would most likely fail.
            order
                .iter()
                .copied()
                .filter(|&i| has_room(&st.slots[i]) && ready(&st.slots[i]))
                .min_by_key(|&i| st.slots[i].in_flight)
        } else {
            // Nothing ready: the key whose pause ends first, then the key
            // rejected longest ago.
            order
                .iter()
                .copied()
                .filter(|&i| has_room(&st.slots[i]) && st.slots[i].rejected.is_none())
                .min_by_key(|&i| st.slots[i].cool_until)
                .or_else(|| {
                    order
                        .iter()
                        .copied()
                        .filter(|&i| has_room(&st.slots[i]))
                        .min_by_key(|&i| st.slots[i].rejected.as_ref().map(|r| r.0))
                })
        };
        let Some(index) = index else {
            // A paused key with room becomes ready when its pause ends.
            let until = st
                .slots
                .iter()
                .filter(|s| s.rejected.is_none() && has_room(s))
                .filter_map(|s| s.cool_until)
                .filter(|&t| t > now)
                .min();
            return Pick::Wait(until);
        };
        st.slots[index].in_flight += 1;
        st.next = index + 1;
        Pick::Key(index, st.slots[index].route)
    }

    /// Record a failed request, and say whether the caller may retry with
    /// another key at once.
    pub fn report(&self, lease: &Lease<'_>, err: &TranslateError) -> Failover {
        let fault = fault(err);
        if fault == Fault::None {
            return Failover::No;
        }
        let mut st = self.state();
        let now = Instant::now();
        if let Some(slot) = st
            .slot_mut(&lease.key)
            .filter(|slot| slot.ok_at.is_none_or(|ok| ok <= lease.started))
        {
            match fault {
                // A fault is dated by when its request was sent, the latest
                // such request counting: a request sent later that succeeds
                // shows the key works again.
                Fault::Cooldown(d, keys_doing) => {
                    let running = slot.cool_until.is_some_and(|t| t > now);
                    let extends = slot.cool_until.is_none_or(|t| t < now + d);
                    if extends {
                        slot.cool_until = Some(now + d);
                    }
                    if !running {
                        slot.cooled_at = Some(lease.started);
                    } else if extends || keys_doing {
                        // A short server pause inside a rate limit leaves
                        // its date.
                        slot.cooled_at = slot.cooled_at.max(Some(lease.started));
                    }
                }
                Fault::Rejected => {
                    if slot
                        .rejected
                        .as_ref()
                        .is_none_or(|(at, _)| *at <= lease.started)
                    {
                        slot.rejected = Some((lease.started, err.to_string()));
                    }
                }
                Fault::None => {}
            }
            slot.route = random_u64();
        }
        // Keys added since the pool last looked are not in `st` yet.
        if self.epoch.load(Ordering::Acquire) != st.epoch {
            return Failover::KeysChanged;
        }
        let limit = self.limit;
        let other = st.keys.iter().zip(&st.slots).any(|(key, s)| {
            *key != lease.key && s.ready(now) && limit.is_none_or(|l| s.in_flight < l)
        });
        if other { Failover::Now } else { Failover::No }
    }

    /// A request with this key succeeded: a failure of a request sent before
    /// it no longer holds. A failure of a request sent after it is newer
    /// news, and stays.
    pub fn succeeded(&self, lease: &Lease<'_>) {
        if let Some(slot) = self.state().slot_mut(&lease.key) {
            slot.ok_at = slot.ok_at.max(Some(lease.started));
            if slot
                .rejected
                .as_ref()
                .is_some_and(|(at, _)| *at <= lease.started)
            {
                slot.rejected = None;
            }
            if slot.cooled_at.is_none_or(|at| at <= lease.started) {
                slot.cool_until = None;
                slot.cooled_at = None;
            }
        }
    }

    /// Per-key state, once the keys have been read (`None` before).
    pub fn status(&self) -> Option<Vec<KeyStatus>> {
        let keys = self
            .loaded
            .try_lock()
            .ok()?
            .as_ref()?
            .1
            .as_ref()
            .ok()?
            .clone();
        let st = self.state();
        let now = Instant::now();
        Some(
            keys.iter()
                .enumerate()
                .map(|(i, key)| {
                    let slot = st.slot(key).cloned().unwrap_or_default();
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
        assert_eq!(
            p.report(&a, &rate_limited()),
            Failover::Now,
            "another key is ready"
        );
        drop(a);
        let b = p.acquire().await.unwrap();
        assert_eq!(b.key, "k-bbbb");
        assert_eq!(p.report(&b, &invalid_key()), Failover::Now);
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
        assert_eq!(p.report(&a, &busy), Failover::Now);
        drop(a);
        assert_eq!(p.acquire().await.unwrap().key, "k-bbbb");
        assert_eq!(p.status().unwrap()[0].state, "cooling");
    }

    #[tokio::test]
    async fn without_ready_keys_a_request_still_goes_out() {
        let p = pool(&["k-aaaa", "k-bbbb"], None);
        let a = p.acquire().await.unwrap();
        assert_eq!(p.report(&a, &invalid_key()), Failover::Now);
        drop(a);
        let b = p.acquire().await.unwrap();
        assert_eq!(
            p.report(&b, &rate_limited()),
            Failover::No,
            "no other key is ready"
        );
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
        assert_eq!(p.report(&k, &invalid_key()), Failover::No);
        drop(k);
        assert_eq!(p.acquire().await.unwrap().key, "only-key");
        let k = p.acquire().await.unwrap();
        assert_eq!(p.report(&k, &rate_limited()), Failover::No);
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
        drop(k);
        std::thread::sleep(Duration::from_millis(2));
        let again = p.acquire().await.unwrap();
        p.succeeded(&again);
        drop(again);
        assert_eq!(p.status().unwrap()[0].state, "ready");
    }

    #[tokio::test]
    async fn a_failure_newer_than_a_success_stays() {
        let p = pool(&["k-aaaa", "k-bbbb"], None);
        let slow = p.acquire().await.unwrap();
        let other = p.acquire().await.unwrap();
        std::thread::sleep(Duration::from_millis(2));
        // A later request with the same key is rate limited and rejected
        // while the first one is still under way...
        let later = p.acquire().await.unwrap();
        assert_eq!(later.key, slow.key);
        p.report(&later, &rate_limited());
        drop(later);
        // ...so the first one's success says nothing about now.
        p.succeeded(&slow);
        assert_eq!(p.status().unwrap()[0].state, "cooling");
        let later = p.acquire().await.unwrap();
        assert_eq!(later.key, "k-bbbb");
        p.report(&later, &invalid_key());
        p.succeeded(&other);
        assert_eq!(p.status().unwrap()[1].state, "rejected");
    }

    #[tokio::test]
    async fn full_ready_keys_are_waited_for_rather_than_rejected_ones() {
        let p = pool(&["k-aaaa", "k-bbbb"], Some(1));
        let b = {
            let a = p.acquire().await.unwrap();
            let b = p.acquire().await.unwrap();
            assert_eq!(p.report(&b, &invalid_key()), Failover::No, "a is full");
            drop(b);
            a
        };
        // a is ready but full, b is rejected: wait for a.
        let waiting = tokio::time::timeout(Duration::from_millis(100), p.acquire()).await;
        assert!(waiting.is_err(), "the rejected key was handed out");
        drop(b);
        let next = tokio::time::timeout(Duration::from_millis(500), p.acquire())
            .await
            .expect("the ready key was freed")
            .unwrap();
        assert_eq!(next.key, "k-aaaa");
    }

    #[tokio::test]
    async fn a_paused_key_is_used_once_its_pause_ends() {
        let p = pool(&["k-aaaa", "k-bbbb"], Some(1));
        let a = p.acquire().await.unwrap();
        let b = p.acquire().await.unwrap();
        p.report(
            &b,
            &TranslateError::RateLimited {
                retry_after: Some(Duration::from_millis(300)),
            },
        );
        drop(b);
        let waiting = tokio::time::timeout(Duration::from_millis(100), p.acquire()).await;
        assert!(waiting.is_err(), "the cooling key was handed out");
        let next = tokio::time::timeout(Duration::from_millis(1500), p.acquire())
            .await
            .expect("b is ready again")
            .unwrap();
        assert_eq!(next.key, "k-bbbb");
        drop(a);
    }

    #[tokio::test]
    async fn reloaded_keys_keep_the_state_of_keys_that_stay() {
        let p = pool(&["k-aaaa", "k-bbbb"], Some(1));
        let a = p.acquire().await.unwrap();
        let b = p.acquire().await.unwrap();
        p.report(&b, &invalid_key());
        drop(b);
        p.reload(Arc::new(|| {
            Ok(vec!["k-cccc".into(), "k-aaaa".into(), "k-bbbb".into()])
        }));
        // a is still full and b still rejected: c goes first.
        let c = p.acquire().await.unwrap();
        assert_eq!((c.key.as_str(), c.index, c.count), ("k-cccc", 0, 3));
        let status = p.status().unwrap();
        assert_eq!(
            status
                .iter()
                .map(|s| (s.tail.as_str(), s.state, s.in_flight))
                .collect::<Vec<_>>(),
            [
                ("cccc", "ready", 1),
                ("aaaa", "ready", 1),
                ("bbbb", "rejected", 0)
            ]
        );
        // The lease taken before the reload frees its key.
        drop(a);
        assert_eq!(p.status().unwrap()[1].in_flight, 0);
        assert_eq!(p.acquire().await.unwrap().key, "k-aaaa");
    }

    #[tokio::test]
    async fn a_reload_wakes_waiting_requests_and_reads_again() {
        let p = Arc::new(KeyPool::new(
            "Relay",
            Arc::new(|| Err("denied".into())),
            Some(1),
        ));
        assert!(p.acquire().await.is_err());
        p.reload(Arc::new(|| Ok(vec!["k-aaaa".into()])));
        let a = p.acquire().await.unwrap();
        let waiter = {
            let p = Arc::clone(&p);
            tokio::spawn(async move { p.acquire().await.map(|l| l.key.clone()) })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        p.reload(Arc::new(|| Ok(vec!["k-aaaa".into(), "k-bbbb".into()])));
        let got = tokio::time::timeout(Duration::from_millis(500), waiter)
            .await
            .expect("the waiting request saw the new key")
            .unwrap()
            .unwrap();
        assert_eq!(got, "k-bbbb");
        drop(a);
    }

    #[test]
    fn faults() {
        assert_eq!(
            fault(&rate_limited()),
            Fault::Cooldown(DEFAULT_COOLDOWN, true)
        );
        assert_eq!(
            fault(&TranslateError::RateLimited {
                retry_after: Some(Duration::from_secs(900))
            }),
            Fault::Cooldown(MAX_COOLDOWN, true)
        );
        assert_eq!(
            fault(&TranslateError::Network("reset".into())),
            Fault::Cooldown(TRANSIENT_PAUSE, false)
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

    #[tokio::test]
    async fn a_failure_after_keys_were_added_tries_the_new_ones() {
        let p = pool(&["k-aaaa"], None);
        let a = p.acquire().await.unwrap();
        p.reload(Arc::new(|| Ok(vec!["k-aaaa".into(), "k-bbbb".into()])));
        assert_eq!(p.report(&a, &invalid_key()), Failover::KeysChanged);
        drop(a);
        assert_eq!(p.acquire().await.unwrap().key, "k-bbbb");
        // A failure that is not the key's still is no reason to switch.
        let p = pool(&["k-aaaa"], None);
        let a = p.acquire().await.unwrap();
        p.reload(Arc::new(|| Ok(vec!["k-bbbb".into()])));
        let bad_request = TranslateError::Rejected {
            status: 400,
            message: "bad model".into(),
        };
        assert_eq!(p.report(&a, &bad_request), Failover::No);
    }

    #[tokio::test]
    async fn a_short_pause_does_not_keep_a_long_cooldown_alive() {
        let p = pool(&["k-aaaa"], None);
        let first = p.acquire().await.unwrap();
        p.report(
            &first,
            &TranslateError::RateLimited {
                retry_after: Some(Duration::from_secs(60)),
            },
        );
        std::thread::sleep(Duration::from_millis(2));
        // Sent after the rate limit (no other key)...
        let second = p.acquire().await.unwrap();
        std::thread::sleep(Duration::from_millis(2));
        // ...then a brief server error inside the long cooldown...
        let busy = TranslateError::Server {
            status: 502,
            retry_after: None,
        };
        p.report(&first, &busy);
        drop(first);
        // ...and the request sent after the rate limit succeeds: the key
        // works again.
        p.succeeded(&second);
        assert_eq!(p.status().unwrap()[0].state, "ready");
    }

    #[tokio::test]
    async fn a_rejected_key_is_tried_again_after_a_while() {
        let p = pool(&["k-aaaa", "k-bbbb"], None);
        let a = p.acquire().await.unwrap();
        p.report(&a, &invalid_key());
        drop(a);
        assert_eq!(p.acquire().await.unwrap().key, "k-bbbb");
        assert_eq!(p.acquire().await.unwrap().key, "k-bbbb");
        let long_ago = Instant::now()
            .checked_sub(REJECTED_RETRY + Duration::from_secs(1))
            .unwrap();
        p.state().slots[0].rejected = Some((long_ago, "invalid key".into()));
        let mut keys = Vec::new();
        for _ in 0..2 {
            keys.push(p.acquire().await.unwrap().key.clone());
        }
        assert!(keys.contains(&"k-aaaa".to_owned()), "{keys:?}");
    }

    #[tokio::test]
    async fn a_later_request_that_succeeds_clears_an_earlier_ones_failure() {
        let p = pool(&["k-aaaa"], None);
        let early = p.acquire().await.unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let later = p.acquire().await.unwrap();
        std::thread::sleep(Duration::from_millis(2));
        // The early request comes back rate limited after the later one
        // was sent...
        p.report(
            &early,
            &TranslateError::RateLimited {
                retry_after: Some(Duration::from_secs(120)),
            },
        );
        drop(early);
        // ...and the later one succeeds: the key works.
        p.succeeded(&later);
        assert_eq!(p.status().unwrap()[0].state, "ready");
    }

    #[tokio::test]
    async fn fault_dates_never_move_back() {
        let p = pool(&["k-aaaa"], None);
        let x = p.acquire().await.unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let z = p.acquire().await.unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let y = p.acquire().await.unwrap();
        // y (sent last) is rejected first, then x (sent first)...
        p.report(&y, &invalid_key());
        p.report(&x, &invalid_key());
        // ...so z, sent between them, proves nothing about y's rejection.
        p.succeeded(&z);
        assert_eq!(p.status().unwrap()[0].state, "rejected");
        // The same for rate limits: an older request's longer cooldown keeps
        // the newer request's date.
        let p = pool(&["k-aaaa"], None);
        let x = p.acquire().await.unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let z = p.acquire().await.unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let y = p.acquire().await.unwrap();
        p.report(&y, &rate_limited());
        p.report(
            &x,
            &TranslateError::RateLimited {
                retry_after: Some(Duration::from_secs(60)),
            },
        );
        p.succeeded(&z);
        assert_eq!(p.status().unwrap()[0].state, "cooling");
    }

    #[tokio::test]
    async fn a_failure_reported_after_a_later_requests_success_is_old_news() {
        let p = pool(&["k-aaaa"], Some(2));
        let long = p.acquire().await.unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let short = p.acquire().await.unwrap();
        // The short request, sent second, succeeds first...
        p.succeeded(&short);
        drop(short);
        // ...then the long one, sent first, comes back out of quota.
        let quota = TranslateError::Rejected {
            status: 403,
            message: "insufficient credit".into(),
        };
        p.report(&long, &quota);
        drop(long);
        assert_eq!(p.status().unwrap()[0].state, "ready");
    }
}
