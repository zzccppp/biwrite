//! Listing a provider's models for the settings UI.

use biwrite_engine::TranslateError;
use serde_json::Value;

use crate::anthropic::API_VERSION;
use crate::config::{ProviderConfig, ProviderKind};
use crate::http::{client, error_from_response, invalid, network, redact, send};

/// Largest `/models` listing body accepted (4 MB).
const MAX_MODELS_BODY: usize = 4 * 1024 * 1024;

/// Model IDs offered by the provider, sorted.
pub async fn list_models(
    config: &ProviderConfig,
    key: &str,
) -> Result<Vec<String>, TranslateError> {
    let client = client(&config.base_url)?;
    let request = match config.kind {
        ProviderKind::Mock => return Ok(vec!["reverse".to_owned()]),
        ProviderKind::OpenaiCompatible => {
            let req = client.get(format!("{}/models", config.base_url));
            if key.is_empty() {
                req
            } else {
                req.bearer_auth(key)
            }
        }
        ProviderKind::Anthropic => {
            let req = client
                .get(format!(
                    "{}/v1/models?limit=1000",
                    crate::config::anthropic_base(&config.base_url)
                ))
                .header("anthropic-version", API_VERSION);
            if key.is_empty() {
                req
            } else {
                req.header("x-api-key", key)
            }
        }
    };
    let mut resp = send(request.timeout(std::time::Duration::from_secs(30)), key).await?;
    if !resp.status().is_success() {
        return Err(error_from_response(resp, key).await);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| network(&e, key))? {
        if bytes.len() + chunk.len() > MAX_MODELS_BODY {
            return Err(invalid("model list response is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|e| invalid(redact(&format!("bad JSON: {e}"), key)))?;
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
