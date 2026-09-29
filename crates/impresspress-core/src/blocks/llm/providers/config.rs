//! Provider configuration — describes a single configured LLM provider
//! (remote API or OpenAI-compatible local server). `ProviderLlmService` is
//! initialized with a `Vec<ProviderConfig>` loaded from the
//! `impresspress__llm__providers` DB collection and routes each `ChatRequest`
//! to the right encoder/decoder based on `protocol`.

use serde::{Deserialize, Serialize};

use crate::llm_wire::openai::MaxTokensField;

// Derives `JsonSchema` because it is published as-is on the provider
// contracts (`contracts::ProviderView` and the create/update requests):
// the schema's `enum` is then the same three tokens `parse` accepts, so a
// caller who reads the schema cannot send an alias the handler refuses.
/// Wire protocol a configured provider speaks.
///
/// `open_ai` and `anthropic` are the providers' native APIs.
/// `open_ai_compatible` covers every third-party endpoint implementing
/// OpenAI's `/v1` interface — Ollama, llama-server, LM Studio, vLLM, LocalAI,
/// KoboldCpp, Azure OpenAI, Groq, Together, OpenRouter, Mistral API, Anyscale,
/// and so on.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderProtocol {
    OpenAi,
    Anthropic,
    OpenAiCompatible,
}

impl ProviderProtocol {
    /// Parse from the string column stored in `impresspress__llm__providers`.
    /// Accepts canonical `snake_case` forms only — callers must write the
    /// same tokens they read. No aliasing across representations.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open_ai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            "open_ai_compatible" => Some(Self::OpenAiCompatible),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "open_ai",
            Self::Anthropic => "anthropic",
            Self::OpenAiCompatible => "open_ai_compatible",
        }
    }

    /// Whether a provider on this protocol may declare a
    /// [`ProviderConfig::max_tokens_field`] override.
    ///
    /// `false` for [`Anthropic`](Self::Anthropic): the Messages API carries
    /// the budget in one field and offers no second spelling, so its encoder
    /// never consults the override and storing one would be a setting that
    /// does nothing.
    pub fn accepts_max_tokens_field(self) -> bool {
        match self {
            Self::OpenAi | Self::OpenAiCompatible => true,
            Self::Anthropic => false,
        }
    }
}

/// A single configured provider. Stored in the DB, loaded on lifecycle(Init),
/// and pushed to `ProviderLlmService::configure(...)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ProviderConfig {
    /// Display name + backend_id key. Must be unique. Used as the
    /// `ChatRequest::backend_id` when routing requests to this provider.
    pub name: String,

    pub protocol: ProviderProtocol,

    /// Base URL, e.g. `https://api.openai.com/v1` or
    /// `http://localhost:11434/v1`. No trailing slash.
    pub endpoint: String,

    /// Inline API key. `None` is valid for local OpenAI-compatible servers
    /// that don't require auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Optional config-var reference, e.g. `"IMPRESSPRESS__LLM__OPENAI_KEY_PROD"`.
    /// When set, the feature block resolves the value from the config client
    /// into `api_key` whenever providers are (re)loaded into the in-memory
    /// service — at `Init` and on every provider CRUD write (see
    /// `routes::reload_provider_service`). Takes precedence over an inline
    /// `api_key` when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_var: Option<String>,

    /// Which field carries the output-token budget in this provider's request
    /// bodies. `None` — the usual case — means the one
    /// [`protocol`](Self::protocol) implies.
    ///
    /// Set it for a server whose wire format departs from its protocol's usual
    /// spelling. Azure OpenAI is declared `open_ai_compatible`, and that
    /// protocol sends `max_tokens`, but an Azure *reasoning* deployment
    /// answers `400` to anything but `max_completion_tokens`. The operator
    /// says so; nothing here infers it from the model id or the endpoint host.
    ///
    /// Meaningless under [`ProviderProtocol::Anthropic`], whose wire format
    /// has one budget field and no second spelling to choose between — the
    /// provider CRUD routes refuse the pairing rather than store a value the
    /// encoder would ignore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,

    /// Explicit model list. Empty means "discover via `/v1/models`".
    #[serde(default)]
    pub models: Vec<String>,

    /// Whether requests routed to this provider should succeed or short-circuit.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl ProviderConfig {
    /// Minimal constructor. `api_key` / `key_var` / `models` default to
    /// empty, `max_tokens_field` to "whatever the protocol says", and
    /// `enabled` to true.
    pub fn new(
        name: impl Into<String>,
        protocol: ProviderProtocol,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            protocol,
            endpoint: endpoint.into(),
            api_key: None,
            key_var: None,
            max_tokens_field: None,
            models: Vec::new(),
            enabled: true,
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn with_key_var(mut self, var: impl Into<String>) -> Self {
        self.key_var = Some(var.into());
        self
    }

    pub fn with_models(mut self, models: Vec<String>) -> Self {
        self.models = models;
        self
    }

    pub fn with_max_tokens_field(mut self, field: MaxTokensField) -> Self {
        self.max_tokens_field = Some(field);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_roundtrip_via_parse_as_str() {
        for p in [
            ProviderProtocol::OpenAi,
            ProviderProtocol::Anthropic,
            ProviderProtocol::OpenAiCompatible,
        ] {
            assert_eq!(ProviderProtocol::parse(p.as_str()), Some(p));
        }
    }

    #[test]
    fn protocol_parse_rejects_aliases() {
        // No translation between representations — "openai" is not "open_ai".
        assert_eq!(ProviderProtocol::parse("openai"), None);
        assert_eq!(ProviderProtocol::parse("OpenAi"), None);
        assert_eq!(ProviderProtocol::parse("compatible"), None);
    }

    #[test]
    fn config_serde_roundtrip_minimal() {
        let cfg = ProviderConfig::new(
            "openai-main",
            ProviderProtocol::OpenAi,
            "https://api.openai.com/v1",
        );
        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, cfg);
    }

    #[test]
    fn config_serde_roundtrip_full() {
        let cfg = ProviderConfig::new(
            "local-llama",
            ProviderProtocol::OpenAiCompatible,
            "http://localhost:11434/v1",
        )
        .with_api_key("none")
        .with_models(vec!["llama3".into(), "mistral".into()]);
        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, cfg);
    }

    #[test]
    fn config_serde_roundtrip_with_a_max_tokens_field() {
        let cfg = ProviderConfig::new(
            "azure-reasoning",
            ProviderProtocol::OpenAiCompatible,
            "https://example.openai.azure.com/openai/v1",
        )
        .with_max_tokens_field(MaxTokensField::MaxCompletionTokens);
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(
            json["max_tokens_field"], "max_completion_tokens",
            "the stored token is the wire field name, not a third spelling"
        );
        let decoded: ProviderConfig = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, cfg);
    }

    #[test]
    fn config_has_no_max_tokens_field_when_missing() {
        let json = r#"{
            "name": "a",
            "protocol": "open_ai_compatible",
            "endpoint": "http://localhost:11434/v1"
        }"#;
        let cfg: ProviderConfig = serde_json::from_str(json).unwrap();
        assert!(
            cfg.max_tokens_field.is_none(),
            "absent means `follow the protocol`, never a guessed spelling"
        );
    }

    /// Anthropic's Messages API has one budget field, so there is nothing for
    /// an override to choose and the CRUD routes refuse one.
    #[test]
    fn only_the_openai_shaped_protocols_accept_a_max_tokens_field() {
        assert!(ProviderProtocol::OpenAi.accepts_max_tokens_field());
        assert!(ProviderProtocol::OpenAiCompatible.accepts_max_tokens_field());
        assert!(!ProviderProtocol::Anthropic.accepts_max_tokens_field());
    }

    #[test]
    fn config_defaults_enabled_when_missing() {
        let json = r#"{
            "name": "a",
            "protocol": "open_ai",
            "endpoint": "https://api.openai.com/v1"
        }"#;
        let cfg: ProviderConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.enabled);
        assert!(cfg.api_key.is_none());
        assert!(cfg.models.is_empty());
    }
}
