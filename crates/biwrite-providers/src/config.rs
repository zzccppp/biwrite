//! Provider configuration (persisted in the app settings; never contains keys).

use serde::{Deserialize, Serialize};

/// Highest temperature offered in settings (translation wants low variance).
pub const MAX_TEMPERATURE: f32 = 0.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// `POST {base_url}/chat/completions` (OpenAI, DeepSeek, Qwen, Kimi, OpenRouter, …).
    OpenaiCompatible,
    /// Anthropic Messages API, `POST {base_url}/v1/messages`.
    Anthropic,
    /// Offline mock (reverses text). Needs no key.
    Mock,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai",
            Self::Anthropic => "anthropic",
            Self::Mock => "mock",
        }
    }

    pub fn needs_key(self) -> bool {
        !matches!(self, Self::Mock)
    }
}

/// Anthropic `output_config.effort`. Translation is a simple task, so `low`
/// is the default; `Default` sends nothing (the model's own default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    #[default]
    Low,
    Medium,
    High,
    /// Don't send the parameter.
    Default,
}

impl Effort {
    pub fn as_param(self) -> Option<&'static str> {
        match self {
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Default => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    /// Stable identifier (also the keychain account name).
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub temperature: f32,
    /// Anthropic only.
    #[serde(default)]
    pub effort: Effort,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("give the provider a name")]
    MissingName,
    #[error("choose a model")]
    MissingModel,
    #[error("the base URL is not valid: {0}")]
    BadUrl(String),
    #[error("the base URL must use https (http is only allowed for localhost)")]
    InsecureUrl,
    #[error("the base URL must not contain credentials; the key goes in the key field")]
    CredentialsInUrl,
}

impl ProviderConfig {
    /// Normalize and check a config coming from the settings form.
    pub fn validated(mut self) -> Result<Self, ConfigError> {
        self.name = self.name.trim().to_owned();
        self.model = self.model.trim().to_owned();
        self.base_url = self.base_url.trim().trim_end_matches('/').to_owned();
        self.temperature = if self.temperature.is_finite() {
            self.temperature.clamp(0.0, MAX_TEMPERATURE)
        } else {
            0.0
        };
        if self.name.is_empty() {
            return Err(ConfigError::MissingName);
        }
        if self.kind == ProviderKind::Mock {
            return Ok(self);
        }
        if self.model.is_empty() {
            return Err(ConfigError::MissingModel);
        }
        let url =
            reqwest::Url::parse(&self.base_url).map_err(|e| ConfigError::BadUrl(e.to_string()))?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ConfigError::CredentialsInUrl);
        }
        match url.scheme() {
            "https" => {}
            "http" if is_loopback(&url) => {}
            "http" => return Err(ConfigError::InsecureUrl),
            other => return Err(ConfigError::BadUrl(format!("unsupported scheme {other}"))),
        }
        Ok(self)
    }

    /// Identity used in cache keys: same model name on another host is a
    /// different provider.
    pub fn cache_identity(&self) -> String {
        match self.kind {
            ProviderKind::Mock => "mock".to_owned(),
            kind => format!("{}:{}", kind.as_str(), self.base_url),
        }
    }
}

pub(crate) fn is_loopback(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    )
}

/// A starting point offered by "Add provider".
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub name: &'static str,
    pub kind: ProviderKind,
    pub base_url: &'static str,
    /// Suggested model; empty means "fetch the list and pick one".
    pub model: &'static str,
}

pub fn presets() -> Vec<Preset> {
    let openai = ProviderKind::OpenaiCompatible;
    vec![
        Preset {
            name: "Anthropic",
            kind: ProviderKind::Anthropic,
            base_url: "https://api.anthropic.com",
            model: "claude-opus-5-5",
        },
        Preset {
            name: "OpenAI",
            kind: openai,
            base_url: "https://api.openai.com/v1",
            model: "",
        },
        Preset {
            name: "DeepSeek",
            kind: openai,
            base_url: "https://api.deepseek.com/v1",
            model: "deepseek-chat",
        },
        Preset {
            name: "Qwen (DashScope)",
            kind: openai,
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
            model: "qwen-plus",
        },
        Preset {
            name: "Kimi (Moonshot)",
            kind: openai,
            base_url: "https://api.moonshot.cn/v1",
            model: "",
        },
        Preset {
            name: "OpenRouter",
            kind: openai,
            base_url: "https://openrouter.ai/api/v1",
            model: "",
        },
        Preset {
            name: "Local (OpenAI-compatible)",
            kind: openai,
            base_url: "http://localhost:11434/v1",
            model: "",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(base_url: &str) -> ProviderConfig {
        ProviderConfig {
            id: "p1".into(),
            name: " DeepSeek ".into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: base_url.into(),
            model: "deepseek-chat".into(),
            temperature: 0.9,
            effort: Effort::Low,
        }
    }

    #[test]
    fn validation_normalizes_and_rejects() {
        let ok = cfg("https://api.deepseek.com/v1/").validated().unwrap();
        assert_eq!(ok.base_url, "https://api.deepseek.com/v1");
        assert_eq!(ok.name, "DeepSeek");
        assert_eq!(ok.temperature, MAX_TEMPERATURE);
        assert!(cfg("http://localhost:11434/v1").validated().is_ok());
        assert!(cfg("http://127.0.0.1:8080/v1").validated().is_ok());
        assert_eq!(
            cfg("http://api.example.com/v1").validated(),
            Err(ConfigError::InsecureUrl)
        );
        assert_eq!(
            cfg("https://user:pw@api.example.com/v1").validated(),
            Err(ConfigError::CredentialsInUrl)
        );
        assert!(matches!(
            cfg("not a url").validated(),
            Err(ConfigError::BadUrl(_))
        ));
        assert!(matches!(
            cfg("ftp://x.com").validated(),
            Err(ConfigError::BadUrl(_))
        ));
    }

    #[test]
    fn cache_identity_includes_host() {
        let a = cfg("https://api.deepseek.com/v1").validated().unwrap();
        let b = cfg("https://openrouter.ai/api/v1").validated().unwrap();
        assert_ne!(a.cache_identity(), b.cache_identity());
    }

    #[test]
    fn config_serializes_without_secrets() {
        let json = serde_json::to_string(&cfg("https://x.com")).unwrap();
        assert!(json.contains("\"baseUrl\""));
        assert!(!json.to_lowercase().contains("key"));
    }
}
