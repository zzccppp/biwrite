//! Settings commands: providers, API keys, models, prompts, concurrency,
//! document note, assistant provider. Responses never include an API key:
//! the key commands are write-only, views carry key counts, and key status
//! names a key by its position and last four characters.

use std::time::Duration;

use biwrite_core::Direction;
use biwrite_engine::TranslationRequest;
use biwrite_providers::prompt::{default_prompt, prompt_file_name};
use biwrite_providers::{
    ChatModel, ChatRequest, KeyStatus, ProviderConfig, ProviderKind, key_fingerprint, key_tail,
    list_models,
};
use serde::Serialize;
use tauri::State;

use crate::error::{CommandError, CommandResult};
use crate::provider_state::translator_for;
use crate::secrets::{self, MAX_KEYS, NamedKey, Vouched};
use crate::settings::{MOCK_ID, ProviderEntry, SettingsView, keeps_keys};
use crate::state::AppState;

/// A test's question and the model's answer, shown as they are.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestAnswer {
    pub question: String,
    pub answer: String,
}

/// Longest answer a test shows, in characters.
const TEST_ANSWER_CHARS: usize = 400;

/// Ask `model` one of the small tasks of a keep-alive (a different one each
/// time) and give the question with its answer.
async fn ask_test(model: &dyn ChatModel, wait: Duration) -> CommandResult<TestAnswer> {
    let (system, question) = biwrite_providers::test_question();
    let request = ChatRequest {
        purpose: "test".to_owned(),
        system,
        user: question.clone(),
        images: Vec::new(),
    };
    let run = model.complete(&request, &|_| {});
    let out = tokio::time::timeout(wait, run).await.map_err(|_| {
        CommandError::Settings(format!("no answer within {} seconds", wait.as_secs()))
    })??;
    let answer: String = out.text.trim().chars().take(TEST_ANSWER_CHARS).collect();
    Ok(TestAnswer { question, answer })
}

pub(crate) fn not_found(id: &str) -> CommandError {
    CommandError::Settings(format!("unknown provider {id}"))
}

/// Run blocking keychain work off the async runtime.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> CommandResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CommandError::Task(e.to_string()))?
        .map_err(|e| CommandError::Settings(format!("keychain: {e}")))
}

fn new_provider_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let n = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    format!("p-{n:016x}")
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> CommandResult<SettingsView> {
    Ok(state.settings_view())
}

/// Add (empty `id`) or update a provider.
#[tauri::command]
pub async fn save_provider(
    state: State<'_, AppState>,
    provider: ProviderConfig,
) -> CommandResult<SettingsView> {
    let mut config = provider
        .clone()
        .validated()
        .map_err(|e| CommandError::Settings(e.to_string()))?;
    if config.id == MOCK_ID || config.kind == ProviderKind::Mock {
        return Err(CommandError::Settings(
            "the mock provider can't be edited".into(),
        ));
    }
    if config.id.is_empty() {
        config.id = new_provider_id();
    }
    let id = config.id.clone();
    let _keys = state.keys_lock.lock().await;
    // A key belongs to one host: never send it elsewhere. The same host on
    // another API type (a relay's Claude and GPT models) keeps it.
    // (Even without a key in the settings: a version without key pools may
    // have left some behind.)
    let moved = state
        .settings()
        .provider(&id)
        .is_some_and(|old| !keeps_keys(&old.config, &config));
    if moved {
        state.settings_writable()?;
        let store = state.secrets.clone();
        let account = id.clone();
        blocking(move || secrets::delete_keys(store.as_ref(), &account)).await?;
    }
    let is_active = {
        let mut s = state.settings();
        match s.provider_mut(&id) {
            Some(entry) => {
                entry.config = config;
                if moved {
                    entry.has_key = false;
                    entry.key_count = 0;
                    entry.key_parts = 0;
                }
            }
            None if provider.id.is_empty() => s.providers.push(ProviderEntry {
                config,
                has_key: false,
                key_count: 0,
                key_names: Default::default(),
                key_through: Default::default(),
                key_parts: 0,
                key_ignored: Default::default(),
            }),
            None => return Err(not_found(&id)),
        }
        state.persist(&s)?;
        s.active_provider == id
    };
    state.invalidate_assistant();
    // Moved to another host, model or API, or no longer squeezable: its
    // squeeze stops.
    let saved = state.settings().provider(&id).cloned();
    if let Some(entry) = saved {
        state.squeeze.board_for(&entry);
    }
    if is_active {
        state.apply_active_provider()?;
    }
    Ok(state.settings_view())
}

#[tauri::command]
pub async fn delete_provider(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<SettingsView> {
    if id == MOCK_ID {
        return Err(CommandError::Settings(
            "the mock provider can't be removed".into(),
        ));
    }
    let _keys = state.keys_lock.lock().await;
    state.settings_writable()?;
    let store = state.secrets.clone();
    let account = id.clone();
    blocking(move || secrets::delete_keys(store.as_ref(), &account)).await?;
    let was_active = {
        let mut s = state.settings();
        s.providers.retain(|p| p.config.id != id);
        state.squeeze.forget(&id);
        let was_active = s.active_provider == id;
        if was_active {
            s.active_provider = MOCK_ID.to_owned();
        }
        if s.assistant_provider == id {
            s.assistant_provider.clear();
        }
        state.persist(&s)?;
        was_active
    };
    state.invalidate_assistant();
    if was_active {
        state.apply_active_provider()?;
    }
    Ok(state.settings_view())
}

fn parse_named(text: &str) -> CommandResult<Vec<NamedKey>> {
    secrets::parse_named_keys(text).map_err(CommandError::Settings)
}

/// Remember the names written next to pasted keys (by fingerprint).
fn remember_names(state: &AppState, id: &str, keys: &[NamedKey]) {
    let mut s = state.settings();
    if let Some(entry) = s.provider_mut(id) {
        for k in keys {
            if let Some(name) = &k.name {
                entry
                    .key_names
                    .insert(key_fingerprint(&k.key), name.trim().to_owned());
            }
        }
    }
}

/// Replace the provider's keys with the pasted ones: one or more, each
/// optionally named (`name` on the line before the key, or `name: key`).
/// Keys go to the OS keychain. Write-only: no command returns keys.
#[tauri::command]
pub async fn set_api_key(
    state: State<'_, AppState>,
    id: String,
    key: String,
) -> CommandResult<SettingsView> {
    let named = parse_named(&key)?;
    let _keys = state.keys_lock.lock().await;
    if state.settings().provider(&id).is_none() {
        return Err(not_found(&id));
    }
    state.settings_writable()?;
    let keys: Vec<String> = named.iter().map(|k| k.key.clone()).collect();
    let count = keys.len();
    let store = state.secrets.clone();
    let account = id.clone();
    let parts = blocking(move || secrets::set_keys(store.as_ref(), &account, &keys, None)).await?;
    {
        let mut s = state.settings();
        if let Some(entry) = s.provider_mut(&id) {
            entry.key_names.clear();
            entry.key_through.clear();
        }
    }
    remember_names(&state, &id, &named);
    update_key_count(&state, &id, count, parts)
}

/// Add the pasted keys (optionally named) to the provider's pool;
/// duplicates are skipped.
#[tauri::command]
pub async fn add_api_keys(
    state: State<'_, AppState>,
    id: String,
    keys: String,
) -> CommandResult<SettingsView> {
    let named = parse_named(&keys)?;
    let _keys = state.keys_lock.lock().await;
    let vouched = state
        .settings()
        .provider(&id)
        .ok_or_else(|| not_found(&id))?
        .vouched();
    state.settings_writable()?;
    let added: Vec<String> = named.iter().map(|k| k.key.clone()).collect();
    let store = state.secrets.clone();
    let account = id.clone();
    let (count, parts) = blocking(move || {
        let stored = secrets::read_stored(store.as_ref(), &account)?;
        let mut pool = stored.keys(vouched);
        for key in added {
            if !pool.contains(&key) {
                pool.push(key);
            }
        }
        if pool.len() > MAX_KEYS {
            return Err(format!("at most {MAX_KEYS} keys per provider"));
        }
        let parts = secrets::set_keys(store.as_ref(), &account, &pool, Some(&stored))?;
        Ok((pool.len(), parts))
    })
    .await?;
    remember_names(&state, &id, &named);
    {
        let mut s = state.settings();
        if !s.set_key_count(&id, count, parts) {
            return Err(not_found(&id));
        }
        state.persist(&s)?;
    }
    // The squeeze tries the keys it read when it started.
    state.squeeze.keys_changed(&id);
    // Added keys: requests in flight go on.
    state.reload_keys(&id)?;
    Ok(state.settings_view())
}

/// Largest key file read on import.
const MAX_KEY_FILE_BYTES: u64 = 256 * 1024;

/// The pool as text: a header naming the provider, then each key's name
/// on one line and the key on the next (the shape the importer reads).
fn pool_text(provider: &ProviderEntry, keys: &[String]) -> String {
    let c = &provider.config;
    let mut out = format!(
        "# BiWrite key pool · {}\n# {} · {} · {}\n# Secret API keys: share them only with people you trust.\n",
        c.name,
        c.wire_label(),
        c.base_url,
        c.model
    );
    for (i, key) in keys.iter().enumerate() {
        let name = provider
            .key_names
            .get(&key_fingerprint(key))
            .cloned()
            .unwrap_or_else(|| format!("key {}", i + 1));
        out.push_str(&format!("{name}\n{key}\n"));
    }
    out
}

/// Save the provider's keys (with their names) to a file the user picks,
/// to move them to another computer or share them. The keys go from the
/// keychain to the file without passing through the window.
#[tauri::command]
pub async fn export_api_keys(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<Option<String>> {
    let provider = state
        .settings()
        .provider(&id)
        .cloned()
        .ok_or_else(|| not_found(&id))?;
    let store = state.secrets.clone();
    let account = id.clone();
    let vouched = provider.vouched();
    let keys = blocking(move || secrets::get_keys(store.as_ref(), &account, vouched)).await?;
    if keys.is_empty() {
        return Err(CommandError::Settings(
            "this provider has no keys to export".into(),
        ));
    }
    let stem: String = provider
        .config
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let name = format!("{}-keys.txt", stem.trim_matches('-'));
    let Some(dest) =
        crate::files::pick_save_kind(&app, &window, "Key pool", &["txt"], None, &name).await
    else {
        return Ok(None);
    };
    let text = pool_text(&provider, &keys);
    crate::files::write_private_atomic(dest.clone(), text.into_bytes()).await?;
    log::info!("exported {} keys of {}", keys.len(), provider.config.name);
    Ok(Some(dest.display().to_string()))
}

/// Add the keys of a file (an exported pool, or any text with keys) to the
/// provider's pool.
#[tauri::command]
pub async fn import_api_keys(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<Option<SettingsView>> {
    let Some(path) =
        crate::files::pick_open_kind(&app, &window, "Key pool", &["txt", "csv", "json", "md"])
            .await
    else {
        return Ok(None);
    };
    let bytes = crate::files::read_small_file(path, MAX_KEY_FILE_BYTES).await?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    add_api_keys(state, id, text).await.map(Some)
}

/// One key of a pool as the key manager shows it (never the key itself).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyEntryView {
    pub number: usize,
    pub fingerprint: String,
    pub tail: String,
    pub name: String,
    /// `ready`, `cooling`, `rejected`, or `idle` before the first request.
    pub state: String,
    pub in_flight: u32,
    pub detail: Option<String>,
    /// When a relay that turns keys away last answered it (Unix
    /// milliseconds); none when it never got through.
    pub last_through: Option<u64>,
    /// Set aside by the author: no request uses it.
    pub ignored: bool,
}

/// The keys of provider `id` with their names and live state. Reads the
/// keychain, so it is only called when the key manager opens.
#[tauri::command]
pub async fn list_api_keys(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<Vec<KeyEntryView>> {
    let (names, through, pool, ignored) = {
        let s = state.settings();
        let entry = s.provider(&id).ok_or_else(|| not_found(&id))?;
        (
            entry.key_names.clone(),
            entry.key_through.clone(),
            entry.vouched(),
            entry.key_ignored.clone(),
        )
    };
    let store = state.secrets.clone();
    let account = id.clone();
    let keys = blocking(move || secrets::get_keys(store.as_ref(), &account, pool)).await?;
    let live = state.key_status(&id).unwrap_or_default();
    Ok(keys
        .iter()
        .enumerate()
        .map(|(i, key)| {
            let fingerprint = key_fingerprint(key);
            let status = live.iter().find(|s| s.fingerprint == fingerprint);
            KeyEntryView {
                number: i + 1,
                name: names.get(&fingerprint).cloned().unwrap_or_default(),
                tail: key_tail(key),
                state: status.map_or("idle", |s| s.state).to_owned(),
                in_flight: status.map_or(0, |s| s.in_flight),
                detail: status.and_then(|s| s.detail.clone()),
                last_through: through.get(&fingerprint).copied(),
                ignored: ignored.contains(&fingerprint),
                fingerprint,
            }
        })
        .collect())
}

/// Put `key` in place of key `number` of the pool (whose tail is `tail`):
/// a new key for the same account. It keeps the old one's name and place;
/// it has not got through yet.
#[tauri::command]
pub async fn replace_api_key(
    state: State<'_, AppState>,
    id: String,
    number: usize,
    tail: String,
    key: String,
) -> CommandResult<SettingsView> {
    let named = parse_named(&key)?;
    let [new] = named.as_slice() else {
        return Err(CommandError::Settings("paste one key".into()));
    };
    let new_key = new.key.clone();
    let new_name = new
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    let _keys = state.keys_lock.lock().await;
    let vouched = state
        .settings()
        .provider(&id)
        .ok_or_else(|| not_found(&id))?
        .vouched();
    state.settings_writable()?;
    let store = state.secrets.clone();
    let account = id.clone();
    let (count, parts, old) = blocking(move || {
        let stored = secrets::read_stored(store.as_ref(), &account)?;
        let mut pool = stored.keys(vouched);
        let i = match number.checked_sub(1).filter(|&i| i < pool.len()) {
            Some(i) if key_tail(&pool[i]) == tail => i,
            _ => return Err("the keys have changed; reopen the settings and try again".into()),
        };
        if pool.contains(&new_key) {
            return Err("this key is in the pool already".into());
        }
        let old = std::mem::replace(&mut pool[i], new_key);
        let parts = secrets::set_keys(store.as_ref(), &account, &pool, Some(&stored))?;
        Ok((pool.len(), parts, key_fingerprint(&old)))
    })
    .await?;
    let fresh = key_fingerprint(&new.key);
    if let Some(entry) = state.settings().provider_mut(&id) {
        let name = entry.key_names.remove(&old);
        entry.key_through.remove(&old);
        match new_name.or(name) {
            Some(name) => {
                entry.key_names.insert(fresh, name);
            }
            None => {
                entry.key_names.remove(&fresh);
            }
        }
    }
    update_key_count(&state, &id, count, parts)
}

/// Name (or rename) the key with this fingerprint. An empty name removes it.
#[tauri::command]
pub async fn rename_api_key(
    state: State<'_, AppState>,
    id: String,
    fingerprint: String,
    name: String,
) -> CommandResult<SettingsView> {
    let name = secrets::name_to_store(&name).map_err(CommandError::Settings)?;
    {
        let mut s = state.settings();
        let entry = s.provider_mut(&id).ok_or_else(|| not_found(&id))?;
        if name.is_empty() {
            entry.key_names.remove(&fingerprint);
        } else {
            entry.key_names.insert(fingerprint, name);
        }
        state.persist(&s)?;
    }
    Ok(state.settings_view())
}

/// Translate the test sentence with one key of the pool only.
#[tauri::command]
pub async fn test_api_key(
    state: State<'_, AppState>,
    id: String,
    fingerprint: String,
) -> CommandResult<TestAnswer> {
    let entry = state
        .settings()
        .provider(&id)
        .cloned()
        .ok_or_else(|| not_found(&id))?;
    let store = state.secrets.clone();
    let account = id.clone();
    let vouched = entry.vouched();
    let key = blocking(move || secrets::get_keys(store.as_ref(), &account, vouched))
        .await?
        .into_iter()
        .find(|k| key_fingerprint(k) == fingerprint)
        .ok_or_else(|| CommandError::Settings("that key is no longer in the pool".into()))?;
    // Little reasoning: a short question costs a few tokens.
    let provider = biwrite_providers::build_http(
        &biwrite_providers::light_config(&entry.config),
        std::sync::Arc::new(move || Ok(vec![key.clone()])),
        state.prompts(),
        state.request_log.clone(),
    )?;
    // A key that answers got through.
    state.squeeze.join(&entry, &provider);
    ask_test(provider.as_ref(), Duration::from_secs(120)).await
}

/// Paragraphs per translation request (1: one per request).
#[tauri::command]
pub async fn set_batch_size(
    state: State<'_, AppState>,
    size: usize,
) -> CommandResult<SettingsView> {
    let size = size.clamp(1, biwrite_engine::MAX_BATCH);
    {
        let mut s = state.settings();
        s.batch_size = size;
        state.persist(&s)?;
    }
    state.engine.set_batch_size(size);
    Ok(state.settings_view())
}

/// Seconds an edited paragraph stays unchanged before it is translated
/// (0 translates at once).
#[tauri::command]
pub async fn set_translate_delay(
    state: State<'_, AppState>,
    seconds: u64,
) -> CommandResult<SettingsView> {
    let seconds = seconds.min(crate::settings::MAX_TRANSLATE_DELAY);
    {
        let mut s = state.settings();
        s.translate_delay = seconds;
        state.persist(&s)?;
    }
    state.engine.set_edit_delay(Duration::from_secs(seconds));
    Ok(state.settings_view())
}

/// Let parallel requests follow the key pool (keys × per-key limit).
#[tauri::command]
pub async fn set_match_pool(state: State<'_, AppState>, on: bool) -> CommandResult<SettingsView> {
    let effective = {
        let mut s = state.settings();
        s.match_pool = on;
        state.persist(&s)?;
        s.effective_concurrency()
    };
    state.engine.set_concurrency(effective);
    Ok(state.settings_view())
}

/// Look for a newer release at startup, or not.
#[tauri::command]
pub async fn set_check_updates(
    state: State<'_, AppState>,
    on: bool,
) -> CommandResult<SettingsView> {
    {
        let mut s = state.settings();
        s.check_updates = on;
        state.persist(&s)?;
    }
    Ok(state.settings_view())
}

/// Set key `fingerprint` of provider `id` aside (`ignored`), or take it
/// back: a key set aside takes no request, squeeze or keep-alive.
#[tauri::command]
pub async fn set_key_ignored(
    state: State<'_, AppState>,
    id: String,
    fingerprint: String,
    ignored: bool,
) -> CommandResult<SettingsView> {
    let _keys = state.keys_lock.lock().await;
    let is_active = {
        let mut s = state.settings();
        let entry = s.provider_mut(&id).ok_or_else(|| not_found(&id))?;
        let changed = if ignored {
            entry.key_ignored.insert(fingerprint.clone())
        } else {
            entry.key_ignored.remove(&fingerprint)
        };
        if !changed {
            return Ok(state.settings_view());
        }
        state.persist(&s)?;
        s.active_provider == id
    };
    if ignored {
        state.squeeze.set_aside(&id, &fingerprint);
    } else {
        state.squeeze.take_back(&id);
    }
    // Providers read their keys again, without the ones set aside.
    state.invalidate_assistant();
    if is_active {
        state.apply_active_provider()?;
    }
    Ok(state.settings_view())
}

/// The mean minutes between two keep-alives of a key, and seconds between
/// two tries of a key in a squeeze.
#[tauri::command]
pub async fn set_pacing(
    state: State<'_, AppState>,
    keep_alive_minutes: f64,
    squeeze_seconds: f64,
    squeeze_enough: usize,
) -> CommandResult<SettingsView> {
    let (keep, pace, enough) = (
        keep_alive_minutes.clamp(1.0, 30.0),
        squeeze_seconds.clamp(1.0, 120.0),
        squeeze_enough.clamp(1, crate::settings::MAX_SQUEEZE_ENOUGH),
    );
    {
        let mut s = state.settings();
        s.keep_alive_minutes = keep;
        s.squeeze_seconds = pace;
        s.squeeze_enough = enough;
        state.persist(&s)?;
    }
    state.squeeze.set_means(keep, pace);
    state.squeeze.set_enough(enough);
    Ok(state.settings_view())
}

/// The reasoning effort of the tries of a squeeze and of keep-alives.
#[tauri::command]
pub async fn set_squeeze_effort(
    state: State<'_, AppState>,
    effort: biwrite_providers::Effort,
) -> CommandResult<SettingsView> {
    {
        let mut s = state.settings();
        s.squeeze_effort = effort;
        state.persist(&s)?;
    }
    state.squeeze.set_effort(effort);
    Ok(state.settings_view())
}

/// Keep the keys that got through to a crowded relay alive, or not.
#[tauri::command]
pub async fn set_keep_alive(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    on: bool,
) -> CommandResult<SettingsView> {
    {
        let mut s = state.settings();
        s.keep_alive = on;
        state.persist(&s)?;
    }
    crate::squeeze_commands::keep_alive_changed(&app, on);
    Ok(state.settings_view())
}

/// Keep the API keys in the local key file or in the keychain, moving them
/// there (the keychain may ask for its password while they move).
#[tauri::command]
pub async fn set_keys_in_file(state: State<'_, AppState>, on: bool) -> CommandResult<SettingsView> {
    let _keys = state.keys_lock.lock().await;
    let accounts: Vec<String> = state
        .settings()
        .providers
        .iter()
        .flat_map(|p| crate::secrets::accounts_of(&p.config.id))
        .collect();
    let store = state.secrets.clone();
    blocking(move || store.move_keys(on, &accounts)).await?;
    {
        let mut s = state.settings();
        s.keys_in_file = on;
        state.persist(&s)?;
    }
    Ok(state.settings_view())
}

/// Keep the keys alive however long the author is away, or only within
/// half an hour of their last use.
#[tauri::command]
pub async fn set_keep_alive_forever(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    on: bool,
) -> CommandResult<SettingsView> {
    let keeping = {
        let mut s = state.settings();
        s.keep_alive_forever = on;
        state.persist(&s)?;
        s.keep_alive
    };
    state.squeeze.set_forever(on);
    crate::squeeze_commands::keep_alive_changed(&app, keeping);
    Ok(state.settings_view())
}

/// Close to the tray (hide the window) or quit when the window is closed.
#[tauri::command]
pub async fn set_close_to_tray(
    state: State<'_, AppState>,
    tray: State<'_, crate::tray::Tray>,
    on: bool,
) -> CommandResult<SettingsView> {
    {
        let mut s = state.settings();
        s.close_to_tray = on;
        state.persist(&s)?;
    }
    tray.set_visible(on);
    Ok(state.settings_view())
}

/// The tray menu follows the interface language ("zh" or "en").
#[tauri::command]
pub fn set_tray_language(tray: State<'_, crate::tray::Tray>, lang: String) {
    tray.set_language(lang == "zh");
}

/// Remove key `number` (1-based) of the pool, if it still ends in `tail`.
#[tauri::command]
pub async fn remove_api_key(
    state: State<'_, AppState>,
    id: String,
    number: usize,
    tail: String,
) -> CommandResult<SettingsView> {
    let _keys = state.keys_lock.lock().await;
    let vouched = state
        .settings()
        .provider(&id)
        .ok_or_else(|| not_found(&id))?
        .vouched();
    state.settings_writable()?;
    let store = state.secrets.clone();
    let account = id.clone();
    let count = blocking(move || {
        let stored = secrets::read_stored(store.as_ref(), &account)?;
        let mut pool = stored.keys(vouched);
        let removed = match number.checked_sub(1).filter(|&i| i < pool.len()) {
            Some(i) if key_tail(&pool[i]) == tail => pool.remove(i),
            _ => return Err("the keys have changed; reopen the settings and try again".into()),
        };
        let parts = secrets::set_keys(store.as_ref(), &account, &pool, Some(&stored))?;
        Ok((pool.len(), parts, key_fingerprint(&removed)))
    })
    .await?;
    let (count, parts, fingerprint) = count;
    if let Some(entry) = state.settings().provider_mut(&id) {
        entry.key_names.remove(&fingerprint);
        entry.key_through.remove(&fingerprint);
    }
    update_key_count(&state, &id, count, parts)
}

#[tauri::command]
pub async fn clear_api_key(state: State<'_, AppState>, id: String) -> CommandResult<SettingsView> {
    let _keys = state.keys_lock.lock().await;
    state.settings_writable()?;
    let store = state.secrets.clone();
    let account = id.clone();
    blocking(move || secrets::delete_keys(store.as_ref(), &account)).await?;
    update_key_count(&state, &id, 0, 0)
}

/// State of each key of provider `id` (ready, cooling down, rejected), once
/// a provider in use has read them. `None` before the first request.
#[tauri::command]
pub async fn key_status(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<Option<Vec<KeyStatus>>> {
    Ok(state.key_status(&id))
}

fn update_key_count(
    state: &AppState,
    id: &str,
    count: usize,
    parts: usize,
) -> CommandResult<SettingsView> {
    // The squeeze tries the keys it read when it started.
    state.squeeze.keys_changed(id);
    let is_active = {
        let mut s = state.settings();
        if !s.set_key_count(id, count, parts) {
            return Err(not_found(id));
        }
        state.persist(&s)?;
        s.active_provider == id
    };
    // Fresh providers read the new keys.
    state.invalidate_assistant();
    if is_active {
        state.apply_active_provider()?;
    }
    Ok(state.settings_view())
}

/// Accept and Revert with the model's help and a build, or as they are.
#[tauri::command]
pub async fn set_assisted_apply(
    state: State<'_, AppState>,
    on: bool,
) -> CommandResult<SettingsView> {
    {
        let mut s = state.settings();
        s.assisted_apply = on;
        state.persist(&s)?;
    }
    Ok(state.settings_view())
}

/// Choose the assistant's provider (empty: use the translation provider).
#[tauri::command]
pub async fn set_assistant_provider(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<SettingsView> {
    {
        let mut s = state.settings();
        if !id.is_empty() {
            let entry = s.provider(&id).ok_or_else(|| not_found(&id))?;
            if entry.config.kind == ProviderKind::Mock {
                return Err(CommandError::Settings(
                    "the offline mock cannot run the assistant".into(),
                ));
            }
        }
        s.assistant_provider = id;
        state.persist(&s)?;
    }
    state.invalidate_assistant();
    Ok(state.settings_view())
}

#[tauri::command]
pub async fn set_active_provider(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<SettingsView> {
    {
        let mut s = state.settings();
        if s.provider(&id).is_none() {
            return Err(not_found(&id));
        }
        s.active_provider = id;
        state.persist(&s)?;
    }
    state.apply_active_provider()?;
    Ok(state.settings_view())
}

/// Translate one sentence with provider `id` (not necessarily active).
#[tauri::command]
pub async fn test_provider(state: State<'_, AppState>, id: String) -> CommandResult<TestAnswer> {
    let entry = state
        .settings()
        .provider(&id)
        .cloned()
        .ok_or_else(|| not_found(&id))?;
    if entry.config.kind == ProviderKind::Mock {
        // The offline mock answers by reversing the question.
        let built = translator_for(
            &entry,
            &state.secrets,
            state.prompts(),
            state.request_log.clone(),
        )?;
        let (_, question) = biwrite_providers::test_question();
        let request = TranslationRequest {
            source: question.clone(),
            ..Default::default()
        };
        let answer = built.translator.translate(&request, &|_| {}).await?.text;
        return Ok(TestAnswer { question, answer });
    }
    // Little reasoning: a short question costs a few tokens.
    let provider = biwrite_providers::build_http(
        &biwrite_providers::light_config(&entry.config),
        crate::provider_state::key_fn(&state.secrets, &entry),
        state.prompts(),
        state.request_log.clone(),
    )?;
    state.squeeze.join(&entry, &provider);
    ask_test(provider.as_ref(), Duration::from_secs(90)).await
}

#[tauri::command]
pub async fn list_provider_models(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<Vec<String>> {
    let entry = state
        .settings()
        .provider(&id)
        .cloned()
        .ok_or_else(|| not_found(&id))?;
    let key = if entry.config.kind.needs_key() {
        let store = state.secrets.clone();
        let account = id.clone();
        blocking(move || secrets::get_keys(store.as_ref(), &account, Vouched::FIRST))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| CommandError::Settings("add an API key first".into()))?
    } else {
        String::new()
    };
    Ok(list_models(&entry.config, &key).await?)
}

#[tauri::command]
pub async fn set_concurrency(
    state: State<'_, AppState>,
    concurrency: usize,
) -> CommandResult<SettingsView> {
    let n = concurrency.clamp(1, crate::settings::MAX_CONCURRENCY);
    let effective = {
        let mut s = state.settings();
        s.concurrency = n;
        state.persist(&s)?;
        s.effective_concurrency()
    };
    state.engine.set_concurrency(effective);
    Ok(state.settings_view())
}

#[tauri::command]
pub async fn set_doc_note(state: State<'_, AppState>, note: String) -> CommandResult<()> {
    state.set_doc_note(note)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptView {
    pub text: String,
    pub is_default: bool,
    pub path: String,
}

fn prompt_view(state: &AppState, direction: Direction) -> PromptView {
    let path = state.paths.prompts.join(prompt_file_name(direction));
    let text = state.prompts().system_prompt(direction);
    PromptView {
        is_default: text == default_prompt(direction),
        text,
        path: path.display().to_string(),
    }
}

#[tauri::command]
pub async fn get_prompt(
    state: State<'_, AppState>,
    direction: Direction,
) -> CommandResult<PromptView> {
    Ok(prompt_view(&state, direction))
}

/// Save the system prompt file (empty text restores the default).
#[tauri::command]
pub async fn save_prompt(
    state: State<'_, AppState>,
    direction: Direction,
    text: String,
) -> CommandResult<PromptView> {
    let text = text.trim();
    let text = if text.is_empty() {
        default_prompt(direction)
    } else {
        text
    };
    let path = state.paths.prompts.join(prompt_file_name(direction));
    std::fs::create_dir_all(&state.paths.prompts)
        .map_err(|e| CommandError::io(&state.paths.prompts, e))?;
    crate::files::write_atomic_blocking(&path, format!("{text}\n").as_bytes())?;
    Ok(prompt_view(&state, direction))
}

/// Show the prompt files in Finder / Explorer.
#[tauri::command]
pub async fn reveal_prompts(state: State<'_, AppState>) -> CommandResult<()> {
    crate::files::reveal(&state.paths.prompts)
}

#[cfg(test)]
mod tests {
    use biwrite_providers::Effort;

    use super::*;
    use crate::settings::same_destination;

    /// Answers every request with its own question, said twice and padded.
    struct Echo;

    impl ChatModel for Echo {
        fn label(&self) -> String {
            "echo".into()
        }

        fn complete<'a>(
            &'a self,
            request: &'a ChatRequest,
            _on_partial: biwrite_engine::PartialFn<'a>,
        ) -> biwrite_engine::BoxFuture<
            'a,
            Result<biwrite_providers::ChatOutput, biwrite_engine::TranslateError>,
        > {
            Box::pin(async move {
                assert_eq!(request.purpose, "test");
                Ok(biwrite_providers::ChatOutput {
                    text: format!("  {}{}  ", request.user, "!".repeat(600)),
                    usage: Default::default(),
                })
            })
        }
    }

    #[tokio::test]
    async fn a_test_asks_a_small_question_and_gives_the_answer() {
        let a = ask_test(&Echo, Duration::from_secs(5)).await.unwrap();
        let b = ask_test(&Echo, Duration::from_secs(5)).await.unwrap();
        assert!(!a.question.is_empty());
        assert!(a.answer.starts_with(&a.question), "trimmed: {}", a.answer);
        assert_eq!(a.answer.chars().count(), TEST_ANSWER_CHARS, "cut short");
        assert_ne!(
            (a.question.clone(), b.question.clone()),
            (String::new(), String::new())
        );
    }

    fn cfg(kind: ProviderKind, base_url: &str) -> ProviderConfig {
        ProviderConfig {
            id: "p".into(),
            name: "n".into(),
            kind,
            base_url: base_url.into(),
            model: "m".into(),
            temperature: 0.0,
            effort: Effort::Low,
            wire_api: biwrite_providers::WireApi::Chat,
            service_tier: None,
            key_concurrency: None,
            max_retries: None,
            long_context: None,
        }
    }

    #[test]
    fn keys_stay_with_the_same_host_on_any_api_type() {
        let relay = cfg(ProviderKind::OpenaiCompatible, "https://anyrouter.top/v1");
        assert!(keeps_keys(
            &relay,
            &cfg(ProviderKind::Anthropic, "https://anyrouter.top")
        ));
        assert!(!keeps_keys(
            &relay,
            &cfg(ProviderKind::Anthropic, "https://api.anthropic.com")
        ));
        assert!(!keeps_keys(
            &relay,
            &cfg(ProviderKind::OpenaiCompatible, "http://anyrouter.top/v1")
        ));
    }

    #[test]
    fn keys_only_stay_with_the_same_api_type_and_origin() {
        let openai = cfg(ProviderKind::OpenaiCompatible, "https://api.openai.com/v1");
        assert!(same_destination(
            &openai,
            &cfg(ProviderKind::OpenaiCompatible, "https://api.openai.com/v2")
        ));
        assert!(!same_destination(
            &openai,
            &cfg(
                ProviderKind::OpenaiCompatible,
                "https://openrouter.ai/api/v1"
            )
        ));
        assert!(!same_destination(
            &openai,
            &cfg(ProviderKind::OpenaiCompatible, "http://api.openai.com/v1")
        ));
        assert!(!same_destination(
            &openai,
            &cfg(
                ProviderKind::OpenaiCompatible,
                "https://api.openai.com:8443/v1"
            )
        ));
        assert!(!same_destination(
            &openai,
            &cfg(ProviderKind::Anthropic, "https://api.openai.com/v1")
        ));
    }
}
