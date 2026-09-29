//! OpenAI native provider transport: endpoint URL, headers and API-key policy.
//!
//! The wire format itself — request-body serialization and the SSE decoder —
//! lives in [`crate::llm_wire::openai`], which compiles with no `block-*`
//! feature so the browser adapter reaches the same implementation. This module
//! is the half that needs a configured provider.

use std::collections::HashMap;

use wafer_core::interfaces::llm::service::ChatRequest;

use super::config::ProviderConfig;
pub use crate::llm_wire::openai::OpenAiSseDecoder;
use crate::llm_wire::openai::{encode_chat_body, MaxTokensField};

/// `(url, headers, body)` triple produced by the encoder.
pub type EncodedRequest = (String, HashMap<String, String>, Vec<u8>);

/// Build an `(url, headers, body)` triple for a streaming OpenAI chat
/// completion. Callers POST the body with the given headers.
///
/// Returns `Err` only if the configured provider is missing the required
/// `api_key` — we never silently omit `Authorization` on the OpenAI native
/// protocol, unlike `openai_compatible` which may. The body itself is
/// [`encode_chat_body`], shared with every other consumer of the format.
///
/// The output-token budget goes out as `max_completion_tokens` unless the
/// provider declares a `max_tokens_field` override: on OpenAI's own API
/// `max_tokens` is the deprecated spelling, and the reasoning models —
/// reachable here because `/v1/models` discovery lists them like any other —
/// refuse a request carrying it with `unsupported_parameter`.
pub fn encode_chat_request(
    req: &ChatRequest,
    provider: &ProviderConfig,
    resolved_api_key: Option<&str>,
) -> Result<EncodedRequest, EncodeError> {
    encode_chat_request_as(
        req,
        provider,
        resolved_api_key,
        MaxTokensField::MaxCompletionTokens,
    )
}

/// [`encode_chat_request`] with the *protocol's* budget spelling named by the
/// caller. `provider.max_tokens_field`, when the operator set one, wins over
/// it — that is the one place the override is applied, so both protocols get
/// it from one line.
///
/// `openai_compatible` is the other caller and passes
/// [`MaxTokensField::MaxTokens`]: the URL, the header policy and the body are
/// otherwise identical across the two protocols, and this keeps that one
/// difference a parameter instead of a second copy of the encoder.
pub(super) fn encode_chat_request_as(
    req: &ChatRequest,
    provider: &ProviderConfig,
    resolved_api_key: Option<&str>,
    protocol_max_tokens_field: MaxTokensField,
) -> Result<EncodedRequest, EncodeError> {
    let max_tokens_field = provider
        .max_tokens_field
        .unwrap_or(protocol_max_tokens_field);
    let url = format!(
        "{}/chat/completions",
        provider.endpoint.trim_end_matches('/')
    );

    let mut headers = HashMap::new();
    headers.insert("Content-Type".into(), "application/json".into());
    match resolved_api_key {
        Some(key) => {
            headers.insert("Authorization".into(), format!("Bearer {key}"));
        }
        None => return Err(EncodeError::MissingApiKey),
    }

    let bytes = encode_chat_body(req, max_tokens_field)
        .map_err(|e| EncodeError::Serialize(e.to_string()))?;
    Ok((url, headers, bytes))
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("missing api_key for openai provider")]
    MissingApiKey,
    #[error("serialize request body: {0}")]
    Serialize(String),
}

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::llm::service::{ChatMessage, ChatParams, ToolDefinition};

    use super::{super::config::ProviderProtocol, *};

    fn openai_provider() -> ProviderConfig {
        ProviderConfig::new(
            "openai-main",
            ProviderProtocol::OpenAi,
            "https://api.openai.com/v1",
        )
    }

    #[test]
    fn encodes_simple_chat_request() {
        let req = ChatRequest::new("openai-main", "gpt-4o-mini", vec![ChatMessage::user("hi")]);
        let (url, headers, body) =
            encode_chat_request(&req, &openai_provider(), Some("sk-test")).expect("encode");
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer sk-test")
        );
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "gpt-4o-mini");
        assert_eq!(json["stream"], true);
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "hi");
        assert_eq!(json["stream_options"]["include_usage"], true);
    }

    #[test]
    fn encodes_multi_turn_with_system() {
        let req = ChatRequest::new(
            "openai-main",
            "gpt-4o",
            vec![
                ChatMessage::system("be terse"),
                ChatMessage::user("what is 2+2?"),
                ChatMessage::assistant("4"),
                ChatMessage::user("now divide by 2"),
            ],
        );
        let (_, _, body) = encode_chat_request(&req, &openai_provider(), Some("sk")).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let msgs = json["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[2]["role"], "assistant");
    }

    #[test]
    fn encodes_params() {
        let mut req = ChatRequest::new("openai-main", "gpt-4o", vec![ChatMessage::user("hi")]);
        req.params = ChatParams {
            temperature: Some(0.3),
            max_tokens: Some(512),
            top_p: Some(0.9),
            seed: Some(42),
            stop_sequences: vec!["END".into()],
            ..Default::default()
        };

        let (_, _, body) = encode_chat_request(&req, &openai_provider(), Some("sk")).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["temperature"], 0.3);
        // OpenAI's own spelling. `max_tokens` is deprecated there and the
        // reasoning models refuse it outright — see
        // `the_native_protocol_sends_only_max_completion_tokens`.
        assert_eq!(json["max_completion_tokens"], 512);
        assert_eq!(json["top_p"], 0.9);
        assert_eq!(json["seed"], 42);
        assert_eq!(json["stop"][0], "END");
    }

    #[test]
    fn encodes_tools() {
        let mut req = ChatRequest::new("openai-main", "gpt-4o", vec![ChatMessage::user("hi")]);
        // ToolDefinition is #[non_exhaustive]; round-trip through serde.
        let tool: ToolDefinition = serde_json::from_value(serde_json::json!({
            "name": "lookup",
            "description": "look up a thing",
            "parameters": {
                "type": "object",
                "properties": {"x": {"type": "string"}}
            }
        }))
        .unwrap();
        req.tools = vec![tool];
        let (_, _, body) = encode_chat_request(&req, &openai_provider(), Some("sk")).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["function"]["name"], "lookup");
        assert_eq!(
            json["tools"][0]["function"]["parameters"]["properties"]["x"]["type"],
            "string"
        );
    }

    /// The native OpenAI protocol sends `max_completion_tokens` and nothing
    /// else.
    ///
    /// `max_tokens` is the deprecated spelling on OpenAI's own API, and its
    /// reasoning models refuse a request that carries it with
    /// `unsupported_parameter` — a 400 on every chat. Those models are
    /// reachable here: `discover_models` lists whatever `/v1/models` returns,
    /// so an operator selects one like any other. Sending both spellings is
    /// not a fix either; this asserts the old one is absent.
    #[test]
    fn the_native_protocol_sends_only_max_completion_tokens() {
        let mut req = ChatRequest::new("openai-main", "o3", vec![ChatMessage::user("hi")]);
        req.params.max_tokens = Some(4096);

        let (_, _, body) = encode_chat_request(&req, &openai_provider(), Some("sk")).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["max_completion_tokens"], 4096);
        assert!(
            json.get("max_tokens").is_none(),
            "a reasoning model 400s on `max_tokens`, got: {json}"
        );
    }

    /// The same override on the native protocol, in the other direction —
    /// which is what makes it a property of the configured provider rather
    /// than a rule about reasoning models wearing a different name.
    #[test]
    fn a_declared_max_tokens_field_overrides_the_protocols_spelling() {
        let provider = openai_provider().with_max_tokens_field(MaxTokensField::MaxTokens);
        let mut req = ChatRequest::new("openai-main", "gpt-4o", vec![ChatMessage::user("hi")]);
        req.params.max_tokens = Some(512);

        let (_, _, body) = encode_chat_request(&req, &provider, Some("sk")).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["max_tokens"], 512);
        assert!(
            json.get("max_completion_tokens").is_none(),
            "the override replaces the protocol's spelling, got: {json}"
        );
    }

    #[test]
    fn rejects_missing_api_key() {
        let req = ChatRequest::new("openai-main", "gpt-4o", vec![ChatMessage::user("hi")]);
        assert!(matches!(
            encode_chat_request(&req, &openai_provider(), None),
            Err(EncodeError::MissingApiKey)
        ));
    }
}
