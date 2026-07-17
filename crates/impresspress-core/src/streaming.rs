//! Response-streaming primitives shared by the request pipeline and the
//! platform adapters (native listener, Cloudflare, browser).
//!
//! A block declares that its response should stream — bytes flowing to the
//! client while the producer is still working — by emitting its response
//! headers as **leading `Meta` events** before the first body `Chunk`.
//! [`wants_streaming`] is the single decision both the pipeline and the
//! Cloudflare adapter consult, so they can never disagree about whether a
//! given response streams or buffers.
//!
//! Two streaming signals are honored:
//! - A streaming `resp.content_type` (SSE / generic byte streams), matched by
//!   [`is_streaming_content_type`].
//! - The explicit [`META_RESP_STREAM`] opt-in marker, set by large binary
//!   download handlers whose real content-type (`image/*`, `application/pdf`,
//!   …) is not one of the streaming families but which must still stream to
//!   avoid buffering the whole object in the isolate.
//!
//! The buffered fallback ([`collect_capped_with_prelude`]) enforces a byte cap
//! so a response that must be buffered (small SSR pages, JSON) cannot balloon
//! the isolate — an over-limit body is reported as [`CappedCollect::OverLimit`]
//! (the adapter maps it to HTTP 413) rather than assembled whole.
//!
//! All logic here is host-testable: it operates on `wafer_block` stream types
//! that run natively under tokio, so the size-cap accounting, the 413
//! threshold, the streaming decision, and the download framing are all covered
//! by unit tests in this module. Only the Worker/`web_sys` glue that turns the
//! resulting `OutputStream` into a platform `Response` lives in the adapters.

use futures::StreamExt;
use wafer_block::{
    http_codec::{self, ResponseMetaPart},
    meta::{META_RESP_CONTENT_TYPE, META_RESP_HEADER_PREFIX},
    stream::StreamEvent,
};
use wafer_core::clients::storage::NativeStorageGetStream;
use wafer_run::{
    streams::output::{BufferedResponse, OutputSink, OutputStream, TerminalNotResponse},
    MetaEntry, MetaGet, WaferError,
};

/// Canonical response-meta key a block sets (value [`STREAM_MARKER_VALUE`]) to
/// force the streaming response path regardless of content-type.
///
/// Used for large binary downloads whose content-type (`image/*`,
/// `application/pdf`, …) is not one of the [`is_streaming_content_type`]
/// families but which must still stream to avoid buffering the whole object in
/// the isolate. Consumed by [`wants_streaming`]; it is **never** emitted as an
/// HTTP header — `wafer_block::http_codec::classify_response_meta` does not
/// recognize the `resp.stream` key, so it is inert to every header-applying
/// adapter.
pub const META_RESP_STREAM: &str = "resp.stream";

/// The value [`META_RESP_STREAM`] must carry to opt into streaming.
pub const STREAM_MARKER_VALUE: &str = "1";

/// Maximum body size the buffered response path assembles in the isolate
/// before reporting [`CappedCollect::OverLimit`] (which the adapter maps to
/// HTTP 413 Payload Too Large). Responses larger than this are expected to
/// take the streaming path, which never buffers. 100 MiB matches the
/// storage/network streaming service caps upstream.
pub const MAX_BUFFERED_RESPONSE_BYTES: usize = 100 * 1024 * 1024;

/// True for content-types that should stream body chunks to the client as
/// they're produced rather than buffer the entire response. Today: SSE and
/// generic byte streams (which feature blocks use for archives / progress).
pub fn is_streaming_content_type(ct: &str) -> bool {
    let lower = ct.to_ascii_lowercase();
    lower.starts_with("text/event-stream") || lower.starts_with("application/octet-stream")
}

/// The canonical `resp.content_type` among the leading meta entries, if any.
/// Legacy aliases (a literal `Content-Type` meta key) are not honored — the
/// canonical-keys-only policy is pinned by `wafer_block::http_codec`.
pub fn leading_content_type(meta: &[MetaEntry]) -> Option<&str> {
    http_codec::response_meta_parts(meta).find_map(|part| match part {
        ResponseMetaPart::ContentType(ct) => Some(ct),
        _ => None,
    })
}

/// The single streaming decision, consulted identically by the pipeline and
/// every adapter: a response streams iff it declared streaming intent up front
/// via leading meta — either the explicit [`META_RESP_STREAM`] marker or a
/// streaming `resp.content_type`. Buffered responses (`respond_with_meta`) put
/// all their meta in the trailing `Complete`, so they carry no leading meta
/// and never match here.
pub fn wants_streaming(leading_meta: &[MetaEntry]) -> bool {
    if MetaGet::get(leading_meta, META_RESP_STREAM) == Some(STREAM_MARKER_VALUE) {
        return true;
    }
    leading_content_type(leading_meta).is_some_and(is_streaming_content_type)
}

/// Pull `Meta` events off the front of an `OutputStream`, stopping at the first
/// non-`Meta` event. Returns the accumulated meta and the next event (if any),
/// letting a caller peek the response's declared headers before deciding
/// whether to stream the body or buffer it.
pub async fn drain_leading_meta(stream: &mut OutputStream) -> (Vec<MetaEntry>, Option<StreamEvent>) {
    let mut meta = Vec::new();
    while let Some(ev) = stream.next().await {
        match ev {
            StreamEvent::Meta(entry) => meta.push(entry),
            other => return (meta, Some(other)),
        }
    }
    (meta, None)
}

/// Forward one `StreamEvent` into an `OutputSink`. Returns the sink back for
/// non-terminal events so the caller can keep pumping; terminal events (and a
/// hung-up consumer) consume it and return `None`.
async fn forward_event(sink: OutputSink, ev: StreamEvent) -> Option<OutputSink> {
    match ev {
        StreamEvent::Chunk(bytes) => sink.send_chunk(bytes).await.ok().map(|()| sink),
        StreamEvent::Meta(entry) => sink.send_meta(entry).await.ok().map(|()| sink),
        StreamEvent::Complete { meta } => {
            let _ = sink.complete(meta).await;
            None
        }
        StreamEvent::Error(err) => {
            let _ = sink.error(*err).await;
            None
        }
        StreamEvent::Drop => {
            let _ = sink.drop_request().await;
            None
        }
        StreamEvent::Continue(msg) => {
            let _ = sink.continue_with(msg).await;
            None
        }
        StreamEvent::Halt { body, meta } => {
            let _ = sink.halt(body, meta).await;
            None
        }
    }
}

/// Replay leading meta + the peeked event + remaining stream events into a
/// fresh `OutputStream`. Used for streaming responses where the pipeline
/// doesn't want to drain the body into memory: the leading meta reaches the
/// adapter before the first body chunk, so headers are applied before the body
/// finishes.
pub fn rebuild_streaming(
    leading_meta: Vec<MetaEntry>,
    next_event: Option<StreamEvent>,
    rest: OutputStream,
) -> OutputStream {
    OutputStream::from_producer(move |sink, _cancel| async move {
        for entry in leading_meta {
            if sink.send_meta(entry).await.is_err() {
                return;
            }
        }
        let Some(next_event) = next_event else {
            // The stream ended right after its leading meta with no terminal;
            // close out as an empty Complete.
            let _ = sink.complete(Vec::new()).await;
            return;
        };
        let Some(mut sink) = forward_event(sink, next_event).await else {
            return;
        };
        let mut rest = rest;
        while let Some(ev) = rest.next().await {
            match forward_event(sink, ev).await {
                Some(s) => sink = s,
                None => return,
            }
        }
        // `rest` ended without a terminal; `from_producer` auto-Completes.
    })
}

/// Outcome of draining a buffered response under a size cap.
pub enum CappedCollect {
    /// The stream terminated within the cap — same shape
    /// `OutputStream::collect_buffered` would have produced over the
    /// reassembled stream.
    Terminal(Result<BufferedResponse, TerminalNotResponse>),
    /// The accumulated body exceeded the cap before a terminal arrived. The
    /// adapter maps this to HTTP 413 rather than assembling the whole body.
    OverLimit,
}

/// Collect the remaining stream events into a buffer, prepending the leading
/// meta + the already-peeked next event, while enforcing a running body-size
/// `cap`.
///
/// Equivalent to running `OutputStream::collect_buffered` over the reassembled
/// stream — including its contract that a `Halt` terminal **replaces** any
/// previously streamed chunks/meta — except that once the accumulated body
/// would exceed `cap`, collection stops immediately with
/// [`CappedCollect::OverLimit`] (no further bytes are read into memory).
///
/// `next_event` must come from [`drain_leading_meta`] (i.e. it is never
/// `StreamEvent::Meta`); a mid-stream `Meta` on `rest` is still accepted.
pub async fn collect_capped_with_prelude(
    rest: OutputStream,
    leading_meta: Vec<MetaEntry>,
    next_event: Option<StreamEvent>,
    cap: usize,
) -> CappedCollect {
    let mut rest = rest;
    let mut body: Vec<u8> = Vec::new();
    let mut meta: Vec<MetaEntry> = leading_meta;
    let mut event = next_event;

    loop {
        match event {
            None => return CappedCollect::Terminal(Err(TerminalNotResponse::Malformed)),
            Some(StreamEvent::Chunk(bytes)) => {
                if body.len().saturating_add(bytes.len()) > cap {
                    return CappedCollect::OverLimit;
                }
                if body.is_empty() {
                    body = bytes;
                } else {
                    body.extend_from_slice(&bytes);
                }
            }
            // `drain_leading_meta` never hands us a leading `Meta`, but a
            // producer may still emit one mid-body — accumulate it in order.
            Some(StreamEvent::Meta(entry)) => meta.push(entry),
            Some(StreamEvent::Complete { meta: trailing }) => {
                meta.extend(trailing);
                return CappedCollect::Terminal(Ok(BufferedResponse { body, meta }));
            }
            Some(StreamEvent::Halt {
                body: halt_body,
                meta: halt_meta,
            }) => {
                // Halt carries a complete response; per the `collect_buffered`
                // contract any prior streamed events — the prelude included —
                // are replaced by its payload.
                if !meta.is_empty() || !body.is_empty() {
                    tracing::warn!(
                        discarded_body_bytes = body.len(),
                        discarded_meta_entries = meta.len(),
                        "Halt terminal arrived after leading Meta / streamed chunks; discarding \
                         prelude (producer must not mix Halt with streamed events)"
                    );
                }
                return CappedCollect::Terminal(Err(TerminalNotResponse::Halt(BufferedResponse {
                    body: halt_body,
                    meta: halt_meta,
                })));
            }
            Some(StreamEvent::Error(err)) => {
                return CappedCollect::Terminal(Err(TerminalNotResponse::Error(*err)))
            }
            Some(StreamEvent::Drop) => {
                return CappedCollect::Terminal(Err(TerminalNotResponse::Drop))
            }
            Some(StreamEvent::Continue(msg)) => {
                return CappedCollect::Terminal(Err(TerminalNotResponse::Continue(msg)))
            }
        }
        event = rest.next().await;
    }
}

/// Uncapped variant of [`collect_capped_with_prelude`] used by the request
/// pipeline (which buffers only to read a status code for the audit log; the
/// isolate-memory cap belongs at the platform-adapter boundary, so it is not
/// applied here). Delegates to the capped collector with an unreachable cap so
/// there is a single drain implementation.
pub async fn collect_buffered_with_prelude(
    rest: OutputStream,
    leading_meta: Vec<MetaEntry>,
    next_event: Option<StreamEvent>,
) -> Result<BufferedResponse, TerminalNotResponse> {
    match collect_capped_with_prelude(rest, leading_meta, next_event, usize::MAX).await {
        CappedCollect::Terminal(t) => t,
        // `usize::MAX` is never exceeded by any real body (bounded by memory).
        CappedCollect::OverLimit => unreachable!("usize::MAX byte cap cannot be exceeded"),
    }
}

/// Rebuild a single-terminal `OutputStream` from an already-collected buffered
/// terminal, so an adapter can feed it straight back through
/// `http_codec::collect_http_response` and reuse the canonical terminal→status
/// mapping (no duplicated `ErrorCode`→status table). Byte-identical to what the
/// codec would have produced for the original stream.
pub fn terminal_to_stream(result: Result<BufferedResponse, TerminalNotResponse>) -> OutputStream {
    match result {
        Ok(buf) => OutputStream::respond_with_meta(buf.body, buf.meta),
        Err(TerminalNotResponse::Halt(buf)) => OutputStream::halt(buf.body, buf.meta),
        Err(TerminalNotResponse::Error(err)) => OutputStream::error(err),
        Err(TerminalNotResponse::Drop) => OutputStream::drop_request(),
        Err(TerminalNotResponse::Continue(msg)) => OutputStream::continue_with(msg),
        Err(TerminalNotResponse::Malformed) => OutputStream::error(WaferError::new(
            wafer_run::ErrorCode::Internal,
            "stream ended without terminal event",
        )),
    }
}

/// Build the leading response-meta for a streamed download: the
/// [`META_RESP_STREAM`] opt-in marker, the object's `Content-Type`, and any
/// extra response headers (e.g. `Content-Disposition`, `Cache-Control`). All
/// entries are emitted as leading meta so the adapter applies them before the
/// first body byte flushes.
pub fn download_leading_meta(content_type: &str, extra_headers: &[(&str, &str)]) -> Vec<MetaEntry> {
    let mut meta = Vec::with_capacity(2 + extra_headers.len());
    meta.push(MetaEntry {
        key: META_RESP_STREAM.to_string(),
        value: STREAM_MARKER_VALUE.to_string(),
    });
    if !content_type.is_empty() {
        meta.push(MetaEntry {
            key: META_RESP_CONTENT_TYPE.to_string(),
            value: content_type.to_string(),
        });
    }
    for (name, value) in extra_headers {
        meta.push(MetaEntry {
            key: format!("{META_RESP_HEADER_PREFIX}{name}"),
            value: (*value).to_string(),
        });
    }
    meta
}

/// Turn a streaming storage download into an `OutputStream`: emit `leading_meta`
/// (headers first), then forward the object body chunk-by-chunk from `stream`
/// without ever buffering the whole object. A body-read failure is surfaced as
/// an `Error` terminal after whatever bytes already streamed (no silent
/// truncation reported as a clean `Complete`). A dropped consumer aborts the
/// upstream read promptly via the paired cancellation token.
pub fn stream_download(stream: NativeStorageGetStream, leading_meta: Vec<MetaEntry>) -> OutputStream {
    OutputStream::from_producer(move |sink, cancel| async move {
        for entry in leading_meta {
            if sink.send_meta(entry).await.is_err() {
                return;
            }
        }
        let mut stream = stream;
        loop {
            // Race the upstream read against cancellation so a dropped consumer
            // aborts a blocked read promptly rather than after the next chunk.
            let next = match cancel.run_until_cancelled(stream.next()).await {
                None => return,
                Some(n) => n,
            };
            match next {
                None => break,
                Some(Ok(chunk)) => {
                    if sink.send_chunk(chunk).await.is_err() {
                        return;
                    }
                }
                Some(Err(e)) => {
                    let _ = sink.error(e).await;
                    return;
                }
            }
        }
        let _ = sink.complete(Vec::new()).await;
    })
}

/// View the body-carrying events of a streaming response as a
/// `Stream<Item = Result<Vec<u8>, WaferError>>`, prepending the already-peeked
/// `first_chunk`. `Chunk`s map to `Ok(bytes)`; an `Error` terminal maps to a
/// final `Err(e)`; mid-body `Meta` is dropped (too late to affect headers) and
/// every other terminal ends the stream. Adapters pipe this straight into a
/// platform `ReadableStream` body.
pub fn download_body_stream(
    first_chunk: Vec<u8>,
    rest: OutputStream,
) -> impl futures::Stream<Item = Result<Vec<u8>, WaferError>> + 'static {
    let head = futures::stream::iter(std::iter::once(Ok(first_chunk)));
    let tail = rest.filter_map(|ev| async move {
        match ev {
            StreamEvent::Chunk(bytes) => Some(Ok(bytes)),
            StreamEvent::Error(err) => Some(Err(*err)),
            _ => None,
        }
    });
    head.chain(tail)
}

#[cfg(test)]
mod tests {
    // `super::*` re-exports the module's own `use` imports (OutputStream,
    // MetaEntry, MetaGet, http_codec, StreamEvent, WaferError, …) plus every
    // public item defined here; only names not already in scope are imported
    // explicitly.
    use wafer_run::{ErrorCode, Message, META_RESP_STATUS};

    use super::*;

    fn meta(key: &str, value: &str) -> MetaEntry {
        MetaEntry {
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn streaming_content_types_match_sse_and_octet_only() {
        assert!(is_streaming_content_type("text/event-stream"));
        assert!(is_streaming_content_type("application/octet-stream"));
        assert!(is_streaming_content_type("TEXT/EVENT-STREAM; charset=utf-8"));
        assert!(!is_streaming_content_type("image/png"));
        assert!(!is_streaming_content_type("application/json"));
    }

    #[test]
    fn wants_streaming_on_marker_regardless_of_content_type() {
        // A download declares an image content-type — not a streaming family —
        // but the explicit marker forces the streaming path.
        let m = vec![
            meta(META_RESP_STREAM, STREAM_MARKER_VALUE),
            meta(META_RESP_CONTENT_TYPE, "image/png"),
        ];
        assert!(wants_streaming(&m));
    }

    #[test]
    fn wants_streaming_on_sse_content_type_without_marker() {
        let m = vec![meta(META_RESP_CONTENT_TYPE, "text/event-stream")];
        assert!(wants_streaming(&m));
    }

    #[test]
    fn wants_streaming_false_for_buffered_response_shape() {
        // Buffered responses carry no leading meta at all.
        assert!(!wants_streaming(&[]));
        // A non-streaming content-type alone (as leading meta) does not stream.
        assert!(!wants_streaming(&[meta(META_RESP_CONTENT_TYPE, "application/json")]));
        // The marker with the wrong value does not opt in.
        assert!(!wants_streaming(&[meta(META_RESP_STREAM, "0")]));
    }

    #[test]
    fn download_leading_meta_carries_marker_content_type_and_headers() {
        let m = download_leading_meta(
            "image/png",
            &[
                ("Content-Disposition", "inline; filename=\"a.png\""),
                ("Cache-Control", "private, max-age=3600"),
            ],
        );
        assert_eq!(MetaGet::get(&m, META_RESP_STREAM), Some(STREAM_MARKER_VALUE));
        assert_eq!(MetaGet::get(&m, META_RESP_CONTENT_TYPE), Some("image/png"));
        assert_eq!(
            MetaGet::get(&m, "resp.header.Content-Disposition"),
            Some("inline; filename=\"a.png\"")
        );
        assert_eq!(
            MetaGet::get(&m, "resp.header.Cache-Control"),
            Some("private, max-age=3600")
        );
        // The marker is inert to the HTTP header layer.
        assert!(
            http_codec::response_meta_parts(&m)
                .all(|p| !matches!(p, ResponseMetaPart::Header { name, .. } if name == "stream")),
            "the streaming marker must never surface as a response header"
        );
    }

    #[tokio::test]
    async fn collect_capped_reports_over_limit_before_buffering_whole_body() {
        // Three 4-byte chunks = 12 bytes; cap = 5. Must bail at OverLimit.
        let mut stream = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_chunk(b"aaaa".to_vec()).await.ok();
            sink.send_chunk(b"bbbb".to_vec()).await.ok();
            sink.send_chunk(b"cccc".to_vec()).await.ok();
            sink.complete(vec![]).await.ok();
        });
        let (leading, next) = drain_leading_meta(&mut stream).await;
        let outcome = collect_capped_with_prelude(stream, leading, next, 5).await;
        assert!(matches!(outcome, CappedCollect::OverLimit));
    }

    #[tokio::test]
    async fn collect_capped_within_limit_returns_buffered_terminal() {
        let stream = OutputStream::respond_with_meta(
            b"hello".to_vec(),
            vec![meta(META_RESP_STATUS, "200")],
        );
        let mut stream = stream;
        let (leading, next) = drain_leading_meta(&mut stream).await;
        match collect_capped_with_prelude(stream, leading, next, MAX_BUFFERED_RESPONSE_BYTES).await {
            CappedCollect::Terminal(Ok(buf)) => {
                assert_eq!(buf.body, b"hello");
                assert_eq!(MetaGet::get(&buf.meta, META_RESP_STATUS), Some("200"));
            }
            _ => panic!("expected a buffered Ok terminal within the cap"),
        }
    }

    #[tokio::test]
    async fn collect_capped_propagates_error_terminal() {
        let stream = OutputStream::error(WaferError::new(ErrorCode::NotFound, "gone"));
        let mut stream = stream;
        let (leading, next) = drain_leading_meta(&mut stream).await;
        match collect_capped_with_prelude(stream, leading, next, 1024).await {
            CappedCollect::Terminal(Err(TerminalNotResponse::Error(e))) => {
                assert_eq!(e.code, ErrorCode::NotFound);
            }
            _ => panic!("expected the Error terminal to propagate"),
        }
    }

    /// `terminal_to_stream` must round-trip through
    /// `http_codec::collect_http_response` to the SAME parts the codec would
    /// have produced for the original terminal — pins zero drift from the
    /// canonical `ErrorCode`→status mapping.
    #[tokio::test]
    async fn terminal_to_stream_round_trips_through_codec() {
        // Ok terminal.
        let buf = BufferedResponse {
            body: b"body".to_vec(),
            meta: vec![meta(META_RESP_STATUS, "201")],
        };
        let parts = http_codec::collect_http_response(terminal_to_stream(Ok(buf))).await;
        assert_eq!(parts.status, 201);
        assert_eq!(parts.body, b"body");

        // Error terminal maps via the codec's ErrorCode→status table.
        let err = WaferError::new(ErrorCode::PermissionDenied, "nope");
        let parts =
            http_codec::collect_http_response(terminal_to_stream(Err(TerminalNotResponse::Error(
                err,
            ))))
            .await;
        assert_eq!(parts.status, 403);

        // Drop → 204.
        let parts =
            http_codec::collect_http_response(terminal_to_stream(Err(TerminalNotResponse::Drop)))
                .await;
        assert_eq!(parts.status, 204);
    }

    #[tokio::test]
    async fn download_body_stream_prepends_first_chunk_and_forwards_rest() {
        let rest = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_meta(meta("mid", "ignored")).await.ok();
            sink.send_chunk(b"two".to_vec()).await.ok();
            sink.send_chunk(b"three".to_vec()).await.ok();
            sink.complete(vec![]).await.ok();
        });
        let collected: Vec<Vec<u8>> = download_body_stream(b"one".to_vec(), rest)
            .map(|r| r.expect("no error terminal"))
            .collect()
            .await;
        assert_eq!(collected, vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]);
    }

    #[tokio::test]
    async fn download_body_stream_surfaces_error_terminal() {
        let rest = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_chunk(b"partial".to_vec()).await.ok();
            let _ = sink
                .error(WaferError::new(ErrorCode::Unavailable, "read failed"))
                .await;
        });
        let items: Vec<Result<Vec<u8>, WaferError>> =
            download_body_stream(b"head".to_vec(), rest).collect().await;
        assert!(items[0].as_ref().is_ok());
        assert!(items.last().unwrap().is_err(), "error terminal must surface");
    }

    #[tokio::test]
    async fn rebuild_streaming_preserves_leading_meta_then_body() {
        let rest = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_chunk(b"body".to_vec()).await.ok();
            sink.complete(vec![]).await.ok();
        });
        let leading = vec![meta(META_RESP_CONTENT_TYPE, "text/event-stream")];
        let rebuilt = rebuild_streaming(
            leading,
            Some(StreamEvent::Chunk(b"first".to_vec())),
            rest,
        );
        // Draining the rebuilt stream must yield the leading meta first, then
        // the peeked chunk, then the rest — the streaming contract.
        let mut rebuilt = rebuilt;
        let (m, next) = drain_leading_meta(&mut rebuilt).await;
        assert_eq!(MetaGet::get(&m, META_RESP_CONTENT_TYPE), Some("text/event-stream"));
        assert!(matches!(next, Some(StreamEvent::Chunk(ref b)) if b == b"first"));
    }

    // A `Message` import sanity anchor so `Message` stays used if the codec
    // signature shifts; `continue_with` reconstruction relies on it.
    #[tokio::test]
    async fn terminal_to_stream_reconstructs_continue() {
        let msg = Message::new("next");
        let out = terminal_to_stream(Err(TerminalNotResponse::Continue(msg)));
        let parts = http_codec::collect_http_response(out).await;
        assert_eq!(parts.status, 200);
        assert!(parts.body.is_empty());
    }
}
