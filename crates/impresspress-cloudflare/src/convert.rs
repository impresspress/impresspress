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
    http_codec::{self, HttpResponseParts, ResponseMetaPart, META_HTTP_PATH},
    meta::META_RESP_CONTENT_TYPE,
    stream::StreamEvent,
    MetaEntry, MetaGet,
};
use wafer_run::{InputStream, Message, OutputStream};
use worker::{Headers, Request, Response, ResponseBuilder, Result};

// ---------------------------------------------------------------------------
// Request conversion
// ---------------------------------------------------------------------------

/// Convert a Cloudflare Worker Request into a WAFER `(Message, InputStream)`.
///
/// Also normalizes paths by stripping the `/api` prefix.
pub async fn worker_request_to_message(req: &Request) -> Result<(Message, InputStream)> {
    let method = req.method().to_string();
    let url = req.url()?;
    let raw_path = url.path().to_string();
    let query = url.query().unwrap_or("").to_string();

    // Normalize path — strip /api prefix
    let mut path = raw_path.clone();
    if path.starts_with("/api") {
        path = path[4..].to_string();
        if path.is_empty() {
            path = "/".to_string();
        }
    }

    // Read body (with size limit). A read error here would otherwise be
    // swallowed and turned into an empty body, silently corrupting POST/PUT.
    const MAX_BODY_SIZE: usize = 10 * 1024 * 1024; // 10 MB

    // Reject oversized bodies on the declared Content-Length *before* buffering
    // them into the (128 MB) Worker isolate. The post-read check below is the
    // backstop for chunked / absent-length requests where the header can't be
    // trusted.
    if let Some(len) = req
        .headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<usize>().ok())
    {
        if len > MAX_BODY_SIZE {
            return Err("request body too large".into());
        }
    }
    let mut req_clone = req.clone()?;
    let body = req_clone.bytes().await?;
    if body.len() > MAX_BODY_SIZE {
        return Err("request body too large".into());
    }

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
    // The message kind and normalized `req.resource` use the /api-stripped
    // path; `http.path` keeps the path as received on the wire.
    msg.set_meta(META_HTTP_PATH, raw_path);

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
fn apply_meta_to_headers(headers: &Headers, meta: &[MetaEntry]) -> Result<()> {
    for part in http_codec::response_meta_parts(meta) {
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
/// Worker's native `ReadableStream`. A body-read `Error` terminal surfaces as a
/// stream error (aborting the response body) rather than a silent truncation —
/// the HTTP status is already committed, so it cannot be downgraded to 413.
fn build_streaming_response(
    leading_meta: Vec<MetaEntry>,
    first_chunk: Vec<u8>,
    rest: OutputStream,
) -> Result<Response> {
    let status = http_codec::resolve_status(&leading_meta, 200);
    let headers = Headers::new();
    apply_meta_to_headers(&headers, &leading_meta)?;
    if !MetaGet::contains_key(&leading_meta, META_RESP_CONTENT_TYPE) {
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
