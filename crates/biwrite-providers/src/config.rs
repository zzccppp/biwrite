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

/// Which OpenAI endpoint an OpenAI-compatible provider speaks.
///
/// Stored next to [`ProviderKind`] instead of as a new kind so that a
/// settings file written by this version still loads in older versions,
/// which ignore unknown fields but reject unknown enum values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireApi {
    /// `POST {base_url}/chat/completions`.
    #[default]
    Chat,
    /// `POST {base_url}/responses` (OpenAI Responses API, also used by Codex relays).
    Responses,
}

impl WireApi {
    pub fn is_chat(&self) -> bool {
        *self == Self::Chat
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
        }
    }
}

/// OpenAI `service_tier`. `Priority` is the fast lane ("fast" in Codex).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    Priority,
    Flex,
    Default,
}

impl ServiceTier {
    pub fn as_param(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Flex => "flex",
            Self::Default => "default",
        }
    }
}

/// Anthropic `output_config.effort` and OpenAI Responses `reasoning.effort`.
/// Translation is a simple task, so `low` is the default; `Default` sends
/// nothing (the model's own default).
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
    /// Anthropic and OpenAI Responses only.
    #[serde(default)]
    pub effort: Effort,
    /// OpenAI-compatible only. Omitted from the file when it is the default.
    #[serde(default, skip_serializing_if = "WireApi::is_chat")]
    pub wire_api: WireApi,
    /// OpenAI-compatible only. `None` sends nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
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
        if self.kind != ProviderKind::OpenaiCompatible {
            self.wire_api = WireApi::Chat;
            self.service_tier = None;
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

    /// The endpoint family the requests go to, for the request log.
    pub fn wire_label(&self) -> &'static str {
        match (self.kind, self.wire_api) {
            (ProviderKind::Mock, _) => "mock",
            (ProviderKind::Anthropic, _) => "anthropic",
            (ProviderKind::OpenaiCompatible, wire) => wire.as_str(),
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
    pub wire_api: WireApi,
    pub base_url: &'static str,
    /// Suggested model; empty means "fetch the list and pick one".
    pub model: &'static str,
    pub effort: Effort,
    pub service_tier: Option<ServiceTier>,
}

impl Preset {
    const fn new(name: &'static str, kind: ProviderKind, base_url: &'static str) -> Self {
        Self {
            name,
            kind,
            wire_api: WireApi::Chat,
            base_url,
            model: "",
            effort: Effort::Low,
            service_tier: None,
        }
    }

    const fn model(mut self, model: &'static str) -> Self {
        self.model = model;
        self
    }
}

/// AnyRouter relays Codex traffic: GPT models are served only on the
/// Responses API, at high effort on the priority tier (Codex "fast").
pub const ANYROUTER: Preset = Preset {
    name: "AnyRouter (GPT-6 Astra)",
    kind: ProviderKind::OpenaiCompatible,
    wire_api: WireApi::Responses,
    base_url: "https://anyrouter.top/v1",
    model: "gpt-6-astra",
    effort: Effort::High,
    service_tier: Some(ServiceTier::Priority),
};

pub fn presets() -> Vec<Preset> {
    let openai = ProviderKind::OpenaiCompatible;
    vec![
        ANYROUTER,
        Preset::new(
            "Anthropic",
            ProviderKind::Anthropic,
            "https://api.anthropic.com",
        )
        .model("claude-opus-5-5"),
        Preset {
            wire_api: WireApi::Responses,
            effort: Effort::Default,
            ..Preset::new("OpenAI (Responses)", openai, "https://api.openai.com/v1")
        },
        Preset::new("OpenAI", openai, "https://api.openai.com/v1"),
        Preset::new("DeepSeek", openai, "https://api.deepseek.com/v1").model("deepseek-chat"),
        Preset::new(
            "Qwen (DashScope)",
            openai,
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
        )
        .model("qwen-plus"),
        Preset::new("Kimi (Moonshot)", openai, "https://api.moonshot.cn/v1"),
        Preset::new("OpenRouter", openai, "https://openrouter.ai/api/v1"),
        Preset::new(
            "Local (OpenAI-compatible)",
            openai,
            "http://localhost:11434/v1",
        ),
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
            wire_api: WireApi::Chat,
            service_tier: None,
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

    /// Settings written before `wireApi` and `serviceTier` existed still load,
    /// and the defaults are not written back, so older versions keep reading
    /// the file this version saves.
    #[test]
    fn new_fields_are_additive() {
        let old = r#"{"id":"p","name":"n","kind":"openai_compatible","baseUrl":"https://x.com","model":"m","temperature":0.0,"effort":"low"}"#;
        let parsed: ProviderConfig = serde_json::from_str(old).unwrap();
        assert_eq!(parsed.wire_api, WireApi::Chat);
        assert_eq!(parsed.service_tier, None);
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(
            !json.contains("wireApi") && !json.contains("serviceTier"),
            "{json}"
        );

        let mut any = cfg("https://anyrouter.top/v1");
        any.wire_api = WireApi::Responses;
        any.service_tier = Some(ServiceTier::Priority);
        let json = serde_json::to_string(&any).unwrap();
        assert!(json.contains(r#""wireApi":"responses""#), "{json}");
        assert!(json.contains(r#""serviceTier":"priority""#), "{json}");
        assert!(json.contains(r#""kind":"openai_compatible""#), "{json}");
        assert_eq!(serde_json::from_str::<ProviderConfig>(&json).unwrap(), any);
    }

    #[test]
    fn wire_and_tier_only_apply_to_openai_compatible() {
        let mut a = cfg("https://api.anthropic.com");
        a.kind = ProviderKind::Anthropic;
        a.wire_api = WireApi::Responses;
        a.service_tier = Some(ServiceTier::Flex);
        let a = a.validated().unwrap();
        assert_eq!((a.wire_api, a.service_tier), (WireApi::Chat, None));
        assert_eq!(a.wire_label(), "anthropic");
    }

    #[test]
    fn anyrouter_preset_matches_the_codex_setup() {
        let p = presets()
            .into_iter()
            .find(|p| p.base_url.contains("anyrouter"))
            .unwrap();
        assert_eq!(p.kind, ProviderKind::OpenaiCompatible);
        assert_eq!(p.wire_api, WireApi::Responses);
        assert_eq!(p.model, "gpt-6-astra");
        assert_eq!(p.effort, Effort::High);
        assert_eq!(p.service_tier, Some(ServiceTier::Priority));
    }
}
