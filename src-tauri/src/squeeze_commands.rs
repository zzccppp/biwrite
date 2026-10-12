//! Getting through to a crowded relay (see `biwrite_providers::Squeeze`).
//!
//! Each provider for which it applies (a crowded relay, or several keys) has
//! a board of the keys that got through. Every provider built for it joins
//! the board, so translation, the writing assistant and key tests send to
//! those keys first, and a switch to another provider and back keeps it. A
//! squeeze started from the key pool runs in the background whichever
//! provider is in use, and says when a key gets through. With keeping alive
//! on, a keeper sends the keys that got through a short translation when
//! they are idle, for every provider whose relay turned keys away, and
//! squeezes a key it kept back in when the key loses its place.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use biwrite_providers::{
    ChangeFn, HttpProvider, KEEP_ALIVE_AFTER, KeyProgress, Squeeze, SqueezeBoard, SqueezeChange,
    key_fingerprint, ping,
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
const KEEP_ALIVE_TICK: Duration = Duration::from_secs(30);
/// A key whose ping failed is pinged again after this long, or after as
/// long as the server asks, up to [`PING_WAIT_MAX`]. A key that got through
/// fails now and then, and a retry soon keeps it before it counts as stale.
const PING_RETRY: Duration = Duration::from_secs(30);
const PING_WAIT_MAX: Duration = Duration::from_secs(10 * 60);

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
    /// Keys kept alive, by provider id: those that got through while
    /// keeping alive was on. One that loses its place is squeezed back in.
    kept: Mutex<HashMap<String, HashSet<String>>>,
    /// Providers whose running squeeze the keeper started.
    auto: Mutex<HashSet<String>>,
}

impl SqueezeState {
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
    /// `entries` with no squeeze running (their fingerprints). The keys
    /// through now join those kept, for every relay that turned keys away.
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
                    kept.iter()
                        .filter(|f| !through.contains(*f))
                        .cloned()
                        .collect()
                };
                if dropped.is_empty() || self.runs().get(&e.config.id).is_some_and(|r| r.running())
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
    /// turned keys away, and keys of it that got through and were not
    /// answered for `after` (their fingerprints).
    pub fn due_for_keep_alive(
        &self,
        entries: &[ProviderEntry],
        after: Duration,
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
                if !board.was_crowded() {
                    return None;
                }
                let due: Vec<String> = board
                    .through()
                    .into_iter()
                    .map(|t| t.fingerprint)
                    .filter(|f| board.idle(f).is_some_and(|idle| idle >= after))
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

/// One key of a provider, for the key pool.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqueezeKeyView {
    pub fingerprint: String,
    /// When it got through (Unix milliseconds), if it counts as through.
    pub through_since: Option<u64>,
    /// When it was last answered (Unix milliseconds), if through.
    pub last_ok: Option<u64>,
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
            key.through_since = Some(t.since_ms);
            key.last_ok = Some(t.last_ok_ms);
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
    if state.squeeze.runs().get(&id).is_some_and(|r| r.running()) {
        return Ok(view(&state.squeeze, &id));
    }
    let store = state.secrets.clone();
    let account = id.clone();
    let vouched = entry.vouched();
    let keys = blocking(move || secrets::get_keys(store.as_ref(), &account, vouched)).await?;
    state.squeeze.cache_keys(&id, Some(keys.clone()));
    if keys.is_empty() {
        return Err(CommandError::Settings(format!(
            "No API key for “{}”. Add one in Settings.",
            entry.config.name
        )));
    }
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
    let on_change: ChangeFn = {
        let app = app.clone();
        let id = id.clone();
        let name = entry.config.name.clone();
        let board = Arc::clone(&board);
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
            let event = SqueezeEvent {
                provider_id: id.clone(),
                provider_name: name.clone(),
                through: fingerprints.iter().filter(|f| board.is_through(f)).count(),
                total,
                running,
                keeping: state.settings().keep_alive && board.was_crowded(),
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
    )?;
    on_change(SqueezeChange::Started);
    if auto {
        log::info!("squeezing keys of {} back in", entry.config.name);
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

/// Keep the keys that got through alive, every [`KEEP_ALIVE_TICK`], while
/// the setting is on.
pub fn spawn_keeper(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(KEEP_ALIVE_TICK).await;
            keep_alive(&app).await;
        }
    });
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
        s.providers.clone()
    };
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
    for (entry, board, due) in state.squeeze.due_for_keep_alive(&entries, KEEP_ALIVE_AFTER) {
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
            keeping: on,
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
    let outcome = ping(&entry.config, key, Arc::clone(&board), state.prompts()).await;
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
            key_parts: 0,
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
    fn keys_are_kept_alive_where_the_relay_turned_keys_away_and_they_are_idle() {
        let state = SqueezeState::default();
        let relay = entry("https://anyrouter.top/v1", 6);
        let board = state.board_for(&relay).unwrap();
        board.answered("fp-a");
        let zero = Duration::ZERO;
        assert!(
            state
                .due_for_keep_alive(std::slice::from_ref(&relay), zero)
                .is_empty(),
            "no key was turned away"
        );
        board.note_turned_away();
        board.note_turned_away();
        let due = state.due_for_keep_alive(std::slice::from_ref(&relay), zero);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].2, ["fp-a".to_owned()]);
        let hour = Duration::from_secs(3600);
        assert!(
            state
                .due_for_keep_alive(std::slice::from_ref(&relay), hour)
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
        other.note_turned_away();
        other.note_turned_away();
        let both = state.due_for_keep_alive(&[relay.clone(), pool.clone()], zero);
        assert_eq!(both.len(), 2);
        // A key whose ping failed waits, an answered one does not.
        state.pinged("p-relay", "fp-a", Some(Duration::from_secs(60)));
        assert_eq!(
            state
                .due_for_keep_alive(&[relay.clone(), pool.clone()], zero)
                .len(),
            1
        );
        state.pinged("p-relay", "fp-a", None);
        assert_eq!(
            state
                .due_for_keep_alive(&[relay.clone(), pool.clone()], zero)
                .len(),
            2
        );
        // Keys changed: nothing through until answered again.
        state.keys_changed("p-pool");
        assert_eq!(state.due_for_keep_alive(&[relay, pool], zero).len(), 1);
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
