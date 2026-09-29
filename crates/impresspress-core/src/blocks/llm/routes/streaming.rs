//! Server-Sent Events framing shared by the two SSE-producing endpoints:
//! chat streaming ([`sse_chat_response`]) and model loading
//! ([`sse_json_response`]). Both emit the same content-type meta event and
//! the same terminal frames ([`SSE_DONE_FRAME`] / [`SSE_ERROR_FRAME`]) via
//! the shared [`sse_json_frame`] encoder, so the wire format can't drift
//! between them.
//!
//! Every SSE response ends in one of those two terminal frames followed by an
//! explicit `Complete` ([`finish_sse`]). The error frame is the in-band
//! failure signal, and a body that ends in it is whole: by the time it is
//! sent the status line and every earlier frame are on the wire on a
//! streaming transport, and an `Error` terminal would only abort that body,
//! or turn it into a 500 on a buffering one, discarding the frame that says
//! what happened. The one path without a terminal is a consumer that has
//! already gone away, where there is no one left to send one to.

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
use crate::blocks::{llm::messages_create, messages::contracts::EntryRole};

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

/// End an SSE response: send `frame` (one of [`SSE_DONE_FRAME`] /
/// [`SSE_ERROR_FRAME`]) and then the `Complete` terminal. A frame the
/// consumer is no longer there to receive ends the producer without a
/// terminal; the dropped sink then closes the stream as an error nobody reads.
async fn finish_sse(sink: OutputSink, frame: &[u8]) {
    if sink.send_chunk(frame.to_vec()).await.is_ok() {
        let _ = sink.complete(Vec::new()).await;
    }
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
/// [`MAX_BUFFERED_RESPONSE_BYTES`] (the first overflowing delta ends
/// accumulation for the rest of the stream, while frames keep flowing to the
/// client), so what is stored is a prefix of the answer rather than one with
/// a hole in it; tool-call/empty deltas are forwarded but not accumulated.
///
/// Reporting does not mirror it. `handle_chat` returns `truncated` because
/// the body it returns *is* the capped text; here the client has already
/// received every frame, so its copy is complete and there is nothing to
/// flag on the wire — only the stored copy is shorter, which is logged at
/// end-of-stream.
///
/// A service error or encode failure terminates the stream with an error
/// frame and skips persistence — the same outcome as `handle_chat`, which
/// returns a 500 without persisting when the stream errors.
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
                finish_sse(sink, SSE_ERROR_FRAME).await;
                return;
            };
            if let ChunkDelta::Text(s) = &chunk.delta {
                if truncated || content.len() + s.len() > MAX_BUFFERED_RESPONSE_BYTES {
                    // Stop accumulating for the rest of the stream (same
                    // stop-for-good semantics as `handle_chat`) but keep
                    // forwarding frames — the client still receives the full
                    // stream. Accepting a later delta that happens to fit
                    // would store the end of the answer joined to its
                    // beginning with the middle missing.
                    truncated = true;
                } else {
                    content.push_str(s);
                }
            }
            let Some(frame) = sse_json_frame(&chunk) else {
                finish_sse(sink, SSE_ERROR_FRAME).await;
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
        // `[DONE]` already sees the new message.
        //
        // Which is exactly why a failed write cannot still send `[DONE]`: the
        // refetch it triggers would replace a complete answer on screen with
        // a conversation that never contained it. `handle_chat` answers the
        // same failure with a status, but by this point the status line and
        // every content frame are already on the wire, so the terminal frame
        // is the only channel left — the same `event: error` a mid-stream
        // service failure emits.
        if let Err(error) = messages_create(
            ctx.as_ref(),
            &msg,
            &thread_id,
            EntryRole::Assistant,
            &content,
        )
        .await
        {
            tracing::error!(
                thread_id = %thread_id,
                reply_bytes = content.len(),
                error = %error,
                "llm streamed assistant turn was delivered but could not be stored"
            );
            finish_sse(sink, SSE_ERROR_FRAME).await;
            return;
        }

        finish_sse(sink, SSE_DONE_FRAME).await;
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
                finish_sse(sink, SSE_ERROR_FRAME).await;
                return;
            };
            if sink.send_chunk(frame).await.is_err() {
                return;
            }
        }
        finish_sse(sink, SSE_DONE_FRAME).await;
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
            .expect("the error frame ends a whole SSE body, so the stream completes");
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

    /// An assistant turn the store refused ends the stream with an error
    /// frame, not `[DONE]`.
    ///
    /// The persistence was `let _ =`, and `[DONE]` is precisely the signal a
    /// client refetches history on — so the refetch replaced a complete
    /// answer on screen with a conversation that never contained it. The
    /// content frames are already on the wire and the status line is long
    /// since committed, so unlike `handle_chat` this path has no status left
    /// to change: the in-band error frame is the only channel it still has,
    /// and it is the same frame a mid-stream service failure emits.
    #[tokio::test]
    async fn sse_chat_response_reports_a_failed_persist_instead_of_done() {
        use crate::blocks::llm::routes::test_support::MessagesWriteFails;

        let ctx = MessagesWriteFails::after(RecordingCtx::default().clone_arc(), 0);
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
            body.contains("Hel") && body.contains("lo"),
            "the frames already delivered are still delivered, got: {body}"
        );
        assert!(
            body.ends_with("event: error\ndata: {}\n\n"),
            "a turn the store refused must not end in [DONE], got: {body}"
        );
        assert!(
            !body.contains("[DONE]"),
            "[DONE] is what a client refetches history on, got: {body}"
        );
    }

    /// The persisted turn is a prefix of the answer, never a splice.
    ///
    /// The cap was checked per delta, so an overflowing delta was skipped and
    /// a later, smaller one was appended anyway — the stored message then read
    /// as a complete answer whose middle was missing. The client still sees
    /// every frame; it is the stored text that must not lie.
    #[tokio::test]
    async fn sse_chat_response_stops_persisting_after_the_first_overflow() {
        let ctx = RecordingCtx::default();
        let msg = Message::new("create:/b/llm/api/chat/stream");
        let head = "a".repeat(MAX_BUFFERED_RESPONSE_BYTES - 10);
        let chunks: Vec<Result<ChatChunk, wafer_run::WaferError>> = vec![
            Ok(ChatChunk::text(head.clone())),
            // Overflows the remaining 10 bytes...
            Ok(ChatChunk::text("B".repeat(100))),
            // ...and this one would still fit, which is the bug.
            Ok(ChatChunk::text("tail")),
        ];

        let out = sse_chat_response(
            futures::stream::iter(chunks),
            ctx.clone_arc(),
            msg,
            "thread-1".to_string(),
        );
        let buf = out.collect_buffered().await.expect("stream completes");
        let body = String::from_utf8(buf.body).expect("SSE body is utf8");
        assert!(
            body.contains("tail"),
            "every frame is still forwarded to the client"
        );

        let calls = ctx.calls();
        assert_eq!(calls.len(), 1, "exactly one persistence call");
        let body_json: serde_json::Value =
            serde_json::from_slice(&calls[0].body).expect("persistence body is JSON");
        let content = body_json["content"].as_str().expect("content is a string");
        assert_eq!(
            content, head,
            "the stored turn stops at the last delta that fitted"
        );
        assert!(
            !content.contains("tail"),
            "a delta after the cap must not be spliced onto the prefix"
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
