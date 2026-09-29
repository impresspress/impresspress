//! Typed request/response contracts for the `/b/llm/api/*` JSON surface.
//!
//! These did not exist before this module. Every llm handler deserialized its
//! body into a private in-function struct and answered with a
//! `serde_json::json!` literal — or, on the per-thread config write, with an
//! echoed database [`Record`] — so the block declared no schemas and its JSON
//! API was invisible in `/openapi.json`. The types below are the *only*
//! source of the schemas declared in [`super::LlmBlock`]'s
//! `BlockInfo::endpoints`: `.input::<T>()` / `.output::<T>()` derive them
//! from the same types the handlers deserialize into and serialize out of.
//!
//! # Nothing credential-bearing belongs here
//!
//! A provider row names the admin configuration variable that holds its API
//! key (`key_var`). The key itself is resolved into the in-memory provider
//! router at reload time (`routes::reload_provider_service`) and from then on
//! sits on the `ProviderConfig`s the handlers hold through
//! `LlmBlock::provider_admin` — one field away from every response.
//! [`ProviderView`] is a closed field list built from a `ProviderConfig` one
//! field at a time, and `api_key` is not on it. Keep it a view type: a
//! `ProviderConfig` re-export would publish the resolved key the moment a
//! handler serialized a router snapshot instead of a row.
//!
//! # The model shapes mirror wafer-run wire types
//!
//! [`ModelInfoView`] and [`ModelStatusView`] are field-for-field mirrors of
//! `wafer_core::clients::llm::{ModelInfo, ModelStatus}`. Those live in
//! wafer-run and do not derive `schemars::JsonSchema`, so the handlers build a
//! view from the real value and serialize *that* — the type the schema is
//! derived from is the type that goes out on the wire, same as
//! `blocks::files::contracts`. The serde attributes are copied so the bytes
//! are identical to what echoing the wire type produced.
//!
//! # `#[schemars(required)]` on `Option<T>` is not decoration
//!
//! Same rule as `blocks::auth_ui::contracts`: `.output::<T>()` generates
//! under schemars' **serialize** contract, and on an `Option<T>` paired with
//! `skip_serializing_if = "Option::is_none"` the attribute drops the `null`
//! branch without forcing the property into `required`. That is exactly
//! [`ModelStatusView::progress`]'s shape: absent unless loading, never
//! `null`. Response types only — never apply it to an input.

use serde::{Deserialize, Serialize};
use wafer_core::clients::llm::{ModelCapabilities, ModelInfo, ModelState, ModelStatus};

use super::{
    providers::config::{ProviderConfig, ProviderProtocol},
    repo::settings::ThreadSettingRow,
};
use crate::llm_wire::openai::MaxTokensField;

// ---------------------------------------------------------------------------
// POST /b/llm/api/chat, POST /b/llm/api/chat/stream
// ---------------------------------------------------------------------------

/// Request body shared by `POST /b/llm/api/chat` and
/// `POST /b/llm/api/chat/stream`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChatRequest {
    /// Messages-block context id the conversation lives in. The user turn is
    /// stored there before the model runs and the assistant turn after it.
    pub thread_id: String,
    /// The user's message.
    pub message: String,
    /// Provider to route to, by name. A per-thread override set through
    /// `POST /b/llm/api/config` takes precedence; when neither is set the
    /// configured default provider is used.
    pub provider: Option<String>,
    /// Model id within the provider. Same precedence as `provider`.
    pub model: Option<String>,
    /// Largest reply, in output tokens, this turn may generate. Omitted, the
    /// deployment's `IMPRESSPRESS__LLM__DEFAULT_MAX_TOKENS` applies — every
    /// request reaches the provider with a budget, because
    /// Anthropic-protocol providers refuse one that carries none.
    ///
    /// Must be at least 1: zero asks for no answer at all and is answered
    /// `400` here rather than by the provider. There is no upper bound on
    /// this side, deliberately — the real ceiling is the model's, it differs
    /// per model and per provider, and the provider is the only party that
    /// knows it. A value the model will not accept comes back as that
    /// provider's own error rather than one invented here. (The configured
    /// default is treated differently: a value that is not a positive integer
    /// falls back with a warning, because an operator's typo must not break
    /// every chat on the deployment at once.)
    #[schemars(range(min = 1))]
    pub max_tokens: Option<u32>,
}

/// `POST /b/llm/api/chat` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChatResponse {
    /// The assistant's reply: every text delta the model produced,
    /// concatenated.
    pub content: String,
    /// Id of the assistant entry persisted in the messages block. Always
    /// populated: a reply the store refused is answered with a 500, not a
    /// body (see `routes::chat::handle_chat`).
    pub message_id: String,
    /// The model the request was served by, after per-thread and default
    /// resolution.
    pub model: String,
    /// `true` when `content` is a prefix of the answer rather than the whole
    /// of it. Two ceilings can do that, and the flag does not distinguish
    /// them because a caller's response to either is the same — ask again,
    /// or ask for less:
    ///
    /// * the model stopped at its output-token budget (the request's own
    ///   `max_tokens`, or `IMPRESSPRESS__LLM__DEFAULT_MAX_TOKENS`), which it
    ///   reports as a `length` finish reason;
    /// * the reply exceeded the 1 MiB buffering cap of this endpoint, in
    ///   which case the text ends at the last delta that fitted and nothing
    ///   after it is appended, so it is never spliced across a gap.
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// /b/llm/api/providers
// ---------------------------------------------------------------------------

// Built field by field from `ProviderConfig`. That struct also carries
// `api_key` — `None` when decoded from a row, but the resolved plaintext key
// on the configs `LlmBlock::provider_admin` holds after a reload — and it is
// deliberately not read here. The reason lives in this plain comment rather
// than the doc comment below because a `///` line is published as the
// schema's `description`.
/// A configured LLM provider as published by the admin API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderView {
    /// Stable row identifier, used by the `/b/llm/api/providers/{id}` routes.
    pub id: String,
    /// Unique provider name. This is the `backend_id` chat requests route on.
    pub name: String,
    pub protocol: ProviderProtocol,
    /// Base URL of the provider's API, e.g. `https://api.openai.com/v1`.
    pub endpoint: String,
    /// Name of the admin configuration variable holding this provider's API
    /// key, or `null` for a provider that runs unauthenticated. The key
    /// itself is never published.
    pub key_var: Option<String>,
    /// Which field carries the output-token budget in this provider's chat
    /// bodies, or `null` to send the one its `protocol` implies.
    pub max_tokens_field: Option<MaxTokensField>,
    /// Explicit model list. Empty means the models are discovered from the
    /// provider's `/v1/models`.
    pub models: Vec<String>,
    /// Whether chat requests may route to this provider.
    pub enabled: bool,
}

impl ProviderView {
    /// Project a row id plus its decoded configuration.
    pub fn from_config(id: &str, cfg: &ProviderConfig) -> Self {
        Self {
            id: id.to_string(),
            name: cfg.name.clone(),
            protocol: cfg.protocol,
            endpoint: cfg.endpoint.clone(),
            key_var: cfg.key_var.clone(),
            max_tokens_field: cfg.max_tokens_field,
            models: cfg.models.clone(),
            enabled: cfg.enabled,
        }
    }
}

/// `GET /b/llm/api/providers` response body: every configured provider,
/// enabled or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderListResponse {
    pub providers: Vec<ProviderView>,
}

// `deny_unknown_fields` is a deliberate exception to this repo's habit of
// tolerating unknown keys (only `prepared_plan.rs` uses it otherwise), for a
// credential-adjacent admin input: `ProviderConfig` has an inline `api_key`
// that this contract does not expose, so an admin who sends one must be told
// so by name. Silently dropping it would answer 200 with a provider that runs
// unauthenticated — after the secret transited the request body.
/// `POST /b/llm/api/providers` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateProviderRequest {
    /// Unique provider name. Becomes the `backend_id` chat requests route on.
    pub name: String,
    pub protocol: ProviderProtocol,
    /// Base URL of the provider's API. Must resolve to a public address:
    /// private, link-local and cloud-metadata ranges are refused.
    pub endpoint: String,
    /// Name of the admin configuration variable holding the API key. Omit,
    /// or send an empty string, for a provider that needs no key.
    pub key_var: Option<String>,
    /// Which field carries the output-token budget in this provider's chat
    /// bodies. Omit to send the one `protocol` implies, which is what all but
    /// a handful of endpoints want. Refused on the `anthropic` protocol,
    /// whose wire format has only one such field.
    pub max_tokens_field: Option<MaxTokensField>,
    /// Explicit model list. Omitted or empty means the models are discovered
    /// from the provider's `/v1/models`.
    pub models: Option<Vec<String>>,
    /// Whether chat requests may route to this provider. Defaults to `true`
    /// when the JSON body omits it; an unticked checkbox on the form path is
    /// `false`, because that is the only thing an absent checkbox can mean.
    pub enabled: Option<bool>,
}

// Same deliberate `deny_unknown_fields` exception as `CreateProviderRequest`,
// for the same reason: an inline `api_key` on the patch must be refused by
// name, not dropped.
/// `PATCH /b/llm/api/providers/{id}` request body. Every field is optional
/// and only the ones present are applied. The two fields a provider can hold
/// as `null` — `key_var` and `max_tokens_field` — are cleared by sending
/// `null` (`key_var` also by an empty string); an empty `name` or `endpoint`
/// is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateProviderRequest {
    pub name: Option<String>,
    pub protocol: Option<ProviderProtocol>,
    /// Re-validated on every change: must resolve to a public address.
    pub endpoint: Option<String>,
    // `key_var` and `max_tokens_field` are the two fields on this body where a
    // present `null` differs from an absent key, because they are the two the
    // provider itself can hold as `null` and so the two an admin has to be
    // able to clear. Both take the same `present_as_some` treatment rather
    // than one convention each: the schema stays that of the value being
    // patched (`string | null`, `MaxTokensField | null`) and the extra
    // `Option` is how Rust holds "was it sent at all", not a second level of
    // nesting on the wire. The remaining fields are not nullable on a
    // provider, so for them `null` and absent mean the same thing.
    //
    // `skip_serializing_if` keeps the derived `Serialize` telling the same
    // story: an absent field stays absent rather than going back out as the
    // `null` that means "clear it".
    /// Name of the admin configuration variable holding the API key, or
    /// `null` (or `""`) to leave the provider unauthenticated.
    #[serde(
        default,
        deserialize_with = "present_as_some",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub key_var: Option<Option<String>>,
    /// Which field carries the output-token budget, or `null` to go back to
    /// the one `protocol` implies.
    #[serde(
        default,
        deserialize_with = "present_as_some",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<MaxTokensField>")]
    pub max_tokens_field: Option<Option<MaxTokensField>>,
    pub models: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

/// Deserialize a *present* field into `Some(..)`, `null` included.
///
/// `Option<Option<T>>` under plain `#[serde(default)]` collapses an absent key
/// and an explicit `null` into the same `None`, so a patch type built that way
/// can set a value but never clear one.
fn present_as_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// `DELETE /b/llm/api/providers/{id}` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderDeleteResponse {
    /// Always `true`: a delete that did not happen is an error response.
    pub deleted: bool,
}

/// `POST /b/llm/api/providers/{id}/discover-models` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiscoveredModelsResponse {
    /// Model ids the provider reported — or its explicit list, when one is
    /// configured — now stored as the provider's `models`.
    pub models: Vec<String>,
}

// ---------------------------------------------------------------------------
// /b/llm/api/models
// ---------------------------------------------------------------------------

/// A model one of the registered backends can serve. Mirrors
/// `wafer_core::clients::llm::ModelInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelInfoView {
    /// Backend (provider name) the model is hosted in.
    pub backend_id: String,
    /// Backend-side model identifier.
    pub model_id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Declared model capabilities.
    pub capabilities: ModelCapabilitiesView,
}

impl From<ModelInfo> for ModelInfoView {
    fn from(info: ModelInfo) -> Self {
        Self {
            backend_id: info.backend_id,
            model_id: info.model_id,
            display_name: info.display_name,
            capabilities: ModelCapabilitiesView::from(info.capabilities),
        }
    }
}

/// Capability flags for a model. Mirrors
/// `wafer_core::clients::llm::ModelCapabilities`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct ModelCapabilitiesView {
    /// Supports streaming responses.
    pub streaming: bool,
    /// Supports tool / function calling.
    pub tools: bool,
    /// Accepts image inputs.
    pub vision: bool,
    /// Supports structured JSON output mode.
    pub json_mode: bool,
    /// Maximum input context window in tokens, when the backend reports one.
    pub max_context_tokens: Option<u32>,
    /// Maximum output tokens per request, when the backend reports one.
    pub max_output_tokens: Option<u32>,
}

impl From<ModelCapabilities> for ModelCapabilitiesView {
    fn from(caps: ModelCapabilities) -> Self {
        Self {
            streaming: caps.streaming,
            tools: caps.tools,
            vision: caps.vision,
            json_mode: caps.json_mode,
            max_context_tokens: caps.max_context_tokens,
            max_output_tokens: caps.max_output_tokens,
        }
    }
}

/// `GET /b/llm/api/models` response body: every model across every
/// registered backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelListResponse {
    pub models: Vec<ModelInfoView>,
}

/// `GET /b/llm/api/models/{backend_id}/{model_id}/status` response body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelStatusResponse {
    pub status: ModelStatusView,
}

/// Lifecycle status of one model on one backend. Mirrors
/// `wafer_core::clients::llm::ModelStatus`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelStatusView {
    /// High-level state.
    pub state: ModelStateView,
    /// Load progress in `0.0..=1.0`. Present only while `state` is
    /// `Loading`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(required)]
    pub progress: Option<f32>,
}

impl From<ModelStatus> for ModelStatusView {
    fn from(status: ModelStatus) -> Self {
        Self {
            state: ModelStateView::from(status.state),
            progress: status.progress,
        }
    }
}

/// High-level lifecycle state of a model. Mirrors
/// `wafer_core::clients::llm::ModelState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum ModelStateView {
    /// Local backend: weights loaded. Remote backend: endpoint reachable.
    Ready,
    /// Local backend: weights currently downloading or initializing.
    Loading,
    /// Local backend only: weights not in memory.
    Unloaded,
    /// Loading or serving failed.
    Error {
        /// Failure message.
        message: String,
    },
}

impl From<ModelState> for ModelStateView {
    fn from(state: ModelState) -> Self {
        match state {
            ModelState::Ready => Self::Ready,
            ModelState::Loading => Self::Loading,
            ModelState::Unloaded => Self::Unloaded,
            ModelState::Error { message } => Self::Error { message },
        }
    }
}

/// `POST /b/llm/api/models/{backend_id}/{model_id}/unload` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelUnloadResponse {
    /// Always `true`: an unload that did not happen is an error response.
    pub unloaded: bool,
}

// ---------------------------------------------------------------------------
// /b/llm/api/config
// ---------------------------------------------------------------------------

/// `GET /b/llm/api/config` response body: the global defaults every thread
/// without an override resolves to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LlmConfigResponse {
    /// Default provider name (`IMPRESSPRESS__LLM__DEFAULT_PROVIDER`).
    pub default_provider: String,
    /// Default model id (`IMPRESSPRESS__LLM__DEFAULT_MODEL`). Empty means the
    /// provider's own default.
    pub default_model: String,
}

/// `POST /b/llm/api/config` request body: create or update one thread's
/// provider/model override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConfigUpdateRequest {
    /// Thread to set the override on. Without it nothing is written and the
    /// request is only acknowledged.
    pub thread_id: Option<String>,
    /// Provider name to pin the thread to. When creating an override, omitted
    /// means `""` — use the default provider.
    pub provider_block: Option<String>,
    /// Model id to pin the thread to. When creating an override, omitted
    /// means `""` — use the default model.
    pub model: Option<String>,
    /// Not settable here: the global default is
    /// `IMPRESSPRESS__LLM__DEFAULT_PROVIDER`. Sending it is refused.
    pub default_provider: Option<String>,
    /// Not settable here: the global default is
    /// `IMPRESSPRESS__LLM__DEFAULT_MODEL`. Sending it is refused.
    pub default_model: Option<String>,
}

/// One thread's provider/model override, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ThreadOverrideView {
    /// Stable row identifier.
    pub id: String,
    /// Messages-block context id the override applies to.
    pub thread_id: String,
    /// Pinned provider name. Empty means the default provider.
    pub provider_block: String,
    /// Pinned model id. Empty means the default model.
    pub model: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    pub updated_at: String,
}

impl From<&ThreadSettingRow> for ThreadOverrideView {
    /// Publish a stored override. The row type is the only thing that reads
    /// the table's columns, so the view is a field-for-field projection with
    /// nothing to decode.
    fn from(row: &ThreadSettingRow) -> Self {
        Self {
            id: row.id.clone(),
            thread_id: row.thread_id.clone(),
            provider_block: row.provider_block.clone(),
            model: row.model.clone(),
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
        }
    }
}

/// `POST /b/llm/api/config` response body: the override after the write, or
/// a bare acknowledgement when the request named no `thread_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ConfigUpdateResponse {
    /// The override row after it was created or updated.
    Override(ThreadOverrideView),
    /// Nothing was written.
    Acknowledged(ConfigAcknowledgement),
}

/// Acknowledgement for a `POST /b/llm/api/config` that named no thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConfigAcknowledgement {
    /// Always `true`, although nothing was written: the request named no
    /// `thread_id`, so there was no override to change.
    pub updated: bool,
}

/// `DELETE /b/llm/api/config/{id}` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConfigDeleteResponse {
    /// Always `true`: a delete that did not happen is an error response.
    pub deleted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::llm::EXAMPLE_KEY_VAR;

    /// The projection is what keeps the resolved key off the wire: a config
    /// holding one still serializes without it.
    #[test]
    fn provider_view_never_carries_the_api_key() {
        let cfg = ProviderConfig::new(
            "openai-main",
            ProviderProtocol::OpenAi,
            "https://api.openai.com/v1",
        )
        .with_api_key("sk-resolved-plaintext")
        .with_key_var(EXAMPLE_KEY_VAR)
        .with_models(vec!["gpt-4o".into()]);

        let value = serde_json::to_value(ProviderView::from_config("row-1", &cfg)).expect("json");

        let mut keys: Vec<&str> = value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "enabled",
                "endpoint",
                "id",
                "key_var",
                "max_tokens_field",
                "models",
                "name",
                "protocol"
            ]
        );
        assert!(
            !value.to_string().contains("sk-resolved-plaintext"),
            "the resolved key must not survive projection: {value}"
        );
        assert_eq!(value["id"], "row-1");
        assert_eq!(value["protocol"], "open_ai");
        assert_eq!(value["key_var"], EXAMPLE_KEY_VAR);
        assert_eq!(value["models"], serde_json::json!(["gpt-4o"]));
        assert_eq!(value["enabled"], true);
    }

    /// The mirrors must serialize byte-for-byte like the wire types they
    /// stand in for, or the schema derived from them describes a body the
    /// handler does not send.
    #[test]
    fn model_views_serialize_like_the_wire_types() {
        let info = ModelInfo::new("openai-main", "gpt-4o", "GPT-4o").with_capabilities(
            ModelCapabilities {
                streaming: true,
                tools: false,
                vision: true,
                json_mode: false,
                max_context_tokens: Some(128_000),
                max_output_tokens: None,
            },
        );
        assert_eq!(
            serde_json::to_value(ModelInfoView::from(info.clone())).expect("view json"),
            serde_json::to_value(&info).expect("wire json")
        );

        for status in [
            ModelStatus::ready(),
            ModelStatus::loading(0.25),
            ModelStatus::unloaded(),
            ModelStatus::error("boom"),
        ] {
            assert_eq!(
                serde_json::to_value(ModelStatusView::from(status.clone())).expect("view json"),
                serde_json::to_value(&status).expect("wire json"),
                "{status:?}"
            );
        }
    }

    #[test]
    fn config_update_response_is_the_override_or_the_acknowledgement() {
        let row = ThreadSettingRow {
            id: "row-1".into(),
            thread_id: "t1".into(),
            provider_block: "openai-main".into(),
            model: "gpt-4o".into(),
            created_at: "2026-08-28T00:00:00Z".into(),
            updated_at: "2026-08-28T00:00:00Z".into(),
        };
        assert_eq!(
            serde_json::to_value(ConfigUpdateResponse::Override(ThreadOverrideView::from(
                &row
            )))
            .expect("json"),
            serde_json::json!({
                "id": "row-1",
                "thread_id": "t1",
                "provider_block": "openai-main",
                "model": "gpt-4o",
                "created_at": "2026-08-28T00:00:00Z",
                "updated_at": "2026-08-28T00:00:00Z",
            })
        );
        assert_eq!(
            serde_json::to_value(ConfigUpdateResponse::Acknowledged(ConfigAcknowledgement {
                updated: true
            }))
            .expect("json"),
            serde_json::json!({ "updated": true })
        );
    }
}
