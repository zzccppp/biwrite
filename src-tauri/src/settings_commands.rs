//! Settings commands: providers, API keys, models, prompts, concurrency,
//! document note, assistant provider. Responses never include an API key:
//! the key commands are write-only, views carry key counts, and key status
//! names a key by its position and last four characters.

use std::time::Duration;

use biwrite_core::Direction;
use biwrite_engine::TranslationRequest;
use biwrite_providers::prompt::{default_prompt, prompt_file_name};
use biwrite_providers::{
    KeyStatus, ProviderConfig, ProviderKind, key_fingerprint, key_tail, list_models,
};
use serde::Serialize;
use tauri::State;

use crate::error::{CommandError, CommandResult};
use crate::provider_state::translator_for;
use crate::secrets::{self, MAX_KEYS, NamedKey, Vouched};
use crate::settings::{MOCK_ID, ProviderEntry, SettingsView, same_destination};
use crate::state::AppState;

const TEST_SENTENCE: &str =
    "Graph neural networks can learn new tasks from a few examples in context.";

fn not_found(id: &str) -> CommandError {
    CommandError::Settings(format!("unknown provider {id}"))
}

/// Run blocking keychain work off the async runtime.
async fn blocking<T: Send + 'static>(
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
    // A key belongs to one API type and host: never send it elsewhere.
    // (Even without a key in the settings: a version without key pools may
    // have left some behind.)
    let moved = state
        .settings()
        .provider(&id)
        .is_some_and(|old| !same_destination(&old.config, &config));
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
                key_parts: 0,
            }),
            None => return Err(not_found(&id)),
        }
        state.persist(&s)?;
        s.active_provider == id
    };
    state.invalidate_assistant();
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
}

/// The keys of provider `id` with their names and live state. Reads the
/// keychain, so it is only called when the key manager opens.
#[tauri::command]
pub async fn list_api_keys(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<Vec<KeyEntryView>> {
    let (names, pool) = {
        let s = state.settings();
        let entry = s.provider(&id).ok_or_else(|| not_found(&id))?;
        (entry.key_names.clone(), entry.vouched())
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
                fingerprint,
            }
        })
        .collect())
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
) -> CommandResult<String> {
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
    let provider = biwrite_providers::build_http(
        &entry.config,
        std::sync::Arc::new(move || Ok(vec![key.clone()])),
        state.prompts(),
        state.request_log.clone(),
    )?;
    let request = TranslationRequest {
        source: TEST_SENTENCE.to_owned(),
        ..Default::default()
    };
    let run = biwrite_engine::Translator::translate(provider.as_ref(), &request, &|_| {});
    match tokio::time::timeout(Duration::from_secs(120), run).await {
        Ok(result) => Ok(result?.text),
        Err(_) => Err(CommandError::Settings(
            "no answer within 120 seconds".into(),
        )),
    }
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
pub async fn test_provider(state: State<'_, AppState>, id: String) -> CommandResult<String> {
    let entry = state
        .settings()
        .provider(&id)
        .cloned()
        .ok_or_else(|| not_found(&id))?;
    let built = translator_for(
        &entry,
        &state.secrets,
        state.prompts(),
        state.request_log.clone(),
    )?;
    let request = TranslationRequest {
        source: TEST_SENTENCE.to_owned(),
        ..Default::default()
    };
    let run = built.translator.translate(&request, &|_| {});
    match tokio::time::timeout(Duration::from_secs(90), run).await {
        Ok(result) => Ok(result?.text),
        Err(_) => Err(CommandError::Settings("no answer within 90 seconds".into())),
    }
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
        }
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
