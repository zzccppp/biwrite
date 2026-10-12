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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use biwrite_engine::{TranslateError, TranslationRequest, Translator};
use serde::Serialize;
use tokio::sync::watch;

use crate::config::ProviderConfig;
use crate::keys::fingerprint;
use crate::observe::NoObserver;
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
/// Pause between two tries of one key.
const PROBE_GAP: Duration = Duration::from_secs(3);
/// Longest wait for one try, and for a rate limit between tries.
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);
/// What a try translates.
pub const PROBE_TEXT: &str = "Good morning.";

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
    /// some time since the board was made.
    crowded_seen: AtomicBool,
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
        if turned.len() >= CROWDED_AFTER
            && turned.front().is_some_and(|t| t.elapsed() < STALE_AFTER)
        {
            self.crowded_seen.store(true, Ordering::Release);
        }
    }

    /// The relay turned keys away as often as [`Self::crowded`] asks at some
    /// time, whether or not crowding applies to this board: its keys that
    /// got through are worth keeping alive.
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
/// `board` as for any request.
pub async fn ping(
    config: &ProviderConfig,
    key: String,
    board: Arc<SqueezeBoard>,
    prompts: Arc<dyn PromptSource>,
) -> Result<(), TranslateError> {
    let provider = HttpProvider::new(
        config.clone(),
        Arc::new(move || Ok(vec![key.clone()])),
        prompts,
        Arc::new(NoObserver),
    )?;
    provider.join_board(board);
    let request = TranslationRequest {
        source: PROBE_TEXT.to_owned(),
        ..Default::default()
    };
    match tokio::time::timeout(
        PROBE_TIMEOUT,
        Translator::translate(&provider, &request, &|_| {}),
    )
    .await
    {
        Ok(result) => result.map(|_| ()),
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
}

impl Squeeze {
    /// Start trying `keys` of the provider `config` on `runtime`. Keys on
    /// `board` that were answered lately are left alone while they stay
    /// so, and the board lets keys that got through go first from now on.
    /// `on_change` is called from the squeeze's tasks.
    pub fn start(
        config: &ProviderConfig,
        keys: Vec<String>,
        board: Arc<SqueezeBoard>,
        prompts: Arc<dyn PromptSource>,
        on_change: ChangeFn,
        runtime: &tokio::runtime::Handle,
    ) -> Result<Arc<Self>, TranslateError> {
        let mut providers = Vec::with_capacity(keys.len());
        for key in &keys {
            let key = key.clone();
            let provider = HttpProvider::new(
                config.clone(),
                Arc::new(move || Ok(vec![key.clone()])),
                Arc::clone(&prompts),
                Arc::new(NoObserver),
            )?;
            provider.join_board(Arc::clone(&board));
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
        let request = TranslationRequest {
            source: PROBE_TEXT.to_owned(),
            ..Default::default()
        };
        let mut stop = self.stop.subscribe();
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
            let attempt = tokio::time::timeout(
                PROBE_TIMEOUT,
                Translator::translate(&provider, &request, &|_| {}),
            );
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
            let wait = {
                let mut progress = self.lock();
                let p = &mut progress[index];
                p.trying = false;
                p.tries += 1;
                match &result {
                    Ok(Ok(_)) => {
                        p.last_error = None;
                        PROBE_GAP
                    }
                    Ok(Err(err)) => {
                        p.last_error = Some(err.to_string());
                        p.refused = !through && !worth_another_try(err);
                        err.retry_after()
                            .map_or(PROBE_GAP, |d| d.clamp(PROBE_GAP, PROBE_TIMEOUT))
                    }
                    Err(_) => {
                        p.last_error = Some(format!(
                            "no answer within {} seconds",
                            PROBE_TIMEOUT.as_secs()
                        ));
                        PROBE_GAP
                    }
                }
            };
            if through {
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
            &TranslateError::RateLimited { retry_after: None }
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
    fn crowding_is_remembered_for_keeping_keys_alive_on_any_board() {
        let ordinary = SqueezeBoard::new();
        ordinary.note_turned_away();
        assert!(!ordinary.was_crowded());
        ordinary.note_turned_away();
        assert!(
            ordinary.was_crowded(),
            "seen, though keys taking turns stay so"
        );
        assert!(!ordinary.crowded());
        assert_eq!(ordinary.idle("a"), None);
        ordinary.answered("a");
        assert!(
            ordinary
                .idle("a")
                .is_some_and(|d| d < Duration::from_secs(1))
        );
    }
}
