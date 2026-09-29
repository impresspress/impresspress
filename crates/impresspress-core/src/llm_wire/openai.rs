//! OpenAI chat-completions wire format — encoder and SSE decoder.
//!
//! This is pure translation between wafer-core's `ChatRequest`/`ChatChunk`
//! types and OpenAI's JSON: no HTTP, no provider configuration, no block
//! machinery. It lives at the crate root rather than under
//! `blocks::llm::providers` because it has three consumers with three
//! different feature sets — the native `ProviderLlmService` (`feature =
//! "llm"`, which owns the API key and the endpoint), the OpenAI-compatible
//! provider that borrows the same wire shape, and `impresspress-browser`'s
//! WebLLM service, which speaks OpenAI chunk JSON over a postMessage bridge
//! with no `block-*` feature enabled at all. It used to be copied into the
//! browser adapter for exactly that reason, and the copies had drifted; the
//! four divergences are settled on [`OpenAiSseDecoder`].
//!
//! See <https://platform.openai.com/docs/api-reference/chat/create> for the
//! reference request/response shapes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use wafer_core::interfaces::llm::service::{
    ChatChunk, ChatContent, ChatMessage, ChatRequest, ChatRole, ContentPart, FinishReason,
    ResponseFormat, TokenUsage, ToolCall, ToolDefinition,
};

use super::sse::{DecodeBatch, FeedLoss, SseFrameStream};

// The two spellings are mutually exclusive, and which one a server accepts is
// a property of the wire format it implements, not of the model string — so it
// is named, never inferred:
//
// * `MaxCompletionTokens` — OpenAI's own API. `max_tokens` is deprecated there
//   and is rejected outright (`unsupported_parameter`) by the reasoning
//   models, which are selectable from `/v1/models` discovery like any other.
// * `MaxTokens` — the OpenAI-*compatible* servers (Ollama, llama.cpp, vLLM,
//   LM Studio, Groq, Together, OpenRouter, …), which accept the original
//   spelling and mostly do not know the new one.
//
// A provider's protocol names it by default, and
// `ProviderConfig::max_tokens_field` overrides that for a server whose wire
// format departs from its protocol's usual spelling — an Azure OpenAI
// reasoning deployment, declared `open_ai_compatible`, requires
// `max_completion_tokens`.
//
// Never both: a body carrying the two fields is an implicit mapping between
// them, and upstreams do not reliably accept it.
//
// Derives `JsonSchema` because it is published on the provider contracts
// (`ProviderView` and the create/update requests), so the `///` below is the
// schema `description` — detail belongs in this plain comment.
/// Which field carries the output-token budget in a `/chat/completions` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    /// `max_tokens` — the spelling the OpenAI-compatible servers accept.
    MaxTokens,
    /// `max_completion_tokens` — OpenAI's current spelling; its reasoning
    /// models refuse the other one.
    MaxCompletionTokens,
}

impl MaxTokensField {
    /// Parse from the string column stored in `impresspress__llm__providers`.
    /// The tokens are the wire field names themselves, so what is stored, what
    /// serde accepts and what goes out in the body are one spelling and not
    /// three.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "max_tokens" => Some(Self::MaxTokens),
            "max_completion_tokens" => Some(Self::MaxCompletionTokens),
            _ => None,
        }
    }

    /// The JSON field name this variant writes, and the token
    /// [`parse`](Self::parse) reads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaxTokens => "max_tokens",
            Self::MaxCompletionTokens => "max_completion_tokens",
        }
    }
}

/// Serialize `req` into an OpenAI `/chat/completions` request body, spelling
/// the output-token budget as `max_tokens_field` says.
///
/// The transport half — endpoint URL, `Authorization` — belongs to whoever is
/// sending it (see `blocks::llm::providers::openai::encode_chat_request` on
/// native); a consumer that already holds a channel to a model, like the
/// browser's WebLLM bridge, needs only this.
pub fn encode_chat_body(
    req: &ChatRequest,
    max_tokens_field: MaxTokensField,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&OpenAiRequest::from_chat_request(req, max_tokens_field))
}

// ---------- Wire format types ----------

#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    /// Set only under [`MaxTokensField::MaxTokens`]; exactly one of this and
    /// `max_completion_tokens` is ever populated.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// Set only under [`MaxTokensField::MaxCompletionTokens`].
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
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
    fn from_chat_request(req: &'a ChatRequest, max_tokens_field: MaxTokensField) -> Self {
        // Exactly one spelling, chosen by the caller's wire format. Both
        // fields skip serializing when `None`, so the body carries the one
        // the server understands and no trace of the other.
        let (max_tokens, max_completion_tokens) = match max_tokens_field {
            MaxTokensField::MaxTokens => (req.params.max_tokens, None),
            MaxTokensField::MaxCompletionTokens => (None, req.params.max_tokens),
        };
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
            max_tokens,
            max_completion_tokens,
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
    /// Wire tool-call `index` → in-flight call id, for the calls this stream
    /// has started and not yet closed.
    ///
    /// OpenAI streams tool_calls as `index` + an `id` and `function.name` on
    /// the first frame only, then bare `function.arguments` fragments that
    /// carry nothing but the `index`. So the index is the correlation key and
    /// this must be keyed on it: the parallel `Vec<String>` this replaced was
    /// keyed on *arrival order*, which silently dropped every argument delta
    /// for a call whose index exceeded the number of calls started so far,
    /// and mis-attributed arguments whenever two calls started out of index
    /// order. A `BTreeMap` rather than a `HashMap` so the `ToolCallComplete`
    /// frames come out in index order instead of an arbitrary one.
    started: BTreeMap<u32, String>,
}

// Tool-call / usage chunks are built via wafer-core's explicit constructors,
// so the wire shape stays in the producer crate. Earlier versions of this
// file round-tripped through `serde_json::from_value(...).expect(...)`; that
// turned every SSE frame into a panic if wafer-core ever renamed a variant.

impl OpenAiSseDecoder {
    pub fn new() -> Self {
        Self {
            frames: SseFrameStream::new(),
            started: BTreeMap::new(),
        }
    }

    /// Feed more bytes. Returns the decoded chunks + whether the stream
    /// terminated (`[DONE]` seen). Tool-call `Complete` frames for any
    /// in-flight ids are emitted on terminal.
    pub fn push(&mut self, bytes: &[u8]) -> DecodeBatch {
        let lost = self.frames.feed(bytes);
        if lost.any() {
            // The frames that did decode are still in the buffer, so drain
            // them: they are the prefix of the answer that survived. `lost`
            // travels with the batch so the consumer can end the stream
            // instead of delivering a reply with a hole in it.
            tracing::warn!(%lost, "openai sse: transport lost part of the stream");
        }

        let mut out = Vec::new();
        let mut done = false;

        while let Some(frame) = self.frames.next_frame() {
            let batch = self.push_frame(&frame.data);
            out.extend(batch.chunks);
            if batch.done {
                done = true;
                break;
            }
        }

        DecodeBatch {
            chunks: out,
            done,
            lost,
        }
    }

    /// Bytes received that never became a frame — see
    /// [`SseFrameStream::has_unparsed_input`]. A transport that ends while
    /// this is true was cut mid-frame.
    pub fn has_unparsed_input(&self) -> bool {
        self.frames.has_unparsed_input()
    }

    /// Decode one already-de-framed `data:` payload — the body of a single SSE
    /// frame, or the `[DONE]` sentinel.
    ///
    /// [`push`](Self::push) is SSE framing plus this; a consumer that receives
    /// OpenAI chunk JSON pre-framed by some other transport (the browser's
    /// WebLLM postMessage bridge hands one chunk object per message) calls this
    /// directly rather than re-wrapping its payloads in `data: …\n\n` just to
    /// have them taken apart again. Both entry points are the same decoder and
    /// the same state, which is the point: a second entry point is not a second
    /// codec.
    pub fn push_frame(&mut self, data: &str) -> DecodeBatch {
        // OpenAI uses only the `data:` field; `event:` is ignored. An empty
        // payload is a comment/keepalive frame.
        if data.is_empty() {
            return DecodeBatch::default();
        }
        if data == "[DONE]" {
            return DecodeBatch {
                chunks: self.close_open_tool_calls(),
                done: true,
                lost: FeedLoss::default(),
            };
        }
        match serde_json::from_str::<OpenAiStreamFrame>(data) {
            Ok(parsed) => DecodeBatch {
                chunks: self.translate(parsed),
                done: false,
                lost: FeedLoss::default(),
            },
            Err(e) => {
                tracing::warn!(error = %e, payload = %data, "openai sse: decode failed");
                DecodeBatch::default()
            }
        }
    }

    /// Terminate the stream out-of-band: emit a `ToolCallComplete` for every
    /// tool call still open and forget them.
    ///
    /// `[DONE]` is how OpenAI's own transport says this, and
    /// [`push_frame`](Self::push_frame) routes the sentinel here. A transport
    /// that carries termination outside the frame stream — the browser's WebLLM
    /// bridge sends a separate `done` message — calls this instead of
    /// synthesizing a sentinel. Idempotent, so a terminal that arrives after a
    /// `finish_reason` already closed the calls emits nothing.
    pub fn finish(&mut self) -> Vec<ChatChunk> {
        self.close_open_tool_calls()
    }

    /// Emit a `ToolCallComplete` for every tool call still open, in wire-index
    /// order, and forget them.
    fn close_open_tool_calls(&mut self) -> Vec<ChatChunk> {
        std::mem::take(&mut self.started)
            .into_values()
            .map(ChatChunk::tool_call_complete)
            .collect()
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
                // First sighting of this index with id + name ⇒ ToolCallStart.
                //
                // This block does NOT `continue`: the start and the arguments
                // are two independent readings of the same delta, because a
                // provider may put both in one frame. OpenAI streams them
                // apart (`{"id","function":{"name","arguments":""}}` then a run
                // of argument fragments), but a provider that derives the whole
                // call from a completed generation — WebLLM, which is what the
                // browser adapter feeds this decoder — emits the id, the name
                // and the complete `arguments` in a single delta. Skipping the
                // arguments block for that frame ran the tool with no
                // arguments at all.
                if let (Some(id), Some(name)) = (
                    tc.id.clone(),
                    tc.function.as_ref().and_then(|f| f.name.clone()),
                ) {
                    if let std::collections::btree_map::Entry::Vacant(slot) =
                        self.started.entry(tc.index)
                    {
                        slot.insert(id.clone());
                        out.push(ChatChunk::tool_call_start(id, name));
                    }
                }
                // Argument deltas. OpenAI omits the id after the first frame
                // but always supplies the index, so the index is what resolves
                // the id; `tc.id` is the fallback for a provider that repeats
                // it and never sent a start frame.
                //
                // Empty fragments are not forwarded. OpenAI's own start frame
                // carries `"arguments":""`, and now that the start block falls
                // through, every one of them would otherwise become an empty
                // `ToolCallArguments` chunk that a consumer accumulating
                // fragments has to filter itself.
                if let Some(f) = tc.function {
                    if let Some(args) = f.arguments.filter(|a| !a.is_empty()) {
                        match self.started.get(&tc.index).cloned().or(tc.id) {
                            Some(id) => out.push(ChatChunk::tool_call_arguments(id, args)),
                            // No start for this index and no id on the frame:
                            // the fragment belongs to no call this decoder can
                            // name, so there is nothing to emit. Dropped with a
                            // warning rather than failing the turn — the same
                            // policy `push_frame` applies to a malformed frame,
                            // and for the same reason: one unusable fragment
                            // from a non-conformant provider must not destroy a
                            // generation the rest of which decoded. (The
                            // deleted browser codec failed the whole stream
                            // here.)
                            None => tracing::warn!(
                                index = tc.index,
                                "openai sse: tool-call arguments before start and \
                                 with no id; fragment dropped"
                            ),
                        }
                    }
                }
            }
            if let Some(reason) = choice.finish_reason {
                // A `finish_reason` ends the turn, so it closes any tool call
                // still open — the same terminal `[DONE]` performs, whichever
                // arrives first (`close_open_tool_calls` is idempotent, so the
                // second one closes nothing). Providers that end the stream by
                // hanging up after the finish frame, with no `[DONE]`, used to
                // leave every tool call open forever. The completes precede the
                // finish frame so a consumer reading in order sees the calls
                // closed before the turn ends.
                //
                // `done` stays false here: OpenAI's usage frame
                // (`stream_options.include_usage`) arrives *after* the finish
                // frame, and this encoder always asks for it, so the caller
                // must keep reading until `[DONE]` or the transport ends.
                out.extend(self.close_open_tool_calls());
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
    use wafer_core::interfaces::llm::service::ChunkDelta;

    use super::*;

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

    /// Concatenate every text delta the decoder produced.
    fn text_of(chunks: &[ChatChunk]) -> String {
        chunks
            .iter()
            .filter_map(|c| match &c.delta {
                ChunkDelta::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Non-ASCII content streamed through the real decoder must survive the
    /// network chunk boundary landing anywhere — including inside a character.
    ///
    /// A split code point makes both halves invalid on their own (incomplete
    /// lead sequence, then bare continuation bytes), which cost two whole
    /// transport chunks — several frames — and left a partial frame that
    /// corrupted the JSON of the next one. Every byte offset is exercised
    /// because reqwest chooses the boundary, not the provider.
    #[test]
    fn non_ascii_deltas_survive_a_split_at_every_byte_offset() {
        let stream = "\
            data: {\"choices\":[{\"delta\":{\"content\":\"héllo \"}}]}\n\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"🙂 日本語\"}}]}\n\n\
            data: {\"choices\":[{\"delta\":{\"content\":\" wörld\"}}]}\n\n\
            data: [DONE]\n\n\
        ";
        let bytes = stream.as_bytes();
        for split in 0..=bytes.len() {
            let mut decoder = OpenAiSseDecoder::new();
            let mut chunks = decoder.push(&bytes[..split]).chunks;
            chunks.extend(decoder.push(&bytes[split..]).chunks);
            assert_eq!(
                text_of(&chunks),
                "héllo 🙂 日本語 wörld",
                "content lost when the transport split at byte {split}"
            );
        }
    }

    /// The same stream delivered one byte at a time — the pathological case a
    /// slow link produces — still decodes to the same text and terminates.
    #[test]
    fn non_ascii_deltas_survive_a_byte_at_a_time_stream() {
        let stream = "\
            data: {\"choices\":[{\"delta\":{\"content\":\"🙂\"}}]}\n\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"é\"}}]}\n\n\
            data: [DONE]\n\n\
        ";
        let mut decoder = OpenAiSseDecoder::new();
        let mut chunks = Vec::new();
        let mut done = false;
        for b in stream.as_bytes() {
            let batch = decoder.push(&[*b]);
            chunks.extend(batch.chunks);
            done |= batch.done;
        }
        assert_eq!(text_of(&chunks), "🙂é");
        assert!(done, "[DONE] must still terminate the stream");
    }

    /// A loss travels on the batch, not just into a log line.
    ///
    /// The frames that survived are still decoded — they are the prefix of
    /// the answer — but `lost` is what lets the consumer end the stream
    /// instead of joining the text after the gap onto the text before it.
    #[test]
    fn an_oversized_frame_is_reported_on_the_batch_it_breaks() {
        let mut decoder = OpenAiSseDecoder::new();
        let first = decoder
            .push("data: {\"choices\":[{\"delta\":{\"content\":\"before\"}}]}\n\n".as_bytes());
        assert_eq!(text_of(&first.chunks), "before");
        assert!(!first.lost.any(), "a complete frame is not a loss");

        // An unterminated frame arriving in pieces, past the cap.
        let mut reported = 0;
        for _ in 0..20 {
            let batch = decoder.push(&vec![b'z'; 64 * 1024]);
            if batch.lost.frame_too_large {
                reported += 1;
            }
        }
        assert_eq!(
            reported, 1,
            "the loss is reported once, on the batch that lost it"
        );
    }

    /// A provider that frames with CRLF — which SSE permits — must decode,
    /// not accumulate bytes forever while emitting nothing.
    #[test]
    fn crlf_framed_stream_decodes() {
        let stream = "\
            data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\r\n\r\n\
            data: [DONE]\r\n\r\n\
        ";
        let mut decoder = OpenAiSseDecoder::new();
        let batch = decoder.push(stream.as_bytes());
        assert_eq!(text_of(&batch.chunks), "hi");
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

/// The four points on which the two OpenAI codecs in this repository had
/// drifted before they became one. Each test names the copy it fails against.
///
/// The copies were `impresspress-core`'s `OpenAiSseDecoder` (this one) and
/// `impresspress-browser`'s `llm::openai_codec::StreamingDecoder` (deleted).
/// The browser fed it one already-de-framed chunk JSON at a time off the
/// WebLLM postMessage bridge; both are exercised here through
/// [`OpenAiSseDecoder::push_frame`], the frame-level entry point the SSE loop
/// itself uses.
#[cfg(test)]
mod divergences {
    use wafer_core::interfaces::llm::service::{ChatChunk, ChunkDelta, FinishReason};

    use super::OpenAiSseDecoder;

    fn frames(payloads: &[&str]) -> Vec<ChatChunk> {
        let mut decoder = OpenAiSseDecoder::new();
        let mut out = Vec::new();
        for payload in payloads {
            out.extend(decoder.push_frame(payload).chunks);
        }
        out
    }

    fn argument_deltas(chunks: &[ChatChunk]) -> Vec<(&str, &str)> {
        chunks
            .iter()
            .filter_map(|c| match &c.delta {
                ChunkDelta::ToolCallArguments {
                    id,
                    arguments_delta,
                } => Some((id.as_str(), arguments_delta.as_str())),
                _ => None,
            })
            .collect()
    }

    fn completed_ids(chunks: &[ChatChunk]) -> Vec<&str> {
        chunks
            .iter()
            .filter_map(|c| match &c.delta {
                ChunkDelta::ToolCallComplete { id } => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }

    fn started_calls(chunks: &[ChatChunk]) -> Vec<(&str, &str)> {
        chunks
            .iter()
            .filter_map(|c| match &c.delta {
                ChunkDelta::ToolCallStart { id, name } => Some((id.as_str(), name.as_str())),
                _ => None,
            })
            .collect()
    }

    // ── Divergence 1: tool calls are keyed on the wire `index` ───────────────

    /// **Fails against `impresspress-core`'s copy.** A single tool call whose
    /// wire `index` is not 0. The old `started: Vec<String>` was a positional
    /// array — `started.get(1)` on a one-element vec is `None`, and OpenAI
    /// omits `id` on every frame after the first, so the argument delta was
    /// silently dropped and the model's tool call arrived with no arguments.
    #[test]
    fn arguments_follow_the_wire_index_not_the_arrival_position() {
        let chunks = frames(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_1","function":{"name":"get_weather"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"city\":\"Paris\"}"}}]}}]}"#,
        ]);

        assert_eq!(
            argument_deltas(&chunks),
            vec![("call_1", "{\"city\":\"Paris\"}")],
            "argument delta for index 1 must reach index 1's id: {chunks:?}"
        );
    }

    /// **Fails against `impresspress-core`'s copy.** Two tool calls whose
    /// first sightings arrive out of index order. A positional array maps
    /// index 0's arguments onto the id it happened to see first, i.e. onto the
    /// *other* tool call — the model then executes one call with another's
    /// arguments.
    #[test]
    fn out_of_order_tool_call_starts_keep_their_own_arguments() {
        let chunks = frames(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"b"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"a"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"A"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"B"}}]}}]}"#,
        ]);

        assert_eq!(
            argument_deltas(&chunks),
            vec![("call_a", "A"), ("call_b", "B")],
            "each index keeps its own id: {chunks:?}"
        );
    }

    // ── Divergence 2: a terminal on `[DONE]` AND on a `finish_reason` ────────

    /// **Fails against `impresspress-core`'s copy.** A provider that ends the
    /// stream with `finish_reason: "tool_calls"` and closes the connection
    /// without a `[DONE]` sentinel — every OpenAI-compatible gateway that does
    /// this left the tool call open forever, so the caller never learned the
    /// arguments were complete. The `ToolCallComplete` frames come out before
    /// the finish frame, so a consumer reading in order sees the call closed
    /// before the turn ends.
    #[test]
    fn a_finish_reason_closes_open_tool_calls_without_a_done_sentinel() {
        let chunks = frames(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","function":{"name":"f"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ]);

        assert_eq!(completed_ids(&chunks), vec!["call_x"], "{chunks:?}");
        let complete_at = chunks
            .iter()
            .position(|c| matches!(&c.delta, ChunkDelta::ToolCallComplete { .. }))
            .expect("ToolCallComplete emitted");
        let finish_at = chunks
            .iter()
            .position(|c| c.finish_reason.is_some())
            .expect("finish frame emitted");
        assert!(
            complete_at < finish_at,
            "ToolCallComplete must precede the finish frame: {chunks:?}"
        );
    }

    /// The other half of "whichever comes first": a `[DONE]` after a
    /// `finish_reason` must not close the same tool call twice. A duplicate
    /// `ToolCallComplete` would make a consumer that executes on completion
    /// run the tool twice.
    #[test]
    fn done_after_a_finish_reason_does_not_re_close_the_tool_call() {
        let chunks = frames(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","function":{"name":"f"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);

        assert_eq!(completed_ids(&chunks), vec!["call_x"], "{chunks:?}");
    }

    /// And `[DONE]` alone still closes an open call — the case the core copy
    /// was the only one to handle. **Fails against the browser copy**, which
    /// emitted `ToolCallComplete` only for a `tool_calls` finish reason and
    /// had no `[DONE]` handling at all (its bridge signalled the end
    /// out-of-band and it simply stopped).
    #[test]
    fn done_alone_closes_an_open_tool_call() {
        let chunks = frames(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","function":{"name":"f"}}]}}]}"#,
            "[DONE]",
        ]);

        assert_eq!(completed_ids(&chunks), vec!["call_x"], "{chunks:?}");
    }

    // ── Divergence 3: an unknown `finish_reason` is an Error terminal ────────

    /// **Fails against the browser copy.** Its `parse_finish_reason` returned
    /// `Option` and an unknown value produced *no terminal frame at all*, so a
    /// provider that invents a reason (`"max_output_tokens"`,
    /// `"guardrail_intervened"`, …) left the turn hanging with no finish and
    /// no error. An unrecognised reason is still the end of the turn.
    #[test]
    fn an_unknown_finish_reason_is_an_error_terminal_not_silence() {
        let chunks =
            frames(&[r#"{"choices":[{"delta":{},"finish_reason":"guardrail_intervened"}]}"#]);

        assert_eq!(
            chunks
                .iter()
                .filter_map(|c| c.finish_reason)
                .collect::<Vec<_>>(),
            vec![FinishReason::Error],
            "{chunks:?}"
        );
    }

    /// The known reasons keep their own mapping — the `Error` fallback above
    /// must not swallow them.
    #[test]
    fn known_finish_reasons_keep_their_mapping() {
        for (wire, expected) in [
            ("stop", FinishReason::Stop),
            ("length", FinishReason::Length),
            ("tool_calls", FinishReason::ToolCall),
            ("content_filter", FinishReason::ContentFilter),
        ] {
            let chunks = frames(&[&format!(
                r#"{{"choices":[{{"delta":{{}},"finish_reason":"{wire}"}}]}}"#
            )]);
            assert_eq!(
                chunks
                    .iter()
                    .filter_map(|c| c.finish_reason)
                    .collect::<Vec<_>>(),
                vec![expected],
                "{wire}"
            );
        }
    }

    // ── The frame-level entry point itself ───────────────────────────────────

    /// `push_frame` and `push` decode the same payload identically — the SSE
    /// loop is framing plus `push_frame`, not a second decoder.
    #[test]
    fn push_frame_agrees_with_the_sse_framed_push() {
        let payload = r#"{"choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]}"#;

        let framed = {
            let mut decoder = OpenAiSseDecoder::new();
            decoder
                .push(format!("data: {payload}\n\ndata: [DONE]\n\n").as_bytes())
                .chunks
        };
        let direct = {
            let mut decoder = OpenAiSseDecoder::new();
            let mut out = decoder.push_frame(payload).chunks;
            out.extend(decoder.push_frame("[DONE]").chunks);
            out
        };

        assert_eq!(framed, direct);
    }

    // ── Divergence 4: a start and its arguments in the SAME delta ────────────

    /// **Fails against the pre-fix `impresspress-core` copy.** The start block
    /// ended in `continue`, so a delta that carried the id, the name *and* the
    /// arguments produced a `ToolCallStart` and nothing else — the tool ran
    /// with empty arguments. The deleted browser codec had no `continue`: its
    /// start and arguments blocks were two independent `if`s, so a combined
    /// delta produced both chunks.
    ///
    /// This is the shape most likely on the browser path, because WebLLM
    /// derives its tool calls from the completed generation rather than
    /// streaming argument fragments, so the whole call arrives in one frame.
    #[test]
    fn a_start_and_its_arguments_in_one_delta_keep_the_arguments() {
        let chunks = frames(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ]);

        assert_eq!(
            started_calls(&chunks),
            vec![("call_1", "get_weather")],
            "{chunks:?}"
        );
        assert_eq!(
            argument_deltas(&chunks),
            vec![("call_1", "{\"city\":\"Paris\"}")],
            "arguments on the start delta must not be dropped: {chunks:?}"
        );
        assert_eq!(completed_ids(&chunks), vec!["call_1"], "{chunks:?}");
    }

    /// The reason the start block cannot simply fall through unguarded:
    /// OpenAI's own first tool-call frame carries `"arguments":""`, and an
    /// empty argument delta is noise on the wire that a consumer accumulating
    /// fragments would have to filter itself.
    #[test]
    fn an_empty_arguments_string_on_the_start_delta_emits_no_argument_chunk() {
        let chunks = frames(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"f","arguments":""}}]}}]}"#,
        ]);

        assert_eq!(started_calls(&chunks), vec![("call_1", "f")], "{chunks:?}");
        assert!(
            argument_deltas(&chunks).is_empty(),
            "an empty `arguments` on the start frame is not an argument delta: {chunks:?}"
        );
    }

    /// The same guard on a *continuation* frame: a provider that pads the
    /// stream with empty argument fragments must not turn each one into a
    /// chunk.
    #[test]
    fn an_empty_arguments_fragment_on_a_later_delta_emits_no_argument_chunk() {
        let chunks = frames(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"f"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}"#,
        ]);

        assert_eq!(
            argument_deltas(&chunks),
            vec![("call_1", "{}")],
            "{chunks:?}"
        );
    }

    // ── The unattributable fragment (decided, not accidental) ────────────────

    /// An argument fragment for an index that never started and that carries no
    /// `id` of its own cannot be attributed to any call, so there is nothing to
    /// emit and it is dropped with a `warn!`. The deleted browser codec failed
    /// the whole turn here; the decoder does not, for the same reason a
    /// malformed frame does not — one unusable fragment from a non-conformant
    /// provider must not destroy a generation the rest of which decoded.
    #[test]
    fn an_unattributable_argument_fragment_is_dropped_not_forwarded() {
        let chunks = frames(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"content":"the rest of the turn survives"}}]}"#,
        ]);

        assert!(argument_deltas(&chunks).is_empty(), "{chunks:?}");
        assert_eq!(
            chunks
                .iter()
                .filter(|c| matches!(&c.delta, ChunkDelta::Text(_)))
                .count(),
            1,
            "the rest of the stream must still decode: {chunks:?}"
        );
    }

    /// A fragment with no prior start but with an `id` of its own IS
    /// attributable, and is forwarded under that id rather than dropped.
    #[test]
    fn an_argument_fragment_that_repeats_its_own_id_is_attributable() {
        let chunks = frames(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_z","function":{"arguments":"{}"}}]}}]}"#,
        ]);

        assert_eq!(
            argument_deltas(&chunks),
            vec![("call_z", "{}")],
            "{chunks:?}"
        );
    }
}

/// The request-body half of the unification. `encode_chat_body` is what the
/// browser adapter's deleted `encode_request_body` becomes, so these pin the
/// shape WebLLM's `chat.completions.create` reads (`messages`, `tools`) plus
/// the one place the copies disagreed.
#[cfg(test)]
mod encode_body {
    use wafer_core::interfaces::llm::service::{
        ChatContent, ChatMessage, ChatRequest, ChatRole, ContentPart, ToolCall, ToolDefinition,
    };

    use super::{encode_chat_body, MaxTokensField};

    /// The body as the OpenAI-compatible servers (and the browser bridge)
    /// receive it. The budget's spelling is the one thing the two protocols
    /// disagree on, and it has its own tests below.
    fn body(req: &ChatRequest) -> serde_json::Value {
        body_as(req, MaxTokensField::MaxTokens)
    }

    fn body_as(req: &ChatRequest, field: MaxTokensField) -> serde_json::Value {
        serde_json::from_slice(&encode_chat_body(req, field).expect("encode")).expect("valid JSON")
    }

    #[test]
    fn encodes_every_role_with_its_content() {
        let req = ChatRequest::new(
            "webllm",
            "Llama-3-8B",
            vec![
                ChatMessage::system("Be helpful."),
                ChatMessage::user("Hello"),
                ChatMessage::assistant("Hi there!"),
                ChatMessage::tool("call_1", "The answer is 42"),
            ],
        );
        let v = body(&req);
        let msgs = v["messages"].as_array().unwrap();

        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Be helpful.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "Hello");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "Hi there!");
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["content"], "The answer is 42");
        assert_eq!(msgs[3]["tool_call_id"], "call_1");
    }

    /// Tool-call arguments go on the wire as a JSON *string*, not a nested
    /// object — the shape OpenAI (and WebLLM's OpenAI-compatible surface)
    /// parses back.
    #[test]
    fn tool_call_arguments_are_a_json_string() {
        let call = ToolCall::new(
            "call_abc",
            "get_weather",
            serde_json::json!({"city": "Paris"}),
        );
        let req = ChatRequest::new(
            "webllm",
            "Llama-3-8B",
            vec![ChatMessage::assistant("").with_tool_calls(vec![call])],
        );
        let v = body(&req);
        let tc = &v["messages"][0]["tool_calls"][0];

        assert_eq!(tc["id"], "call_abc");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "get_weather");
        let args: serde_json::Value =
            serde_json::from_str(tc["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["city"], "Paris");
    }

    /// `tools` is omitted entirely when the request declares none — a `"tools":
    /// []` key makes some OpenAI-compatible servers refuse the request.
    #[test]
    fn tools_are_omitted_when_empty_and_present_when_declared() {
        let mut req = ChatRequest::new("webllm", "Llama-3-8B", vec![ChatMessage::user("hi")]);
        assert!(body(&req).get("tools").is_none());

        req.tools = vec![ToolDefinition::new(
            "lookup",
            "Look something up",
            serde_json::json!({"type": "object", "properties": {}}),
        )];
        let v = body(&req);
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["function"]["name"], "lookup");
        assert_eq!(
            v["tools"][0]["function"]["description"],
            "Look something up"
        );
    }

    /// The two spellings are exclusive, and the caller's protocol picks.
    ///
    /// OpenAI's own API rejects `max_tokens` on its reasoning models with
    /// `unsupported_parameter`, and the OpenAI-compatible servers mostly do
    /// not know `max_completion_tokens` at all — so a body carrying both, or
    /// the wrong one, is a 400 from one side or the other.
    #[test]
    fn the_budget_is_spelled_the_way_the_protocol_asks_and_never_both_ways() {
        let mut req = ChatRequest::new("p", "m", vec![ChatMessage::user("hi")]);
        req.params.max_tokens = Some(321);

        let native = body_as(&req, MaxTokensField::MaxCompletionTokens);
        assert_eq!(native["max_completion_tokens"], 321);
        assert!(
            native.get("max_tokens").is_none(),
            "OpenAI's reasoning models refuse a body carrying `max_tokens`, got: {native}"
        );

        let compatible = body_as(&req, MaxTokensField::MaxTokens);
        assert_eq!(compatible["max_tokens"], 321);
        assert!(
            compatible.get("max_completion_tokens").is_none(),
            "a compatible server is sent the only spelling it knows, got: {compatible}"
        );
    }

    /// The stored token, the serde token and the JSON field name are one
    /// string. Storing `max_completion_tokens` and writing something else on
    /// the wire is the sort of mapping layer this repo does not have.
    #[test]
    fn the_variant_tokens_are_the_wire_field_names() {
        for field in [
            MaxTokensField::MaxTokens,
            MaxTokensField::MaxCompletionTokens,
        ] {
            assert_eq!(MaxTokensField::parse(field.as_str()), Some(field));
            assert_eq!(
                serde_json::to_value(field).unwrap(),
                serde_json::Value::String(field.as_str().to_string())
            );
            let mut req = ChatRequest::new("p", "m", vec![ChatMessage::user("hi")]);
            req.params.max_tokens = Some(7);
            assert_eq!(
                body_as(&req, field)[field.as_str()],
                7,
                "the variant must write the field it is named after"
            );
        }
    }

    #[test]
    fn max_tokens_field_parse_rejects_anything_else() {
        assert_eq!(MaxTokensField::parse("maxTokens"), None);
        assert_eq!(MaxTokensField::parse("MaxTokens"), None);
        assert_eq!(MaxTokensField::parse(""), None);
    }

    /// No budget, no field — under either spelling. The request the browser
    /// bridge builds for a model with no cap must not grow a `null`.
    #[test]
    fn neither_spelling_appears_when_the_request_names_no_budget() {
        let req = ChatRequest::new("p", "m", vec![ChatMessage::user("hi")]);
        for field in [
            MaxTokensField::MaxTokens,
            MaxTokensField::MaxCompletionTokens,
        ] {
            let v = body_as(&req, field);
            assert!(v.get("max_tokens").is_none(), "{field:?}: {v}");
            assert!(v.get("max_completion_tokens").is_none(), "{field:?}: {v}");
        }
    }

    /// **Behaviour change for the browser.** Its deleted copy answered
    /// `BackendError("webllm: multimodal content not supported")` for any
    /// `ChatContent::Parts` message and never put one on the wire. There is
    /// nothing browser-specific about a content-part array — it is the same
    /// OpenAI shape the native providers already send, and WebLLM's
    /// vision-capable models accept it — so the shared encoder encodes it and
    /// lets the engine decide. A model that cannot take an image now says so
    /// itself instead of the adapter refusing on its behalf.
    #[test]
    fn multimodal_parts_are_encoded_not_refused() {
        let req = ChatRequest::new(
            "webllm",
            "Llama-3-8B-Vision",
            vec![ChatMessage::new(
                ChatRole::User,
                ChatContent::Parts(vec![
                    ContentPart::Text("what is this?".into()),
                    ContentPart::ImageUrl {
                        url: "https://example.com/cat.png".into(),
                        detail: None,
                    },
                ]),
            )],
        );

        let v = body(&req);
        let parts = v["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "https://example.com/cat.png");
    }
}
