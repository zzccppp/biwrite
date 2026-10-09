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
use crate::secrets::{self, MAX_KEYS, NamedKey};
use crate::settings::{MOCK_ID, ProviderEntry, SettingsView};
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

/// Same API type and origin (scheme, host, port): the stored key may be reused.
fn same_destination(a: &ProviderConfig, b: &ProviderConfig) -> bool {
    let origin = |c: &ProviderConfig| {
        reqwest::Url::parse(&c.base_url)
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_default()
    };
    a.kind == b.kind && origin(a) == origin(b)
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
    // A key belongs to one API type and host: never send it elsewhere.
    let moved = state
        .settings()
        .provider(&id)
        .is_some_and(|old| old.has_key && !same_destination(&old.config, &config));
    if moved {
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
                }
            }
            None if provider.id.is_empty() => s.providers.push(ProviderEntry {
                config,
                has_key: false,
                key_count: 0,
                key_names: Default::default(),
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
    if state.settings().provider(&id).is_none() {
        return Err(not_found(&id));
    }
    let keys: Vec<String> = named.iter().map(|k| k.key.clone()).collect();
    let count = keys.len();
    let store = state.secrets.clone();
    let account = id.clone();
    blocking(move || secrets::set_keys(store.as_ref(), &account, &keys)).await?;
    {
        let mut s = state.settings();
        if let Some(entry) = s.provider_mut(&id) {
            entry.key_names.clear();
        }
    }
    remember_names(&state, &id, &named);
    update_key_count(&state, &id, count)
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
    if state.settings().provider(&id).is_none() {
        return Err(not_found(&id));
    }
    let added: Vec<String> = named.iter().map(|k| k.key.clone()).collect();
    let store = state.secrets.clone();
    let account = id.clone();
    let count = blocking(move || {
        let mut pool = secrets::get_keys(store.as_ref(), &account)?;
        for key in added {
            if !pool.contains(&key) {
                pool.push(key);
            }
        }
        if pool.len() > MAX_KEYS {
            return Err(format!("at most {MAX_KEYS} keys per provider"));
        }
        secrets::set_keys(store.as_ref(), &account, &pool)?;
        Ok(pool.len())
    })
    .await?;
    remember_names(&state, &id, &named);
    update_key_count(&state, &id, count)
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
    let names = state
        .settings()
        .provider(&id)
        .ok_or_else(|| not_found(&id))?
        .key_names
        .clone();
    let store = state.secrets.clone();
    let account = id.clone();
    let keys = blocking(move || secrets::get_keys(store.as_ref(), &account)).await?;
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
    {
        let mut s = state.settings();
        let entry = s.provider_mut(&id).ok_or_else(|| not_found(&id))?;
        let name: String = name.trim().chars().take(40).collect();
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
    let key = blocking(move || secrets::get_keys(store.as_ref(), &account))
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
pub async fn set_check_updates(state: State<'_, AppState>, on: bool) -> CommandResult<SettingsView> {
    {
        let mut s = state.settings();
        s.check_updates = on;
        state.persist(&s)?;
    }
    Ok(state.settings_view())
}

/// Remove key `number` (1-based) of the pool, if it still ends in `tail`.
#[tauri::command]
pub async fn remove_api_key(
    state: State<'_, AppState>,
    id: String,
    number: usize,
    tail: String,
) -> CommandResult<SettingsView> {
    if state.settings().provider(&id).is_none() {
        return Err(not_found(&id));
    }
    let store = state.secrets.clone();
    let account = id.clone();
    let count = blocking(move || {
        let mut pool = secrets::get_keys(store.as_ref(), &account)?;
        let removed = match number.checked_sub(1).filter(|&i| i < pool.len()) {
            Some(i) if key_tail(&pool[i]) == tail => pool.remove(i),
            _ => return Err("the keys have changed; reopen the settings and try again".into()),
        };
        secrets::set_keys(store.as_ref(), &account, &pool)?;
        Ok((pool.len(), key_fingerprint(&removed)))
    })
    .await?;
    let (count, fingerprint) = count;
    if let Some(entry) = state.settings().provider_mut(&id) {
        entry.key_names.remove(&fingerprint);
    }
    update_key_count(&state, &id, count)
}

#[tauri::command]
pub async fn clear_api_key(state: State<'_, AppState>, id: String) -> CommandResult<SettingsView> {
    let store = state.secrets.clone();
    let account = id.clone();
    blocking(move || secrets::delete_keys(store.as_ref(), &account)).await?;
    update_key_count(&state, &id, 0)
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

fn update_key_count(state: &AppState, id: &str, count: usize) -> CommandResult<SettingsView> {
    let is_active = {
        let mut s = state.settings();
        if !s.set_key_count(id, count) {
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
        blocking(move || secrets::get_keys(store.as_ref(), &account))
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
