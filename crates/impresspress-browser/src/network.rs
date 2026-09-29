use std::collections::HashMap;

use impresspress_core::streaming::MAX_NETWORK_RESPONSE_BYTES;
use serde::Deserialize;
use wafer_block::OutputStream;
use wafer_core::interfaces::network::service::{
    NetworkError, NetworkService, Request, Response, ResponseHead,
};

use crate::{
    bridge,
    storage::{drain_reader_into, release_reader_in},
};

pub struct BrowserNetworkService;

// SAFETY: `BrowserNetworkService` is a unit struct with no shared state.
// wasm32-unknown-unknown has no threads, so the `Send`/`Sync` bounds
// required by `Arc<dyn NetworkService>` are satisfied trivially — no
// cross-thread aliasing or data races are possible.
unsafe impl Send for BrowserNetworkService {}
unsafe impl Sync for BrowserNetworkService {}

/// JS object shape returned by bridge.httpFetch (NOT a JSON string):
/// `{ status: number, headers: [[name, value], ...], body: Uint8Array }`.
/// Decoded directly via `serde_wasm_bindgen::from_value` — `serde_wasm_bindgen`
/// deserializes a JS `Uint8Array` straight into `Vec<u8>`.
///
/// `headers` is an **array of pairs**, not an object, because a response may
/// carry the same header name more than once and a JS object cannot hold it
/// twice. `Set-Cookie` is the case that bites: the Fetch spec's header
/// iteration combines repeated names into one comma-joined value *except* for
/// `Set-Cookie`, which it yields once per cookie — so the object this replaced
/// kept only the last one, and a response that set a session cookie and a CSRF
/// cookie silently lost one of them. (Comma-joining `Set-Cookie` is not a fix
/// either: the header's own grammar uses commas, in `Expires` dates among
/// other places, so a joined value cannot be split back apart.)
#[derive(Deserialize)]
struct FetchResponse {
    status: u16,
    #[serde(default)]
    headers: Vec<(String, String)>,
    /// One bulk copy out of the real `Uint8Array`; see the `serde_bytes` note
    /// in `Cargo.toml` for why the adapter is not optional here.
    #[serde(default, with = "serde_bytes")]
    body: Vec<u8>,
}

/// `httpFetchStream`'s resolved shape: the head eagerly, and the id of the
/// registered byte reader carrying the body — or `null` for a response with no
/// body at all (204, `HEAD`).
#[derive(Deserialize)]
struct FetchStreamStart {
    status: u16,
    #[serde(default)]
    headers: Vec<(String, String)>,
    stream_id: Option<String>,
}

/// The one place the request URL is checked before it reaches `fetch`.
///
/// A free function rather than a line in `do_request`, because
/// `do_request_streaming` has to apply exactly the same gate and a second copy
/// is a second thing to forget — the mistake the Cloudflare adapter avoids by
/// routing both entry points through one `send`.
fn refuse_if_ssrf(url: &str) -> Result<(), NetworkError> {
    if impresspress_core::ssrf::is_ssrf_blocked_url(url) {
        return Err(NetworkError::RequestError(format!(
            "SSRF: refusing request to internal/blocked address: {url}"
        )));
    }
    Ok(())
}

#[async_trait::async_trait(?Send)]
impl NetworkService for BrowserNetworkService {
    /// Dispatch one request through the page's `fetch`.
    ///
    /// **SSRF precheck.** Before anything is dispatched, the request URL goes
    /// through the shared [`is_ssrf_blocked_url`](impresspress_core::ssrf::is_ssrf_blocked_url)
    /// gate — the same one the Cloudflare adapter applies — so a request whose
    /// URL *literally* names an internal target (a
    /// private/loopback/link-local/CGNAT IP literal, `localhost`, an
    /// IPv6-embedded-v4 form, or a well-known cloud-metadata hostname) is
    /// refused here rather than handed to `fetch`.
    ///
    /// This is not theoretical for a browser-hosted runtime. Blocks reach this
    /// service with URLs that come from configuration and from request
    /// payloads (webhook targets, model endpoints, image fetches), and the
    /// worker runs inside the user's own browser: a fetch to
    /// `http://192.168.1.1/` or `http://localhost:8080/` from here is a fetch
    /// from *inside the user's network*, against hosts nothing on the public
    /// internet can reach. That is a wider blast radius than the same bug on a
    /// server, not a narrower one.
    ///
    /// The gate sees the URL the caller asked for and nothing else, so a
    /// followed `3xx` would reach a second URL it never inspected. That half is
    /// closed on the JS side: `bridge.js`'s `httpFetch` issues every request
    /// with `redirect: 'error'` instead of the Fetch API's default `'follow'`,
    /// so `https://evil.example/x` answering `302 Location:
    /// http://169.254.169.254/…` fails the request rather than fetching the
    /// metadata service. `'manual'` would not do: a cross-origin redirect
    /// response is opaque, with no readable `Location` to revalidate. The
    /// native path revalidates per hop instead (reqwest's
    /// `ssrf_revalidating_redirect_policy`) because it has a hook for it; here
    /// a legitimate redirect surfaces as a request error, which is the right
    /// trade against a silent fetch of an internal address.
    ///
    /// Response bytes are capped at
    /// [`MAX_NETWORK_RESPONSE_BYTES`](impresspress_core::streaming::MAX_NETWORK_RESPONSE_BYTES),
    /// the cap shared with the Cloudflare adapter, and enforced on the JS side
    /// because that is where the bytes are read — an advertised
    /// `Content-Length` over the cap is refused before the body is touched, and
    /// the running total is checked per chunk for a chunked response that
    /// advertises nothing. A Service Worker shares one linear memory with
    /// everything else the page's runtime is doing, so the ceiling matters more
    /// here than on a server, not less.
    ///
    /// Honest boundary: this is a URL/host-literal precheck. Like the Worker
    /// path, it does NOT defend against DNS rebinding — a public-looking
    /// hostname that resolves to a private address at connect time still
    /// reaches `fetch`, because the Fetch API exposes no resolve-before-connect
    /// hook. The native backend closes that with an SSRF-filtering resolver;
    /// here it is the browser's own network partitioning that has to. With the
    /// initial URL gated and redirects refused, that is the sole residual.
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
        refuse_if_ssrf(&req.url)?;

        let headers_json = serde_json::to_string(&req.headers)
            .map_err(|e| NetworkError::Other(format!("failed to serialize headers: {e}")))?;

        let body_bytes: &[u8] = req.body.as_deref().unwrap_or(&[]);

        let js_val = bridge::http_fetch(
            &req.method,
            &req.url,
            &headers_json,
            body_bytes,
            MAX_NETWORK_RESPONSE_BYTES as f64,
        )
        .await
        .map_err(|e| NetworkError::RequestError(bridge::describe(&e)))?;

        // The bridge resolves a JS object `{ status, headers, body:
        // Uint8Array }` — decode it directly, with no JSON round-trip
        // (previously this called `JSON::stringify` on the resolved value and
        // fed the result to `serde_json::from_str`, which double-encoded every
        // response into a JSON string literal and failed with "invalid type:
        // string, expected struct").
        //
        // The body is NOT a one-step decode by default: `serde_wasm_bindgen`'s
        // bulk byte path is `deserialize_byte_buf`, which serde's `Vec<u8>`
        // never asks for. `FetchResponse::body` carries the `serde_bytes`
        // adapter for exactly that reason.
        let fetch_resp: FetchResponse = serde_wasm_bindgen::from_value(js_val).map_err(|e| {
            NetworkError::RequestError(format!("failed to decode fetch response: {e}"))
        })?;

        Ok(Response {
            status_code: fetch_resp.status,
            headers: group_headers(fetch_resp.headers),
            body: fetch_resp.body,
        })
    }

    /// Streams the response body chunk by chunk instead of taking the trait
    /// default, which calls [`do_request`](Self::do_request) and wraps the
    /// whole buffered body as a single chunk.
    ///
    /// That default is what ran here before. It is not a neutral fallback in
    /// this target: the buffer is in the Service Worker's linear memory, which
    /// sql.js and the rest of the runtime share, and a consumer that asked to
    /// stream did so precisely because it did not want the body resident.
    ///
    /// Goes through the same gates as the buffered path and shares both with
    /// it rather than restating them: [`refuse_if_ssrf`] before dispatch, and
    /// `bridge.js`'s `fetchInit` — `redirect: 'error'` included — for the
    /// request itself. See [`do_request`](Self::do_request)'s doc for why
    /// refusing a redirect is the other half of the URL gate, and for the DNS-
    /// rebinding residual neither half closes.
    ///
    /// [`MAX_NETWORK_RESPONSE_BYTES`] is enforced two ways, mirroring
    /// `impresspress-cloudflare`'s: an advertised `Content-Length` over the
    /// cap is refused before a byte streams, and the running total is checked
    /// per chunk, which is the only guard a chunked response has. Over the cap
    /// the stream ends in an `Error` terminal after the bytes already
    /// forwarded — never a silent truncation reported as a clean completion.
    async fn do_request_streaming(
        &self,
        req: &Request,
    ) -> Result<(ResponseHead, OutputStream), NetworkError> {
        refuse_if_ssrf(&req.url)?;

        let headers_json = serde_json::to_string(&req.headers)
            .map_err(|e| NetworkError::Other(format!("failed to serialize headers: {e}")))?;
        let body_bytes: &[u8] = req.body.as_deref().unwrap_or(&[]);

        let js_val = bridge::http_fetch_stream(&req.method, &req.url, &headers_json, body_bytes)
            .await
            .map_err(|e| NetworkError::RequestError(bridge::describe(&e)))?;

        let started: FetchStreamStart = match serde_wasm_bindgen::from_value(js_val.clone()) {
            Ok(started) => started,
            Err(e) => {
                // The reader id is inside the value that just failed to
                // decode, so without this the HTTP connection it holds could
                // be neither drained nor cancelled for the life of the Service
                // Worker.
                release_reader_in(&js_val).await;
                return Err(NetworkError::RequestError(format!(
                    "failed to decode fetch response head: {e}"
                )));
            }
        };

        let headers = group_headers(started.headers);
        let cap = MAX_NETWORK_RESPONSE_BYTES;

        // Refuse an over-large advertised length before any byte streams.
        if let Some(advertised) = advertised_length_over_cap(&headers, cap) {
            if let Some(id) = &started.stream_id {
                bridge::reader_cancel(id).await;
            }
            return Err(NetworkError::RequestError(format!(
                "response body {advertised} bytes exceeds cap of {cap} bytes"
            )));
        }

        let head = ResponseHead {
            status_code: started.status,
            headers,
        };

        // A bodyless response (204, HEAD) registers no reader; answer with an
        // empty body stream rather than failing the request, which is parity
        // with the buffered path's empty `Vec`.
        let body_stream = match started.stream_id {
            Some(stream_id) => {
                let what = format!("reading response body from {}", req.url);
                OutputStream::from_producer(move |sink, cancel| async move {
                    drain_reader_into(stream_id, Some(cap), &what, sink, cancel).await;
                })
            }
            None => OutputStream::respond(Vec::new()),
        };

        Ok((head, body_stream))
    }
}

/// The advertised body length when it is over `cap`, or `None` when the
/// response advertises nothing, advertises something unparseable, or advertises
/// a length that fits.
///
/// Pure, so the first of the two cap enforcement points is testable without a
/// live `fetch` — the second is the running total in
/// `storage::drain_into`. The header name is matched lowercase because
/// `bridge.js` builds the pair list from the Fetch API's own header iteration,
/// which lowercases names.
fn advertised_length_over_cap(headers: &HashMap<String, Vec<String>>, cap: usize) -> Option<usize> {
    headers
        .get("content-length")
        .and_then(|values| values.first())
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|advertised| *advertised > cap)
}

/// Group the wire's `(name, value)` pairs into the `name → [values]` map
/// `Response` carries, appending rather than replacing so every value of a
/// repeated header survives. Mirrors `collect_headers` in
/// `impresspress-cloudflare/src/network_service.rs`.
fn group_headers(pairs: Vec<(String, String)>) -> HashMap<String, Vec<String>> {
    let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in pairs {
        grouped.entry(name).or_default().push(value);
    }
    grouped
}

pub fn make_network_service(
) -> std::sync::Arc<dyn wafer_core::interfaces::network::service::NetworkService> {
    std::sync::Arc::new(BrowserNetworkService)
}

// `bridge::http_fetch` is a `#[wasm_bindgen(module = "/js/bridge.js")]` extern
// import backed by the page's real `fetch`, so the tests below never let a
// request get that far: they either assert on the decode step in isolation or
// on the SSRF gate, which returns before the bridge is touched.
#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use js_sys::{Array, Object, Reflect, Uint8Array};
    use wafer_core::interfaces::network::service::{NetworkError, NetworkService, Request};
    use wasm_bindgen::JsValue;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{BrowserNetworkService, FetchResponse};

    /// Build the JS object shape `bridge.js`'s `httpFetch` resolves:
    /// `{ status: number, headers: [[name, value], ...], body: Uint8Array }`.
    fn make_fetch_response_object(status: u16, headers: &[(&str, &str)], body: &[u8]) -> JsValue {
        let obj = Object::new();
        Reflect::set(
            &obj,
            &JsValue::from_str("status"),
            &JsValue::from_f64(status as f64),
        )
        .unwrap();

        let pairs = Array::new();
        for (name, value) in headers {
            let pair = Array::new();
            pair.push(&JsValue::from_str(name));
            pair.push(&JsValue::from_str(value));
            pairs.push(&pair);
        }
        Reflect::set(&obj, &JsValue::from_str("headers"), &pairs).unwrap();

        Reflect::set(
            &obj,
            &JsValue::from_str("body"),
            &Uint8Array::from(body).into(),
        )
        .unwrap();

        obj.into()
    }

    fn decode(value: JsValue) -> FetchResponse {
        serde_wasm_bindgen::from_value(value).expect("decode fetch response")
    }

    #[wasm_bindgen_test]
    fn decodes_js_object_response_in_one_step() {
        let body = b"hello world";
        let decoded = decode(make_fetch_response_object(
            200,
            &[("content-type", "application/json")],
            body,
        ));

        assert_eq!(decoded.status, 200);
        assert_eq!(decoded.body, body.to_vec());
        assert_eq!(
            decoded.headers,
            vec![("content-type".to_string(), "application/json".to_string())]
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
        let decoded = decode(obj.into());

        assert_eq!(decoded.status, 204);
        assert!(decoded.body.is_empty());
        assert!(decoded.headers.is_empty());
    }

    /// Regression guard for the double-encode bug: `bridge.js` used to
    /// `JSON.stringify` the response envelope into a JS *string*, and
    /// `network.rs` then called `JSON::stringify` on that string AGAIN before
    /// `serde_json::from_str::<FetchResponse>` — so every response failed with
    /// "invalid type: string, expected struct". If `httpFetch` ever regresses
    /// back to resolving a string instead of an object, decoding must fail
    /// loudly here rather than silently producing a wrong value.
    #[wasm_bindgen_test]
    fn old_json_string_shape_fails_to_decode_as_object() {
        let json_string = JsValue::from_str(r#"{"status":200,"headers":[],"body":[104,105]}"#);

        let result: Result<FetchResponse, _> = serde_wasm_bindgen::from_value(json_string);

        assert!(
            result.is_err(),
            "a JSON string must not decode as the FetchResponse object shape"
        );
    }

    /// **Fails on the pre-fix tree.** `headers` was a
    /// `HashMap<String, String>`, so the second `Set-Cookie` overwrote the
    /// first and a login response that set a session cookie and a CSRF cookie
    /// delivered only one of them. Both must survive, in order.
    #[wasm_bindgen_test]
    fn a_repeated_set_cookie_survives() {
        let decoded = decode(make_fetch_response_object(
            200,
            &[
                ("set-cookie", "session=abc; Path=/; HttpOnly"),
                ("content-type", "text/html"),
                ("set-cookie", "csrf=xyz; Path=/"),
            ],
            b"",
        ));

        let grouped = super::group_headers(decoded.headers);
        assert_eq!(
            grouped.get("set-cookie"),
            Some(&vec![
                "session=abc; Path=/; HttpOnly".to_string(),
                "csrf=xyz; Path=/".to_string(),
            ]),
            "both cookies must survive: {grouped:?}"
        );
        assert_eq!(
            grouped.get("content-type"),
            Some(&vec!["text/html".to_string()])
        );
    }

    fn get(url: &str) -> Request {
        Request {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: std::collections::HashMap::new(),
            body: None,
        }
    }

    async fn refusal_for(url: &str) -> String {
        match BrowserNetworkService.do_request(&get(url)).await {
            Err(NetworkError::RequestError(msg)) => msg,
            other => panic!("expected an SSRF refusal for {url}, got {other:?}"),
        }
    }

    async fn streaming_refusal_for(url: &str) -> String {
        // `OutputStream` is not `Debug`, so the success arm cannot be printed
        // — it is enough to say the request was not refused.
        match BrowserNetworkService.do_request_streaming(&get(url)).await {
            Err(NetworkError::RequestError(msg)) => msg,
            Err(other) => panic!("expected an SSRF refusal for {url}, got {other:?}"),
            Ok(_) => panic!("expected an SSRF refusal for {url}, the request was dispatched"),
        }
    }

    /// **Fails on the pre-fix tree**, where every one of these went straight to
    /// `fetch`. A browser-hosted runtime that will fetch a link-local or
    /// loopback address on request is server-side request forgery aimed at the
    /// user's own network — the gate has to run before the bridge, and these
    /// assertions prove it does, because they never reach the bridge at all.
    #[wasm_bindgen_test]
    async fn the_ssrf_gate_refuses_internal_targets_before_fetching() {
        for url in [
            "http://localhost/admin",
            "http://localhost:8080/admin",
            // The RFC 6761 pseudo-domain, which resolves to loopback in Chrome
            // and Firefox: in a browser-hosted runtime `app.localhost:3000` is
            // a live dev server and `api.localhost` an internal service. The
            // upstream classifier matches the bare string only.
            "http://api.localhost:8080/admin",
            "http://app.localhost:3000/",
            "http://localhost./",
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1/",
            "http://192.168.1.1/",
            "http://10.0.0.1/",
            "http://[::1]/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "file:///etc/passwd",
        ] {
            let msg = refusal_for(url).await;
            assert!(
                msg.starts_with("SSRF: refusing request to internal/blocked address:"),
                "{url} was not refused by the SSRF gate: {msg}"
            );
        }
    }

    /// The streaming entry point applies the SAME gate as the buffered one.
    /// **Fails on the pre-change tree** in a way worth spelling out: there was
    /// no streaming entry point at all, so `do_request_streaming` was the
    /// trait default, which called `do_request` and inherited its gate by
    /// accident. Now that this target implements the method, the gate has to
    /// be applied deliberately — which is why both go through
    /// `refuse_if_ssrf`, and why this test exists next to its buffered twin.
    #[wasm_bindgen_test]
    async fn the_streaming_path_refuses_the_same_internal_targets() {
        for url in [
            "http://localhost/admin",
            "http://api.localhost:8080/admin",
            "http://localhost./",
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1/",
            "http://192.168.1.1/",
            "http://[::1]/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "file:///etc/passwd",
        ] {
            let msg = streaming_refusal_for(url).await;
            assert!(
                msg.starts_with("SSRF: refusing request to internal/blocked address:"),
                "{url} was not refused by the SSRF gate on the streaming path: {msg}"
            );
        }
    }

    /// `httpFetchStream` resolves the head plus a reader id, not a body. A
    /// bodyless response (204, `HEAD`) carries `stream_id: null`, which is not
    /// an error — the buffered path answers an empty `Vec` for the same
    /// response.
    #[wasm_bindgen_test]
    fn the_streaming_head_decodes_with_and_without_a_body() {
        let with_body = Object::new();
        Reflect::set(
            &with_body,
            &JsValue::from_str("status"),
            &JsValue::from_f64(200.0),
        )
        .unwrap();
        let pairs = Array::new();
        let pair = Array::new();
        pair.push(&JsValue::from_str("content-length"));
        pair.push(&JsValue::from_str("11"));
        pairs.push(&pair);
        Reflect::set(&with_body, &JsValue::from_str("headers"), &pairs).unwrap();
        Reflect::set(
            &with_body,
            &JsValue::from_str("stream_id"),
            &JsValue::from_str("bytes-1"),
        )
        .unwrap();

        let decoded: super::FetchStreamStart =
            serde_wasm_bindgen::from_value(with_body.into()).expect("decode streaming head");
        assert_eq!(decoded.status, 200);
        assert_eq!(decoded.stream_id.as_deref(), Some("bytes-1"));
        assert_eq!(
            decoded.headers,
            vec![("content-length".to_string(), "11".to_string())]
        );

        let bodyless = Object::new();
        Reflect::set(
            &bodyless,
            &JsValue::from_str("status"),
            &JsValue::from_f64(204.0),
        )
        .unwrap();
        Reflect::set(&bodyless, &JsValue::from_str("headers"), &Array::new()).unwrap();
        Reflect::set(&bodyless, &JsValue::from_str("stream_id"), &JsValue::NULL).unwrap();

        let decoded: super::FetchStreamStart =
            serde_wasm_bindgen::from_value(bodyless.into()).expect("decode bodyless head");
        assert_eq!(decoded.status, 204);
        assert!(decoded.stream_id.is_none());
    }

    /// The first of the two `MAX_NETWORK_RESPONSE_BYTES` enforcement points:
    /// an advertised length over the cap is refused before a byte streams (the
    /// second is the running total, exercised in `storage::drain_loop`).
    ///
    /// A Service Worker shares one linear memory with everything else the
    /// page's runtime is doing, so nothing here may fall open on a header the
    /// server chose: an absent, unparseable or hostile `Content-Length` has to
    /// leave the running total as the guard rather than skip the check
    /// silently and admit the body.
    #[wasm_bindgen_test]
    fn an_advertised_length_over_the_cap_is_refused_before_the_body() {
        use super::advertised_length_over_cap;

        let headers = |value: &str| {
            let mut map = std::collections::HashMap::new();
            map.insert("content-length".to_string(), vec![value.to_string()]);
            map
        };

        assert_eq!(advertised_length_over_cap(&headers("101"), 100), Some(101));
        assert_eq!(advertised_length_over_cap(&headers("100"), 100), None);
        assert_eq!(advertised_length_over_cap(&headers("0"), 100), None);

        // Nothing advertised: a chunked response. Only the running total can
        // stop it, and it must not be refused up front either.
        assert_eq!(
            advertised_length_over_cap(&std::collections::HashMap::new(), 100),
            None
        );

        // Unparseable, negative, or overflowing values are not lengths. The
        // running total still bounds what actually arrives.
        for hostile in ["not-a-number", "-1", "99999999999999999999999999", ""] {
            assert_eq!(
                advertised_length_over_cap(&headers(hostile), 100),
                None,
                "unparseable Content-Length {hostile:?} must not be read as a length"
            );
        }

        // `bridge.js` builds the pair list from the Fetch API's own header
        // iteration, which lowercases names — so only the lowercase spelling
        // can appear, and matching the capitalized one would be dead code.
        let mut capitalized = std::collections::HashMap::new();
        capitalized.insert("Content-Length".to_string(), vec!["101".to_string()]);
        assert_eq!(advertised_length_over_cap(&capitalized, 100), None);
    }
}
