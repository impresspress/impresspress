//! OpenAI-compatible endpoints — Ollama, llama-server, LM Studio, vLLM,
//! LocalAI, KoboldCpp, Azure OpenAI, Groq, Together, OpenRouter, Mistral,
//! Anyscale, and so on.
//!
//! Wire format is identical to OpenAI's native API, so request encoding +
//! SSE decoding delegate to `openai.rs`. Differences handled here:
//! - `api_key` is optional — local servers typically don't need one, so
//!   `Authorization` is only added when present.
//! - `/v1/models` discovery responses can be sparse (some servers omit
//!   `display_name`, `capabilities`, etc.). See `decode_models_response`.

use serde::Deserialize;
use wafer_core::interfaces::llm::service::{ChatRequest, ModelInfo};

use super::{config::ProviderConfig, openai};
use crate::llm_wire::openai::MaxTokensField;

/// Encode a chat request for any OpenAI-compatible endpoint. Unlike
/// `openai::encode_chat_request`, a missing `api_key` is not an error — the
/// `Authorization` header is simply omitted.
///
/// The output-token budget goes out as `max_tokens`, the original spelling:
/// it is what Ollama, llama.cpp, vLLM, LM Studio, Groq, Together and
/// OpenRouter accept, and most of them do not know OpenAI's newer
/// `max_completion_tokens` at all. A provider that declares a
/// `max_tokens_field` overrides that — Azure OpenAI is configured on this
/// protocol and its reasoning deployments accept only the newer spelling.
pub fn encode_chat_request(
    req: &ChatRequest,
    provider: &ProviderConfig,
    resolved_api_key: Option<&str>,
) -> Result<openai::EncodedRequest, openai::EncodeError> {
    match resolved_api_key {
        Some(_) => openai::encode_chat_request_as(
            req,
            provider,
            resolved_api_key,
            MaxTokensField::MaxTokens,
        ),
        None => {
            // Call the OpenAI encoder with a placeholder key, then strip
            // Authorization. Keeps the wire body identical to the native
            // OpenAI path and concentrates wire-format knowledge in one place.
            let (url, mut headers, body) = openai::encode_chat_request_as(
                req,
                provider,
                Some("placeholder"),
                MaxTokensField::MaxTokens,
            )?;
            headers.remove("Authorization");
            Ok((url, headers, body))
        }
    }
}

/// Re-export the OpenAI SSE decoder — the streaming wire format is identical
/// across every OpenAI-compatible endpoint we've seen.
pub use super::openai::OpenAiSseDecoder as SseDecoder;

/// Parse an OpenAI-compatible `/v1/models` response into `ModelInfo`s.
///
/// Tolerant: missing `object`, `owned_by`, `created` fields are fine. Only
/// `id` is required — it becomes the `model_id`. `display_name` falls back to
/// the id; `capabilities` defaults to all-false / unlimited.
pub fn decode_models_response(
    bytes: &[u8],
    provider_name: &str,
) -> Result<Vec<ModelInfo>, DecodeError> {
    let resp: ModelsResponse =
        serde_json::from_slice(bytes).map_err(|e| DecodeError::Decode(e.to_string()))?;
    Ok(resp
        .data
        .into_iter()
        .map(|m| {
            ModelInfo::new(provider_name, &m.id, &m.id)
            // Capabilities default to all-false; callers who know the model
            // set caps via admin UI rather than trying to infer from a
            // provider that may or may not report them.
        })
        .collect())
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("models response decode: {0}")]
    Decode(String),
}

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::llm::service::ChatMessage;

    use super::{
        super::config::{ProviderConfig, ProviderProtocol},
        *,
    };

    fn local_provider() -> ProviderConfig {
        ProviderConfig::new(
            "local-ollama",
            ProviderProtocol::OpenAiCompatible,
            "http://localhost:11434/v1",
        )
    }

    #[test]
    fn encodes_without_auth_header_when_api_key_missing() {
        let req = ChatRequest::new("local-ollama", "llama3", vec![ChatMessage::user("hello")]);
        let (url, headers, body) = encode_chat_request(&req, &local_provider(), None).unwrap();
        assert_eq!(url, "http://localhost:11434/v1/chat/completions");
        assert!(
            !headers.contains_key("Authorization"),
            "no Authorization header when api_key is None"
        );
        // Body shape is the same as OpenAI native.
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "llama3");
        assert_eq!(json["stream"], true);
    }

    #[test]
    fn encodes_with_auth_header_when_api_key_present() {
        let req = ChatRequest::new("local-ollama", "llama3", vec![ChatMessage::user("hi")]);
        let (_, headers, _) =
            encode_chat_request(&req, &local_provider(), Some("shared-secret")).unwrap();
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer shared-secret")
        );
    }

    /// The compatible protocol sends `max_tokens` and nothing else — the
    /// inverse of the native path, and for the inverse reason: Ollama,
    /// llama.cpp, vLLM, LM Studio and the hosted OpenAI-compatible gateways
    /// accept the original spelling, and most reject or ignore
    /// `max_completion_tokens`. A budget that lands in a field the server
    /// ignores is a silently uncapped reply.
    #[test]
    fn the_compatible_protocol_sends_only_max_tokens() {
        let mut req = ChatRequest::new("local-ollama", "llama3", vec![ChatMessage::user("hi")]);
        req.params.max_tokens = Some(4096);

        for key in [None, Some("shared-secret")] {
            let (_, _, body) = encode_chat_request(&req, &local_provider(), key).unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["max_tokens"], 4096, "api_key={key:?}");
            assert!(
                json.get("max_completion_tokens").is_none(),
                "api_key={key:?}: a compatible server gets the spelling it knows, got: {json}"
            );
        }
    }

    /// A provider that declares `max_completion_tokens` gets it, on the
    /// protocol whose default is the other spelling.
    ///
    /// Azure OpenAI is configured here, not on `open_ai` — it is a different
    /// URL shape with its own deployment path — and its reasoning deployments
    /// reject `max_tokens`. The protocol default alone left that operator with
    /// no reachable configuration at all.
    #[test]
    fn a_declared_max_tokens_field_overrides_the_protocols_spelling() {
        let provider = local_provider().with_max_tokens_field(MaxTokensField::MaxCompletionTokens);
        let mut req = ChatRequest::new("local-ollama", "o3-mini", vec![ChatMessage::user("hi")]);
        req.params.max_tokens = Some(4096);

        for key in [None, Some("azure-key")] {
            let (_, _, body) = encode_chat_request(&req, &provider, key).unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["max_completion_tokens"], 4096, "api_key={key:?}");
            assert!(
                json.get("max_tokens").is_none(),
                "api_key={key:?}: one spelling only, got: {json}"
            );
        }
    }

    #[test]
    fn decodes_minimal_models_response() {
        let body = br#"{"data":[{"id":"llama3"},{"id":"mistral"}]}"#;
        let models = decode_models_response(body, "local-ollama").unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].model_id, "llama3");
        assert_eq!(models[0].backend_id, "local-ollama");
        assert_eq!(models[0].display_name, "llama3");
    }

    #[test]
    fn decodes_empty_models_response() {
        let body = br#"{"data":[]}"#;
        let models = decode_models_response(body, "local-ollama").unwrap();
        assert!(models.is_empty());
    }

    #[test]
    fn decodes_response_with_extra_fields_tolerantly() {
        let body = br#"{
            "object": "list",
            "data": [
                {"id":"qwen2","object":"model","created":1700000000,"owned_by":"alibaba"}
            ]
        }"#;
        let models = decode_models_response(body, "local-ollama").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "qwen2");
    }
}
