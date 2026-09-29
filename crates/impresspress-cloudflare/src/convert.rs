//! HTTP ↔ Message conversion for Cloudflare Workers.
//!
//! Thin platform glue: the protocol mapping (method→action table, request
//! meta layout, response-meta classification, terminal-event mapping) lives
//! in `wafer_block::http_codec`, and the response-streaming decision + framing
//! live in `impresspress_core::streaming` — the same implementations the
//! request pipeline and the browser adapter use. Only worker-type I/O lives
//! here: reading the request body/headers and building the Worker `Response`
//! (buffered or `ReadableStream`-backed).

use futures::StreamExt;
use impresspress_core::streaming::{self, CappedCollect};
use wafer_block::{
    http_codec::{self, HttpResponseParts, ResponseMetaPart},
    stream::StreamEvent,
    MetaEntry,
};
use wafer_run::{InputStream, Message, OutputStream};
use worker::{Headers, Request, Response, ResponseBuilder, Result};

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
/// Two paths, chosen by the shared [`streaming::wants_streaming`] decision so
/// the adapter can never disagree with the pipeline:
///
/// 1. **Streaming** — the producer declared streaming intent up front via
///    leading `Meta` (the `resp.stream` marker or a streaming content-type,
///    e.g. a large file download or an SSE response). Status + headers are
///    applied from the leading meta, then the body chunks are piped straight
///    into the Worker `Response`'s native `ReadableStream`
///    (`ResponseBuilder::from_stream`) — the object never sits in the isolate
///    whole. This path must NOT route through `collect_http_response` (which
///    buffers).
/// 2. **Buffered** (default) — small SSR pages / JSON / buffered replays. The
///    body is drained under [`streaming::MAX_BUFFERED_RESPONSE_BYTES`]; an
///    over-limit body becomes **HTTP 413** (not a generic 500 / an isolate
///    OOM), and everything within the cap is mapped through the canonical
///    `http_codec::collect_http_response` terminal→status logic (reusing
///    [`streaming::terminal_to_stream`] so the `ErrorCode`→status table is
///    never re-implemented here).
pub async fn output_to_response(mut output: OutputStream) -> Result<Response> {
    let (leading_meta, next_event) = streaming::drain_leading_meta(&mut output).await;

    if streaming::wants_streaming(&leading_meta) {
        return match next_event {
            // Declared streaming AND a body chunk to forward — stream it.
            Some(StreamEvent::Chunk(first)) => {
                build_streaming_response(leading_meta, first, output)
            }
            // Declared streaming but the terminal arrived before any body
            // (empty SSE / empty download) — render the (short) buffered form.
            other => {
                finalise_buffered(
                    streaming::collect_capped_with_prelude(
                        output,
                        leading_meta,
                        other,
                        streaming::MAX_BUFFERED_RESPONSE_BYTES,
                    )
                    .await,
                )
                .await
            }
        };
    }

    finalise_buffered(
        streaming::collect_capped_with_prelude(
            output,
            leading_meta,
            next_event,
            streaming::MAX_BUFFERED_RESPONSE_BYTES,
        )
        .await,
    )
    .await
}

/// Apply classified response-meta parts to a Worker `Headers`. Status parts
/// are resolved separately (`http_codec::resolve_status`) and skipped here.
/// Only the canonical `resp.*` meta keys are honored (the `resp.stream`
/// streaming marker is not a header and is ignored by `classify_response_meta`).
fn apply_parts_to_headers(headers: &Headers, parts: &[ResponseMetaPart<'_>]) -> Result<()> {
    for part in parts {
        match part {
            ResponseMetaPart::Status(_) => {}
            ResponseMetaPart::Header { name, value } => headers.set(name, value)?,
            ResponseMetaPart::SetCookie(v) => headers.append("Set-Cookie", v)?,
            ResponseMetaPart::ContentType(v) => headers.set("Content-Type", v)?,
        }
    }
    Ok(())
}

/// Build a streaming Worker `Response`: status + headers from the leading meta
/// (applied *before* the body finishes), body piped chunk-by-chunk into the
/// Worker's native `ReadableStream`. Every ending
/// [`streaming::download_body_stream`] frames as an `Err` — an `Error`
/// terminal (a body-read failure, or a producer that stopped without a
/// terminal), no terminal at all, a `Halt` after the body started — surfaces
/// as a stream error (aborting the response body) rather than a silent
/// truncation: the HTTP status is already committed, so it cannot be
/// downgraded to 413 or 500.
///
/// For the same reason, leading meta no transport can send is answered with
/// the codec's `unsendable_response` here, before a status or a byte is
/// committed. Handed the whole leading meta, the codec's 500 keeps the
/// terminal's sendable security and CORS headers (`Content-Security-Policy`,
/// `X-Frame-Options`, …) and drops the ones describing the body it replaces
/// (`Content-Disposition`, `Content-Encoding`, cache headers), adding
/// `Cache-Control: no-store`.
fn build_streaming_response(
    leading_meta: Vec<MetaEntry>,
    first_chunk: Vec<u8>,
    rest: OutputStream,
) -> Result<Response> {
    let parts = match http_codec::response_meta_parts(&leading_meta) {
        Ok(parts) => parts,
        Err(invalid) => {
            return parts_to_response(http_codec::unsendable_response(&leading_meta, &invalid))
        }
    };
    let status = http_codec::resolve_status(&leading_meta, 200);
    let headers = Headers::new();
    apply_parts_to_headers(&headers, &parts)?;
    if !parts
        .iter()
        .any(|part| matches!(part, ResponseMetaPart::ContentType(_)))
    {
        // Streaming bodies without an explicit content-type fall back to
        // octet-stream (not the JSON default the buffered path uses).
        headers.set("Content-Type", "application/octet-stream")?;
    }

    let body = streaming::download_body_stream(first_chunk, rest)
        .map(|chunk| chunk.map_err(|e| worker::Error::RustError(e.message)));

    ResponseBuilder::new()
        .with_status(status)
        .with_headers(headers)
        .from_stream(body)
}

/// Render a capped buffered collection to a Worker `Response`.
async fn finalise_buffered(collected: CappedCollect) -> Result<Response> {
    match collected {
        // The body would have exceeded the isolate buffering cap — return a
        // clean 413 instead of assembling it whole (which the CF runtime would
        // reject as an opaque error, i.e. the "generic 500" this replaces).
        CappedCollect::OverLimit => over_limit_response(),
        // Within the cap: reuse the canonical terminal→status mapping by
        // reconstructing a single-terminal stream and running it back through
        // `collect_http_response` (no duplicated ErrorCode→status table).
        CappedCollect::Terminal(result) => {
            let parts =
                http_codec::collect_http_response(streaming::terminal_to_stream(result)).await;
            parts_to_response(parts)
        }
    }
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
    let headers = Headers::new();
    for (name, value) in &parts.headers {
        headers.append(name, value)?;
    }
    Ok(Response::from_bytes(parts.body)?
        .with_status(parts.status)
        .with_headers(headers))
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
    use impresspress_core::{
        blocks::errors::{error_response, ErrorCode},
        streaming::{META_RESP_STREAM, STREAM_MARKER_VALUE},
    };
    use wafer_block::{meta::META_RESP_CONTENT_TYPE, MetaEntry};
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
