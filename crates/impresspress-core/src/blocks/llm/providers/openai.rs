//! OpenAI native API wire format.
//!
//! Encoder: translates a wafer-core `ChatRequest` into a `POST /v1/chat/completions`
//! payload. Decoder: parses OpenAI's SSE frames (`data: {json}\n\n` + terminal
//! `data: [DONE]`) into `ChatChunk` events.
//!
//! See <https://platform.openai.com/docs/api-reference/chat/create> for the
//! reference request/response shapes.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wafer_core::interfaces::llm::service::{
    ChatContent, ChatMessage, ChatRequest, ChatRole, ContentPart, ResponseFormat, ToolCall,
    ToolDefinition,
};

use super::config::ProviderConfig;

/// Build an `(url, headers, body)` triple for a streaming OpenAI chat
/// completion. Callers POST the body with the given headers.
///
/// Returns `Err` only if the configured provider is missing the required
/// `api_key` — we never silently omit `Authorization` on the OpenAI native
/// protocol, unlike `openai_compatible` which may.
/// `(url, headers, body)` triple produced by the encoder.
pub type EncodedRequest = (String, HashMap<String, String>, Vec<u8>);

pub fn encode_chat_request(
    req: &ChatRequest,
    provider: &ProviderConfig,
    resolved_api_key: Option<&str>,
) -> Result<EncodedRequest, EncodeError> {
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

    let body = OpenAiRequest::from_chat_request(req);
    let bytes = serde_json::to_vec(&body).map_err(|e| EncodeError::Serialize(e.to_string()))?;
    Ok((url, headers, bytes))
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("missing api_key for openai provider")]
    MissingApiKey,
    #[error("serialize request body: {0}")]
    Serialize(String),
}

// ---------- Wire format types ----------

#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<OpenAiResponseFormat<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool<'a>>,
    /// When `true`, OpenAI includes a usage frame as the last SSE event
    /// (`stream_options.include_usage`). We always ask for it so our decoder
    /// can emit a terminal `ChatChunk` with `TokenUsage`.
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

impl<'a> OpenAiRequest<'a> {
    fn from_chat_request(req: &'a ChatRequest) -> Self {
        let stop = if req.params.stop_sequences.is_empty() {
            None
        } else {
            Some(req.params.stop_sequences.as_slice())
        };
        let response_format = req
            .params
            .response_format
            .as_ref()
            .map(encode_response_format);
        let tools = req.tools.iter().map(encode_tool).collect::<Vec<_>>();

        Self {
            model: &req.model,
            messages: req.messages.iter().map(encode_message).collect(),
            stream: true,
            temperature: req.params.temperature,
            max_tokens: req.params.max_tokens,
            top_p: req.params.top_p,
            seed: req.params.seed,
            stop,
            response_format,
            tools,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
        }
    }
}

#[derive(Serialize)]
struct OpenAiMessage<'a> {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<OpenAiContent<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OpenAiToolCall<'a>>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum OpenAiContent<'a> {
    Text(&'a str),
    Parts(Vec<OpenAiContentPart<'a>>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum OpenAiContentPart<'a> {
    #[serde(rename = "text")]
    Text { text: &'a str },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: OpenAiImageUrl<'a> },
}

#[derive(Serialize)]
struct OpenAiImageUrl<'a> {
    url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
}

#[derive(Serialize)]
struct OpenAiToolCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    function: OpenAiToolCallFunction<'a>,
}

#[derive(Serialize)]
struct OpenAiToolCallFunction<'a> {
    name: &'a str,
    /// Wire format is a JSON-encoded string, not a nested object.
    arguments: String,
}

#[derive(Serialize)]
struct OpenAiTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OpenAiToolFunction<'a>,
}

#[derive(Serialize)]
struct OpenAiToolFunction<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiResponseFormat<'a> {
    Text,
    #[serde(rename = "json_object")]
    Json,
    #[serde(rename = "json_schema")]
    JsonSchema {
        json_schema: &'a serde_json::Value,
    },
}

// ---------- Translation helpers ----------

fn encode_role(role: ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

fn encode_message(m: &ChatMessage) -> OpenAiMessage<'_> {
    let content = match &m.content {
        ChatContent::Text(s) => Some(OpenAiContent::Text(s)),
        ChatContent::Parts(parts) => Some(OpenAiContent::Parts(
            parts.iter().map(encode_part).collect(),
        )),
    };
    // Assistant messages invoking tools send `tool_calls` and may omit content.
    let content = if content.is_some()
        && matches!(&m.content, ChatContent::Text(s) if s.is_empty())
        && !m.tool_calls.is_empty()
    {
        None
    } else {
        content
    };

    OpenAiMessage {
        role: encode_role(m.role),
        content,
        tool_call_id: m.tool_call_id.as_deref(),
        tool_calls: m.tool_calls.iter().map(encode_tool_call).collect(),
    }
}

fn encode_part(p: &ContentPart) -> OpenAiContentPart<'_> {
    match p {
        ContentPart::Text(s) => OpenAiContentPart::Text { text: s },
        ContentPart::ImageUrl { url, detail } => OpenAiContentPart::ImageUrl {
            image_url: OpenAiImageUrl {
                url,
                detail: detail.as_deref(),
            },
        },
        // OpenAI's image API accepts data URLs, but encoding bytes here would
        // need a base64 dep that impresspress-core doesn't already carry. Callers
        // that want to send raw bytes can encode them into a data URL upstream
        // and use `ImageUrl`. Fall back to a text part labeling what happened.
        ContentPart::ImageBytes { .. } => OpenAiContentPart::Text {
            text: "[image bytes unsupported — encode as data URL in ImageUrl]",
        },
    }
}

fn encode_tool_call(call: &ToolCall) -> OpenAiToolCall<'_> {
    OpenAiToolCall {
        id: &call.id,
        kind: "function",
        function: OpenAiToolCallFunction {
            name: &call.name,
            arguments: call.arguments.to_string(),
        },
    }
}

fn encode_tool(t: &ToolDefinition) -> OpenAiTool<'_> {
    OpenAiTool {
        kind: "function",
        function: OpenAiToolFunction {
            name: &t.name,
            description: &t.description,
            parameters: &t.parameters,
        },
    }
}

fn encode_response_format(r: &ResponseFormat) -> OpenAiResponseFormat<'_> {
    match r {
        ResponseFormat::Text => OpenAiResponseFormat::Text,
        ResponseFormat::Json => OpenAiResponseFormat::Json,
        ResponseFormat::JsonSchema(v) => OpenAiResponseFormat::JsonSchema { json_schema: v },
    }
}

// ===========================================================================
// SSE decoder
// ===========================================================================

use wafer_core::interfaces::llm::service::{ChatChunk, FinishReason, TokenUsage};

use super::sse::{DecodeBatch, SseFrame, SseFrameStream};

/// Stateful line-by-line SSE decoder. Feed it successive chunks of response
/// body bytes (they may split inside a frame); it buffers until a blank-line
/// terminator and emits zero-or-more `ChatChunk`s per chunk of input.
///
/// Handles:
///  - `data: {json}\n\n` frames with a choice containing `delta.content` /
///    `delta.tool_calls[]` / `finish_reason`.
///  - `data: [DONE]` terminal sentinel — decoded as `DecodedFrame::Done`.
///  - Top-level `usage` object (emitted by OpenAI when
///    `stream_options.include_usage=true`) — becomes a meta-only chunk with
///    `TokenUsage`.
///  - Malformed / unknown frames are skipped silently; tracing::warn logs them
///    so operators can notice.
pub struct OpenAiSseDecoder {
    frames: SseFrameStream,
    /// Tool-call index -> in-flight id. OpenAI streams tool_calls with
    /// `index` + optional `id` + partial `function.name` + partial
    /// `function.arguments`. We emit `ToolCallStart` on first sight of
    /// `id`+`name`, then `ToolCallArguments` per arg delta, and
    /// `ToolCallComplete` only when the stream terminates.
    started: Vec<String>,
}

// Tool-call / usage chunks are built via wafer-core's explicit constructors,
// so the wire shape stays in the producer crate. Earlier versions of this
// file round-tripped through `serde_json::from_value(...).expect(...)`; that
// turned every SSE frame into a panic if wafer-core ever renamed a variant.

impl OpenAiSseDecoder {
    pub fn new() -> Self {
        Self {
            frames: SseFrameStream::new(),
            started: Vec::new(),
        }
    }

    /// Feed more bytes. Returns the decoded chunks + whether the stream
    /// terminated (`[DONE]` seen). Tool-call `Complete` frames for any
    /// in-flight ids are emitted on terminal.
    pub fn push(&mut self, bytes: &[u8]) -> DecodeBatch {
        if !self.frames.feed(bytes) {
            tracing::warn!("openai sse: non-utf8 bytes — dropping");
            return DecodeBatch::default();
        }

        let mut out = Vec::new();
        let mut done = false;

        while let Some(frame) = self.frames.next_frame() {
            if let Some(outcome) = self.decode_frame(&frame) {
                match outcome {
                    FrameOutcome::Chunks(cs) => out.extend(cs),
                    FrameOutcome::Done => {
                        done = true;
                        for id in std::mem::take(&mut self.started) {
                            out.push(ChatChunk::tool_call_complete(id));
                        }
                        break;
                    }
                }
            }
        }

        DecodeBatch { chunks: out, done }
    }

    fn decode_frame(&mut self, frame: &SseFrame) -> Option<FrameOutcome> {
        // OpenAI uses only the `data:` field; `event:` is ignored.
        if frame.data.is_empty() {
            return None;
        }
        if frame.data == "[DONE]" {
            return Some(FrameOutcome::Done);
        }
        match serde_json::from_str::<OpenAiStreamFrame>(&frame.data) {
            Ok(parsed) => Some(FrameOutcome::Chunks(self.translate(parsed))),
            Err(e) => {
                tracing::warn!(error = %e, payload = %frame.data, "openai sse: decode failed");
                None
            }
        }
    }

    fn translate(&mut self, frame: OpenAiStreamFrame) -> Vec<ChatChunk> {
        let mut out = Vec::new();

        // OpenAI's usage frame has no choices but populates `usage`.
        if let Some(u) = frame.usage {
            let mut usage = TokenUsage::new(u.prompt_tokens, u.completion_tokens);
            if let Some(cached) = u
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
            {
                usage = usage.with_cached(cached);
            }
            if let Some(reasoning) = u
                .completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens)
            {
                usage = usage.with_reasoning(reasoning);
            }
            out.push(ChatChunk::usage(usage));
        }

        for choice in frame.choices.into_iter() {
            if let Some(content) = choice.delta.content {
                // OpenAI never emits empty content deltas in practice; if it
                // ever does, `ChatChunk::text` still encodes correctly.
                out.push(ChatChunk::text(content));
            }
            for tc in choice.delta.tool_calls.into_iter() {
                // First sighting with id + name ⇒ ToolCallStart.
                if let (Some(id), Some(name)) = (
                    tc.id.clone(),
                    tc.function.as_ref().and_then(|f| f.name.clone()),
                ) {
                    if !self.started.contains(&id) {
                        self.started.push(id.clone());
                        out.push(ChatChunk::tool_call_start(id, name));
                        continue;
                    }
                }
                // Subsequent frames carry argument deltas. OpenAI omits id
                // after the first frame but always supplies the index.
                if let Some(f) = tc.function {
                    if let Some(args) = f.arguments {
                        // `started` is a parallel array keyed on insertion order.
                        let id = self.started.get(tc.index as usize).cloned().or(tc.id);
                        if let Some(id) = id {
                            out.push(ChatChunk::tool_call_arguments(id, args));
                        }
                    }
                }
            }
            if let Some(reason) = choice.finish_reason {
                out.push(ChatChunk::finish(map_finish_reason(&reason), None));
            }
        }
        out
    }
}

impl Default for OpenAiSseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

enum FrameOutcome {
    Chunks(Vec<ChatChunk>),
    Done,
}

#[derive(Deserialize)]
struct OpenAiStreamFrame {
    #[serde(default)]
    choices: Vec<OpenAiStreamChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiStreamChoice {
    #[serde(default)]
    delta: OpenAiStreamDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct OpenAiStreamDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiStreamToolCall>,
}

#[derive(Deserialize)]
struct OpenAiStreamToolCall {
    #[serde(default)]
    index: u32,
    id: Option<String>,
    function: Option<OpenAiStreamToolCallFunction>,
}

#[derive(Deserialize)]
struct OpenAiStreamToolCallFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    prompt_tokens_details: Option<OpenAiUsageDetails>,
    completion_tokens_details: Option<OpenAiUsageDetails>,
}

#[derive(Deserialize)]
struct OpenAiUsageDetails {
    cached_tokens: Option<u32>,
    reasoning_tokens: Option<u32>,
}

fn map_finish_reason(s: &str) -> FinishReason {
    match s {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" => FinishReason::ToolCall,
        "content_filter" => FinishReason::ContentFilter,
        _ => FinishReason::Error,
    }
}

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::llm::service::{
        ChatMessage, ChatParams, ChatRequest, ToolDefinition,
    };

    use super::{
        super::config::{ProviderConfig, ProviderProtocol},
        *,
    };

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
        assert_eq!(json["max_tokens"], 512);
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

    #[test]
    fn rejects_missing_api_key() {
        let req = ChatRequest::new("openai-main", "gpt-4o", vec![ChatMessage::user("hi")]);
        assert!(matches!(
            encode_chat_request(&req, &openai_provider(), None),
            Err(EncodeError::MissingApiKey)
        ));
    }

    // ---------- SSE decoder ----------

    use wafer_core::interfaces::llm::service::{ChunkDelta, FinishReason};

    fn decode_all(input: &str) -> Vec<ChatChunk> {
        let mut decoder = OpenAiSseDecoder::new();
        decoder.push(input.as_bytes()).chunks
    }

    #[test]
    fn decodes_simple_text_delta() {
        let frame =
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n";
        let chunks = decode_all(frame);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0].delta, ChunkDelta::Text(t) if t == "hello"));
    }

    #[test]
    fn decodes_sequence_of_text_deltas() {
        let stream = "\
            data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
            data: {\"choices\":[{\"delta\":{\"content\":\" wo\"}}]}\n\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"rld\"}}]}\n\n\
        ";
        let chunks = decode_all(stream);
        let texts: Vec<_> = chunks
            .iter()
            .filter_map(|c| match &c.delta {
                ChunkDelta::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["hel", "lo", " wo", "rld"]);
    }

    #[test]
    fn done_sentinel_terminates() {
        let stream = "\
            data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n\
            data: [DONE]\n\n\
        ";
        let mut decoder = OpenAiSseDecoder::new();
        let batch = decoder.push(stream.as_bytes());
        assert!(batch.done);
    }

    #[test]
    fn decodes_split_frames_across_pushes() {
        let mut decoder = OpenAiSseDecoder::new();
        let part1 = "data: {\"choices\":[{\"delta\":{\"content\":";
        let part2 = "\"hello\"}}]}\n\n";
        assert_eq!(decoder.push(part1.as_bytes()).chunks, vec![]);
        let chunks = decoder.push(part2.as_bytes()).chunks;
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0].delta, ChunkDelta::Text(t) if t == "hello"));
    }

    #[test]
    fn decodes_finish_reason_on_terminal_choice() {
        let frame = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
        let chunks = decode_all(frame);
        assert!(chunks
            .iter()
            .any(|c| c.finish_reason == Some(FinishReason::Stop)));
    }

    #[test]
    fn decodes_usage_frame_as_terminal_meta_chunk() {
        let frame =
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":42}}\n\n";
        let chunks = decode_all(frame);
        let usage = chunks.iter().find_map(|c| c.usage.as_ref()).unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 42);
    }

    #[test]
    fn decodes_tool_call_start_and_arguments_stream() {
        let stream = "\
            data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"lookup\"}}]}}]}\n\n\
            data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"x\\\":\"}}]}}]}\n\n\
            data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]}}]}\n\n\
            data: [DONE]\n\n\
        ";
        let mut decoder = OpenAiSseDecoder::new();
        let batch = decoder.push(stream.as_bytes());
        assert!(batch.done);

        let starts = batch
            .chunks
            .iter()
            .filter(|c| matches!(&c.delta, ChunkDelta::ToolCallStart { .. }))
            .count();
        let args = batch
            .chunks
            .iter()
            .filter(|c| matches!(&c.delta, ChunkDelta::ToolCallArguments { .. }))
            .count();
        let completes = batch
            .chunks
            .iter()
            .filter(|c| matches!(&c.delta, ChunkDelta::ToolCallComplete { .. }))
            .count();
        assert_eq!(starts, 1, "exactly one ToolCallStart");
        assert_eq!(args, 2, "two ToolCallArguments deltas");
        assert_eq!(completes, 1, "ToolCallComplete on terminal");
    }

    #[test]
    fn malformed_frame_is_skipped() {
        let stream = "\
            data: not-valid-json\n\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
        ";
        let chunks = decode_all(stream);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0].delta, ChunkDelta::Text(t) if t == "ok"));
    }
}
