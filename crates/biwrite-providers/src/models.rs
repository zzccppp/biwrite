//! Listing a provider's models for the settings UI.

use biwrite_engine::TranslateError;
use serde_json::Value;

use crate::anthropic::API_VERSION;
use crate::config::{ProviderConfig, ProviderKind};
use crate::http::{client, error_from_response, invalid, network, send};

/// Model IDs offered by the provider, sorted.
pub async fn list_models(
    config: &ProviderConfig,
    key: &str,
) -> Result<Vec<String>, TranslateError> {
    let client = client(&config.base_url)?;
    let request = match config.kind {
        ProviderKind::Mock => return Ok(vec!["reverse".to_owned()]),
        ProviderKind::OpenaiCompatible => client
            .get(format!("{}/models", config.base_url))
            .bearer_auth(key),
        ProviderKind::Anthropic => client
            .get(format!("{}/v1/models?limit=1000", config.base_url))
            .header("x-api-key", key)
            .header("anthropic-version", API_VERSION),
    };
    let resp = send(request.timeout(std::time::Duration::from_secs(30)), key).await?;
    if !resp.status().is_success() {
        return Err(error_from_response(resp, key).await);
    }
    let bytes = resp.bytes().await.map_err(|e| network(&e, key))?;
    let body: Value =
        serde_json::from_slice(&bytes).map_err(|e| invalid(format!("bad JSON: {e}")))?;
    let mut ids: Vec<String> = body["data"]
        .as_array()
        .ok_or_else(|| invalid("unexpected model list format"))?
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}
