//! Anthropic Messages API wire format.
//!
//! Encoder: translates a wafer-core `ChatRequest` into a `POST /v1/messages`
//! payload. Decoder: parses Anthropic's event-stream frames (`event: …\ndata:
//! …\n\n`) into `ChatChunk` events.
//!
//! See <https://docs.anthropic.com/claude/reference/messages_post> and
//! <https://docs.anthropic.com/claude/reference/messages-streaming>.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wafer_core::interfaces::llm::service::{
    ChatChunk, ChatContent, ChatMessage, ChatRequest, ChatRole, ContentPart, FinishReason,
    TokenUsage, ToolDefinition,
};

use super::{
    config::ProviderConfig,
    sse::{DecodeBatch, SseFrame, SseFrameStream},
};

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// `(url, headers, body)` triple produced by the encoder.
pub type EncodedRequest = (String, HashMap<String, String>, Vec<u8>);

pub fn encode_chat_request(
    req: &ChatRequest,
    provider: &ProviderConfig,
    resolved_api_key: Option<&str>,
) -> Result<EncodedRequest, EncodeError> {
    let url = format!("{}/messages", provider.endpoint.trim_end_matches('/'));

    let mut headers = HashMap::new();
    headers.insert("Content-Type".into(), "application/json".into());
    headers.insert("anthropic-version".into(), ANTHROPIC_VERSION.into());
    match resolved_api_key {
        Some(key) => {
            headers.insert("x-api-key".into(), key.into());
        }
        None => return Err(EncodeError::MissingApiKey),
    }

    let body = AnthropicRequest::from_chat_request(req)?;
    let bytes = serde_json::to_vec(&body).map_err(|e| EncodeError::Serialize(e.to_string()))?;
    Ok((url, headers, bytes))
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("missing api_key for anthropic provider")]
    MissingApiKey,
    #[error("max_tokens is required by Anthropic; set ChatParams::max_tokens")]
    MissingMaxTokens,
    #[error("serialize request body: {0}")]
    Serialize(String),
}

// ---------- Wire format types ----------

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    /// Anthropic requires max_tokens — unlike OpenAI where it's optional.
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<AnthropicMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    stop_sequences: &'a [String],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool<'a>>,
}

impl<'a> AnthropicRequest<'a> {
    fn from_chat_request(req: &'a ChatRequest) -> Result<Self, EncodeError> {
        let max_tokens = req.params.max_tokens.ok_or(EncodeError::MissingMaxTokens)?;

        // Pull system messages out into the top-level `system` field.
        // Anthropic doesn't accept `role: "system"` inside messages[].
        let system = extract_system(&req.messages);
        let messages = req
            .messages
            .iter()
            .filter(|m| !matches!(m.role, ChatRole::System))
            .map(encode_message)
            .collect();

        Ok(Self {
            model: &req.model,
            max_tokens,
            stream: true,
            system,
            messages,
            temperature: req.params.temperature,
            top_p: req.params.top_p,
            stop_sequences: &req.params.stop_sequences,
            tools: req.tools.iter().map(encode_tool).collect(),
        })
    }
}

fn extract_system(msgs: &[ChatMessage]) -> Option<&str> {
    msgs.iter().find_map(|m| match (&m.role, &m.content) {
        (ChatRole::System, ChatContent::Text(s)) => Some(s.as_str()),
        _ => None,
    })
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'static str,
    content: AnthropicContent<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum AnthropicContent<'a> {
    Text(&'a str),
    Blocks(Vec<AnthropicContentBlock<'a>>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock<'a> {
    #[serde(rename = "text")]
    Text { text: &'a str },
    #[serde(rename = "image")]
    Image { source: AnthropicImageSource<'a> },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: &'a serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: &'a str,
        content: &'a str,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicImageSource<'a> {
    #[serde(rename = "url")]
    Url { url: &'a str },
    // Note: base64 source encoding omitted — callers provide URLs.
}

#[derive(Serialize)]
struct AnthropicTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a serde_json::Value,
}

fn encode_role(role: ChatRole) -> &'static str {
    match role {
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "user", // tool results are user-role messages on Anthropic
        _ => "user",              // system handled separately; non_exhaustive fallback
    }
}

fn encode_message(m: &ChatMessage) -> AnthropicMessage<'_> {
    // Anthropic tool-result messages must use blocks with `tool_result`.
    if matches!(m.role, ChatRole::Tool) {
        if let ChatContent::Text(s) = &m.content {
            let id = m.tool_call_id.as_deref().unwrap_or("");
            return AnthropicMessage {
                role: "user",
                content: AnthropicContent::Blocks(vec![AnthropicContentBlock::ToolResult {
                    tool_use_id: id,
                    content: s,
                }]),
            };
        }
    }

    // Assistant messages carrying tool calls use mixed text + tool_use blocks.
    if matches!(m.role, ChatRole::Assistant) && !m.tool_calls.is_empty() {
        let mut blocks = Vec::new();
        if let ChatContent::Text(s) = &m.content {
            if !s.is_empty() {
                blocks.push(AnthropicContentBlock::Text { text: s });
            }
        }
        for tc in &m.tool_calls {
            blocks.push(AnthropicContentBlock::ToolUse {
                id: &tc.id,
                name: &tc.name,
                input: &tc.arguments,
            });
        }
        return AnthropicMessage {
            role: "assistant",
            content: AnthropicContent::Blocks(blocks),
        };
    }

    let content = match &m.content {
        ChatContent::Text(s) => AnthropicContent::Text(s),
        ChatContent::Parts(parts) => {
            AnthropicContent::Blocks(parts.iter().map(encode_part).collect())
        }
    };

    AnthropicMessage {
        role: encode_role(m.role),
        content,
    }
}

fn encode_part(p: &ContentPart) -> AnthropicContentBlock<'_> {
    match p {
        ContentPart::Text(s) => AnthropicContentBlock::Text { text: s },
        ContentPart::ImageUrl { url, .. } => AnthropicContentBlock::Image {
            source: AnthropicImageSource::Url { url },
        },
        ContentPart::ImageBytes { .. } => AnthropicContentBlock::Text {
            text: "[image bytes unsupported — use ImageUrl]",
        },
    }
}

fn encode_tool(t: &ToolDefinition) -> AnthropicTool<'_> {
    AnthropicTool {
        name: &t.name,
        description: &t.description,
        input_schema: &t.parameters,
    }
}

// ===========================================================================
// SSE decoder
// ===========================================================================

/// Anthropic's streaming format emits a sequence of typed events, each with
/// `event: name\ndata: {json}\n\n`. The types we care about:
///
/// - `message_start` — new message begins; includes initial usage.
/// - `content_block_start` — a block (`text` / `tool_use`) opens; index marks it.
/// - `content_block_delta` — `text_delta` carrying `text`, or
///   `input_json_delta` carrying tool arguments.
/// - `content_block_stop` — block closes; emit `ToolCallComplete` when the
///   block was a tool_use.
/// - `message_delta` — carries `stop_reason` + incremental usage.
/// - `message_stop` — terminal.
pub struct AnthropicSseDecoder {
    frames: SseFrameStream,
    /// Per-content-block index: the tool_use id (if it was a tool_use block).
    /// Keyed by Anthropic's `index`, which is stable within a message.
    tool_blocks: Vec<Option<String>>,
}

/// Hard cap on the number of distinct `content_block` indices we'll track
/// for a single stream. Real-world responses use a handful of blocks; this
/// just keeps a malicious or buggy server from growing `tool_blocks`
/// without bound.
const MAX_CONTENT_BLOCK_INDEX: u32 = 1024;

impl AnthropicSseDecoder {
    pub fn new() -> Self {
        Self {
            frames: SseFrameStream::new(),
            tool_blocks: Vec::new(),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> DecodeBatch {
        if !self.frames.feed(bytes) {
            tracing::warn!("anthropic sse: non-utf8 bytes — dropping");
            return DecodeBatch::default();
        }

        let mut out = Vec::new();
        let mut done = false;

        while let Some(frame) = self.frames.next_frame() {
            let (chunks, terminal) = self.decode_frame(&frame);
            out.extend(chunks);
            if terminal {
                done = true;
                break;
            }
        }

        DecodeBatch { chunks: out, done }
    }

    fn decode_frame(&mut self, frame: &SseFrame) -> (Vec<ChatChunk>, bool) {
        let event_name = frame.event.as_deref().unwrap_or_default();
        let data_payload = frame.data.as_str();
        if data_payload.is_empty() {
            return (Vec::new(), false);
        }

        match event_name {
            "content_block_start" => {
                match serde_json::from_str::<AnthropicBlockStart>(data_payload) {
                    Ok(s) => {
                        if !self.ensure_block_slot(s.index) {
                            return (Vec::new(), false);
                        }
                        match s.content_block {
                            AnthropicBlockStartContent::Text { .. } => (Vec::new(), false),
                            AnthropicBlockStartContent::ToolUse { id, name, .. } => {
                                self.tool_blocks[s.index as usize] = Some(id.clone());
                                (vec![ChatChunk::tool_call_start(id, name)], false)
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "anthropic sse: content_block_start decode");
                        (Vec::new(), false)
                    }
                }
            }
            "content_block_delta" => {
                match serde_json::from_str::<AnthropicBlockDelta>(data_payload) {
                    Ok(d) => {
                        if !self.ensure_block_slot(d.index) {
                            return (Vec::new(), false);
                        }
                        match d.delta {
                            AnthropicBlockDeltaKind::TextDelta { text } => {
                                if text.is_empty() {
                                    (Vec::new(), false)
                                } else {
                                    (vec![ChatChunk::text(text)], false)
                                }
                            }
                            AnthropicBlockDeltaKind::InputJsonDelta { partial_json } => {
                                if let Some(Some(id)) = self.tool_blocks.get(d.index as usize) {
                                    (
                                        vec![ChatChunk::tool_call_arguments(id, partial_json)],
                                        false,
                                    )
                                } else {
                                    (Vec::new(), false)
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "anthropic sse: content_block_delta decode");
                        (Vec::new(), false)
                    }
                }
            }
            "content_block_stop" => {
                match serde_json::from_str::<AnthropicBlockStop>(data_payload) {
                    Ok(s) => {
                        if let Some(Some(id)) = self.tool_blocks.get(s.index as usize).cloned() {
                            (vec![ChatChunk::tool_call_complete(id)], false)
                        } else {
                            (Vec::new(), false)
                        }
                    }
                    Err(_) => (Vec::new(), false),
                }
            }
            "message_delta" => match serde_json::from_str::<AnthropicMessageDelta>(data_payload) {
                Ok(md) => {
                    let mut chunks = Vec::new();
                    if let Some(reason) = md.delta.stop_reason {
                        chunks.push(ChatChunk::finish(map_stop_reason(&reason), None));
                    }
                    if let Some(u) = md.usage {
                        // Only emit usage when at least one field is set —
                        // mid-stream message_delta frames carry just stop_reason.
                        if u.input_tokens.is_some() || u.output_tokens.is_some() {
                            let usage = TokenUsage::new(
                                u.input_tokens.unwrap_or(0),
                                u.output_tokens.unwrap_or(0),
                            );
                            chunks.push(ChatChunk::usage(usage));
                        }
                    }
                    (chunks, false)
                }
                Err(_) => (Vec::new(), false),
            },
            "message_stop" => (Vec::new(), true),
            _ => (Vec::new(), false),
        }
    }

    /// Returns `true` if the slot is reachable, `false` if the index exceeds
    /// our cap (malicious / buggy server emitting unbounded indices). Capping
    /// here keeps `tool_blocks` from growing without bound across a long
    /// stream — see [`MAX_CONTENT_BLOCK_INDEX`].
    fn ensure_block_slot(&mut self, index: u32) -> bool {
        if index > MAX_CONTENT_BLOCK_INDEX {
            tracing::warn!(
                index,
                cap = MAX_CONTENT_BLOCK_INDEX,
                "anthropic sse: content_block index exceeds cap — dropping",
            );
            return false;
        }
        while self.tool_blocks.len() <= index as usize {
            self.tool_blocks.push(None);
        }
        true
    }
}

impl Default for AnthropicSseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ---- Wire types for the decoder ----

#[derive(Deserialize)]
struct AnthropicBlockStart {
    index: u32,
    content_block: AnthropicBlockStartContent,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AnthropicBlockStartContent {
    // Wire variant carries a `text` field we don't currently emit — the
    // content flows through `content_block_delta` events instead.
    #[serde(rename = "text")]
    Text {
        #[serde(default, rename = "text")]
        _text: serde_json::Value,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        // Initial input (typically empty `{}`) — actual args arrive via
        // input_json_delta frames.
        #[serde(default, rename = "input")]
        _input: serde_json::Value,
    },
}

#[derive(Deserialize)]
struct AnthropicBlockDelta {
    index: u32,
    delta: AnthropicBlockDeltaKind,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AnthropicBlockDeltaKind {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Deserialize)]
struct AnthropicBlockStop {
    index: u32,
}

#[derive(Deserialize)]
struct AnthropicMessageDelta {
    delta: AnthropicMessageDeltaInner,
    usage: Option<AnthropicMessageDeltaUsage>,
}

#[derive(Deserialize)]
struct AnthropicMessageDeltaInner {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicMessageDeltaUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}

fn map_stop_reason(s: &str) -> FinishReason {
    match s {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCall,
        _ => FinishReason::Error,
    }
}

// ChatChunk / TokenUsage are built via wafer-core's typed constructors (see
// `ChatChunk::tool_call_*` and `TokenUsage::new`) — no wire-shape
// round-tripping in this crate.

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::llm::service::{ChatMessage, ChatParams, ChatRequest, ChunkDelta};

    use super::{
        super::config::{ProviderConfig, ProviderProtocol},
        *,
    };

    fn anthropic_provider() -> ProviderConfig {
        ProviderConfig::new(
            "anthropic-main",
            ProviderProtocol::Anthropic,
            "https://api.anthropic.com/v1",
        )
    }

    fn req_with_max_tokens(msgs: Vec<ChatMessage>) -> ChatRequest {
        let mut req = ChatRequest::new("anthropic-main", "claude-3-5-sonnet", msgs);
        req.params = ChatParams {
            max_tokens: Some(1024),
            ..Default::default()
        };
        req
    }

    // ---------- encoder ----------

    #[test]
    fn encodes_user_message_with_system_extracted() {
        let req = req_with_max_tokens(vec![
            ChatMessage::system("be terse"),
            ChatMessage::user("hi"),
        ]);
        let (url, headers, body) =
            encode_chat_request(&req, &anthropic_provider(), Some("sk-ant-test")).unwrap();
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
        assert_eq!(
            headers.get("x-api-key").map(String::as_str),
            Some("sk-ant-test")
        );
        assert_eq!(
            headers.get("anthropic-version").map(String::as_str),
            Some(ANTHROPIC_VERSION)
        );

        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "claude-3-5-sonnet");
        assert_eq!(json["max_tokens"], 1024);
        assert_eq!(json["system"], "be terse");
        // system must not appear inside messages[]
        let msgs = json["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn rejects_missing_max_tokens() {
        let req = ChatRequest::new(
            "anthropic-main",
            "claude-3-5-sonnet",
            vec![ChatMessage::user("hi")],
        );
        assert!(matches!(
            encode_chat_request(&req, &anthropic_provider(), Some("sk")),
            Err(EncodeError::MissingMaxTokens)
        ));
    }

    #[test]
    fn rejects_missing_api_key() {
        let req = req_with_max_tokens(vec![ChatMessage::user("hi")]);
        assert!(matches!(
            encode_chat_request(&req, &anthropic_provider(), None),
            Err(EncodeError::MissingApiKey)
        ));
    }

    #[test]
    fn encodes_tool_result_as_user_block() {
        let req = req_with_max_tokens(vec![
            ChatMessage::user("what is the weather?"),
            ChatMessage::tool("call_1", "sunny, 72F"),
        ]);
        let (_, _, body) = encode_chat_request(&req, &anthropic_provider(), Some("sk")).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let msgs = json["messages"].as_array().unwrap();
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[1]["content"][0]["tool_use_id"], "call_1");
    }

    // ---------- decoder ----------

    fn decode_all(input: &str) -> Vec<ChatChunk> {
        let mut d = AnthropicSseDecoder::new();
        d.push(input.as_bytes()).chunks
    }

    #[test]
    fn decodes_text_delta_frames() {
        let stream = "\
            event: content_block_delta\n\
            data: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\n\
            event: content_block_delta\n\
            data: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n\
        ";
        let chunks = decode_all(stream);
        let texts: Vec<_> = chunks
            .iter()
            .filter_map(|c| match &c.delta {
                ChunkDelta::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["hello ", "world"]);
    }

    #[test]
    fn decodes_tool_use_block_lifecycle() {
        let stream = "\
            event: content_block_start\n\
            data: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"lookup\",\"input\":{}}}\n\n\
            event: content_block_delta\n\
            data: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"x\\\":\"}}\n\n\
            event: content_block_delta\n\
            data: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"1}\"}}\n\n\
            event: content_block_stop\n\
            data: {\"index\":1}\n\n\
        ";
        let chunks = decode_all(stream);
        assert_eq!(
            chunks
                .iter()
                .filter(|c| matches!(&c.delta, ChunkDelta::ToolCallStart { .. }))
                .count(),
            1
        );
        assert_eq!(
            chunks
                .iter()
                .filter(|c| matches!(&c.delta, ChunkDelta::ToolCallArguments { .. }))
                .count(),
            2
        );
        assert_eq!(
            chunks
                .iter()
                .filter(|c| matches!(&c.delta, ChunkDelta::ToolCallComplete { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn decodes_message_delta_stop_and_usage() {
        let stream = "\
            event: message_delta\n\
            data: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}\n\n\
        ";
        let chunks = decode_all(stream);
        assert!(chunks
            .iter()
            .any(|c| c.finish_reason == Some(FinishReason::Stop)));
        let usage = chunks.iter().find_map(|c| c.usage.as_ref()).unwrap();
        assert_eq!(usage.output_tokens, 42);
    }

    #[test]
    fn message_stop_sets_done() {
        let stream = "\
            event: message_stop\n\
            data: {}\n\n\
        ";
        let mut d = AnthropicSseDecoder::new();
        let batch = d.push(stream.as_bytes());
        assert!(batch.done);
    }

    #[test]
    fn unknown_events_are_skipped() {
        let stream = "\
            event: ping\n\
            data: {}\n\n\
            event: content_block_delta\n\
            data: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n\
        ";
        let chunks = decode_all(stream);
        assert_eq!(chunks.len(), 1);
    }
}
