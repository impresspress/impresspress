//! HTTP ↔ Message conversion for the browser Service Worker adapter.
//!
//! Thin platform glue: the protocol mapping (method→action table, request
//! meta layout, response-meta classification, `ErrorCode`→status table) lives
//! in `wafer_block::http_codec` — the same implementation the native axum
//! listener and the Cloudflare adapter use. Only `web_sys` I/O lives here:
//! reading the request body/headers, the Service-Worker cookie re-injection,
//! and building `web_sys::Response` (buffered or `ReadableStream`-backed).

use futures::StreamExt;
use impresspress_core::streaming::{self, CappedCollect};
use js_sys::{ArrayBuffer, Uint8Array};
use wafer_block::{
    http_codec::{self, ResponseMetaPart},
    stream::StreamEvent,
    streams::{input::InputStream, output::OutputStream},
    Message, MetaEntry,
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

/// Apply classified response-meta parts to a `web_sys::Headers`. Status parts
/// are resolved separately (see `http_codec::resolve_status`) and skipped here.
/// Only the canonical `resp.*` meta keys are honored — legacy aliases
/// (`http.status`, `http.resp.header.*`, `http.resp.set-cookie.*`, a literal
/// `Content-Type` meta key) are ignored by `http_codec`.
fn apply_response_parts(headers: &Headers, parts: &[ResponseMetaPart<'_>]) -> Result<(), JsValue> {
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

/// Apply transport-neutral [`http_codec::HttpResponseParts`] to a
/// `web_sys::Response`.
///
/// `Set-Cookie` is appended, since the parts may carry several; every other
/// header is set, so a name the parts repeat in a different case (a
/// `resp.header.content-type` beside the codec's own `Content-Type`) ends as
/// one header, the later value winning — what [`apply_response_parts`] does
/// for a streamed response.
fn parts_to_response(parts: http_codec::HttpResponseParts) -> Result<web_sys::Response, JsValue> {
    let headers = Headers::new()?;
    for (name, value) in &parts.headers {
        if name.eq_ignore_ascii_case("set-cookie") {
            headers.append(name, value)?;
        } else {
            headers.set(name, value)?;
        }
    }
    make_response(parts.body, parts.status, headers)
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

/// Build a JS `ReadableStream` that yields `first_chunk` and then every
/// subsequent `Chunk` event from `remaining`.
///
/// The framing is [`streaming::download_body_stream`] — the same function the
/// Cloudflare adapter pipes into its Worker `ReadableStream`, so the two agree
/// on what a mid-body failure means: an `Error` terminal (which is also how
/// wafer-run ends a stream whose producer stopped without a terminal), a
/// stream with no terminal at all, or a `Halt` after the body started becomes
/// an `Err` item, which errors the JS stream and aborts the response body.
/// Mid-body `Meta` is dropped, and a `Complete`, `Drop` or `Continue` terminal
/// ends the body cleanly.
///
/// The status is already committed by the time a body read fails, so an error
/// cannot be downgraded to a 413/500 — but a reader that gets an abort knows
/// the bytes are incomplete, which a clean end-of-stream does not tell it. No
/// file download sets `Content-Length`, so this is the client's only signal.
fn make_streaming_body(
    first_chunk: Vec<u8>,
    remaining: OutputStream,
) -> wasm_streams::ReadableStream {
    let body = streaming::download_body_stream(first_chunk, remaining).map(|chunk| {
        chunk
            .map(|bytes| JsValue::from(Uint8Array::from(bytes.as_slice())))
            .map_err(|err| {
                JsValue::from_str(&format!(
                    "impresspress-browser: streaming response aborted: {}",
                    err.message
                ))
            })
    });

    wasm_streams::ReadableStream::from_stream(body)
}

/// Convert a WAFER `OutputStream` into a browser `web_sys::Response`.
///
/// Two paths, and the choice between them is [`streaming::wants_streaming`] —
/// the single decision the request pipeline and every other adapter consult,
/// so the browser cannot disagree with Cloudflare about whether a given
/// response streams. The body framing of the streaming path is shared with it
/// too ([`streaming::download_body_stream`], via [`make_streaming_body`]), so
/// neither can they disagree about what a mid-body failure does to the body:
/// 1. **Streaming** — for blocks that declare streaming intent in leading
///    `Meta` events BEFORE the first `Chunk`, either with a streaming
///    `resp.content_type` (SSE, `application/octet-stream`) or with the
///    explicit [`streaming::META_RESP_STREAM`] marker that large binary
///    download handlers set on a real content type (`application/pdf`,
///    `image/*`, …). Status + headers are applied to a `Response` backed by a
///    `ReadableStream` and subsequent chunks are piped straight to the browser
///    — so a multi-minute SSE response isn't held back behind a buffer that
///    flushes at the very end (which Chrome's idle keep-alive treats as a hung
///    fetch and drops with `net::ERR_FAILED`), and a large download never sits
///    in the Service Worker's heap whole. This path must NOT route through
///    `collect_http_response` (which buffers).
/// 2. **Buffered** (default) — for blocks that emit `Chunk(bytes),
///    Complete{meta}` via `respond_with_meta`. Status, headers, and body all
///    live in the terminal, so we read the whole stream before building the
///    `Response` — under [`streaming::MAX_BUFFERED_RESPONSE_BYTES`], so an
///    over-large body becomes a clean **413** instead of exhausting the one
///    linear memory the whole page's runtime shares. The terminal is rendered
///    by `http_codec::collect_http_response` itself, as on Cloudflare, so the
///    status, headers, drift decisions and the `500` for meta no transport
///    can send are the codec's.
pub async fn output_to_response(mut output: OutputStream) -> Result<web_sys::Response, JsValue> {
    // Peek leading Meta events without consuming Chunks. Buffered blocks send
    // no Meta before their first Chunk, so this returns an empty vec for them
    // and the buffered branch below handles the terminal.
    let (leading_meta, next_event) = streaming::drain_leading_meta(&mut output).await;

    if streaming::wants_streaming(&leading_meta) {
        return match next_event {
            // Declared streaming AND a body chunk to forward — stream it.
            Some(StreamEvent::Chunk(first)) => {
                build_streaming_response(leading_meta, first, output)
            }
            // Declared streaming but the terminal arrived before any body
            // (empty SSE / empty download) — render the (short) buffered form.
            other => finalise_capped(collect_capped(output, leading_meta, other).await).await,
        };
    }

    finalise_capped(collect_capped(output, leading_meta, next_event).await).await
}

/// Drain the remainder of a buffered response under the shared byte cap.
async fn collect_capped(
    rest: OutputStream,
    leading_meta: Vec<MetaEntry>,
    next_event: Option<StreamEvent>,
) -> CappedCollect {
    streaming::collect_capped_with_prelude(
        rest,
        leading_meta,
        next_event,
        streaming::MAX_BUFFERED_RESPONSE_BYTES,
    )
    .await
}

/// Render a capped buffered collection to a `web_sys::Response`.
async fn finalise_capped(collected: CappedCollect) -> Result<web_sys::Response, JsValue> {
    match collected {
        CappedCollect::Terminal(result) => parts_to_response(
            http_codec::collect_http_response(streaming::terminal_to_stream(result)).await,
        ),
        CappedCollect::OverLimit => {
            let headers = Headers::new()?;
            headers.set("Content-Type", "text/plain; charset=utf-8")?;
            make_response(b"payload too large".to_vec(), 413, headers)
        }
    }
}

/// Build a streaming `web_sys::Response` from the leading meta (carrying
/// status + headers) and an `OutputStream` whose remaining events are piped
/// into the body. Meta is classified and applied *before* the body finishes —
/// the whole point of the streaming path.
///
/// Leading meta no transport can send is answered with the codec's
/// `unsendable_response` here, before a status or a byte is committed: once
/// the body streams, a failure can only abort it. Handed the whole leading
/// meta, the codec's 500 keeps the terminal's sendable security and CORS
/// headers (`Content-Security-Policy`, `X-Frame-Options`, …) and drops the
/// ones describing the body it replaces (`Content-Disposition`,
/// `Content-Encoding`, cache headers), adding `Cache-Control: no-store`.
fn build_streaming_response(
    leading_meta: Vec<MetaEntry>,
    first_chunk: Vec<u8>,
    remaining: OutputStream,
) -> Result<web_sys::Response, JsValue> {
    let parts = match http_codec::response_meta_parts(&leading_meta) {
        Ok(parts) => parts,
        Err(invalid) => {
            return parts_to_response(http_codec::unsendable_response(&leading_meta, &invalid))
        }
    };
    let status = http_codec::resolve_status(&leading_meta, 200);
    let headers = Headers::new()?;
    apply_response_parts(&headers, &parts)?;

    if !parts
        .iter()
        .any(|part| matches!(part, ResponseMetaPart::ContentType(_)))
    {
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
    use impresspress_core::streaming::{
        MAX_BUFFERED_RESPONSE_BYTES, META_RESP_STREAM, STREAM_MARKER_VALUE,
    };
    use wafer_block::{
        meta::META_RESP_CONTENT_TYPE, streams::output::OutputStream, ErrorCode, MetaEntry,
        WaferError,
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
    /// Cloudflare, reading the same marker through `streaming::wants_streaming`,
    /// streamed it.
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
    /// download path sets `Content-Length`, so the reader has no other way to
    /// tell. Routed through `streaming::download_body_stream` — the framing
    /// Cloudflare already used — the `Error` terminal errors the stream, and
    /// reading the body rejects.
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
