//! Getting through to a crowded relay.
//!
//! A relay such as AnyRouter, when its upstream is full, answers the keys
//! that got through a while ago and turns the others away with a server
//! error, whatever else a request carries. A [`SqueezeBoard`] records which
//! keys of a provider got through and when. It outlives the providers built
//! for it, so switching to another provider and back keeps it. While the
//! relay keeps turning keys away, the key pool sends requests to the keys
//! that got through first.
//!
//! A [`Squeeze`] tries every key that has not got through, a few seconds
//! apart, with a short translation, until all have (or it is stopped). A
//! request turned away costs nothing, and one that gets through is answered.
//! A key that got through loses its place when left idle for some minutes:
//! [`ping`] sends it a short translation, which keeps its place.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use biwrite_engine::TranslateError;
use serde::Serialize;
use tokio::sync::watch;

use crate::config::{Effort, ProviderConfig};
use crate::keys::fingerprint;
use crate::observe::{NoObserver, RequestObserver};
use crate::prompt::PromptSource;
use crate::provider::HttpProvider;

/// A key that got through stops counting as through after this many server
/// errors in a row. One that got through answers most requests and fails
/// now and then, never many times in a row.
pub const DROP_AFTER: u32 = 6;
/// A key that got through and has not been answered for this long may have
/// lost its place: its next server error drops it, and a squeeze tries it
/// again. The relay is taken as crowded for this long after it turned keys
/// away.
pub const STALE_AFTER: Duration = Duration::from_secs(5 * 60);
/// Keys turned away within [`STALE_AFTER`] before the relay counts as
/// crowded. One stray server error does not change how a pool takes turns.
pub const CROWDED_AFTER: usize = 2;
/// A key kept alive is sent a short translation after this long without an
/// answer. Measured at AnyRouter, a key idle for four minutes kept its
/// place and one idle for ten minutes had lost it. Three minutes leave time
/// for a few retries before [`STALE_AFTER`].
pub const KEEP_ALIVE_AFTER: Duration = Duration::from_secs(3 * 60);
/// Keys are kept alive, and squeezed back in, only this long after the
/// author last used their provider: someone away for longer pays for no
/// requests.
pub const KEEP_ALIVE_FOR: Duration = Duration::from_secs(30 * 60);
/// Default number of consecutive failed keep-alive requests before a key's
/// background requests stop. Foreground requests remain available.
pub const DEFAULT_KEEP_ALIVE_FAILURE_LIMIT: u32 = 20;
/// Pause between two tries of one key.
const PROBE_GAP: Duration = Duration::from_secs(3);
/// Longest pause between two tries of a squeeze nobody watches, which
/// doubles the pause after every try turned away.
const UNATTENDED_GAP_MAX: Duration = Duration::from_secs(60);
/// The author used the keys this lately: a squeeze nobody started tries as
/// often as one they started, since they are waiting for it.
const ATTENDED_WITHIN: Duration = Duration::from_secs(2 * 60);
/// Longest wait for one try, and for a rate limit between tries.
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
struct Through {
    since: SystemTime,
    last_ok: Instant,
    last_ok_wall: SystemTime,
    /// Server errors since the last answer.
    fails: u32,
}

#[derive(Debug, Default)]
struct BackgroundKey {
    generation: u64,
    failures: u32,
    stopped: Option<String>,
    target_met_at_stop: bool,
    rejoinable_stop: bool,
}

/// The keys of one provider that got through, by fingerprint.
#[derive(Debug, Default)]
pub struct SqueezeBoard {
    keys: Mutex<HashMap<String, Through>>,
    /// When the server turned keys away lately (at most [`CROWDED_AFTER`]
    /// within [`STALE_AFTER`] are kept).
    turned_away: Mutex<VecDeque<Instant>>,
    /// Keys that got through go first while the relay is crowded. Off for a
    /// pool that is not a crowded relay until a squeeze is started for it,
    /// so an ordinary pool takes turns as without a board.
    crowding: AtomicBool,
    /// The relay turned keys away as often as [`Self::crowded`] asks, at
    /// some time since crowding applies, or the author started a squeeze.
    crowded_seen: AtomicBool,
    /// When a request of the author's last went out with one of the keys
    /// (a translation, the writing assistant, a key test) or a squeeze was
    /// started; pings and the tries of a squeeze do not count.
    used: Mutex<Option<Instant>>,
    /// Keys a squeeze gave up on (refused, or a request that cannot
    /// succeed), until answered: not squeezed back in by themselves. With
    /// what the server said to the try given up on.
    given_up: Mutex<HashMap<String, String>>,
    /// The mean pause between two tries of one key the author chose; the
    /// pause is drawn around it each time (see [`jitter`]). Unset, it is
    /// [`PROBE_GAP`].
    pace: Mutex<Option<Duration>>,
    /// The keeper keeps the keys that got through alive: a squeeze leaves
    /// them to it, however long they were not answered.
    keeping: AtomicBool,
    /// A squeeze stops once this many keys hold their place (0: once every
    /// key does).
    enough: AtomicUsize,
    /// The reasoning effort of the tries of a squeeze and of the pings the
    /// author chose (low unless set).
    effort: Mutex<Effort>,
    /// Separate from foreground routing: an ordinary successful request
    /// cannot silently resume background work stopped by failed pings.
    background: Mutex<HashMap<String, BackgroundKey>>,
    background_generation: AtomicU64,
    /// Zero uses the default until the saved setting is applied.
    keep_alive_failure_limit: AtomicU32,
}

/// A pause drawn around `mean`: a normal distribution with that mean, kept
/// within `spread` of it either way (`0.5` for half), its standard
/// deviation a quarter of that width, so most pauses fall near the mean
/// and none outside the bounds.
pub fn jitter(mean: Duration, spread: f64) -> Duration {
    let unit = || (crate::keys::random_u64() >> 11) as f64 / (1u64 << 53) as f64;
    let m = mean.as_secs_f64();
    let half = m * spread.clamp(0.0, 1.0);
    let sigma = half / 2.0;
    for _ in 0..32 {
        // Box-Muller: a standard normal from two uniform draws.
        let (u, v) = (unit().max(f64::MIN_POSITIVE), unit());
        let z = (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos();
        let x = m + z * sigma;
        if (m - half..=m + half).contains(&x) {
            return Duration::from_secs_f64(x.max(0.0));
        }
    }
    mean
}

/// A key that got through, for the settings view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThroughView {
    pub fingerprint: String,
    /// When it got through (Unix milliseconds).
    pub since_ms: u64,
    /// When it was last answered (Unix milliseconds).
    pub last_ok_ms: u64,
}

pub(crate) fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The key itself was refused (invalid, no permission, out of quota).
pub(crate) fn refuses_key(err: &TranslateError) -> bool {
    match err {
        TranslateError::Rejected { status, message } => {
            matches!(status, 401..=403 | 429) || crate::keys::mentions_quota(message)
        }
        _ => false,
    }
}

impl SqueezeBoard {
    /// A board for a pool that is not a crowded relay: keys that got through
    /// go first only after [`Self::allow_crowding`].
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A board for a crowded relay, such as AnyRouter.
    pub fn for_relay() -> Arc<Self> {
        let board = Self::new();
        board.allow_crowding();
        board
    }

    /// Let keys that got through go first while the relay turns keys away.
    pub fn allow_crowding(&self) {
        self.crowding.store(true, Ordering::Release);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Through>> {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn turned(&self) -> MutexGuard<'_, VecDeque<Instant>> {
        self.turned_away
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn given_up(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.given_up.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn background(&self) -> MutexGuard<'_, HashMap<String, BackgroundKey>> {
        self.background
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn next_generation(&self) -> u64 {
        self.background_generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Authorize a background request. A result from an older generation
    /// cannot change this key after it was stopped, removed or resumed.
    pub fn background_token(&self, fingerprint: &str) -> Option<u64> {
        let mut keys = self.background();
        let key = keys
            .entry(fingerprint.to_owned())
            .or_insert_with(|| BackgroundKey {
                generation: self.next_generation(),
                ..BackgroundKey::default()
            });
        key.stopped.is_none().then_some(key.generation)
    }

    pub fn background_token_current(&self, fingerprint: &str, token: u64) -> bool {
        self.background()
            .get(fingerprint)
            .is_some_and(|k| k.generation == token && k.stopped.is_none())
    }

    pub fn background_generation_matches(&self, fingerprint: &str, generation: u64) -> bool {
        self.background()
            .get(fingerprint)
            .is_some_and(|k| k.generation == generation)
    }

    /// Invalidate requests already in flight, without changing the user's
    /// background stop decision or the consecutive ping count.
    pub fn invalidate_background(&self, fingerprint: &str) {
        let mut keys = self.background();
        let key = keys.entry(fingerprint.to_owned()).or_default();
        key.generation = self.next_generation();
    }

    pub fn invalidate_background_requests(&self) {
        for key in self.background().values_mut() {
            key.generation = self.next_generation();
        }
    }

    pub fn background_state(&self, fingerprint: &str) -> (u32, bool, Option<String>) {
        self.background()
            .get(fingerprint)
            .map_or((0, false, None), |k| {
                (k.failures, k.stopped.is_some(), k.stopped.clone())
            })
    }

    pub fn background_stopped(&self, fingerprint: &str) -> bool {
        self.background()
            .get(fingerprint)
            .is_some_and(|k| k.stopped.is_some())
    }

    pub fn stop_target_was_met(&self, fingerprint: &str) -> bool {
        self.background()
            .get(fingerprint)
            .is_some_and(|k| k.target_met_at_stop)
    }

    pub fn stop_can_rejoin(&self, fingerprint: &str) -> bool {
        self.background()
            .get(fingerprint)
            .is_some_and(|k| k.rejoinable_stop)
    }

    /// Apply to subsequent ping results without clearing failures or
    /// resuming keys already stopped.
    pub fn set_keep_alive_failure_limit(&self, limit: u32) {
        self.keep_alive_failure_limit
            .store(limit.max(1), Ordering::Relaxed);
    }

    pub fn keep_alive_failure_limit(&self) -> u32 {
        match self.keep_alive_failure_limit.load(Ordering::Relaxed) {
            0 => DEFAULT_KEEP_ALIVE_FAILURE_LIMIT,
            limit => limit,
        }
    }

    pub fn stopped_keys(&self) -> Vec<(String, u32, String)> {
        self.background()
            .iter()
            .filter_map(|(f, k)| {
                k.stopped
                    .as_ref()
                    .map(|reason| (f.clone(), k.failures, reason.clone()))
            })
            .collect()
    }

    pub fn restore_stopped(&self, fingerprint: &str, failures: u32, reason: &str) {
        self.background().insert(
            fingerprint.to_owned(),
            BackgroundKey {
                generation: self.next_generation(),
                failures,
                stopped: Some(reason.to_owned()),
                target_met_at_stop: true,
                rejoinable_stop: false,
            },
        );
        self.forget(fingerprint);
    }

    /// An explicit user choice. It only makes the key eligible; it does
    /// not start a squeeze or issue a request.
    pub fn resume_background(&self, fingerprint: &str) -> bool {
        let mut keys = self.background();
        let Some(key) = keys.get_mut(fingerprint) else {
            return false;
        };
        if key.stopped.is_none() {
            return false;
        }
        key.generation = self.next_generation();
        key.failures = 0;
        key.stopped = None;
        key.target_met_at_stop = false;
        key.rejoinable_stop = false;
        self.given_up().remove(fingerprint);
        self.forget(fingerprint);
        true
    }

    /// Apply a full ping once. Internal protocol retries do not count as
    /// separate failures. Every successful ping resets the count.
    fn finish_ping(
        &self,
        fingerprint: &str,
        token: u64,
        result: &Result<(), TranslateError>,
    ) -> Option<u64> {
        let target_met = self.has_enough();
        let mut keys = self.background();
        let key = keys
            .get_mut(fingerprint)
            .filter(|k| k.generation == token && k.stopped.is_none())?;
        match result {
            Ok(()) => {
                key.failures = 0;
                self.answered(fingerprint);
            }
            Err(err) => {
                key.failures = key.failures.saturating_add(1);
                if key.failures >= self.keep_alive_failure_limit() || refuses_key(err) {
                    key.stopped = Some(err.to_string());
                    key.target_met_at_stop = target_met;
                    key.rejoinable_stop = !refuses_key(err);
                    key.generation = self.next_generation();
                    self.forget(fingerprint);
                    if refuses_key(err) {
                        self.give_up(fingerprint, &err.to_string());
                    }
                } else if crate::keys::answered_anyway(err) {
                    self.answered(fingerprint);
                } else if matches!(err, TranslateError::Server { .. })
                    && !self.is_through(fingerprint)
                {
                    self.note_turned_away();
                }
            }
        }
        Some(key.generation)
    }

    fn finish_try(
        &self,
        fingerprint: &str,
        token: u64,
        result: &Result<(), TranslateError>,
        sent: Instant,
    ) -> bool {
        let keys = self.background();
        if !keys
            .get(fingerprint)
            .is_some_and(|k| k.generation == token && k.stopped.is_none())
        {
            return false;
        }
        match result {
            Ok(()) => self.answered(fingerprint),
            Err(err) if crate::keys::answered_anyway(err) => self.answered(fingerprint),
            Err(err) => {
                let was_through = self.is_through(fingerprint);
                self.failed(fingerprint, err, sent);
                if !was_through && matches!(err, TranslateError::Server { .. }) {
                    self.note_turned_away();
                }
            }
        }
        true
    }

    /// The mean pause between two tries of one key (`None`: the default).
    /// The reasoning effort of the tries of a squeeze and of the pings.
    pub fn set_effort(&self, effort: Effort) {
        *self.effort.lock().unwrap_or_else(PoisonError::into_inner) = effort;
    }

    pub fn effort(&self) -> Effort {
        *self.effort.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_pace(&self, mean: Option<Duration>) {
        *self.pace.lock().unwrap_or_else(PoisonError::into_inner) = mean;
    }

    /// The pause before the next try of a key: drawn within half the mean
    /// either way, or [`PROBE_GAP`].
    fn next_gap(&self) -> Duration {
        let mean = *self.pace.lock().unwrap_or_else(PoisonError::into_inner);
        mean.map_or(PROBE_GAP, |m| jitter(m, 0.5))
    }

    /// The author sent a request with one of the keys, or started a squeeze.
    pub fn note_use(&self) {
        *self.used.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    }

    /// The author used the keys within `d`.
    pub fn used_within(&self, d: Duration) -> bool {
        self.used
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|t| t.elapsed() < d)
    }

    /// A squeeze gave up on this key, after the server said `why`.
    pub fn give_up(&self, fingerprint: &str, why: &str) {
        self.given_up()
            .insert(fingerprint.to_owned(), why.to_owned());
    }

    /// A squeeze gave up on this key, and it was not answered since.
    pub fn gave_up(&self, fingerprint: &str) -> bool {
        self.given_up().contains_key(fingerprint)
    }

    /// The keys a squeeze gave up on and not answered since, with what the
    /// server said.
    pub fn given_up_on(&self) -> Vec<(String, String)> {
        self.given_up()
            .iter()
            .map(|(f, why)| (f.clone(), why.clone()))
            .collect()
    }

    pub fn is_through(&self, fingerprint: &str) -> bool {
        self.lock().contains_key(fingerprint)
    }

    /// The keeper keeps the keys that got through alive (or no more).
    pub fn set_keeping(&self, on: bool) {
        self.keeping.store(on, Ordering::Release);
    }

    /// A squeeze stops once `n` keys hold their place (0: once every key
    /// does).
    pub fn set_enough(&self, n: usize) {
        self.enough.store(n, Ordering::Release);
    }

    /// As many keys hold their place as a squeeze is asked to get through.
    pub fn has_enough(&self) -> bool {
        let n = self.enough.load(Ordering::Acquire);
        if n == 0 {
            return false;
        }
        let through: Vec<String> = self.lock().keys().cloned().collect();
        through.iter().filter(|f| self.holds(f)).count() >= n
    }

    /// A squeeze need not try this key: answered within [`STALE_AFTER`], or
    /// through and kept alive by the keeper.
    fn holds(&self, fingerprint: &str) -> bool {
        !self.background_stopped(fingerprint)
            && (self.is_fresh(fingerprint)
                || (self.keeping.load(Ordering::Acquire) && self.is_through(fingerprint)))
    }

    /// Through and answered within [`STALE_AFTER`].
    pub fn is_fresh(&self, fingerprint: &str) -> bool {
        self.lock()
            .get(fingerprint)
            .is_some_and(|t| t.last_ok.elapsed() < STALE_AFTER)
    }

    /// The server turned a key that was not through away (a server error).
    pub fn note_turned_away(&self) {
        let mut turned = self.turned();
        turned.push_back(Instant::now());
        while turned.len() > CROWDED_AFTER {
            turned.pop_front();
        }
        if self.crowding.load(Ordering::Acquire)
            && turned.len() >= CROWDED_AFTER
            && turned.front().is_some_and(|t| t.elapsed() < STALE_AFTER)
        {
            self.crowded_seen.store(true, Ordering::Release);
        }
    }

    /// The author started a squeeze: crowding applies, and the keys that get
    /// through are worth keeping alive, however the relay turned them away
    /// (a squeeze's own tries do not count as turned away).
    pub fn note_squeezed(&self) {
        self.allow_crowding();
        self.crowded_seen.store(true, Ordering::Release);
    }

    /// The relay turned keys away as often as [`Self::crowded`] asks at some
    /// time since crowding applies (a crowded relay, or a pool a squeeze was
    /// started for), or the author started a squeeze: its keys that got
    /// through are worth keeping alive. An ordinary pool's server errors
    /// never count.
    pub fn was_crowded(&self) -> bool {
        self.crowded_seen.load(Ordering::Acquire)
    }

    /// How long a key that got through has not been answered.
    pub fn idle(&self, fingerprint: &str) -> Option<Duration> {
        self.lock().get(fingerprint).map(|t| t.last_ok.elapsed())
    }

    /// The relay turned [`CROWDED_AFTER`] keys away within [`STALE_AFTER`],
    /// and crowding applies to this board: keys that got through are worth
    /// waiting for.
    pub fn crowded(&self) -> bool {
        if !self.crowding.load(Ordering::Acquire) {
            return false;
        }
        let turned = self.turned();
        turned.len() >= CROWDED_AFTER && turned.front().is_some_and(|t| t.elapsed() < STALE_AFTER)
    }

    /// A request with this key was answered.
    pub fn answered(&self, fingerprint: &str) {
        self.given_up().remove(fingerprint);
        let now = SystemTime::now();
        let mut keys = self.lock();
        let entry = keys.entry(fingerprint.to_owned()).or_insert(Through {
            since: now,
            last_ok: Instant::now(),
            last_ok_wall: now,
            fails: 0,
        });
        entry.last_ok = Instant::now();
        entry.last_ok_wall = now;
        entry.fails = 0;
    }

    /// A request with this key, sent at `sent`, failed with `err`. Returns
    /// whether the key still counts as through (false if it did not
    /// before). A request sent before the key's latest answer is old news,
    /// and a network error is not the relay's doing.
    pub(crate) fn failed(&self, fingerprint: &str, err: &TranslateError, sent: Instant) -> bool {
        let mut keys = self.lock();
        let Some(entry) = keys.get_mut(fingerprint) else {
            return false;
        };
        if sent < entry.last_ok {
            return true;
        }
        let keep = match err {
            TranslateError::Server { .. } => {
                entry.fails += 1;
                entry.fails < DROP_AFTER && entry.last_ok.elapsed() < STALE_AFTER
            }
            err => !refuses_key(err),
        };
        if !keep {
            keys.remove(fingerprint);
        }
        keep
    }

    /// The keys changed: no key counts as through until it is answered
    /// again.
    pub fn clear(&self) {
        self.lock().clear();
        self.given_up().clear();
        self.background().clear();
    }

    /// A pool edit invalidates old requests but retains user decisions
    /// for keys whose fingerprints remain in the pool.
    pub fn clear_through(&self) {
        self.lock().clear();
        self.given_up().clear();
        self.invalidate_background_requests();
    }

    pub fn retain_background_keys(&self, fingerprints: &[String]) {
        self.background().retain(|f, _| fingerprints.contains(f));
    }

    /// This key is no longer in the pool.
    pub fn forget(&self, fingerprint: &str) {
        self.lock().remove(fingerprint);
    }

    /// The keys that got through, earliest first.
    pub fn through(&self) -> Vec<ThroughView> {
        let mut out: Vec<ThroughView> = self
            .lock()
            .iter()
            .map(|(fingerprint, t)| ThroughView {
                fingerprint: fingerprint.clone(),
                since_ms: unix_ms(t.since),
                last_ok_ms: unix_ms(t.last_ok_wall),
            })
            .collect();
        out.retain(|t| !self.background_stopped(&t.fingerprint));
        out.sort_by_key(|t| t.since_ms);
        out
    }
}

/// How a squeeze is getting on with one key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyProgress {
    pub fingerprint: String,
    pub tries: u32,
    /// The latest try's error (redacted).
    pub last_error: Option<String>,
    /// A try is on its way.
    pub trying: bool,
    /// The key is not tried again: it was refused (invalid, no permission,
    /// out of quota), or the request was (a wrong model, a bad request).
    pub refused: bool,
}

/// What changed in a squeeze.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SqueezeChange {
    /// The squeeze started (said by whoever started it).
    Started,
    /// A try failed.
    Tried,
    /// Key `number` (1-based, in pool order) got through.
    Through { number: usize },
    /// The squeeze ended. `all_through`: at least one key got through and
    /// every other key was refused, rather than stopped.
    Ended {
        #[serde(rename = "allThrough")]
        all_through: bool,
    },
    /// A round of keeping keys alive went by (said by the keeper).
    Kept,
    /// A key stopped its background work after failed keep-alives.
    #[serde(rename = "keyStopped")]
    KeyStopped {
        number: usize,
        fingerprint: String,
        failures: u32,
        reason: String,
        rejoined: bool,
    },
}

pub type ChangeFn = Arc<dyn Fn(SqueezeChange) + Send + Sync>;

/// The result of one complete ping and the generation that accepted it.
/// A result discarded after a pool edit or stop has no generation.
pub struct PingOutcome {
    pub result: Result<(), TranslateError>,
    pub generation: Option<u64>,
}

/// Send `key` of the provider `config` a short translation, which keeps its
/// place at a crowded relay. The answer, or the failure, is recorded on
/// `board` as for any request (but not as the author's use), and the request
/// is reported to `observer` as [`crate::KEEP_ALIVE`].
pub async fn ping(
    config: &ProviderConfig,
    key: String,
    board: Arc<SqueezeBoard>,
    prompts: Arc<dyn PromptSource>,
    observer: Arc<dyn RequestObserver>,
) -> Result<(), TranslateError> {
    ping_checked(config, key, board, prompts, observer)
        .await
        .result
}

pub async fn ping_checked(
    config: &ProviderConfig,
    key: String,
    board: Arc<SqueezeBoard>,
    prompts: Arc<dyn PromptSource>,
    observer: Arc<dyn RequestObserver>,
) -> PingOutcome {
    let print = fingerprint(&key);
    let Some(token) = board.background_token(&print) else {
        return PingOutcome {
            result: Err(TranslateError::Network(
                "background requests for this key are stopped".into(),
            )),
            generation: None,
        };
    };
    // Not joined to the board: a ping that fails is sent again soon, and
    // the key keeps its place meanwhile.
    let provider = match HttpProvider::new(
        crate::probe::config(config, board.effort()),
        Arc::new(move || Ok(vec![key.clone()])),
        prompts,
        observer,
    ) {
        Ok(provider) => provider,
        Err(err) => {
            return PingOutcome {
                result: Err(err),
                generation: None,
            };
        }
    };
    let result = match tokio::time::timeout(
        PROBE_TIMEOUT,
        provider.probe(crate::provider::KEEP_ALIVE),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(TranslateError::Network(format!(
            "no answer within {} seconds",
            PROBE_TIMEOUT.as_secs()
        ))),
    };
    let generation = board.finish_ping(&print, token, &result);
    PingOutcome { result, generation }
}

/// The relay turned the try away for now: trying again may get through.
fn worth_another_try(err: &TranslateError) -> bool {
    matches!(
        err,
        TranslateError::Server { .. }
            | TranslateError::Network(_)
            | TranslateError::RateLimited { .. }
            | TranslateError::InvalidResponse(_)
    )
}

/// A running squeeze of one provider's keys.
pub struct Squeeze {
    stop: watch::Sender<bool>,
    progress: Mutex<Vec<KeyProgress>>,
    started: SystemTime,
    running: AtomicBool,
    /// A task saw every key through: all tasks end.
    done: AtomicBool,
    /// Nobody asked for it just now: while the author does not use the
    /// keys, it waits longer after every try turned away.
    unattended: bool,
}

impl Squeeze {
    /// Start trying `keys` of the provider `config` on `runtime`. Keys on
    /// `board` that were answered lately are left alone while they stay
    /// so, and the board lets keys that got through go first from now on.
    /// `on_change` is called from the squeeze's tasks. An `unattended`
    /// squeeze (nobody asked for it just now) waits longer after every try
    /// turned away, up to a minute, unless the author used the keys within
    /// [`ATTENDED_WITHIN`].
    pub fn start(
        config: &ProviderConfig,
        keys: Vec<String>,
        board: Arc<SqueezeBoard>,
        prompts: Arc<dyn PromptSource>,
        on_change: ChangeFn,
        runtime: &tokio::runtime::Handle,
        unattended: bool,
    ) -> Result<Arc<Self>, TranslateError> {
        let mut providers = Vec::with_capacity(keys.len());
        for key in &keys {
            let key = key.clone();
            let provider = HttpProvider::new(
                crate::probe::config(config, board.effort()),
                Arc::new(move || Ok(vec![key.clone()])),
                Arc::clone(&prompts),
                Arc::new(NoObserver),
            )?;
            providers.push(provider);
        }
        board.allow_crowding();
        let (stop, _) = watch::channel(false);
        let squeeze = Arc::new(Self {
            stop,
            progress: Mutex::new(
                keys.iter()
                    .map(|k| KeyProgress {
                        fingerprint: fingerprint(k),
                        ..KeyProgress::default()
                    })
                    .collect(),
            ),
            started: SystemTime::now(),
            running: AtomicBool::new(true),
            done: AtomicBool::new(false),
            unattended,
        });
        let tasks: Vec<_> = providers
            .into_iter()
            .enumerate()
            .map(|(index, provider)| {
                let squeeze = Arc::clone(&squeeze);
                let board = Arc::clone(&board);
                let on_change = Arc::clone(&on_change);
                runtime
                    .spawn(async move { squeeze.work(index, provider, &board, &on_change).await })
            })
            .collect();
        let done = Arc::clone(&squeeze);
        runtime.spawn(async move {
            for task in tasks {
                let _ = task.await;
            }
            done.running.store(false, Ordering::Release);
            let all_through = !*done.stop.borrow() && done.done.load(Ordering::Acquire);
            on_change(SqueezeChange::Ended { all_through });
        });
        Ok(squeeze)
    }

    pub fn stop(&self) {
        self.stop.send_replace(true);
    }

    /// Running and not asked to stop.
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Acquire) && !self.stopped()
    }

    /// When it started (Unix milliseconds).
    pub fn started_ms(&self) -> u64 {
        unix_ms(self.started)
    }

    /// The author set key `fingerprint` aside: it is tried no more.
    pub fn set_aside(&self, fingerprint: &str) {
        for p in self
            .lock()
            .iter_mut()
            .filter(|p| p.fingerprint == fingerprint)
        {
            p.refused = true;
        }
    }

    pub fn progress(&self) -> Vec<KeyProgress> {
        self.lock().clone()
    }

    /// Rejoin an eligible key in this existing squeeze. This never starts
    /// a new run and cannot undo a completed target.
    pub fn rejoin_key(&self, board: &SqueezeBoard, fingerprint: &str) -> bool {
        if !self.running() || self.done.load(Ordering::Acquire) || board.has_enough() {
            return false;
        }
        if !self
            .lock()
            .iter()
            .any(|p| p.fingerprint == fingerprint && !p.refused)
        {
            return false;
        }
        board.resume_background(fingerprint)
    }

    fn lock(&self) -> MutexGuard<'_, Vec<KeyProgress>> {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// At least one key got through lately, and every other key did too or
    /// is not tried again.
    fn all_through(&self, board: &SqueezeBoard) -> bool {
        if board.has_enough() {
            return true;
        }
        let progress = self.lock();
        progress.iter().any(|p| board.holds(&p.fingerprint))
            && progress.iter().all(|p| {
                p.refused || board.background_stopped(&p.fingerprint) || board.holds(&p.fingerprint)
            })
    }

    fn stopped(&self) -> bool {
        *self.stop.borrow()
    }

    /// Sleep for `d`, or until stopped. Returns whether it was stopped.
    async fn pause(&self, d: Duration) -> bool {
        let mut stop = self.stop.subscribe();
        if *stop.borrow_and_update() {
            return true;
        }
        tokio::select! {
            () = tokio::time::sleep(d) => self.stopped(),
            _ = stop.changed() => true,
        }
    }

    /// Try key `index` until it got through and every other key did too,
    /// until it is refused, or until stopped. A key that got through and was
    /// not answered for [`STALE_AFTER`] is tried again.
    async fn work(
        &self,
        index: usize,
        provider: HttpProvider,
        board: &SqueezeBoard,
        on_change: &ChangeFn,
    ) {
        let fingerprint = self.lock()[index].fingerprint.clone();
        let mut stop = self.stop.subscribe();
        // Tries turned away in a row.
        let mut misses: u32 = 0;
        loop {
            if self.stopped() || self.done.load(Ordering::Acquire) || self.lock()[index].refused {
                return;
            }
            if self.all_through(board) {
                self.done.store(true, Ordering::Release);
                return;
            }
            if board.background_stopped(&fingerprint) {
                // Keep a worker available for an explicit rejoin while
                // another eligible key is still being squeezed.
                if self
                    .lock()
                    .iter()
                    .all(|p| p.refused || board.background_stopped(&p.fingerprint))
                {
                    return;
                }
                if self.pause(Duration::from_millis(100)).await {
                    return;
                }
                continue;
            }
            if board.holds(&fingerprint) {
                // Watch it: translations may find it lost its place.
                if self.pause(Duration::from_secs(1)).await {
                    return;
                }
                continue;
            }
            let was_through = board.is_through(&fingerprint);
            let Some(token) = board.background_token(&fingerprint) else {
                continue;
            };
            let sent = Instant::now();
            self.lock()[index].trying = true;
            // Each try asks for other words (see `crate::probe`).
            let attempt =
                tokio::time::timeout(PROBE_TIMEOUT, provider.probe(crate::provider::TRANSLATE));
            let result = tokio::select! {
                r = attempt => r,
                _ = stop.changed() => {
                    self.lock()[index].trying = false;
                    return;
                }
            };
            let result = result.unwrap_or_else(|_| {
                Err(TranslateError::Network(format!(
                    "no answer within {} seconds",
                    PROBE_TIMEOUT.as_secs()
                )))
            });
            if !board.finish_try(&fingerprint, token, &result, sent) {
                self.lock()[index].trying = false;
                continue;
            }
            // An answer counts even when it held no translation.
            let through = board.is_fresh(&fingerprint);
            misses = if through { 0 } else { misses.saturating_add(1) };
            let base = board.next_gap();
            let gap_max = if self.unattended && !board.used_within(ATTENDED_WITHIN) {
                UNATTENDED_GAP_MAX.max(base)
            } else {
                base
            };
            let gap = base
                .saturating_mul(1 << misses.saturating_sub(1).min(5))
                .min(gap_max);
            let wait = {
                let mut progress = self.lock();
                let p = &mut progress[index];
                p.trying = false;
                p.tries += 1;
                match &result {
                    Ok(_) => {
                        p.last_error = None;
                        gap
                    }
                    Err(err) => {
                        let why = err.to_string();
                        p.refused = !through && !worth_another_try(err);
                        if p.refused {
                            board.give_up(&fingerprint, &why);
                        }
                        p.last_error = Some(why);
                        err.retry_after()
                            .map_or(gap, |d| d.clamp(gap, PROBE_TIMEOUT))
                    }
                }
            };
            if through {
                // A key got through for the author, who may have stepped
                // away while waiting: it is kept alive from now on.
                if !was_through && !self.unattended {
                    board.note_use();
                }
                // A key that kept its place was only checked again.
                on_change(if was_through {
                    SqueezeChange::Tried
                } else {
                    SqueezeChange::Through { number: index + 1 }
                });
                continue;
            }
            on_change(SqueezeChange::Tried);
            if self.pause(wait).await {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pauses_spread_around_their_mean_within_the_bounds() {
        let mean = Duration::from_secs(240);
        let draws: Vec<f64> = (0..4000)
            .map(|_| jitter(mean, 0.25).as_secs_f64())
            .collect();
        assert!(
            draws.iter().all(|d| (180.0..=300.0).contains(d)),
            "within three to five minutes"
        );
        let avg = draws.iter().sum::<f64>() / draws.len() as f64;
        assert!(
            (avg - 240.0).abs() < 3.0,
            "about four minutes on average: {avg}"
        );
        assert!(
            draws.iter().any(|d| *d < 200.0) && draws.iter().any(|d| *d > 280.0),
            "spread out"
        );
        let near = draws.iter().filter(|d| (**d - 240.0).abs() < 30.0).count();
        assert!(near * 2 > draws.len(), "most near the mean: {near}");
        let tries: Vec<f64> = (0..2000)
            .map(|_| jitter(Duration::from_secs(3), 0.5).as_secs_f64())
            .collect();
        assert!(tries.iter().all(|d| (1.5..=4.5).contains(d)));
        let board = SqueezeBoard::default();
        assert_eq!(board.next_gap(), PROBE_GAP, "unset: the fixed pause");
        board.set_pace(Some(Duration::from_secs(20)));
        let gap = board.next_gap().as_secs_f64();
        assert!((10.0..=30.0).contains(&gap), "{gap}");
    }

    fn server() -> TranslateError {
        TranslateError::Server {
            status: 500,
            retry_after: None,
            message: String::new(),
        }
    }

    fn fail(board: &SqueezeBoard, key: &str, err: &TranslateError) -> bool {
        board.failed(key, err, Instant::now())
    }

    #[test]
    fn twenty_failed_pings_stop_background_work_without_disabling_foreground_requests() {
        let board = SqueezeBoard::new();
        assert_eq!(board.keep_alive_failure_limit(), 20);
        board.answered("a");
        let token = board.background_token("a").unwrap();
        for expected in 1..DEFAULT_KEEP_ALIVE_FAILURE_LIMIT {
            let _ = board.finish_ping("a", token, &Err(server()));
            assert_eq!(board.background_state("a").0, expected);
            assert!(board.is_through("a"));
        }
        board.answered("a");
        assert_eq!(
            board.background_state("a").0,
            DEFAULT_KEEP_ALIVE_FAILURE_LIMIT - 1,
            "foreground success is separate"
        );
        let _ = board.finish_ping("a", token, &Ok(()));
        assert_eq!(board.background_state("a"), (0, false, None));
        for _ in 0..DEFAULT_KEEP_ALIVE_FAILURE_LIMIT {
            let _ = board.finish_ping("a", token, &Err(server()));
        }
        assert!(board.background_stopped("a"));
        assert_eq!(
            board.background_state("a").0,
            DEFAULT_KEEP_ALIVE_FAILURE_LIMIT
        );
        assert!(!board.is_through("a"));
        assert!(board.background_token("a").is_none());
        // A foreground response may still route normally, but cannot
        // restart the keeper or make this key count toward its target.
        board.answered("a");
        board.set_enough(1);
        assert!(board.is_through("a"));
        assert!(!board.has_enough());
        assert!(board.through().is_empty());
        let _ = board.finish_ping("a", token, &Ok(()));
        assert_eq!(
            board.background_state("a").0,
            DEFAULT_KEEP_ALIVE_FAILURE_LIMIT
        );
        assert!(!board.finish_try("a", token, &Ok(()), Instant::now()));
        assert!(board.resume_background("a"));
        assert_eq!(board.background_state("a"), (0, false, None));
        assert!(
            !board.background_token_current("a", token),
            "resume invalidates old responses too"
        );
        let fresh = board.background_token("a").unwrap();
        let _ = board.finish_ping("a", fresh, &Ok(()));
        assert!(board.has_enough());
    }

    #[test]
    fn raising_the_limit_keeps_failures_and_does_not_resume_stopped_keys() {
        let board = SqueezeBoard::new();
        board.set_keep_alive_failure_limit(10);
        board.answered("a");
        let token = board.background_token("a").unwrap();
        for _ in 0..9 {
            let _ = board.finish_ping("a", token, &Err(server()));
        }
        board.set_keep_alive_failure_limit(30);
        assert_eq!(board.background_state("a"), (9, false, None));
        for expected in 10..30 {
            let _ = board.finish_ping("a", token, &Err(server()));
            assert_eq!(board.background_state("a"), (expected, false, None));
        }
        let _ = board.finish_ping("a", token, &Err(server()));
        assert!(board.background_stopped("a"));
        assert_eq!(board.background_state("a").0, 30);
        board.set_keep_alive_failure_limit(100);
        assert!(board.background_stopped("a"));
        assert!(board.background_token("a").is_none());
        assert!(
            board.stop_can_rejoin("a"),
            "the recorded cause stays retryable"
        );
    }

    #[test]
    fn lowering_the_limit_applies_to_the_next_ping_result() {
        let board = SqueezeBoard::new();
        board.set_keep_alive_failure_limit(30);
        let token = board.background_token("a").unwrap();
        for _ in 0..10 {
            let _ = board.finish_ping("a", token, &Err(server()));
        }
        board.set_keep_alive_failure_limit(5);
        assert_eq!(board.background_state("a"), (10, false, None));
        let _ = board.finish_ping("a", token, &Err(server()));
        assert!(board.background_stopped("a"));
        assert_eq!(board.background_state("a").0, 11);
        assert!(board.resume_background("a"));
        let fresh = board.background_token("a").unwrap();
        for _ in 0..4 {
            let _ = board.finish_ping("a", fresh, &Err(server()));
        }
        let _ = board.finish_ping("a", fresh, &Ok(()));
        assert_eq!(board.background_state("a"), (0, false, None));
    }

    #[test]
    fn a_refused_ping_stops_immediately_and_cannot_resume_itself() {
        let board = SqueezeBoard::new();
        board.answered("a");
        let token = board.background_token("a").unwrap();
        let _ = board.finish_ping(
            "a",
            token,
            &Err(TranslateError::Rejected {
                status: 401,
                message: "invalid key".into(),
            }),
        );
        assert!(board.background_stopped("a"));
        assert!(board.gave_up("a"));
        assert!(!board.stop_can_rejoin("a"));
        assert_eq!(board.background_state("a").0, 1);
        board.answered("a");
        assert!(board.background_stopped("a"));
        assert!(board.resume_background("a"));
        assert_eq!(board.background_state("a"), (0, false, None));
    }

    #[test]
    fn invalidated_background_results_cannot_reinsert_a_removed_key() {
        let board = SqueezeBoard::new();
        let token = board.background_token("a").unwrap();
        board.invalidate_background("a");
        assert!(!board.finish_try("a", token, &Ok(()), Instant::now()));
        let _ = board.finish_ping("a", token, &Ok(()));
        assert!(!board.is_through("a"));
        let token = board.background_token("a").unwrap();
        board.clear();
        let fresh = board.background_token("a").unwrap();
        assert_ne!(token, fresh);
        let _ = board.finish_ping("a", token, &Err(server()));
        assert_eq!(board.background_state("a").0, 0);
    }

    #[test]
    fn unusable_ping_answers_count_as_failures_until_a_real_success() {
        let board = SqueezeBoard::new();
        let token = board.background_token("a").unwrap();
        for _ in 0..DEFAULT_KEEP_ALIVE_FAILURE_LIMIT {
            let _ = board.finish_ping(
                "a",
                token,
                &Err(TranslateError::InvalidResponse("cut off".into())),
            );
        }
        assert!(board.background_stopped("a"));
        assert_eq!(
            board.background_state("a").0,
            DEFAULT_KEEP_ALIVE_FAILURE_LIMIT
        );
    }

    #[test]
    fn editing_other_pool_keys_keeps_stops_and_invalidates_old_results() {
        let board = SqueezeBoard::new();
        board.restore_stopped("stopped", 10, "server unavailable");
        let old = board.background_token("active").unwrap();
        board.answered("active");
        board.clear_through();
        assert!(board.background_stopped("stopped"));
        assert!(!board.is_through("active"));
        let _ = board.finish_ping("active", old, &Ok(()));
        assert!(!board.is_through("active"));
        board.retain_background_keys(&["stopped".into(), "new".into()]);
        assert!(board.background_stopped("stopped"));
        board.retain_background_keys(&["new".into()]);
        assert!(
            board.stopped_keys().is_empty(),
            "deleted fingerprints are pruned"
        );
    }

    #[test]
    fn background_completion_and_foreground_routing_can_run_concurrently() {
        let board = SqueezeBoard::new();
        board.set_enough(1);
        let (done, received) = std::sync::mpsc::channel();
        for worker in 0..3 {
            let board = Arc::clone(&board);
            let done = done.clone();
            std::thread::spawn(move || {
                for _ in 0..1000 {
                    match worker {
                        0 => {
                            if let Some(token) = board.background_token("a") {
                                let _ = board.finish_ping("a", token, &Err(server()));
                            } else {
                                board.resume_background("a");
                            }
                        }
                        1 => board.answered("a"),
                        _ => {
                            let _ = board.has_enough();
                            let _ = board.through();
                        }
                    }
                }
                let _ = done.send(());
            });
        }
        for _ in 0..3 {
            received
                .recv_timeout(Duration::from_secs(5))
                .expect("board lock ordering must complete");
        }
    }

    #[test]
    fn an_answer_puts_a_key_on_the_board_and_keeps_its_first_time() {
        let board = SqueezeBoard::new();
        assert!(!board.is_through("a"));
        board.answered("a");
        let first = board.through()[0].since_ms;
        board.answered("a");
        assert!(board.is_through("a"));
        assert!(board.is_fresh("a"));
        assert_eq!(board.through().len(), 1);
        assert_eq!(board.through()[0].since_ms, first);
    }

    #[test]
    fn a_key_through_survives_a_few_server_errors() {
        let board = SqueezeBoard::new();
        board.answered("a");
        for _ in 0..DROP_AFTER - 1 {
            assert!(fail(&board, "a", &server()));
        }
        // An answer starts the count again.
        board.answered("a");
        for _ in 0..DROP_AFTER - 1 {
            assert!(fail(&board, "a", &server()));
        }
        assert!(!fail(&board, "a", &server()), "six in a row drop it");
        assert!(!board.is_through("a"));
    }

    #[test]
    fn network_errors_and_requests_sent_before_an_answer_do_not_count() {
        let board = SqueezeBoard::new();
        let sent_before = Instant::now();
        board.answered("a");
        for _ in 0..2 * DROP_AFTER {
            assert!(fail(
                &board,
                "a",
                &TranslateError::Network("offline".into())
            ));
            assert!(board.failed("a", &server(), sent_before));
        }
        // Only five server errors of requests sent after the answer so far.
        for _ in 0..DROP_AFTER - 1 {
            assert!(fail(&board, "a", &server()));
        }
        assert!(board.is_through("a"));
    }

    #[test]
    fn a_refused_key_leaves_the_board_and_a_rate_limit_does_not() {
        let board = SqueezeBoard::new();
        board.answered("a");
        assert!(fail(
            &board,
            "a",
            &TranslateError::RateLimited {
                retry_after: None,
                message: String::new()
            }
        ));
        let bad = TranslateError::Rejected {
            status: 400,
            message: "bad request".into(),
        };
        assert!(fail(&board, "a", &bad));
        let invalid = TranslateError::Rejected {
            status: 401,
            message: "invalid key".into(),
        };
        assert!(!fail(&board, "a", &invalid));
        assert!(!board.is_through("a"));
        assert!(!fail(&board, "b", &server()), "never through");
        board.answered("c");
        let quota = TranslateError::Rejected {
            status: 429,
            message: "insufficient_quota".into(),
        };
        assert!(!fail(&board, "c", &quota));
    }

    #[test]
    fn a_relay_is_crowded_after_turning_keys_away_twice_and_only_when_allowed() {
        let ordinary = SqueezeBoard::new();
        for _ in 0..10 {
            ordinary.note_turned_away();
        }
        assert!(!ordinary.crowded(), "an ordinary pool takes turns");
        ordinary.allow_crowding();
        assert!(ordinary.crowded(), "a squeeze was started for it");

        let relay = SqueezeBoard::for_relay();
        assert!(!relay.crowded());
        relay.note_turned_away();
        assert!(!relay.crowded(), "one stray server error");
        relay.note_turned_away();
        assert!(relay.crowded());
    }

    #[test]
    fn crowding_is_remembered_for_keeping_keys_alive_where_it_applies() {
        // An ordinary pool's server errors (an outage) are not a crowd.
        let ordinary = SqueezeBoard::new();
        for _ in 0..10 {
            ordinary.note_turned_away();
        }
        assert!(!ordinary.was_crowded());
        // Once a squeeze was started for it, they are.
        ordinary.allow_crowding();
        ordinary.note_turned_away();
        assert!(ordinary.was_crowded());

        let relay = SqueezeBoard::for_relay();
        relay.note_turned_away();
        relay.note_turned_away();
        assert!(relay.was_crowded());
        assert_eq!(relay.idle("a"), None);
        relay.answered("a");
        assert!(relay.idle("a").is_some_and(|d| d < Duration::from_secs(1)));
    }

    #[test]
    fn the_authors_use_is_remembered_for_a_while() {
        let board = SqueezeBoard::new();
        let hour = Duration::from_secs(3600);
        assert!(!board.used_within(hour), "never used");
        board.note_use();
        assert!(board.used_within(KEEP_ALIVE_FOR));
        let Some(long_ago) = Instant::now().checked_sub(KEEP_ALIVE_FOR + Duration::from_secs(1))
        else {
            return;
        };
        *board.used.lock().unwrap() = Some(long_ago);
        assert!(!board.used_within(KEEP_ALIVE_FOR));
    }

    #[test]
    fn enough_keys_through_is_as_many_as_asked() {
        let board = SqueezeBoard::for_relay();
        board.answered("a");
        assert!(!board.has_enough(), "unset: every key");
        board.set_enough(2);
        assert!(!board.has_enough());
        board.answered("b");
        assert!(board.has_enough());
        board.forget("b");
        assert!(!board.has_enough(), "one lost its place");
        board.set_enough(1);
        assert!(board.has_enough());
    }

    #[test]
    fn a_squeeze_leaves_the_keys_the_keeper_keeps_alive_to_it() {
        let board = SqueezeBoard::for_relay();
        board.answered("a");
        let Some(long_ago) = Instant::now().checked_sub(STALE_AFTER + Duration::from_secs(1))
        else {
            return;
        };
        board.lock().get_mut("a").unwrap().last_ok = long_ago;
        assert!(!board.holds("a"), "not answered lately: tried again");
        board.set_keeping(true);
        assert!(board.holds("a"), "the keeper pings it");
        assert!(!board.holds("b"), "a key not through is tried");
        board.set_keeping(false);
        assert!(!board.holds("a"));
    }

    #[test]
    fn a_key_given_up_on_is_so_until_answered_or_the_keys_change() {
        let board = SqueezeBoard::new();
        board.give_up("a", "HTTP 403");
        board.give_up("b", "HTTP 401");
        assert!(board.gave_up("a") && board.gave_up("b"));
        assert_eq!(board.given_up_on().len(), 2);
        board.answered("a");
        assert!(!board.gave_up("a"));
        board.clear();
        assert!(!board.gave_up("b"));
    }
}
