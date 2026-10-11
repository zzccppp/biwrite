//! Getting through to a crowded relay (see `biwrite_providers::Squeeze`).
//!
//! Each provider for which it applies (a crowded relay, or several keys) has
//! a board of the keys that got through. Every provider built for it joins
//! the board, so translation, the writing assistant and key tests send to
//! those keys first, and a switch to another provider and back keeps it. A
//! squeeze started from the key pool runs in the background whichever
//! provider is in use, and says when a key gets through.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use biwrite_providers::{
    ChangeFn, HttpProvider, KeyProgress, Squeeze, SqueezeBoard, SqueezeChange,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, UserAttentionType};

use crate::error::{CommandError, CommandResult};
use crate::secrets;
use crate::settings::ProviderEntry;
use crate::settings_commands::{blocking, not_found};
use crate::state::AppState;

pub const EVENT_SQUEEZE: &str = "squeeze";

/// Boards and running squeezes, by provider id.
#[derive(Default)]
pub struct SqueezeState {
    /// Each board with the target it was made for
    /// (`ProviderEntry::squeeze_target`).
    boards: Mutex<HashMap<String, (String, Arc<SqueezeBoard>)>>,
    runs: Mutex<HashMap<String, Arc<Squeeze>>>,
}

impl SqueezeState {
    fn boards(&self) -> MutexGuard<'_, HashMap<String, (String, Arc<SqueezeBoard>)>> {
        self.boards.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn runs(&self) -> MutexGuard<'_, HashMap<String, Arc<Squeeze>>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
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
        let board = if entry.crowded_relay() {
            SqueezeBoard::for_relay()
        } else {
            SqueezeBoard::new()
        };
        boards.insert(id.clone(), (target, Arc::clone(&board)));
        Some(board)
    }

    /// The keys of provider `id` changed: its squeeze, which tries the keys
    /// it read, stops.
    pub fn stop(&self, id: &str) {
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
    if keys.is_empty() {
        return Err(CommandError::Settings(format!(
            "No API key for “{}”. Add one in Settings.",
            entry.config.name
        )));
    }
    let total = keys.len();
    let fingerprints: Vec<String> = keys
        .iter()
        .map(|k| biwrite_providers::key_fingerprint(k))
        .collect();
    let on_change: ChangeFn = {
        let app = app.clone();
        let id = id.clone();
        let name = entry.config.name.clone();
        let board = Arc::clone(&board);
        Arc::new(move |change: SqueezeChange| {
            let state = app.state::<AppState>();
            let running = !matches!(change, SqueezeChange::Ended { .. });
            if let SqueezeChange::Through { .. } = change {
                // Paragraphs that failed while nothing got through go again.
                if state.settings().active_provider == id {
                    state.engine.retry_failed();
                }
                if let Some(window) = app.get_webview_window("main") {
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
                change,
            };
            if let Err(e) = app.emit(EVENT_SQUEEZE, event) {
                log::warn!("could not emit {EVENT_SQUEEZE}: {e}");
            }
        })
    };
    let run = Squeeze::start(
        &entry.config,
        keys,
        board,
        state.prompts(),
        Arc::clone(&on_change),
        tauri::async_runtime::handle().inner(),
    )?;
    on_change(SqueezeChange::Started);
    log::info!("squeeze started for {}", entry.config.name);
    if let Some(old) = state.squeeze.runs().insert(id.clone(), run) {
        old.stop();
    }
    Ok(view(&state.squeeze, &id))
}

/// Stop the squeeze of provider `id`. The keys that got through stay.
#[tauri::command]
pub async fn squeeze_stop(state: State<'_, AppState>, id: String) -> CommandResult<SqueezeView> {
    if let Some(run) = state.squeeze.runs().get(&id) {
        run.stop();
    }
    Ok(view(&state.squeeze, &id))
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
            })
        );
        let through = serde_json::to_value(event(SqueezeChange::Through { number: 3 })).unwrap();
        assert_eq!(through["kind"], "through");
        assert_eq!(through["number"], 3);
        let started = serde_json::to_value(event(SqueezeChange::Started)).unwrap();
        assert_eq!(started["kind"], "started");
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
        state.stop("p-relay");
        assert!(ends(&run).await, "keys changed");
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
