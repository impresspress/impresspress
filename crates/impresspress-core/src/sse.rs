//! Server-Sent Events responses: the one place their wire rules live.
//!
//! Every SSE response impresspress serves is produced through
//! [`event_stream`], which holds the rules a producer would otherwise have to
//! remember one by one:
//!
//! - **The head.** The response leads with `Content-Type: text/event-stream`
//!   (which is what makes every transport stream it, see
//!   `wafer_block::http_codec::declares_streaming`), `Cache-Control: no-cache`
//!   and `X-Accel-Buffering: no`. The last one is nginx's: a reverse proxy in
//!   front of a native server buffers a proxied body unless the response says
//!   not to, and a buffered event stream arrives all at once at its end.
//! - **The open frame.** The first body bytes are [`OPEN_FRAME`], an SSE
//!   comment every client ignores. A transport commits a streamed head only
//!   with the first body chunk (`http_codec::open_response` waits for it), so
//!   a stream whose first event is a model load or a model's first token
//!   away would otherwise hold its status line and headers until then — and
//!   a client could not tell a slow stream from a dead one.
//! - **The consumer going away.** A producer reads its upstream through
//!   [`EventSink::pull`], which answers [`Pulled::ConsumerGone`] the moment
//!   the response is dropped (a client that disconnects, a Worker body the
//!   runtime cancels), even while the upstream has nothing to say. The
//!   upstream is then dropped with the producer, which is what stops it: an
//!   LLM call stops generating (and billing) instead of running to its next
//!   token, or to its end, for nobody.
//! - **The end.** A stream ends in [`DONE_FRAME`] or [`ERROR_FRAME`] and then
//!   an explicit `Complete` ([`EventSink::finish`]). The error frame is the
//!   in-band failure signal, and a body that ends in it is whole: by the time
//!   it is sent the status line and every earlier frame are on the wire, and
//!   an `Error` terminal would only abort that body, discarding the frame
//!   that says what happened. The one path without a terminal is a consumer
//!   that has already gone away, where there is no one left to send one to.

use std::future::Future;

use futures::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use wafer_block::meta::{META_RESP_CONTENT_TYPE, META_RESP_HEADER_PREFIX};
use wafer_run::{
    streams::output::{OutputSink, SinkClosed},
    MaybeSend, MetaEntry, OutputStream,
};

/// The content type of every SSE response.
pub const CONTENT_TYPE: &str = "text/event-stream";

/// The first body bytes of every SSE response: a comment frame, ignored by
/// every SSE client, that commits the response head at once.
pub const OPEN_FRAME: &[u8] = b": open\n\n";

/// Terminal frame for a natural end of stream, letting a client tell it from
/// a transport-level disconnect.
pub const DONE_FRAME: &[u8] = b"data: [DONE]\n\n";

/// Terminal frame for a stream that failed after its head was sent: an
/// upstream error, an item that would not encode, a result that could not be
/// kept.
pub const ERROR_FRAME: &[u8] = b"event: error\ndata: {}\n\n";

/// The leading meta of an SSE response, in the order it is sent: the content
/// type, then `Cache-Control: no-cache` and `X-Accel-Buffering: no` (see the
/// module docs for why each).
pub fn leading_meta() -> Vec<MetaEntry> {
    let header = |name: &str, value: &str| MetaEntry {
        key: format!("{META_RESP_HEADER_PREFIX}{name}"),
        value: value.to_string(),
    };
    vec![
        MetaEntry {
            key: META_RESP_CONTENT_TYPE.to_string(),
            value: CONTENT_TYPE.to_string(),
        },
        header("Cache-Control", "no-cache"),
        header("X-Accel-Buffering", "no"),
    ]
}

/// Encode one item as an SSE `data: <json>\n\n` frame. `None` when the item
/// does not encode; the caller ends the stream with [`ERROR_FRAME`].
pub fn json_frame<T: serde::Serialize>(item: &T) -> Option<Vec<u8>> {
    let json = serde_json::to_vec(item).ok()?;
    let mut frame = Vec::with_capacity(json.len() + 8);
    frame.extend_from_slice(b"data: ");
    frame.extend_from_slice(&json);
    frame.extend_from_slice(b"\n\n");
    Some(frame)
}

/// An SSE response whose events `produce` writes.
///
/// The head ([`leading_meta`]) and [`OPEN_FRAME`] are sent before `produce`
/// runs; a consumer that is already gone by then ends the producer before it
/// starts. `produce` reads its upstream through [`EventSink::pull`] and ends
/// the stream with [`EventSink::finish`].
pub fn event_stream<F, Fut>(produce: F) -> OutputStream
where
    F: FnOnce(EventSink) -> Fut + MaybeSend + 'static,
    Fut: Future<Output = ()> + MaybeSend + 'static,
{
    OutputStream::from_producer(move |sink, cancel| async move {
        for entry in leading_meta() {
            if sink.send_meta(entry).await.is_err() {
                return;
            }
        }
        if sink.send_chunk(OPEN_FRAME.to_vec()).await.is_err() {
            return;
        }
        produce(EventSink { sink, cancel }).await;
    })
}

/// What [`EventSink::pull`] read from an upstream.
#[derive(Debug, PartialEq, Eq)]
pub enum Pulled<T> {
    /// The upstream's next item.
    Item(T),
    /// The upstream ended.
    Ended,
    /// The response was dropped before the upstream said anything more. The
    /// producer returns; no terminal is sent, because no one is left to read
    /// it.
    ConsumerGone,
}

/// The producer's handle on an SSE response opened by [`event_stream`].
pub struct EventSink {
    sink: OutputSink,
    cancel: CancellationToken,
}

impl EventSink {
    /// The next item of `upstream`, raced against the response being dropped.
    ///
    /// Without the race a producer blocked on an upstream that is silent —
    /// a model thinking, a model load between milestones — only learns the
    /// client left when it next sends, so the upstream runs on, for nobody,
    /// until it next has something to say. A consumer already gone when the
    /// pull starts wins over an item that is ready.
    pub async fn pull<S>(&self, upstream: &mut S) -> Pulled<S::Item>
    where
        S: Stream + Unpin,
    {
        match self.cancel.run_until_cancelled(upstream.next()).await {
            None => Pulled::ConsumerGone,
            Some(None) => Pulled::Ended,
            Some(Some(item)) => Pulled::Item(item),
        }
    }

    /// Send one frame. `Err` means the consumer has gone away; the producer
    /// returns.
    pub async fn send(&self, frame: Vec<u8>) -> Result<(), SinkClosed> {
        self.sink.send_chunk(frame).await
    }

    /// End the stream: send `frame` ([`DONE_FRAME`] or [`ERROR_FRAME`]), then
    /// the `Complete` terminal. A frame the consumer is no longer there to
    /// receive ends the producer without a terminal; the dropped sink then
    /// closes the stream as an error nobody reads.
    pub async fn finish(self, frame: &[u8]) {
        if self.sink.send_chunk(frame.to_vec()).await.is_ok() {
            let _ = self.sink.complete(Vec::new()).await;
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::{sync::Arc, time::Duration};

    use tokio::sync::Notify;
    use wafer_block::stream::StreamEvent;

    use super::*;

    /// The next event, or a failure naming `what` if none arrives in time.
    /// The bound only turns a hang into a failure; nothing waits on it.
    async fn next_event(out: &mut OutputStream, what: &str) -> StreamEvent {
        tokio::time::timeout(Duration::from_secs(5), out.next())
            .await
            .unwrap_or_else(|_| panic!("no event within 5 s: {what}"))
            .unwrap_or_else(|| panic!("the stream ended before {what}"))
    }

    /// The head and the open frame are sent before the producer says
    /// anything: a producer that is still waiting on its upstream has
    /// already committed `text/event-stream`, `no-cache` and
    /// `X-Accel-Buffering: no`, and the open frame.
    #[tokio::test]
    async fn the_head_and_the_open_frame_go_out_before_the_first_event() {
        let gate = Arc::new(Notify::new());
        let held = Arc::clone(&gate);
        let mut out = event_stream(move |events| async move {
            held.notified().await;
            events.finish(DONE_FRAME).await;
        });

        let mut head = Vec::new();
        for _ in 0..3 {
            match next_event(&mut out, "the head").await {
                StreamEvent::Meta(entry) => head.push((entry.key, entry.value)),
                other => panic!("expected the head, got {other:?}"),
            }
        }
        assert_eq!(
            head,
            [
                (
                    "resp.content_type".to_string(),
                    "text/event-stream".to_string()
                ),
                (
                    "resp.header.Cache-Control".to_string(),
                    "no-cache".to_string()
                ),
                (
                    "resp.header.X-Accel-Buffering".to_string(),
                    "no".to_string()
                ),
            ]
        );
        assert_eq!(
            next_event(&mut out, "the open frame").await,
            StreamEvent::Chunk(OPEN_FRAME.to_vec()),
            "the open frame follows the head while the producer is still gated"
        );

        gate.notify_one();
        assert_eq!(
            next_event(&mut out, "the done frame").await,
            StreamEvent::Chunk(DONE_FRAME.to_vec())
        );
        assert!(matches!(
            next_event(&mut out, "the terminal").await,
            StreamEvent::Complete { .. }
        ));
    }

    /// An upstream that never yields, recording when it is dropped.
    struct Silent(Arc<Notify>);

    impl Stream for Silent {
        type Item = ();

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<()>> {
            std::task::Poll::Pending
        }
    }

    impl Drop for Silent {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    /// Dropping the response while the upstream is silent ends the pull with
    /// `ConsumerGone`, and the upstream is dropped with the producer.
    #[tokio::test]
    async fn a_dropped_response_ends_a_pull_on_a_silent_upstream() {
        let dropped = Arc::new(Notify::new());
        let upstream = Silent(Arc::clone(&dropped));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut out = event_stream(move |events| async move {
            let mut upstream = upstream;
            let pulled = events.pull(&mut upstream).await;
            let _ = tx.send(pulled);
        });
        // Read up to the open frame, so the producer is in its pull.
        for _ in 0..4 {
            next_event(&mut out, "the head and the open frame").await;
        }

        drop(out);

        let pulled = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("the pull must end once the response is dropped")
            .expect("the producer reports what it pulled");
        assert_eq!(pulled, Pulled::ConsumerGone);
        tokio::time::timeout(Duration::from_secs(5), dropped.notified())
            .await
            .expect("the upstream is dropped with the producer");
    }

    /// `pull` passes items through and reports the end of the upstream.
    #[tokio::test]
    async fn a_pull_yields_items_then_the_end() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _out = event_stream(move |events| async move {
            let mut upstream = futures::stream::iter([1, 2]);
            let mut pulled = Vec::new();
            for _ in 0..3 {
                pulled.push(events.pull(&mut upstream).await);
            }
            let _ = tx.send(pulled);
        });

        let pulled = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("the producer runs")
            .expect("the producer reports what it pulled");
        assert_eq!(pulled, [Pulled::Item(1), Pulled::Item(2), Pulled::Ended]);
    }
}
