//! The two SSE-producing llm endpoints: chat streaming
//! ([`sse_chat_response`]) and model loading ([`sse_json_response`]).
//!
//! Both are [`crate::sse::event_stream`]s, so the SSE wire rules — the head
//! (`text/event-stream`, `Cache-Control: no-cache`, `X-Accel-Buffering: no`),
//! the open frame that commits it at once, the frame encoding, the
//! `[DONE]` / `event: error` terminals, and stopping as soon as the client
//! goes away — are the shared module's, and cannot drift between the two.
//!
//! A client that goes away stops the model: each producer reads its upstream
//! through [`EventSink::pull`], which ends the read the moment the response
//! is dropped, and the upstream stream is dropped with the producer. That is
//! what fires the cancellation token `wafer-core`'s llm handler gave the
//! service, so a provider request in flight is abandoned rather than run to
//! its next token — or, for a model that is still thinking, to its end.

use std::sync::Arc;

use wafer_core::clients::{
    llm::{ChatChunk, ChunkDelta},
    NativeTypedFrameStream,
};
use wafer_run::{context::Context, Message, OutputStream};

use super::chat::MAX_BUFFERED_RESPONSE_BYTES;
use crate::{
    blocks::{llm::messages_create, messages::contracts::EntryRole},
    sse::{event_stream, json_frame, EventSink, Pulled, DONE_FRAME, ERROR_FRAME},
};

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
/// answers a failed stream with an error status and stores nothing. A
/// failure that is the model's first item never reaches this function:
/// `handle_chat_stream` reads that item before the response starts and
/// answers it with a status, as `handle_chat` does.
///
/// A client that goes away mid-answer stops the model and stores nothing:
/// the answer was neither finished nor delivered, so there is no turn to
/// keep, as for a model that failed part-way.
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
    event_stream(move |events: EventSink| async move {
        let mut stream = stream;
        let mut content = String::new();
        let mut truncated = false;
        loop {
            let chunk = match events.pull(&mut stream).await {
                Pulled::Item(Ok(chunk)) => chunk,
                Pulled::Item(Err(_)) => {
                    events.finish(ERROR_FRAME).await;
                    return;
                }
                Pulled::Ended => break,
                Pulled::ConsumerGone => return,
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
            let Some(frame) = json_frame(&chunk) else {
                events.finish(ERROR_FRAME).await;
                return;
            };
            if events.send(frame).await.is_err() {
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
            events.finish(ERROR_FRAME).await;
            return;
        }

        events.finish(DONE_FRAME).await;
    })
}

/// Stream a typed frame stream to the client as JSON Server-Sent Events:
/// each item as a `data: <json>\n\n` frame, then `data: [DONE]` at a natural
/// end. A service or encode error becomes a terminal `event: error` frame.
///
/// Used by the model-load endpoint, whose first progress item can be a
/// download away: the head and the open frame go out before it.
pub(super) fn sse_json_response<T>(stream: NativeTypedFrameStream<T>) -> OutputStream
where
    T: serde::Serialize + serde::de::DeserializeOwned + Unpin + Send + 'static,
{
    event_stream(move |events: EventSink| async move {
        let mut stream = stream;
        loop {
            let item = match events.pull(&mut stream).await {
                Pulled::Item(item) => item,
                Pulled::Ended => break,
                Pulled::ConsumerGone => return,
            };
            // A mid-stream service error or a JSON-encode failure both
            // terminate the stream with a final `event: error` frame, so the
            // consumer sees a clean SSE event instead of an abrupt disconnect.
            let Some(frame) = item.ok().and_then(|v| json_frame(&v)) else {
                events.finish(ERROR_FRAME).await;
                return;
            };
            if events.send(frame).await.is_err() {
                return;
            }
        }
        events.finish(DONE_FRAME).await;
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::StreamExt;

    use super::*;
    use crate::{blocks::llm::routes::test_support::RecordingCtx, sse::OPEN_FRAME};

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
            body.starts_with(std::str::from_utf8(OPEN_FRAME).expect("utf8")),
            "the open frame comes first, got: {body}"
        );
        assert!(
            body.ends_with("data: [DONE]\n\n"),
            "expected terminal [DONE] frame, got: {body}"
        );
        assert!(
            body.contains("Hel") && body.contains("lo"),
            "both text deltas must be forwarded as frames, got: {body}"
        );
        assert_eq!(
            buf.meta,
            crate::sse::leading_meta(),
            "the head is the SSE head: content type, no-cache, no proxy buffering"
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

    /// An upstream that never yields, the way a model is while it thinks,
    /// and that reports when it is dropped.
    struct Thinking(Option<tokio::sync::oneshot::Sender<()>>);

    impl futures::Stream for Thinking {
        type Item = Result<ChatChunk, wafer_run::WaferError>;

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }

    impl Drop for Thinking {
        fn drop(&mut self) {
            if let Some(dropped) = self.0.take() {
                let _ = dropped.send(());
            }
        }
    }

    /// A consumer that goes away while the model is silent stops the model
    /// at once, and no turn is stored.
    ///
    /// The producer used to wait on the model's next item with the
    /// cancellation token unread, so a client that left while the model was
    /// thinking kept the provider call running until its next token — and
    /// a model that never sent another one kept it running for good.
    #[tokio::test]
    async fn a_dropped_consumer_stops_the_upstream_while_it_is_idle() {
        let ctx = RecordingCtx::default();
        let (dropped, upstream_dropped) = tokio::sync::oneshot::channel();
        let mut out = sse_chat_response(
            futures::stream::iter([Ok(ChatChunk::text("Hel"))]).chain(Thinking(Some(dropped))),
            ctx.clone_arc(),
            Message::new("create:/b/llm/api/chat/stream"),
            "thread-1".to_string(),
        );
        // Read up to the first content frame: the producer is then waiting
        // on the silent model.
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), out.next())
                .await
                .expect("the first frame is forwarded")
                .expect("the stream is open");
            if matches!(&event, wafer_block::stream::StreamEvent::Chunk(c) if c.starts_with(b"data: "))
            {
                break;
            }
        }

        drop(out);

        tokio::time::timeout(Duration::from_secs(5), upstream_dropped)
            .await
            .expect("the model's stream is dropped once the consumer goes away")
            .expect("the drop is reported");
        assert!(
            ctx.calls().is_empty(),
            "an answer the client left part-way is not stored"
        );
    }
}
