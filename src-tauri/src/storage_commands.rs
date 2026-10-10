//! The translation cache (what it holds, clearing it) and the log folder.

use biwrite_core::Direction;
use biwrite_engine::CacheStats;
use serde::Serialize;
use tauri::State;

use crate::error::{CommandError, CommandResult};
use crate::settings::{AppSettings, MOCK_ID};
use crate::state::AppState;

/// Cache contents for the settings drawer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheView {
    pub entries: u64,
    pub bytes: u64,
    /// The database file; `None` for the temporary in-memory cache.
    pub location: Option<String>,
    pub groups: Vec<CacheGroupView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheGroupView {
    /// The provider identity stored in the cache (unique with model and
    /// direction).
    pub id: String,
    /// The configured provider's name, or the API host if it was removed.
    pub provider: String,
    pub model: String,
    pub direction: Direction,
    pub entries: u64,
}

/// A readable name for a cache provider identity (`kind:base_url`).
fn provider_label(identity: &str, settings: &AppSettings) -> String {
    if identity == MOCK_ID {
        return "Mock".to_owned();
    }
    if let Some(p) = settings
        .providers
        .iter()
        .find(|p| p.config.cache_identity() == identity)
    {
        return p.config.name.clone();
    }
    let url = identity.split_once(':').map_or(identity, |(_, url)| url);
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| identity.to_owned())
}

fn view(stats: CacheStats, state: &AppState) -> CacheView {
    let settings = state.settings();
    CacheView {
        entries: stats.entries,
        bytes: stats.bytes,
        location: state.paths.cache.as_ref().map(|p| p.display().to_string()),
        groups: stats
            .groups
            .into_iter()
            .map(|g| CacheGroupView {
                provider: provider_label(&g.provider, &settings),
                id: g.provider,
                model: g.model,
                direction: g.direction,
                entries: g.entries,
            })
            .collect(),
    }
}

/// Run cache work (SQLite IO) off the async runtime.
async fn with_cache<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&dyn biwrite_engine::TranslationCache) -> Result<T, biwrite_engine::CacheError>
    + Send
    + 'static,
) -> CommandResult<T> {
    let cache = state.cache.clone();
    tokio::task::spawn_blocking(move || f(cache.as_ref()))
        .await
        .map_err(|e| CommandError::Task(e.to_string()))?
        .map_err(|e| CommandError::Settings(e.to_string()))
}

#[tauri::command]
pub async fn get_cache(state: State<'_, AppState>) -> CommandResult<CacheView> {
    let stats = with_cache(&state, |c| c.stats()).await?;
    Ok(view(stats, &state))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearedView {
    /// Entries deleted.
    pub removed: u64,
    pub cache: CacheView,
}

/// Delete every cached translation. Translations on screen stay; anything
/// else is translated again when needed.
#[tauri::command]
pub async fn clear_cache(state: State<'_, AppState>) -> CommandResult<ClearedView> {
    let removed = with_cache(&state, |c| c.clear()).await?;
    log::info!("cleared the translation cache ({removed} entries)");
    let stats = with_cache(&state, |c| c.stats()).await?;
    Ok(ClearedView {
        removed,
        cache: view(stats, &state),
    })
}

/// Show the log folder in Finder / Explorer.
#[tauri::command]
pub async fn reveal_logs(state: State<'_, AppState>) -> CommandResult<()> {
    let dir = state
        .paths
        .logs
        .as_ref()
        .ok_or_else(|| CommandError::Settings("logs are not written to files".into()))?;
    crate::files::reveal(dir)
}

#[cfg(test)]
mod tests {
    use biwrite_providers::{Effort, ProviderConfig, ProviderKind, WireApi};

    use super::*;
    use crate::settings::ProviderEntry;

    #[test]
    fn cache_providers_get_readable_names() {
        let mut settings = AppSettings::default();
        let config = ProviderConfig {
            id: "p-1".into(),
            name: "DeepSeek".into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            temperature: 0.0,
            effort: Effort::Low,
            wire_api: WireApi::Chat,
            service_tier: None,
            key_concurrency: None,
            max_retries: None,
        };
        let identity = config.cache_identity();
        settings.providers.push(ProviderEntry {
            config,
            has_key: true,
            key_count: 1,
            key_names: Default::default(),
            key_parts: 0,
        });
        assert_eq!(provider_label(&identity, &settings), "DeepSeek");
        assert_eq!(provider_label("mock", &settings), "Mock");
        // A provider that was since removed: its API host.
        assert_eq!(
            provider_label("anthropic:https://api.anthropic.com", &settings),
            "api.anthropic.com"
        );
        assert_eq!(provider_label("odd", &settings), "odd");
    }
}
