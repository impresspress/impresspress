//! HTTP ↔ Message conversion for the browser Service Worker adapter.
//!
//! Thin platform glue: the protocol mapping (method→action table, request
//! meta layout, the streaming decision, how a response's head and body are
//! rendered) lives in `wafer_block::http_codec` — the same implementation the
//! native listener and the Cloudflare adapter use. Only `web_sys` I/O lives here:
//! reading the request body/headers, the Service-Worker cookie re-injection,
//! and building `web_sys::Response` (buffered or `ReadableStream`-backed).

use futures::StreamExt;
use impresspress_core::streaming::{self, CappedCollect};
use js_sys::{ArrayBuffer, Uint8Array};
use wafer_block::{
    http_codec::{self, OpenedResponse, StreamedHttpResponse},
    streams::{input::InputStream, output::OutputStream},
    Message,
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
///
/// A body over [`streaming::MAX_REQUEST_BODY_BYTES`] is dropped rather than
/// dispatched: the message is marked with
/// [`streaming::META_REQ_BODY_TOO_LARGE`] and paired with an empty
/// `InputStream`, and `impresspress_core::pipeline::handle_request` answers
/// 413 from inside the flow — where the CORS and security headers and the
/// `request_logs` row are. An `Err` here becomes the Service Worker's own
/// failure and the client sees a fetch that died rather than a status; a
/// `Response` built here would skip the flow.
///
/// Unlike the Cloudflare adapter there is no pre-read check to make: a
/// `Request` handed to a Service Worker is read through `array_buffer()`, so
/// the bytes are resident before their length can be measured. The cap
/// changes the status, not the peak memory.
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

    // Read body bytes via ArrayBuffer, and measure them against the shared
    // transport cap. An over-cap body is dropped here rather than copied out.
    let (body, too_large): (Vec<u8>, bool) = {
        let promise = request.array_buffer()?;
        let ab_val = JsFuture::from(promise).await?;
        let ab: ArrayBuffer = ab_val.dyn_into()?;
        let arr = Uint8Array::new(&ab);
        if arr.length() as usize > streaming::MAX_REQUEST_BODY_BYTES {
            (Vec::new(), true)
        } else {
            (arr.to_vec(), false)
        }
    };

    // Collect headers into (name, value) pairs for the codec.
    let mut header_pairs: Vec<(String, String)> = Vec::new();
    let mut saw_host = false;
    let mut saw_fetch_site = false;
    let headers: Headers = request.headers();
    let iter =
        js_sys::try_iter(&headers)?.ok_or_else(|| JsValue::from_str("headers not iterable"))?;
    for item in iter {
        let item = item?;
        // Each entry is a JS Array [name, value].
        let arr: js_sys::Array = item.dyn_into()?;
        let key = arr.get(0).as_string().unwrap_or_default();
        let val = arr.get(1).as_string().unwrap_or_default();
        let lower = key.to_ascii_lowercase();
        saw_host |= lower == "host";
        saw_fetch_site |= lower == "sec-fetch-site";
        header_pairs.push((key, val));
    }

    // Synthesize the request metadata a service worker cannot be sent.
    //
    // `Host` and `Sec-Fetch-Site` are both absent from `FetchEvent.request`:
    // `Host` is a forbidden header name the `Headers` view never exposes, and
    // the Fetch spec appends `Sec-Fetch-*` during HTTP-network-or-cache fetch
    // — *after* service-worker interception. So does `Origin`, and `Referer`.
    // The result is that `impresspress_core::csrf::enforce_origin_policy` —
    // which reads `sec-fetch-site` first and falls back to `origin`/`referer`
    // against `host` — finds nothing at all and takes its fail-closed tail,
    // rejecting **every** cookie-authenticated mutation the browser bundle
    // makes. Not a sandbox problem: it is every `fetch`-driven admin form.
    //
    // The worker can prove what the missing headers would have said, and the
    // proof is the set of requests that can reach a service worker at all:
    //
    // * a subresource request (`fetch`, `XHR`, a form posted by script) is
    //   dispatched to the worker only when the *client that issued it* is
    //   controlled by this worker — which requires that client to be
    //   same-origin with the worker's scope. A cross-site page's request to
    //   this origin is never handed to this worker; it goes straight to the
    //   network (or to that page's own worker). So a same-origin request URL
    //   on a non-navigation really is a same-origin request.
    // * a navigation into the scope is dispatched to the worker whoever
    //   started it, which is exactly the CSRF case a cross-site `<form>`
    //   POST uses. `Request::referrer` — an attribute, not a header, and so
    //   readable here — is the only thing that can separate our own page
    //   from somebody else's, and a referrer that is ABSENT separates
    //   nothing: suppression is attacker-controllable, so it is refused
    //   rather than reported as `Sec-Fetch-Site: none` (which the policy
    //   accepts). See [`fetch_site_for`] for the full argument.
    //
    // Anything that does not positively match one of those is `cross-site`,
    // so a value this cannot prove stays a refusal. A header that really is
    // present is never overwritten — a real `Sec-Fetch-Site` from a client
    // that sends one outranks anything inferred here.
    //
    // Outside a worker global (a unit test, or a main-thread caller) the
    // worker's own origin is unknowable, and nothing is synthesized at all:
    // the policy then fails closed exactly as it did before this ran.
    if let Some(location) = worker_location() {
        if !saw_host {
            // Defence in depth, and **not the live path**. `host` only matters
            // to `csrf::enforce_origin_policy`'s `origin`/`referer` fallback,
            // and that fallback is unreachable whenever this block runs at
            // all: the `sec-fetch-site` synthesized just below is
            // unconditional inside this `if`, and the policy reads
            // `sec-fetch-site` *first* and returns on it. So a future reader
            // should not assume the fallback is exercised by any test here —
            // it is what would carry the policy if the `Sec-Fetch-Site` arm
            // were ever removed, or if a client sent a `host` header of its
            // own that this branch then declines to overwrite.
            header_pairs.push(("host".to_string(), location.host));
        }
        if !saw_fetch_site {
            header_pairs.push((
                "sec-fetch-site".to_string(),
                fetch_site_for(
                    request_mode(request),
                    &request.referrer(),
                    &url.origin(),
                    &location.origin,
                )
                .to_string(),
            ));
        }
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
    // from the method+path. Paths are served exactly as received — nothing in
    // the request path rewrites them.
    let mut msg = http_codec::build_http_message(
        &method,
        &path,
        &raw_query,
        "127.0.0.1",
        header_pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    );

    if too_large {
        msg.set_meta(
            streaming::META_REQ_BODY_TOO_LARGE,
            streaming::BODY_TOO_LARGE_VALUE,
        );
        return Ok((msg, InputStream::empty()));
    }

    Ok((msg, InputStream::from_bytes(body)))
}

/// The worker's own `origin` and `host`, as its global `location` reports them.
struct WorkerLocation {
    /// `scheme://host[:port]` — what a request URL's origin is compared to.
    origin: String,
    /// `host[:port]` — the authority `csrf::enforce_origin_policy` compares an
    /// `Origin`/`Referer` header against.
    host: String,
}

/// Read the worker global's `location`, or `None` when there is no worker
/// global (a `wasm-pack test --node` harness, or a main-thread caller).
///
/// `None` is deliberately not a fallback to the request's own origin: that
/// would let the request under inspection supply the authority it is checked
/// against, which is not a check at all.
fn worker_location() -> Option<WorkerLocation> {
    let scope = js_sys::global()
        .dyn_into::<web_sys::WorkerGlobalScope>()
        .ok()?;
    let location = scope.location();
    Some(WorkerLocation {
        origin: location.origin(),
        host: location.host(),
    })
}

/// `Request::mode` as the Fetch spec spells it, or `""` for a value this
/// build of `web-sys` does not name.
fn request_mode(request: &web_sys::Request) -> &'static str {
    match request.mode() {
        web_sys::RequestMode::SameOrigin => "same-origin",
        web_sys::RequestMode::Cors => "cors",
        web_sys::RequestMode::NoCors => "no-cors",
        web_sys::RequestMode::Navigate => "navigate",
        _ => "",
    }
}

/// What `Sec-Fetch-Site` a service worker can *prove* for a request it was
/// handed. Pure, so every arm is testable without a browser.
///
/// * `mode` — `Request::mode`, as [`request_mode`] spells it.
/// * `referrer` — `Request::referrer`; `""` when the request has none.
/// * `request_origin` — the origin of the request's own URL.
/// * `self_origin` — the worker's own origin ([`worker_location`]).
///
/// The security argument is at the call site. The rule, restated: a
/// non-navigation reaching this worker came from a client this worker
/// controls, so a same-origin URL makes it same-origin; a navigation is
/// same-origin only when its referrer says so; everything else — **including
/// a navigation with no referrer at all** — is `cross-site`, which is what an
/// unprovable case must resolve to.
///
/// # Why a referrer-less navigation is not `none`
///
/// `Sec-Fetch-Site: none` means "no initiator" — a typed URL, a bookmark — and
/// `csrf::enforce_origin_policy` accepts it. A service worker cannot tell that
/// apart from a navigation whose referrer was **suppressed**, and suppression
/// is attacker-controllable: `<form referrerpolicy="no-referrer">`, a
/// `<meta name="referrer">` on the attacking page, or a redirect that drops
/// it. A same-site sibling's top-level `<form>` POST is exactly the shape
/// `SameSite=Lax` still attaches the `auth_token` cookie to, so answering
/// `none` here would hand that request the CSRF check's approval.
///
/// Nothing legitimate is lost by refusing it. This value is only ever
/// consulted for a cookie-authenticated **unsafe** method (the policy returns
/// early otherwise), and a same-origin form POST carries a referrer under the
/// `Referrer-Policy: strict-origin-when-cross-origin` the security-headers
/// block sets — a bookmark or typed URL is a `GET`, which never reaches the
/// check at all.
fn fetch_site_for(
    mode: &str,
    referrer: &str,
    request_origin: &str,
    self_origin: &str,
) -> &'static str {
    // An empty `self_origin` would make `"" == ""` true for a request whose
    // origin is also unreadable, turning two unknowns into a same-origin
    // verdict. Refuse before any comparison can do that.
    if self_origin.is_empty() {
        return "cross-site";
    }
    match mode {
        // The client asked for a same-origin-only fetch and got one; the
        // browser would have failed it otherwise.
        "same-origin" => "same-origin",
        // The default for `fetch()` is `cors`, so this is the ordinary
        // same-origin API call. `no-cors` covers `<img>`, `<script>` and
        // `sendBeacon` from the same controlled client.
        "cors" | "no-cors" => {
            if request_origin == self_origin {
                "same-origin"
            } else {
                "cross-site"
            }
        }
        // A navigation is judged on its referrer, and ONLY a referrer that
        // is provably ours passes. An absent one is refused rather than
        // reported as `none` — see the note above; this function never
        // returns `none`.
        //
        // `collapsible_match` wants this folded into a match guard, with the
        // refusal falling through to `_` below. Refused on purpose: the
        // refusal is the security-relevant half, and as a fall-through its
        // target is whatever arm happens to sit last. Any arm added between
        // this one and `_` would silently take it over. Kept adjacent to the
        // condition it belongs to, so the verdict does not depend on arm
        // order.
        //
        // `"cors" | "no-cors"` above is the same shape and clippy does not
        // flag it — not because an or-pattern cannot carry a guard (it can:
        // `"cors" | "no-cors" if cond => ..` compiles), simply because the
        // lint does not fire there.
        //
        // The lint itself is toolchain-dependent: measured absent on 1.94.0
        // and present on 1.98.0. On a toolchain that does not fire it the
        // `expect` below reports itself as unfulfilled, which is the honest
        // signal — an `allow` would have read as inert instead.
        #[expect(
            clippy::collapsible_match,
            reason = "the suggested guard leaves the refusal as a fall-through to \
                      whatever arm sits last; keeping it adjacent to its condition \
                      is what makes the verdict independent of arm order"
        )]
        "navigate" => {
            if !referrer.is_empty() && origin_of(referrer) == self_origin {
                "same-origin"
            } else {
                "cross-site"
            }
        }
        _ => "cross-site",
    }
}

/// The `scheme://authority` prefix of an absolute URL, or `""` when the string
/// is not one.
///
/// Deliberately not `web_sys::Url` (a JS global this crate's pure tests must
/// not need) and deliberately not lenient: `about:client` — the referrer
/// placeholder a `Request` can carry before the referrer is resolved — has no
/// authority, returns `""`, and is therefore judged cross-site.
fn origin_of(url: &str) -> &str {
    let Some(after_scheme) = url.find("://").map(|i| i + 3) else {
        return "";
    };
    match url[after_scheme..].find('/') {
        Some(slash) => &url[..after_scheme + slash],
        None => url,
    }
}

// ---------------------------------------------------------------------------
// Response conversion
// ---------------------------------------------------------------------------

/// Header pairs, as the codec renders them, in a `web_sys::Headers`.
///
/// Appended in order, as `http_codec::HttpResponseParts` tells a transport
/// to and as the Cloudflare adapter does: the codec already keeps one pair
/// per header name (case-insensitively, the last write winning), so the
/// only name a rendering repeats is `Set-Cookie`, and each cookie must
/// reach the client.
fn headers_from(pairs: &[(String, String)]) -> Result<Headers, JsValue> {
    let headers = Headers::new()?;
    for (name, value) in pairs {
        headers.append(name, value)?;
    }
    Ok(headers)
}

/// Apply transport-neutral [`http_codec::HttpResponseParts`] to a
/// `web_sys::Response`.
fn parts_to_response(parts: http_codec::HttpResponseParts) -> Result<web_sys::Response, JsValue> {
    make_response(parts.body, parts.status, headers_from(&parts.headers)?)
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

/// Convert a WAFER `OutputStream` into a browser `web_sys::Response`.
///
/// `http_codec::open_response` decides, as it does for the native listener
/// and Cloudflare, so the browser cannot disagree with either about whether
/// a response streams or how it is rendered:
///
/// 1. **Streamed** — the producer declared streaming in leading `Meta` (a
///    streaming `resp.content_type` such as SSE, or the `resp.stream` marker
///    a download sets on its real content type) and has its first bytes. The
///    codec has rendered the head, and the body is piped into a
///    `ReadableStream` as it arrives — so a multi-minute SSE response isn't
///    held back behind a buffer that flushes at the very end (which Chrome's
///    idle keep-alive treats as a hung fetch and drops with
///    `net::ERR_FAILED`), and a large download never sits in the Service
///    Worker's heap whole. See [`streamed_response`].
/// 2. **Buffered** — everything else. Read whole under
///    [`streaming::MAX_BUFFERED_RESPONSE_BYTES`], so an over-large body
///    becomes a clean **413** instead of exhausting the one linear memory the
///    whole page's runtime shares, and rendered by
///    `http_codec::terminal_to_http_response`, the native listener's
///    renderer: status, headers, drift decisions and the `500` for meta no
///    transport can send are the codec's.
pub async fn output_to_response(output: OutputStream) -> Result<web_sys::Response, JsValue> {
    match http_codec::open_response(output).await {
        OpenedResponse::Streamed(streamed) => streamed_response(streamed),
        OpenedResponse::Buffered(output) => {
            match streaming::collect_capped(output, streaming::MAX_BUFFERED_RESPONSE_BYTES).await {
                CappedCollect::Terminal(terminal) => {
                    parts_to_response(http_codec::terminal_to_http_response(terminal))
                }
                CappedCollect::OverLimit => {
                    let headers = Headers::new()?;
                    headers.set("Content-Type", "text/plain; charset=utf-8")?;
                    make_response(b"payload too large".to_vec(), 413, headers)
                }
            }
        }
    }
}

/// A `web_sys::Response` backed by a `ReadableStream`: the status and headers
/// the codec rendered from the leading meta, committed before the body
/// finishes, plus `Content-Length` when the response declared its length (a
/// stored object's size), so the page sees the download's size and progress.
///
/// The body is the codec's `StreamedBody`. Every ending it frames as an
/// `Err` — an `Error` terminal (also how wafer-run ends a stream whose
/// producer stopped without one), no terminal at all, a `Halt`/`Drop`/
/// `Continue` after the body started, a body that breaks its declared length
/// — errors the JS stream and aborts the response body. The status is
/// already committed by then, so an error cannot be downgraded to a 413/500;
/// a reader that gets an abort knows the bytes are incomplete, which a clean
/// end-of-stream would not tell it.
fn streamed_response(streamed: StreamedHttpResponse) -> Result<web_sys::Response, JsValue> {
    let StreamedHttpResponse {
        status,
        headers,
        content_length,
        body,
    } = streamed;
    let headers = headers_from(&headers)?;
    if let Some(length) = content_length {
        headers.set("Content-Length", &length.to_string())?;
    }
    let body = body.map(|chunk| {
        chunk
            .map(|bytes| JsValue::from(Uint8Array::from(bytes.as_slice())))
            .map_err(|err| {
                JsValue::from_str(&format!(
                    "impresspress-browser: streaming response aborted: {}",
                    err.message
                ))
            })
    });
    let init = ResponseInit::new();
    init.set_status(status);
    init.set_headers(&headers);
    web_sys::Response::new_with_opt_readable_stream_and_init(
        Some(&wasm_streams::ReadableStream::from_stream(body).into_raw()),
        &init,
    )
}

// ---------------------------------------------------------------------------
// The `Sec-Fetch-Site` mapping
// ---------------------------------------------------------------------------

/// `impresspress-browser` only compiles for `wasm32`, so these run under
/// `wasm-pack test --node` — the same harness `storage.rs` and `bridge.rs`
/// use. [`fetch_site_for`] and [`origin_of`] are pure, so nothing here needs a
/// worker, a `Request` or a network.
#[cfg(all(test, target_arch = "wasm32"))]
mod fetch_site_tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{fetch_site_for, origin_of};

    const SELF_ORIGIN: &str = "https://dev.impresspress.org";

    #[wasm_bindgen_test]
    fn a_same_origin_mode_request_is_same_origin() {
        assert_eq!(
            fetch_site_for("same-origin", "", SELF_ORIGIN, SELF_ORIGIN),
            "same-origin"
        );
    }

    /// The ordinary case: `fetch('/b/dev/api/files/write', {method:'POST'})`
    /// from a page this worker controls. `fetch`'s default mode is `cors`.
    #[wasm_bindgen_test]
    fn a_cors_request_for_our_own_origin_is_same_origin() {
        assert_eq!(
            fetch_site_for("cors", "", SELF_ORIGIN, SELF_ORIGIN),
            "same-origin"
        );
        assert_eq!(
            fetch_site_for("no-cors", "", SELF_ORIGIN, SELF_ORIGIN),
            "same-origin"
        );
    }

    #[wasm_bindgen_test]
    fn a_cors_request_for_another_origin_is_cross_site() {
        assert_eq!(
            fetch_site_for("cors", "", "https://evil.example", SELF_ORIGIN),
            "cross-site"
        );
        assert_eq!(
            fetch_site_for("no-cors", "", "https://evil.example", SELF_ORIGIN),
            "cross-site"
        );
    }

    /// Clicking a link on our own page.
    #[wasm_bindgen_test]
    fn a_navigation_referred_by_our_own_page_is_same_origin() {
        assert_eq!(
            fetch_site_for(
                "navigate",
                "https://dev.impresspress.org/b/admin/",
                SELF_ORIGIN,
                SELF_ORIGIN
            ),
            "same-origin"
        );
    }

    /// A referrer-less navigation is REFUSED, not reported as `none`.
    ///
    /// `none` would mean "no initiator" (a typed URL, a bookmark) and
    /// `csrf::enforce_origin_policy` accepts it — but a worker cannot tell
    /// that from a navigation whose referrer an attacker suppressed with
    /// `referrerpolicy="no-referrer"`, a `<meta name=referrer>`, or a
    /// redirect. A same-site sibling's top-level `<form>` POST carries the
    /// `SameSite=Lax` cookie, so `none` here would be a CSRF bypass. Nothing
    /// legitimate is lost: this value is only consulted for a
    /// cookie-authenticated unsafe method, and a real same-origin form POST
    /// has a referrer.
    #[wasm_bindgen_test]
    fn a_navigation_with_no_referrer_is_cross_site() {
        assert_eq!(
            fetch_site_for("navigate", "", SELF_ORIGIN, SELF_ORIGIN),
            "cross-site"
        );
    }

    /// Stated on its own because it is the property the arm above exists for:
    /// nothing this function can answer is ever `none`.
    #[wasm_bindgen_test]
    fn no_input_produces_none() {
        for mode in [
            "same-origin",
            "cors",
            "no-cors",
            "navigate",
            "websocket",
            "",
        ] {
            for referrer in ["", "about:client", SELF_ORIGIN, "https://evil.example/a"] {
                for request_origin in ["", SELF_ORIGIN, "https://evil.example"] {
                    assert_ne!(
                        fetch_site_for(mode, referrer, request_origin, SELF_ORIGIN),
                        "none",
                        "mode {mode}, referrer {referrer}, origin {request_origin}",
                    );
                }
            }
        }
    }

    /// The CSRF case this whole mapping exists to keep refused: a cross-site
    /// page posting a `<form>` at us navigates into the worker's scope, so the
    /// worker *does* see it.
    #[wasm_bindgen_test]
    fn a_navigation_referred_by_someone_else_is_cross_site() {
        assert_eq!(
            fetch_site_for(
                "navigate",
                "https://evil.example/attack.html",
                SELF_ORIGIN,
                SELF_ORIGIN
            ),
            "cross-site"
        );
    }

    /// `about:client` has no authority, so it cannot be shown to be ours.
    #[wasm_bindgen_test]
    fn a_navigation_with_an_unparseable_referrer_is_cross_site() {
        assert_eq!(
            fetch_site_for("navigate", "about:client", SELF_ORIGIN, SELF_ORIGIN),
            "cross-site"
        );
    }

    /// A mode this build of `web-sys` does not name resolves to `""`, and an
    /// unprovable case is a refusal.
    #[wasm_bindgen_test]
    fn an_unknown_mode_is_cross_site() {
        assert_eq!(
            fetch_site_for("", "", SELF_ORIGIN, SELF_ORIGIN),
            "cross-site"
        );
        assert_eq!(
            fetch_site_for("websocket", "", SELF_ORIGIN, SELF_ORIGIN),
            "cross-site"
        );
    }

    /// Two unknowns must not compare equal into a same-origin verdict.
    #[wasm_bindgen_test]
    fn an_unknown_self_origin_is_cross_site_whatever_the_request_says() {
        for mode in ["same-origin", "cors", "no-cors", "navigate"] {
            assert_eq!(
                fetch_site_for(mode, "", "", ""),
                "cross-site",
                "mode {mode} with no worker origin",
            );
        }
    }

    #[wasm_bindgen_test]
    fn origin_of_takes_the_scheme_and_authority_only() {
        assert_eq!(origin_of("https://a.example/b/c?d=e"), "https://a.example");
        assert_eq!(origin_of("http://127.0.0.1:8082/"), "http://127.0.0.1:8082");
        assert_eq!(origin_of("http://127.0.0.1:8082"), "http://127.0.0.1:8082");
        assert_eq!(origin_of("about:client"), "");
        assert_eq!(origin_of(""), "");
    }
}

/// The two response-shaping defects this adapter carried: a response that
/// declared streaming intent was buffered anyway unless its content type
/// happened to be SSE or `application/octet-stream`, and a buffered response
/// had no ceiling at all.
///
/// These build real `OutputStream`s and real `web_sys::Response`s, so they run
/// under `wasm-pack test --node` (Node ≥18 provides `Response`/`Headers`, and
/// `OutputStream::from_producer` is `wasm_bindgen_futures::spawn_local` on this
/// target).
#[cfg(all(test, target_arch = "wasm32"))]
mod response_tests {
    use std::{cell::Cell, rc::Rc};

    use futures::channel::oneshot;
    use impresspress_core::streaming::MAX_BUFFERED_RESPONSE_BYTES;
    use wafer_block::{
        http_codec::{META_RESP_CONTENT_LENGTH, META_RESP_STREAM, STREAM_MARKER_VALUE},
        meta::META_RESP_CONTENT_TYPE,
        streams::output::OutputStream,
        ErrorCode, MetaEntry, WaferError,
    };
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::output_to_response;

    fn meta(key: &str, value: &str) -> MetaEntry {
        MetaEntry {
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    /// Yield to the JS microtask queue once.
    async fn tick() {
        let _ = JsFuture::from(js_sys::Promise::resolve(&JsValue::UNDEFINED)).await;
    }

    /// **Fails on the pre-fix tree.** A download handler declares its response
    /// streams with the `resp.stream` marker on its real content type — an
    /// `application/pdf` is not one of the two streaming content-type families
    /// the old private `is_streaming_content_type` recognised, so the browser
    /// buffered the whole object into the Service Worker's heap while
    /// Cloudflare, reading the same marker, streamed it. Both now open every
    /// response through `http_codec::open_response`.
    ///
    /// "Streamed" is asserted the only way it is observable: the response
    /// resolves while the producer is still parked mid-body. A buffered
    /// implementation cannot return until the terminal arrives, so it resolves
    /// only after the release below and fails on `released` — it does not hang.
    #[wasm_bindgen_test]
    async fn a_marked_stream_response_streams_rather_than_buffering() {
        let (release_tx, release_rx) = oneshot::channel::<()>();

        let stream = OutputStream::from_producer(move |sink, _cancel| async move {
            let _ = sink
                .send_meta(meta(META_RESP_STREAM, STREAM_MARKER_VALUE))
                .await;
            let _ = sink
                .send_meta(meta(META_RESP_CONTENT_TYPE, "application/pdf"))
                .await;
            let _ = sink.send_chunk(b"%PDF-1.7 first".to_vec()).await;
            // Park mid-body: a real download is still reading from storage here.
            let _ = release_rx.await;
            let _ = sink.send_chunk(b" rest".to_vec()).await;
            let _ = sink.complete(Vec::new()).await;
        });

        let released = Rc::new(Cell::new(false));
        wasm_bindgen_futures::spawn_local({
            let released = released.clone();
            async move {
                // Enough turns for a buffering implementation to have settled
                // into waiting on the producer before the body is released.
                for _ in 0..32 {
                    tick().await;
                }
                released.set(true);
                let _ = release_tx.send(());
            }
        });

        let resp = output_to_response(stream).await.expect("build response");

        assert!(
            !released.get(),
            "the response only resolved after the body completed — it buffered"
        );
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers().get("content-type").unwrap().as_deref(),
            Some("application/pdf"),
            "the declared content type must survive the streaming path"
        );
        assert!(
            resp.body().is_some(),
            "a streamed response is backed by a ReadableStream"
        );
    }

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
            let _ = sink
                .send_meta(meta(META_RESP_STREAM, STREAM_MARKER_VALUE))
                .await;
            let _ = sink
                .send_meta(meta(META_RESP_CONTENT_TYPE, "application/pdf"))
                .await;
            let _ = sink
                .send_meta(meta("resp.header.X-Frame-Options", "DENY"))
                .await;
            let _ = sink
                .send_meta(meta(
                    "resp.header.Content-Disposition",
                    "inline\r\nX-Injected: 1",
                ))
                .await;
            let _ = sink.send_chunk(b"%PDF-1.7".to_vec()).await;
            let _ = sink.complete(Vec::new()).await;
        });

        let resp = output_to_response(stream).await.expect("build response");

        assert_eq!(resp.status(), 500);
        assert_eq!(
            resp.headers().get("content-disposition").unwrap(),
            None,
            "the refused header is never sent"
        );
        assert_eq!(
            resp.headers().get("x-frame-options").unwrap().as_deref(),
            Some("DENY"),
            "the terminal's security headers survive on its 500"
        );
        assert_eq!(
            resp.headers().get("cache-control").unwrap().as_deref(),
            Some("no-store"),
            "the 500 must not be cached"
        );
    }

    /// A buffered answer whose meta no transport can send is the codec's 500
    /// as well: the buffered path is `http_codec::collect_http_response`'s.
    #[wasm_bindgen_test]
    async fn a_buffered_answer_with_an_unsendable_header_is_a_500() {
        let stream = OutputStream::respond_with_meta(
            b"<p>ok</p>".to_vec(),
            vec![meta("resp.header.X-Note", "caf\u{e9}")],
        );

        let resp = output_to_response(stream).await.expect("build response");

        assert_eq!(resp.status(), 500);
        assert_eq!(resp.headers().get("x-note").unwrap(), None);
    }

    /// A marked stream that terminates before any body chunk (an empty
    /// download) still renders — it takes the short buffered form rather than
    /// building a `ReadableStream` with nothing in it.
    #[wasm_bindgen_test]
    async fn a_marked_stream_with_no_body_still_renders() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            let _ = sink
                .send_meta(meta(META_RESP_STREAM, STREAM_MARKER_VALUE))
                .await;
            let _ = sink
                .send_meta(meta(META_RESP_CONTENT_TYPE, "application/pdf"))
                .await;
            let _ = sink.complete(Vec::new()).await;
        });

        let resp = output_to_response(stream).await.expect("build response");

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers().get("content-type").unwrap().as_deref(),
            Some("application/pdf")
        );
    }

    /// A streaming content type with no marker still streams — the marker is
    /// an addition to the old rule, not a replacement for it.
    #[wasm_bindgen_test]
    async fn a_streaming_content_type_still_streams_without_the_marker() {
        let (release_tx, release_rx) = oneshot::channel::<()>();

        let stream = OutputStream::from_producer(move |sink, _cancel| async move {
            let _ = sink
                .send_meta(meta(META_RESP_CONTENT_TYPE, "text/event-stream"))
                .await;
            let _ = sink.send_chunk(b"data: one\n\n".to_vec()).await;
            let _ = release_rx.await;
            let _ = sink.complete(Vec::new()).await;
        });

        let released = Rc::new(Cell::new(false));
        wasm_bindgen_futures::spawn_local({
            let released = released.clone();
            async move {
                for _ in 0..32 {
                    tick().await;
                }
                released.set(true);
                let _ = release_tx.send(());
            }
        });

        let resp = output_to_response(stream).await.expect("build response");

        assert!(!released.get(), "SSE must not be buffered");
        assert_eq!(
            resp.headers().get("content-type").unwrap().as_deref(),
            Some("text/event-stream")
        );
    }

    /// **Fails on the pre-fix tree**, which had no cap: the buffered path
    /// concatenated whatever arrived until the Service Worker's linear memory
    /// gave out, taking the page's whole runtime with it. Over the cap is a
    /// clean 413.
    ///
    /// The first chunk is one byte and the second is the whole cap, so the
    /// collector rejects on the second before copying it — the peak allocation
    /// is the one oversized chunk this test creates, not two of them.
    #[wasm_bindgen_test]
    async fn a_buffered_response_over_the_cap_is_413() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            let _ = sink.send_chunk(vec![b'x'; 1]).await;
            let _ = sink
                .send_chunk(vec![b'x'; MAX_BUFFERED_RESPONSE_BYTES])
                .await;
            let _ = sink.complete(Vec::new()).await;
        });

        let resp = output_to_response(stream).await.expect("build response");

        assert_eq!(resp.status(), 413);
        assert_eq!(
            resp.headers().get("content-type").unwrap().as_deref(),
            Some("text/plain; charset=utf-8")
        );
    }

    /// **Fails on the pre-fix tree.** A download whose storage read fails
    /// mid-body used to end the `ReadableStream` cleanly (a `console.warn` and
    /// `return` from the hand-written pump), so the browser saw a normal
    /// end-of-body: the partial file landed on disk looking complete. No
    /// download path set `Content-Length` then, so the reader had no other way
    /// to tell. Through the codec's `StreamedBody` — the framing Cloudflare
    /// and the native listener use — the `Error` terminal errors the stream,
    /// and reading the body rejects.
    #[wasm_bindgen_test]
    async fn a_mid_body_error_aborts_the_body_instead_of_ending_it() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            let _ = sink
                .send_meta(meta(META_RESP_STREAM, STREAM_MARKER_VALUE))
                .await;
            let _ = sink
                .send_meta(meta(META_RESP_CONTENT_TYPE, "application/pdf"))
                .await;
            let _ = sink.send_chunk(b"%PDF-1.7 first half".to_vec()).await;
            let _ = sink
                .error(WaferError::new(
                    ErrorCode::Unavailable,
                    "object read failed",
                ))
                .await;
        });

        let resp = output_to_response(stream).await.expect("build response");

        // The status is already committed by the time the read fails — the
        // signal is the aborted body, not the status.
        assert_eq!(resp.status(), 200);
        let read = JsFuture::from(resp.array_buffer().expect("array_buffer")).await;
        assert!(
            read.is_err(),
            "a truncated download must not read back as a complete body"
        );
    }

    /// A producer that stops mid-body without a terminal — it returned early,
    /// panicked or was cancelled — aborts the body too. wafer-run ends such a
    /// stream with an `Error` terminal, and the status is already committed,
    /// so the aborted body is the only way left to say the file is not whole.
    #[wasm_bindgen_test]
    async fn a_producer_that_stops_mid_body_aborts_the_body() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            let _ = sink
                .send_meta(meta(META_RESP_STREAM, STREAM_MARKER_VALUE))
                .await;
            let _ = sink
                .send_meta(meta(META_RESP_CONTENT_TYPE, "application/pdf"))
                .await;
            let _ = sink.send_chunk(b"%PDF-1.7 first half".to_vec()).await;
        });

        let resp = output_to_response(stream).await.expect("build response");

        assert_eq!(resp.status(), 200);
        let read = JsFuture::from(resp.array_buffer().expect("array_buffer")).await;
        assert!(
            read.is_err(),
            "a body cut off without a terminal must not read back as complete"
        );
    }

    /// A download that declares its length (`resp.content_length`, the
    /// stored object's size) is answered with that `Content-Length`, and the
    /// body delivers exactly those bytes. One that declares none has none.
    #[wasm_bindgen_test]
    async fn a_declared_length_is_sent_as_content_length() {
        let download = |length: Option<&'static str>| {
            OutputStream::from_producer(move |sink, _cancel| async move {
                let _ = sink
                    .send_meta(meta(META_RESP_STREAM, STREAM_MARKER_VALUE))
                    .await;
                let _ = sink
                    .send_meta(meta(META_RESP_CONTENT_TYPE, "application/pdf"))
                    .await;
                if let Some(length) = length {
                    let _ = sink.send_meta(meta(META_RESP_CONTENT_LENGTH, length)).await;
                }
                let _ = sink.send_chunk(b"abc".to_vec()).await;
                let _ = sink.send_chunk(b"def".to_vec()).await;
                let _ = sink.complete(Vec::new()).await;
            })
        };

        let resp = output_to_response(download(Some("6")))
            .await
            .expect("build response");
        assert_eq!(
            resp.headers().get("content-length").unwrap().as_deref(),
            Some("6")
        );
        let text = JsFuture::from(resp.text().expect("text"))
            .await
            .expect("read");
        assert_eq!(text.as_string().as_deref(), Some("abcdef"));

        let resp = output_to_response(download(None))
            .await
            .expect("build response");
        assert_eq!(resp.headers().get("content-length").unwrap(), None);
    }

    /// A body that runs past its declared length is aborted, not ended.
    #[wasm_bindgen_test]
    async fn a_body_overrunning_its_declared_length_aborts() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            let _ = sink
                .send_meta(meta(META_RESP_STREAM, STREAM_MARKER_VALUE))
                .await;
            let _ = sink.send_meta(meta(META_RESP_CONTENT_LENGTH, "5")).await;
            let _ = sink.send_chunk(b"ab".to_vec()).await;
            let _ = sink.send_chunk(b"cdef".to_vec()).await;
            let _ = sink.complete(Vec::new()).await;
        });

        let resp = output_to_response(stream).await.expect("build response");

        assert_eq!(resp.status(), 200);
        let read = JsFuture::from(resp.array_buffer().expect("array_buffer")).await;
        assert!(
            read.is_err(),
            "a body longer than it declared must not read back as complete"
        );
    }

    /// The same path without a failure still delivers every byte — the abort
    /// above is the error case, not a stream that drops its tail.
    #[wasm_bindgen_test]
    async fn a_streamed_body_delivers_every_chunk() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            let _ = sink
                .send_meta(meta(META_RESP_STREAM, STREAM_MARKER_VALUE))
                .await;
            let _ = sink
                .send_meta(meta(META_RESP_CONTENT_TYPE, "application/pdf"))
                .await;
            let _ = sink.send_chunk(b"one ".to_vec()).await;
            let _ = sink.send_chunk(b"two ".to_vec()).await;
            let _ = sink.send_chunk(b"three".to_vec()).await;
            let _ = sink.complete(Vec::new()).await;
        });

        let resp = output_to_response(stream).await.expect("build response");
        let text = JsFuture::from(resp.text().expect("text"))
            .await
            .expect("read");
        assert_eq!(text.as_string().as_deref(), Some("one two three"));
    }

    /// An error answers with the codec's error body, detail code included,
    /// and keeps the error meta's headers: both cookies (appended, not the
    /// last one set) and one `Content-Type`, the JSON the body is, even when
    /// the error meta names another. The body is compared byte for byte with
    /// `http_codec::error_to_http_response`, the renderer native and
    /// Cloudflare go through.
    ///
    /// The error is built here, so this is the contract for a NATIVE block's
    /// error, which may set cookies (a 401 that clears a session). A WASM
    /// guest's error never arrives with a cookie or another sensitive header
    /// it did not declare: `WasmiBlock` strips them from every guest egress,
    /// and the sandbox refuses a guest that declares any
    /// (`impresspress-core`'s `wafer_guest_golden` pins that end to end).
    #[wasm_bindgen_test]
    async fn an_error_answers_with_the_codecs_error_body_and_headers() {
        let mut err = WaferError::new(ErrorCode::Unauthenticated, "Not authenticated")
            .with_detail_code("not_authenticated");
        err.meta.push(meta("resp.set_cookie.a", "a=1; Path=/"));
        err.meta.push(meta("resp.set_cookie.b", "b=2; Path=/"));
        err.meta.push(meta("resp.header.Retry-After", "30"));
        err.meta.push(meta(META_RESP_CONTENT_TYPE, "text/html"));
        let expected = wafer_block::http_codec::error_to_http_response(&err);

        let resp = output_to_response(OutputStream::error(err))
            .await
            .expect("build response");

        assert_eq!(resp.status(), 401);
        let headers = resp.headers();
        assert_eq!(
            headers.get("content-type").unwrap().as_deref(),
            Some("application/json")
        );
        assert_eq!(headers.get("retry-after").unwrap().as_deref(), Some("30"));
        let cookies = headers.get("set-cookie").unwrap().unwrap_or_default();
        assert!(
            cookies.contains("a=1") && cookies.contains("b=2"),
            "both cookies must reach the client: {cookies}"
        );
        let text = JsFuture::from(resp.text().unwrap()).await.unwrap();
        let body = text.as_string().expect("a text body");
        assert_eq!(body.as_bytes(), expected.body.as_slice());
        let json: serde_json::Value = serde_json::from_str(&body).expect("a JSON body");
        assert_eq!(json["code"], "not_authenticated");
    }

    /// A drop answers 204 with the headers the flow's drop carries (its CORS
    /// headers, a cookie a middleware set) and no `Content-Type`, as the
    /// codec renders it for native and Cloudflare.
    #[wasm_bindgen_test]
    async fn a_drop_keeps_its_headers_and_has_no_content_type() {
        let drop = OutputStream::drop_request_with_meta(vec![
            meta(
                "resp.header.Access-Control-Allow-Origin",
                "https://a.example",
            ),
            meta("resp.set_cookie.0", "s=1; Path=/"),
            meta(META_RESP_CONTENT_TYPE, "text/html"),
        ]);

        let resp = output_to_response(drop).await.expect("build response");

        assert_eq!(resp.status(), 204);
        let headers = resp.headers();
        assert_eq!(
            headers
                .get("access-control-allow-origin")
                .unwrap()
                .as_deref(),
            Some("https://a.example")
        );
        assert_eq!(
            headers.get("set-cookie").unwrap().as_deref(),
            Some("s=1; Path=/")
        );
        assert_eq!(headers.get("content-type").unwrap(), None);
    }

    /// And a body that fits is unaffected — the cap must not change the
    /// ordinary buffered response at all.
    #[wasm_bindgen_test]
    async fn a_buffered_response_within_the_cap_is_unchanged() {
        let stream = OutputStream::from_producer(|sink, _cancel| async move {
            let _ = sink.send_chunk(b"hello ".to_vec()).await;
            let _ = sink.send_chunk(b"world".to_vec()).await;
            let _ = sink
                .complete(vec![meta(META_RESP_CONTENT_TYPE, "text/plain")])
                .await;
        });

        let resp = output_to_response(stream).await.expect("build response");

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers().get("content-type").unwrap().as_deref(),
            Some("text/plain")
        );
        let text = JsFuture::from(resp.text().unwrap()).await.unwrap();
        assert_eq!(text.as_string().as_deref(), Some("hello world"));
    }
}

/// An event stream (`impresspress_core::sse`, what every SSE route produces)
/// through the real `output_to_response`, read back the way the page reads
/// the Service Worker's `Response`.
///
/// The producer waits on an upstream that never answers — a model thinking,
/// a model load before its first progress — so every assertion holds while
/// the producer cannot have finished. Each wait is bounded by a JS timer,
/// which only turns a hang into a failure.
#[cfg(all(test, target_arch = "wasm32"))]
mod event_stream_tests {
    use std::future::Future;

    use futures::{channel::oneshot, StreamExt};
    use impresspress_core::sse::{event_stream, Pulled, OPEN_FRAME};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::output_to_response;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = setTimeout)]
        fn set_timeout(callback: &js_sys::Function, delay_ms: i32) -> JsValue;
    }

    /// `fut`'s output, or a failure naming `what` after five seconds.
    async fn bounded<F: Future + Unpin>(fut: F, what: &str) -> F::Output {
        let timer =
            wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |resolve, _reject| {
                set_timeout(&resolve, 5_000);
            }));
        match futures::future::select(fut, timer).await {
            futures::future::Either::Left((output, _)) => output,
            futures::future::Either::Right(_) => panic!("{what}: nothing within 5 s"),
        }
    }

    /// The `Response` carries the SSE head and the open frame while the
    /// producer still waits on its upstream, and a page that cancels the body
    /// ends the producer's read of that upstream: the body's `ReadableStream`
    /// drops the Rust stream it pulls, which cancels the producer.
    ///
    /// Fails without the head's cache and proxy headers, without the open
    /// frame (the response never resolves: the head waits for a first chunk
    /// that only the upstream could send), and with a read of the upstream
    /// that does not race the consumer (the producer never learns the page
    /// left).
    #[wasm_bindgen_test]
    async fn an_event_stream_opens_at_once_and_its_producer_stops_when_the_body_is_cancelled() {
        let (pulled_tx, pulled_rx) = oneshot::channel();
        let stream = event_stream(move |events| async move {
            let mut upstream = futures::stream::pending::<()>();
            let _ = pulled_tx.send(events.pull(&mut upstream).await);
        });

        let resp = bounded(Box::pin(output_to_response(stream)), "the response head")
            .await
            .expect("build response");
        assert_eq!(resp.status(), 200);
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

        let mut body = wasm_streams::ReadableStream::from_raw(
            resp.body().expect("a streamed body").unchecked_into(),
        )
        .into_stream();
        let first = bounded(body.next(), "the open frame")
            .await
            .expect("a first chunk")
            .expect("a readable chunk");
        assert_eq!(js_sys::Uint8Array::new(&first).to_vec(), OPEN_FRAME);

        drop(body);
        assert_eq!(
            bounded(pulled_rx, "the producer's read of its upstream")
                .await
                .expect("the producer reports its read"),
            Pulled::ConsumerGone
        );
    }
}
