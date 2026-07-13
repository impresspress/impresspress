//! Pure-Rust OpenAI chat-completion codec.
//!
//! Handles encoding `ChatMessage` + `ToolDefinition` slices into OpenAI
//! request JSON, and streaming-decoding of OpenAI SSE chunk JSON into
//! `ChatChunk`s.

use std::collections::HashMap;

use wafer_core::interfaces::llm::service::{
    ChatChunk, ChatContent, ChatMessage, ChatRole, ChunkDelta, FinishReason, LlmError, TokenUsage,
    ToolDefinition,
};

// ---------- Encoding ----------

/// Encode messages + tools into an OpenAI chat-completion JSON body string.
///
/// The `"model"` and `"stream"` fields are intentionally absent — callers
/// (e.g. `BrowserLlmService`) inject those before sending.
pub fn encode_request_body(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
) -> Result<String, LlmError> {
    let mut obj = serde_json::Map::new();

    // Messages
    let mut msgs = Vec::with_capacity(messages.len());
    for msg in messages {
        let role_str = match msg.role {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        };

        let content_val = match &msg.content {
            ChatContent::Text(s) => serde_json::Value::String(s.clone()),
            ChatContent::Parts(_) => {
                return Err(LlmError::BackendError(
                    "webllm: multimodal content not supported".into(),
                ))
            }
        };

        let mut m = serde_json::Map::new();
        m.insert("role".into(), serde_json::Value::String(role_str.into()));
        m.insert("content".into(), content_val);

        if let Some(id) = &msg.tool_call_id {
            m.insert("tool_call_id".into(), serde_json::Value::String(id.clone()));
        }

        if !msg.tool_calls.is_empty() {
            // Propagate JSON encoding failures: silently rewriting tool-call
            // arguments to `{}` corrupts the model's view of the call and
            // breaks tool execution at the next turn.
            let tc_arr: Vec<serde_json::Value> = msg
                .tool_calls
                .iter()
                .map(|tc| {
                    let args_str = serde_json::to_string(&tc.arguments).map_err(|e| {
                        LlmError::BackendError(format!(
                            "encode tool-call '{}' arguments: {e}",
                            tc.name
                        ))
                    })?;
                    Ok::<_, LlmError>(serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": args_str
                        }
                    }))
                })
                .collect::<Result<_, _>>()?;
            m.insert("tool_calls".into(), serde_json::Value::Array(tc_arr));
        }

        msgs.push(serde_json::Value::Object(m));
    }
    obj.insert("messages".into(), serde_json::Value::Array(msgs));

    // Tools (only when non-empty)
    if !tools.is_empty() {
        let tool_arr: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    }
                })
            })
            .collect();
        obj.insert("tools".into(), serde_json::Value::Array(tool_arr));
    }

    serde_json::to_string(&serde_json::Value::Object(obj))
        .map_err(|e| LlmError::BackendError(format!("request encode: {e}")))
}

// ---------- Streaming decoder ----------

struct OpenToolCall {
    id: String,
}

/// Streaming decoder — stateful across calls to `feed`.
///
/// Tracks open tool-call indices so that argument fragments can be associated
/// with the correct call id, and `ToolCallComplete` frames can be emitted on
/// termination.
pub struct StreamingDecoder {
    open_tool_calls: HashMap<u64, OpenToolCall>, // index → { id }
}

impl Default for StreamingDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingDecoder {
    pub fn new() -> Self {
        Self {
            open_tool_calls: HashMap::new(),
        }
    }

    /// Feed a single OpenAI chunk JSON string. Emits zero or more `ChatChunk`s.
    pub fn feed(&mut self, chunk_json: &str) -> Result<Vec<ChatChunk>, LlmError> {
        let v: serde_json::Value = serde_json::from_str(chunk_json)
            .map_err(|e| LlmError::BackendError(format!("chunk parse: {e}")))?;

        let mut out: Vec<ChatChunk> = Vec::new();

        // Extract choices[0]
        let choice = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first());

        if let Some(choice) = choice {
            let delta = choice.get("delta");

            // Text content
            if let Some(content) = delta
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                if !content.is_empty() {
                    out.push(ChatChunk::text(content));
                }
            }

            // Tool-call deltas
            if let Some(tc_arr) = delta
                .and_then(|d| d.get("tool_calls"))
                .and_then(|t| t.as_array())
            {
                for entry in tc_arr {
                    let index = entry.get("index").and_then(|i| i.as_u64()).unwrap_or(0);

                    let entry_id = entry.get("id").and_then(|v| v.as_str());
                    let func_name = entry
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str());
                    let func_args = entry
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str());

                    // ToolCallStart — first time we see this index with id + name
                    if let (Some(id), Some(name)) = (entry_id, func_name) {
                        if let std::collections::hash_map::Entry::Vacant(e) =
                            self.open_tool_calls.entry(index)
                        {
                            e.insert(OpenToolCall { id: id.to_string() });
                            out.push(ChatChunk::delta(ChunkDelta::ToolCallStart {
                                id: id.to_string(),
                                name: name.to_string(),
                            }));
                        }
                    }

                    // ToolCallArguments — when arguments fragment is present
                    if let Some(args) = func_args {
                        let id = self
                            .open_tool_calls
                            .get(&index)
                            .map(|tc| tc.id.clone())
                            .ok_or_else(|| {
                                LlmError::BackendError("tool-call arguments before start".into())
                            })?;
                        out.push(ChatChunk::delta(ChunkDelta::ToolCallArguments {
                            id,
                            arguments_delta: args.to_string(),
                        }));
                    }
                }
            }

            // Finish reason
            let finish_reason_str = choice.get("finish_reason").and_then(|f| f.as_str());

            // Usage (may appear top-level or mid-stream)
            let usage = v.get("usage").and_then(parse_usage);

            if let Some(reason_str) = finish_reason_str {
                if let Some(reason) = parse_finish_reason(reason_str) {
                    // Emit ToolCallComplete for every open call on tool_calls finish
                    if reason == FinishReason::ToolCall {
                        // Collect to avoid borrow issues
                        let ids: Vec<String> = self
                            .open_tool_calls
                            .values()
                            .map(|tc| tc.id.clone())
                            .collect();
                        self.open_tool_calls.clear();
                        for id in ids {
                            out.push(ChatChunk::delta(ChunkDelta::ToolCallComplete { id }));
                        }
                    }
                    out.push(ChatChunk::finish(reason, usage));
                }
                // Unknown finish reason → no terminal frame
            } else if let Some(u) = usage {
                // Mid-stream usage without finish_reason → implicit terminal
                out.push(ChatChunk::finish(FinishReason::Stop, Some(u)));
            }
        }

        Ok(out)
    }
}

// ---------- Helpers ----------

/// Parse an OpenAI `finish_reason` string into a `FinishReason`.
/// Returns `None` for unknown values.
pub fn parse_finish_reason(s: &str) -> Option<FinishReason> {
    match s {
        "stop" => Some(FinishReason::Stop),
        "length" => Some(FinishReason::Length),
        "tool_calls" => Some(FinishReason::ToolCall),
        "content_filter" => Some(FinishReason::ContentFilter),
        _ => None,
    }
}

/// Parse an OpenAI `usage` JSON object into a `TokenUsage`.
pub fn parse_usage(v: &serde_json::Value) -> Option<TokenUsage> {
    let prompt = v.get("prompt_tokens").and_then(|t| t.as_u64())?;
    let completion = v.get("completion_tokens").and_then(|t| t.as_u64())?;
    Some(TokenUsage {
        input_tokens: prompt as u32,
        output_tokens: completion as u32,
        ..Default::default()
    })
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::llm::service::{
        ChatContent, ChatMessage, ChatRole, ToolCall, ToolDefinition,
    };

    use super::*;

    // ---- encode_request_body ----

    #[test]
    fn encodes_plain_messages() {
        let msgs = vec![
            ChatMessage::system("Be helpful."),
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there!"),
            ChatMessage::tool("call_1", "The answer is 42"),
        ];
        let body = encode_request_body(&msgs, &[]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let arr = v["messages"].as_array().unwrap();

        assert_eq!(arr[0]["role"], "system");
        assert_eq!(arr[0]["content"], "Be helpful.");

        assert_eq!(arr[1]["role"], "user");
        assert_eq!(arr[1]["content"], "Hello");

        assert_eq!(arr[2]["role"], "assistant");
        assert_eq!(arr[2]["content"], "Hi there!");

        assert_eq!(arr[3]["role"], "tool");
        assert_eq!(arr[3]["content"], "The answer is 42");
        assert_eq!(arr[3]["tool_call_id"], "call_1");
    }

    #[test]
    fn encode_rejects_multimodal() {
        let msg = ChatMessage::new(ChatRole::User, ChatContent::Parts(vec![]));
        let err = encode_request_body(&[msg], &[]).unwrap_err();
        assert!(matches!(err, LlmError::BackendError(ref s) if s.contains("multimodal")));
    }

    #[test]
    fn encode_forwards_tool_calls() {
        let tc = ToolCall::new(
            "call_abc",
            "get_weather",
            serde_json::json!({"city": "Paris"}),
        );
        let msg = ChatMessage::assistant("").with_tool_calls(vec![tc]);
        let body = encode_request_body(&[msg], &[]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let tc = &v["messages"][0]["tool_calls"][0];
        assert_eq!(tc["id"], "call_abc");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "get_weather");
        // arguments must be a JSON string
        let args_str = tc["function"]["arguments"].as_str().unwrap();
        let args: serde_json::Value = serde_json::from_str(args_str).unwrap();
        assert_eq!(args["city"], "Paris");
    }

    #[test]
    fn encode_forwards_tool_definitions() {
        let tools = vec![ToolDefinition::new(
            "lookup",
            "Look something up",
            serde_json::json!({"type": "object", "properties": {}}),
        )];
        let body = encode_request_body(&[], &tools).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let t = &v["tools"][0];
        assert_eq!(t["type"], "function");
        assert_eq!(t["function"]["name"], "lookup");
        assert_eq!(t["function"]["description"], "Look something up");
    }

    #[test]
    fn encode_omits_tools_when_empty() {
        let body = encode_request_body(&[ChatMessage::user("hi")], &[]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            v.get("tools").is_none(),
            "tools key should be absent when empty"
        );
    }

    // ---- StreamingDecoder ----

    #[test]
    fn decoder_text_delta() {
        let mut dec = StreamingDecoder::new();
        let chunks = dec
            .feed(r#"{"choices":[{"delta":{"content":"hi"},"finish_reason":null}]}"#)
            .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].delta, ChunkDelta::Text("hi".into()));
        assert!(chunks[0].finish_reason.is_none());
    }

    #[test]
    fn decoder_empty_content_is_noop() {
        let mut dec = StreamingDecoder::new();
        let chunks = dec
            .feed(
                r#"{"choices":[{"delta":{"content":"","role":"assistant"},"finish_reason":null}]}"#,
            )
            .unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn decoder_finish_stop_terminal() {
        let mut dec = StreamingDecoder::new();
        let chunks = dec
            .feed(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
            .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].finish_reason, Some(FinishReason::Stop));
        assert!(chunks[0].usage.is_none());
    }

    #[test]
    fn decoder_finish_with_usage() {
        let mut dec = StreamingDecoder::new();
        let chunks = dec
            .feed(r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#)
            .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].finish_reason, Some(FinishReason::Stop));
        let u = chunks[0].usage.unwrap();
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 20);
    }

    #[test]
    fn decoder_malformed_json_errors() {
        let mut dec = StreamingDecoder::new();
        let err = dec.feed("not json {{{").unwrap_err();
        assert!(matches!(err, LlmError::BackendError(ref s) if s.starts_with("chunk parse:")));
    }

    #[test]
    fn decoder_tool_call_start_and_args() {
        let mut dec = StreamingDecoder::new();

        // First chunk: id + name (ToolCallStart)
        let chunks1 = dec
            .feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather"}}]},"finish_reason":null}]}"#)
            .unwrap();
        assert_eq!(chunks1.len(), 1);
        assert_eq!(
            chunks1[0].delta,
            ChunkDelta::ToolCallStart {
                id: "call_1".into(),
                name: "get_weather".into()
            }
        );

        // Second chunk: first argument fragment
        let chunks2 = dec
            .feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":"}}]},"finish_reason":null}]}"#)
            .unwrap();
        assert_eq!(chunks2.len(), 1);
        assert_eq!(
            chunks2[0].delta,
            ChunkDelta::ToolCallArguments {
                id: "call_1".into(),
                arguments_delta: r#"{"q":"#.into()
            }
        );

        // Third chunk: second argument fragment
        let chunks3 = dec
            .feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]},"finish_reason":null}]}"#)
            .unwrap();
        assert_eq!(chunks3.len(), 1);
        assert_eq!(
            chunks3[0].delta,
            ChunkDelta::ToolCallArguments {
                id: "call_1".into(),
                arguments_delta: "\"Paris\"}".into()
            }
        );
    }

    #[test]
    fn decoder_tool_call_complete_on_finish() {
        let mut dec = StreamingDecoder::new();

        // Start a tool call
        dec.feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","function":{"name":"fn_name"}}]},"finish_reason":null}]}"#)
            .unwrap();

        // Finish with tool_calls reason
        let chunks = dec
            .feed(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#)
            .unwrap();

        // Should emit ToolCallComplete for "call_x" then a Finish frame
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            chunks[0].delta,
            ChunkDelta::ToolCallComplete {
                id: "call_x".into()
            }
        );
        assert_eq!(chunks[1].finish_reason, Some(FinishReason::ToolCall));
        assert_eq!(chunks[1].delta, ChunkDelta::Empty);
    }

    #[test]
    fn finish_reason_maps_standard_values() {
        assert_eq!(parse_finish_reason("stop"), Some(FinishReason::Stop));
        assert_eq!(parse_finish_reason("length"), Some(FinishReason::Length));
        assert_eq!(
            parse_finish_reason("tool_calls"),
            Some(FinishReason::ToolCall)
        );
        assert_eq!(
            parse_finish_reason("content_filter"),
            Some(FinishReason::ContentFilter)
        );
        assert_eq!(parse_finish_reason("nope"), None);
    }
}
