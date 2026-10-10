//! Rust-side wrapper over the SW↔page LLM postMessage bridge.
//!
//! Glue around the `llm*` functions in `bridge.js`. Turns the async
//! postMessage exchanges into typed Rust calls. Not testable in native —
//! exercised via the `BrowserLlmService` integration and the browser smoke
//! test.

use wafer_core::interfaces::llm::service::LlmError;

use crate::bridge::{llm_cancel_stream, llm_chat_stream, llm_next_stream_frame, llm_unload_engine};

/// A refusal (no page can run the chat now) is [`LlmError::EngineUnavailable`]
/// with bridge.js's caller-facing message; anything else is a backend fault.
fn js_err(e: wasm_bindgen::JsValue) -> LlmError {
    match crate::bridge::engine_unavailable(&e) {
        Some(message) => LlmError::EngineUnavailable(message),
        None => LlmError::BackendError(format!("webllm bridge: {}", crate::bridge::describe(&e))),
    }
}

pub async fn unload_engine(model_id: &str) -> Result<(), LlmError> {
    llm_unload_engine(model_id)
        .await
        .map(|_| ())
        .map_err(js_err)
}

pub async fn start_chat_stream(model_id: &str, body_json: &str) -> Result<String, LlmError> {
    let v = llm_chat_stream(model_id, body_json).await.map_err(js_err)?;
    v.as_string()
        .ok_or_else(|| LlmError::BackendError("webllm bridge: stream id not a string".into()))
}

/// One frame pulled from the page-side chat stream. Chat emits `Chunk`
/// (OpenAI chunk JSON) frames and terminates with `Done` or `Error`.
pub enum StreamFrame {
    /// OpenAI chunk JSON string. Pass to
    /// `impresspress_core::llm_wire::openai::OpenAiSseDecoder::push_frame`.
    Chunk(String),
    Done,
    Error(String),
}

pub async fn next_chunk(stream_id: &str) -> Result<StreamFrame, LlmError> {
    let v = llm_next_stream_frame(stream_id).await.map_err(js_err)?;
    let s = v
        .as_string()
        .ok_or_else(|| LlmError::BackendError("webllm bridge: frame not a string".into()))?;
    let frame: serde_json::Value = serde_json::from_str(&s)
        .map_err(|e| LlmError::BackendError(format!("webllm bridge: frame parse: {e}")))?;
    let kind = frame.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let payload = || {
        frame
            .get("payload")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    match kind {
        "chunk" => Ok(StreamFrame::Chunk(payload())),
        "done" => Ok(StreamFrame::Done),
        // The page running the chat went away mid-stream: bridge.js ends the
        // stream with the refusal's code and caller-facing message.
        "error"
            if frame.get("code").and_then(|v| v.as_str())
                == Some(crate::bridge::ENGINE_UNAVAILABLE) =>
        {
            Err(LlmError::EngineUnavailable(payload()))
        }
        "error" => Ok(StreamFrame::Error(if payload().is_empty() {
            "unknown".to_string()
        } else {
            payload()
        })),
        other => Err(LlmError::BackendError(format!(
            "webllm bridge: unknown frame kind '{other}'"
        ))),
    }
}

pub async fn cancel_stream(stream_id: &str) -> Result<(), LlmError> {
    llm_cancel_stream(stream_id)
        .await
        .map(|_| ())
        .map_err(js_err)
}
