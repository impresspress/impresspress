//! HTTP ↔ Message conversion for Cloudflare Workers.
//!
//! Thin platform glue: the protocol mapping (method→action table, request
//! meta layout, the streaming decision, how a response's head and body are
//! rendered) lives in `wafer_block::http_codec` — the same implementation the
//! native listener, the request pipeline and the browser adapter use. Only
//! worker-type I/O lives here: reading the request body/headers and building
//! the Worker `Response` (buffered, or `ReadableStream`-backed).

use futures::StreamExt;
use impresspress_core::streaming::{self, CappedCollect};
use wafer_block::http_codec::{self, HttpResponseParts, OpenedResponse, StreamedHttpResponse};
use wafer_run::{InputStream, Message, OutputStream};
use worker::{FixedLengthStream, Headers, Request, Response, ResponseBuilder, Result};

// ---------------------------------------------------------------------------
// Request conversion
// ---------------------------------------------------------------------------

/// Convert a Cloudflare Worker Request into a WAFER `(Message, InputStream)`.
///
/// The path is passed through exactly as received. Nothing rewrites it: the
/// `/api` prefix this adapter used to strip was never honoured by the
/// site-main flow's router (which matches `/b/**`, `/health`, `/openapi.json`,
/// `/.well-known/agent.json`, `/` and falls the rest through to
/// `wafer-run/web`), so stripping here quietly re-pointed paths only this
/// transport could serve.
///
/// A body over [`streaming::MAX_REQUEST_BODY_BYTES`] is **not** read and not
/// dispatched: the message is marked with
/// [`streaming::META_REQ_BODY_TOO_LARGE`] and paired with an empty
/// `InputStream`, and `impresspress_core::pipeline::handle_request` answers
/// 413 from inside the flow — where the CORS and security headers and the
/// `request_logs` row are. Returning a `worker::Error` here is what made an
/// oversized upload an opaque 500 with a correlation id; building the
/// `Response` here instead would drop the headers and the audit row.
pub async fn worker_request_to_message(req: &Request) -> Result<(Message, InputStream)> {
    let method = req.method().to_string();
    let url = req.url()?;
    let path = url.path().to_string();
    let query = url.query().unwrap_or("").to_string();

    // A declared Content-Length over the cap is refused before the body is
    // read, so a well-formed oversized upload never enters the (128 MB)
    // isolate at all. The post-read check below is the backstop for chunked /
    // absent-length requests, where the header cannot be trusted and the bytes
    // are resident by the time they can be measured.
    let declared_too_large = req
        .headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|len| len > streaming::MAX_REQUEST_BODY_BYTES);

    // Read the body unless the declared length already refused it. A read
    // error would otherwise be swallowed and turned into an empty body,
    // silently corrupting POST/PUT.
    let body = if declared_too_large {
        Vec::new()
    } else {
        let mut req_clone = req.clone()?;
        req_clone.bytes().await?
    };
    let too_large = declared_too_large || body.len() > streaming::MAX_REQUEST_BODY_BYTES;

    // Extract remote address
    let remote_addr = req
        .headers()
        .get("cf-connecting-ip")
        .ok()
        .flatten()
        .or_else(|| req.headers().get("x-forwarded-for").ok().flatten())
        .unwrap_or_else(|| "unknown".to_string());

    let mut msg =
        http_codec::build_http_message(&method, &path, &query, &remote_addr, req.headers());

    if too_large {
        msg.set_meta(
            streaming::META_REQ_BODY_TOO_LARGE,
            streaming::BODY_TOO_LARGE_VALUE,
        );
        return Ok((msg, InputStream::empty()));
    }

    Ok((msg, InputStream::from_bytes(body)))
}

// ---------------------------------------------------------------------------
// Response conversion
// ---------------------------------------------------------------------------

/// Convert a WAFER `OutputStream` into a Cloudflare Worker `Response`.
///
/// `http_codec::open_response` decides, as it does for every transport:
///
/// 1. **Streamed** — the producer declared streaming up front in leading
///    `Meta` (the `resp.stream` marker or a streaming content type: a file
///    download, an SSE response) and has its first bytes. The codec has
///    rendered the head; the body is piped chunk by chunk into the Worker
///    `Response`'s native `ReadableStream`, so the object never sits in the
///    isolate whole. See [`streamed_response`].
/// 2. **Buffered** (everything else: SSR pages, JSON, an empty stream, a
///    streaming declaration the codec refused at its head). The body is
///    drained under [`streaming::MAX_BUFFERED_RESPONSE_BYTES`]; an over-limit
///    body becomes **HTTP 413** (not a generic 500 / an isolate OOM), and
///    everything within the cap is rendered by
///    `http_codec::terminal_to_http_response`, the native listener's
///    renderer.
pub async fn output_to_response(output: OutputStream) -> Result<Response> {
    match http_codec::open_response(output).await {
        OpenedResponse::Streamed(streamed) => streamed_response(streamed),
        OpenedResponse::Buffered(output) => {
            match streaming::collect_capped(output, streaming::MAX_BUFFERED_RESPONSE_BYTES).await {
                // The body would have exceeded the isolate buffering cap — a
                // clean 413 instead of assembling it whole (which the CF
                // runtime would reject as an opaque error).
                CappedCollect::OverLimit => over_limit_response(),
                CappedCollect::Terminal(terminal) => {
                    parts_to_response(http_codec::terminal_to_http_response(terminal))
                }
            }
        }
    }
}

/// A streaming Worker `Response`: the status and headers the codec rendered
/// from the leading meta, committed before the body finishes, and the body
/// piped into the Worker's native `ReadableStream`.
///
/// Every ending the codec's `StreamedBody` frames as an `Err` — an `Error`
/// terminal (a body-read failure, or a producer that stopped without one),
/// no terminal at all, a `Halt`/`Drop`/`Continue` after the body started, a
/// body that breaks its declared length — surfaces as a stream error that
/// aborts the response body rather than a silent truncation: the status is
/// already committed, so it cannot be downgraded to 413 or 500.
///
/// A declared length (`resp.content_length`, a stored object's size) goes
/// through a `FixedLengthStream`, which is how Workers sends `Content-Length`
/// on a streamed body: a `Content-Length` header set by hand on a
/// `ReadableStream` response is not what Workers frames the body by.
fn streamed_response(streamed: StreamedHttpResponse) -> Result<Response> {
    let StreamedHttpResponse {
        status,
        headers,
        content_length,
        body,
    } = streamed;
    let builder = ResponseBuilder::new()
        .with_status(status)
        .with_headers(headers_from(&headers)?);
    let body = body.map(|chunk| chunk.map_err(|e| worker::Error::RustError(e.message)));
    match content_length {
        None => builder.from_stream(body),
        Some(length) => {
            let fixed =
                worker::worker_sys::FixedLengthStream::from(FixedLengthStream::wrap(body, length));
            Ok(builder.stream(fixed.readable()))
        }
    }
}

/// Header pairs, as the codec renders them, in a Worker `Headers`. Appended
/// in application order: a rendering may legitimately repeat a name
/// (`Set-Cookie`).
fn headers_from(pairs: &[(String, String)]) -> Result<Headers> {
    let headers = Headers::new();
    for (name, value) in pairs {
        headers.append(name, value)?;
    }
    Ok(headers)
}

/// A 413 Payload Too Large response for an over-limit buffered body.
fn over_limit_response() -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", "text/plain; charset=utf-8")?;
    Ok(ResponseBuilder::new()
        .with_status(413)
        .with_headers(headers)
        .fixed(b"payload too large".to_vec()))
}

/// Apply transport-neutral [`HttpResponseParts`] to the Worker types. Headers
/// are appended in application order (`headers` may legitimately repeat a name,
/// e.g. `Set-Cookie`).
fn parts_to_response(parts: HttpResponseParts) -> Result<Response> {
    Ok(Response::from_bytes(parts.body)?
        .with_status(parts.status)
        .with_headers(headers_from(&parts.headers)?))
}

/// Request-conversion tests: the transport body cap and the path pass-through.
///
/// They build a real `worker::Request` (a `web_sys::Request`, which Node ≥18
/// provides) and run it through the real `worker_request_to_message`, so they
/// exercise the header pre-check, the post-read backstop and the meta the
/// flow then routes on. They need no `worker::Env`, so they run under the
/// `cloudflare-wasm-test` job like the rest of this crate's wasm tests.
///
/// What the marked message then *becomes* is `impresspress-core`'s: a 413
/// through the flow, pinned by `pipeline`'s `oversized_body_tests` and
/// `tests/oversized_body_flow.rs` on the host.
#[cfg(all(test, target_arch = "wasm32"))]
mod request_tests {
    use impresspress_core::streaming::{
        body_too_large, BODY_TOO_LARGE_VALUE, MAX_REQUEST_BODY_BYTES, META_REQ_BODY_TOO_LARGE,
    };
    use wafer_block::meta::META_REQ_RESOURCE;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_test::wasm_bindgen_test;
    use worker::{Method, RequestInit};

    use super::{worker_request_to_message, InputStream, Message, Request};

    /// A POST whose body is `len` bytes of zeroes.
    fn post_with_body(url: &str, len: usize) -> Request {
        let body = js_sys::Uint8Array::new_with_length(len as u32);
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_body(Some(JsValue::from(body)));
        Request::new_with_init(url, &init).expect("build request")
    }

    async fn convert(req: &Request) -> (Message, InputStream) {
        worker_request_to_message(req).await.expect("convert")
    }

    /// Bytes the returned `InputStream` carries.
    async fn drain(input: InputStream) -> Vec<u8> {
        input
            .collect_to_bytes()
            .await
            .expect("an in-memory body does not fail")
    }

    /// **Fails on the pre-fix tree**, where an over-cap body returned
    /// `Err("request body too large")` — a `worker::Error` the `run` entry
    /// point's catch-all turns into a 500 with a correlation id, telling the
    /// uploader nothing and an operator to go read the isolate log for what is
    /// not an internal error at all. It is now a marked message the flow
    /// answers 413 for, and the body is dropped rather than forwarded.
    #[wasm_bindgen_test]
    async fn an_over_cap_body_is_marked_and_not_forwarded() {
        let req = post_with_body(
            "https://example.test/b/storage/api/buckets/photos/objects?key=big.bin",
            MAX_REQUEST_BODY_BYTES + 1,
        );
        let (msg, input) = convert(&req).await;

        assert!(body_too_large(&msg), "the marker the pipeline refuses on");
        assert_eq!(msg.get_meta(META_REQ_BODY_TOO_LARGE), BODY_TOO_LARGE_VALUE);
        assert!(
            drain(input).await.is_empty(),
            "an oversized body must not reach a block"
        );
        assert_eq!(
            msg.get_meta(META_REQ_RESOURCE),
            "/b/storage/api/buckets/photos/objects",
            "the refusal still routes as the request it was, so it is logged as one",
        );
    }

    /// A body exactly at the cap is admitted, whole — the refusal is `>`, not
    /// `>=`, and the boundary is the one the files block's clamped quota
    /// reports.
    #[wasm_bindgen_test]
    async fn a_body_at_the_cap_is_admitted_whole() {
        let req = post_with_body(
            "https://example.test/b/storage/api/buckets/photos/objects?key=big.bin",
            MAX_REQUEST_BODY_BYTES,
        );
        let (msg, input) = convert(&req).await;

        assert!(!body_too_large(&msg));
        assert_eq!(drain(input).await.len(), MAX_REQUEST_BODY_BYTES);
    }

    /// **Fails on the pre-fix tree**: this adapter rewrote the path, stripping
    /// `/api`, so a path the flow router would have served from the SPA
    /// fallback was re-pointed at the API on this transport alone — and
    /// `/apiary` lost its first four bytes, becoming `ary`.
    #[wasm_bindgen_test]
    async fn the_path_reaches_the_message_exactly_as_sent() {
        for path in ["/api/b/storage", "/api/api/x", "/apiary/hives", "/api"] {
            let req = post_with_body(&format!("https://example.test{path}"), 0);
            let (msg, _) = convert(&req).await;
            assert_eq!(
                msg.get_meta(META_REQ_RESOURCE),
                path,
                "the adapter must not rewrite paths",
            );
        }
    }
}

/// Response-conversion tests: what a client reads from an error.
///
/// They run the real `output_to_response` over the error a block answers with
/// and read the `worker::Response` back, so they cover the codec call and the
/// `parts_to_response` glue together.
#[cfg(all(test, target_arch = "wasm32"))]
mod response_tests {
    use impresspress_core::blocks::errors::{error_response, ErrorCode};
    use wafer_block::{
        http_codec::{META_RESP_CONTENT_LENGTH, META_RESP_STREAM, STREAM_MARKER_VALUE},
        meta::META_RESP_CONTENT_TYPE,
        MetaEntry,
    };
    use wafer_run::{ErrorCode as WaferErrorCode, OutputStream, WaferError};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::output_to_response;

    /// A streamed response whose leading meta holds a header value no
    /// transport can send (a CR/LF, here) is answered with the codec's 500
    /// before a status or a byte goes out — never streamed without the entry,
    /// and never with it. Once the body streams, a failure can only abort it.
    ///
    /// The 500 keeps the terminal's sendable security headers and drops the
    /// ones describing the body it replaces, so the error page is served
    /// under the same frame policy as the page it stands in for.
    #[wasm_bindgen_test]
    async fn a_stream_with_an_unsendable_header_is_a_500_before_it_starts() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            for (key, value) in [
                (META_RESP_STREAM, STREAM_MARKER_VALUE),
                (META_RESP_CONTENT_TYPE, "application/pdf"),
                ("resp.header.X-Frame-Options", "DENY"),
                ("resp.header.Content-Disposition", "inline\r\nX-Injected: 1"),
            ] {
                let _ = sink
                    .send_meta(MetaEntry {
                        key: key.to_string(),
                        value: value.to_string(),
                    })
                    .await;
            }
            let _ = sink.send_chunk(b"%PDF-1.7".to_vec()).await;
            let _ = sink.complete(Vec::new()).await;
        });

        let mut resp = output_to_response(stream).await.expect("build response");

        assert_eq!(resp.status_code(), 500);
        assert_eq!(
            resp.headers().get("content-disposition").expect("headers"),
            None,
            "the refused header is never sent"
        );
        assert_eq!(
            resp.headers()
                .get("x-frame-options")
                .expect("headers")
                .as_deref(),
            Some("DENY"),
            "the terminal's security headers survive on its 500"
        );
        assert_eq!(
            resp.headers()
                .get("cache-control")
                .expect("headers")
                .as_deref(),
            Some("no-store"),
            "the 500 must not be cached"
        );
        let body: serde_json::Value =
            serde_json::from_str(&resp.text().await.expect("read body")).expect("a JSON body");
        assert_eq!(body["error"], "Internal");
    }

    /// A streamed body is aborted, not ended, when it fails part-way: by
    /// then the status is committed, so the aborted body is the only signal
    /// that the bytes are not whole. Two ways a body fails: an `Error`
    /// terminal, and a producer that stops without one, which wafer-run ends
    /// with an `Error` of its own.
    #[wasm_bindgen_test]
    async fn a_body_that_fails_part_way_is_aborted_not_ended() {
        for explicit_error in [true, false] {
            let stream = OutputStream::from_producer(move |sink, _cancel| async move {
                for (key, value) in [
                    (META_RESP_STREAM, STREAM_MARKER_VALUE),
                    (META_RESP_CONTENT_TYPE, "application/pdf"),
                ] {
                    let _ = sink
                        .send_meta(MetaEntry {
                            key: key.to_string(),
                            value: value.to_string(),
                        })
                        .await;
                }
                let _ = sink.send_chunk(b"%PDF-1.7 first half".to_vec()).await;
                if explicit_error {
                    let _ = sink
                        .error(WaferError::new(
                            WaferErrorCode::Unavailable,
                            "object read failed",
                        ))
                        .await;
                }
            });

            let mut resp = output_to_response(stream).await.expect("build response");

            assert_eq!(resp.status_code(), 200);
            assert!(
                resp.bytes().await.is_err(),
                "a truncated body must not read back as complete (explicit error: {explicit_error})"
            );
        }
    }

    /// The same path without a failure delivers every byte.
    #[wasm_bindgen_test]
    async fn a_streamed_body_delivers_every_chunk() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            for (key, value) in [
                (META_RESP_STREAM, STREAM_MARKER_VALUE),
                (META_RESP_CONTENT_TYPE, "application/pdf"),
            ] {
                let _ = sink
                    .send_meta(MetaEntry {
                        key: key.to_string(),
                        value: value.to_string(),
                    })
                    .await;
            }
            let _ = sink.send_chunk(b"one ".to_vec()).await;
            let _ = sink.send_chunk(b"two".to_vec()).await;
            let _ = sink.complete(Vec::new()).await;
        });

        let mut resp = output_to_response(stream).await.expect("build response");

        assert_eq!(resp.text().await.expect("read body"), "one two");
    }

    /// Node has no Workers `FixedLengthStream`. This stands one in — a
    /// `TransformStream` that counts its constructions and keeps the length
    /// it was made with — so the adapter's wiring can be read back. What
    /// Workers itself does with the length (the `Content-Length` it sends)
    /// is the runtime's, and not observable here.
    fn install_fixed_length_stream_stand_in() {
        js_sys::Function::new_no_args(
            "if (!globalThis.__fixedLengths) { \
                 globalThis.__fixedLengths = []; \
                 globalThis.FixedLengthStream = class extends TransformStream { \
                     constructor(length) { super(); globalThis.__fixedLengths.push(Number(length)); } \
                 }; \
             }",
        )
        .call0(&wasm_bindgen::JsValue::UNDEFINED)
        .expect("install the stand-in");
    }

    /// The lengths every `FixedLengthStream` so far was constructed with.
    fn fixed_lengths() -> Vec<f64> {
        let lengths = js_sys::Reflect::get(&js_sys::global(), &"__fixedLengths".into())
            .expect("the stand-in is installed");
        js_sys::Array::from(&lengths)
            .iter()
            .map(|v| v.as_f64().expect("a number"))
            .collect()
    }

    /// A download that declares its length (`resp.content_length`, the
    /// stored object's size) is framed by a `FixedLengthStream` of exactly
    /// that length — how Workers sends `Content-Length` on a streamed body —
    /// and delivers every byte. One that declares none streams without it.
    #[wasm_bindgen_test]
    async fn a_declared_length_frames_the_streamed_body_with_a_fixed_length_stream() {
        install_fixed_length_stream_stand_in();
        let download = |length: Option<&'static str>| {
            OutputStream::from_producer(move |sink, _cancel| async move {
                let mut leading = vec![
                    (META_RESP_STREAM, STREAM_MARKER_VALUE),
                    (META_RESP_CONTENT_TYPE, "application/pdf"),
                ];
                if let Some(length) = length {
                    leading.push((META_RESP_CONTENT_LENGTH, length));
                }
                for (key, value) in leading {
                    let _ = sink
                        .send_meta(MetaEntry {
                            key: key.to_string(),
                            value: value.to_string(),
                        })
                        .await;
                }
                let _ = sink.send_chunk(b"abc".to_vec()).await;
                let _ = sink.send_chunk(b"def".to_vec()).await;
                let _ = sink.complete(Vec::new()).await;
            })
        };

        let before = fixed_lengths().len();
        let mut resp = output_to_response(download(Some("6")))
            .await
            .expect("build response");
        assert_eq!(resp.status_code(), 200);
        assert_eq!(
            fixed_lengths()[before..],
            [6.0],
            "the body is framed by a FixedLengthStream of the declared length"
        );
        assert_eq!(resp.text().await.expect("read body"), "abcdef");

        let before = fixed_lengths().len();
        let mut resp = output_to_response(download(None))
            .await
            .expect("build response");
        assert_eq!(
            fixed_lengths().len(),
            before,
            "no declared length, no FixedLengthStream"
        );
        assert_eq!(resp.text().await.expect("read body"), "abcdef");
    }

    /// A body that runs past its declared length is aborted, not ended: the
    /// codec's body yields an `Err` in place of the chunk that would overrun
    /// it, and the response body errors.
    #[wasm_bindgen_test]
    async fn a_body_overrunning_its_declared_length_is_aborted() {
        install_fixed_length_stream_stand_in();
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            for (key, value) in [
                (META_RESP_STREAM, STREAM_MARKER_VALUE),
                (META_RESP_CONTENT_TYPE, "application/pdf"),
                (META_RESP_CONTENT_LENGTH, "5"),
            ] {
                let _ = sink
                    .send_meta(MetaEntry {
                        key: key.to_string(),
                        value: value.to_string(),
                    })
                    .await;
            }
            let _ = sink.send_chunk(b"ab".to_vec()).await;
            let _ = sink.send_chunk(b"cdef".to_vec()).await;
            let _ = sink.complete(Vec::new()).await;
        });

        let mut resp = output_to_response(stream).await.expect("build response");

        assert_eq!(resp.status_code(), 200);
        assert!(
            resp.bytes().await.is_err(),
            "a body longer than it declared must not read back as complete"
        );
    }

    /// **Fails before wafer-run 9a080676**, whose codec rendered an error as
    /// `{"error", "message"}` only: an `errors::error_response` attaches its
    /// precise code with `with_detail_code`, and the JS SDK reads it from the
    /// body's `code` (`http-client.ts`), which this transport never sent.
    #[wasm_bindgen_test]
    async fn an_error_response_reaches_the_client_with_its_detail_code() {
        let mut resp = output_to_response(error_response(
            ErrorCode::NotAuthenticated,
            "Not authenticated",
        ))
        .await
        .expect("build response");

        assert_eq!(resp.status_code(), 401);
        let body: serde_json::Value =
            serde_json::from_str(&resp.text().await.expect("read body")).expect("a JSON body");
        assert_eq!(
            body,
            serde_json::json!({
                "error": "Unauthenticated",
                "message": "Not authenticated",
                "code": "not_authenticated",
            })
        );
    }
}

/// An event stream (`impresspress_core::sse`, what every SSE route produces)
/// through the real `output_to_response`, read back the way the Workers
/// runtime reads a `Response`.
///
/// The producer waits on an upstream that never answers — a model thinking,
/// a model load before its first progress — so every assertion holds while
/// the producer cannot have finished. Each wait is bounded by a JS timer,
/// which only turns a hang into a failure.
#[cfg(all(test, target_arch = "wasm32"))]
mod event_stream_tests {
    use futures::{channel::oneshot, StreamExt};
    use impresspress_core::sse::{event_stream, Pulled, OPEN_FRAME};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::output_to_response;
    use crate::test_fakes::bounded;

    /// The Worker `Response` carries the SSE head and the open frame while
    /// the producer still waits on its upstream, and a client that cancels
    /// the body ends the producer's read of that upstream: the body's
    /// `ReadableStream` drops the Rust stream it pulls, which cancels the
    /// producer.
    ///
    /// Fails without the head's cache and proxy headers, without the open
    /// frame (the response never resolves: the head waits for a first chunk
    /// that only the upstream could send), and with a read of the upstream
    /// that does not race the consumer (the producer never learns the client
    /// left).
    #[wasm_bindgen_test]
    async fn an_event_stream_opens_at_once_and_its_producer_stops_when_the_body_is_cancelled() {
        let (pulled_tx, pulled_rx) = oneshot::channel();
        let stream = event_stream(move |events| async move {
            let mut upstream = futures::stream::pending::<()>();
            let _ = pulled_tx.send(events.pull(&mut upstream).await);
        });

        let mut resp = bounded(Box::pin(output_to_response(stream)), "the response head")
            .await
            .expect("build response");
        assert_eq!(resp.status_code(), 200);
        for (name, value) in [
            ("content-type", "text/event-stream"),
            ("cache-control", "no-cache"),
            ("x-accel-buffering", "no"),
        ] {
            assert_eq!(
                resp.headers().get(name).expect("headers").as_deref(),
                Some(value),
                "the SSE head carries {name}"
            );
        }

        let mut body = resp.stream().expect("a streamed body");
        let first = bounded(body.next(), "the open frame")
            .await
            .expect("a first chunk")
            .expect("a readable chunk");
        assert_eq!(first, OPEN_FRAME);

        drop(body);
        assert_eq!(
            bounded(pulled_rx, "the producer's read of its upstream")
                .await
                .expect("the producer reports its read"),
            Pulled::ConsumerGone
        );
    }
}

/// Request-scope tests: a streamed body's producer and the request's
/// services.
///
/// On Cloudflare every service a block reaches is a proxy that looks up the
/// request being polled ([`crate::request_services`]); outside a poll of
/// that request's dispatch future it refuses the call. A producer handed to
/// `OutputStream::from_producer` runs on a task of its own, polled from the
/// JS microtask queue, and a streamed body is read after `dispatch` has
/// returned the `Response`. The isolate's spawn wrapper
/// (`request_services::wrap_spawned`) gives such a task the scope current
/// when it was spawned. These install it, spawn the producer the way a
/// block does — inside the scoped future — or outside any request, run the
/// real `output_to_response`, and read the body after the scope has
/// finished, the way the Workers runtime does.
#[cfg(all(test, target_arch = "wasm32"))]
mod request_scope_tests {
    use std::sync::Arc;

    use wafer_block::{meta::META_RESP_CONTENT_TYPE, MetaEntry};
    use wafer_core::interfaces::crypto::service::CryptoService;
    use wafer_run::OutputStream;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::output_to_response;
    use crate::{
        request_services::{
            self, wrap_spawned, RequestServices, ScopedCryptoService, SpawnWrapperGuard,
        },
        test_fakes::FixedCrypto,
    };

    /// One SSE frame saying whether a scoped service call reached the
    /// request's services.
    async fn scoped_call_frame(when: &str) -> Vec<u8> {
        match ScopedCryptoService.random_bytes(1).await {
            Ok(_) => format!("data: {when}: in scope\n\n").into_bytes(),
            Err(error) => format!("data: {when}: {error}\n\n").into_bytes(),
        }
    }

    /// An SSE producer that calls a scoped service before and after its
    /// first chunk — the second the shape of a chat stream storing its reply
    /// once the last token is out — and reports each call in a frame.
    fn sse_calling_a_service() -> OutputStream {
        OutputStream::from_producer(|sink, _cancel| async move {
            let _ = sink
                .send_meta(MetaEntry {
                    key: META_RESP_CONTENT_TYPE.to_string(),
                    value: "text/event-stream".to_string(),
                })
                .await;
            let before_head = scoped_call_frame("before the first chunk").await;
            let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
            let after_head = scoped_call_frame("after the first chunk").await;
            let _ = sink.send_chunk(before_head).await;
            let _ = sink.send_chunk(after_head).await;
            let _ = sink.complete(Vec::new()).await;
        })
    }

    async fn read_streamed(mut resp: worker::Response) -> String {
        assert_eq!(resp.status_code(), 200);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .expect("headers")
                .as_deref(),
            Some("text/event-stream"),
            "the response streams"
        );
        resp.text().await.expect("read body")
    }

    /// **Review finding 3 (SSE plan).** A producer spawned inside a request
    /// — as a block's `handle` spawns it — reaches that request's services
    /// on every poll, before its first chunk and after the scope has handed
    /// the `Response` back. Fails without the spawn wrapper: both calls are
    /// "used outside request poll scope".
    #[wasm_bindgen_test]
    async fn a_producer_spawned_in_its_request_reaches_its_services_after_its_first_chunk() {
        let _wrapper = SpawnWrapperGuard::install(Some(wrap_spawned));
        let resp = request_services::scope(
            RequestServices::marker_with_crypto(1, Arc::new(FixedCrypto)),
            async { output_to_response(sse_calling_a_service()).await },
        )
        .await
        .expect("build response");

        assert_eq!(
            read_streamed(resp).await,
            "data: one\n\n\
             data: before the first chunk: in scope\n\n\
             data: after the first chunk: in scope\n\n",
            "every service call the producer makes reaches its request's services"
        );
    }

    /// A producer spawned outside any request stays bare even when its
    /// stream is later read inside one: the wrapper has nothing to capture
    /// and must not fall back to some other request's services, so the
    /// producer's calls are refused loudly rather than reaching a request it
    /// does not belong to. (That the scope is captured at spawn time, not at
    /// poll time, is pinned by the positive test above: a poll-time lookup
    /// would also leave this producer bare.)
    #[wasm_bindgen_test]
    async fn a_producer_spawned_outside_any_request_stays_bare_and_is_refused() {
        let _wrapper = SpawnWrapperGuard::install(Some(wrap_spawned));
        let stream = sse_calling_a_service();
        let resp = request_services::scope(
            RequestServices::marker_with_crypto(1, Arc::new(FixedCrypto)),
            output_to_response(stream),
        )
        .await
        .expect("build response");

        assert_eq!(
            read_streamed(resp).await,
            "data: one\n\n\
             data: before the first chunk: Cloudflare crypto service used outside request poll scope\n\n\
             data: after the first chunk: Cloudflare crypto service used outside request poll scope\n\n",
        );
    }
}
