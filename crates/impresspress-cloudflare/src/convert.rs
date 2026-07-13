//! HTTP ↔ Message conversion for Cloudflare Workers.
//!
//! Thin platform glue: the protocol mapping (method→action table, request
//! meta layout, response-meta classification, terminal-event mapping) lives
//! in `wafer_block::http_codec` — the same implementation the native axum
//! listener and the browser adapter use. Only worker-type I/O lives here.

use wafer_block::http_codec::{self, META_HTTP_PATH};
use wafer_run::{InputStream, Message, OutputStream};
use worker::{Request, Response, Result};

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
/// Buffers the full stream through the canonical
/// `http_codec::collect_http_response` terminal mapping, then applies the
/// transport-neutral parts to the worker types.
pub async fn output_to_response(output: OutputStream) -> Result<Response> {
    let parts = http_codec::collect_http_response(output).await;
    let headers = worker::Headers::new();
    for (name, value) in &parts.headers {
        headers.append(name, value)?;
    }
    Ok(Response::from_bytes(parts.body)?
        .with_status(parts.status)
        .with_headers(headers))
}
