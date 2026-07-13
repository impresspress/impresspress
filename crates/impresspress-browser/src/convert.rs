//! HTTP ↔ Message conversion for the browser Service Worker adapter.
//!
//! Thin platform glue: the protocol mapping (method→action table, request
//! meta layout, response-meta classification, `ErrorCode`→status table) lives
//! in `wafer_block::http_codec` — the same implementation the native axum
//! listener and the Cloudflare adapter use. Only `web_sys` I/O lives here:
//! reading the request body/headers, the Service-Worker cookie re-injection,
//! and building `web_sys::Response` (buffered or `ReadableStream`-backed).

use futures::{SinkExt, StreamExt};
use js_sys::{ArrayBuffer, Uint8Array};
use wafer_block::{
    http_codec::{self, ResponseMetaPart},
    meta::META_RESP_CONTENT_TYPE,
    stream::StreamEvent,
    streams::{
        input::InputStream,
        output::{BufferedResponse, OutputStream, TerminalNotResponse},
    },
    Message, MetaEntry, MetaGet,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, ResponseInit};

// ---------------------------------------------------------------------------
// Request conversion
// ---------------------------------------------------------------------------

/// Convert a browser `web_sys::Request` into a WAFER `(Message, InputStream)` pair.
///
/// The protocol mapping (kind, `http.*` / `req.*` meta, method→action, header
/// and query decoding) is delegated to `http_codec::build_http_message`; only
/// the `web_sys` body/header reads and the Service-Worker cookie re-injection
/// are browser-specific. The remote address is always `"127.0.0.1"` — in a
/// Service Worker the request comes from the same device.
pub async fn request_to_message(
    request: &web_sys::Request,
) -> Result<(Message, InputStream), JsValue> {
    let method = request.method();
    let url_str = request.url();

    // Parse the URL so we can separate path and query string.
    let url = web_sys::Url::new(&url_str)?;
    let path = url.pathname();
    // search() includes the leading '?' — strip it.
    let search = url.search();
    let raw_query = if let Some(stripped) = search.strip_prefix('?') {
        stripped.to_string()
    } else {
        search
    };

    // Read body bytes via ArrayBuffer.
    const MAX_BODY_SIZE: usize = 10 * 1024 * 1024; // 10MB
    let body: Vec<u8> = {
        let promise = request.array_buffer()?;
        let ab_val = JsFuture::from(promise).await?;
        let ab: ArrayBuffer = ab_val.dyn_into()?;
        let arr = Uint8Array::new(&ab);
        if arr.length() as usize > MAX_BODY_SIZE {
            return Err(JsValue::from_str("Request body too large"));
        }
        arr.to_vec()
    };

    // Collect headers into (name, value) pairs for the codec.
    let mut header_pairs: Vec<(String, String)> = Vec::new();
    let headers: Headers = request.headers();
    let iter =
        js_sys::try_iter(&headers)?.ok_or_else(|| JsValue::from_str("headers not iterable"))?;
    for item in iter {
        let item = item?;
        // Each entry is a JS Array [name, value].
        let arr: js_sys::Array = item.dyn_into()?;
        let key = arr.get(0).as_string().unwrap_or_default();
        let val = arr.get(1).as_string().unwrap_or_default();
        header_pairs.push((key, val));
    }

    // Re-inject the `Cookie` header from the SW's CookieStore.
    // `FetchEvent.request.headers` filters `Cookie` out per the SW spec, so
    // the header iteration above never sees it even though the browser sends
    // cookies on same-origin requests. CookieStore is the only way to read
    // them back inside the SW.
    let cookie_val = crate::bridge::read_cookie_header().await;
    if let Some(s) = cookie_val.as_string() {
        if !s.is_empty() {
            header_pairs.push(("cookie".to_string(), s));
        }
    }

    // `build_http_message` builds `kind`, `http.*` and normalized `req.*` meta
    // from the method+path. The browser serves paths as-received (no `/api`
    // prefix to strip, unlike the Cloudflare adapter), so no post-fixup.
    let msg = http_codec::build_http_message(
        &method,
        &path,
        &raw_query,
        "127.0.0.1",
        header_pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    );

    Ok((msg, InputStream::from_bytes(body)))
}

// ---------------------------------------------------------------------------
// Response conversion
// ---------------------------------------------------------------------------

/// Apply classified response-meta parts to a `web_sys::Headers`. Status parts
/// are resolved separately (see `http_codec::resolve_status`) and skipped here.
/// Only the canonical `resp.*` meta keys are honored — legacy aliases
/// (`http.status`, `http.resp.header.*`, `http.resp.set-cookie.*`, a literal
/// `Content-Type` meta key) are ignored by `http_codec`.
fn apply_response_meta(headers: &Headers, meta: &[MetaEntry]) -> Result<(), JsValue> {
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

/// True when `meta` carries an explicit `resp.content_type` entry.
fn has_content_type(meta: &[MetaEntry]) -> bool {
    MetaGet::contains_key(meta, META_RESP_CONTENT_TYPE)
}

/// Build a `web_sys::Response` from raw bytes, a status code, and a
/// `web_sys::Headers` object.
fn make_response(
    body: Vec<u8>,
    status: u16,
    headers: Headers,
) -> Result<web_sys::Response, JsValue> {
    let init = ResponseInit::new();
    init.set_status(status);
    init.set_headers(&headers);

    if body.is_empty() {
        web_sys::Response::new_with_opt_str_and_init(None, &init)
    } else {
        // Copy into a Uint8Array then pass as BufferSource.
        let arr = Uint8Array::new_with_length(body.len() as u32);
        arr.copy_from(&body);
        let ab: ArrayBuffer = arr.buffer();
        web_sys::Response::new_with_opt_buffer_source_and_init(Some(&ab.into()), &init)
    }
}

/// Pull `Meta` events off the front of an `OutputStream`, stopping at the
/// first non-Meta event. Returns the accumulated meta and the next event
/// (if any). Used by `output_to_response` to peek the response's headers
/// before deciding whether to stream the body or buffer it.
async fn drain_leading_meta(output: &mut OutputStream) -> (Vec<MetaEntry>, Option<StreamEvent>) {
    let mut meta = Vec::new();
    while let Some(ev) = output.next().await {
        match ev {
            StreamEvent::Meta(entry) => meta.push(entry),
            other => return (meta, Some(other)),
        }
    }
    (meta, None)
}

/// Build a JS `ReadableStream` that yields `first_chunk` and then every
/// subsequent `Chunk` event from `remaining`. Mid-body `Meta` is dropped
/// (too late to apply to HTTP headers); any terminal closes the stream.
fn make_streaming_body(
    first_chunk: Vec<u8>,
    mut remaining: OutputStream,
) -> wasm_streams::ReadableStream {
    use futures::channel::mpsc;
    let (mut tx, rx) = mpsc::channel::<Result<JsValue, JsValue>>(8);

    // Channel cap is 8 and we have one item to send — try_send fits. If it
    // fails (caller dropped the stream before consuming) we just discard.
    let _ = tx.try_send(Ok(JsValue::from(Uint8Array::from(first_chunk.as_slice()))));

    wasm_bindgen_futures::spawn_local(async move {
        while let Some(ev) = remaining.next().await {
            match ev {
                StreamEvent::Chunk(bytes) => {
                    let val: Result<JsValue, JsValue> =
                        Ok(JsValue::from(Uint8Array::from(bytes.as_slice())));
                    if tx.send(val).await.is_err() {
                        // Browser dropped the response stream — stop pumping.
                        return;
                    }
                }
                // Mid-body Meta is too late to apply to HTTP headers; drop it.
                StreamEvent::Meta(_) => {}
                // Any terminal closes the body. Error after partial body has
                // already streamed bytes can't change the HTTP status — log
                // and close cleanly so the browser sees a normal end-of-body.
                StreamEvent::Error(err) => {
                    web_sys::console::warn_1(
                        &format!(
                            "impresspress-browser: streaming response aborted: {}",
                            err.message
                        )
                        .into(),
                    );
                    return;
                }
                StreamEvent::Complete { .. }
                | StreamEvent::Drop
                | StreamEvent::Continue(_)
                | StreamEvent::Halt { .. } => {
                    // Mid-body Halt cannot change the HTTP status (headers
                    // already flushed); treat it as another terminal that
                    // closes the body cleanly. The browser sees a normal
                    // end-of-stream.
                    return;
                }
            }
        }
    });

    wasm_streams::ReadableStream::from_stream(rx)
}

/// Convert a WAFER `OutputStream` into a browser `web_sys::Response`.
///
/// Two paths:
/// 1. **Streaming** — for blocks that emit leading `Meta` events declaring
///    `Content-Type: text/event-stream` (or `application/octet-stream`)
///    BEFORE the first `Chunk`. We classify the leading meta with
///    `http_codec::response_meta_parts` and apply status + headers to a
///    `Response` backed by a `ReadableStream`, piping subsequent chunks
///    straight to the browser — so a multi-minute SSE response isn't held
///    back behind a buffer that flushes at the very end (which Chrome's idle
///    keep-alive treats as a hung fetch and drops with `net::ERR_FAILED`).
///    The meta is applied *before the body finishes*, so this path must NOT
///    route through `collect_http_response` (which buffers).
/// 2. **Buffered** (default) — for blocks that emit `Chunk(bytes),
///    Complete{meta}` via `respond_with_meta`. Status, headers, and body all
///    live in the terminal, so we read the whole stream before building the
///    `Response`. The terminal-event mapping mirrors
///    `http_codec::collect_http_response` (whose drift decisions —
///    `Continue` → empty `200`, default `Content-Type: application/json`,
///    `Ok`/`Halt` identical — are pinned by the codec's tests).
pub async fn output_to_response(mut output: OutputStream) -> Result<web_sys::Response, JsValue> {
    // Peek leading Meta events without consuming Chunks. The streaming path is
    // signalled by an early Content-Type meta; buffered blocks send no Meta
    // before their first Chunk, so this returns an empty vec for them and the
    // buffered branch below handles the terminal.
    let (leading_meta, next_event) = drain_leading_meta(&mut output).await;

    let leading_ct = http_codec::response_meta_parts(&leading_meta).find_map(|part| match part {
        ResponseMetaPart::ContentType(ct) => Some(ct.to_string()),
        _ => None,
    });

    if let (Some(ct), Some(StreamEvent::Chunk(first))) = (leading_ct, &next_event) {
        if is_streaming_content_type(&ct) {
            return build_streaming_response(leading_meta, first.clone(), output);
        }
    }

    // Buffered path — drain the remainder, prepending the leading meta + the
    // event we peeked, then map the terminal to a response exactly as
    // `http_codec::collect_http_response` would for a non-peeked stream.
    let terminal = collect_buffered_with_prelude(output, leading_meta, next_event).await;
    finalise_buffered(terminal)
}

/// True for content-types that should stream body chunks to the browser as
/// they're produced rather than buffer the entire response. Today: SSE and
/// generic byte streams (which feature blocks use for downloads / archives).
fn is_streaming_content_type(ct: &str) -> bool {
    let lower = ct.to_ascii_lowercase();
    lower.starts_with("text/event-stream") || lower.starts_with("application/octet-stream")
}

/// Drain the remaining stream into a buffered terminal, prepending the
/// already-peeked leading meta + next event. Mirrors the contract of
/// `OutputStream::collect_buffered` (a `Halt` terminal replaces any streamed
/// prelude), reproduced here because `impresspress-browser` cannot depend on
/// `impresspress-core`'s `pipeline::collect_buffered_with_prelude`.
async fn collect_buffered_with_prelude(
    rest: OutputStream,
    leading_meta: Vec<MetaEntry>,
    next_event: Option<StreamEvent>,
) -> Result<BufferedResponse, TerminalNotResponse> {
    match next_event {
        Some(StreamEvent::Chunk(first)) => match rest.collect_buffered().await {
            Ok(buf) => {
                let mut body = first;
                body.extend(buf.body);
                let mut meta = leading_meta;
                meta.extend(buf.meta);
                Ok(BufferedResponse { body, meta })
            }
            Err(terminal) => Err(terminal),
        },
        Some(StreamEvent::Meta(_)) => unreachable!("drain_leading_meta consumes Meta events"),
        Some(StreamEvent::Complete { meta }) => {
            let mut all_meta = leading_meta;
            all_meta.extend(meta);
            Ok(BufferedResponse {
                body: Vec::new(),
                meta: all_meta,
            })
        }
        Some(StreamEvent::Halt { body, meta }) => {
            // Halt carries a complete response; per the `collect_buffered`
            // contract any prior streamed events — the prelude included — are
            // replaced by its payload.
            Err(TerminalNotResponse::Halt(BufferedResponse { body, meta }))
        }
        Some(StreamEvent::Error(err)) => Err(TerminalNotResponse::Error(*err)),
        Some(StreamEvent::Drop) => Err(TerminalNotResponse::Drop),
        Some(StreamEvent::Continue(msg)) => Err(TerminalNotResponse::Continue(msg)),
        None => Err(TerminalNotResponse::Malformed),
    }
}

/// Map a buffered terminal to a `web_sys::Response`, mirroring
/// `http_codec::collect_http_response`'s terminal handling (the codec maps to
/// transport-neutral parts; we apply them to `web_sys` types here).
fn finalise_buffered(
    result: Result<BufferedResponse, TerminalNotResponse>,
) -> Result<web_sys::Response, JsValue> {
    match result {
        // Ok and Halt are the single buffered code path (codec finding 55).
        Ok(buf) | Err(TerminalNotResponse::Halt(buf)) => {
            let status = http_codec::resolve_status(&buf.meta, 200);
            let headers = Headers::new()?;
            apply_response_meta(&headers, &buf.meta)?;
            if !has_content_type(&buf.meta) {
                headers.set("Content-Type", http_codec::DEFAULT_RESPONSE_CONTENT_TYPE)?;
            }
            make_response(buf.body, status, headers)
        }

        Err(TerminalNotResponse::Error(err)) => {
            let status = http_codec::resolve_error_status(&err);
            let headers = Headers::new()?;
            apply_response_meta(&headers, &err.meta)?;
            // Error bodies ARE JSON; a `resp.content_type` on the error meta is
            // superseded (exactly one Content-Type, matching the codec).
            headers.set("Content-Type", http_codec::DEFAULT_RESPONSE_CONTENT_TYPE)?;
            // Surface the precise application code (set via
            // `WaferError::with_detail_code`, carried as `error.code` meta) as a
            // machine-readable `code` field; the coarse wafer code stays in
            // `error`. Omitted when no detail code was attached.
            let mut body = serde_json::json!({
                "error": err.code,
                "message": err.message,
            });
            if let Some(detail) = err.detail_code() {
                body["code"] = serde_json::Value::String(detail.to_string());
            }
            let body = body.to_string().into_bytes();
            make_response(body, status, headers)
        }

        Err(TerminalNotResponse::Drop) => make_response(Vec::new(), 204, Headers::new()?),

        Err(TerminalNotResponse::Continue(msg)) => {
            // Codec drift: `Continue` at the HTTP boundary → empty-body 200
            // with the message's response meta applied (nowhere to forward).
            let headers = Headers::new()?;
            apply_response_meta(&headers, &msg.meta)?;
            headers.set("Content-Type", http_codec::DEFAULT_RESPONSE_CONTENT_TYPE)?;
            make_response(Vec::new(), 200, headers)
        }

        Err(TerminalNotResponse::Malformed) => {
            web_sys::console::error_1(
                &"impresspress-browser: stream ended without terminal event".into(),
            );
            let headers = Headers::new()?;
            make_response(b"internal server error".to_vec(), 500, headers)
        }
    }
}

/// Build a streaming `web_sys::Response` from the leading meta (carrying
/// status + headers) and an `OutputStream` whose remaining events are piped
/// into the body. Meta is classified and applied *before* the body finishes —
/// the whole point of the streaming path.
fn build_streaming_response(
    leading_meta: Vec<MetaEntry>,
    first_chunk: Vec<u8>,
    remaining: OutputStream,
) -> Result<web_sys::Response, JsValue> {
    let status = http_codec::resolve_status(&leading_meta, 200);
    let headers = Headers::new()?;
    apply_response_meta(&headers, &leading_meta)?;

    if !has_content_type(&leading_meta) {
        // Streaming bodies without an explicit Content-Type fall back to
        // octet-stream rather than the JSON default the buffered path uses.
        headers.set("Content-Type", "application/octet-stream")?;
    }

    let stream = make_streaming_body(first_chunk, remaining);
    let raw_js = stream.into_raw();
    let init = ResponseInit::new();
    init.set_status(status);
    init.set_headers(&headers);
    web_sys::Response::new_with_opt_readable_stream_and_init(Some(&raw_js), &init)
}
