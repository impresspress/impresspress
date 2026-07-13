//! Shared HTTP response helpers for the Cloudflare Worker.

use worker::*;

/// Build a JSON response with the given status code.
pub fn json_response<T: serde::Serialize>(data: &T, status: u16) -> Result<Response> {
    let body = serde_json::to_string(data)
        .map_err(|e| Error::RustError(format!("JSON serialize: {e}")))?;
    let resp = Response::ok(body)?;
    let mut resp = resp.with_status(status);
    resp.headers_mut().set("Content-Type", "application/json")?;
    Ok(resp)
}
