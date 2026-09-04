//! Shared request pipeline — the core impresspress request handling logic.
//!
//! Both Cloudflare and native adapters call `handle_request()` after
//! converting their platform-specific HTTP types into a WAFER Message.

use std::cell::Cell;

use wafer_block::http_codec;
use wafer_core::clients::{config as config_client, database as db};
use wafer_run::{
    context::Context, streams::output::TerminalNotResponse, AuthLevel, BlockInfo, ErrorCode,
    InputStream, Message, MetaEntry, OutputStream, WaferError, META_REQ_RESOURCE,
};

use crate::{
    endpoint_match,
    features::FeatureConfig,
    http::ResponseBuilder,
    routing::{self, ExtraRoute},
    ui,
};

/// How the pipeline persists the per-request audit row.
///
/// `Inline` (default; native): `db::create` awaited on the response path —
/// today's behavior. `Queued` (Cloudflare): the completed row is pushed to a
/// thread-local queue; the platform entry drains it after dispatch and
/// attaches the write to `ctx.wait_until`, so responses stop paying one D1
/// write of latency. Rows are plain data, so it does not matter which
/// interleaved request's drain flushes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestLogMode {
    Inline,
    Queued,
}

/// One queued audit row (table + column map), ready for `DatabaseService::create`.
pub struct QueuedRequestLog {
    pub table: &'static str,
    pub data: std::collections::HashMap<String, serde_json::Value>,
}

thread_local! {
    static REQUEST_LOG_MODE: Cell<RequestLogMode> = const { Cell::new(RequestLogMode::Inline) };
    /// Queued audit rows for this isolate.
    ///
    /// [`IsolateCell`](crate::IsolateCell) rather than `RefCell`: this is
    /// isolate-lifetime state on the request path, and the push below can
    /// reallocate the `Vec` — a wide enough window for a Cloudflare hard stop
    /// to land in. A borrow flag stranded that way stays set for the life of
    /// the isolate and traps every later request that logs. See
    /// `crate::isolate_cell`.
    static REQUEST_LOG_QUEUE: crate::IsolateCell<Vec<QueuedRequestLog>> =
        const { crate::IsolateCell::new() };
}

/// Select the request-log persistence mode for this thread (isolate).
/// The Cloudflare target sets it (idempotently) at the top of every request;
/// native never calls it.
pub fn set_request_log_mode(mode: RequestLogMode) {
    REQUEST_LOG_MODE.with(|m| m.set(mode));
}

fn request_log_mode() -> RequestLogMode {
    REQUEST_LOG_MODE.with(Cell::get)
}

fn enqueue_request_log(
    table: &'static str,
    data: std::collections::HashMap<String, serde_json::Value>,
) {
    REQUEST_LOG_QUEUE.with(|queue| {
        let mut rows = queue.take().unwrap_or_default();
        rows.push(QueuedRequestLog { table, data });
        queue.set(rows);
    });
}

/// Take every queued row, clearing the queue. The platform entry calls this
/// after each dispatch and persists the rows off the response path.
pub fn drain_queued_request_logs() -> Vec<QueuedRequestLog> {
    REQUEST_LOG_QUEUE.with(|queue| queue.take().unwrap_or_default())
}

/// The `AuthLevel` ceiling for this request, used to filter the WebMCP tool
/// manifest.
///
/// Reads the SAME source the router's admin gate enforces with:
/// `crate::util::is_admin` (`util.rs:173`) inspects the `auth.user_roles`
/// meta set from the verified JWT by `extract_auth_meta`, and
/// `routing.rs:375` admits `RouteAccess::Admin` on exactly that basis.
///
/// It is tempting to query roles from the database instead. Do not — the
/// manifest would then answer a different question than the gate. A user
/// granted admin in the roles table after their token was minted would be
/// advertised admin tools that the router then 403s (publishing tool names
/// to someone who cannot invoke them, the precise SEC-073 problem this
/// filtering exists to prevent), and a revoked admin whose JWT is still live
/// would be under-reported while the router still admits their calls. Same
/// source, no drift — and no DB round trip per page view.
///
/// Synchronous and infallible by construction: there is nothing to fail.
fn caller_auth_level(msg: &Message) -> AuthLevel {
    if msg.user_id().is_empty() {
        return AuthLevel::Public;
    }
    if crate::util::is_admin(msg) {
        return AuthLevel::Admin;
    }
    AuthLevel::Authenticated
}

/// Every registered block with its endpoint list narrowed to what `caller`
/// may invoke, resolved with `routing::effective_access` — the filter the
/// WebMCP manifest applies, reused so `/openapi.json`, the agent card and the
/// manifest cannot disagree about who is told an endpoint exists. Blocks are
/// kept even when nothing in them is visible, so block-level metadata the
/// documents carry stays stable across tiers.
fn visible_to_caller(
    block_infos: &[BlockInfo],
    caller: AuthLevel,
    extra_routes: &[ExtraRoute],
) -> Vec<BlockInfo> {
    let ceiling = endpoint_match::auth_rank(caller);
    block_infos
        .iter()
        .map(|block| {
            let mut visible = block.clone();
            visible.endpoints.retain(|ep| {
                endpoint_match::auth_rank(routing::effective_access(block, ep, extra_routes))
                    <= ceiling
            });
            visible
        })
        .collect()
}

/// Handle a impresspress request.
///
/// This is the shared entry point that both CF and native adapters call
/// after building a Message from the incoming HTTP request.
///
/// Steps:
/// 1. Strip `/api` prefix (CF convention — native doesn't use it)
/// 2. Validate JWT and set auth meta
/// 3. CSRF: enforce the Fetch-Metadata/Origin policy for cookie-authenticated
///    unsafe-method requests (see `crate::csrf`)
/// 4. Route to the appropriate impresspress block
/// 5. Log the request to `request_logs` (async, best-effort) — a streamed
///    download with a definite status is audited too; only open-ended streams
///    (SSE) skip the row
///
/// # Errors
///
/// Never returns an error directly — errors are encoded inside the
/// returned `OutputStream` as `StreamEvent::Error`. Request-log
/// persistence failures are intentionally swallowed (best-effort) so a
/// failing audit-log table never breaks the response.
// This is the single request-pipeline entry point; each argument is a distinct
// piece of request/runtime context and a param-struct refactor is out of scope
// for a lint sweep (behavior-preserving cleanup only).
#[allow(clippy::too_many_arguments)]
pub async fn handle_request(
    ctx: &dyn Context,
    mut msg: Message,
    input: InputStream,
    auth_header: Option<&str>,
    jwt_secret: &str,
    cookie_authenticated: bool,
    features: &dyn FeatureConfig,
    block_infos: &[BlockInfo],
    extra_routes: &[ExtraRoute],
) -> OutputStream {
    // 0. (Discovery documents moved below step 2 — they are filtered by the
    //    caller's tier, which is not known here.)

    // 1. Strip /api prefix from resource path
    let resource = msg.path().to_string();
    if let Some(stripped) = resource.strip_prefix("/api") {
        msg.set_meta(META_REQ_RESOURCE, stripped);
    }

    // 2. Validate JWT or API key and set auth meta
    if let Some(header) = auth_header {
        if header.starts_with("Bearer ") {
            // [SEC-038] Read the deployment's expected issuer once per request
            // so JWTs minted under a different deployment's FRONTEND_URL get
            // rejected even if their HMAC secret matches. [SEC-042] also
            // consults the JWT blocklist via the ctx-aware extractor.
            let expected_iss = crate::blocks::auth::helpers::expected_issuer(ctx).await;
            crate::crypto::extract_auth_meta(ctx, header, jwt_secret, &expected_iss, &mut msg)
                .await;
        } else if let Some(api_key) = header.strip_prefix("ApiKey ") {
            crate::blocks::auth::authenticate_api_key(ctx, api_key, &mut msg).await;
        }
    }

    // Capture request info before routing (for logging)
    let method = msg.action().to_string();
    let path = msg.path().to_string();
    let client_ip = msg.remote_addr().to_string();
    let user_id = msg.user_id().to_string();
    let start_ms = crate::util::now_millis();

    // Discovery documents: `/openapi.json` and the agent card. Placed after
    // step 2, with the manifest, because they are filtered by the caller's
    // tier: an endpoint the caller could not invoke is not described to
    // them. At step 0 `msg.user_id()` is always empty, so a filter there
    // would hand every caller the anonymous document — silently, since that
    // is a valid document (`openapi_describes_admin_endpoints_to_an_admin`
    // pins the placement).
    //
    // The route itself stays reachable without credentials: an anonymous
    // caller gets the Public subset, which is exactly what they can use.
    if path == "/openapi.json" || path == "/.well-known/agent.json" {
        let is_openapi = path == "/openapi.json";
        let host = msg.header("host").to_string();
        let server_url = format!("https://{host}");
        // The project/display name for the discovery documents (OpenAPI
        // `info.title` and the agent-card `name`). Previously this was
        // derived from the `Host` header (`host.split('.').next()`), which
        // produced garbage for IP-addressed hosts — e.g. `127.0.0.1:8093`
        // yielded the literal title `"127"`. `WAFER_RUN_SHARED__APP_NAME` is
        // the existing single-sourced display-name config var (already used
        // for emails, the login page, and the browser `<title>` — see
        // `blocks/email.rs`, `ui/mod.rs`), so discovery documents reuse it
        // instead of inventing a second name knob; it falls back to the
        // constant `"Impresspress"`, never to the host.
        let project_name =
            config_client::get_default(ctx, "WAFER_RUN_SHARED__APP_NAME", "Impresspress").await;

        // Same ceiling and the same resolver as the manifest below, so the
        // three projections of one declaration agree on who is told about
        // it. Two projections with different disclosure rules is the pattern
        // that produced the `dedupe_hash` leak.
        let caller = caller_auth_level(&msg);
        let visible_infos = visible_to_caller(block_infos, caller, extra_routes);

        let body = if is_openapi {
            wafer_core::discovery::generate_openapi(&visible_infos, &project_name, "", &server_url)
        } else {
            wafer_core::discovery::generate_agent_card(
                &visible_infos,
                &project_name,
                "",
                &server_url,
            )
        };

        // [SEC-073] Only emit `Access-Control-Allow-Origin: *` in dev.
        // Advertising `*` to every cross-origin caller in production lets
        // unauthenticated browser code at any site map the API surface —
        // now the Public subset, but still reconnaissance. In prod we just
        // omit the header; non-browser clients (curl, the agent runtime,
        // server-side fetchers) don't care about CORS so they still see the
        // body.
        let environment =
            config_client::get_default(ctx, "WAFER_RUN_SHARED__ENVIRONMENT", "development").await;
        let is_dev = environment.eq_ignore_ascii_case("development");

        // Per-caller by construction, like the manifest: a shared cache
        // serving one visitor's document to another would leak the
        // privileged surface. Was `public, max-age=3600` while the document
        // was the same for everyone.
        let mut resp = ResponseBuilder::new().set_header("Cache-Control", "no-store");
        if is_dev {
            resp = resp.set_header("Access-Control-Allow-Origin", "*");
        }
        return resp.json(&body);
    }

    // WebMCP tool manifest. Placed after step 2 because it needs the resolved
    // identity, like the discovery documents above.
    if path == "/b/webmcp/manifest.json" {
        let caller = caller_auth_level(&msg);

        // `block_infos` is every REGISTERED block, but `route_to_block`
        // 404s any block the admin feature toggle has turned off
        // (routing.rs's feature gate, backed by the live `block_settings`
        // row). Advertising a disabled block's tools would hand the agent
        // names that 404 on every call, so the manifest is generated from
        // the enabled subset only — gated under the same name the router
        // gates with (`feature_gate_name`; the inspector's `BlockInfo` name
        // and its route's `block` name differ).
        let enabled_infos: Vec<BlockInfo> = block_infos
            .iter()
            .filter(|b| features.is_block_enabled(routing::feature_gate_name(&b.name)))
            .cloned()
            .collect();

        // MUST resolve the auth ceiling with `routing::effective_access`, not
        // the plain `ep.auth`. This router admits on `max(prefix_tier,
        // ep.auth)` (routing.rs:440), so a `generate_webmcp_declared_auth`-
        // style filter on `ep.auth` alone would advertise a Public-declared
        // endpoint mounted under an Admin prefix to anonymous callers — the
        // router still 403s, so it is not a data leak, but it publishes a
        // tool name the caller cannot use (the recon surface this filtering
        // exists to prevent) and hands the agent a tool that always fails.
        //
        // `extra_routes` is threaded in so a downstream `add_route` — which
        // `route_to_block` enforces just like a built-in — is resolved too.
        //
        // MUST be `generate_webmcp_report`, not `generate_webmcp`. This
        // route is unauthenticated and served with `Cache-Control: no-store`,
        // so every anonymous GET re-runs generation; `generate_webmcp`'s
        // wrapper logs one `tracing::warn!` per refused endpoint on every
        // call, which turns an unauthenticated endpoint into unbounded
        // warn-level log volume for a caller in a loop. Refusals are mostly
        // static — a defect in a block's own declarations, identical for
        // every call and every caller — with one exception:
        // `DuplicateToolName` is counted per-manifest against the
        // auth-filtered set this same `caller`/effective-auth pair would
        // produce (see `generate_webmcp_report`'s doc comment, "Refusals are
        // the same for every caller — with one exception"), so it is not
        // static across callers, only across repeated calls by the same
        // caller. Either way they are computed and logged exactly once, at
        // runtime construction, in `builder::registration::build()` (using
        // an `AuthLevel::Admin` ceiling, which — because the auth filter is
        // monotone — still sees every collision that exists anywhere,
        // including ones invisible at this route's actual `caller`). The
        // manifest content emitted here is unaffected either way: `_report`
        // runs the identical generation and only changes where the refusal
        // list goes. Refusals discarded on purpose — see the comment above.
        let (body, _refused) =
            wafer_core::discovery::generate_webmcp_report(&enabled_infos, caller, |block, ep| {
                routing::effective_access(block, ep, extra_routes)
            });

        // Per-session by construction: a shared cache serving one visitor's
        // manifest to another would leak the privileged tool surface.
        return ResponseBuilder::new()
            .set_header("Cache-Control", "no-store")
            .json(&body);
    }

    // WebMCP registration script at a stable path — beside the manifest
    // above for the same reason the discovery documents sit here: it needs
    // no routing through `SystemBlock`/`route_to_block` at all. SSR pages get
    // `webmcp.js` injected by `ui::layout` at the content-hashed URL
    // `ui::assets::webmcp_js_url()` embeds (`/b/static/webmcp-{hash}.js`,
    // served by `SystemBlock`'s `CORE_TABLE`), which changes every deploy. A
    // page written under `site/` — served by `wafer-run/web`, not this
    // pipeline — never gets that injection and has no way to discover the
    // current hash, so it needs one path that never moves. Public like the
    // manifest: the script bytes don't vary by caller, only the manifest it
    // fetches does, so there's no identity to resolve here.
    if path == ui::assets::WEBMCP_JS_STABLE_PATH {
        // Served from embedded bytes in every build. `ui::assets::webmcp_js`
        // is deliberately ungated for this reason — see its doc comment.
        //
        // RFC 9110 §8.8.3: an entity-tag is an opaque *quoted-string*, so the
        // quotes are part of the value, not formatting. A bare hash is not a
        // well-formed `ETag`, and a client that echoes it back verbatim in
        // `If-None-Match` — which is the whole point of sending one — offers
        // something the comparison rules cannot match, so no `304` would ever
        // fire even with a comparison in place. `webmcp_js_hash()` itself
        // stays bare: it is the hash, and `webmcp_js_url()` embeds it in a
        // filename where quotes would be nonsense.
        let etag = format!("\"{}\"", ui::assets::webmcp_js_hash());
        // The comparison `http::conditional::not_modified` runs is what makes
        // the `no-cache` revalidation below actually cheap: a repeat visitor's
        // `If-None-Match` matching this `ETag` gets a bodyless `304` instead
        // of the whole script re-downloaded on every navigation.
        if let Some(not_modified) = crate::http::conditional::not_modified(&msg, &etag, "no-cache")
        {
            return not_modified;
        }
        return ResponseBuilder::new()
            .set_header("Cache-Control", "no-cache")
            .set_header("ETag", &etag)
            .set_header("X-Content-Type-Options", "nosniff")
            .body(
                ui::assets::webmcp_js().as_bytes().to_vec(),
                "application/javascript; charset=utf-8",
            );
    }

    // 2a. CSRF: cookie-authenticated unsafe-method requests must pass the
    // Fetch-Metadata/Origin/Referer policy before any block sees them. Bearer
    // -authenticated callers (`cookie_authenticated == false`) are exempt — see
    // `crate::csrf` module docs. One central check for every mutation. Routed
    // through the same `stream` variable (rather than an early `return`) so a
    // rejection flows through the normal status-resolution + audit-log tail
    // below exactly like a dispatched response would.
    //
    // 3. Route to block.
    let mut stream = match crate::csrf::enforce_origin_policy(&msg, cookie_authenticated) {
        Some(denied) => denied,
        None => routing::route_to_block(ctx, msg, input, features, block_infos, extra_routes).await,
    };

    // 3a. A response that declares streaming intent up front (its headers in a
    //     leading-meta frame) is forwarded without draining the body. Two
    //     sub-cases, split by whether it carries the `resp.stream` marker:
    //
    //     - A DEFINITE streamed response — a file download / share access —
    //       carries the marker and a known status in that header frame, so it
    //       STILL gets its `request_logs` row (status resolved from the leading
    //       meta, duration = time-to-headers) before the body streams. These
    //       are short, audit-worthy request/responses; the audit row must not
    //       be lost just because the body streams — and on platforms whose
    //       adapter buffers the body anyway (the native axum listener), losing
    //       it would be pure regression with no offsetting streaming benefit.
    //
    //     - A genuinely OPEN-ENDED stream — SSE / chat: a streaming
    //       content-type with no marker, no definite status or completion —
    //       skips the row. Buffering it just to grab a status would defeat
    //       streaming, and these long-lived progress feeds aren't the short
    //       request/responses request_logs is built for.
    let (leading_meta, next_event) = crate::streaming::drain_leading_meta(&mut stream).await;
    if crate::streaming::wants_streaming(&leading_meta) {
        if crate::streaming::has_stream_marker(&leading_meta) {
            let status_code = i64::from(http_codec::resolve_status(&leading_meta, 200));
            let status_label = if status_code >= 400 { "ERROR" } else { "OK" };
            let duration_ms = i64::try_from(crate::util::now_millis().saturating_sub(start_ms))
                .unwrap_or(i64::MAX);
            write_request_log(
                ctx,
                RequestLogRow {
                    method: &method,
                    path: &path,
                    status_label,
                    status_code,
                    error_message: "",
                    duration_ms,
                    client_ip: &client_ip,
                    user_id: &user_id,
                },
            )
            .await;
        }
        return crate::streaming::rebuild_streaming(leading_meta, next_event, stream);
    }

    let (status_label, status_code, error_message, reply): (
        &'static str,
        i64,
        String,
        OutputStream,
    ) = match crate::streaming::collect_buffered_with_prelude(stream, leading_meta, next_event)
        .await
    {
        Ok(buf) => {
            let code = i64::from(http_codec::resolve_status(&buf.meta, 200));
            (
                "OK",
                code,
                String::new(),
                replay_buffered(buf.body, buf.meta),
            )
        }
        Err(TerminalNotResponse::Error(err)) => {
            // The error's OWN code decides the status. This was hardcoded 500,
            // so a `NotFound` — which `error_code_to_http_status` maps to 404,
            // and `PermissionDenied` to 403 — was served and logged as a server
            // error. `resolve_error_status` is the mapping `wafer-block`
            // already provides for exactly this; the pipeline simply never
            // called it. A crawler reads 500 as "retry later" and 404 as "drop
            // this URL", so the old behaviour also invited retries of URLs that
            // will never exist. See `an_unmatched_endpoint_is_404_not_500`.
            let message = err.message.clone();
            let code = i64::from(http_codec::resolve_error_status(&err));
            ("ERROR", code, message, OutputStream::error(err))
        }
        Err(TerminalNotResponse::Drop) => ("OK", 204, String::new(), OutputStream::drop_request()),
        Err(TerminalNotResponse::Continue(m)) => {
            ("OK", 200, String::new(), OutputStream::continue_with(m))
        }
        Err(TerminalNotResponse::Malformed) => (
            "ERROR",
            500,
            "stream ended without terminal event".to_string(),
            OutputStream::error(WaferError {
                code: ErrorCode::Internal,
                message: "stream ended without terminal event".to_string(),
                meta: vec![],
            }),
        ),
        Err(TerminalNotResponse::Halt(buf)) => {
            let code = i64::from(http_codec::resolve_status(&buf.meta, 200));
            (
                "OK",
                code,
                String::new(),
                OutputStream::from_buffered_response(buf),
            )
        }
    };

    // 4. Log the request (best-effort, don't block the response).
    // `now_millis()` reads wall clock — saturating_sub guards against clock
    // skew on suspend/resume from regressing the subtraction, and try_into
    // clamps the unlikely case of an absurdly large delta to `i64::MAX`.
    let duration_ms =
        i64::try_from(crate::util::now_millis().saturating_sub(start_ms)).unwrap_or(i64::MAX);
    write_request_log(
        ctx,
        RequestLogRow {
            method: &method,
            path: &path,
            status_label,
            status_code,
            error_message: &error_message,
            duration_ms,
            client_ip: &client_ip,
            user_id: &user_id,
        },
    )
    .await;

    reply
}

/// Fields of one `request_logs` audit row. Bundled into a struct so
/// [`write_request_log`] stays a two-argument call (the row shape is shared by
/// the buffered response tail and the streamed-download branch).
struct RequestLogRow<'a> {
    method: &'a str,
    path: &'a str,
    status_label: &'a str,
    status_code: i64,
    error_message: &'a str,
    duration_ms: i64,
    client_ip: &'a str,
    user_id: &'a str,
}

/// Write one `request_logs` audit row (best-effort; never fails the request).
/// Static-asset and health-check paths are skipped to keep the table
/// signal-heavy — the prefix is the shared `routing::STATIC_PREFIX` const so it
/// can't drift from the routing table and the `ui::assets` URL builders.
///
/// Shared by the buffered response tail and the streamed-download branch so a
/// download produces the same row on every platform, whether the adapter
/// streams or buffers its body.
/// The stored `path` for a request that matched no route.
///
/// The path is attacker-supplied. Storing it verbatim lets anyone mint
/// unbounded DISTINCT rows by walking `/aaa1`, `/aaa2`, … and puts their text
/// into the admin UI. A 404 carries no routing information worth keeping, so
/// every one collapses to this single label.
pub const UNMATCHED_PATH_LABEL: &str = "<unmatched>";

/// The most `request_logs` rows one isolate will write in one window.
///
/// The backstop. `RequestLogPolicy` reasons about WHICH requests deserve a
/// row; this bounds HOW MANY regardless of that reasoning being right. Real
/// 5xx traffic never approaches it; a flood hits it immediately.
///
/// Note the honest limit: this bounds an isolate, not the account. Cloudflare
/// may run many isolates, so a determined flood still multiplies by that
/// count — which is why it is the third layer and not the only one.
pub const REQUEST_LOG_CEILING_PER_WINDOW: usize = 200;

/// The ceiling's window. Long enough that a flood cannot simply wait it out at
/// a useful rate, short enough that a genuine incident is not silenced all day.
const REQUEST_LOG_WINDOW_MS: u64 = 3_600_000;

/// What `request_logs` keeps. See [`crate::config_vars::REQUEST_LOG_CONFIG_KEY`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestLogPolicy {
    /// Every request (minus static assets and `/health`). The default.
    All,
    /// Server errors only — 5xx.
    ///
    /// 4xx is deliberately excluded even though it looks diagnostic: it is
    /// entirely attacker-mintable (any GET to a non-route is a 404) and
    /// Cloudflare's edge analytics already counts it. 5xx is the only class
    /// carrying an `error_message` no edge log can reconstruct.
    Errors,
    /// Nothing.
    Off,
}

impl RequestLogPolicy {
    /// Parse the config value. Anything unrecognised — including an empty
    /// string — is [`RequestLogPolicy::All`], so a typo degrades to today's
    /// behaviour rather than silently disabling the audit trail.
    fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).unwrap_or_default() {
            "errors" => Self::Errors,
            "off" => Self::Off,
            _ => Self::All,
        }
    }

    fn keeps(self, status_code: i64) -> bool {
        match self {
            Self::All => true,
            Self::Errors => status_code >= 500,
            Self::Off => false,
        }
    }
}

thread_local! {
    /// `(window start ms, rows written in it)` for this isolate.
    static REQUEST_LOG_BUDGET: Cell<(u64, usize)> = const { Cell::new((0, 0)) };
}

/// Claim one row against this isolate's ceiling; `false` means refuse to write.
fn claim_request_log_budget(now_ms: u64) -> bool {
    REQUEST_LOG_BUDGET.with(|budget| {
        let (window_start, used) = budget.get();
        let (window_start, used) = if now_ms.saturating_sub(window_start) >= REQUEST_LOG_WINDOW_MS {
            (now_ms, 0)
        } else {
            (window_start, used)
        };
        if used >= REQUEST_LOG_CEILING_PER_WINDOW {
            budget.set((window_start, used));
            return false;
        }
        budget.set((window_start, used + 1));
        true
    })
}

/// Reset the isolate's ceiling. Tests only — a shared thread outlives a
/// fixture, so without this a count would depend on test ordering.
#[cfg(test)]
pub(crate) fn reset_request_log_budget_for_test() {
    REQUEST_LOG_BUDGET.with(|budget| budget.set((0, 0)));
}

async fn write_request_log(ctx: &dyn Context, row: RequestLogRow<'_>) {
    if row.path.starts_with(routing::STATIC_PREFIX) || row.path == "/health" {
        return;
    }
    let policy =
        RequestLogPolicy::parse(ctx.config_get(crate::config_vars::REQUEST_LOG_CONFIG_KEY));
    if !policy.keeps(row.status_code) {
        return;
    }
    if !claim_request_log_budget(crate::util::now_millis()) {
        return;
    }
    // Collapse the attacker-controlled half of the key space before it is
    // stored. See `UNMATCHED_PATH_LABEL`.
    let path = if row.status_code == 404 {
        UNMATCHED_PATH_LABEL
    } else {
        row.path
    };
    let mut data = std::collections::HashMap::new();
    data.insert("method".to_string(), serde_json::json!(row.method));
    data.insert("path".to_string(), serde_json::json!(path));
    data.insert("status".to_string(), serde_json::json!(row.status_label));
    data.insert(
        "status_code".to_string(),
        serde_json::json!(row.status_code),
    );
    data.insert(
        "duration_ms".to_string(),
        serde_json::json!(row.duration_ms),
    );
    data.insert(
        "error_message".to_string(),
        serde_json::json!(row.error_message),
    );
    data.insert("client_ip".to_string(), serde_json::json!(row.client_ip));
    data.insert("user_id".to_string(), serde_json::json!(row.user_id));
    crate::util::stamp_created(&mut data);

    match request_log_mode() {
        RequestLogMode::Inline => {
            // Best-effort: don't fail the request if logging fails.
            let _ = db::create(ctx, crate::blocks::admin::REQUEST_LOGS_TABLE, data).await;
        }
        RequestLogMode::Queued => {
            enqueue_request_log(crate::blocks::admin::REQUEST_LOGS_TABLE, data);
        }
    }
}

/// Rebuild an `OutputStream` from an already-collected buffered response.
/// Used by the pipeline after intercepting the stream for logging.
fn replay_buffered(body: Vec<u8>, meta: Vec<MetaEntry>) -> OutputStream {
    OutputStream::respond_with_meta(body, meta)
}

#[cfg(test)]
mod discovery_tests {
    //! Covers the two OpenAPI/agent-card fixes:
    //!  1. `info.title` (and the agent-card `name`) comes from
    //!     `WAFER_RUN_SHARED__APP_NAME` (fallback `"Impresspress"`), never from
    //!     the `Host` header — an IP-addressed host used to yield the
    //!     literal title `"127"`.
    //!  2. The core developer-facing auth/storage/products endpoints now
    //!     declare schemas, so `wafer_core::discovery::generate_openapi`
    //!     (which skips any endpoint failing `has_schema()`) includes them.
    //!
    //! `real_block_infos()` and `discovery_json()` live in
    //! `test_support.rs` now — shared with the per-block openapi snapshot
    //! gate (`tests/openapi_snapshot.rs`) so there is one implementation
    //! rather than two.

    use wafer_run::{AuthLevel, BlockEndpoint, BlockInfo, InputStream};

    use super::handle_request;
    use crate::{
        features::{AllEnabled, FeatureConfig},
        routing,
        test_support::{
            anon_msg, bearer_for_roles, collect_or_panic, discovery_json, discovery_json_as,
            real_block_infos, TestContext, TEST_JWT_SECRET,
        },
        ui,
    };

    #[tokio::test]
    async fn openapi_title_falls_back_to_impresspress_not_host_derived_127() {
        let ctx = TestContext::new().await;
        // The exact host shape that produced the bug: an IP:port `Host`
        // header. `host.split('.').next()` on `"127.0.0.1:8093"` yields
        // the literal string `"127"`.
        let body = discovery_json(&ctx, "/openapi.json", "127.0.0.1:8093").await;

        assert_eq!(
            body["info"]["title"], "Impresspress",
            "no WAFER_RUN_SHARED__APP_NAME configured — title must fall back to the constant, not derive from the Host header: {body}"
        );
        assert_ne!(body["info"]["title"], "127");
    }

    #[tokio::test]
    async fn openapi_title_honors_configured_app_name() {
        let mut ctx = TestContext::new().await;
        ctx.set_config("WAFER_RUN_SHARED__APP_NAME", "Acme Corp");
        let body = discovery_json(&ctx, "/openapi.json", "127.0.0.1:8093").await;

        assert_eq!(body["info"]["title"], "Acme Corp");
    }

    #[tokio::test]
    async fn agent_card_name_uses_the_same_configured_project_name() {
        let mut ctx = TestContext::new().await;
        ctx.set_config("WAFER_RUN_SHARED__APP_NAME", "Acme Corp");
        let body = discovery_json(&ctx, "/.well-known/agent.json", "127.0.0.1:8093").await;

        assert_eq!(
            body["name"], "Acme Corp",
            "agent-card generation must use the same corrected project_name as openapi: {body}"
        );
    }

    #[tokio::test]
    async fn openapi_documents_core_auth_endpoints_with_schemas() {
        let ctx = TestContext::new().await;
        let body = discovery_json(&ctx, "/openapi.json", "impresspress.example.com").await;
        let paths = &body["paths"];

        let login = &paths["/b/auth/api/login"]["post"];
        assert!(
            !login.is_null(),
            "login must appear in /openapi.json: {body}"
        );
        assert_eq!(
            login["requestBody"]["content"]["application/json"]["schema"]["required"],
            serde_json::json!(["email", "password"]),
            "login request schema must match the real handler body: {login}"
        );
        assert!(
            !login["responses"]["200"]["content"]["application/json"]["schema"].is_null(),
            "login response schema missing: {login}"
        );
        assert!(
            login.get("security").is_none(),
            "login is AuthLevel::Public — must not carry a security requirement: {login}"
        );

        let me = &paths["/b/auth/api/me"]["get"];
        assert!(!me.is_null(), "me must appear in /openapi.json: {body}");
        assert_eq!(
            me["responses"]["200"]["content"]["application/json"]["schema"]["properties"]["user"]
                ["properties"]["roles"]["type"],
            "array",
            "me response schema must match api/me.rs's {{user: {{..., roles: [...]}}}} shape: {me}"
        );
        assert_eq!(
            me["security"][0]["bearerAuth"],
            serde_json::json!([]),
            "me is AuthLevel::Authenticated — must carry bearerAuth security: {me}"
        );

        // PATCH /b/auth/api/me was dispatched in handle() but undeclared, so
        // it was absent here and from the access-tier table. It now shares
        // GET's response type, so the two cannot drift.
        let me_patch = &paths["/b/auth/api/me"]["patch"];
        assert!(
            !me_patch.is_null(),
            "PATCH me must appear in /openapi.json now that it's declared: {body}"
        );
        let mut patch_fields: Vec<String> = me_patch["requestBody"]["content"]["application/json"]
            ["schema"]["properties"]
            .as_object()
            .expect("PATCH me request schema has properties")
            .keys()
            .cloned()
            .collect();
        patch_fields.sort();
        assert_eq!(
            patch_fields,
            vec!["avatar_url".to_string(), "name".to_string()],
            "PATCH me request schema must expose exactly the two user-editable fields: {me_patch}"
        );
        assert_eq!(
            me_patch["responses"]["200"]["content"]["application/json"]["schema"],
            me["responses"]["200"]["content"]["application/json"]["schema"],
            "PATCH me must publish the same response schema as GET me: {me_patch}"
        );
        assert_eq!(
            me_patch["security"][0]["bearerAuth"],
            serde_json::json!([]),
            "PATCH me is AuthLevel::Authenticated — must carry bearerAuth security: {me_patch}"
        );

        // /b/auth/api/refresh was previously entirely undeclared (dispatched
        // in handle() but absent from .endpoints) — now documented.
        let refresh = &paths["/b/auth/api/refresh"]["post"];
        assert!(
            !refresh.is_null(),
            "refresh must appear in /openapi.json now that it's declared: {body}"
        );
        assert_eq!(
            refresh["requestBody"]["content"]["application/json"]["schema"]["required"],
            serde_json::json!(["refresh_token"]),
        );
        assert!(
            refresh.get("security").is_none(),
            "refresh is AuthLevel::Public — must not carry a security requirement: {refresh}"
        );
    }

    #[tokio::test]
    async fn openapi_documents_core_storage_endpoints_with_schemas() {
        let ctx = TestContext::new().await;
        let body = discovery_json(&ctx, "/openapi.json", "impresspress.example.com").await;
        let paths = &body["paths"];

        let list = &paths["/b/storage/api/buckets/{name}/objects"]["get"];
        assert!(
            !list.is_null(),
            "list-objects must appear in /openapi.json: {body}"
        );
        assert_eq!(
            list["parameters"]
                .as_array()
                .expect("list-objects has path+query parameters")
                .iter()
                .filter(|p| p["in"] == "path" && p["name"] == "name")
                .count(),
            1,
            "list-objects must declare the {{name}} bucket path param: {list}"
        );
        assert!(
            !list["responses"]["200"]["content"]["application/json"]["schema"]["properties"]
                ["objects"]
                .is_null(),
            "list-objects response schema must match ObjectList {{objects, total_count}}: {list}"
        );

        let get_obj = &paths["/b/storage/api/buckets/{name}/objects/{key}"]["get"];
        assert!(
            !get_obj.is_null(),
            "get-object must appear in /openapi.json: {body}"
        );
        assert!(
            get_obj["responses"]["200"].get("content").is_none(),
            "get-object returns raw bytes, not JSON — must not claim an application/json response: {get_obj}"
        );
    }

    #[tokio::test]
    async fn openapi_documents_core_products_endpoints_with_schemas() {
        let ctx = TestContext::new().await;
        let body = discovery_json(&ctx, "/openapi.json", "impresspress.example.com").await;
        let paths = &body["paths"];

        let catalog = &paths["/b/products/catalog"]["get"];
        assert!(
            !catalog.is_null(),
            "catalog list must appear in /openapi.json: {body}"
        );
        let catalog_props = &catalog["responses"]["200"]["content"]["application/json"]["schema"]
            ["properties"]["records"]["items"]["properties"];
        assert_eq!(
            catalog_props["stock"]["type"], "integer",
            "catalog rows are flat `CatalogProductView`s: {catalog}"
        );
        // The admin row is every column of the products table; the public
        // catalog row is the same table minus the ownership, moderation and
        // provider columns, which `CatalogProductView` withholds by not
        // naming them.
        let product_props = &paths["/b/products/api/admin/products"]["get"]["responses"]["200"]
            ["content"]["application/json"]["schema"]["properties"]["records"]["items"]
            ["properties"];
        for field in [
            "group_template_id",
            "product_template_id",
            "requires",
            "created_by",
            "owner_kind",
            "owner_id",
            "seller_account_id",
            "approval_status",
            "fulfillment_kind",
            "stripe_product_id",
            "current_version",
            "submitted_at",
            "published_at",
            "deleted_at",
        ] {
            assert!(
                !product_props[field].is_null(),
                "product schema is missing real column `{field}`: {product_props}"
            );
        }
        for field in [
            "created_by",
            "owner_kind",
            "owner_id",
            "seller_account_id",
            "approval_status",
            "stripe_product_id",
            "current_version",
            "submitted_at",
            "deleted_at",
        ] {
            assert!(
                catalog_props[field].is_null(),
                "the public catalog must not publish `{field}`: {catalog_props}"
            );
        }

        let detail = &paths["/b/products/catalog/{id}"]["get"];
        assert!(
            !detail.is_null(),
            "product detail must appear in /openapi.json: {body}"
        );
        assert_eq!(
            detail["parameters"][0]["name"], "id",
            "product detail must declare the {{id}} path param: {detail}"
        );

        let groups = &paths["/b/products/groups"];
        assert!(
            !groups["get"].is_null() && !groups["post"].is_null(),
            "owned group list/create must appear in /openapi.json: {groups}"
        );
        assert_eq!(
            groups["post"]["requestBody"]["content"]["application/json"]["schema"]["required"],
            serde_json::json!(["name"]),
            "group creation must document its required name: {groups}"
        );
        let group_products = &paths["/b/products/groups/{id}/products"]["get"];
        assert_eq!(
            group_products["parameters"][0]["name"], "id",
            "owned group product listing must document its group id: {group_products}"
        );

        let preview = &paths["/b/products/pricing/preview"]["post"];
        assert_eq!(
            preview["requestBody"]["content"]["application/json"]["schema"]["required"],
            serde_json::json!(["offer_id"]),
            "offer pricing preview must document its request body: {preview}"
        );
        assert!(
            !preview["responses"]["200"]["content"]["application/json"]["schema"]["properties"]
                ["amounts"]
                .is_null(),
            "offer pricing preview must document its integer-minor-unit amounts: {preview}"
        );

        let webhook = &paths["/b/products/webhooks"]["post"];
        assert_eq!(
            webhook["requestBody"]["content"]["application/json"]["schema"]["required"],
            serde_json::json!(["type", "data"]),
            "the signed Stripe webhook payload must appear in discovery: {webhook}"
        );
        assert_eq!(
            webhook["responses"]["200"]["content"]["application/json"]["schema"]["properties"]
                ["received"]["type"],
            "boolean",
            "the Stripe webhook acknowledgement must be documented: {webhook}"
        );

        let checkout = &paths["/b/products/checkout"]["post"];
        assert_eq!(
            checkout["requestBody"]["content"]["application/json"]["schema"]["required"],
            serde_json::json!(["offer_id"]),
            "checkout must document its typed-offer body: {checkout}"
        );
        let checkout_response =
            &checkout["responses"]["200"]["content"]["application/json"]["schema"];
        assert!(
            !checkout_response["properties"]["receipt_token"].is_null()
                && !checkout_response["properties"]["amounts"].is_null(),
            "checkout must document the receipt token and minor-unit amounts: {checkout}"
        );
        assert!(
            checkout_response["required"]
                .as_array()
                .is_some_and(|r| r.contains(&serde_json::json!("receipt_token"))),
            "the receipt token is what checkout returns, so it is always present: {checkout}"
        );
        // `writeOnly` asserts a field is never present in a response. Both of
        // these are only ever present in a response — the receipt token is
        // the product of checkout, the client secret is how an embedded
        // checkout is opened — so the flag was a false statement that a
        // strict OpenAPI 3.1 client would act on. Sensitivity is stated in
        // the description instead.
        for field in ["receipt_token", "client_secret"] {
            assert!(
                checkout_response["properties"][field]["writeOnly"].is_null(),
                "{field} is returned in the response and must not be marked writeOnly: {checkout}"
            );
        }

        let guest_status = &paths["/b/products/orders/{id}/status"]["get"];
        let guest_props = &guest_status["responses"]["200"]["content"]["application/json"]
            ["schema"]["properties"];
        assert_eq!(
            guest_props["amounts"]["properties"]["total_minor"]["type"],
            "integer"
        );
        assert!(
            guest_props["buyer_email"].is_null()
                && guest_props["stripe_payment_intent_id"].is_null(),
            "guest order discovery must not expose buyer/provider identifiers: {guest_props}"
        );

        let subscription = &paths["/b/products/subscription"]["get"];
        let subscription_props = &subscription["responses"]["200"]["content"]["application/json"]
            ["schema"]["properties"]["subscription"]["properties"];
        assert!(
            subscription_props["stripe_customer_id"].is_null()
                && subscription_props["user_id"].is_null(),
            "subscription discovery must mirror the curated secret-free projection: {subscription_props}"
        );

        let portal = &paths["/b/products/billing-portal"]["post"];
        assert_eq!(
            portal["requestBody"]["content"]["application/json"]["schema"]["required"],
            serde_json::json!(["return_url"])
        );
        assert_eq!(
            portal["responses"]["200"]["content"]["application/json"]["schema"]["properties"]
                ["url"]["format"],
            "uri"
        );

        for (path, method) in [
            ("/b/products/api/seller/account", "get"),
            ("/b/products/api/seller/stats", "get"),
            ("/b/products/api/seller/orders", "get"),
            ("/b/products/api/seller/orders/{id}", "get"),
            ("/b/products/api/seller/orders/{id}/refund", "post"),
            ("/b/products/api/seller/onboarding", "post"),
            ("/b/products/api/seller/dashboard", "post"),
        ] {
            assert!(
                !paths[path][method].is_null(),
                "seller endpoint {method} {path} must appear in discovery"
            );
        }
        let seller_stats = &paths["/b/products/api/seller/stats"]["get"]["responses"]["200"]
            ["content"]["application/json"]["schema"]["properties"];
        let analytics_props = &seller_stats["currency_analytics"]["items"]["properties"];
        assert_eq!(analytics_props["open_dispute_count"]["type"], "integer");
        assert_eq!(
            analytics_props["open_disputed_volume_minor"]["type"],
            "integer"
        );
        assert_eq!(analytics_props["lost_dispute_count"]["type"], "integer");
        assert_eq!(
            analytics_props["lost_disputed_volume_minor"]["type"],
            "integer"
        );
        let failure_props = &seller_stats["recent_failures"]["items"]["properties"];
        assert_eq!(failure_props["total_minor"]["type"], "integer");
        assert!(
            failure_props["buyer_email"].is_null(),
            "seller failure summaries must remain ownership-safe: {failure_props}"
        );
        let onboarding = &paths["/b/products/api/seller/onboarding"]["post"];
        assert_eq!(
            onboarding["requestBody"]["content"]["application/json"]["schema"]["required"],
            serde_json::json!(["return_url", "refresh_url"])
        );
        let seller_refund = &paths["/b/products/api/seller/orders/{id}/refund"]["post"];
        assert_eq!(
            seller_refund["requestBody"]["content"]["application/json"]["schema"]["properties"]
                ["amount_minor"]["minimum"],
            1
        );

        for (path, method) in [
            ("/b/products/api/admin/purchases", "get"),
            ("/b/products/api/admin/purchases/{id}", "get"),
            ("/b/products/api/admin/purchases/{id}/refund", "post"),
            ("/b/products/api/admin/stats", "get"),
            ("/b/products/api/admin/stripe/status", "get"),
            ("/b/products/api/admin/webhook-events", "get"),
            ("/b/products/api/admin/webhook-events/{id}/replay", "post"),
            ("/b/products/api/admin/sellers", "get"),
            ("/b/products/api/admin/sellers/{id}", "get"),
            ("/b/products/api/admin/sellers/{id}/suspend", "post"),
            ("/b/products/api/admin/sellers/{id}/reactivate", "post"),
            ("/b/products/api/admin/products/{id}/approve", "post"),
            ("/b/products/api/admin/products/{id}/reject", "post"),
        ] {
            assert!(
                !paths[path][method].is_null(),
                "administrator endpoint {method} {path} must appear in discovery"
            );
        }
        let stripe_status = &paths["/b/products/api/admin/stripe/status"]["get"]["responses"]
            ["200"]["content"]["application/json"]["schema"]["properties"];
        assert!(
            stripe_status["secret_key"].is_null()
                && stripe_status["webhook_secret"].is_null()
                && stripe_status["publishable_key"].is_null(),
            "Stripe health discovery must never expose credential values: {stripe_status}"
        );
        // Order rows are flat `contracts::*View`s; the `{id, data}` record
        // envelope is gone from the detail's `purchase` and `disputes`.
        let order_dispute = &paths["/b/products/api/admin/purchases/{id}"]["get"]["responses"]
            ["200"]["content"]["application/json"]["schema"]["properties"]["disputes"]["items"]
            ["properties"];
        assert_eq!(order_dispute["amount_minor"]["type"], "integer");
        assert!(
            order_dispute["status"]["enum"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == "needs_response")),
            "order detail must document the durable dispute projection: {order_dispute}"
        );
        let order_payment = &paths["/b/products/api/admin/purchases/{id}"]["get"]["responses"]
            ["200"]["content"]["application/json"]["schema"]["properties"]["purchase"]
            ["properties"];
        assert!(
            order_payment["receipt_token_hash"].is_null()
                && order_payment["receipt_token_expires_at"].is_null(),
            "the guest receipt digest is never published on an order row: {order_payment}"
        );
        assert_eq!(
            order_payment["payment_intent_event_created"]["type"],
            "integer"
        );
        assert!(
            order_payment["provider_payment_status"]["enum"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == "payment_failed")),
            "order detail must document the PaymentIntent operational projection: {order_payment}"
        );
        let webhook_events = &paths["/b/products/api/admin/webhook-events"]["get"]["responses"]
            ["200"]["content"]["application/json"]["schema"]["properties"]["records"]["items"]
            ["properties"];
        assert!(
            webhook_events["payload"].is_null() && webhook_events["processing_owner"].is_null(),
            "webhook recovery discovery must remain payload/token safe: {webhook_events}"
        );
        assert!(
            !paths["/b/products/api/admin/purchases/{id}/refund"]["post"].is_null()
                && paths["/b/products/api/admin/purchases/{id}/refund"]["patch"].is_null(),
            "admin refunds are POST-only; the legacy PATCH alias was removed"
        );

        for prefix in ["/b/products/api/admin/products", "/b/products/api/products"] {
            let collection = &paths[prefix];
            assert_eq!(
                collection["post"]["requestBody"]["content"]["application/json"]["schema"]
                    ["required"],
                serde_json::json!(["name"]),
                "product creation must document its required name: {collection}"
            );
            // The row is `contracts::ProductView`, flat: the `{id, data}`
            // record envelope the untyped handlers echoed is gone.
            let row = &collection["get"]["responses"]["200"]["content"]["application/json"]
                ["schema"]["properties"]["records"]["items"];
            assert_eq!(
                row["properties"]["approval_status"]["type"], "string",
                "builder product lists must use the commerce-v2 row contract: {collection}"
            );
            assert!(
                row["properties"]["data"].is_null(),
                "builder product rows are flat views, not {{id, data}} records: {collection}"
            );

            let duplicate = &paths[&format!("{prefix}/{{id}}/duplicate")]["post"];
            assert_eq!(
                duplicate["responses"]["200"]["content"]["application/json"]["schema"]
                    ["required"],
                serde_json::json!(["product", "offers"]),
                "whole-product duplication returns both the new product and cloned offers: {duplicate}"
            );

            let offers = &paths[&format!("{prefix}/{{product_id}}/offers")];
            assert_eq!(
                offers["get"]["responses"]["200"]["content"]["application/json"]["schema"]
                    ["required"],
                serde_json::json!(["offers"]),
                "offer lists use their real envelope: {offers}"
            );
            assert!(
                offers["post"]["requestBody"]["content"]["application/json"]["schema"]["required"]
                    .as_array()
                    .is_some_and(|required| required.iter().any(|field| field == "components")),
                "offer creation must document the component definition: {offers}"
            );

            let presets = &paths[&format!("{prefix}/{{product_id}}/offers/{{offer_id}}/presets")];
            assert_eq!(
                presets["get"]["responses"]["200"]["content"]["application/json"]["schema"]
                    ["required"],
                serde_json::json!(["presets"]),
                "preset lists use their real envelope: {presets}"
            );
            let links =
                &paths[&format!("{prefix}/{{product_id}}/offers/{{offer_id}}/payment-links")];
            assert_eq!(
                links["get"]["responses"]["200"]["content"]["application/json"]["schema"]
                    ["required"],
                serde_json::json!(["payment_links"]),
                "Payment Link lists use their real envelope: {links}"
            );
        }

        // The envelope is `{records, total_count, page, page_size}` and the
        // handler always emits all four; the hand-written schema understated
        // `required`.
        for path in [
            "/b/products/api/admin/groups",
            "/b/products/api/admin/types",
        ] {
            assert_eq!(
                paths[path]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
                    ["required"],
                serde_json::json!(["records", "total_count", "page", "page_size"]),
                "admin builder list must document its envelope: {}",
                paths[path]["get"]
            );
        }
        // The legacy pricing-template/formula-variable builders were removed
        // with the typed-offer redesign and must stay out of discovery.
        for path in [
            "/b/products/api/admin/pricing",
            "/b/products/api/admin/variables",
        ] {
            assert!(
                paths[path].is_null(),
                "removed legacy builder path must not be documented: {path}"
            );
        }
    }

    // -------------------------------------------------------------------
    // -------------------------------------------------------------------
    // WebMCP manifest — the third discovery document, but auth-filtered and
    // per-session (unlike `/openapi.json` and the agent card, which are
    // anonymous by design).
    // -------------------------------------------------------------------

    /// Like `discovery_json`, but returns the response headers instead of
    /// parsing the body — used to assert on `Cache-Control`.
    async fn discovery_headers(
        ctx: &TestContext,
        path: &str,
        host: &str,
    ) -> std::collections::HashMap<String, String> {
        let mut msg = anon_msg("retrieve", path);
        msg.set_meta("http.header.host", host);
        let out = handle_request(
            ctx,
            msg,
            InputStream::from_bytes(Vec::new()),
            None,
            "test-jwt-secret",
            false,
            &AllEnabled,
            &real_block_infos(),
            &[],
        )
        .await;
        let buf = collect_or_panic(out).await;
        buf.meta
            .into_iter()
            .filter_map(|entry| {
                entry
                    .key
                    .strip_prefix("resp.header.")
                    .map(|name| (name.to_string(), entry.value))
            })
            .collect()
    }

    /// Drive `GET /b/webmcp/manifest.json` through the whole pipeline and
    /// return the parsed manifest.
    ///
    /// `roles: None` sends no `Authorization` header at all (an anonymous
    /// visitor). `Some(roles)` mints a real, signed access token carrying
    /// that `roles` claim — `Some(&[])` is a logged-in non-admin,
    /// `Some(&["admin"])` an admin.
    ///
    /// Deliberately NOT `test_support::auth_msg` / `admin_msg`, which stamp
    /// `auth.user_id` / `auth.user_roles` directly onto the `Message`
    /// before `handle_request` ever runs. That would leave the identity
    /// populated even if the WebMCP manifest branch were wrongly placed at
    /// step 0 — before `extract_auth_meta` populates auth meta from the
    /// header — which would defeat the tests that exist to catch that
    /// placement bug. Only a Bearer header that step 2 must independently
    /// verify exercises the ordering.
    async fn webmcp_manifest(
        ctx: &TestContext,
        roles: Option<&[&str]>,
        infos: &[BlockInfo],
        features: &dyn FeatureConfig,
    ) -> serde_json::Value {
        let secret = TEST_JWT_SECRET;
        let auth_header = roles.map(bearer_for_roles);

        let mut msg = anon_msg("retrieve", "/b/webmcp/manifest.json");
        msg.set_meta("http.header.host", "impresspress.example.com");
        let out = handle_request(
            ctx,
            msg,
            InputStream::from_bytes(Vec::new()),
            auth_header.as_deref(),
            secret,
            false,
            features,
            infos,
            &[],
        )
        .await;
        let buf = collect_or_panic(out).await;
        serde_json::from_slice(&buf.body).expect("manifest response is valid JSON")
    }

    /// `/openapi.json` and the agent card were generated at step 0, before
    /// auth, so every caller received the complete document — Admin paths
    /// included. They now mirror the manifest: an endpoint the caller could
    /// not invoke is not described to them. Two projections of one
    /// declaration with different disclosure rules was the pattern behind
    /// the `dedupe_hash` leak; this closes the discovery-side half.
    #[tokio::test]
    async fn openapi_omits_endpoints_above_the_callers_tier() {
        let ctx = TestContext::new().await;
        let host = "impresspress.example.com";

        let anon = discovery_json_as(&ctx, "/openapi.json", host, None).await;
        assert!(
            !anon["paths"]["/b/products/storefront/config"].is_null(),
            "Public endpoints are described to everyone: {}",
            anon["paths"]
        );
        assert!(
            anon["paths"]["/b/auth/api/me"].is_null(),
            "an Authenticated endpoint must not be described to an anonymous caller: {}",
            anon["paths"]
        );
        assert!(
            anon["paths"]["/b/admin/api/users"].is_null(),
            "an Admin endpoint must not be described to an anonymous caller: {}",
            anon["paths"]
        );

        let user = discovery_json_as(&ctx, "/openapi.json", host, Some(&["user"])).await;
        assert!(
            !user["paths"]["/b/auth/api/me"].is_null(),
            "an authenticated caller sees Authenticated endpoints: {}",
            user["paths"]
        );
        assert!(
            user["paths"]["/b/admin/api/users"].is_null(),
            "an authenticated non-admin must not see Admin endpoints: {}",
            user["paths"]
        );
    }

    /// Placement regression. At step 0 `msg.user_id()` is empty, so a filter
    /// there hands every caller the anonymous document — a valid document,
    /// invisible to a smoke test. The same trap the manifest route has a
    /// test for.
    #[tokio::test]
    async fn openapi_describes_admin_endpoints_to_an_admin() {
        // A real bearer, resolved by step 2 — not pre-set meta, which a
        // step-0 filter would also see. Needs the auth tables.
        let ctx = TestContext::with_auth().await;
        let bearer = bearer_for_roles(&["admin"]);
        let mut msg = anon_msg("retrieve", "/openapi.json");
        msg.set_meta("http.header.host", "impresspress.example.com");
        let out = handle_request(
            &ctx,
            msg,
            InputStream::from_bytes(Vec::new()),
            Some(&bearer),
            TEST_JWT_SECRET,
            false,
            &AllEnabled,
            &real_block_infos(),
            &[],
        )
        .await;
        let admin: serde_json::Value = serde_json::from_slice(&collect_or_panic(out).await.body)
            .expect("openapi response is valid JSON");
        assert!(
            !admin["paths"]["/b/admin/api/users"].is_null(),
            "an admin must receive the Admin endpoints: {}",
            admin["paths"]
        );
    }

    #[tokio::test]
    async fn agent_card_omits_skills_above_the_callers_tier() {
        let ctx = TestContext::new().await;
        let host = "impresspress.example.com";
        fn skill_ids(card: &serde_json::Value) -> Vec<String> {
            card["skills"]
                .as_array()
                .expect("agent card skills array")
                .iter()
                .map(|s| s["id"].as_str().expect("skill id").to_string())
                .collect()
        }

        let anon = skill_ids(&discovery_json_as(&ctx, "/.well-known/agent.json", host, None).await);
        assert!(
            anon.iter().any(|id| id.contains("storefront")),
            "Public skills are listed for everyone: {anon:?}"
        );
        assert!(
            !anon.iter().any(|id| id.starts_with("impresspress/admin/")),
            "an anonymous card must not list an Admin block's skills: {anon:?}"
        );

        let admin = skill_ids(
            &discovery_json_as(&ctx, "/.well-known/agent.json", host, Some(&["admin"])).await,
        );
        assert!(
            admin.iter().any(|id| id.starts_with("impresspress/admin/")),
            "an admin's card lists the Admin block's skills: {admin:?}"
        );
    }

    /// Per-caller by construction now, so a shared cache serving one
    /// visitor's document to another would leak the privileged surface —
    /// the same reasoning as the manifest's `no-store`.
    #[tokio::test]
    async fn discovery_documents_are_not_cacheable() {
        let ctx = TestContext::new().await;
        for path in ["/openapi.json", "/.well-known/agent.json"] {
            let headers = discovery_headers(&ctx, path, "impresspress.example.com").await;
            let cache_control = headers
                .get("Cache-Control")
                .map(String::as_str)
                .unwrap_or_default();
            assert!(
                cache_control.contains("no-store"),
                "{path} is per-caller and must not be cached, got: {cache_control:?}"
            );
        }
    }

    /// Every tool name in a manifest.
    fn tool_names(manifest: &serde_json::Value) -> Vec<&str> {
        manifest["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().expect("tool name"))
            .collect()
    }

    /// A block declaring one Admin-tier agent tool.
    ///
    /// Nothing shipped declares one — every real tool today is Public or
    /// Authenticated — so without this fixture no assertion could tell
    /// `caller_auth_level`'s Admin branch apart from its Authenticated
    /// branch: both would produce the same tool set, and an Admin test
    /// would pass whether or not the branch worked. Mounted under
    /// `/b/admin/`, so `routing::effective_access` resolves it to Admin the
    /// same way the router would.
    fn admin_tool_block() -> BlockInfo {
        BlockInfo::new(
            "impresspress/admin",
            "0.0.1",
            "http-handler@v1",
            "admin tool probe",
        )
        .endpoints(vec![BlockEndpoint::get("/b/admin/api/tool-probe")
            .summary("Admin-only probe")
            .auth(AuthLevel::Admin)
            .agent_tool("admin_only_probe", "An admin-only probe tool.")])
    }

    #[tokio::test]
    async fn webmcp_manifest_is_served_and_versioned() {
        let ctx = TestContext::new().await;
        let body = webmcp_manifest(&ctx, None, &real_block_infos(), &AllEnabled).await;

        assert_eq!(body["schema_version"], serde_json::json!(1));
        assert!(
            body["tools"].is_array(),
            "manifest must carry a tools array: {body}"
        );
    }

    #[tokio::test]
    async fn webmcp_manifest_for_anonymous_caller_contains_no_privileged_tools() {
        let ctx = TestContext::new().await;

        // An unauthenticated request must see Public tools only. Anything
        // requiring a session is recon surface if its name is published
        // here. `list_my_purchases` is the shipped Authenticated tool;
        // `admin_only_probe` (fixture) is kept alongside the shipped admin
        // tools so this still covers the Admin tier if admin's own surface
        // ever goes away.
        let mut infos = real_block_infos();
        infos.push(admin_tool_block());
        let body = webmcp_manifest(&ctx, None, &infos, &AllEnabled).await;
        let names = tool_names(&body);

        for forbidden in [
            "list_my_purchases",
            "admin_only_probe",
            "list_users",
            "list_roles",
            "get_site_settings",
            "list_audit_log",
        ] {
            assert!(
                !names.contains(&forbidden),
                "anonymous manifest must not name the privileged tool {forbidden}: {names:?}"
            );
        }
    }

    #[tokio::test]
    async fn anonymous_manifest_exposes_the_storefront_purchase_path() {
        let ctx = TestContext::new().await;
        let body = webmcp_manifest(&ctx, None, &real_block_infos(), &AllEnabled).await;
        let names = tool_names(&body);

        for expected in [
            "get_storefront_config",
            "list_products",
            "get_product",
            "preview_price",
            "start_checkout",
            "get_order_status",
        ] {
            assert!(
                names.contains(&expected),
                "anonymous visitors must get the public purchase path; missing {expected}: {names:?}"
            );
        }
    }

    /// Pins the producer-to-consumer contract for `invocation` — the object
    /// `ui/assets/webmcp.js` reads to build every request it makes.
    ///
    /// `webmcp.js` has no test infrastructure of its own, and the wafer-run
    /// rev this consumes is pinned to a branch that is still moving, so a
    /// producer-side rename (`path_params` to `pathParams`, a dropped
    /// `method`, a changed placeholder syntax) would otherwise land silently
    /// green here and break every tool at runtime. Asserting the WHOLE
    /// object — not a key at a time — is what makes a rename fail.
    ///
    /// `get_order_status` is the tool that exercises both a path param and
    /// a query param, so it pins the most contract surface of the six.
    #[tokio::test]
    async fn webmcp_manifest_pins_the_producer_invocation_contract() {
        let ctx = TestContext::new().await;
        let body = webmcp_manifest(&ctx, None, &real_block_infos(), &AllEnabled).await;

        let tool = body["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .find(|t| t["name"] == "get_order_status")
            .unwrap_or_else(|| panic!("get_order_status must be published: {body}"));

        assert_eq!(
            tool["invocation"],
            serde_json::json!({
                "method": "get",
                "path": "/b/products/orders/{id}/status",
                "path_params": ["id"],
                "query_params": ["receipt_token"],
                "body_params": [],
            }),
            "invocation shape drifted from what ui/assets/webmcp.js reads: {tool}"
        );
    }

    /// Pins the producer-to-consumer contract for `outputSchema` — the field
    /// `ui/assets/webmcp.js` reads to decide whether to pass a schema to
    /// `registerTool` and to populate `structuredContent` from the parsed
    /// response body.
    ///
    /// `get_order_status`'s schema is derived from `contracts::GuestOrderStatus`
    /// (`.output::<T>()`), so the literal is not repeated here — the per-block
    /// snapshot gate (`tests/openapi_snapshot.rs`) already pins every byte of
    /// it. What this test pins is what that gate cannot:
    ///
    /// 1. The manifest carries the *same* projection of that declaration as
    ///    `/openapi.json` does, minus the root `title` the producer strips
    ///    (`wafer_core::discovery::agent_output_schema` inlines refs and drops
    ///    document-level keys; a self-contained schema comes through otherwise
    ///    unchanged). A producer-side rename of the field, or a dropped
    ///    property, fails here rather than silently at runtime.
    /// 2. The shape `webmcp.js` and the storefront rely on: an object schema
    ///    whose `required` names the fields a guest can always read.
    #[tokio::test]
    async fn webmcp_manifest_pins_the_producer_output_schema_contract() {
        let ctx = TestContext::new().await;
        let body = webmcp_manifest(&ctx, None, &real_block_infos(), &AllEnabled).await;

        let tool = body["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .find(|t| t["name"] == "get_order_status")
            .unwrap_or_else(|| panic!("get_order_status must be published: {body}"));

        let openapi = discovery_json(&ctx, "/openapi.json", "127.0.0.1:8093").await;
        let mut expected = openapi["paths"]["/b/products/orders/{id}/status"]["get"]["responses"]
            ["200"]["content"]["application/json"]["schema"]
            .clone();
        let expected_obj = expected
            .as_object_mut()
            .unwrap_or_else(|| panic!("the endpoint's response schema must be in /openapi.json"));
        expected_obj.remove("title");

        assert_eq!(
            tool["outputSchema"], expected,
            "outputSchema must be the /openapi.json projection of the same declaration, \
             minus the root title: {tool}"
        );
        assert_eq!(tool["outputSchema"]["type"], "object");
        assert_eq!(
            tool["outputSchema"]["required"],
            serde_json::json!([
                "schema_version",
                "order_id",
                "status",
                "reconciliation_status",
                "amounts",
                "subscription_cancel_at_period_end"
            ]),
            "the fields a guest can always read drifted from what ui/assets/webmcp.js and \
             the storefront rely on: {tool}"
        );
    }

    /// `list_my_purchases` is the other WebMCP tool whose output is a products
    /// contract, and the one whose shape changed when the order rows were
    /// typed: flat `PurchaseView` rows under `records`, with the guest receipt
    /// digest (`receipt_token_hash`, `receipt_token_expires_at`) withheld.
    /// Same pin as for `get_order_status`: the manifest's `outputSchema` is
    /// the `/openapi.json` projection of `GET /b/products/purchases` minus the
    /// root `title`, and no property name published under it starts with
    /// `receipt_token`.
    #[tokio::test]
    async fn webmcp_manifest_pins_list_my_purchases_to_the_typed_order_rows() {
        let ctx = TestContext::with_auth().await;
        let body = webmcp_manifest(&ctx, Some(&[]), &real_block_infos(), &AllEnabled).await;

        let tool = body["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .find(|t| t["name"] == "list_my_purchases")
            .unwrap_or_else(|| {
                panic!("list_my_purchases must be published to an authenticated caller: {body}")
            });

        let openapi = discovery_json(&ctx, "/openapi.json", "127.0.0.1:8093").await;
        let mut expected = openapi["paths"]["/b/products/purchases"]["get"]["responses"]["200"]
            ["content"]["application/json"]["schema"]
            .clone();
        let expected_obj = expected
            .as_object_mut()
            .unwrap_or_else(|| panic!("the endpoint's response schema must be in /openapi.json"));
        expected_obj.remove("title");

        assert_eq!(
            tool["outputSchema"], expected,
            "outputSchema must be the /openapi.json projection of the same declaration, \
             minus the root title: {tool}"
        );

        let published = property_names(&tool["outputSchema"]);
        assert!(
            published.contains(&"refunded_total_cents".to_string()),
            "the walk must reach the order row's properties: {published:?}"
        );
        assert!(
            !published
                .iter()
                .any(|name| name.starts_with("receipt_token")),
            "the guest receipt digest must not be published to an agent: {published:?}"
        );
    }

    /// `list_products` is the agent's entry point and the only anonymous
    /// tool that returns a list of rows, so it is the one place an internal
    /// products column would reach an unauthenticated agent in bulk.
    ///
    /// Before the catalog was projected through `CatalogProductView` this
    /// endpoint echoed the stored row: `owner_id`, `created_by`,
    /// `seller_account_id`, `owner_kind`, `stripe_product_id`,
    /// `approval_status`, `submitted_at`, `current_version` and
    /// `deleted_at` were all public. Annotating it as a tool is only safe
    /// on top of that projection, and this pins the two together.
    #[tokio::test]
    async fn webmcp_manifest_pins_list_products_to_the_public_catalog_view() {
        let ctx = TestContext::new().await;
        let body = webmcp_manifest(&ctx, None, &real_block_infos(), &AllEnabled).await;

        let tool = body["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .find(|t| t["name"] == "list_products")
            .unwrap_or_else(|| {
                panic!("list_products must be published to an anonymous caller: {body}")
            });

        let published = property_names(&tool["outputSchema"]);
        assert!(
            published.contains(&"name".to_string()) && published.contains(&"slug".to_string()),
            "the walk must reach the catalog row's properties: {published:?}"
        );
        assert!(
            published.contains(&"id".to_string()),
            "the tool's own description calls it the only way to discover a \
             product id, so the id must be published: {published:?}"
        );
        for withheld in [
            "owner_id",
            "created_by",
            "seller_account_id",
            "owner_kind",
            "stripe_product_id",
            "approval_status",
            "submitted_at",
            "current_version",
            "deleted_at",
        ] {
            assert!(
                !published.contains(&withheld.to_string()),
                "the public catalog tool must not publish the internal column \
                 {withheld}: {published:?}"
            );
        }
    }

    /// Every `properties` key anywhere under `schema`: the names a consumer
    /// of the schema can read, at any depth.
    fn property_names(schema: &serde_json::Value) -> Vec<String> {
        fn walk(node: &serde_json::Value, out: &mut Vec<String>) {
            match node {
                serde_json::Value::Object(map) => {
                    if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                        out.extend(props.keys().cloned());
                    }
                    for value in map.values() {
                        walk(value, out);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        walk(item, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        walk(schema, &mut out);
        out
    }

    #[tokio::test]
    async fn webmcp_manifest_is_not_cacheable() {
        let ctx = TestContext::new().await;
        let headers =
            discovery_headers(&ctx, "/b/webmcp/manifest.json", "impresspress.example.com").await;

        let cache_control = headers
            .get("Cache-Control")
            .map(String::as_str)
            .unwrap_or_default();
        assert!(
            cache_control.contains("no-store"),
            "the manifest is per-session and must not be cached, got: {cache_control:?}"
        );
    }

    /// The stable `/b/webmcp/webmcp.js` route beside the manifest: a page
    /// written under `site/` (no SSR `ui::layout` injection, so no way to
    /// discover the content-hashed `/b/static/webmcp-{hash}.js`) can
    /// hardcode this path and get the same script.
    #[tokio::test]
    async fn webmcp_js_is_served_at_the_stable_path_for_anonymous_callers() {
        let ctx = TestContext::new().await;
        let mut msg = anon_msg("retrieve", ui::assets::WEBMCP_JS_STABLE_PATH);
        msg.set_meta("http.header.host", "impresspress.example.com");
        let out = handle_request(
            &ctx,
            msg,
            InputStream::from_bytes(Vec::new()),
            None,
            TEST_JWT_SECRET,
            false,
            &AllEnabled,
            &real_block_infos(),
            &[],
        )
        .await;
        let buf = collect_or_panic(out).await;

        assert_eq!(
            buf.body,
            ui::assets::webmcp_js().as_bytes(),
            "stable-path route must serve the same composed script as the hashed URL"
        );

        let header = |key: &str| {
            buf.meta
                .iter()
                .find(|m| m.key == key)
                .map(|m| m.value.as_str())
        };
        assert_eq!(
            header(wafer_run::META_RESP_CONTENT_TYPE),
            Some("application/javascript; charset=utf-8")
        );
        assert_eq!(
            header("resp.header.Cache-Control"),
            Some("no-cache"),
            "stable path must be revalidated every load, not cached like the immutable hashed URL"
        );
        let expected_etag = format!("\"{}\"", ui::assets::webmcp_js_hash());
        assert_eq!(
            header("resp.header.ETag"),
            Some(expected_etag.as_str()),
            "ETag must be the hash webmcp_js_url() embeds, as an RFC 9110 quoted-string"
        );
        assert!(
            ui::assets::webmcp_js_url()
                .ends_with(&format!("webmcp-{}.js", ui::assets::webmcp_js_hash())),
            "webmcp_js_hash() must be the same hash webmcp_js_url() embeds: {}",
            ui::assets::webmcp_js_url()
        );
        assert_eq!(
            header("resp.header.X-Content-Type-Options"),
            Some("nosniff"),
            "a public, unauthenticated script route must not be MIME-sniffable"
        );
    }

    /// The `ETag` the previous test pins is not decorative: a repeat visitor
    /// who echoes it back in `If-None-Match` gets a bodyless `304`, and a
    /// stale/foreign value still gets the full `200`.
    #[tokio::test]
    async fn webmcp_js_stable_path_answers_conditional_get() {
        let ctx = TestContext::new().await;
        let etag = format!("\"{}\"", ui::assets::webmcp_js_hash());

        let mut fresh = anon_msg("retrieve", ui::assets::WEBMCP_JS_STABLE_PATH);
        fresh.set_meta("http.header.host", "impresspress.example.com");
        fresh.set_meta("http.header.if-none-match", &etag);
        let out = handle_request(
            &ctx,
            fresh,
            InputStream::from_bytes(Vec::new()),
            None,
            TEST_JWT_SECRET,
            false,
            &AllEnabled,
            &real_block_infos(),
            &[],
        )
        .await;
        let buf = collect_or_panic(out).await;
        assert!(
            buf.body.is_empty(),
            "a matching If-None-Match must produce an empty 304 body"
        );
        assert_eq!(
            buf.meta
                .iter()
                .find(|m| m.key == wafer_run::META_RESP_STATUS)
                .map(|m| m.value.as_str()),
            Some("304")
        );
        assert_eq!(
            buf.meta
                .iter()
                .find(|m| m.key == "resp.header.ETag")
                .map(|m| m.value.as_str()),
            Some(etag.as_str()),
            "a 304 still carries the ETag"
        );

        let mut stale = anon_msg("retrieve", ui::assets::WEBMCP_JS_STABLE_PATH);
        stale.set_meta("http.header.host", "impresspress.example.com");
        stale.set_meta("http.header.if-none-match", "\"not-the-current-hash\"");
        let out = handle_request(
            &ctx,
            stale,
            InputStream::from_bytes(Vec::new()),
            None,
            TEST_JWT_SECRET,
            false,
            &AllEnabled,
            &real_block_infos(),
            &[],
        )
        .await;
        let buf = collect_or_panic(out).await;
        assert_eq!(
            buf.body,
            ui::assets::webmcp_js().as_bytes(),
            "a mismatching If-None-Match must fall through to the full 200 body"
        );
        assert_eq!(
            buf.meta
                .iter()
                .find(|m| m.key == wafer_run::META_RESP_STATUS)
                .map(|m| m.value.as_str()),
            None,
            "200 is the default status — no resp.status meta is set"
        );
    }

    #[tokio::test]
    async fn webmcp_manifest_reflects_an_authenticated_caller() {
        let ctx = TestContext::with_auth().await;

        // A valid session for a non-admin user (empty `roles` claim).
        let body = webmcp_manifest(&ctx, Some(&[]), &real_block_infos(), &AllEnabled).await;
        let names = tool_names(&body);

        assert!(
            names.contains(&"list_my_purchases"),
            "an authenticated caller must receive Authenticated-level tools — if this \
             fails with only Public tools present, the manifest branch is running \
             before auth meta is set (step 0 instead of after step 2): {names:?}"
        );
    }

    /// `caller_auth_level`'s Admin branch: an `admin` role in the verified
    /// JWT must raise the manifest's ceiling to Admin, not stop at
    /// Authenticated.
    #[tokio::test]
    async fn webmcp_manifest_reflects_an_admin_caller() {
        let ctx = TestContext::with_auth().await;
        let mut infos = real_block_infos();
        infos.push(admin_tool_block());

        let as_admin = webmcp_manifest(&ctx, Some(&["admin"]), &infos, &AllEnabled).await;
        let admin_names = tool_names(&as_admin);
        assert!(
            admin_names.contains(&"admin_only_probe"),
            "an admin session must receive Admin-level tools: {admin_names:?}"
        );
        assert!(
            admin_names.contains(&"list_my_purchases"),
            "an admin is also authenticated — the lower tiers must still be there: {admin_names:?}"
        );

        // The discriminating half: the SAME blocks, for a logged-in caller
        // without the role. If this passed too, the assertions above would
        // prove nothing about the Admin branch specifically.
        let as_user = webmcp_manifest(&ctx, Some(&[]), &infos, &AllEnabled).await;
        let user_names = tool_names(&as_user);
        assert!(
            !user_names.contains(&"admin_only_probe"),
            "a logged-in non-admin must NOT receive Admin-level tools: {user_names:?}"
        );
    }

    /// No Admin-tier tool anywhere is a write.
    ///
    /// `admin/mod.rs` has its own copy of this over `AdminBlock`, which is
    /// where an author annotating an admin endpoint will trip it. This one is
    /// the backstop: the policy is about the Admin tier, not about one block,
    /// so a POST tool declared under an admin-access route in any other block
    /// has to fail somewhere too.
    #[test]
    fn no_admin_tier_tool_is_a_write() {
        for block in real_block_infos() {
            for ep in &block.endpoints {
                if !ep.is_agent_tool() {
                    continue;
                }
                if routing::effective_access(&block, ep, &[]) != AuthLevel::Admin {
                    continue;
                }
                assert_eq!(
                    ep.method,
                    wafer_run::HttpMethod::Get,
                    "{} {} is an Admin-tier agent tool and must be a read: a tool's \
                     execute runs with the visitor's full ambient authority",
                    block.name,
                    ep.path
                );
            }
        }
    }

    /// The same three-tier property as above, but against the tools the
    /// admin block actually ships rather than the `admin_only_probe`
    /// fixture.
    ///
    /// The design spec calls the Admin tier the thinnest coverage on the
    /// impresspress side and the level where a filtering mistake is most
    /// costly: these four tools read the site's user list, its roles, its
    /// configuration and its audit trail. A fixture cannot catch an admin
    /// endpoint that was mis-tiered in `admin/mod.rs` — only the real
    /// `BlockInfo` can.
    #[tokio::test]
    async fn shipped_admin_tools_reach_only_admin_callers() {
        const ADMIN_TOOLS: [&str; 4] = [
            "list_users",
            "list_roles",
            "get_site_settings",
            "list_audit_log",
        ];

        let ctx = TestContext::with_auth().await;
        let infos = real_block_infos();

        let admin_body = webmcp_manifest(&ctx, Some(&["admin"]), &infos, &AllEnabled).await;
        let as_admin = tool_names(&admin_body);
        for tool in ADMIN_TOOLS {
            assert!(
                as_admin.contains(&tool),
                "an admin session must receive the shipped admin tool {tool}: {as_admin:?}"
            );

            // Presence of the name is not enough. The producer drops an
            // `outputSchema` it cannot vouch for and still publishes the
            // tool, so a refused schema is invisible to a name check: an
            // agent gets a tool whose result it cannot interpret.
            // `AdminSettingsResponse` was exactly this — a free-form map
            // (`additionalProperties: true`) that the wall refused.
            let published = admin_body["tools"]
                .as_array()
                .expect("tools array")
                .iter()
                .find(|t| t["name"] == tool)
                .expect("just asserted present");
            assert_eq!(
                published["outputSchema"]["type"], "object",
                "{tool} must publish an object outputSchema, not a dropped or \
                 non-object one: {published}"
            );
        }

        // The two discriminating halves. A logged-in non-admin and an
        // anonymous visitor must both be told nothing about these names:
        // publishing them is recon surface, and the manifest is the only
        // place tool names are handed out.
        let user_body = webmcp_manifest(&ctx, Some(&[]), &infos, &AllEnabled).await;
        let anon_body = webmcp_manifest(&ctx, None, &infos, &AllEnabled).await;
        let as_user = tool_names(&user_body);
        let as_anon = tool_names(&anon_body);
        for tool in ADMIN_TOOLS {
            assert!(
                !as_user.contains(&tool),
                "a logged-in non-admin must NOT receive the admin tool {tool}: {as_user:?}"
            );
            assert!(
                !as_anon.contains(&tool),
                "an anonymous visitor must NOT receive the admin tool {tool}: {as_anon:?}"
            );
        }
    }

    /// The manifest must not advertise tools from a block the admin
    /// disable toggle has turned off — `route_to_block` 404s every call to
    /// such a block, so every advertised tool would fail.
    #[tokio::test]
    async fn disabled_block_contributes_no_tools() {
        // Everything on except `impresspress/products` — the shape a live
        // admin toggle produces.
        struct ProductsDisabled;
        impl FeatureConfig for ProductsDisabled {
            fn is_block_enabled(&self, full_name: &str) -> bool {
                full_name != "impresspress/products"
            }
        }

        let ctx = TestContext::new().await;
        let infos = real_block_infos();

        let enabled = webmcp_manifest(&ctx, None, &infos, &AllEnabled).await;
        assert!(
            tool_names(&enabled).contains(&"get_product"),
            "precondition: the products block publishes tools while enabled: {enabled}"
        );

        let disabled = webmcp_manifest(&ctx, None, &infos, &ProductsDisabled).await;
        assert_eq!(
            tool_names(&disabled),
            Vec::<&str>::new(),
            "a disabled block must contribute no tools — every call to it 404s: {disabled}"
        );
    }

    // -------------------------------------------------------------------
    // Refusal-logging amplification (see also
    // `builder::registration::tests::webmcp_refusals_are_logged_once_at_build`
    // for the "still reported somewhere" half of this fix).
    // -------------------------------------------------------------------

    /// The shared log-capture subscriber. Lives in `test_support` because
    /// `blocks::dev::tools` proves the same "this route must log nothing"
    /// property about `/b/dev/api/tools.json` and needs the identical
    /// machinery.
    use crate::test_support::MessageCapture;

    /// The exact text `generate_webmcp`'s wrapper (and now
    /// `builder::registration::build()`) attaches to the refusal warning —
    /// duplicated here rather than imported so this test does not depend on
    /// the message staying byte-for-byte in sync with production wording
    /// beyond this recognizable substring.
    const REFUSAL_WARNING: &str =
        "webmcp: endpoint opted in to agent-tool exposure but was refused";

    /// A block declaring two endpoints that opt into the SAME tool name —
    /// `WebMcpRefusal::DuplicateToolName`. Unlike every other refusal
    /// reason, this one is caller-dependent in general (its census is
    /// counted per auth-filtered manifest — see
    /// `generate_webmcp_report`'s doc comment), but both endpoints here
    /// declare the same `Public` auth, so the collision is visible to every
    /// caller and the distinction does not matter for this fixture. Used to
    /// prove the per-request manifest path no longer logs about it.
    fn duplicate_tool_name_block() -> BlockInfo {
        BlockInfo::new(
            "test/webmcp-refusal-fixture",
            "0.0.1",
            "http-handler@v1",
            "two endpoints sharing one tool name, on purpose",
        )
        .endpoints(vec![
            BlockEndpoint::get("/b/webmcp-refusal-fixture/one")
                .summary("first")
                .auth(AuthLevel::Public)
                .agent_tool("webmcp_refusal_fixture_dup", "first"),
            BlockEndpoint::get("/b/webmcp-refusal-fixture/two")
                .summary("second")
                .auth(AuthLevel::Public)
                .agent_tool("webmcp_refusal_fixture_dup", "second"),
        ])
    }

    /// The bug this whole fix targets: N refused endpoints previously meant
    /// N `tracing::warn!` calls on EVERY GET of the unauthenticated,
    /// `no-store` manifest route — unbounded warn-level log volume for any
    /// anonymous caller that loops the request. Refusals are static across
    /// repeated calls by the same caller (this fixture's `DuplicateToolName`
    /// refusal happens to also be the same across callers, since both
    /// endpoints share one auth tier — that is not true of the reason in
    /// general), so the per-request path must log zero of them; they are
    /// logged once elsewhere instead (see
    /// `builder::registration::tests::webmcp_refusals_are_logged_once_at_build`).
    #[tokio::test]
    async fn webmcp_manifest_request_does_not_log_refusals() {
        let ctx = TestContext::new().await;
        let infos = vec![duplicate_tool_name_block()];

        // Precondition: this fixture really does trigger a refusal — both
        // endpoints sharing the name — so a silently-inert fixture couldn't
        // make the assertion below pass vacuously.
        let (_, refused) = wafer_core::discovery::generate_webmcp_report(
            &infos,
            AuthLevel::Admin,
            |_block, ep| ep.auth,
        );
        assert_eq!(
            refused.len(),
            2,
            "precondition: both endpoints sharing the tool name must be refused: {refused:?}"
        );

        let capture = MessageCapture::default();
        let guard = tracing::subscriber::set_default(capture.clone());
        // Hit the manifest endpoint more than once — the bug is per-request
        // amplification, so one call passing would be weak evidence.
        let _first = webmcp_manifest(&ctx, None, &infos, &AllEnabled).await;
        let _second = webmcp_manifest(&ctx, None, &infos, &AllEnabled).await;
        drop(guard);

        assert_eq!(
            capture.count_containing(REFUSAL_WARNING),
            0,
            "the per-request manifest path must not log per-refusal warnings — refusals \
             are static across repeated calls and are logged once, at runtime \
             construction, not per anonymous request"
        );
    }
}

/// End-to-end proof that `handle_request` actually wires
/// `crate::csrf::enforce_origin_policy` in — not just that the policy
/// function itself is correct (covered exhaustively in `crate::csrf`'s own
/// tests). Uses `extra_routes` to reach a dispatch-probe block the same way
/// `routing::tests::extra_routes_honor_the_feature_gate` does, since the
/// built-in `ROUTES` table has no test-only entry.
#[cfg(test)]
mod csrf_wiring_tests {
    use async_trait::async_trait;
    use wafer_run::{Block as RunBlock, BlockCategory, LifecycleEvent, WaferError};

    use super::*;
    use crate::{
        features::AllEnabled,
        routing::{ExtraRoute, RouteAccess},
        test_support::{auth_msg, TestContext},
    };

    struct DispatchProbeBlock;
    #[async_trait]
    impl RunBlock for DispatchProbeBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/csrf-probe", "0.0.1", "echo@v1", "csrf wiring probe")
                .category(BlockCategory::Service)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            ResponseBuilder::new()
                .status(200)
                .body(b"DISPATCHED".to_vec(), "text/plain")
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _e: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    async fn ctx_with_probe() -> TestContext {
        let mut ctx = TestContext::new().await;
        ctx.register_block("test/csrf-probe", std::sync::Arc::new(DispatchProbeBlock));
        ctx
    }

    fn extra_route() -> Vec<ExtraRoute> {
        vec![ExtraRoute::new(
            "/x/csrf-probe",
            "test/csrf-probe",
            RouteAccess::Public,
        )]
    }

    #[tokio::test]
    async fn cookie_authenticated_cross_site_post_is_rejected_before_dispatch() {
        let ctx = ctx_with_probe().await;
        let mut msg = auth_msg("create", "/x/csrf-probe", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "cross-site");

        let out = handle_request(
            &ctx,
            msg,
            InputStream::empty(),
            None, // no Authorization header — this credential came from the cookie
            "test-secret",
            true, // cookie_authenticated
            &AllEnabled,
            &[],
            &extra_route(),
        )
        .await;

        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "cross-site cookie-authenticated POST must be rejected before block dispatch"
        );
    }

    #[tokio::test]
    async fn cookie_authenticated_same_origin_post_is_dispatched() {
        let ctx = ctx_with_probe().await;
        let mut msg = auth_msg("create", "/x/csrf-probe", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "same-origin");

        let out = handle_request(
            &ctx,
            msg,
            InputStream::empty(),
            None,
            "test-secret",
            true,
            &AllEnabled,
            &[],
            &extra_route(),
        )
        .await;

        let buf = out
            .collect_buffered()
            .await
            .expect("same-origin cookie-authenticated POST must reach dispatch");
        assert_eq!(buf.body, b"DISPATCHED");
    }

    #[tokio::test]
    async fn bearer_authenticated_cross_site_post_is_not_blocked() {
        // cookie_authenticated=false: this credential came from a real
        // `Authorization: Bearer` header, not the cookie fallback — never
        // CSRF-able, so the cross-site Sec-Fetch-Site value is irrelevant.
        let ctx = ctx_with_probe().await;
        let mut msg = auth_msg("create", "/x/csrf-probe", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "cross-site");

        let out = handle_request(
            &ctx,
            msg,
            InputStream::empty(),
            None,
            "test-secret",
            false, // cookie_authenticated
            &AllEnabled,
            &[],
            &extra_route(),
        )
        .await;

        let buf = out
            .collect_buffered()
            .await
            .expect("Bearer-authenticated cross-site POST must not be blocked");
        assert_eq!(buf.body, b"DISPATCHED");
    }
}

/// The download audit-row fix: a *marked* streamed response (file download /
/// share access) still writes its `request_logs` row from the leading-meta
/// status, while a genuinely open-ended stream (SSE — a streaming content-type
/// with no marker) skips it. Drives `handle_request` end-to-end through a stub
/// block reached via `extra_routes`, then queries `request_logs`.
#[cfg(test)]
mod streaming_audit_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use wafer_run::{
        Block as RunBlock, BlockCategory, LifecycleEvent, MetaEntry, WaferError,
        META_RESP_CONTENT_TYPE,
    };

    use super::*;
    use crate::{
        features::AllEnabled,
        routing::{ExtraRoute, RouteAccess},
        streaming::{META_RESP_STREAM, STREAM_MARKER_VALUE},
        test_support::{anon_msg, TestContext},
    };

    /// A DEFINITE streamed download: leading meta with the `resp.stream` marker
    /// + a real content-type, then a body chunk.
    struct MarkedDownloadBlock;
    #[async_trait]
    impl RunBlock for MarkedDownloadBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/dl", "0.0.1", "echo@v1", "marked download probe")
                .category(BlockCategory::Service)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::from_producer(|sink, _cancel| async move {
                let _ = sink
                    .send_meta(MetaEntry {
                        key: META_RESP_STREAM.into(),
                        value: STREAM_MARKER_VALUE.into(),
                    })
                    .await;
                let _ = sink
                    .send_meta(MetaEntry {
                        key: META_RESP_CONTENT_TYPE.into(),
                        value: "image/png".into(),
                    })
                    .await;
                let _ = sink.send_chunk(b"PNGDATA".to_vec()).await;
                let _ = sink.complete(vec![]).await;
            })
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _e: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// An OPEN-ENDED SSE stream: streaming content-type, NO marker.
    struct SseStreamBlock;
    #[async_trait]
    impl RunBlock for SseStreamBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/sse", "0.0.1", "echo@v1", "sse probe")
                .category(BlockCategory::Service)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::from_producer(|sink, _cancel| async move {
                let _ = sink
                    .send_meta(MetaEntry {
                        key: META_RESP_CONTENT_TYPE.into(),
                        value: "text/event-stream".into(),
                    })
                    .await;
                let _ = sink.send_chunk(b"data: hi\n\n".to_vec()).await;
                let _ = sink.complete(vec![]).await;
            })
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _e: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    fn route(prefix: &str, block: &str) -> Vec<ExtraRoute> {
        vec![ExtraRoute::new(
            prefix.to_string(),
            block.to_string(),
            RouteAccess::Public,
        )]
    }

    async fn drive(ctx: &TestContext, path: &str, routes: &[ExtraRoute]) {
        // Inline mode so the audit write lands in the DB synchronously (not the
        // CF wait-until queue), making the row queryable in-test.
        set_request_log_mode(RequestLogMode::Inline);
        let out = handle_request(
            ctx,
            anon_msg("retrieve", path),
            InputStream::empty(),
            None,
            "test-secret",
            false,
            &AllEnabled,
            &[],
            routes,
        )
        .await;
        // Consume the streamed response so the producer runs to completion.
        let _ = out.collect_buffered().await;
    }

    async fn request_log_count(ctx: &TestContext) -> i64 {
        db::count(ctx, crate::blocks::admin::REQUEST_LOGS_TABLE, &[])
            .await
            .expect("count request_logs")
    }

    #[tokio::test]
    async fn streamed_download_with_marker_still_writes_request_log() {
        let mut ctx = TestContext::with_admin().await;
        ctx.register_block("test/dl", Arc::new(MarkedDownloadBlock));
        drive(&ctx, "/x/dl", &route("/x/dl", "test/dl")).await;
        assert_eq!(
            request_log_count(&ctx).await,
            1,
            "a marked streamed download must still produce a request_logs row"
        );
    }

    #[tokio::test]
    async fn open_ended_sse_stream_skips_request_log() {
        let mut ctx = TestContext::with_admin().await;
        ctx.register_block("test/sse", Arc::new(SseStreamBlock));
        drive(&ctx, "/x/sse", &route("/x/sse", "test/sse")).await;
        assert_eq!(
            request_log_count(&ctx).await,
            0,
            "an open-ended SSE stream must skip the request_logs row"
        );
    }
}

#[cfg(test)]
mod request_log_mode_tests {
    use super::{
        drain_queued_request_logs, enqueue_request_log, request_log_mode, set_request_log_mode,
        RequestLogMode,
    };
    use crate::blocks::admin;

    #[test]
    fn default_mode_is_inline_and_drain_is_empty() {
        assert_eq!(request_log_mode(), RequestLogMode::Inline);
        assert!(drain_queued_request_logs().is_empty());
    }

    #[test]
    fn queued_mode_accumulates_and_drain_clears() {
        set_request_log_mode(RequestLogMode::Queued);
        let mut data = std::collections::HashMap::new();
        data.insert("path".to_string(), serde_json::json!("/x"));
        enqueue_request_log(admin::REQUEST_LOGS_TABLE, data.clone());
        enqueue_request_log(admin::REQUEST_LOGS_TABLE, data);

        let drained = drain_queued_request_logs();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].table, admin::REQUEST_LOGS_TABLE);
        assert!(drain_queued_request_logs().is_empty(), "drain must clear");

        set_request_log_mode(RequestLogMode::Inline); // restore for other tests
    }
}

#[cfg(test)]
mod request_log_policy_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use wafer_run::{
        Block as RunBlock, BlockCategory, LifecycleEvent, MetaEntry, WaferError, META_RESP_STATUS,
    };

    use super::*;
    use crate::{
        config_vars::REQUEST_LOG_CONFIG_KEY,
        features::AllEnabled,
        routing::{ExtraRoute, RouteAccess},
        test_support::{anon_msg, TestContext},
    };

    /// Answers 200. The traffic an attacker floods and Cloudflare already logs.
    struct OkBlock;
    #[async_trait]
    impl RunBlock for OkBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/ok", "0.0.1", "echo@v1", "ok probe")
                .category(BlockCategory::Service)
        }
        async fn handle(&self, _c: &dyn Context, _m: Message, _i: InputStream) -> OutputStream {
            OutputStream::from_producer(|sink, _cancel| async move {
                let _ = sink.send_chunk(b"ok".to_vec()).await;
                let _ = sink.complete(vec![]).await;
            })
        }
        async fn lifecycle(&self, _c: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// Answers 500 — the only class whose `error_message` nothing else has.
    struct BoomBlock;
    #[async_trait]
    impl RunBlock for BoomBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/boom", "0.0.1", "echo@v1", "error probe")
                .category(BlockCategory::Service)
        }
        async fn handle(&self, _c: &dyn Context, _m: Message, _i: InputStream) -> OutputStream {
            OutputStream::from_producer(|sink, _cancel| async move {
                let _ = sink
                    .send_meta(MetaEntry {
                        key: META_RESP_STATUS.into(),
                        value: "500".into(),
                    })
                    .await;
                let _ = sink.send_chunk(b"boom".to_vec()).await;
                let _ = sink.complete(vec![]).await;
            })
        }
        async fn lifecycle(&self, _c: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
            Ok(())
        }
    }

    fn route(prefix: &str, block: &str) -> Vec<ExtraRoute> {
        vec![ExtraRoute::new(prefix, block, RouteAccess::Public)]
    }

    async fn ctx_with(policy: Option<&str>) -> TestContext {
        let mut ctx = TestContext::with_admin().await;
        if let Some(policy) = policy {
            ctx.set_config(REQUEST_LOG_CONFIG_KEY, policy);
        }
        ctx.register_block("test/ok", Arc::new(OkBlock));
        ctx.register_block("test/boom", Arc::new(BoomBlock));
        ctx
    }

    async fn drive(ctx: &TestContext, path: &str, routes: &[ExtraRoute]) {
        set_request_log_mode(RequestLogMode::Inline);
        reset_request_log_budget_for_test();
        let out = handle_request(
            ctx,
            anon_msg("retrieve", path),
            InputStream::empty(),
            None,
            "test-secret",
            false,
            &AllEnabled,
            &[],
            routes,
        )
        .await;
        let _ = out.collect_buffered().await;
    }

    async fn status_codes(ctx: &TestContext) -> Vec<i64> {
        db::list_all(ctx, crate::blocks::admin::REQUEST_LOGS_TABLE, vec![])
            .await
            .expect("list request_logs")
            .into_iter()
            .map(|r| {
                r.data
                    .get("status_code")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default()
            })
            .collect()
    }

    /// The stored `path` of every row, which is what the collapse test needs
    /// and what a count test can also use.
    async fn rows(ctx: &TestContext) -> Vec<String> {
        db::list_all(ctx, crate::blocks::admin::REQUEST_LOGS_TABLE, vec![])
            .await
            .expect("list request_logs")
            .into_iter()
            .map(|r| {
                r.data
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    /// The default must not change behaviour for anyone who does not set the
    /// var: absent config means log everything, exactly as before.
    #[tokio::test]
    async fn the_default_policy_logs_every_request() {
        let ctx = ctx_with(None).await;
        drive(&ctx, "/x/ok", &route("/x/ok", "test/ok")).await;
        assert_eq!(rows(&ctx).await.len(), 1, "absent config must mean `all`");
    }

    /// The whole point: a 200 is what a flood is made of, and Cloudflare's edge
    /// already records it. Under `errors` it must cost zero D1 writes.
    #[tokio::test]
    async fn errors_policy_does_not_log_a_successful_request() {
        let ctx = ctx_with(Some("errors")).await;
        drive(&ctx, "/x/ok", &route("/x/ok", "test/ok")).await;
        assert_eq!(
            rows(&ctx).await.len(),
            0,
            "a 200 under `errors` must write nothing"
        );
    }

    /// …but the 5xx must survive, because its `error_message` is the one field
    /// no edge log can reconstruct.
    #[tokio::test]
    async fn errors_policy_still_logs_a_server_error() {
        let ctx = ctx_with(Some("errors")).await;
        drive(&ctx, "/x/boom", &route("/x/boom", "test/boom")).await;
        assert_eq!(
            rows(&ctx).await.len(),
            1,
            "a 5xx under `errors` must still be recorded"
        );
    }

    /// A 404 is fully attacker-minted and the edge counts it for free.
    #[tokio::test]
    async fn errors_policy_does_not_log_a_client_error() {
        let ctx = ctx_with(Some("errors")).await;
        drive(&ctx, "/x/nope", &[]).await;
        assert_eq!(
            rows(&ctx).await.len(),
            0,
            "4xx is attacker-controlled volume; the edge already has it"
        );
    }

    #[tokio::test]
    async fn off_policy_logs_nothing_at_all() {
        let ctx = ctx_with(Some("off")).await;
        drive(&ctx, "/x/boom", &route("/x/boom", "test/boom")).await;
        assert_eq!(rows(&ctx).await.len(), 0, "`off` must write nothing");
    }

    /// The path is attacker-supplied. Storing it verbatim lets anyone mint
    /// unbounded DISTINCT rows — and puts their text in the admin UI. An
    /// unmatched route must collapse to one fixed label.
    #[tokio::test]
    async fn an_unmatched_path_is_collapsed_not_stored_verbatim() {
        let ctx = ctx_with(Some("all")).await;
        drive(&ctx, "/x/../attacker-controlled-junk-9f2", &[]).await;
        let rows = rows(&ctx).await;
        assert_eq!(rows.len(), 1, "the request is still counted");
        assert_eq!(
            rows[0], UNMATCHED_PATH_LABEL,
            "an unmatched path must be collapsed, not echoed into the table"
        );
    }

    /// The backstop. Whatever the policy reasoning, one isolate cannot be made
    /// to write without limit — this is what makes the worst case bounded.
    #[tokio::test]
    async fn a_flood_cannot_exceed_the_per_isolate_write_ceiling() {
        let ctx = ctx_with(Some("all")).await;
        set_request_log_mode(RequestLogMode::Inline);
        reset_request_log_budget_for_test();
        for _ in 0..(REQUEST_LOG_CEILING_PER_WINDOW + 25) {
            let out = handle_request(
                &ctx,
                anon_msg("retrieve", "/x/ok"),
                InputStream::empty(),
                None,
                "test-secret",
                false,
                &AllEnabled,
                &[],
                &route("/x/ok", "test/ok"),
            )
            .await;
            let _ = out.collect_buffered().await;
        }
        assert_eq!(
            rows(&ctx).await.len(),
            REQUEST_LOG_CEILING_PER_WINDOW,
            "the ceiling must hold no matter how many requests arrive"
        );
    }

    /// An error's own code must decide the status.
    ///
    /// `pipeline` hardcoded 500 for every `TerminalNotResponse::Error`, so a
    /// `NotFound` — which `http_codec::error_code_to_http_status` maps to 404 —
    /// was served, and logged, as a server error. That is wrong on its own
    /// terms (a crawler reads 500 as "retry later" and 404 as "drop it"), and
    /// it defeats `RequestLogPolicy::Errors`: every attacker-minted junk URL
    /// would count as a 5xx and be logged.
    #[tokio::test]
    async fn an_unmatched_endpoint_is_404_not_500() {
        let ctx = ctx_with(Some("all")).await;
        drive(&ctx, "/x/nope", &[]).await;
        let codes = status_codes(&ctx).await;
        assert_eq!(
            codes,
            vec![404],
            "an unroutable endpoint is a client error, not a server error"
        );
    }
}
