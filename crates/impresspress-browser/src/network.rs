use std::collections::HashMap;

use serde::Deserialize;
use wafer_core::interfaces::network::service::{NetworkError, NetworkService, Request, Response};

use crate::bridge;

pub struct BrowserNetworkService;

// SAFETY: `BrowserNetworkService` is a unit struct with no shared state.
// wasm32-unknown-unknown has no threads, so the `Send`/`Sync` bounds
// required by `Arc<dyn NetworkService>` are satisfied trivially — no
// cross-thread aliasing or data races are possible.
unsafe impl Send for BrowserNetworkService {}
unsafe impl Sync for BrowserNetworkService {}

/// JS object shape returned by bridge.httpFetch (NOT a JSON string):
/// `{ status: number, headers: { [key: string]: string }, body: Uint8Array }`.
/// Decoded directly via `serde_wasm_bindgen::from_value` — `serde_wasm_bindgen`
/// deserializes a JS `Uint8Array` straight into `Vec<u8>`.
#[derive(Deserialize)]
struct FetchResponse {
    status: u16,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: Vec<u8>,
}

#[async_trait::async_trait(?Send)]
impl NetworkService for BrowserNetworkService {
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
        let headers_json = serde_json::to_string(&req.headers)
            .map_err(|e| NetworkError::Other(format!("failed to serialize headers: {e}")))?;

        let body_bytes: &[u8] = req.body.as_deref().unwrap_or(&[]);

        let js_val = bridge::http_fetch(&req.method, &req.url, &headers_json, body_bytes)
            .await
            .map_err(|e| NetworkError::RequestError(bridge::describe(&e)))?;

        // The bridge resolves a JS object `{ status, headers, body:
        // Uint8Array }` — decode it directly. `serde_wasm_bindgen`
        // deserializes the JS object into `FetchResponse` and the
        // `Uint8Array` body straight into `Vec<u8>` in one step, with no
        // JSON round-trip (previously this called `JSON::stringify` on the
        // resolved value and fed the result to `serde_json::from_str`,
        // which double-encoded every response into a JSON string literal
        // and failed with "invalid type: string, expected struct").
        let fetch_resp: FetchResponse = serde_wasm_bindgen::from_value(js_val).map_err(|e| {
            NetworkError::RequestError(format!("failed to decode fetch response: {e}"))
        })?;

        // Collapse single-value headers into Vec<String> as NetworkService::Response expects.
        let headers: HashMap<String, Vec<String>> = fetch_resp
            .headers
            .into_iter()
            .map(|(k, v)| (k, vec![v]))
            .collect();

        Ok(Response {
            status_code: fetch_resp.status,
            headers,
            body: fetch_resp.body,
        })
    }
}

pub fn make_network_service(
) -> std::sync::Arc<dyn wafer_core::interfaces::network::service::NetworkService> {
    std::sync::Arc::new(BrowserNetworkService)
}

// `bridge::http_fetch` is a `#[wasm_bindgen(module = "/js/bridge.js")]` extern
// import, so `do_request` itself can't be exercised outside a real Service
// Worker/page context (the module path doesn't resolve under `wasm-pack
// test`). What CAN be verified in isolation — and is exactly what the
// double-encode bug broke — is the decode step: does a JS value shaped like
// what `bridge.js`'s `httpFetch` now resolves deserialize into
// `FetchResponse` correctly, and does the *old* buggy shape (a JSON string,
// from the removed `JSON.stringify` round-trip) correctly fail instead of
// silently coercing?
#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use js_sys::{Object, Reflect, Uint8Array};
    use wasm_bindgen::JsValue;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::FetchResponse;

    /// Build the JS object shape `bridge.js`'s `httpFetch` resolves post-fix:
    /// `{ status: number, headers: {[k]: string}, body: Uint8Array }`.
    fn make_fetch_response_object(status: u16, body: &[u8]) -> JsValue {
        let obj = Object::new();
        Reflect::set(
            &obj,
            &JsValue::from_str("status"),
            &JsValue::from_f64(status as f64),
        )
        .unwrap();

        let headers = Object::new();
        Reflect::set(
            &headers,
            &JsValue::from_str("content-type"),
            &JsValue::from_str("application/json"),
        )
        .unwrap();
        Reflect::set(&obj, &JsValue::from_str("headers"), &headers).unwrap();

        Reflect::set(
            &obj,
            &JsValue::from_str("body"),
            &Uint8Array::from(body).into(),
        )
        .unwrap();

        obj.into()
    }

    #[wasm_bindgen_test]
    fn decodes_js_object_response_in_one_step() {
        let body = b"hello world";
        let js_val = make_fetch_response_object(200, body);

        let decoded: FetchResponse =
            serde_wasm_bindgen::from_value(js_val).expect("decode fetch response");

        assert_eq!(decoded.status, 200);
        assert_eq!(decoded.body, body.to_vec());
        assert_eq!(
            decoded.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
    }

    #[wasm_bindgen_test]
    fn decodes_empty_body_and_headers_via_serde_default() {
        let obj = Object::new();
        Reflect::set(
            &obj,
            &JsValue::from_str("status"),
            &JsValue::from_f64(204.0),
        )
        .unwrap();
        // No `headers` or `body` keys at all — `#[serde(default)]` must
        // fill both rather than erroring as "missing field".
        let js_val: JsValue = obj.into();

        let decoded: FetchResponse =
            serde_wasm_bindgen::from_value(js_val).expect("decode fetch response");

        assert_eq!(decoded.status, 204);
        assert!(decoded.body.is_empty());
        assert!(decoded.headers.is_empty());
    }

    /// Regression guard for the exact bug this task fixes: `bridge.js` used
    /// to `JSON.stringify` the response envelope into a JS *string*, and
    /// `network.rs` then called `JSON::stringify` on that string AGAIN
    /// before `serde_json::from_str::<FetchResponse>` — double-encoding, so
    /// every response failed with "invalid type: string, expected struct".
    /// If `httpFetch` ever regresses back to resolving a string instead of
    /// an object, decoding must fail loudly here rather than silently
    /// producing a wrong value.
    #[wasm_bindgen_test]
    fn old_json_string_shape_fails_to_decode_as_object() {
        let json_string = JsValue::from_str(r#"{"status":200,"headers":{},"body":[104,105]}"#);

        let result: Result<FetchResponse, _> = serde_wasm_bindgen::from_value(json_string);

        assert!(
            result.is_err(),
            "a JSON string must not decode as the FetchResponse object shape"
        );
    }
}
