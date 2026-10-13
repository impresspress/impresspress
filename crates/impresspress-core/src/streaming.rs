//! Response-streaming helpers the request pipeline, the blocks and the wasm
//! platform adapters (Cloudflare, browser) share.
//!
//! Whether a response streams is not decided here. A block declares it by
//! emitting its response headers as **leading `Meta` events** before the
//! first body `Chunk`: a streaming `resp.content_type` (SSE, byte streams)
//! or the `resp.stream` marker on a real content type (a file download). The
//! rule is `wafer_block::http_codec::declares_streaming`, and every transport
//! opens a response through `http_codec::open_response` — the native listener
//! in wafer-run, and both wasm adapters here — so none of them can disagree
//! with another, or with the pipeline, about whether a response streams or
//! how its head and body are rendered.
//!
//! What lives here:
//! - The transport byte caps: response ([`MAX_BUFFERED_RESPONSE_BYTES`]),
//!   request ([`MAX_REQUEST_BODY_BYTES`]) and upstream fetch
//!   ([`MAX_NETWORK_RESPONSE_BYTES`]), one declaration each, so an adapter
//!   and a block cannot enforce different numbers for the same limit.
//! - [`collect_capped`], the wasm adapters' buffered path: what
//!   `OutputStream::collect_buffered` does, under a byte cap, so a response
//!   that must be buffered (small SSR pages, JSON) cannot balloon the isolate
//!   — an over-limit body is [`CappedCollect::OverLimit`] (HTTP 413) rather
//!   than assembled whole.
//! - The download framing a block streams a stored object with
//!   ([`download_leading_meta`], [`stream_download`]) and the upload part
//!   buffer the R2 adapter splits a body with ([`UploadPartBuffer`]).
//!
//! All of it runs natively under tokio, so the caps, the download framing and
//! the part splitting are covered by unit tests in this module. Only the
//! Worker/`web_sys` glue that turns an opened response into a platform
//! `Response` lives in the adapters.

use futures::StreamExt;
use wafer_block::{
    http_codec::{self, META_RESP_CONTENT_LENGTH},
    meta::{META_RESP_CONTENT_TYPE, META_RESP_HEADER_PREFIX},
    stream::StreamEvent,
};
use wafer_core::clients::storage::NativeStorageGetStream;
use wafer_run::{
    streams::output::{BufferedResponse, OutputStream, TerminalNotResponse},
    MetaEntry,
};

/// Maximum body size the buffered response path assembles in the isolate
/// before reporting [`CappedCollect::OverLimit`] (which the adapter maps to
/// HTTP 413 Payload Too Large). Responses larger than this are expected to
/// take the streaming path, which never buffers. 100 MiB is a generous ceiling
/// for a *buffered* (non-streaming) HTTP response — SSR pages and JSON API
/// bodies are orders of magnitude smaller — chosen to catch a runaway buffered
/// body before the Worker runtime rejects an oversized `Response`. It is
/// independent of the outbound-fetch response cap
/// ([`MAX_NETWORK_RESPONSE_BYTES`]), which bounds an *upstream* body on a
/// different axis.
pub const MAX_BUFFERED_RESPONSE_BYTES: usize = 100 * 1024 * 1024;

/// Maximum body size a `NetworkService` reads from an *upstream* response, in
/// bytes. SEC-020 — a hostile or runaway upstream cannot read unbounded into
/// the isolate.
///
/// Both wasm adapters enforce this: `impresspress-cloudflare`'s
/// `WorkerFetchService` and `impresspress-browser`'s `BrowserNetworkService`
/// (which passes it across the bridge to `bridge.js`'s `httpFetch`, where the
/// bytes are actually read). It is the same 50 MiB the native backend defaults
/// to — `wafer_block_network::service::DEFAULT_MAX_RESPONSE_BYTES`, a **free**
/// constant on that crate, not the method-scoped one the adapters' comments
/// used to name — and it lives here rather than being re-declared per adapter,
/// which is how it came to have three copies kept in step by comment.
///
/// It is not re-exported from the native crate: `wafer-block-network` pulls in
/// reqwest and does not build for `wasm32-unknown-unknown`.
///
/// Unlike the native default this is **fixed**, not operator-tunable. Native
/// reads `WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES` from the process env at
/// service construction; neither wasm adapter's unit-shaped service has config
/// plumbing to read it from, and this is a security floor rather than a knob.
pub const MAX_NETWORK_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

/// Maximum **request** body a transport admits, in bytes. A larger body is
/// refused with HTTP 413 and never reaches a block whole.
///
/// Both wasm adapters read the body whole before building the
/// `(Message, InputStream)` pair and enforce this constant there —
/// `impresspress-cloudflare`'s `worker_request_to_message` and
/// `impresspress-browser`'s `request_to_message`, which is why it is a single
/// constant rather than a literal in each. The native listener
/// (`wafer-block-http-listener`) streams the body instead, under its own
/// `max_body_bytes` config, whose default is this same 10 MiB —
/// `impresspress_native::serve::register_http_listener` does not set it, so
/// the default is what it runs. It refuses a declared `Content-Length` over
/// the cap before dispatch, and fails the `InputStream` of a body that grows
/// past it (or stalls, or whose connection drops) with an `Err` item, which
/// every consumer answers as a failure rather than storing the prefix.
///
/// It is the hard ceiling on an upload, so anything a block advertises as a
/// per-request size limit has to be clamped to it — see
/// [`crate::blocks::files::quota::clamp_to_transport`], which the files block's
/// admin-editable per-file cap goes through.
///
/// # What the check does and does not buy
///
/// On the wasm adapters the cap is a contract, not a memory guard: the
/// Cloudflare adapter checks a declared `Content-Length` first and returns
/// without reading the stream, so a well-formed oversized upload never enters
/// the isolate, but a chunked request (no length, or a lying one) is caught by
/// the post-read check, by which time the bytes are already in the isolate —
/// and the browser adapter reads the whole `ArrayBuffer` before it can measure
/// it at all, so there the cap only changes the status, never the peak memory.
///
/// # Why it is not simply larger
///
/// The wasm adapters hold the body whole (128 MB on a Cloudflare Worker, one
/// shared linear memory in the Service Worker), and a multipart upload holds
/// the envelope and the extracted file at once. A genuinely larger upload
/// needs those adapters to hand the transport's byte stream to
/// `wafer_run::InputStream::from_stream`, mapping every read error to an `Err`
/// item, and the files block to store it without collecting it first.
///
/// # Cross-repo coupling
///
/// The native half of this number is not ours: `wafer-block-http-listener`
/// owns `max_body_bytes` and its default, and that default is a **private**
/// const in that crate, reachable only through the `ConfigVar` the block
/// declares. If wafer-run raises it and this stays put, the clamp above would
/// enforce *below* what native accepts.
/// `impresspress-native/tests/transport_body_cap.rs` reads the block's own
/// declared default and fails if the two part company.
pub const MAX_REQUEST_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Meta key an adapter sets (value [`BODY_TOO_LARGE_VALUE`]) on a message
/// whose request body exceeded [`MAX_REQUEST_BODY_BYTES`].
///
/// The adapter hands the runtime this marker and an **empty** body rather than
/// building a 413 itself: a response built outside the flow carries neither
/// the CORS and security headers `wafer-run/cors` and
/// `wafer-run/security-headers` put on the message nor a `request_logs` row.
///
/// [`crate::blocks::body_limit`] — a flow step ahead of the router — turns the
/// marker into that 413, so the refusal does not depend on which route the
/// request would have matched. `pipeline::handle_request` checks it too, first
/// thing, for a consumer flow that dispatches to the router without that step.
pub const META_REQ_BODY_TOO_LARGE: &str = "req.body_too_large";

/// The value [`META_REQ_BODY_TOO_LARGE`] carries.
pub const BODY_TOO_LARGE_VALUE: &str = "1";

/// True when an adapter marked this message's body as over
/// [`MAX_REQUEST_BODY_BYTES`].
pub fn body_too_large(msg: &wafer_run::Message) -> bool {
    msg.get_meta(META_REQ_BODY_TOO_LARGE) == BODY_TOO_LARGE_VALUE
}

/// The error message the 413 carries when a request body exceeds
/// [`MAX_REQUEST_BODY_BYTES`]. Shared so the number a client is told is the
/// number that was enforced.
pub fn request_too_large_message() -> String {
    format!("request body too large (limit {MAX_REQUEST_BODY_BYTES} bytes)")
}

/// Outcome of draining a buffered response under a size cap.
pub enum CappedCollect {
    /// The stream ended within the cap: exactly what
    /// `OutputStream::collect_buffered` returns for it.
    Terminal(Result<BufferedResponse, TerminalNotResponse>),
    /// The accumulated body exceeded the cap before a terminal arrived. The
    /// adapter maps this to HTTP 413 rather than assembling the whole body.
    OverLimit,
}

/// `OutputStream::collect_buffered`, stopping with
/// [`CappedCollect::OverLimit`] as soon as the accumulated body would exceed
/// `cap` — the chunk that would cross it is not copied, and nothing after it
/// is read.
///
/// Within the cap the outcome is `collect_buffered`'s, terminal for terminal,
/// so a wasm adapter renders it through
/// `http_codec::terminal_to_http_response` exactly as the native listener
/// renders the same stream through `http_codec::collect_http_response`. That
/// includes a `Halt` arriving after a `Chunk` or `Meta`, which is a protocol
/// violation and an `Error`, not the halt's payload.
pub async fn collect_capped(mut output: OutputStream, cap: usize) -> CappedCollect {
    let mut body: Vec<u8> = Vec::new();
    let mut meta: Vec<MetaEntry> = Vec::new();
    let mut any_body_event = false;
    while let Some(event) = output.next().await {
        if matches!(event, StreamEvent::Chunk(_) | StreamEvent::Meta(_)) {
            any_body_event = true;
        }
        match event {
            StreamEvent::Chunk(bytes) => {
                if body.len().saturating_add(bytes.len()) > cap {
                    return CappedCollect::OverLimit;
                }
                if body.is_empty() {
                    body = bytes;
                } else {
                    body.extend_from_slice(&bytes);
                }
            }
            StreamEvent::Meta(entry) => meta.push(entry),
            terminal => {
                // Everything collected so far goes back in front of the
                // terminal, so `collect_buffered` decides the outcome — one
                // implementation of the terminal rules, not two.
                // `any_body_event` is false only when nothing came before the
                // terminal, so the prelude is then the terminal alone.
                let mut prelude = Vec::with_capacity(meta.len() + 2);
                if any_body_event {
                    prelude.extend(meta.into_iter().map(StreamEvent::Meta));
                    prelude.push(StreamEvent::Chunk(body));
                }
                prelude.push(terminal);
                output.prepend(prelude);
                return CappedCollect::Terminal(output.collect_buffered().await);
            }
        }
    }
    CappedCollect::Terminal(Err(TerminalNotResponse::Malformed))
}

/// Build the leading response-meta for a streamed download: the
/// `resp.stream` opt-in marker ([`http_codec::META_RESP_STREAM`]), the
/// object's `Content-Type`, and any
/// extra response headers (e.g. `Content-Disposition`, `Cache-Control`). All
/// entries are emitted as leading meta so the adapter applies them before the
/// first body byte flushes.
pub fn download_leading_meta(content_type: &str, extra_headers: &[(&str, &str)]) -> Vec<MetaEntry> {
    let mut meta = Vec::with_capacity(2 + extra_headers.len());
    meta.push(MetaEntry {
        key: http_codec::META_RESP_STREAM.to_string(),
        value: http_codec::STREAM_MARKER_VALUE.to_string(),
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
/// (headers first) and the object's size as `resp.content_length`
/// ([`META_RESP_CONTENT_LENGTH`]), then forward the object body chunk-by-chunk
/// from `stream` without ever buffering the whole object.
///
/// The declared length is the size the backend reported for this very read,
/// so every transport sends `Content-Length` — a client sees the download's
/// size and progress, and a `HEAD` its length — and the codec's body enforces
/// it: a body that ends short of it or runs past it is aborted, never ended
/// cleanly. A body-read failure is surfaced as an `Error` terminal after
/// whatever bytes already streamed (no silent truncation reported as a clean
/// `Complete`). A dropped consumer aborts the upstream read promptly via the
/// paired cancellation token.
pub fn stream_download(
    stream: NativeStorageGetStream,
    mut leading_meta: Vec<MetaEntry>,
) -> OutputStream {
    // A backend reporting no size (a negative one) declares no length; the
    // body then streams chunked, as before.
    if let Ok(size) = u64::try_from(stream.info().size) {
        leading_meta.push(MetaEntry {
            key: META_RESP_CONTENT_LENGTH.to_string(),
            value: size.to_string(),
        });
    }
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
            let Some(next) = cancel.run_until_cancelled(stream.next()).await else {
                return;
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

/// Splits a streamed upload body into fixed-size parts for a chunked /
/// multipart object-storage upload (e.g. Cloudflare R2 multipart).
///
/// Object stores that can't accept an unbounded [`InputStream`] directly —
/// R2's `put` rejects a `ReadableStream` of unknown length — instead take the
/// body as a sequence of uniform parts (S3-style multipart). This accumulator
/// buffers arriving byte chunks and hands back **exactly `part_size`-byte**
/// parts as soon as enough bytes are available, then yields whatever remains
/// (0..`part_size` bytes) as the final part via [`finish`](Self::finish). That
/// invariant matches the object-store rule that every part except the last be
/// the same size (and above the store's minimum), so the caller can upload
/// each [`take_part`](Self::take_part) result as a non-final part and the
/// [`finish`](Self::finish) tail as the last part.
///
/// Peak memory is bounded by one part plus one arriving chunk — never the whole
/// object — which is exactly what streaming the upload is meant to buy over
/// buffering the body whole. The byte order of the concatenated parts followed
/// by the tail is identical to the concatenation of every pushed chunk (no
/// bytes reordered, dropped, or duplicated); this is what the unit tests pin.
///
/// This is pure, allocation-only logic with no platform or async dependency,
/// so it is host-unit-tested here and reused verbatim by the wasm-only
/// Cloudflare R2 adapter, which cannot itself be unit-tested off-Worker.
#[derive(Debug)]
pub struct UploadPartBuffer {
    part_size: usize,
    buf: Vec<u8>,
}

impl UploadPartBuffer {
    /// Create an accumulator that emits `part_size`-byte parts.
    ///
    /// # Panics
    /// Panics if `part_size` is zero — a zero-size part can never fill, so the
    /// accumulator would buffer the entire object and defeat its own purpose.
    pub fn new(part_size: usize) -> Self {
        assert!(part_size > 0, "part_size must be non-zero");
        Self {
            part_size,
            buf: Vec::new(),
        }
    }

    /// Append a body chunk. Bytes are held until they can be drained as
    /// full parts via [`take_part`](Self::take_part).
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// If at least one full part is buffered, drain and return exactly
    /// `part_size` bytes; otherwise return `None`. Call in a loop after each
    /// [`push`](Self::push) to flush every full part that a chunk completed.
    pub fn take_part(&mut self) -> Option<Vec<u8>> {
        if self.buf.len() >= self.part_size {
            Some(self.buf.drain(..self.part_size).collect())
        } else {
            None
        }
    }

    /// Bytes currently buffered but not yet drained as a full part. Always
    /// strictly less than `part_size` once [`take_part`](Self::take_part) has
    /// been drained to `None`.
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// Consume the accumulator, returning the trailing bytes that never filled
    /// a whole part — the final part of the upload. May be empty when the total
    /// body length is an exact multiple of `part_size` (the caller then
    /// completes the upload with the parts already drained, adding no empty
    /// final part).
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use wafer_block::http_codec::{ResponseMetaPart, META_RESP_STREAM, STREAM_MARKER_VALUE};
    use wafer_run::{ErrorCode, Message, MetaGet, WaferError, META_RESP_STATUS};

    use super::*;

    fn meta(key: &str, value: &str) -> MetaEntry {
        MetaEntry {
            key: key.to_string(),
            value: value.to_string(),
        }
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
        assert_eq!(
            MetaGet::get(&m, META_RESP_STREAM),
            Some(STREAM_MARKER_VALUE)
        );
        assert!(
            http_codec::declares_streaming(&m),
            "the codec opens a download as a streamed response"
        );
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
                .expect("the download meta is sendable")
                .iter()
                .all(|p| !matches!(p, ResponseMetaPart::Header { name, .. } if *name == "stream")),
            "the streaming marker must never surface as a response header"
        );
    }

    #[tokio::test]
    async fn collect_capped_reports_over_limit_before_buffering_whole_body() {
        // Three 4-byte chunks = 12 bytes; cap = 5. Must bail at OverLimit.
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_chunk(b"aaaa".to_vec()).await.ok();
            sink.send_chunk(b"bbbb".to_vec()).await.ok();
            sink.send_chunk(b"cccc".to_vec()).await.ok();
            sink.complete(vec![]).await.ok();
        });
        assert!(matches!(
            collect_capped(stream, 5).await,
            CappedCollect::OverLimit
        ));
    }

    /// A body exactly at the cap is admitted; the refusal is `>`, not `>=`.
    #[tokio::test]
    async fn collect_capped_admits_a_body_exactly_at_the_cap() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            sink.send_chunk(b"aaa".to_vec()).await.ok();
            sink.send_chunk(b"bb".to_vec()).await.ok();
            sink.complete(vec![]).await.ok();
        });
        match collect_capped(stream, 5).await {
            CappedCollect::Terminal(Ok(buf)) => assert_eq!(buf.body, b"aaabb"),
            _ => panic!("a body at the cap must be collected"),
        }
    }

    /// A named way to build one stream, so each can be built twice.
    type Shape = (&'static str, fn() -> OutputStream);

    /// The streams below, built fresh for each call: every terminal kind,
    /// with and without leading meta and body before it.
    fn shapes() -> Vec<Shape> {
        vec![
            ("respond_with_meta", || {
                OutputStream::respond_with_meta(
                    b"hello".to_vec(),
                    vec![meta(META_RESP_STATUS, "201")],
                )
            }),
            ("leading meta, chunks, complete", || {
                OutputStream::from_producer(|sink, _cancel| async move {
                    sink.send_meta(meta(META_RESP_CONTENT_TYPE, "text/plain"))
                        .await
                        .ok();
                    sink.send_chunk(b"one ".to_vec()).await.ok();
                    sink.send_meta(meta("resp.header.X-Late", "1")).await.ok();
                    sink.send_chunk(b"two".to_vec()).await.ok();
                    sink.complete(vec![meta(META_RESP_STATUS, "202")])
                        .await
                        .ok();
                })
            }),
            ("error", || {
                OutputStream::error(WaferError::new(ErrorCode::NotFound, "gone"))
            }),
            ("halt alone", || {
                OutputStream::halt(b"halted".to_vec(), vec![meta(META_RESP_STATUS, "429")])
            }),
            ("halt after leading meta", || {
                let mut out =
                    OutputStream::halt(b"halted".to_vec(), vec![meta(META_RESP_STATUS, "429")]);
                out.prepend([StreamEvent::Meta(meta(META_RESP_CONTENT_TYPE, "text/html"))]);
                out
            }),
            ("halt after an empty chunk", || {
                let mut out = OutputStream::halt(b"halted".to_vec(), Vec::new());
                out.prepend([StreamEvent::Chunk(Vec::new())]);
                out
            }),
            ("drop", || {
                OutputStream::drop_request_with_meta(vec![meta(
                    "resp.header.access-control-allow-origin",
                    "https://a.example",
                )])
            }),
            ("continue", || {
                OutputStream::continue_with(Message::new("next"))
            }),
            ("producer stops without a terminal", || {
                OutputStream::from_producer(|sink, _cancel| async move {
                    sink.send_chunk(b"partial".to_vec()).await.ok();
                })
            }),
        ]
    }

    /// Within the cap the capped collector is `collect_buffered`, terminal
    /// for terminal — including a `Halt` after body events, which
    /// `collect_buffered` answers as a protocol-violation `Error` where the
    /// old impresspress collector served the halt's payload. So a wasm
    /// adapter renders a buffered response exactly as the native listener's
    /// `collect_http_response` does.
    #[tokio::test]
    async fn collect_capped_within_the_cap_is_collect_buffered() {
        for (shape, build) in shapes() {
            let CappedCollect::Terminal(capped) =
                collect_capped(build(), MAX_BUFFERED_RESPONSE_BYTES).await
            else {
                panic!("{shape}: over the cap");
            };
            let buffered = build().collect_buffered().await;
            assert_eq!(
                format!("{capped:?}"),
                format!("{buffered:?}"),
                "{shape}: the capped collector must equal collect_buffered"
            );
            assert_eq!(
                http_codec::terminal_to_http_response(capped),
                http_codec::collect_http_response(build()).await,
                "{shape}: and render as the listener renders it"
            );
        }
    }

    // --- UploadPartBuffer ---

    /// Drive `chunks` through the accumulator exactly as the R2 adapter does —
    /// push each chunk, drain every full part it completed, then `finish` for
    /// the tail — and return `(full_parts, tail)`.
    fn split(part_size: usize, chunks: &[&[u8]]) -> (Vec<Vec<u8>>, Vec<u8>) {
        let mut acc = UploadPartBuffer::new(part_size);
        let mut parts = Vec::new();
        for chunk in chunks {
            acc.push(chunk);
            while let Some(part) = acc.take_part() {
                parts.push(part);
            }
        }
        (parts, acc.finish())
    }

    /// Every non-final part is exactly `part_size`; the tail is strictly
    /// smaller; and parts-then-tail concatenate back to the input verbatim.
    fn assert_split_invariants(part_size: usize, chunks: &[&[u8]]) {
        let (parts, tail) = split(part_size, chunks);
        for part in &parts {
            assert_eq!(part.len(), part_size, "non-final parts must be uniform");
        }
        assert!(tail.len() < part_size, "tail must be a partial part");
        let mut rebuilt = Vec::new();
        for part in &parts {
            rebuilt.extend_from_slice(part);
        }
        rebuilt.extend_from_slice(&tail);
        let original: Vec<u8> = chunks.concat();
        assert_eq!(
            rebuilt, original,
            "no byte reordered, dropped, or duplicated"
        );
    }

    #[test]
    fn part_buffer_empty_stream_yields_no_parts_and_empty_tail() {
        let (parts, tail) = split(4, &[]);
        assert!(parts.is_empty());
        assert!(tail.is_empty());
    }

    #[test]
    fn part_buffer_small_body_stays_in_tail() {
        // Under one part → buffered put path (no parts emitted).
        let (parts, tail) = split(4, &[b"ab", b"c"]);
        assert!(parts.is_empty());
        assert_eq!(tail, b"abc");
    }

    #[test]
    fn part_buffer_exact_part_emits_one_and_empties_tail() {
        let (parts, tail) = split(4, &[b"abcd"]);
        assert_eq!(parts, vec![b"abcd".to_vec()]);
        assert!(tail.is_empty(), "exact multiple leaves no final part");
    }

    #[test]
    fn part_buffer_single_oversized_chunk_splits_into_uniform_parts() {
        // 2.5 parts in a single push → two full parts + a half-part tail.
        let (parts, tail) = split(4, &[b"aabbccdde"]);
        assert_eq!(parts, vec![b"aabb".to_vec(), b"ccdd".to_vec()]);
        assert_eq!(tail, b"e");
    }

    #[test]
    fn part_buffer_many_small_chunks_span_boundaries() {
        // Chunks that don't align to part boundaries still repack uniformly.
        let chunks: &[&[u8]] = &[b"a", b"bc", b"def", b"gh", b"ijkl", b"m"];
        assert_split_invariants(4, chunks);
        let (parts, tail) = split(4, chunks);
        assert_eq!(
            parts,
            vec![b"abcd".to_vec(), b"efgh".to_vec(), b"ijkl".to_vec()]
        );
        assert_eq!(tail, b"m");
    }

    #[test]
    fn part_buffer_exact_multiple_across_chunks_has_empty_tail() {
        let chunks: &[&[u8]] = &[b"abc", b"def", b"ghi", b"jkl"];
        assert_split_invariants(4, chunks);
        let (parts, tail) = split(4, chunks);
        assert_eq!(parts.len(), 3, "12 bytes / 4 = 3 full parts");
        assert!(tail.is_empty());
    }

    #[test]
    fn part_buffer_take_part_drains_one_full_part_at_a_time() {
        // A chunk worth three parts must be drained part-by-part, not at once.
        let mut acc = UploadPartBuffer::new(2);
        acc.push(b"aabbcc");
        assert_eq!(acc.take_part(), Some(b"aa".to_vec()));
        assert_eq!(acc.buffered_len(), 4);
        assert_eq!(acc.take_part(), Some(b"bb".to_vec()));
        assert_eq!(acc.take_part(), Some(b"cc".to_vec()));
        assert_eq!(acc.take_part(), None);
        assert!(acc.finish().is_empty());
    }

    #[test]
    #[should_panic(expected = "part_size must be non-zero")]
    fn part_buffer_zero_part_size_panics() {
        UploadPartBuffer::new(0);
    }
}
