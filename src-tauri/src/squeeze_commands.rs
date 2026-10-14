//! Getting through to a crowded relay (see `biwrite_providers::Squeeze`).
//!
//! Each provider for which it applies (a crowded relay, or several keys) has
//! a board of the keys that got through. Every provider built for it joins
//! the board, so translation, the writing assistant and key tests send to
//! those keys first, and a switch to another provider and back keeps it. A
//! squeeze started from the key pool runs in the background whichever
//! provider is in use, and says when a key gets through. With keeping alive
//! on, a keeper sends the keys that got through a short translation when
//! they are idle, for every provider whose relay turned keys away (a crowded
//! relay, or a pool a squeeze was started for), and squeezes a key it kept
//! back in when the key loses its place, unless a squeeze gave up on it. It
//! does so however long the author is away (keeping alive forever, the
//! default, which squeezes back in every few seconds as a squeeze the author
//! started does), or only for a provider used within [`KEEP_ALIVE_FOR`].
//! The keys kept are noted in the settings, and after a restart or an
//! update they are squeezed back in. The app's log has a line for each key
//! that gets through, each key given up on, what the relay said when it
//! turned a key away (once per key while the words stay the same, at most
//! every few minutes), and the end of each squeeze.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use biwrite_providers::{
    CROWDED_AFTER, ChangeFn, HttpProvider, KEEP_ALIVE_AFTER, KEEP_ALIVE_FOR, KeyProgress, Squeeze,
    SqueezeBoard, SqueezeChange, jitter, key_fingerprint, ping,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, UserAttentionType};

use crate::error::{CommandError, CommandResult};
use crate::secrets;
use crate::settings::ProviderEntry;
use crate::settings_commands::{blocking, not_found};
use crate::state::AppState;

pub const EVENT_SQUEEZE: &str = "squeeze";

/// How often the keeper looks for keys to keep alive.
const KEEP_ALIVE_TICK: Duration = Duration::from_secs(10);
/// A key whose ping failed is pinged again after this long, or after as
/// long as the server asks, up to [`PING_WAIT_MAX`]. A key that got through
/// fails now and then, and a retry soon keeps it before it counts as stale.
const PING_RETRY: Duration = Duration::from_secs(30);
const PING_WAIT_MAX: Duration = Duration::from_secs(10 * 60);
/// The keeper's first round after a start, soon: keys kept before a restart
/// are squeezed back in.
const FIRST_ROUND_AFTER: Duration = Duration::from_secs(5);
/// Updating from a version that did not note the keys kept alive, the keys
/// of a crowded relay answered this lately are squeezed back in (a key
/// through all along is noted once an hour, see [`THROUGH_NOTE_EVERY`]).
const ANSWERED_LATELY: u64 = 90 * 60 * 1000;
/// A key turned away with the same words is logged again after this long.
const TURNED_AWAY_LOG_EVERY: Duration = Duration::from_secs(5 * 60);

/// Boards and running squeezes, by provider id.
#[derive(Default)]
pub struct SqueezeState {
    /// Each board with the target it was made for
    /// (`ProviderEntry::squeeze_target`).
    boards: Mutex<HashMap<String, (String, Arc<SqueezeBoard>)>>,
    runs: Mutex<HashMap<String, Arc<Squeeze>>>,
    /// The keys of a provider as last read, for the keeper (`None`: they
    /// could not be read, and are not read again until they change).
    keys: Mutex<HashMap<String, Option<Vec<String>>>>,
    /// Keys whose ping failed, by provider id and fingerprint, with the
    /// time before which they are not pinged again.
    ping_after: Mutex<HashMap<(String, String), Instant>>,
    /// When each key's latest keep-alive was answered (Unix milliseconds),
    /// by provider id and fingerprint.
    kept_at: Mutex<HashMap<(String, String), u64>>,
    /// Keys kept alive, by provider id: those that got through while
    /// keeping alive was on. One that loses its place is squeezed back in.
    kept: Mutex<HashMap<String, HashSet<String>>>,
    /// Providers whose running squeeze the keeper started.
    auto: Mutex<HashSet<String>>,
    /// Keys are kept alive however long the author is away (the setting
    /// `keep_alive_forever`), not only within [`KEEP_ALIVE_FOR`].
    forever: AtomicBool,
    /// Mean milliseconds between two keep-alives of a key, and between two
    /// tries of a key in a squeeze (the settings); zero before they are set.
    keep_mean_ms: AtomicU64,
    pace_ms: AtomicU64,
    /// How long each key waits idle before its next keep-alive, drawn
    /// around the mean once per keep-alive, by provider id and fingerprint.
    ping_every: Mutex<HashMap<(String, String), Duration>>,
}

impl SqueezeState {
    /// The mean wait between two keep-alives of a key (minutes) and between
    /// two tries of a key in a squeeze (seconds), as the settings say.
    pub fn set_means(&self, keep_alive_minutes: f64, squeeze_seconds: f64) {
        let keep = (keep_alive_minutes.clamp(1.0, 30.0) * 60_000.0) as u64;
        if self.keep_mean_ms.swap(keep, Ordering::Relaxed) != keep {
            // Drawn again around the new mean.
            self.ping_every
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        }
        let pace = (squeeze_seconds.clamp(1.0, 120.0) * 1000.0) as u64;
        if self.pace_ms.swap(pace, Ordering::Relaxed) != pace {
            for (_, board) in self.boards().values() {
                board.set_pace(Some(Duration::from_millis(pace)));
            }
        }
    }

    /// The mean pause between two tries of a key in a squeeze.
    fn pace(&self) -> Option<Duration> {
        match self.pace_ms.load(Ordering::Relaxed) {
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        }
    }

    /// How long key `fingerprint` of provider `id` waits idle before its
    /// next keep-alive: drawn within a quarter of the mean either way, the
    /// same until it is answered.
    fn keep_alive_after(&self, id: &str, fingerprint: &str) -> Duration {
        let mean = match self.keep_mean_ms.load(Ordering::Relaxed) {
            0 => KEEP_ALIVE_AFTER,
            ms => Duration::from_millis(ms),
        };
        *self
            .ping_every
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry((id.to_owned(), fingerprint.to_owned()))
            .or_insert_with(|| jitter(mean, 0.25))
    }

    /// The author set key `fingerprint` of provider `id` aside: it is
    /// through no more, not kept alive, and a squeeze running tries it no
    /// more. The other keys keep their places.
    pub fn set_aside(&self, id: &str, fingerprint: &str) {
        if let Some((_, board)) = self.boards().get(id) {
            board.forget(fingerprint);
        }
        if let Some(kept) = self.kept().get_mut(id) {
            kept.remove(fingerprint);
        }
        if let Some(run) = self.runs().get(id) {
            run.set_aside(fingerprint);
        }
        self.keys().remove(id);
        let key = (id.to_owned(), fingerprint.to_owned());
        self.ping_after().remove(&key);
        self.ping_every
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
    }

    /// The author took key `fingerprint` back: read with the others again.
    pub fn take_back(&self, id: &str) {
        self.keys().remove(id);
    }

    /// Keep keys alive however long the author is away, or not.
    pub fn set_forever(&self, on: bool) {
        self.forever.store(on, Ordering::Relaxed);
    }

    /// The keys of `board` are kept alive now: always while keeping alive
    /// forever, else when the author used them within [`KEEP_ALIVE_FOR`].
    fn in_use(&self, board: &SqueezeBoard) -> bool {
        self.forever() || board.used_within(KEEP_ALIVE_FOR)
    }

    fn forever(&self) -> bool {
        self.forever.load(Ordering::Relaxed)
    }

    /// The keys kept alive, by provider id (fingerprints, in order), for
    /// the settings.
    fn kept_keys(&self) -> BTreeMap<String, Vec<String>> {
        self.kept()
            .iter()
            .filter(|(_, keys)| !keys.is_empty())
            .map(|(id, keys)| {
                let mut keys: Vec<String> = keys.iter().cloned().collect();
                keys.sort();
                (id.clone(), keys)
            })
            .collect()
    }

    /// After a restart, the keys kept alive before it (`kept`, by provider
    /// id) are kept again for providers among `entries`, so the keeper
    /// finds them gone and squeezes them back in. Their relay counts as one
    /// that turned keys away, and as used now. The number of keys.
    pub fn restore_kept(
        &self,
        entries: &[ProviderEntry],
        kept: &BTreeMap<String, Vec<String>>,
    ) -> usize {
        let mut restored = 0;
        for e in entries.iter().filter(|e| e.squeezable()) {
            let Some(prints) = kept.get(&e.config.id).filter(|p| !p.is_empty()) else {
                continue;
            };
            let Some(board) = self.board_for(e) else {
                continue;
            };
            board.allow_crowding();
            for _ in 0..CROWDED_AFTER {
                board.note_turned_away();
            }
            board.note_use();
            self.kept()
                .entry(e.config.id.clone())
                .or_default()
                .extend(prints.iter().cloned());
            restored += prints.len();
        }
        restored
    }

    fn boards(&self) -> MutexGuard<'_, HashMap<String, (String, Arc<SqueezeBoard>)>> {
        self.boards.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn runs(&self) -> MutexGuard<'_, HashMap<String, Arc<Squeeze>>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn keys(&self) -> MutexGuard<'_, HashMap<String, Option<Vec<String>>>> {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn ping_after(&self) -> MutexGuard<'_, HashMap<(String, String), Instant>> {
        self.ping_after
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn kept(&self) -> MutexGuard<'_, HashMap<String, HashSet<String>>> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn auto(&self) -> MutexGuard<'_, HashSet<String>> {
        self.auto.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The board of provider `entry` as it is, if any (none is made).
    fn board_of(&self, entry: &ProviderEntry) -> Option<Arc<SqueezeBoard>> {
        self.boards()
            .get(&entry.config.id)
            .filter(|(made_for, _)| *made_for == entry.squeeze_target())
            .map(|(_, b)| Arc::clone(b))
    }

    /// Keys kept alive that lost their place since, by provider among
    /// `entries` with no squeeze running and used lately (their
    /// fingerprints). The keys through now join those kept, for every relay
    /// that turned keys away; a key a squeeze gave up on leaves them.
    pub fn dropped_keys(
        &self,
        entries: &[ProviderEntry],
    ) -> Vec<(ProviderEntry, Arc<SqueezeBoard>, Vec<String>)> {
        entries
            .iter()
            .filter(|e| e.squeezable())
            .filter_map(|e| {
                let board = self.board_of(e)?;
                if !board.was_crowded() {
                    return None;
                }
                let through: HashSet<String> =
                    board.through().into_iter().map(|t| t.fingerprint).collect();
                let mut dropped: Vec<String> = {
                    let mut kept = self.kept();
                    let kept = kept.entry(e.config.id.clone()).or_default();
                    kept.extend(through.iter().cloned());
                    kept.retain(|f| !board.gave_up(f));
                    kept.iter()
                        .filter(|f| !through.contains(*f))
                        .cloned()
                        .collect()
                };
                if dropped.is_empty()
                    || !self.in_use(&board)
                    || self.runs().get(&e.config.id).is_some_and(|r| r.running())
                {
                    return None;
                }
                dropped.sort();
                Some((e.clone(), board, dropped))
            })
            .collect()
    }

    /// The author stopped the squeeze of provider `id`: keys that lost their
    /// place are not squeezed back in (those through now are kept again).
    pub fn stop_squeezing_back(&self, id: &str) {
        self.kept().remove(id);
        self.auto().remove(id);
    }

    /// The squeezes the keeper started for providers among `entries` not
    /// used within [`KEEP_ALIVE_FOR`] stop. Their keys stay kept, so they
    /// are squeezed back in once the provider is used again.
    pub fn stop_unused_runs(&self, entries: &[ProviderEntry]) {
        // Not held while the boards are looked at (they come first).
        let auto: Vec<String> = self.auto().iter().cloned().collect();
        let unused: Vec<String> = auto
            .into_iter()
            .filter(|id| {
                entries
                    .iter()
                    .find(|e| e.config.id == **id)
                    .and_then(|e| self.board_of(e))
                    .is_none_or(|b| !self.in_use(&b))
            })
            .collect();
        for id in unused {
            self.auto().remove(&id);
            if let Some(run) = self.runs().get(&id) {
                run.stop();
            }
        }
    }

    /// Keeping alive was turned off: the squeezes the keeper started stop,
    /// and the keys it kept are forgotten.
    pub fn keep_alive_off(&self) {
        self.kept().clear();
        let ids: Vec<String> = self.auto().drain().collect();
        let runs = self.runs();
        for id in ids {
            if let Some(run) = runs.get(&id) {
                run.stop();
            }
        }
    }

    /// The keys of provider `id` if read since they last changed: `Some(None)`
    /// if they could not be read.
    fn cached_keys(&self, id: &str) -> Option<Option<Vec<String>>> {
        self.keys().get(id).cloned()
    }

    fn cache_keys(&self, id: &str, keys: Option<Vec<String>>) {
        self.keys().insert(id.to_owned(), keys);
    }

    /// Provider `id` still has the key with this fingerprint, as last read.
    fn has_key(&self, id: &str, fingerprint: &str) -> bool {
        self.keys()
            .get(id)
            .and_then(Option::as_ref)
            .is_some_and(|keys| keys.iter().any(|k| key_fingerprint(k) == fingerprint))
    }

    /// A ping of key `fingerprint` of provider `id` ended: answered (`None`),
    /// or failed and not to be sent again for `wait`.
    fn pinged(&self, id: &str, fingerprint: &str, wait: Option<Duration>) {
        let key = (id.to_owned(), fingerprint.to_owned());
        match wait {
            None => {
                self.ping_after().remove(&key);
                // The next wait is drawn anew.
                self.ping_every
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&key);
                self.kept_at
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(key, now_ms());
            }
            Some(wait) => {
                self.ping_after().insert(key, Instant::now() + wait);
            }
        }
    }

    fn may_ping(&self, id: &str, fingerprint: &str) -> bool {
        self.ping_after()
            .get(&(id.to_owned(), fingerprint.to_owned()))
            .is_none_or(|t| *t <= Instant::now())
    }

    /// The providers among `entries` with keys to keep alive: a relay that
    /// turned keys away and that the author used within [`KEEP_ALIVE_FOR`],
    /// and keys of it that got through and were not answered for `after`,
    /// or else for the wait drawn for each (their fingerprints).
    pub fn due_for_keep_alive(
        &self,
        entries: &[ProviderEntry],
        after: Option<Duration>,
    ) -> Vec<(ProviderEntry, Arc<SqueezeBoard>, Vec<String>)> {
        entries
            .iter()
            .filter(|e| e.squeezable())
            .filter_map(|e| {
                let board = self
                    .boards()
                    .get(&e.config.id)
                    .filter(|(made_for, _)| *made_for == e.squeeze_target())
                    .map(|(_, b)| Arc::clone(b))?;
                if !board.was_crowded() || !self.in_use(&board) {
                    return None;
                }
                let due: Vec<String> = board
                    .through()
                    .into_iter()
                    .map(|t| t.fingerprint)
                    .filter(|f| {
                        let after = after.unwrap_or_else(|| self.keep_alive_after(&e.config.id, f));
                        board.idle(f).is_some_and(|idle| idle >= after)
                    })
                    .filter(|f| self.may_ping(&e.config.id, f))
                    .collect();
                (!due.is_empty()).then(|| (e.clone(), board, due))
            })
            .collect()
    }

    /// The board of provider `entry`, if getting through applies to it. A
    /// provider moved to another host, model or API gets a new board, and
    /// its squeeze stops.
    pub fn board_for(&self, entry: &ProviderEntry) -> Option<Arc<SqueezeBoard>> {
        let id = &entry.config.id;
        if !entry.squeezable() {
            self.forget(id);
            return None;
        }
        let target = entry.squeeze_target();
        let mut boards = self.boards();
        if let Some((made_for, board)) = boards.get(id)
            && *made_for == target
        {
            return Some(Arc::clone(board));
        }
        if let Some(run) = self.runs().remove(id) {
            run.stop();
        }
        self.keys().remove(id);
        self.ping_after().retain(|(owner, _), _| owner != id);
        self.kept().remove(id);
        self.auto().remove(id);
        let board = if entry.crowded_relay() {
            SqueezeBoard::for_relay()
        } else {
            SqueezeBoard::new()
        };
        boards.insert(id.clone(), (target, Arc::clone(&board)));
        Some(board)
    }

    /// The keys of provider `id` changed: its squeeze, which tries the keys
    /// it read, stops, and no key counts as through until it is answered
    /// again (a removed key would count otherwise).
    pub fn keys_changed(&self, id: &str) {
        if let Some((_, board)) = self.boards().get(id) {
            board.clear();
        }
        self.keys().remove(id);
        self.ping_after().retain(|(owner, _), _| owner != id);
        self.kept().remove(id);
        self.auto().remove(id);
        if let Some(run) = self.runs().remove(id) {
            run.stop();
        }
    }

    /// `http`, built for `entry`, sends to the keys that got through first.
    pub fn join(&self, entry: &ProviderEntry, http: &HttpProvider) {
        if let Some(board) = self.board_for(entry) {
            http.join_board(board);
        }
    }

    /// Provider `id` was removed: its squeeze stops and its board goes.
    pub fn forget(&self, id: &str) {
        self.boards().remove(id);
        self.keys().remove(id);
        self.ping_after().retain(|(owner, _), _| owner != id);
        self.kept().remove(id);
        self.auto().remove(id);
        if let Some(run) = self.runs().remove(id) {
            run.stop();
        }
    }
}

/// Now, in Unix milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// One key of a provider, for the key pool.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqueezeKeyView {
    pub fingerprint: String,
    /// When it got through (Unix milliseconds), if it counts as through.
    pub through_since: Option<u64>,
    /// When it was last answered (Unix milliseconds), if through.
    pub last_ok: Option<u64>,
    /// When its latest keep-alive was answered (Unix milliseconds), since
    /// it got through.
    pub kept_at: Option<u64>,
    /// Its latest keep-alive failed, and it is tried again shortly.
    pub keep_retrying: bool,
    pub tries: u32,
    pub trying: bool,
    pub refused: bool,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqueezeView {
    pub running: bool,
    /// The relay turned keys away lately, so keys that got through go first.
    pub crowded: bool,
    /// When the latest squeeze started (Unix milliseconds).
    pub started: Option<u64>,
    /// The keys the board or the latest squeeze knows about.
    pub keys: Vec<SqueezeKeyView>,
}

/// A change, for the status bar and the notices.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqueezeEvent {
    pub provider_id: String,
    pub provider_name: String,
    #[serde(flatten)]
    pub change: SqueezeChange,
    pub running: bool,
    /// Keys through, of `total`.
    pub through: usize,
    pub total: usize,
    /// Keeping alive is on and the relay turned keys away: the keys that
    /// got through are kept alive.
    pub keeping: bool,
}

fn view(state: &SqueezeState, id: &str) -> SqueezeView {
    let board = state.boards().get(id).map(|(_, b)| Arc::clone(b));
    let run = state.runs().get(id).cloned();
    let through = board.as_ref().map(|b| b.through()).unwrap_or_default();
    let progress: Vec<KeyProgress> = run.as_ref().map(|r| r.progress()).unwrap_or_default();
    let mut keys: Vec<SqueezeKeyView> = progress
        .iter()
        .map(|p| SqueezeKeyView {
            fingerprint: p.fingerprint.clone(),
            tries: p.tries,
            trying: p.trying,
            refused: p.refused,
            last_error: p.last_error.clone(),
            ..SqueezeKeyView::default()
        })
        .collect();
    for t in through {
        if !keys.iter().any(|k| k.fingerprint == t.fingerprint) {
            keys.push(SqueezeKeyView {
                fingerprint: t.fingerprint.clone(),
                ..SqueezeKeyView::default()
            });
        }
        if let Some(key) = keys.iter_mut().find(|k| k.fingerprint == t.fingerprint) {
            let at = (id.to_owned(), t.fingerprint.clone());
            key.through_since = Some(t.since_ms);
            key.last_ok = Some(t.last_ok_ms);
            key.kept_at = state
                .kept_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&at)
                .copied()
                .filter(|kept| *kept >= t.since_ms);
            key.keep_retrying = state.ping_after().contains_key(&at);
        }
    }
    SqueezeView {
        crowded: board.as_ref().is_some_and(|b| b.crowded()),
        running: run.as_ref().is_some_and(|r| r.running()),
        started: run.map(|r| r.started_ms()),
        keys,
    }
}

/// How provider `id` is getting through: its keys that got through, and
/// the latest squeeze's tries.
#[tauri::command]
pub async fn squeeze_status(state: State<'_, AppState>, id: String) -> CommandResult<SqueezeView> {
    Ok(view(&state.squeeze, &id))
}

/// Try the keys of provider `id` that have not got through until all have,
/// in the background.
#[tauri::command]
pub async fn squeeze_start(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<SqueezeView> {
    // Keys and providers do not change while it starts.
    let _keys = state.keys_lock.lock().await;
    let entry = state
        .settings()
        .provider(&id)
        .cloned()
        .ok_or_else(|| not_found(&id))?;
    let board = state.squeeze.board_for(&entry).ok_or_else(|| {
        CommandError::Settings("getting through applies to a relay or a pool of keys".into())
    })?;
    // The author's own squeeze goes on; one the keeper started gives way to
    // this one, which tries every key as often as it can.
    let running = state.squeeze.runs().get(&id).is_some_and(|r| r.running());
    if running && !state.squeeze.auto().contains(&id) {
        return Ok(view(&state.squeeze, &id));
    }
    let store = state.secrets.clone();
    let account = id.clone();
    let vouched = entry.vouched();
    let keys = blocking(move || secrets::get_keys(store.as_ref(), &account, vouched)).await?;
    // Keys set aside are not squeezed.
    let keys = entry.usable(keys);
    state.squeeze.cache_keys(&id, Some(keys.clone()));
    if keys.is_empty() {
        return Err(CommandError::Settings(format!(
            "No API key for “{}”. Add one in Settings.",
            entry.config.name
        )));
    }
    // Asked for: the keys that get through are kept alive (so are those
    // getting through later, see `Squeeze::start`), even when no request
    // of the author's was turned away yet.
    board.note_use();
    board.note_squeezed();
    start_run(&app, &entry, board, &keys, None)?;
    Ok(view(&state.squeeze, &id))
}

/// Start a squeeze of provider `entry` with `keys`, its pool: of all of them,
/// or (`only`, by fingerprint) of the keys the keeper squeezes back in, which
/// does not ask for the author's attention.
fn start_run(
    app: &AppHandle,
    entry: &ProviderEntry,
    board: Arc<SqueezeBoard>,
    keys: &[String],
    only: Option<&[String]>,
) -> CommandResult<()> {
    let state = app.state::<AppState>();
    let id = entry.config.id.clone();
    let total = keys.len();
    // Tries paced around the mean the author chose.
    {
        let s = state.settings();
        state
            .squeeze
            .set_means(s.keep_alive_minutes, s.squeeze_seconds);
    }
    board.set_pace(state.squeeze.pace());
    let fingerprints: Vec<String> = keys.iter().map(|k| key_fingerprint(k)).collect();
    // The keys tried, with their numbers in the pool.
    let (numbers, tried): (Vec<usize>, Vec<String>) = keys
        .iter()
        .zip(&fingerprints)
        .enumerate()
        .filter(|(_, (_, f))| only.is_none_or(|only| only.contains(*f)))
        .map(|(i, (k, _))| (i + 1, k.clone()))
        .unzip();
    let auto = only.is_some();
    let unattended = backs_off(&state.squeeze, auto);
    let lines = Arc::new(SqueezeLog {
        name: entry.config.name.clone(),
        numbers: numbers.clone(),
        run: OnceLock::new(),
        said: Mutex::default(),
        given_up: Mutex::default(),
    });
    let on_change: ChangeFn = {
        let app = app.clone();
        let id = id.clone();
        let name = entry.config.name.clone();
        let board = Arc::clone(&board);
        let lines = Arc::clone(&lines);
        Arc::new(move |change: SqueezeChange| {
            let state = app.state::<AppState>();
            let running = !matches!(change, SqueezeChange::Ended { .. });
            let change = match change {
                SqueezeChange::Through { number } => SqueezeChange::Through {
                    number: numbers.get(number - 1).copied().unwrap_or(number),
                },
                other => other,
            };
            if let SqueezeChange::Through { .. } = change {
                // Paragraphs that failed while nothing got through go again.
                if state.settings().active_provider == id {
                    state.engine.retry_failed();
                }
                if !auto && let Some(window) = app.get_webview_window("main") {
                    // A window hidden to the tray has no taskbar button to
                    // flash (the Dock icon of macOS stays).
                    #[cfg(not(target_os = "macos"))]
                    if !window.is_visible().unwrap_or(true) {
                        let _ = window.show();
                        let _ = window.minimize();
                    }
                    let _ = window.request_user_attention(Some(UserAttentionType::Critical));
                }
            }
            let through = fingerprints.iter().filter(|f| board.is_through(f)).count();
            lines.note(&change, through, total);
            let event = SqueezeEvent {
                provider_id: id.clone(),
                provider_name: name.clone(),
                through,
                total,
                running,
                keeping: state.settings().keep_alive && keeps(&state.squeeze, &board),
                change,
            };
            if let Err(e) = app.emit(EVENT_SQUEEZE, event) {
                log::warn!("could not emit {EVENT_SQUEEZE}: {e}");
            }
        })
    };
    let run = Squeeze::start(
        &entry.config,
        tried,
        board,
        state.prompts(),
        Arc::clone(&on_change),
        tauri::async_runtime::handle().inner(),
        unattended,
    )?;
    let _ = lines.run.set(Arc::downgrade(&run));
    on_change(SqueezeChange::Started);
    if auto {
        log::info!(
            "squeezing keys {} of {} back in{}",
            numbered(&lines.numbers),
            entry.config.name,
            if unattended {
                ", more slowly while unused"
            } else {
                ""
            }
        );
        state.squeeze.auto().insert(id.clone());
    } else {
        log::info!("squeeze started for {}", entry.config.name);
        state.squeeze.auto().remove(&id);
    }
    if let Some(old) = state.squeeze.runs().insert(id, run) {
        old.stop();
    }
    Ok(())
}

/// Stop the squeeze of provider `id`. The keys that got through stay, and
/// keys that lost their place are not squeezed back in.
#[tauri::command]
pub async fn squeeze_stop(state: State<'_, AppState>, id: String) -> CommandResult<SqueezeView> {
    if let Some(run) = state.squeeze.runs().get(&id) {
        run.stop();
    }
    state.squeeze.stop_squeezing_back(&id);
    Ok(view(&state.squeeze, &id))
}

/// A squeeze backs off while the author is away: one the keeper started
/// (`auto`), unless keeping alive forever. Squeezing back in while keeping
/// alive forever tries every few seconds, as a squeeze the author started
/// does.
fn backs_off(squeeze: &SqueezeState, auto: bool) -> bool {
    auto && !squeeze.forever()
}

/// Lines in the app's log about a squeeze (see the module).
struct SqueezeLog {
    name: String,
    /// The pool numbers of the keys tried, in their order.
    numbers: Vec<usize>,
    run: OnceLock<Weak<Squeeze>>,
    /// Per key tried (by position): the relay's words last logged, and when.
    said: Mutex<HashMap<usize, (String, Instant)>>,
    /// Keys tried that were given up on, logged once.
    given_up: Mutex<HashSet<usize>>,
}

impl SqueezeLog {
    fn note(&self, change: &SqueezeChange, through: usize, total: usize) {
        let Some(run) = self.run.get().and_then(Weak::upgrade) else {
            return;
        };
        let progress = run.progress();
        match change {
            SqueezeChange::Through { number } => {
                let tries = self
                    .numbers
                    .iter()
                    .position(|n| n == number)
                    .and_then(|i| progress.get(i))
                    .map_or(0, |p| p.tries);
                log::info!(
                    "squeeze of {}: key {number} got through after {tries} tries ({through} of {total} through)",
                    self.name
                );
            }
            SqueezeChange::Tried => self.turned_away(&progress),
            SqueezeChange::Ended { .. } => {
                self.turned_away(&progress);
                let tries: u32 = progress.iter().map(|p| p.tries).sum();
                log::info!(
                    "squeeze of {} ended: {through} of {total} keys through, {tries} tries",
                    self.name
                );
            }
            SqueezeChange::Started | SqueezeChange::Kept => {}
        }
    }

    /// What the relay said to keys it turned away, and keys given up on.
    fn turned_away(&self, progress: &[KeyProgress]) {
        for (i, p) in progress.iter().enumerate() {
            let number = self.numbers.get(i).copied().unwrap_or(i + 1);
            let Some(error) = &p.last_error else {
                continue;
            };
            if p.refused {
                if self
                    .given_up
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(i)
                {
                    log::info!(
                        "squeeze of {}: gave up on key {number} after {} tries: {error}",
                        self.name,
                        p.tries
                    );
                }
                continue;
            }
            let words = without_request_id(error);
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            let due = said
                .get(&i)
                .is_none_or(|(w, at)| *w != words || at.elapsed() >= TURNED_AWAY_LOG_EVERY);
            if due {
                said.insert(i, (words, Instant::now()));
                log::info!(
                    "squeeze of {}: key {number} turned away, {} tries so far: {error}",
                    self.name,
                    p.tries
                );
            }
        }
    }
}

/// `words` without a relay's request id, which differs every time.
fn without_request_id(words: &str) -> String {
    let Some(at) = words.find("(request id:") else {
        return words.to_owned();
    };
    let end = words[at..].find(')').map_or(words.len(), |e| at + e + 1);
    format!("{}{}", words[..at].trim_end(), &words[end..])
}

/// Pool numbers as a list, "1, 3, 5".
fn numbered(numbers: &[usize]) -> String {
    numbers
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Keep the keys that got through alive, every [`KEEP_ALIVE_TICK`], while
/// the setting is on, and note when each key got through.
pub fn spawn_keeper(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        restore_kept(&app.state::<AppState>());
        let mut wait = FIRST_ROUND_AFTER;
        loop {
            tokio::time::sleep(wait).await;
            wait = KEEP_ALIVE_TICK;
            record_through(&app.state::<AppState>());
            keep_alive(&app).await;
        }
    });
}

/// Least time between two notes of the same key's getting through, so
/// the settings file is not written each round.
const THROUGH_NOTE_EVERY: u64 = 60 * 60 * 1000;

/// Note in the settings when each key of a relay that turns keys away was
/// last answered, so the pool shows the keys that never got through, after
/// a restart too.
/// The keys kept alive before a restart (in the settings) are kept again,
/// while keeping alive is on. Settings of a version that did not note them
/// give the keys of a crowded relay answered lately instead.
fn restore_kept(state: &AppState) {
    let (entries, kept) = {
        let s = state.settings();
        if !s.keep_alive {
            return;
        }
        state.squeeze.set_forever(s.keep_alive_forever);
        let kept = s
            .kept_keys
            .clone()
            .unwrap_or_else(|| answered_lately(&s.providers, now_ms()));
        if kept.is_empty() {
            return;
        }
        (s.providers.clone(), kept)
    };
    let restored = state.squeeze.restore_kept(&entries, &kept);
    if restored > 0 {
        log::info!("{restored} keys kept alive before the restart are squeezed back in");
    }
}

/// The keys of each crowded relay among `entries` noted as answered within
/// [`ANSWERED_LATELY`] of `now` (Unix milliseconds), by provider id.
fn answered_lately(entries: &[ProviderEntry], now: u64) -> BTreeMap<String, Vec<String>> {
    entries
        .iter()
        .filter(|e| e.squeezable() && e.crowded_relay())
        .filter_map(|e| {
            let keys: Vec<String> = e
                .key_through
                .iter()
                .filter(|(_, at)| now.saturating_sub(**at) <= ANSWERED_LATELY)
                .map(|(f, _)| f.clone())
                .collect();
            (!keys.is_empty()).then(|| (e.config.id.clone(), keys))
        })
        .collect()
}

pub(crate) fn record_through(state: &AppState) {
    let through: Vec<(String, Vec<(String, u64)>)> = state
        .squeeze
        .boards()
        .iter()
        .map(|(id, (_, board))| {
            let keys = board
                .through()
                .into_iter()
                .map(|t| (t.fingerprint, t.last_ok_ms))
                .collect::<Vec<_>>();
            (id.clone(), keys)
        })
        .filter(|(_, t)| !t.is_empty())
        .collect();
    let kept = Some(state.squeeze.kept_keys());
    let mut s = state.settings();
    // The keys kept alive, squeezed back in after a restart.
    let mut changed = s.kept_keys != kept;
    if changed {
        s.kept_keys = kept;
    }
    for (id, keys) in through {
        let Some(entry) = s.provider_mut(&id) else {
            continue;
        };
        for (fingerprint, last_ok) in keys {
            let noted = entry.key_through.get(&fingerprint).copied();
            if noted.is_none_or(|n| last_ok >= n + THROUGH_NOTE_EVERY) {
                entry.key_through.insert(fingerprint, last_ok);
                changed = true;
            }
        }
    }
    // Settings that could not be read are not written over.
    if changed
        && !s.keep_file
        && let Err(e) = state.persist(&s)
    {
        log::warn!("could not note the keys that got through or are kept alive: {e}");
    }
}

/// The keys of provider `entry`, as read since they last changed. `None` if
/// they cannot be read: not read again until they change, so the keychain
/// does not ask every round.
async fn keys_of(state: &AppState, entry: &ProviderEntry) -> Option<Vec<String>> {
    let id = &entry.config.id;
    match state.squeeze.cached_keys(id) {
        Some(keys) => keys,
        None => {
            let store = state.secrets.clone();
            let account = id.clone();
            let vouched = entry.vouched();
            match blocking(move || secrets::get_keys(store.as_ref(), &account, vouched)).await {
                Ok(keys) => {
                    // Keys set aside are not kept alive.
                    let keys = entry.usable(keys);
                    state.squeeze.cache_keys(id, Some(keys.clone()));
                    Some(keys)
                }
                Err(e) => {
                    state.squeeze.cache_keys(id, None);
                    log::warn!(
                        "keep-alive: the keys of {} could not be read: {e}",
                        entry.config.name
                    );
                    None
                }
            }
        }
    }
}

/// One round of keeping alive: keys that lost their place are squeezed back
/// in, each key due for it (see `due_for_keep_alive`) is sent a short
/// translation, all at once, and the frontend hears how many keys are kept.
async fn keep_alive(app: &AppHandle) {
    let state = app.state::<AppState>();
    let entries = {
        let s = state.settings();
        if !s.keep_alive {
            return;
        }
        state.squeeze.set_forever(s.keep_alive_forever);
        state
            .squeeze
            .set_means(s.keep_alive_minutes, s.squeeze_seconds);
        s.providers.clone()
    };
    state.squeeze.stop_unused_runs(&entries);
    for (entry, board, dropped) in state.squeeze.dropped_keys(&entries) {
        // Keys and providers do not change while it starts.
        let _keys = state.keys_lock.lock().await;
        let unchanged = state
            .settings()
            .provider(&entry.config.id)
            .is_some_and(|e| e.squeeze_target() == entry.squeeze_target());
        let Some(keys) = keys_of(&state, &entry).await.filter(|_| unchanged) else {
            continue;
        };
        if let Err(e) = start_run(app, &entry, board, &keys, Some(&dropped)) {
            log::warn!(
                "could not squeeze keys of {} back in: {e}",
                entry.config.name
            );
        }
    }
    let mut pings = tokio::task::JoinSet::new();
    for (entry, board, due) in state.squeeze.due_for_keep_alive(&entries, None) {
        let Some(keys) = keys_of(&state, &entry).await else {
            continue;
        };
        for (index, key) in keys.into_iter().enumerate() {
            let print = key_fingerprint(&key);
            if !due.contains(&print) {
                continue;
            }
            let (app, entry, board) = (app.clone(), entry.clone(), Arc::clone(&board));
            pings.spawn(async move { ping_one(&app, &entry, index, key, &print, board).await });
        }
    }
    while pings.join_next().await.is_some() {}
    announce_kept(app, &entries, true);
}

/// The keys of `board` that got through are kept alive (while the setting
/// is on): its relay turned keys away, and keeping alive goes on forever or
/// the author used it lately.
fn keeps(squeeze: &SqueezeState, board: &SqueezeBoard) -> bool {
    board.was_crowded() && squeeze.in_use(board)
}

/// Tell the frontend how many keys of each relay that turned keys away are
/// kept alive (`on`), or that none are any more.
fn announce_kept(app: &AppHandle, entries: &[ProviderEntry], on: bool) {
    let state = app.state::<AppState>();
    for entry in entries.iter().filter(|e| e.squeezable()) {
        let id = &entry.config.id;
        let Some(board) = state.squeeze.board_of(entry) else {
            continue;
        };
        if !board.was_crowded() {
            continue;
        }
        let event = SqueezeEvent {
            provider_id: id.clone(),
            provider_name: entry.config.name.clone(),
            change: SqueezeChange::Kept,
            running: state.squeeze.runs().get(id).is_some_and(|r| r.running()),
            through: board.through().len(),
            total: entry.key_count,
            keeping: on && keeps(&state.squeeze, &board),
        };
        if let Err(e) = app.emit(EVENT_SQUEEZE, event) {
            log::warn!("could not emit {EVENT_SQUEEZE}: {e}");
        }
    }
}

/// Keeping alive was turned on or off.
pub fn keep_alive_changed(app: &AppHandle, on: bool) {
    let state = app.state::<AppState>();
    if !on {
        state.squeeze.keep_alive_off();
    }
    let entries = state.settings().providers.clone();
    announce_kept(app, &entries, on);
}

/// Ping key `index` of provider `entry` unless the setting, the provider or
/// the key changed since the round started.
async fn ping_one(
    app: &AppHandle,
    entry: &ProviderEntry,
    index: usize,
    key: String,
    print: &str,
    board: Arc<SqueezeBoard>,
) {
    let state = app.state::<AppState>();
    let id = &entry.config.id;
    let unchanged = {
        let s = state.settings();
        s.keep_alive
            && s.provider(id)
                .is_some_and(|e| e.squeeze_target() == entry.squeeze_target())
    };
    if !unchanged || !state.squeeze.has_key(id, print) || !board.is_through(print) {
        return;
    }
    let outcome = ping(
        &entry.config,
        key,
        Arc::clone(&board),
        state.prompts(),
        state.request_log.clone(),
    )
    .await;
    if !state.squeeze.has_key(id, print) {
        // The key was removed meanwhile: its answer does not count.
        board.forget(print);
        return;
    }
    let name = &entry.config.name;
    match outcome {
        Ok(()) => {
            state.squeeze.pinged(id, print, None);
            log::info!("kept key {} of {name} alive", index + 1);
        }
        Err(e) => {
            let wait = e
                .retry_after()
                .map_or(PING_RETRY, |d| d.clamp(PING_RETRY, PING_WAIT_MAX));
            state.squeeze.pinged(id, print, Some(wait));
            log::info!("keeping key {} of {name} alive failed: {e}", index + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use biwrite_providers::{Effort, ProviderConfig, ProviderKind, WireApi};

    use super::*;

    fn entry(base_url: &str, keys: usize) -> ProviderEntry {
        ProviderEntry {
            config: ProviderConfig {
                id: "p-relay".into(),
                name: "Relay".into(),
                kind: ProviderKind::OpenaiCompatible,
                base_url: base_url.into(),
                model: "gpt-6-astra".into(),
                temperature: 0.0,
                effort: Effort::High,
                wire_api: WireApi::Responses,
                service_tier: None,
                key_concurrency: Some(2),
                max_retries: None,
            },
            has_key: keys > 0,
            key_count: keys,
            key_names: BTreeMap::new(),
            key_through: Default::default(),
            key_parts: 0,
            key_ignored: Default::default(),
        }
    }

    #[test]
    fn it_applies_to_anyrouter_and_to_pools_of_several_keys_only() {
        assert!(entry("https://anyrouter.top/v1", 1).squeezable());
        assert!(entry("https://api.example.com/v1", 2).squeezable());
        assert!(!entry("https://api.deepseek.com/v1", 1).squeezable());
        let mut mock = entry("", 3);
        mock.config.kind = ProviderKind::Mock;
        assert!(!mock.squeezable());
        assert!(
            SqueezeState::default()
                .board_for(&entry("https://api.deepseek.com/v1", 1))
                .is_none()
        );
    }

    #[test]
    fn a_key_set_aside_is_through_no_more_and_the_others_keep_their_places() {
        let state = SqueezeState::default();
        state.set_forever(true);
        let pool = entry("https://api.example.com/v1", 6);
        let board = state.board_for(&pool).unwrap();
        board.answered("fp-a");
        board.answered("fp-b");
        board.note_squeezed();
        state.set_aside("p-relay", "fp-a");
        assert!(!board.is_through("fp-a"));
        assert!(board.is_through("fp-b"), "the other key stays through");
        let due = state.due_for_keep_alive(std::slice::from_ref(&pool), Some(Duration::ZERO));
        assert_eq!(due[0].2, ["fp-b".to_owned()]);
    }

    #[test]
    fn keep_alives_wait_a_time_drawn_around_the_mean() {
        let state = SqueezeState::default();
        state.set_means(4.0, 3.0);
        let waits: Vec<Duration> = (0..300)
            .map(|i| state.keep_alive_after("p", &format!("fp-{i}")))
            .collect();
        assert!(
            waits
                .iter()
                .all(|w| (180.0..=300.0).contains(&w.as_secs_f64())),
            "three to five minutes"
        );
        assert!(waits.iter().any(|w| *w != waits[0]), "drawn, not fixed");
        assert_eq!(
            state.keep_alive_after("p", "fp-7"),
            waits[7],
            "the same until its keep-alive is answered"
        );
        assert_eq!(state.pace(), Some(Duration::from_secs(3)));
        state.set_means(2.0, 20.0);
        assert!(
            (90.0..=150.0).contains(&state.keep_alive_after("p", "fp-7").as_secs_f64()),
            "drawn again around a new mean"
        );
    }

    #[test]
    fn keys_a_squeeze_of_the_authors_got_through_are_kept_alive() {
        let state = SqueezeState::default();
        state.set_forever(true);
        let relay = entry("https://api.example.com/v1", 6);
        let board = state.board_for(&relay).unwrap();
        board.answered("fp-a");
        let zero = Duration::ZERO;
        assert!(
            state
                .due_for_keep_alive(std::slice::from_ref(&relay), Some(zero))
                .is_empty(),
            "no key was turned away"
        );
        // Its tries failed to connect: no request was turned away, yet the
        // keys that got through are kept alive.
        board.note_squeezed();
        let due = state.due_for_keep_alive(std::slice::from_ref(&relay), Some(zero));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].2, ["fp-a".to_owned()]);
    }

    #[test]
    fn keys_are_kept_alive_where_the_relay_turned_keys_away_and_they_are_idle() {
        let state = SqueezeState::default();
        let relay = entry("https://anyrouter.top/v1", 6);
        let board = state.board_for(&relay).unwrap();
        board.answered("fp-a");
        board.note_use();
        let zero = Duration::ZERO;
        assert!(
            state
                .due_for_keep_alive(std::slice::from_ref(&relay), Some(zero))
                .is_empty(),
            "no key was turned away"
        );
        board.note_turned_away();
        board.note_turned_away();
        let due = state.due_for_keep_alive(std::slice::from_ref(&relay), Some(zero));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].2, ["fp-a".to_owned()]);
        let hour = Duration::from_secs(3600);
        assert!(
            state
                .due_for_keep_alive(std::slice::from_ref(&relay), Some(hour))
                .is_empty(),
            "answered lately"
        );
        // Another pool of several keys, once its keys were turned away.
        let pool = ProviderEntry {
            config: ProviderConfig {
                id: "p-pool".into(),
                base_url: "https://api.example.com/v1".into(),
                ..relay.config.clone()
            },
            ..relay.clone()
        };
        let other = state.board_for(&pool).unwrap();
        other.answered("fp-b");
        other.note_use();
        // A squeeze was started for it.
        other.allow_crowding();
        other.note_turned_away();
        other.note_turned_away();
        let both = state.due_for_keep_alive(&[relay.clone(), pool.clone()], Some(zero));
        assert_eq!(both.len(), 2);
        // A key whose ping failed waits, an answered one does not.
        state.pinged("p-relay", "fp-a", Some(Duration::from_secs(60)));
        assert_eq!(
            state
                .due_for_keep_alive(&[relay.clone(), pool.clone()], Some(zero))
                .len(),
            1
        );
        state.pinged("p-relay", "fp-a", None);
        assert_eq!(
            state
                .due_for_keep_alive(&[relay.clone(), pool.clone()], Some(zero))
                .len(),
            2
        );
        // Keys changed: nothing through until answered again.
        state.keys_changed("p-pool");
        assert_eq!(
            state.due_for_keep_alive(&[relay, pool], Some(zero)).len(),
            1
        );
    }

    /// The frontend reads `kind` and camel-case fields next to the totals.
    #[test]
    fn events_reach_the_frontend_in_its_shape() {
        let event = |change| SqueezeEvent {
            provider_id: "p-relay".into(),
            provider_name: "Relay".into(),
            change,
            running: false,
            through: 2,
            total: 6,
            keeping: true,
        };
        let ended =
            serde_json::to_value(event(SqueezeChange::Ended { all_through: true })).unwrap();
        assert_eq!(
            ended,
            serde_json::json!({
                "providerId": "p-relay",
                "providerName": "Relay",
                "kind": "ended",
                "allThrough": true,
                "running": false,
                "through": 2,
                "total": 6,
                "keeping": true,
            })
        );
        let through = serde_json::to_value(event(SqueezeChange::Through { number: 3 })).unwrap();
        assert_eq!(through["kind"], "through");
        assert_eq!(through["number"], 3);
        let started = serde_json::to_value(event(SqueezeChange::Started)).unwrap();
        assert_eq!(started["kind"], "started");
        let kept = serde_json::to_value(event(SqueezeChange::Kept)).unwrap();
        assert_eq!(kept["kind"], "kept");
    }

    /// Keys kept alive that lose their place are squeezed back in, unless a
    /// squeeze runs already, until the author stops it or turns keeping
    /// alive off.
    #[tokio::test]
    async fn kept_keys_that_lose_their_place_are_squeezed_back_in() {
        let state = SqueezeState::default();
        let relay = entry("https://anyrouter.top/v1", 6);
        let entries = std::slice::from_ref(&relay);
        let board = state.board_for(&relay).unwrap();
        board.answered("fp-a");
        board.answered("fp-b");
        board.note_use();
        assert!(
            state.dropped_keys(entries).is_empty(),
            "no key was turned away"
        );
        board.note_turned_away();
        board.note_turned_away();
        assert!(state.dropped_keys(entries).is_empty(), "both still through");
        board.forget("fp-b");
        let dropped = state.dropped_keys(entries);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].2, ["fp-b".to_owned()]);
        // A key that never got through is not one to squeeze back in.
        assert!(!dropped[0].2.contains(&"fp-c".to_owned()));

        // While a squeeze runs, it does the work.
        let run = run_for(&state, &relay);
        assert!(state.dropped_keys(entries).is_empty());
        run.stop();
        assert_eq!(state.dropped_keys(entries).len(), 1);

        // Stopped by the author: the keys that dropped are let go.
        state.stop_squeezing_back("p-relay");
        assert!(state.dropped_keys(entries).is_empty());
        // fp-a, still through, is kept again: when it drops, it comes back.
        board.forget("fp-a");
        assert_eq!(state.dropped_keys(entries)[0].2, ["fp-a".to_owned()]);

        // Keeping alive turned off forgets them too.
        state.keep_alive_off();
        assert!(state.dropped_keys(entries).is_empty());
        // So do changed keys.
        board.answered("fp-d");
        assert!(state.dropped_keys(entries).is_empty());
        state.keys_changed("p-relay");
        assert!(
            state.dropped_keys(entries).is_empty(),
            "nothing through any more"
        );
    }

    /// The keys kept alive before a restart are noted, and after it they
    /// count as dropped, so the keeper squeezes them back in.
    #[test]
    fn keys_kept_before_a_restart_are_squeezed_back_in() {
        let state = SqueezeState::default();
        let relay = entry("https://anyrouter.top/v1", 6);
        let entries = std::slice::from_ref(&relay);
        let board = state.board_for(&relay).unwrap();
        board.answered("fp-a");
        board.answered("fp-b");
        board.note_turned_away();
        board.note_turned_away();
        board.note_use();
        assert!(state.dropped_keys(entries).is_empty(), "both through");
        let noted = state.kept_keys();
        assert_eq!(noted["p-relay"], ["fp-a".to_owned(), "fp-b".to_owned()]);

        // The restart: nothing is known until the keys kept are restored.
        let after = SqueezeState::default();
        assert!(after.dropped_keys(entries).is_empty());
        assert_eq!(after.restore_kept(entries, &noted), 2);
        let dropped = after.dropped_keys(entries);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].2, ["fp-a".to_owned(), "fp-b".to_owned()]);
        // Not used lately and not kept alive forever, it still counts as
        // used: the restart is the author's.
        assert!(dropped[0].1.used_within(KEEP_ALIVE_FOR));

        // A provider getting through does not apply to is left alone.
        let single = ProviderEntry {
            config: ProviderConfig {
                id: "p-one".into(),
                base_url: "https://api.example.com/v1".into(),
                ..relay.config.clone()
            },
            key_count: 1,
            ..relay.clone()
        };
        let mut kept = BTreeMap::new();
        kept.insert("p-one".to_owned(), vec!["fp-x".to_owned()]);
        assert_eq!(
            SqueezeState::default().restore_kept(std::slice::from_ref(&single), &kept),
            0
        );
    }

    /// Squeezing back in backs off while the author is away only when
    /// keeping alive is not forever: forever, it tries every few seconds as
    /// in a squeeze the author started.
    /// Updating from a version that did not note the keys kept alive: the
    /// keys of a crowded relay answered within the last 90 minutes.
    #[test]
    fn without_a_note_the_keys_answered_lately_are_squeezed_back_in() {
        let now = 1_800_000_000_000;
        let minute = 60 * 1000;
        let mut relay = entry("https://anyrouter.top/v1", 6);
        relay.key_through.insert("fp-new".into(), now - 5 * minute);
        relay
            .key_through
            .insert("fp-hour".into(), now - 70 * minute);
        relay
            .key_through
            .insert("fp-old".into(), now - 5 * 60 * minute);
        let mut pool = entry("https://api.example.com/v1", 2);
        pool.config.id = "p-pool".into();
        pool.key_through.insert("fp-pool".into(), now - minute);
        let lately = answered_lately(&[relay, pool], now);
        assert_eq!(lately.len(), 1, "an ordinary pool is not kept alive");
        assert_eq!(
            lately["p-relay"],
            ["fp-hour".to_owned(), "fp-new".to_owned()]
        );
    }

    #[test]
    fn squeezing_back_in_while_keeping_alive_forever_does_not_back_off() {
        let state = SqueezeState::default();
        state.set_forever(true);
        assert!(!backs_off(&state, true));
        assert!(!backs_off(&state, false), "the author's squeeze");
        state.set_forever(false);
        assert!(backs_off(&state, true));
        assert!(!backs_off(&state, false));
    }

    #[test]
    fn a_relays_request_id_is_left_out_when_comparing_its_words() {
        assert_eq!(
            without_request_id(
                "provider server error (HTTP 500): 当前模型 m 负载已经达到上限，请稍后重试 (request id: 2026101300064)"
            ),
            "provider server error (HTTP 500): 当前模型 m 负载已经达到上限，请稍后重试"
        );
        assert_eq!(
            without_request_id("network error: timed out"),
            "network error: timed out"
        );
        assert_eq!(numbered(&[1, 3, 5]), "1, 3, 5");
    }

    /// Keys are kept alive, and squeezed back in, only while the author uses
    /// the provider, and an ordinary pool whose server failed (an outage,
    /// not a crowd) is never kept alive.
    #[test]
    fn keys_are_kept_alive_only_where_the_author_uses_a_crowded_relay() {
        let state = SqueezeState::default();
        let zero = Duration::ZERO;
        let relay = entry("https://anyrouter.top/v1", 6);
        let board = state.board_for(&relay).unwrap();
        board.answered("fp-a");
        board.answered("fp-b");
        board.note_turned_away();
        board.note_turned_away();
        let entries = std::slice::from_ref(&relay);
        assert!(state.dropped_keys(entries).is_empty());
        board.forget("fp-b");
        assert!(
            state.due_for_keep_alive(entries, Some(zero)).is_empty(),
            "not used"
        );
        assert!(state.dropped_keys(entries).is_empty(), "not used");
        board.note_use();
        assert_eq!(state.due_for_keep_alive(entries, Some(zero)).len(), 1);
        assert_eq!(state.dropped_keys(entries)[0].2, ["fp-b".to_owned()]);

        // Two keys at an official API, during an outage.
        let pool = ProviderEntry {
            config: ProviderConfig {
                id: "p-pool".into(),
                base_url: "https://api.example.com/v1".into(),
                ..relay.config.clone()
            },
            key_count: 2,
            ..relay.clone()
        };
        let other = state.board_for(&pool).unwrap();
        other.note_use();
        other.note_turned_away();
        other.note_turned_away();
        other.answered("fp-c");
        let pools = std::slice::from_ref(&pool);
        assert!(state.due_for_keep_alive(pools, Some(zero)).is_empty());
        assert!(state.dropped_keys(pools).is_empty());
    }

    /// Keeping alive forever (the default setting): the keys of a crowded
    /// relay are kept alive and squeezed back in also when the author has
    /// not used them since the app started (a night away), the keeper's
    /// squeeze goes on, and an ordinary pool's outage still starts nothing.
    #[tokio::test]
    async fn keeping_alive_forever_goes_on_while_the_author_is_away() {
        let state = SqueezeState::default();
        state.set_forever(true);
        let zero = Duration::ZERO;
        let relay = entry("https://anyrouter.top/v1", 6);
        let board = state.board_for(&relay).unwrap();
        board.answered("fp-a");
        board.answered("fp-b");
        board.note_turned_away();
        board.note_turned_away();
        let entries = std::slice::from_ref(&relay);
        assert!(state.dropped_keys(entries).is_empty());
        board.forget("fp-b");
        assert!(!board.used_within(KEEP_ALIVE_FOR), "never used");
        assert_eq!(state.due_for_keep_alive(entries, Some(zero)).len(), 1);
        assert_eq!(state.dropped_keys(entries)[0].2, ["fp-b".to_owned()]);
        assert!(keeps(&state, &board));
        let run = run_for(&state, &relay);
        state.auto().insert("p-relay".into());
        state.stop_unused_runs(entries);
        assert!(run.running(), "the keeper's squeeze goes on");
        // Unticked: half an hour after the last use, as before.
        state.set_forever(false);
        assert!(state.due_for_keep_alive(entries, Some(zero)).is_empty());
        assert!(!keeps(&state, &board));
        state.stop_unused_runs(entries);
        assert!(!run.running());

        // Two keys at an official API, during an outage.
        state.set_forever(true);
        let pool = ProviderEntry {
            config: ProviderConfig {
                id: "p-pool".into(),
                base_url: "https://api.example.com/v1".into(),
                ..relay.config.clone()
            },
            key_count: 2,
            ..relay.clone()
        };
        let other = state.board_for(&pool).unwrap();
        other.note_turned_away();
        other.note_turned_away();
        other.answered("fp-c");
        let pools = std::slice::from_ref(&pool);
        assert!(state.due_for_keep_alive(pools, Some(zero)).is_empty());
        assert!(state.dropped_keys(pools).is_empty());
    }

    /// The key pool shows when a key was last kept alive, and that a
    /// failed keep-alive is being tried again.
    #[test]
    fn the_pool_shows_the_last_keep_alive_of_each_key() {
        let state = SqueezeState::default();
        let relay = entry("https://anyrouter.top/v1", 6);
        let board = state.board_for(&relay).unwrap();
        board.answered("fp-a");
        let key = |v: &SqueezeView| {
            v.keys
                .iter()
                .find(|k| k.fingerprint == "fp-a")
                .cloned()
                .unwrap()
        };
        let a = key(&view(&state, "p-relay"));
        assert!(a.through_since.is_some());
        assert_eq!((a.kept_at, a.keep_retrying), (None, false), "just through");
        state.pinged("p-relay", "fp-a", None);
        let a = key(&view(&state, "p-relay"));
        assert!(a.kept_at.is_some_and(|t| t >= a.through_since.unwrap()));
        state.pinged("p-relay", "fp-a", Some(Duration::from_secs(30)));
        assert!(key(&view(&state, "p-relay")).keep_retrying);
        // Through again later: an older keep-alive does not count.
        board.forget("fp-a");
        state
            .kept_at
            .lock()
            .unwrap()
            .insert(("p-relay".into(), "fp-a".into()), 1);
        board.answered("fp-a");
        assert_eq!(key(&view(&state, "p-relay")).kept_at, None);
    }

    /// A kept key that a squeeze gave up on (refused, or a request that
    /// cannot succeed) is not squeezed back in every round.
    #[test]
    fn a_key_a_squeeze_gave_up_on_is_not_squeezed_back_in() {
        let state = SqueezeState::default();
        let relay = entry("https://anyrouter.top/v1", 6);
        let entries = std::slice::from_ref(&relay);
        let board = state.board_for(&relay).unwrap();
        board.answered("fp-a");
        board.answered("fp-b");
        board.note_use();
        board.note_turned_away();
        board.note_turned_away();
        assert!(state.dropped_keys(entries).is_empty());
        board.forget("fp-b");
        assert_eq!(state.dropped_keys(entries)[0].2, ["fp-b".to_owned()]);
        board.give_up("fp-b");
        assert!(state.dropped_keys(entries).is_empty());
        // Once it got through again, it is kept again.
        board.answered("fp-b");
        assert!(state.dropped_keys(entries).is_empty());
        board.forget("fp-b");
        assert_eq!(state.dropped_keys(entries)[0].2, ["fp-b".to_owned()]);
    }

    /// A squeeze the keeper started stops once the provider was not used
    /// for a while; one the author started goes on.
    #[tokio::test]
    async fn the_keepers_squeeze_stops_when_the_provider_is_not_used() {
        let state = SqueezeState::default();
        let relay = entry("https://anyrouter.top/v1", 6);
        let entries = std::slice::from_ref(&relay);
        // Never used, as if not used for long.
        let run = run_for(&state, &relay);
        state.stop_unused_runs(entries);
        assert!(run.running(), "the author's own squeeze goes on");
        state.auto().insert("p-relay".into());
        state.stop_unused_runs(entries);
        assert!(!run.running(), "the keeper's stops");
        assert!(!state.auto().contains("p-relay"));

        let run = run_for(&state, &relay);
        state.auto().insert("p-relay".into());
        state.board_for(&relay).unwrap().note_use();
        state.stop_unused_runs(entries);
        assert!(run.running(), "used lately");
        run.stop();
    }

    /// A squeeze of `entry` against a port nothing listens on, so its tries
    /// fail until it is stopped.
    fn run_for(state: &SqueezeState, entry: &ProviderEntry) -> Arc<Squeeze> {
        let board = state.board_for(entry).unwrap();
        let mut config = entry.config.clone();
        config.base_url = "http://127.0.0.1:9/v1".into();
        let run = Squeeze::start(
            &config,
            vec!["sk-a".into(), "sk-b".into()],
            board,
            Arc::new(biwrite_providers::DefaultPrompts),
            Arc::new(|_| {}),
            &tokio::runtime::Handle::current(),
            false,
        )
        .unwrap();
        state
            .runs()
            .insert(entry.config.id.clone(), Arc::clone(&run));
        run
    }

    async fn ends(run: &Squeeze) -> bool {
        for _ in 0..50 {
            if !run.running() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn changing_the_keys_or_the_provider_stops_its_squeeze() {
        let state = SqueezeState::default();
        let pool = entry("https://api.example.com/v1", 2);
        let run = run_for(&state, &pool);
        assert!(run.running());
        state.board_for(&pool).unwrap().answered("fingerprint-a");
        state.keys_changed("p-relay");
        assert!(ends(&run).await, "keys changed");
        assert!(
            state.board_for(&pool).unwrap().through().is_empty(),
            "the board starts over"
        );
        assert!(view(&state, "p-relay").keys.is_empty() || !view(&state, "p-relay").running);

        let run = run_for(&state, &pool);
        let mut one_key = pool.clone();
        one_key.key_count = 1;
        assert!(state.board_for(&one_key).is_none());
        assert!(ends(&run).await, "no longer squeezable");

        let run = run_for(&state, &pool);
        let mut moved = pool.clone();
        moved.config.model = "another-model".into();
        assert!(state.board_for(&moved).is_some());
        assert!(ends(&run).await, "moved to another model");
    }

    #[test]
    fn a_provider_keeps_its_board_until_it_moves_to_another_model() {
        let state = SqueezeState::default();
        let relay = entry("https://anyrouter.top/v1", 6);
        let board = state.board_for(&relay).unwrap();
        board.answered("fingerprint-a");
        let again = state.board_for(&relay).unwrap();
        assert!(
            Arc::ptr_eq(&board, &again),
            "the same board after a switch back"
        );
        assert_eq!(view(&state, "p-relay").keys.len(), 1);
        let mut moved = relay.clone();
        moved.config.model = "gpt-5-codex".into();
        let fresh = state.board_for(&moved).unwrap();
        assert!(!Arc::ptr_eq(&board, &fresh));
        assert!(view(&state, "p-relay").keys.is_empty());
        state.forget("p-relay");
        assert!(view(&state, "p-relay").keys.is_empty());
    }
}
