//! Server-Sent Events framing shared by the two SSE-producing endpoints:
//! chat streaming ([`sse_chat_response`]) and model loading
//! ([`sse_json_response`]). Both emit the same content-type meta event and
//! the same terminal frames ([`SSE_DONE_FRAME`] / [`SSE_ERROR_FRAME`]) via
//! the shared [`sse_json_frame`] encoder, so the wire format can't drift
//! between them.

use std::sync::Arc;

use futures::StreamExt;
use wafer_core::clients::{
    llm::{ChatChunk, ChunkDelta},
    NativeTypedFrameStream,
};
use wafer_run::{
    context::Context, Message, MetaEntry, OutputSink, OutputStream, META_RESP_CONTENT_TYPE,
};

use super::chat::MAX_BUFFERED_RESPONSE_BYTES;
use crate::blocks::llm::messages_create;

/// Terminal SSE frame for natural end-of-stream, letting clients distinguish
/// it from a transport-level disconnect.
const SSE_DONE_FRAME: &[u8] = b"data: [DONE]\n\n";

/// Terminal SSE frame emitted when the service stream yields an error or an
/// item fails to JSON-encode, so the consumer sees a clean SSE event instead
/// of an abrupt disconnect.
const SSE_ERROR_FRAME: &[u8] = b"event: error\ndata: {}\n\n";

/// Encode one typed item as an SSE `data: <json>\n\n` frame.
///
/// Returns `None` when JSON encoding fails; callers emit [`SSE_ERROR_FRAME`]
/// and terminate. Shared by [`sse_json_response`] and [`sse_chat_response`]
/// so the SSE wire format cannot drift between the generic and the
/// chat-finalizing paths.
fn sse_json_frame<T: serde::Serialize>(item: &T) -> Option<Vec<u8>> {
    let json = serde_json::to_vec(item).ok()?;
    let mut frame = Vec::with_capacity(json.len() + 8);
    frame.extend_from_slice(b"data: ");
    frame.extend_from_slice(&json);
    frame.extend_from_slice(b"\n\n");
    Some(frame)
}

/// Send the `text/event-stream` content-type as a mid-stream meta event so
/// the HTTP listener writes the SSE header before the first `data:` frame.
/// A send failure only means the consumer already dropped the stream; the
/// producer's next `send_chunk` surfaces that, so it is ignored here.
async fn send_sse_content_type(sink: &OutputSink) {
    let _ = sink
        .send_meta(MetaEntry {
            key: META_RESP_CONTENT_TYPE.to_string(),
            value: "text/event-stream".to_string(),
        })
        .await;
}

/// SSE wrapper for the chat endpoint: frames each [`ChatChunk`] exactly like
/// [`sse_json_response`] while accumulating `ChunkDelta::Text` deltas, then
/// persists the assistant turn via [`messages_create`] at natural
/// end-of-stream (immediately before the terminal `data: [DONE]` frame, so a
/// client that refetches history on `[DONE]` sees the new message).
///
/// Accumulation mirrors `handle_chat`: text deltas are concatenated up to
/// [`MAX_BUFFERED_RESPONSE_BYTES`] (an overflowing delta stops accumulation
/// with a warning at end-of-stream, while frames keep flowing to the
/// client), and tool-call/empty deltas are forwarded but not accumulated. A
/// service error or encode failure terminates the stream with an error frame
/// and skips persistence — the same outcome as `handle_chat`, which returns
/// a 500 without persisting when the stream errors.
///
/// Generic over the chunk stream (rather than taking
/// [`NativeTypedFrameStream`]`<ChatChunk>` directly, whose constructor is
/// private to wafer-core) so tests can drive it with a scripted stream. The
/// `MaybeSend` bound keeps it compilable on wasm32, where
/// [`OutputStream::from_producer`] does not require `Send`.
pub(super) fn sse_chat_response<S>(
    stream: S,
    ctx: Arc<dyn Context>,
    msg: Message,
    thread_id: String,
) -> OutputStream
where
    S: futures::Stream<Item = Result<ChatChunk, wafer_run::WaferError>>
        + wafer_run::MaybeSend
        + Unpin
        + 'static,
{
    OutputStream::from_producer(move |sink, _cancel| async move {
        send_sse_content_type(&sink).await;

        let mut stream = stream;
        let mut content = String::new();
        let mut truncated = false;
        while let Some(item) = stream.next().await {
            let Ok(chunk) = item else {
                let _ = sink.send_chunk(SSE_ERROR_FRAME.to_vec()).await;
                return;
            };
            if let ChunkDelta::Text(s) = &chunk.delta {
                if content.len() + s.len() > MAX_BUFFERED_RESPONSE_BYTES {
                    // Stop accumulating (same skip-the-delta semantics as
                    // `handle_chat`) but keep forwarding frames — the client
                    // still receives the full stream.
                    truncated = true;
                } else {
                    content.push_str(s);
                }
            }
            let Some(frame) = sse_json_frame(&chunk) else {
                let _ = sink.send_chunk(SSE_ERROR_FRAME.to_vec()).await;
                return;
            };
            if sink.send_chunk(frame).await.is_err() {
                return;
            }
        }
        if truncated {
            tracing::warn!(
                cap = MAX_BUFFERED_RESPONSE_BYTES,
                "llm streamed response exceeded persistence cap — stored assistant message truncated"
            );
        }

        // Natural end-of-stream: persist the assistant turn before
        // signalling `[DONE]`, so a client that refetches history on
        // `[DONE]` already sees the new message. Persistence failure is
        // non-fatal here, exactly as in `handle_chat` (`messages_create`
        // logs and returns `None`).
        let _ = messages_create(ctx.as_ref(), &msg, &thread_id, "assistant", &content).await;

        let _ = sink.send_chunk(SSE_DONE_FRAME.to_vec()).await;
    })
}

/// Stream a typed frame stream to the client as JSON Server-Sent Events.
///
/// Emits the `text/event-stream` content-type as a mid-stream meta event so
/// the HTTP listener writes the SSE header before the first `data:` frame,
/// then re-encodes each typed item as a `data: <json>\n\n` frame. A service
/// or encode error becomes a terminal `event: error\ndata: {}\n\n` frame; a
/// natural end-of-stream becomes a terminal `data: [DONE]\n\n` frame so
/// clients can distinguish it from a transport-level disconnect.
///
/// Used by the model-load endpoint; the chat-stream endpoint uses
/// [`sse_chat_response`], which shares the same frame encoding via
/// [`sse_json_frame`] and the same terminal frames so the wire format can't
/// drift between them.
pub(super) fn sse_json_response<T>(stream: NativeTypedFrameStream<T>) -> OutputStream
where
    T: serde::Serialize + serde::de::DeserializeOwned + Unpin + Send + 'static,
{
    OutputStream::from_producer(move |sink, _cancel| async move {
        send_sse_content_type(&sink).await;

        let mut stream = stream;
        while let Some(item) = stream.next().await {
            // A mid-stream service error or a JSON-encode failure both
            // terminate the stream with a final `event: error` frame, so the
            // consumer sees a clean SSE event instead of an abrupt disconnect.
            let Some(frame) = item.ok().and_then(|v| sse_json_frame(&v)) else {
                let _ = sink.send_chunk(SSE_ERROR_FRAME.to_vec()).await;
                return;
            };
            if sink.send_chunk(frame).await.is_err() {
                return;
            }
        }
        let _ = sink.send_chunk(SSE_DONE_FRAME.to_vec()).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::llm::routes::test_support::RecordingCtx;

    #[tokio::test]
    async fn sse_chat_response_persists_assistant_turn_at_done() {
        let ctx = RecordingCtx::default();
        let msg = Message::new("create:/b/llm/api/chat/stream");
        let chunks: Vec<Result<ChatChunk, wafer_run::WaferError>> =
            vec![Ok(ChatChunk::text("Hel")), Ok(ChatChunk::text("lo"))];

        let out = sse_chat_response(
            futures::stream::iter(chunks),
            ctx.clone_arc(),
            msg,
            "thread-1".to_string(),
        );
        let buf = out.collect_buffered().await.expect("stream completes");
        let body = String::from_utf8(buf.body).expect("SSE body is utf8");

        assert!(
            body.ends_with("data: [DONE]\n\n"),
            "expected terminal [DONE] frame, got: {body}"
        );
        assert!(
            body.contains("Hel") && body.contains("lo"),
            "both text deltas must be forwarded as frames, got: {body}"
        );
        assert!(
            buf.meta
                .iter()
                .any(|m| m.key == META_RESP_CONTENT_TYPE && m.value == "text/event-stream"),
            "content-type meta must announce text/event-stream"
        );

        let calls = ctx.calls();
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one persistence call, got {}",
            calls.len()
        );
        let call = &calls[0];
        assert_eq!(call.block_name, "impresspress/messages");
        assert_eq!(
            call.msg.get_meta("req.resource"),
            "/b/messages/api/contexts/thread-1/entries"
        );
        let body_json: serde_json::Value =
            serde_json::from_slice(&call.body).expect("persistence body is JSON");
        assert_eq!(body_json["role"], "assistant");
        assert_eq!(body_json["content"], "Hello");
    }

    #[tokio::test]
    async fn sse_chat_response_skips_persistence_when_stream_errors() {
        let ctx = RecordingCtx::default();
        let msg = Message::new("create:/b/llm/api/chat/stream");
        let chunks: Vec<Result<ChatChunk, wafer_run::WaferError>> = vec![
            Ok(ChatChunk::text("partial")),
            Err(wafer_run::WaferError::new(
                wafer_run::ErrorCode::Internal,
                "backend died",
            )),
        ];

        let out = sse_chat_response(
            futures::stream::iter(chunks),
            ctx.clone_arc(),
            msg,
            "thread-1".to_string(),
        );
        let buf = out
            .collect_buffered()
            .await
            .expect("producer auto-completes");
        let body = String::from_utf8(buf.body).expect("SSE body is utf8");

        assert!(
            body.ends_with("event: error\ndata: {}\n\n"),
            "expected terminal error frame, got: {body}"
        );
        assert!(!body.contains("[DONE]"), "no [DONE] after an error frame");
        assert!(
            ctx.calls().is_empty(),
            "an errored stream must not persist an assistant turn (mirrors handle_chat)"
        );
    }

    #[tokio::test]
    async fn sse_chat_response_caps_persisted_content() {
        let ctx = RecordingCtx::default();
        let msg = Message::new("create:/b/llm/api/chat/stream");
        let head = "a".repeat(MAX_BUFFERED_RESPONSE_BYTES);
        let chunks: Vec<Result<ChatChunk, wafer_run::WaferError>> =
            vec![Ok(ChatChunk::text(head)), Ok(ChatChunk::text("overflow"))];

        let out = sse_chat_response(
            futures::stream::iter(chunks),
            ctx.clone_arc(),
            msg,
            "thread-1".to_string(),
        );
        let buf = out.collect_buffered().await.expect("stream completes");
        let body = String::from_utf8(buf.body).expect("SSE body is utf8");

        // The overflowing delta is still forwarded to the client...
        assert!(
            body.contains("overflow"),
            "frames keep flowing past the cap"
        );
        assert!(body.ends_with("data: [DONE]\n\n"), "still ends with [DONE]");

        // ...but the persisted assistant message stops at the cap.
        let calls = ctx.calls();
        assert_eq!(calls.len(), 1, "exactly one persistence call");
        let body_json: serde_json::Value =
            serde_json::from_slice(&calls[0].body).expect("persistence body is JSON");
        let content = body_json["content"].as_str().expect("content is a string");
        assert_eq!(
            content.len(),
            MAX_BUFFERED_RESPONSE_BYTES,
            "persisted content stops at the cap (overflowing delta skipped)"
        );
        assert!(
            !content.contains("overflow"),
            "the overflowing delta must not be persisted"
        );
    }
}
