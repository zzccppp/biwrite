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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use biwrite_engine::TranslateError;
use serde::Serialize;
use tokio::sync::watch;

use crate::config::ProviderConfig;
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
/// Pause between two tries of one key.
const PROBE_GAP: Duration = Duration::from_secs(3);
/// Longest pause between two tries of a squeeze nobody watches, which
/// doubles [`PROBE_GAP`] after every try turned away.
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
    /// succeed), until answered: not squeezed back in by themselves.
    given_up: Mutex<HashSet<String>>,
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

    fn given_up(&self) -> MutexGuard<'_, HashSet<String>> {
        self.given_up.lock().unwrap_or_else(PoisonError::into_inner)
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

    /// A squeeze gave up on this key.
    pub fn give_up(&self, fingerprint: &str) {
        self.given_up().insert(fingerprint.to_owned());
    }

    /// A squeeze gave up on this key, and it was not answered since.
    pub fn gave_up(&self, fingerprint: &str) -> bool {
        self.given_up().contains(fingerprint)
    }

    pub fn is_through(&self, fingerprint: &str) -> bool {
        self.lock().contains_key(fingerprint)
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
}

pub type ChangeFn = Arc<dyn Fn(SqueezeChange) + Send + Sync>;

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
    let provider = HttpProvider::new(
        crate::probe::config(config),
        Arc::new(move || Ok(vec![key.clone()])),
        prompts,
        observer,
    )?;
    provider.join_board_quietly(board);
    match tokio::time::timeout(PROBE_TIMEOUT, provider.probe(crate::provider::KEEP_ALIVE)).await {
        Ok(result) => result,
        Err(_) => Err(TranslateError::Network(format!(
            "no answer within {} seconds",
            PROBE_TIMEOUT.as_secs()
        ))),
    }
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
                crate::probe::config(config),
                Arc::new(move || Ok(vec![key.clone()])),
                Arc::clone(&prompts),
                Arc::new(NoObserver),
            )?;
            provider.join_board_quietly(Arc::clone(&board));
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

    pub fn progress(&self) -> Vec<KeyProgress> {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<KeyProgress>> {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// At least one key got through lately, and every other key did too or
    /// is not tried again.
    fn all_through(&self, board: &SqueezeBoard) -> bool {
        let progress = self.lock();
        progress.iter().any(|p| board.is_fresh(&p.fingerprint))
            && progress
                .iter()
                .all(|p| p.refused || board.is_fresh(&p.fingerprint))
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
            if board.is_fresh(&fingerprint) {
                // Watch it: translations may find it lost its place.
                if self.pause(Duration::from_secs(1)).await {
                    return;
                }
                continue;
            }
            let was_through = board.is_through(&fingerprint);
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
            if matches!(result, Ok(Ok(_))) {
                board.answered(&fingerprint);
            }
            // An answer counts even when it held no translation.
            let through = board.is_fresh(&fingerprint);
            misses = if through { 0 } else { misses.saturating_add(1) };
            let gap_max = if self.unattended && !board.used_within(ATTENDED_WITHIN) {
                UNATTENDED_GAP_MAX
            } else {
                PROBE_GAP
            };
            let gap = PROBE_GAP
                .saturating_mul(1 << misses.saturating_sub(1).min(5))
                .min(gap_max);
            let wait = {
                let mut progress = self.lock();
                let p = &mut progress[index];
                p.trying = false;
                p.tries += 1;
                match &result {
                    Ok(Ok(_)) => {
                        p.last_error = None;
                        gap
                    }
                    Ok(Err(err)) => {
                        p.last_error = Some(err.to_string());
                        p.refused = !through && !worth_another_try(err);
                        if p.refused {
                            board.give_up(&fingerprint);
                        }
                        err.retry_after()
                            .map_or(gap, |d| d.clamp(gap, PROBE_TIMEOUT))
                    }
                    Err(_) => {
                        p.last_error = Some(format!(
                            "no answer within {} seconds",
                            PROBE_TIMEOUT.as_secs()
                        ));
                        gap
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
    fn a_key_given_up_on_is_so_until_answered_or_the_keys_change() {
        let board = SqueezeBoard::new();
        board.give_up("a");
        board.give_up("b");
        assert!(board.gave_up("a") && board.gave_up("b"));
        board.answered("a");
        assert!(!board.gave_up("a"));
        board.clear();
        assert!(!board.gave_up("b"));
    }
}
