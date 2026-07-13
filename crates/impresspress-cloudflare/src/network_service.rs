use std::collections::HashMap;

use wafer_core::interfaces::network::service::{NetworkError, NetworkService, Request, Response};

/// NetworkService using CF Worker's fetch API.
pub struct WorkerFetchService;

// SAFETY: `WorkerFetchService` is unit-shaped and contains no shared state.
// wasm32-unknown-unknown has no threads, so `Send`/`Sync` are satisfied
// trivially — no cross-thread aliasing or data races are possible.
unsafe impl Send for WorkerFetchService {}
unsafe impl Sync for WorkerFetchService {}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl NetworkService for WorkerFetchService {
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
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

        let mut resp = worker::Fetch::Request(worker_req)
            .send()
            .await
            .map_err(|e| NetworkError::RequestError(format!("fetch error: {e}")))?;

        let status_code = resp.status_code();
        let resp_body = resp
            .bytes()
            .await
            .map_err(|e| NetworkError::RequestError(format!("read body: {e}")))?;
        let mut resp_headers: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in resp.headers() {
            resp_headers.entry(k).or_default().push(v);
        }

        Ok(Response {
            status_code,
            headers: resp_headers,
            body: resp_body,
        })
    }
}
