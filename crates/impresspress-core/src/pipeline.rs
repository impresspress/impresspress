//! Shared request pipeline — the core impresspress request handling logic.
//!
//! Both Cloudflare and native adapters call `handle_request()` after
//! converting their platform-specific HTTP types into a WAFER Message.

use std::cell::{Cell, RefCell};

use wafer_block::http_codec;
use wafer_core::clients::{config as config_client, database as db};
use wafer_run::{
    context::Context, streams::output::TerminalNotResponse, BlockInfo, ErrorCode, InputStream,
    Message, MetaEntry, OutputStream, WaferError, META_REQ_RESOURCE,
};

use crate::{
    features::FeatureConfig,
    http::ResponseBuilder,
    routing::{self, ExtraRoute},
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
    static REQUEST_LOG_QUEUE: RefCell<Vec<QueuedRequestLog>> = const { RefCell::new(Vec::new()) };
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
    REQUEST_LOG_QUEUE.with(|q| q.borrow_mut().push(QueuedRequestLog { table, data }));
}

/// Take every queued row, clearing the queue. The platform entry calls this
/// after each dispatch and persists the rows off the response path.
pub fn drain_queued_request_logs() -> Vec<QueuedRequestLog> {
    REQUEST_LOG_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Handle a impresspress request.
///
/// This is the shared entry point that both CF and native adapters call
/// after building a Message from the incoming HTTP request.
///
/// Steps:
/// 1. Strip `/api` prefix (CF convention — native doesn't use it)
/// 2. Validate JWT and set auth meta
/// 2a. CSRF: enforce the Fetch-Metadata/Origin policy for cookie-authenticated
///     unsafe-method requests (see `crate::csrf`)
/// 3. Route to the appropriate impresspress block
/// 4. Log the request to `request_logs` (async, best-effort)
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
    // 0. Discovery endpoints — public, no auth required
    let path = msg.path();
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

        let body = if is_openapi {
            wafer_core::discovery::generate_openapi(block_infos, &project_name, "", &server_url)
        } else {
            wafer_core::discovery::generate_agent_card(block_infos, &project_name, "", &server_url)
        };

        // [SEC-073] Only emit `Access-Control-Allow-Origin: *` in dev. These
        // endpoints are intentionally public-discovery (no auth, anyone can
        // GET them), but advertising `*` to every cross-origin caller in
        // production lets unauthenticated browser code at any site map the
        // whole impresspress API surface — useful reconnaissance for a targeted
        // attack. In prod we just omit the header; non-browser clients
        // (curl, the agent runtime, server-side fetchers) don't care about
        // CORS so they still see the body.
        let environment =
            config_client::get_default(ctx, "WAFER_RUN_SHARED__ENVIRONMENT", "development").await;
        let is_dev = environment.eq_ignore_ascii_case("development");

        let mut resp = ResponseBuilder::new().set_header("Cache-Control", "public, max-age=3600");
        if is_dev {
            resp = resp.set_header("Access-Control-Allow-Origin", "*");
        }
        return resp.json(&body);
    }

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

    // 3a. If the block declares a streaming Content-Type up front (SSE, raw
    //     byte stream), don't drain the response into memory just to grab a
    //     status code for the audit log. The whole point of those formats is
    //     bytes flowing while the producer is still working — buffering
    //     defeats that. Skip request_logs for these responses; the trade is
    //     intentional and acceptable for v1 (callers reach for SSE for
    //     long-lived progress / chat streams which aren't the audit-worthy
    //     short request/responses that request_logs is built for).
    let (leading_meta, next_event) = crate::streaming::drain_leading_meta(&mut stream).await;
    if crate::streaming::wants_streaming(&leading_meta) {
        return crate::streaming::rebuild_streaming(leading_meta, next_event, stream);
    }

    let (status_label, status_code, error_message, reply): (
        &'static str,
        i64,
        String,
        OutputStream,
    ) = match crate::streaming::collect_buffered_with_prelude(stream, leading_meta, next_event).await
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
            let message = err.message.clone();
            ("ERROR", 500, message, OutputStream::error(err))
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

    // Skip logging static asset requests to reduce noise (one request_logs
    // write per CSS/JS/font/logo fetch otherwise). The prefix is the shared
    // `routing::STATIC_PREFIX` const so it can't drift from the routing
    // table and the `ui::assets` URL builders again.
    if !path.starts_with(routing::STATIC_PREFIX) && path != "/health" {
        let mut data = std::collections::HashMap::new();
        data.insert("method".to_string(), serde_json::json!(method));
        data.insert("path".to_string(), serde_json::json!(path));
        data.insert("status".to_string(), serde_json::json!(status_label));
        data.insert("status_code".to_string(), serde_json::json!(status_code));
        data.insert("duration_ms".to_string(), serde_json::json!(duration_ms));
        data.insert(
            "error_message".to_string(),
            serde_json::json!(error_message),
        );
        data.insert("client_ip".to_string(), serde_json::json!(client_ip));
        data.insert("user_id".to_string(), serde_json::json!(user_id));
        crate::util::stamp_created(&mut data);

        match request_log_mode() {
            RequestLogMode::Inline => {
                // Best-effort: don't fail the request if logging fails
                let _ = db::create(ctx, crate::blocks::admin::REQUEST_LOGS_TABLE, data).await;
            }
            RequestLogMode::Queued => {
                enqueue_request_log(crate::blocks::admin::REQUEST_LOGS_TABLE, data);
            }
        }
    }

    reply
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

    use wafer_run::{Block, BlockInfo, InputStream};

    use super::handle_request;
    use crate::{
        blocks::{auth_ui::AuthUiBlock, files::FilesBlock, products::ProductsBlock},
        features::AllEnabled,
        test_support::{anon_msg, collect_or_panic, TestContext},
    };

    /// `BlockInfo` for the three blocks this PR added schemas to, fetched
    /// from the real block structs (not hand-rolled fixtures) so the test
    /// exercises the actual declarations shipped in `blocks/{auth_ui,files,
    /// products}/mod.rs`.
    fn real_block_infos() -> Vec<BlockInfo> {
        vec![
            AuthUiBlock::new().info(),
            FilesBlock::new().info(),
            ProductsBlock::new().info(),
        ]
    }

    async fn discovery_json(ctx: &TestContext, path: &str, host: &str) -> serde_json::Value {
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
        serde_json::from_slice(&buf.body).expect("discovery response is valid JSON")
    }

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
        assert_eq!(
            catalog["responses"]["200"]["content"]["application/json"]["schema"]["properties"]
                ["records"]["items"]["properties"]["data"]["properties"]["base_price"]["type"],
            "number",
            "catalog response schema must match the real products row shape: {catalog}"
        );
        // The product object schema must cover every column the migration
        // declares (see `001_products_schema.sqlite.sql`), not just a subset
        // — `SELECT *` means all 22 columns land in the real response.
        // `created_by`/`deleted_at` were previously missing entirely; pin
        // them (plus the other four FK/requires columns) so the gap can't
        // silently reopen.
        let product_props = &catalog["responses"]["200"]["content"]["application/json"]["schema"]
            ["properties"]["records"]["items"]["properties"]["data"]["properties"];
        for field in [
            "group_template_id",
            "product_template_id",
            "pricing_template_id",
            "requires",
            "created_by",
            "deleted_at",
        ] {
            assert!(
                !product_props[field].is_null(),
                "product schema is missing real column `{field}`: {product_props}"
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
        vec![ExtraRoute {
            prefix: "/x/csrf-probe".to_string(),
            access: RouteAccess::Public,
            block_name: "test/csrf-probe".to_string(),
        }]
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
