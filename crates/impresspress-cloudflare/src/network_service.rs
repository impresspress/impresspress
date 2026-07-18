use std::collections::HashMap;

use futures::StreamExt;
use wafer_block::{common::ErrorCode, OutputStream, WaferError};
use wafer_core::interfaces::network::service::{
    NetworkError, NetworkService, Request, Response, ResponseHead,
};

/// Response-body cap for the streaming fetch path, in bytes. Mirrors the
/// native `wafer-block-network` SEC-020 default
/// (`HttpNetworkService::DEFAULT_MAX_RESPONSE_BYTES`, 50 MiB) so a hostile or
/// runaway upstream cannot stream unbounded into the isolate. A fixed constant
/// rather than a config read: the unit-shaped `WorkerFetchService` has no
/// config plumbing, and this is a security floor, not an operator knob.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

/// NetworkService using CF Worker's fetch API.
pub struct WorkerFetchService;

// SAFETY: `WorkerFetchService` is unit-shaped and contains no shared state.
// wasm32-unknown-unknown has no threads, so `Send`/`Sync` are satisfied
// trivially — no cross-thread aliasing or data races are possible.
unsafe impl Send for WorkerFetchService {}
unsafe impl Sync for WorkerFetchService {}

impl WorkerFetchService {
    /// Shared request setup + dispatch for both the buffered [`do_request`] and
    /// the streaming [`do_request_streaming`] paths: method parse, request
    /// init, header/body build, and the `fetch` call. Keeping it in one place
    /// means the two entry points can never drift on how a request is built or
    /// on any gate applied before dispatch — mirroring how the native
    /// `wafer-run/network` backend shares one `send_request`.
    ///
    /// SSRF precheck (defense-in-depth, SEC-019 consumer follow-up): before
    /// dispatching the subrequest, the request URL is run through the shared
    /// [`is_ssrf_blocked_url`](impresspress_core::ssrf::is_ssrf_blocked_url)
    /// gate, so a request whose URL *literally* names an internal target —
    /// private/loopback/link-local/CGNAT IP literal, `localhost`, an
    /// IPv6-embedded-v4 form, or a well-known cloud-metadata hostname — is
    /// refused here rather than relying solely on Cloudflare's runtime
    /// sandbox. Applied in this shared helper so the buffered `do_request` and
    /// the streaming `do_request_streaming` can never drift on the gate.
    ///
    /// Honest boundary: a Worker cannot resolve DNS before `fetch`, so this
    /// precheck is necessarily URL/host-literal-based. It does NOT defend
    /// against DNS rebinding — a public-looking hostname that resolves to a
    /// private IP at connect time still reaches `fetch`; that residual case is
    /// Cloudflare's own subrequest-SSRF layer to catch (the native path closes
    /// it with a resolve-before-connect resolver, which the Workers `fetch`
    /// API gives no hook for).
    ///
    /// [`do_request`]: NetworkService::do_request
    /// [`do_request_streaming`]: NetworkService::do_request_streaming
    async fn send(&self, req: &Request) -> Result<worker::Response, NetworkError> {
        if impresspress_core::ssrf::is_ssrf_blocked_url(&req.url) {
            return Err(NetworkError::RequestError(format!(
                "SSRF: refusing request to internal/blocked address: {}",
                req.url
            )));
        }

        let method = match req.method.to_uppercase().as_str() {
            "GET" => worker::Method::Get,
            "POST" => worker::Method::Post,
            "PUT" => worker::Method::Put,
            "PATCH" => worker::Method::Patch,
            "DELETE" => worker::Method::Delete,
            "HEAD" => worker::Method::Head,
            other => {
                return Err(NetworkError::RequestError(format!(
                    "unsupported HTTP method: {other}"
                )));
            }
        };

        let mut init = worker::RequestInit::new();
        init.with_method(method);
        if let Some(ref body) = req.body {
            let uint8arr = js_sys::Uint8Array::from(&body[..]);
            init.with_body(Some(uint8arr.into()));
        }

        let mut worker_req = worker::Request::new_with_init(&req.url, &init)
            .map_err(|e| NetworkError::RequestError(format!("fetch init error: {e}")))?;

        // Propagate header failures instead of silently dropping the entire
        // header block — callers rely on Authorization, Content-Type etc.
        let headers = worker_req
            .headers_mut()
            .map_err(|e| NetworkError::RequestError(format!("headers_mut: {e}")))?;
        for (k, v) in &req.headers {
            headers
                .set(k, v)
                .map_err(|e| NetworkError::RequestError(format!("set header {k}: {e}")))?;
        }

        worker::Fetch::Request(worker_req)
            .send()
            .await
            .map_err(|e| NetworkError::RequestError(format!("fetch error: {e}")))
    }
}

/// Flatten a Worker response's headers into the wire-facing
/// `name → [values]` map (one entry per header name, all values preserved).
/// Shared by both the buffered and streaming paths so their head shapes match.
fn collect_headers(resp: &worker::Response) -> HashMap<String, Vec<String>> {
    let mut resp_headers: HashMap<String, Vec<String>> = HashMap::new();
    for (k, v) in resp.headers() {
        resp_headers.entry(k).or_default().push(v);
    }
    resp_headers
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl NetworkService for WorkerFetchService {
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
        let mut resp = self.send(req).await?;

        let status_code = resp.status_code();
        let resp_body = resp
            .bytes()
            .await
            .map_err(|e| NetworkError::RequestError(format!("read body: {e}")))?;
        let resp_headers = collect_headers(&resp);

        Ok(Response {
            status_code,
            headers: resp_headers,
            body: resp_body,
        })
    }

    /// Streams the upstream `fetch` response body through the Worker's native
    /// `ReadableStream` (`Response::stream` → `worker::ByteStream`) into an
    /// [`OutputStream`], instead of buffering the whole body into a `Vec` like
    /// [`do_request`](Self::do_request). The [`ResponseHead`] (status +
    /// headers) is resolved eagerly from the response head; body chunks flow
    /// verbatim as they arrive.
    ///
    /// Goes through the same [`send`](Self::send) path as the buffered request,
    /// so the SSRF/allowlist posture is identical (see `send`'s doc). A
    /// body-read failure surfaces as an `Error` terminal after whatever bytes
    /// already streamed; a dropped consumer aborts the blocked read via the
    /// paired cancellation token. A response with no body (e.g. `HEAD`, 204)
    /// yields an empty (`Complete`-only) body stream.
    ///
    /// SEC-020 response cap ([`DEFAULT_MAX_RESPONSE_BYTES`]) is enforced on this
    /// new streaming capability the same way the native backend does: an
    /// over-large advertised `Content-Length` is rejected before any bytes
    /// stream, and the running byte total is checked per chunk (the only guard
    /// for chunked / unknown-length responses), surfacing an `Error` terminal
    /// on overflow after the partial bytes already forwarded.
    async fn do_request_streaming(
        &self,
        req: &Request,
    ) -> Result<(ResponseHead, OutputStream), NetworkError> {
        let mut resp = self.send(req).await?;

        let head = ResponseHead {
            status_code: resp.status_code(),
            headers: collect_headers(&resp),
        };

        // Reject up front when the advertised length already exceeds the cap,
        // before streaming any bytes. `Headers::get` is case-insensitive.
        // Read before `resp.stream()` takes its mutable borrow.
        let cap = DEFAULT_MAX_RESPONSE_BYTES;
        if let Ok(Some(len)) = resp.headers().get("content-length") {
            if let Ok(advertised) = len.parse::<usize>() {
                if advertised > cap {
                    return Err(NetworkError::RequestError(format!(
                        "response body {advertised} bytes exceeds cap of {cap} bytes"
                    )));
                }
            }
        }

        // `Response::stream` errors only when the response carries no body
        // (`ResponseBody::Empty`); map that to an empty body stream rather than
        // failing the whole request — parity with the buffered path, where
        // `bytes()` returns an empty `Vec` for a bodyless response.
        let body_stream = match resp.stream() {
            Ok(byte_stream) => {
                // `ByteStream` owns a `'static` handle to the JS ReadableStream,
                // so it outlives `resp`. Box-pin so the producer loop can
                // `.next()` it regardless of the stream's own `Unpin`-ness.
                let mut byte_stream = Box::pin(byte_stream);
                OutputStream::from_producer(move |sink, cancel| async move {
                    let mut received: usize = 0;
                    loop {
                        let Some(next) = cancel.run_until_cancelled(byte_stream.next()).await
                        else {
                            return;
                        };
                        match next {
                            None => break,
                            Some(Ok(chunk)) => {
                                // Running-total cap (SEC-020): bounds chunked /
                                // unknown-length responses the advertised-length
                                // guard above can't. Over-cap → Error terminal
                                // after the partial bytes already sent (never a
                                // silent truncation reported as Complete).
                                received = received.saturating_add(chunk.len());
                                if received > cap {
                                    let _ = sink
                                        .error(WaferError::new(
                                            ErrorCode::Unavailable,
                                            format!("response body exceeds cap of {cap} bytes"),
                                        ))
                                        .await;
                                    return;
                                }
                                if sink.send_chunk(chunk).await.is_err() {
                                    return;
                                }
                            }
                            Some(Err(e)) => {
                                let _ = sink
                                    .error(WaferError::new(
                                        ErrorCode::Unavailable,
                                        format!("reading response body: {e}"),
                                    ))
                                    .await;
                                return;
                            }
                        }
                    }
                    let _ = sink.complete(vec![]).await;
                })
            }
            Err(_) => OutputStream::respond_with_meta(vec![], vec![]),
        };

        Ok((head, body_stream))
    }
}
