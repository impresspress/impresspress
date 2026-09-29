//! Shared request pipeline — the core impresspress request handling logic.
//!
//! Both Cloudflare and native adapters call `handle_request()` after
//! converting their platform-specific HTTP types into a WAFER Message.

use std::cell::Cell;

use wafer_block::http_codec;
use wafer_core::clients::config as config_client;
use wafer_run::{
    context::Context, streams::output::TerminalNotResponse, AuthLevel, BlockEndpoint, BlockInfo,
    ErrorCode, InputStream, Message, MetaEntry, OutputStream, WaferError,
};

use crate::{
    config_vars::{APP_NAME_KEY, DEFAULT_APP_NAME, ENVIRONMENT_KEY},
    endpoint_match,
    features::FeatureConfig,
    http::ResponseBuilder,
    platform_state::request_logs::{self, NewRequestLog},
    routing::{self, ExtraRoute},
    ui,
};

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

/// The registered blocks the admin feature toggle leaves on — the set every
/// discovery projection (`/openapi.json`, the agent card and the WebMCP
/// manifest) is generated from.
///
/// `block_infos` is every REGISTERED block, but `route_to_block` 404s any
/// block the toggle has turned off (routing.rs's feature gate, backed by the
/// live `block_settings` row). Describing a disabled block's endpoints would
/// hand the reader routes that 404 on every call, so the documents are built
/// from the enabled subset only — gated under the same name the router gates
/// with (`feature_gate_name`; the inspector's `BlockInfo` name and its
/// route's `block` name differ).
fn enabled_infos(block_infos: &[BlockInfo], features: &dyn FeatureConfig) -> Vec<BlockInfo> {
    block_infos
        .iter()
        .filter(|b| features.is_block_enabled(routing::feature_gate_name(&b.name)))
        .cloned()
        .collect()
}

/// The 413 a request whose body exceeded
/// [`crate::streaming::MAX_REQUEST_BODY_BYTES`] is answered with.
///
/// An ordinary error terminal: `ResourceExhausted` with an explicit
/// `resp.status` of 413, because `ErrorCode` has no payload-too-large member
/// and `http_codec::resolve_error_status` lets the error's own status win over
/// the code's 429. Its message is [`crate::streaming::request_too_large_message`],
/// so the JSON error envelope names the limit that was enforced.
///
/// It has to stop the flow: a plain response terminal does not. The executor
/// stores its body, applies its meta to the message and runs the next step, so
/// a refusal built that way ahead of the router is followed by the router
/// serving its own body over it — `an_oversized_body_to_an_unrouted_path_is_413_not_the_spa`
/// caught exactly that, with the `wafer-run/web` fallback reached and its
/// `index.html` served under a 413 status. An error under `on_error: stop`
/// ends the flow, and the executor carries the response headers the
/// middleware steps (`wafer-run/cors`, `wafer-run/security-headers`) left on
/// the message onto it, so a cross-origin uploader's browser can read the 413
/// instead of reporting a CORS failure.
///
/// `oversized_body_flow.rs` pins that against the real executor, the real
/// `site-main` flow and the real middleware blocks.
///
/// [`refuse_oversized_body`] is what callers want: this builds the answer,
/// that one also records it.
pub fn payload_too_large_error() -> OutputStream {
    let mut error = WaferError::new(
        ErrorCode::ResourceExhausted,
        crate::streaming::request_too_large_message(),
    );
    error.meta.push(MetaEntry {
        key: wafer_run::META_RESP_STATUS.to_string(),
        value: "413".to_string(),
    });
    OutputStream::error(error)
}

/// Refuse a request whose body the transport would not carry: the 413 from
/// [`payload_too_large_error`], plus the `request_logs` row any other
/// refusal would have written.
///
/// Two callers, and they cannot both fire for one request:
/// [`crate::blocks::body_limit::BodyLimitBlock`] is a flow step ahead of the
/// router, so in the site-main flow it answers first and
/// `impresspress/router` never runs; [`handle_request`]'s own check covers a
/// consumer whose flow dispatches to the router without that step.
///
/// The row carries no user id when the block answers: it runs before JWT
/// validation, and a refusal that never reached authentication has no
/// authenticated caller to name. Everything else — method, path, client IP,
/// the 413, the duration — is what a routed refusal records, and
/// `block_infos` / `extra_routes` are what keep the path out of the
/// `<unmatched>` collapse.
pub async fn refuse_oversized_body(
    ctx: &dyn Context,
    msg: &Message,
    block_infos: &[BlockInfo],
    extra_routes: &[ExtraRoute],
) -> OutputStream {
    write_request_log(
        ctx,
        NewRequestLog {
            method: msg.action(),
            path: msg.path(),
            status_code: 413,
            error_message: "",
            duration_ms: 0,
            client_ip: msg.remote_addr(),
            user_id: msg.user_id(),
        },
        block_infos,
        extra_routes,
    )
    .await;
    payload_too_large_error()
}

/// Refuse a request whose credential could not be checked, with the error
/// `crate::blocks::auth::credential_check_failed` classified, plus the
/// `request_logs` row any other refusal would have written.
///
/// The row carries no user id: the credential that would have named one is
/// exactly what could not be checked.
async fn refuse_unchecked_credential(
    ctx: &dyn Context,
    msg: &Message,
    error: WaferError,
    block_infos: &[BlockInfo],
    extra_routes: &[ExtraRoute],
) -> OutputStream {
    write_request_log(
        ctx,
        NewRequestLog {
            method: msg.action(),
            path: msg.path(),
            status_code: i64::from(http_codec::resolve_error_status(&error)),
            error_message: &error.message,
            duration_ms: 0,
            client_ip: msg.remote_addr(),
            user_id: "",
        },
        block_infos,
        extra_routes,
    )
    .await;
    OutputStream::error(error)
}

/// Handle a impresspress request.
///
/// This is the shared entry point that both CF and native adapters call
/// after building a Message from the incoming HTTP request.
///
/// Steps:
/// 1. Refuse a request whose body the transport would not carry
///    ([`crate::streaming::META_REQ_BODY_TOO_LARGE`]) with a 413 — before
///    everything else, including the discovery/WebMCP early returns, because
///    its body is already gone. In the site-main flow
///    [`crate::blocks::body_limit`] has answered before this function runs.
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
#[expect(
    clippy::too_many_arguments,
    reason = "the single request-pipeline entry point: each argument is a distinct \
              piece of request or runtime context, none derivable here from another"
)]
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

    // 1. A body the transport refused to carry never becomes a block call.
    //    The adapter marked the message and handed over an empty body, so
    //    every route below would be answering a request whose body is gone —
    //    including the four early returns further down, which would otherwise
    //    serve a discovery document or the WebMCP asset as if nothing had
    //    happened. First, therefore, and before authentication, which a
    //    request with no body to act on does not need.
    //
    //    In the site-main flow this is unreachable: `impresspress/body-limit`
    //    is a step ahead of the router and answers first, which is what
    //    extends the refusal to the paths that never reach this function
    //    (`wafer-run/web`'s `/**` fallback). This is the same refusal for a
    //    consumer flow that routes here without that step.
    if crate::streaming::body_too_large(&msg) {
        return refuse_oversized_body(ctx, &msg, block_infos, extra_routes).await;
    }

    // 2. Validate JWT or API key and set auth meta. A credential the check
    //    could not be completed for (its database read failed) refuses the
    //    request here: continuing as anonymous would answer a signed-in
    //    caller "sign in" — the router redirects a page to the login form
    //    and refuses an API call "authentication required" — for as long as
    //    the database is unreachable.
    if let Some(header) = auth_header {
        let checked = if header.starts_with("Bearer ") {
            // [SEC-038] Read the deployment's expected issuer once per request
            // so JWTs minted under a different deployment's FRONTEND_URL get
            // rejected even if their HMAC secret matches. [SEC-042] also
            // consults the JWT blocklist via the ctx-aware extractor.
            let expected_iss = match crate::crypto::expected_issuer(ctx).await {
                Ok(expected_iss) => expected_iss,
                Err(e) => {
                    return crate::blocks::crud::db_error_internal(
                        e,
                        "pipeline: token issuer read failed",
                    )
                }
            };
            crate::crypto::extract_auth_meta(ctx, header, jwt_secret, &expected_iss, &mut msg).await
        } else if let Some(api_key) = header.strip_prefix("ApiKey ") {
            crate::blocks::auth::authenticate_api_key(ctx, api_key, &mut msg).await
        } else {
            Ok(())
        };
        if let Err(error) = checked {
            return refuse_unchecked_credential(ctx, &msg, error, block_infos, extra_routes).await;
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
        // instead of inventing a second name knob; it falls back to
        // `DEFAULT_APP_NAME`, never to the host.
        let project_name = match config_client::get_default(ctx, APP_NAME_KEY, DEFAULT_APP_NAME)
            .await
        {
            Ok(name) => name,
            Err(e) => {
                return crate::blocks::crud::db_error_internal(e, "discovery: app name read failed")
            }
        };

        // Same block set, ceiling and resolver as the manifest below, so the
        // three projections of one declaration agree on who is told about
        // it. Two projections with different disclosure rules is the pattern
        // that produced the `dedupe_hash` leak. The resolver also decides
        // each operation's OpenAPI `security` requirement, so a Public
        // endpoint the router gates as Admin is published with `bearerAuth`.
        let caller = caller_auth_level(&msg);
        let enabled_infos = enabled_infos(block_infos, features);
        let effective_auth = |block: &BlockInfo, ep: &BlockEndpoint| {
            routing::effective_access(block, ep, extra_routes)
        };

        let body = if is_openapi {
            wafer_core::discovery::generate_openapi(
                &enabled_infos,
                caller,
                effective_auth,
                &project_name,
                &crate::blocks::errors::openapi_description(),
                &server_url,
            )
        } else {
            wafer_core::discovery::generate_agent_card(
                &enabled_infos,
                caller,
                effective_auth,
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
            match config_client::get_default(ctx, ENVIRONMENT_KEY, "development").await {
                Ok(environment) => environment,
                Err(e) => {
                    return crate::blocks::crud::db_error_internal(
                        e,
                        "discovery: environment read failed",
                    )
                }
            };
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
        let enabled_infos = enabled_infos(block_infos, features);

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
            let duration_ms = i64::try_from(crate::util::now_millis().saturating_sub(start_ms))
                .unwrap_or(i64::MAX);
            write_request_log(
                ctx,
                NewRequestLog {
                    method: &method,
                    path: &path,
                    status_code,
                    error_message: "",
                    duration_ms,
                    client_ip: &client_ip,
                    user_id: &user_id,
                },
                block_infos,
                extra_routes,
            )
            .await;
        }
        return crate::streaming::rebuild_streaming(leading_meta, next_event, stream);
    }

    let (status_code, error_message, reply): (i64, String, OutputStream) =
        match crate::streaming::collect_buffered_with_prelude(stream, leading_meta, next_event)
            .await
        {
            Ok(buf) => {
                let code = i64::from(http_codec::resolve_status(&buf.meta, 200));
                (code, String::new(), replay_buffered(buf.body, buf.meta))
            }
            Err(TerminalNotResponse::Error(err)) => {
                // The error's OWN code decides the logged status. This was
                // hardcoded 500, so a `NotFound` was recorded as a server error.
                //
                // Only the audit row was wrong, never the response: every adapter
                // renders an error through `http_codec::error_to_http_response`
                // (native and Cloudflare via `collect_http_response`), which
                // resolves the status with `resolve_error_status`, so the client
                // is served the 404/403/401 the error means.
                // The row simply disagreed with the response that was sent —
                // which is what an audit row exists not to do, and what defeats
                // `RequestLogPolicy::Errors`: it selects on `status_code`, so
                // every attacker-minted junk URL would have counted as a 5xx and
                // been kept. Same function as the adapters use, so the two cannot
                // drift. See `an_unmatched_endpoint_is_logged_404_not_500`.
                //
                // An error carrying an explicit `META_RESP_STATUS` override can
                // resolve below 400; the row's label follows the code, as on every
                // arm. See `the_label_follows_the_resolved_status`.
                let message = err.message.clone();
                let code = i64::from(http_codec::resolve_error_status(&err));
                (code, message, OutputStream::error(err))
            }
            Err(TerminalNotResponse::Drop { meta }) => (
                204,
                String::new(),
                OutputStream::drop_request_with_meta(meta),
            ),
            Err(TerminalNotResponse::Continue(m)) => {
                (200, String::new(), OutputStream::continue_with(m))
            }
            Err(TerminalNotResponse::Malformed) => (
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
        NewRequestLog {
            method: &method,
            path: &path,
            status_code,
            error_message: &error_message,
            duration_ms,
            client_ip: &client_ip,
            user_id: &user_id,
        },
        block_infos,
        extra_routes,
    )
    .await;

    reply
}

/// Whether a route's `{name}` path variable binds a capability rather than an
/// identifier.
///
/// The convention, and the whole of it: the variable is called `token`, or
/// ends in `_token`. `/b/storage/direct/{token}` is the case that exists —
/// the share link's token IS the credential, checked by equality against
/// `impresspress__files__cloud_shares.token`.
///
/// Deliberately NOT `{id}`, which is a row id: redacting it would cost the
/// audit log the thing an operator opens it for. Deliberately not a substring
/// match either — `{tokenize}` is not a token.
///
/// `{key}` is the one worth spelling out, because it has two meanings in this
/// build and neither is a capability:
///
///  * an object key inside a storage bucket
///    (`/b/storage/api/buckets/{bucket}/objects/{key}`) — a filename, and
///    knowing it grants nothing: those routes are authenticated and
///    authorize per bucket;
///  * a config variable's NAME
///    (`/b/admin/api/settings/{key}`, `/b/admin/variables/{key}` and their
///    `/edit` and `/reset-to-environment` siblings) — the key, never the
///    value. `WAFER_RUN__AUTH__JWT_SECRET` in a path says which secret an
///    admin opened, which is exactly what an audit row is for; the value it
///    holds is what [`crate::secret_tables`] keeps out of every read surface.
///
/// So both are logged as they arrived.
fn path_var_is_capability(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "token" || name.ends_with("_token")
}

/// The `{name}` variables a route template binds, rest-variable marker
/// (`...`) stripped.
fn template_vars(template: &str) -> impl Iterator<Item = &str> {
    template.split('/').filter_map(|seg| {
        seg.strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .map(|name| name.strip_suffix("...").unwrap_or(name))
    })
}

/// Every declared endpoint template that binds a capability path variable.
///
/// Derived from the blocks' own `BlockInfo::endpoints` — the same declarations
/// the router resolves access from — so a new route that puts a capability in
/// its path is covered the day it is declared, with nothing to remember to add
/// here. `exactly_one_declared_route_carries_a_capability_in_its_path` is
/// where a reviewer is told that the set changed.
fn capability_path_templates(block_infos: &[BlockInfo]) -> Vec<&str> {
    block_infos
        .iter()
        .flat_map(|info| info.endpoints.iter())
        .map(|ep| ep.path.as_str())
        .filter(|template| template_vars(template).any(path_var_is_capability))
        .collect()
}

/// A path in the shape [`redact_capability_path_vars`] compares against a
/// route template: lowercased, with trailing slashes removed.
///
/// Neither transformation is what the ROUTER does — routing is case-sensitive
/// and `/b/storage/direct/x/` genuinely resolves to nothing. That asymmetry is
/// the point: the router's job is to decide what to serve, and this one's is
/// to decide what must not be written down, which has to be the more
/// suspicious of the two.
fn normalize_for_match(path: &str) -> String {
    path.trim_end_matches('/').to_ascii_lowercase()
}

/// `path` with every capability-bound segment replaced by the variable's own
/// name, or `None` when `path` resembles no such route.
///
/// `/b/storage/direct/sharetok-9f3c…` becomes `/b/storage/direct/{token}`: the
/// row still says which route was hit, which is what the audit log is for,
/// and carries none of the credential.
///
/// Matching is by route template, not by the path variables the router bound,
/// and that is the point: a request that 404s binds nothing, and a *failed*
/// share access is exactly when someone goes looking in the logs. A redaction
/// that only worked once a route had resolved would leak on every probe.
///
/// # It matches more loosely than the router does, deliberately
///
/// A URL that only NEARLY names the route still carries a live token, and the
/// ways of nearly naming it are ordinary user error rather than attacks: a
/// pasted share link with a trailing slash, a client that capitalised the
/// host-style prefix. Both 404 — no file is served — and both would otherwise
/// put the capability into the audit table in the clear.
///
/// So the match runs against a normalised path ([`normalize_for_match`]:
/// lowercased, trailing slashes trimmed) while the REBUILD takes its literal
/// and non-capability segments from the path as it arrived. The row therefore
/// keeps the casing that explains why the request missed, and loses only the
/// stray trailing slash and the credential.
///
/// Over-matching here is the cheap direction: the worst it can do is print
/// `{token}` in place of one segment of a path that was never going to serve
/// anything. Under-matching logs a live capability. `an_ordinary_path_variable_is_logged_verbatim`
/// and `redaction_keeps_every_other_segment` pin the other direction, so the
/// looseness cannot grow into redacting identifiers.
///
/// What it still does NOT normalise, stated rather than implied: duplicated
/// (`/b//storage/…`) or percent-encoded (`%2f`) separators, and `.`/`..`
/// segments. Those change how the path splits rather than how a segment reads,
/// and no adapter this runtime ships hands them through — but a path shaped
/// that way is matched by no template and so is logged as it arrived.
fn redact_capability_path_vars(path: &str, block_infos: &[BlockInfo]) -> Option<String> {
    let normalized = normalize_for_match(path);
    for template in capability_path_templates(block_infos) {
        if endpoint_match::match_template(template, &normalized).is_none() {
            continue;
        }
        let template_segments: Vec<&str> = template.split('/').collect();
        // Rebuilt from the path as it ARRIVED, not from the normalised copy,
        // so the row keeps the request's own casing. The trailing-slash trim
        // is what makes the two align segment for segment.
        let path_segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
        let mut out: Vec<&str> = Vec::with_capacity(path_segments.len());
        for (i, segment) in template_segments.iter().enumerate() {
            let var = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}'));
            let Some(var) = var else {
                // A literal segment. Taken from the PATH, not the template:
                // the match was case-insensitive, and the row should show the
                // request as it arrived.
                out.push(path_segments.get(i).copied().unwrap_or(segment));
                continue;
            };
            let rest = var.ends_with("...");
            let name = var.strip_suffix("...").unwrap_or(var);
            if path_var_is_capability(name) {
                out.push(segment);
                if rest {
                    // A rest variable binds every remaining segment, all of
                    // them part of the capability.
                    break;
                }
            } else if rest {
                out.extend(path_segments.iter().skip(i));
                break;
            } else {
                out.push(path_segments.get(i).copied().unwrap_or(segment));
            }
        }
        return Some(out.join("/"));
    }
    None
}

/// The stored `path` for a request that resembles no declared route.
///
/// The path is attacker-supplied. Storing it verbatim lets anyone mint
/// unbounded DISTINCT values by walking `/aaa1`, `/aaa2`, … and puts their
/// text into every surface that reads the table (the admin Logs page, the
/// Network page, the SQL explorer). A request that names no route carries no
/// routing information worth keeping, so all of them collapse to this one
/// label.
///
/// Deliberately narrower than "every 404": a 404 from a route that DOES exist
/// is the diagnostic case, and [`redact_capability_path_vars`] exists to keep
/// exactly those rows readable while removing the credential. Collapsing on
/// the status code would have thrown that away — a mistyped share link would
/// have become `<unmatched>` instead of `/b/storage/direct/{token}`.
/// See [`resembles_a_declared_route`].
pub const UNMATCHED_PATH_LABEL: &str = "<unmatched>";

/// Whether `path` resembles a route this build serves.
///
/// Matched the same loose way [`redact_capability_path_vars`] matches, and for
/// the same reason: this decides what is worth writing down, not what to
/// serve, so it must be the more generous of the two. A path that ALMOST names
/// a route is a user's mistake and keeps its row; only one that names nothing
/// at all collapses.
///
/// Three things count as naming a route, because a block's declared
/// `BlockEndpoint`s are not all of the routing table:
///
///  * **`/`**, which no block declares and every public site serves most.
///    [`routing::route_to_block`] answers it from an arm of its own, above all
///    block dispatch — a redirect, or the landing page through
///    `wafer-run/web`, whose `BlockInfo` declares no endpoints at all. Without
///    this arm the single highest-traffic request on a site would be stored in
///    the same bucket as attacker junk, which is the opposite of what the
///    collapse is for.
///  * `extra_routes`, the prefixes a consumer registered through
///    `ImpresspressBuilder::add_route`, whose paths are declared as endpoints
///    nowhere this function can see.
///  * every declared `BlockEndpoint` template.
///
/// [`routing::ROUTES`] is deliberately NOT consulted, and it is NOT redundant
/// with the endpoint templates: its entries are prefixes, so an *undeclared*
/// path beneath one (`/b/admin/aaa1`, `/b/admin/aaa2`, …) routes to the block
/// and is refused by the access gate without ever matching a template.
/// Consulting the prefixes would keep exactly those rows — the unbounded
/// attacker-minted key space this collapse exists to close. A real endpoint
/// under the same prefix matches its own template and is kept.
fn resembles_a_declared_route(
    path: &str,
    block_infos: &[BlockInfo],
    extra_routes: &[ExtraRoute],
) -> bool {
    if path == "/" {
        return true;
    }
    let normalized = normalize_for_match(path);
    if extra_routes
        .iter()
        .any(|route| normalized.starts_with(&normalize_for_match(&route.prefix)))
    {
        return true;
    }
    block_infos
        .iter()
        .flat_map(|info| info.endpoints.iter())
        .any(|endpoint| endpoint_match::match_template(&endpoint.path, &normalized).is_some())
}

/// What `request_logs` keeps. Set by
/// [`crate::config_vars::REQUEST_LOG_CONFIG_KEY`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestLogPolicy {
    /// Every request (minus static assets and `/health`). The default.
    All,
    /// Server errors only — 5xx.
    ///
    /// 4xx is deliberately excluded even though it looks diagnostic: it is
    /// entirely attacker-mintable (any GET to a non-route is a 404, and any
    /// unauthenticated GET to a private one is a 401) and Cloudflare's edge
    /// analytics already counts it. 5xx is the only class carrying an
    /// `error_message` no edge log can reconstruct.
    Errors,
    /// Nothing.
    Off,
}

impl RequestLogPolicy {
    /// Parse the config value. Anything unrecognised — including an empty
    /// string and an absent key — is [`RequestLogPolicy::All`], so a typo
    /// degrades to today's behaviour rather than silently disabling the audit
    /// trail.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).unwrap_or_default() {
            "errors" => Self::Errors,
            "off" => Self::Off,
            _ => Self::All,
        }
    }

    /// Whether this policy writes any row at all.
    pub fn writes_rows(self) -> bool {
        self != Self::Off
    }

    fn keeps(self, status_code: i64) -> bool {
        match self {
            Self::All => true,
            Self::Errors => status_code >= 500,
            Self::Off => false,
        }
    }

    /// Whether [`REQUEST_LOG_CEILING_PER_WINDOW`] applies.
    ///
    /// Only under [`Errors`](Self::Errors), and the asymmetry is the point.
    ///
    /// `All` is an operator saying "record everything", so a ceiling there
    /// would be the thing that binds rather than a backstop — and it binds
    /// *first-come-first-served*, so a flood of 200s in the first minute of an
    /// hour would silence a genuine 5xx in the fiftieth. That is exactly the
    /// wrong triage order, and on Cloudflare the budget is per isolate, a
    /// count Cloudflare controls and varies by traffic and colo, so an
    /// operator could not even predict what fraction of the audit trail
    /// survived. A nondeterministic cap on an audit log is worse than no cap:
    /// the operator asked for every row, and the honest answer to "that is too
    /// many rows" is `errors`, not a silent sample of `all`.
    ///
    /// Under `Errors` the kept class is 5xx only, so the ceiling is a genuine
    /// backstop against an error storm rather than a quota on ordinary
    /// traffic, and first-come-first-served is a fair sample *within one
    /// class*: the first 200 errors of an hour describe the storm as well as
    /// any other 200 would.
    fn is_bounded(self) -> bool {
        matches!(self, Self::Errors)
    }
}

/// The most `request_logs` rows one isolate/thread will write in one window
/// **under [`RequestLogPolicy::Errors`]** — see
/// [`is_bounded`](RequestLogPolicy::is_bounded) for why it is that policy and
/// no other.
///
/// The backstop against an error storm: `Errors` already drops everything an
/// attacker can mint directly, so what is left to bound is a 5xx loop the
/// application itself generates. Real error traffic never approaches this;
/// a storm hits it immediately.
///
/// It bounds a thread-local, so it is per *isolate* on Cloudflare and per
/// *tokio worker thread* natively. A flood therefore still multiplies by
/// whatever that count is — which is why it is a backstop behind the policy
/// rather than the thing the policy relies on.
pub const REQUEST_LOG_CEILING_PER_WINDOW: usize = 200;

/// The ceiling's window. Long enough that a flood cannot simply wait it out at
/// a useful rate, short enough that a genuine incident is not silenced all day.
const REQUEST_LOG_WINDOW_MS: u64 = 3_600_000;

thread_local! {
    /// `(window start ms, rows written in it)` for this isolate/thread.
    static REQUEST_LOG_BUDGET: Cell<(u64, usize)> = const { Cell::new((0, 0)) };
}

/// Claim one row against this isolate's ceiling; `false` means refuse to write.
///
/// Warns on the transition into a saturated window, once per window rather
/// than once per refused row: an audit log that has quietly stopped recording
/// looks exactly like a deployment with no traffic, which is the same reason
/// [`write_request_log`] warns when an insert fails.
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
        if used + 1 == REQUEST_LOG_CEILING_PER_WINDOW {
            tracing::warn!(
                ceiling = REQUEST_LOG_CEILING_PER_WINDOW,
                window_ms = REQUEST_LOG_WINDOW_MS,
                "request_logs write ceiling reached — further audit rows are \
                 dropped until the window rolls over"
            );
        }
        true
    })
}

/// Reset the isolate's ceiling. Tests only — a tokio worker thread outlives a
/// fixture, so without this a count would depend on test ordering.
#[cfg(test)]
pub(crate) fn reset_request_log_budget_for_test() {
    REQUEST_LOG_BUDGET.with(|budget| budget.set((0, 0)));
}

/// Write one `request_logs` audit row (best-effort; never fails the request).
/// Static-asset and health-check paths are skipped to keep the table
/// signal-heavy — the prefix is the shared `routing::STATIC_PREFIX` const so it
/// can't drift from the routing table and the `ui::assets` URL builders.
///
/// Shared by the buffered response tail and the streamed-download branch so a
/// download produces the same row on every platform, whether the adapter
/// streams or buffers its body.
///
/// Three filters run before anything is written, cheapest first: the
/// static/health skip above, the operator's [`RequestLogPolicy`], and the
/// per-isolate ceiling ([`claim_request_log_budget`]). Only then is the path
/// rewritten — credential redaction, then the unmatched-path collapse — so a
/// row that is never stored costs no template matching at all.
async fn write_request_log(
    ctx: &dyn Context,
    row: NewRequestLog<'_>,
    block_infos: &[BlockInfo],
    extra_routes: &[ExtraRoute],
) {
    if row.path.starts_with(routing::STATIC_PREFIX) || row.path == "/health" {
        return;
    }
    let policy =
        RequestLogPolicy::parse(ctx.config_get(crate::config_vars::REQUEST_LOG_CONFIG_KEY));
    if !policy.keeps(row.status_code) {
        return;
    }
    // The budget is claimed before the insert, so a failed write still spends
    // it. That is deliberate: under `errors` the thing worth bounding is the
    // number of write ATTEMPTS an error storm makes, and on Cloudflare a
    // failing D1 insert costs a subrequest exactly like a succeeding one.
    if policy.is_bounded() && !claim_request_log_budget(crate::util::now_millis()) {
        return;
    }
    // A capability that travels in the path is redacted here rather than at
    // either call site, so both the buffered tail and the streamed-download
    // branch are covered by the one rule — and so is every consumer of the
    // table downstream (the admin Logs page, the Network page, the SQL
    // explorer), because the secret never enters the row in the first place.
    //
    // Redaction runs FIRST and the collapse only where it found nothing: a
    // redacted path already names a real route, so it is a row worth keeping,
    // and a near-miss share link must stay `/b/storage/direct/{token}` rather
    // than becoming `<unmatched>`.
    let redacted = redact_capability_path_vars(row.path, block_infos);
    let row = match &redacted {
        Some(path) => NewRequestLog { path, ..row },
        None if !resembles_a_declared_route(row.path, block_infos, extra_routes) => NewRequestLog {
            path: UNMATCHED_PATH_LABEL,
            ..row
        },
        None => row,
    };
    // Queued when the platform runs this request inside an
    // `after_response::scope` (Cloudflare: the row is written after the
    // response, from room held back for it); inserted here otherwise.
    let queued = crate::after_response::QueuedRequestLog {
        table: request_logs::TABLE,
        data: row.to_data(),
    };
    if crate::after_response::queue_audit_row(queued).is_err() {
        // Best-effort: don't fail the request if logging fails — but say
        // so. The row being optional is the deliberate part; the silence
        // was not, and a deployment whose audit log has quietly stopped
        // recording looks exactly like one with no traffic.
        if let Err(error) = request_logs::insert(ctx, &row).await {
            tracing::warn!(%error, "request audit row not written");
        }
    }
}

/// Rebuild an `OutputStream` from an already-collected buffered response.
/// Used by the pipeline after intercepting the stream for logging.
fn replay_buffered(body: Vec<u8>, meta: Vec<MetaEntry>) -> OutputStream {
    OutputStream::respond_with_meta(body, meta)
}

// Every test here asserts on the document generated from
// `test_support::real_block_infos()`, which is itself gated on the full block
// set: a build missing one of those blocks would be asserting about a
// different document than the one this module describes.
#[cfg(all(
    test,
    feature = "block-files",
    feature = "block-messages",
    feature = "block-products",
    feature = "block-tickets",
    feature = "block-llm",
    feature = "block-vector"
))]
mod discovery_tests {
    //! Covers the two OpenAPI/agent-card fixes:
    //!  1. `info.title` (and the agent-card `name`) comes from
    //!     `WAFER_RUN_SHARED__APP_NAME` (fallback `DEFAULT_APP_NAME`), never from
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
        config_vars::{APP_NAME_KEY, DEFAULT_APP_NAME},
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        // The exact host shape that produced the bug: an IP:port `Host`
        // header. `host.split('.').next()` on `"127.0.0.1:8093"` yields
        // the literal string `"127"`.
        let body = discovery_json(&ctx, "/openapi.json", "127.0.0.1:8093").await;

        assert_eq!(
            body["info"]["title"], DEFAULT_APP_NAME,
            "no WAFER_RUN_SHARED__APP_NAME configured — title must fall back to the constant, not derive from the Host header: {body}"
        );
        assert_ne!(body["info"]["title"], "127");
    }

    #[tokio::test]
    async fn openapi_title_honors_configured_app_name() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        ctx.set_config(APP_NAME_KEY, "Acme Corp");
        let body = discovery_json(&ctx, "/openapi.json", "127.0.0.1:8093").await;

        assert_eq!(body["info"]["title"], "Acme Corp");
    }

    #[tokio::test]
    async fn agent_card_name_uses_the_same_configured_project_name() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        ctx.set_config(APP_NAME_KEY, "Acme Corp");
        let body = discovery_json(&ctx, "/.well-known/agent.json", "127.0.0.1:8093").await;

        assert_eq!(
            body["name"], "Acme Corp",
            "agent-card generation must use the same corrected project_name as openapi: {body}"
        );
    }

    #[tokio::test]
    async fn openapi_documents_core_auth_endpoints_with_schemas() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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

        // The row is declared `…/objects/{key...}` — impresspress's matcher
        // syntax for a rest segment. OpenAPI has no multi-segment parameter,
        // so wafer-core's projection publishes it as a plain `{key}` naming
        // the same parameter.
        let get_obj = &paths["/b/storage/api/buckets/{name}/objects/{key}"]["get"];
        assert!(
            !get_obj.is_null(),
            "get-object must appear in /openapi.json: {body}"
        );
        assert_eq!(
            get_obj["parameters"]
                .as_array()
                .expect("get-object has path parameters")
                .iter()
                .filter(|p| p["in"] == "path" && p["name"] == "key")
                .count(),
            1,
            "get-object must declare the rest segment as the {{key}} path param: {get_obj}"
        );
        assert!(
            get_obj["responses"]["200"].get("content").is_none(),
            "get-object returns raw bytes, not JSON — must not claim an application/json response: {get_obj}"
        );
    }

    #[tokio::test]
    async fn openapi_documents_core_products_endpoints_with_schemas() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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

    /// The document tells a client which errors it must not retry as they
    /// stand — the statement budget's two codes — keyed on the `code` field
    /// the SDK surfaces as `detailCode`.
    #[tokio::test]
    async fn openapi_documents_the_error_body_and_the_codes_not_to_retry() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let mut msg = anon_msg("retrieve", "/openapi.json");
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
        let doc: serde_json::Value = serde_json::from_slice(&collect_or_panic(out).await.body)
            .expect("openapi response is valid JSON");
        let description = doc["info"]["description"]
            .as_str()
            .expect("info.description is a string");
        for code in [
            wafer_block::wire::database::STATEMENT_BUDGET_EXHAUSTED,
            wafer_block::wire::database::STATEMENT_BUDGET_EXCEEDS_LIMIT,
        ] {
            assert!(description.contains(code), "{code} missing: {description}");
        }
        assert!(
            description.contains("do not retry it automatically"),
            "{description}"
        );
    }

    #[tokio::test]
    async fn agent_card_omits_skills_above_the_callers_tier() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let body = webmcp_manifest(&ctx, None, &real_block_infos(), &AllEnabled).await;

        assert_eq!(body["schema_version"], serde_json::json!(1));
        assert!(
            body["tools"].is_array(),
            "manifest must carry a tools array: {body}"
        );
    }

    #[tokio::test]
    async fn webmcp_manifest_for_anonymous_caller_contains_no_privileged_tools() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);

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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);

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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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

    /// `/openapi.json` and the agent card describe the same block set as the
    /// manifest above: a block the admin toggle turned off 404s every route,
    /// so neither document may describe one of its endpoints.
    #[tokio::test]
    async fn disabled_block_is_described_in_no_discovery_document() {
        struct ProductsDisabled;
        impl FeatureConfig for ProductsDisabled {
            fn is_block_enabled(&self, full_name: &str) -> bool {
                full_name != "impresspress/products"
            }
        }

        async fn document(
            ctx: &TestContext,
            path: &str,
            features: &dyn FeatureConfig,
        ) -> serde_json::Value {
            let mut msg = anon_msg("retrieve", path);
            msg.set_meta("http.header.host", "impresspress.example.com");
            let out = handle_request(
                ctx,
                msg,
                InputStream::from_bytes(Vec::new()),
                None,
                TEST_JWT_SECRET,
                false,
                features,
                &real_block_infos(),
                &[],
            )
            .await;
            serde_json::from_slice(&collect_or_panic(out).await.body)
                .expect("discovery response is valid JSON")
        }
        fn product_skills(card: &serde_json::Value) -> Vec<String> {
            card["skills"]
                .as_array()
                .expect("agent card skills array")
                .iter()
                .map(|s| s["id"].as_str().expect("skill id").to_string())
                .filter(|id| id.starts_with("impresspress/products/"))
                .collect()
        }
        const STOREFRONT: &str = "/b/products/storefront/config";

        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);

        let enabled = document(&ctx, "/openapi.json", &AllEnabled).await;
        assert!(
            !enabled["paths"][STOREFRONT].is_null(),
            "precondition: the products block is described while enabled: {}",
            enabled["paths"]
        );
        let disabled = document(&ctx, "/openapi.json", &ProductsDisabled).await;
        assert!(
            disabled["paths"][STOREFRONT].is_null(),
            "a disabled block's endpoints must not be in /openapi.json: {}",
            disabled["paths"]
        );

        let enabled = document(&ctx, "/.well-known/agent.json", &AllEnabled).await;
        assert!(
            !product_skills(&enabled).is_empty(),
            "precondition: the products block lists skills while enabled: {enabled}"
        );
        let disabled = document(&ctx, "/.well-known/agent.json", &ProductsDisabled).await;
        assert_eq!(
            product_skills(&disabled),
            Vec::<String>::new(),
            "a disabled block must list no skills in the agent card"
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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

/// End-to-end proof that the request path actually wires
/// `crate::csrf::enforce_origin_policy` in — not just that the policy
/// function itself is correct (covered exhaustively in `crate::csrf`'s own
/// tests). Each request goes through `TestContext::request`: the router block
/// takes the credential off the cookie or the `Authorization` header and says
/// which, and `handle_request` verifies it and applies the policy. The
/// session is a real one, signed in through the login route, so the
/// cookie-vs-header distinction is the router's own, not a flag a test sets.
/// Uses `extra_routes` to reach a dispatch-probe block the same way
/// `routing::tests::extra_routes_honor_the_feature_gate` does, since the
/// built-in `ROUTES` table has no test-only entry.
#[cfg(test)]
mod csrf_wiring_tests {
    use async_trait::async_trait;
    use wafer_run::{Block as RunBlock, BlockCategory, LifecycleEvent, WaferError};

    use super::*;
    use crate::{
        routing::{ExtraRoute, RouteAccess},
        test_support::{anon_msg, Session, TestContext},
    };

    struct DispatchProbeBlock;
    #[async_trait]
    impl RunBlock for DispatchProbeBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/csrf-probe", "0.0.1", "echo@v1", "csrf wiring probe")
                .category(BlockCategory::Service)
        }
        /// Answers with the identity the pipeline resolved.
        async fn handle(
            &self,
            _ctx: &dyn Context,
            msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            ResponseBuilder::new().status(200).body(
                format!("DISPATCHED as {:?}", msg.user_id()).into_bytes(),
                "text/plain",
            )
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _e: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    fn dispatched_as(user_id: &str) -> Vec<u8> {
        format!("DISPATCHED as {user_id:?}").into_bytes()
    }

    /// A fixture that signs people in, with the probe mounted at
    /// `/x/csrf-probe`, and a signed-in user.
    async fn ctx_with_probe() -> (TestContext, Session) {
        let mut ctx = TestContext::with_auth().await.with_sign_in_added();
        ctx.register_block("test/csrf-probe", std::sync::Arc::new(DispatchProbeBlock));
        ctx.add_extra_route(ExtraRoute::new(
            "/x/csrf-probe",
            "test/csrf-probe",
            RouteAccess::Public,
        ));
        ctx.seed_account("user-1@example.com", "correct-horse-battery", "user")
            .await;
        let session = ctx
            .sign_in("user-1@example.com", "correct-horse-battery")
            .await;
        (ctx, session)
    }

    fn probe_post(fetch_site: &str) -> Message {
        let mut msg = anon_msg("create", "/x/csrf-probe");
        msg.set_meta("http.header.sec-fetch-site", fetch_site);
        msg
    }

    #[tokio::test]
    async fn cookie_authenticated_cross_site_post_is_rejected_before_dispatch() {
        let (ctx, session) = ctx_with_probe().await;

        let out = ctx.request(session.cookie(probe_post("cross-site"))).await;

        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "cross-site cookie-authenticated POST must be rejected before block dispatch"
        );
    }

    #[tokio::test]
    async fn cookie_authenticated_same_origin_post_is_dispatched() {
        let (ctx, session) = ctx_with_probe().await;

        let out = ctx.request(session.cookie(probe_post("same-origin"))).await;

        let buf = out
            .collect_buffered()
            .await
            .expect("same-origin cookie-authenticated POST must reach dispatch");
        assert_eq!(buf.body, dispatched_as(&session.user_id));
    }

    #[tokio::test]
    async fn bearer_authenticated_cross_site_post_is_not_blocked() {
        // A real `Authorization: Bearer` header, not the cookie fallback —
        // never CSRF-able, so the cross-site Sec-Fetch-Site value is
        // irrelevant.
        let (ctx, session) = ctx_with_probe().await;

        let out = ctx.request(session.bearer(probe_post("cross-site"))).await;

        let buf = out
            .collect_buffered()
            .await
            .expect("Bearer-authenticated cross-site POST must not be blocked");
        assert_eq!(buf.body, dispatched_as(&session.user_id));
    }

    /// A header wins over the cookie: the router resolves the credential
    /// from `Authorization` when both are present, so a cross-site page
    /// cannot make its cookie count by also sending a header it cannot know.
    /// What it can send is a header that does not verify; the cookie is then
    /// ignored, the request is anonymous, and there is no session to ride.
    #[tokio::test]
    async fn a_cookie_does_not_ride_along_with_a_bogus_header() {
        let (ctx, session) = ctx_with_probe().await;

        let mut msg = session.cookie(probe_post("cross-site"));
        msg.set_meta("http.header.authorization", "Bearer not-a-token");
        let out = ctx.request(msg).await;

        // Dispatched (the probe is Public) — as nobody.
        let buf = out
            .collect_buffered()
            .await
            .expect("an anonymous request to a public route is dispatched");
        assert_eq!(
            buf.body,
            dispatched_as(""),
            "the cookie must not have signed it"
        );
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

    /// Read by the test, not by the router the requests ran as.
    async fn request_log_count(ctx: &TestContext) -> i64 {
        request_logs::paginated(&ctx.fixture(), 1, 20, "", false)
            .await
            .expect("count request_logs")
            .total_count
    }

    #[tokio::test]
    async fn streamed_download_with_marker_still_writes_request_log() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
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
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        ctx.register_block("test/sse", Arc::new(SseStreamBlock));
        drive(&ctx, "/x/sse", &route("/x/sse", "test/sse")).await;
        assert_eq!(
            request_log_count(&ctx).await,
            0,
            "an open-ended SSE stream must skip the request_logs row"
        );
    }

    /// Answers after yielding once, so two requests driven together
    /// interleave inside the handler.
    struct YieldingBlock;
    #[async_trait]
    impl RunBlock for YieldingBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/yield", "0.0.1", "echo@v1", "yielding probe")
                .category(BlockCategory::Service)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            let mut yielded = false;
            std::future::poll_fn(|cx| {
                if yielded {
                    std::task::Poll::Ready(())
                } else {
                    yielded = true;
                    cx.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
            })
            .await;
            OutputStream::respond(b"ok".to_vec())
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _e: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// Two requests interleaved in one isolate, each run inside its own
    /// `after_response` scope (as the Cloudflare adapter runs every
    /// dispatch): each request's audit row lands in its own scope — to be
    /// written later from the room its own invocation held back — and none
    /// is inserted inline.
    #[tokio::test]
    async fn interleaved_requests_queue_their_audit_rows_in_their_own_scopes() {
        use crate::after_response::{scope, AfterResponse};

        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        ctx.register_block("test/yield", Arc::new(YieldingBlock));
        let routes = route("/x/", "test/yield");
        let after_a = AfterResponse::new();
        let after_b = AfterResponse::new();

        tokio::join!(
            scope(std::rc::Rc::clone(&after_a), drive(&ctx, "/x/a", &routes)),
            scope(std::rc::Rc::clone(&after_b), drive(&ctx, "/x/b", &routes)),
        );

        let path = |after: &AfterResponse| {
            let row = after.take_audit_row().expect("the request queued its row");
            assert_eq!(row.table, request_logs::TABLE);
            row.data["path"].as_str().unwrap().to_string()
        };
        assert_eq!(path(&after_a), "/x/a");
        assert_eq!(path(&after_b), "/x/b");
        assert_eq!(
            request_log_count(&ctx).await,
            0,
            "a queued row is not also inserted on the response path"
        );
    }
}

// Driven through `test_support::real_block_infos()` — see `discovery_tests`
// for why that needs the full block set. The route whose token must not be
// logged is the files block's `/b/storage/direct/{token}`.
#[cfg(all(
    test,
    feature = "block-files",
    feature = "block-messages",
    feature = "block-products",
    feature = "block-tickets",
    feature = "block-llm",
    feature = "block-vector"
))]
mod secret_path_redaction_tests {
    //! A capability that travels in the URL path must not be copied into the
    //! audit log.
    //!
    //! `GET /b/storage/direct/{token}` is a public share link: the token IS the
    //! credential, and `impresspress__files__cloud_shares.token` stores it in
    //! the clear because the handler looks it up by equality. Writing the
    //! request path verbatim into `impresspress__admin__request_logs` put that
    //! same credential into an ops table that the admin Logs page, the Network
    //! page and the SQL explorer all read — so refusing the shares table while
    //! leaving the token in the log would have been a boundary with a hole in
    //! it, not a boundary.
    //!
    //! Redaction happens where the row is written, so every consumer is fixed
    //! by the one change, and it is derived from the route templates rather
    //! than from a hardcoded path: any endpoint that binds a `{token}` /
    //! `{*_token}` variable is covered the day it is declared.

    use super::*;
    use crate::{
        features::AllEnabled,
        platform_state::request_logs,
        test_support::{anon_msg, real_block_infos, TestContext},
    };

    /// The value a share link carries. Distinctive enough that a substring
    /// check over the whole stored row is meaningful.
    const SHARE_TOKEN: &str = "sharetok-9f3c21aa77b4e5d1";

    /// The `(path, status_code)` of every audit row, so a test can pin the
    /// status its reasoning depends on instead of asserting it in a comment.
    async fn logged_rows(ctx: &TestContext) -> Vec<(String, i64)> {
        request_logs::paginated(ctx, 1, 50, "", false)
            .await
            .expect("list request_logs")
            .rows
            .iter()
            .map(|r| (r.path.clone(), r.status_code))
            .collect()
    }

    /// Drive one request through the real pipeline with the real blocks'
    /// declared endpoints, and return the `(path, status_code)` rows it
    /// logged.
    ///
    /// No request here reaches the share handler, which is the case that
    /// matters: a *failed* share access is exactly when an operator goes
    /// looking in the logs, and a redaction that only worked on the success
    /// path would leak on every probe. Each fails at a different point, all
    /// of them before any handler, and the tests pin the status rather than
    /// asserting it in prose (the sentence this doc replaced claimed a 404
    /// none of them produce):
    ///
    ///  * `/b/storage/direct/<tok>` IS declared — `real_block_infos` includes
    ///    `FilesBlock::info()` — so it routes, and then dies in dispatch
    ///    because this harness registers no block INSTANCE: "block
    ///    'impresspress/files' not registered in TestContext";
    ///  * a capitalised spelling misses the case-sensitive `/b/storage/`
    ///    prefix in `routing::ROUTES` and is refused as "endpoint not found";
    ///  * the trailing-slash spelling matches that prefix but is not a
    ///    declared endpoint, so the access gate refuses it ("authentication
    ///    required") before the block is called.
    ///
    /// All three are recorded with the status their own `ErrorCode` resolves
    /// to — the audit tail no longer hardcodes 500 — which is the same code
    /// the client was served. The near-miss test still asserts `>= 400` rather
    /// than a code that says more than it knows: the three stop at three
    /// different points and need not agree on which 4xx they are.
    ///
    /// None of them binds `{token}` as a path variable, which is why the
    /// redaction matches templates itself instead of reading back what
    /// routing bound.
    async fn drive_and_read_rows(path: &str) -> Vec<(String, i64)> {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let infos = real_block_infos();
        let out = handle_request(
            &ctx,
            anon_msg("retrieve", path),
            InputStream::empty(),
            None,
            "test-secret",
            false,
            &AllEnabled,
            &infos,
            &[],
        )
        .await;
        let _ = out.collect_buffered().await;
        logged_rows(&ctx).await
    }

    #[tokio::test]
    async fn a_share_token_never_reaches_the_audit_log() {
        let rows = drive_and_read_rows(&format!("/b/storage/direct/{SHARE_TOKEN}")).await;
        assert_eq!(rows.len(), 1, "expected exactly one audit row: {rows:?}");
        let (path, status) = &rows[0];
        assert!(
            !path.contains(SHARE_TOKEN),
            "the share token was written to request_logs.path: {path:?}"
        );
        assert_eq!(
            path, "/b/storage/direct/{token}",
            "the row must still say which route was hit"
        );
        // Pinned, not asserted in prose: this path IS declared, so it resolves
        // and then dies in dispatch because the harness registers no block
        // instance. It never reached the share handler either way. The row
        // records 501 because "block not registered" is
        // `ErrorCode::Unimplemented`, as the runtime answers it, and the audit
        // tail takes the error's own status — the code the client was served,
        // see the `TerminalNotResponse::Error` arm of `handle_request`.
        assert_eq!(*status, 501, "{rows:?}");
    }

    /// A URL that *nearly* names the share route still carries a live token,
    /// and a near miss is ordinary user error rather than an attack: a pasted
    /// link with a trailing slash, a hostname-style capitalisation. Both 404,
    /// so no file is served — and both used to put the capability into
    /// `request_logs.path` in the clear, which is the one table this whole
    /// change exists to keep tokens out of.
    #[tokio::test]
    async fn a_near_miss_url_has_its_token_redacted_too() {
        for path in [
            // A trailing slash on a pasted share link.
            format!("/b/storage/direct/{SHARE_TOKEN}/"),
            // Capitalisation someone's client or their muscle memory added.
            format!("/b/Storage/direct/{SHARE_TOKEN}"),
            format!("/B/STORAGE/DIRECT/{SHARE_TOKEN}"),
            // Both at once.
            format!("/b/Storage/Direct/{SHARE_TOKEN}/"),
        ] {
            let rows = drive_and_read_rows(&path).await;
            assert_eq!(rows.len(), 1, "{path}: {rows:?}");
            let (logged, status) = &rows[0];
            assert!(
                !logged.contains(SHARE_TOKEN),
                "{path}: the token reached request_logs.path as {logged:?}"
            );
            // Refused, never served — see the helper's doc for where each
            // one stops. Nothing bound the token as a path variable, so only
            // the template match could have found it.
            assert!(
                *status >= 400,
                "{path} was served rather than refused: {rows:?}"
            );
        }
    }

    /// Redaction must not cost the audit log the casing an operator needs to
    /// see WHY a request missed: the row keeps the path as it was typed, with
    /// only the capability segment replaced.
    #[tokio::test]
    async fn a_near_miss_keeps_the_casing_that_explains_it() {
        let rows = drive_and_read_rows(&format!("/b/Storage/direct/{SHARE_TOKEN}")).await;
        assert_eq!(rows[0].0, "/b/Storage/direct/{token}");
    }

    #[tokio::test]
    async fn an_ordinary_path_variable_is_logged_verbatim() {
        // `{id}` is an identifier, not a capability: redacting it would cost
        // the audit log the thing it exists for.
        let rows = drive_and_read_rows("/b/admin/api/users/user_12345").await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, "/b/admin/api/users/user_12345");
    }

    /// The closed set: every declared endpoint that binds a secret path
    /// variable, as the real blocks declare them.
    ///
    /// A new route that puts a capability in its path joins this list the day
    /// it is declared, and this test is where a reviewer is told about it —
    /// the derivation is by convention, but the convention having been applied
    /// to something new is not something anyone should have to notice
    /// unprompted.
    #[test]
    fn exactly_one_declared_route_carries_a_capability_in_its_path() {
        let infos = real_block_infos();
        let mut found: Vec<String> = capability_path_templates(&infos)
            .into_iter()
            .map(str::to_string)
            .collect();
        found.sort();
        found.dedup();
        assert_eq!(
            found,
            vec!["/b/storage/direct/{token}".to_string()],
            "the set of routes carrying a capability in the path changed — \
             confirm the new one is redacted in the audit log and update this list"
        );
    }

    #[test]
    fn the_variable_convention_is_the_name_saying_token() {
        assert!(path_var_is_capability("token"));
        assert!(path_var_is_capability("share_token"));
        assert!(path_var_is_capability("TOKEN"));
        assert!(!path_var_is_capability("id"));
        assert!(!path_var_is_capability("key"));
        assert!(!path_var_is_capability("tokenize"));
    }

    /// Redaction replaces only the capability segment.
    #[test]
    fn redaction_keeps_every_other_segment() {
        let infos = vec![
            BlockInfo::new("t/x", "0", "http-handler@v1", "probe").endpoints(vec![
                wafer_run::BlockEndpoint::get("/b/x/{bucket}/{token}/meta")
                    .auth(wafer_run::AuthLevel::Public)
                    .summary("probe"),
            ]),
        ];
        assert_eq!(
            redact_capability_path_vars("/b/x/photos/abc123/meta", &infos),
            Some("/b/x/photos/{token}/meta".to_string())
        );
        assert_eq!(redact_capability_path_vars("/b/x/photos", &infos), None);
    }
}

#[cfg(test)]
mod request_log_policy_tests {
    //! What `request_logs` is allowed to cost.
    //!
    //! A row per request on a public unauthenticated route means anyone can
    //! mint rows by sending GETs. Three layers answer that, and each is tested
    //! here for the thing only it does:
    //!
    //!  1. [`RequestLogPolicy`] — WHICH requests deserve a row, the operator's
    //!     choice, defaulting to today's "all of them";
    //!  2. [`UNMATCHED_PATH_LABEL`] — what a row is allowed to STORE from a
    //!     path nobody's route claims;
    //!  3. [`REQUEST_LOG_CEILING_PER_WINDOW`] — HOW MANY rows, under
    //!     `errors` only, as a backstop against a 5xx storm.
    //!
    //! The status fix belongs here too rather than beside them: layer 1
    //! selects on `status_code`, so an audit tail that called every failure a
    //! 500 would have made `Errors` keep exactly the attacker-minted traffic
    //! it exists to drop.

    use std::sync::Arc;

    use wafer_block::core_types::{ErrorCode, LifecycleEvent, WaferError};
    use wafer_run::Block as RunBlock;

    use super::*;
    use crate::{
        config_vars::REQUEST_LOG_CONFIG_KEY,
        features::AllEnabled,
        platform_state::request_logs,
        routing::{ExtraRoute, RouteAccess},
        test_support::{anon_msg, TestContext},
    };

    /// Answers 200. The traffic a flood is made of, and the traffic
    /// Cloudflare's edge analytics already counts for free.
    struct OkBlock;

    #[wafer_block::wafer_async_trait]
    impl RunBlock for OkBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/ok", "0.1.0", "test/probe@v1", "ok probe")
        }
        async fn handle(&self, _c: &dyn Context, _m: Message, _i: InputStream) -> OutputStream {
            OutputStream::respond(b"ok".to_vec())
        }
        async fn lifecycle(&self, _c: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// Answers 500 — the only class carrying an `error_message` that no edge
    /// log can reconstruct, which is why `errors` keeps it.
    struct BoomBlock;

    #[wafer_block::wafer_async_trait]
    impl RunBlock for BoomBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/boom", "0.1.0", "test/probe@v1", "error probe")
        }
        async fn handle(&self, _c: &dyn Context, _m: Message, _i: InputStream) -> OutputStream {
            OutputStream::error(WaferError::new(ErrorCode::Internal, "boom"))
        }
        async fn lifecycle(&self, _c: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// Fails with an explicit `META_RESP_STATUS` below 400 — the case that
    /// separates "the error's code" from "every error is an ERROR row".
    struct RedirectingErrorBlock;

    #[wafer_block::wafer_async_trait]
    impl RunBlock for RedirectingErrorBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/moved", "0.1.0", "test/probe@v1", "redirect probe")
        }
        async fn handle(&self, _c: &dyn Context, _m: Message, _i: InputStream) -> OutputStream {
            OutputStream::error(WaferError {
                code: ErrorCode::Internal,
                message: "moved".to_string(),
                meta: vec![MetaEntry {
                    key: wafer_run::META_RESP_STATUS.into(),
                    value: "302".into(),
                }],
            })
        }
        async fn lifecycle(&self, _c: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// Answers the styled HTML 500 page as a RESPONSE, the way a handler
    /// that hit a failure it renders for a browser does. The pipeline sees a
    /// buffered `Response` terminal carrying status 500, not an error.
    struct HtmlServerErrorBlock;

    #[wafer_block::wafer_async_trait]
    impl RunBlock for HtmlServerErrorBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/page500", "0.1.0", "test/probe@v1", "html 500 probe")
        }
        async fn handle(&self, _c: &dyn Context, m: Message, _i: InputStream) -> OutputStream {
            crate::ui::server_error_response(&m)
        }
        async fn lifecycle(&self, _c: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// Answers a complete 403 through the `Halt` terminal.
    struct HaltForbiddenBlock;

    #[wafer_block::wafer_async_trait]
    impl RunBlock for HaltForbiddenBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/halt403", "0.1.0", "test/probe@v1", "halt 403 probe")
        }
        async fn handle(&self, _c: &dyn Context, _m: Message, _i: InputStream) -> OutputStream {
            OutputStream::halt(
                b"forbidden".to_vec(),
                vec![MetaEntry {
                    key: wafer_run::META_RESP_STATUS.into(),
                    value: "403".into(),
                }],
            )
        }
        async fn lifecycle(&self, _c: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
            Ok(())
        }
    }

    const OK_ROUTE: &str = "/x/ok";
    const BOOM_ROUTE: &str = "/x/boom";
    const MOVED_ROUTE: &str = "/x/moved";
    const PAGE_500_ROUTE: &str = "/x/page500";
    const HALT_403_ROUTE: &str = "/x/halt403";

    fn routes() -> Vec<ExtraRoute> {
        vec![
            ExtraRoute::new(OK_ROUTE, "test/ok", RouteAccess::Public),
            ExtraRoute::new(BOOM_ROUTE, "test/boom", RouteAccess::Public),
            ExtraRoute::new(MOVED_ROUTE, "test/moved", RouteAccess::Public),
            ExtraRoute::new(PAGE_500_ROUTE, "test/page500", RouteAccess::Public),
            ExtraRoute::new(HALT_403_ROUTE, "test/halt403", RouteAccess::Public),
        ]
    }

    async fn ctx_with(policy: Option<&str>) -> TestContext {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        if let Some(policy) = policy {
            ctx.set_config(REQUEST_LOG_CONFIG_KEY, policy);
        }
        ctx.register_block("test/ok", Arc::new(OkBlock));
        ctx.register_block("test/boom", Arc::new(BoomBlock));
        ctx.register_block("test/moved", Arc::new(RedirectingErrorBlock));
        ctx.register_block("test/page500", Arc::new(HtmlServerErrorBlock));
        ctx.register_block("test/halt403", Arc::new(HaltForbiddenBlock));
        ctx
    }

    /// `(path, status_code)` of every row written, sorted so a test states
    /// which rows exist without also pinning `paginated`'s newest-first order.
    async fn logged(ctx: &TestContext) -> Vec<(String, i64)> {
        let mut rows: Vec<(String, i64)> = request_logs::paginated(ctx, 1, 1000, "", false)
            .await
            .expect("list request_logs")
            .rows
            .iter()
            .map(|r| (r.path.clone(), r.status_code))
            .collect();
        rows.sort();
        rows
    }

    /// Drive one request through the real pipeline. `register_block` mirrors
    /// each block's `BlockInfo` into the context, but those carry no declared
    /// `BlockEndpoint`s — the `ExtraRoute` prefix is what both routes the
    /// request and makes the path resemble a declared route, which is exactly
    /// the consumer-registered shape `resembles_a_declared_route` has to
    /// honour.
    async fn drive(ctx: &TestContext, path: &str) {
        drive_msg(ctx, anon_msg("retrieve", path)).await;
    }

    /// [`drive`] with a caller-built request, for a test that needs headers.
    async fn drive_msg(ctx: &TestContext, msg: Message) {
        let out = handle_request(
            ctx,
            msg,
            InputStream::empty(),
            None,
            "test-secret",
            false,
            &AllEnabled,
            &[],
            &routes(),
        )
        .await;
        let _ = out.collect_buffered().await;
    }

    /// An error's own code decides the logged status.
    ///
    /// The tail hardcoded 500 for every `TerminalNotResponse::Error`, so an
    /// unroutable path — `ErrorCode::NotFound`, which
    /// `http_codec::error_code_to_http_status` maps to 404 — was RECORDED as a
    /// server error while the client was correctly SERVED a 404 (every adapter
    /// resolves its own status through `collect_http_response`). An audit row
    /// that disagrees with the response that was sent is wrong on its own
    /// terms, and it defeats `RequestLogPolicy::Errors`, which selects on this
    /// number.
    #[tokio::test]
    async fn an_unmatched_endpoint_is_logged_404_not_500() {
        let ctx = ctx_with(None).await;
        reset_request_log_budget_for_test();
        drive(&ctx, "/x/nope").await;
        assert_eq!(
            logged(&ctx).await.iter().map(|r| r.1).collect::<Vec<_>>(),
            vec![404],
            "an unroutable endpoint is a client error, not a server error",
        );
    }

    /// The `status` label follows the resolved code on the error arm too. An
    /// error carrying an explicit `META_RESP_STATUS` below 400 is served a
    /// 3xx, so a row labelled ERROR beside it would disagree with the
    /// response that was sent.
    #[tokio::test]
    async fn the_label_follows_the_resolved_status() {
        let ctx = ctx_with(Some("all")).await;
        reset_request_log_budget_for_test();
        drive(&ctx, MOVED_ROUTE).await;

        let rows = request_logs::paginated(&ctx, 1, 10, "", false)
            .await
            .expect("list request_logs")
            .rows;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].status_code, 302, "the override wins over the code");
        assert_eq!(
            rows[0].status, "OK",
            "a sub-400 status must not be labelled ERROR",
        );
    }

    /// The one stored row, and the dashboard's today/daily error counts over
    /// it, after driving a single request.
    async fn sole_row_and_dashboard_errors(
        ctx: &TestContext,
    ) -> (request_logs::RequestLogRow, i64, i64) {
        let rows = request_logs::paginated(ctx, 1, 10, "", false)
            .await
            .expect("list request_logs")
            .rows;
        assert_eq!(rows.len(), 1, "{rows:?}");
        let today_start = format!("{}T00:00:00", chrono::Utc::now().format("%Y-%m-%d"));
        let today = request_logs::today_counts(ctx, &today_start)
            .await
            .expect("today_counts");
        assert_eq!(today.requests, 1);
        let daily: i64 = request_logs::daily_counts(ctx, &today_start)
            .await
            .expect("daily_counts")
            .iter()
            .map(|d| d.errors)
            .sum();
        (rows[0].clone(), today.errors, daily)
    }

    /// A handler that renders the styled HTML 500 page answers with a
    /// buffered `Response`, not an error terminal. The row is still an error:
    /// labelled so, and counted by the dashboard's tiles and series, the same
    /// as the network page's `status_code` column counts it.
    #[tokio::test]
    async fn a_buffered_html_500_response_is_an_error_row() {
        let ctx = ctx_with(Some("all")).await;
        reset_request_log_budget_for_test();
        let mut msg = anon_msg("retrieve", PAGE_500_ROUTE);
        msg.set_meta("http.header.accept", "text/html");
        drive_msg(&ctx, msg).await;

        let (row, today_errors, daily_errors) = sole_row_and_dashboard_errors(&ctx).await;
        assert_eq!(row.status_code, 500, "the styled page is a 500");
        assert_eq!(today_errors, 1, "today_counts must count the 500");
        assert_eq!(daily_errors, 1, "daily_counts must count the 500");
        assert_eq!(row.status, "ERROR", "a buffered 500 must be labelled ERROR");
    }

    /// A complete 4xx through the `Halt` terminal is an error row too.
    #[tokio::test]
    async fn a_halted_4xx_response_is_an_error_row() {
        let ctx = ctx_with(Some("all")).await;
        reset_request_log_budget_for_test();
        drive(&ctx, HALT_403_ROUTE).await;

        let (row, today_errors, daily_errors) = sole_row_and_dashboard_errors(&ctx).await;
        assert_eq!(row.status_code, 403);
        assert_eq!(today_errors, 1, "today_counts must count the 403");
        assert_eq!(daily_errors, 1, "daily_counts must count the 403");
        assert_eq!(row.status, "ERROR", "a halted 403 must be labelled ERROR");
    }

    /// The buffered `Response` arm's success case: a 200 is not an error.
    #[tokio::test]
    async fn a_buffered_200_response_is_not_an_error_row() {
        let ctx = ctx_with(Some("all")).await;
        reset_request_log_budget_for_test();
        drive(&ctx, OK_ROUTE).await;

        let (row, today_errors, daily_errors) = sole_row_and_dashboard_errors(&ctx).await;
        assert_eq!((row.status_code, row.status.as_str()), (200, "OK"));
        assert_eq!((today_errors, daily_errors), (0, 0));
    }

    /// The default must not change for anyone who does not set the var.
    #[tokio::test]
    async fn the_default_policy_logs_every_request() {
        let ctx = ctx_with(None).await;
        reset_request_log_budget_for_test();
        drive(&ctx, OK_ROUTE).await;
        drive(&ctx, BOOM_ROUTE).await;
        assert_eq!(
            logged(&ctx).await,
            vec![(BOOM_ROUTE.to_string(), 500), (OK_ROUTE.to_string(), 200),],
            "an absent config key must mean `all`",
        );
    }

    /// An unparseable value is `all` too: a typo must degrade to today's
    /// behaviour rather than silently disable the audit trail.
    #[tokio::test]
    async fn an_unrecognised_policy_value_falls_back_to_all() {
        for value in ["", "  ", "ERRORS", "none", "true"] {
            let ctx = ctx_with(Some(value)).await;
            reset_request_log_budget_for_test();
            drive(&ctx, OK_ROUTE).await;
            assert_eq!(
                logged(&ctx).await.len(),
                1,
                "{value:?} is not a policy, so it must behave as `all`",
            );
        }
    }

    /// The whole point: a 200 is what a flood is made of, and the edge already
    /// records it. Under `errors` it must cost zero database writes.
    #[tokio::test]
    async fn errors_policy_does_not_log_a_successful_request() {
        let ctx = ctx_with(Some("errors")).await;
        reset_request_log_budget_for_test();
        drive(&ctx, OK_ROUTE).await;
        assert_eq!(
            logged(&ctx).await.len(),
            0,
            "a 200 under `errors` must write nothing",
        );
    }

    /// …but the 5xx survives, because its `error_message` is the one field no
    /// edge log can reconstruct.
    #[tokio::test]
    async fn errors_policy_still_logs_a_server_error() {
        let ctx = ctx_with(Some("errors")).await;
        reset_request_log_budget_for_test();
        drive(&ctx, BOOM_ROUTE).await;
        assert_eq!(
            logged(&ctx).await,
            vec![(BOOM_ROUTE.to_string(), 500)],
            "a 5xx under `errors` must still be recorded",
        );
    }

    /// A 4xx is fully attacker-minted and the edge counts it for free. This is
    /// also the row that only passes because the status fix landed: under the
    /// old hardcoded 500 an unroutable path would have been kept.
    #[tokio::test]
    async fn errors_policy_does_not_log_a_client_error() {
        let ctx = ctx_with(Some("errors")).await;
        reset_request_log_budget_for_test();
        drive(&ctx, "/x/nope").await;
        assert_eq!(
            logged(&ctx).await.len(),
            0,
            "4xx is attacker-controlled volume; the edge already has it",
        );
    }

    #[tokio::test]
    async fn off_policy_logs_nothing_at_all() {
        let ctx = ctx_with(Some("off")).await;
        reset_request_log_budget_for_test();
        drive(&ctx, BOOM_ROUTE).await;
        assert_eq!(logged(&ctx).await.len(), 0, "`off` must write nothing");
    }

    /// The path is attacker-supplied. Storing it verbatim lets anyone mint
    /// unbounded DISTINCT values and puts their text into every surface that
    /// reads the table.
    #[tokio::test]
    async fn a_path_resembling_no_route_is_collapsed_not_stored_verbatim() {
        let ctx = ctx_with(Some("all")).await;
        reset_request_log_budget_for_test();
        drive(&ctx, "/x/attacker-controlled-junk-9f2").await;
        assert_eq!(
            logged(&ctx).await,
            vec![(UNMATCHED_PATH_LABEL.to_string(), 404)],
            "the request is still counted, but none of its text is stored",
        );
    }

    /// The other half of that rule, and the reason it is not "every 404": a
    /// path that DOES name a route keeps its row readable. Here the route
    /// exists and the block refuses the request — the diagnostic case an
    /// operator opens the log for.
    ///
    /// Note what this does NOT cover, because an earlier draft of this comment
    /// claimed it did: `secret_path_redaction_tests`' near-miss share link is
    /// saved by the REDACTION arm, which fires first and returns `Some`, so
    /// `resembles_a_declared_route` never runs for it. The two arms overlap by
    /// construction — redaction matches a subset of the templates this
    /// function matches — so the ordering is belt-and-braces, not load-bearing
    /// for that case.
    #[tokio::test]
    async fn a_path_that_names_a_route_is_stored_even_when_it_fails() {
        let ctx = ctx_with(Some("all")).await;
        reset_request_log_budget_for_test();
        drive(&ctx, &format!("{BOOM_ROUTE}/deeper")).await;
        assert_eq!(
            logged(&ctx).await,
            vec![(format!("{BOOM_ROUTE}/deeper"), 500)],
            "a path under a registered route must not be collapsed",
        );
    }

    /// The backstop, under the one policy that has it: an error storm cannot
    /// make one isolate write without limit.
    #[tokio::test]
    async fn an_error_storm_cannot_exceed_the_per_isolate_write_ceiling() {
        let ctx = ctx_with(Some("errors")).await;
        reset_request_log_budget_for_test();
        for _ in 0..(REQUEST_LOG_CEILING_PER_WINDOW + 25) {
            drive(&ctx, BOOM_ROUTE).await;
        }
        assert_eq!(
            logged(&ctx).await.len(),
            REQUEST_LOG_CEILING_PER_WINDOW,
            "under `errors` the ceiling must hold no matter how many arrive",
        );
    }

    /// …and `all` has no ceiling at all.
    ///
    /// An operator who asked to record everything gets everything. A ceiling
    /// here would bind before the policy did, and it would bind
    /// first-come-first-served — so a flood of 200s early in a window would
    /// silence a genuine 5xx later in it, which is the wrong way round. The
    /// honest answer to "that is too many rows" is `errors`, not a silent
    /// sample of `all`. This drives past the ceiling deliberately: it is what
    /// makes `the_default_policy_logs_every_request`'s name true rather than
    /// true only for the first two requests.
    #[tokio::test]
    async fn the_all_policy_has_no_ceiling() {
        let ctx = ctx_with(Some("all")).await;
        reset_request_log_budget_for_test();
        let total = REQUEST_LOG_CEILING_PER_WINDOW + 25;
        for _ in 0..total {
            drive(&ctx, OK_ROUTE).await;
        }
        assert_eq!(
            logged(&ctx).await.len(),
            total,
            "`all` means all — no row may be dropped by a write ceiling",
        );
    }

    /// The ceiling is the policy's, not the writer's: a 5xx that `all` would
    /// have kept must not be refused because an `errors` run earlier in the
    /// same window exhausted the budget. Pins that `is_bounded` gates the
    /// claim rather than the claim happening regardless and being ignored.
    #[tokio::test]
    async fn an_exhausted_budget_does_not_reach_the_all_policy() {
        reset_request_log_budget_for_test();
        // The CURRENT window, not an arbitrary timestamp: `write_request_log`
        // claims against `now_millis()`, so a budget filled at ms 1000 would
        // simply have rolled over by then and the test would pass without
        // exercising anything.
        let now = crate::util::now_millis();
        for _ in 0..REQUEST_LOG_CEILING_PER_WINDOW {
            assert!(claim_request_log_budget(now));
        }
        assert!(
            !claim_request_log_budget(now),
            "pre-condition: this window is exhausted",
        );

        let ctx = ctx_with(Some("all")).await;
        drive(&ctx, BOOM_ROUTE).await;
        assert_eq!(
            logged(&ctx).await,
            vec![(BOOM_ROUTE.to_string(), 500)],
            "`all` must not consult a budget it does not have",
        );
    }

    // The only test in this module driven through the real block set — see
    // `discovery_tests` for why that needs every block compiled.
    #[cfg(all(
        feature = "block-files",
        feature = "block-messages",
        feature = "block-products",
        feature = "block-tickets",
        feature = "block-llm",
        feature = "block-vector"
    ))]
    #[tokio::test]
    async fn repro_site_root_is_not_collapsed() {
        let ctx = ctx_with(Some("all")).await;
        reset_request_log_budget_for_test();
        let infos = crate::test_support::real_block_infos();
        let out = handle_request(
            &ctx,
            anon_msg("retrieve", "/"),
            InputStream::empty(),
            None,
            "test-secret",
            false,
            &AllEnabled,
            &infos,
            &[],
        )
        .await;
        let _ = out.collect_buffered().await;
        let rows = logged(&ctx).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].0, "/",
            "the site root is the highest-traffic route on any public site; \
             collapsing it destroys the signal the collapse exists to create",
        );
    }

    /// Pure unit coverage of the parse, so the truth table is stated once and
    /// the request-driving tests above only have to pin the behaviour.
    #[test]
    fn policy_parse_and_selection() {
        assert_eq!(RequestLogPolicy::parse(None), RequestLogPolicy::All);
        assert_eq!(RequestLogPolicy::parse(Some("")), RequestLogPolicy::All);
        assert_eq!(
            RequestLogPolicy::parse(Some(" errors ")),
            RequestLogPolicy::Errors,
        );
        assert_eq!(RequestLogPolicy::parse(Some("off")), RequestLogPolicy::Off);
        assert_eq!(
            RequestLogPolicy::parse(Some("Errors")),
            RequestLogPolicy::All,
            "the value is matched exactly, lowercase — an unrecognised \
             spelling degrades to `all` rather than to `off`",
        );

        assert!(RequestLogPolicy::All.keeps(200));
        assert!(RequestLogPolicy::All.keeps(404));
        assert!(!RequestLogPolicy::Errors.keeps(200));
        assert!(!RequestLogPolicy::Errors.keeps(404));
        assert!(RequestLogPolicy::Errors.keeps(500));
        assert!(RequestLogPolicy::Errors.keeps(503));
        assert!(!RequestLogPolicy::Off.keeps(500));

        assert!(
            RequestLogPolicy::Errors.is_bounded(),
            "the ceiling is a backstop against a 5xx storm",
        );
        assert!(
            !RequestLogPolicy::All.is_bounded(),
            "`all` means all; a ceiling there would bind before the policy did",
        );
        assert!(
            !RequestLogPolicy::Off.is_bounded(),
            "`off` writes nothing, so there is nothing to bound",
        );
    }

    /// The window rolls over, so a ceiling reached during an incident does not
    /// silence the log for the rest of the deployment's life.
    #[test]
    fn the_ceiling_window_rolls_over() {
        reset_request_log_budget_for_test();
        for _ in 0..REQUEST_LOG_CEILING_PER_WINDOW {
            assert!(claim_request_log_budget(1_000));
        }
        assert!(!claim_request_log_budget(1_000), "the window is exhausted",);
        assert!(
            claim_request_log_budget(1_000 + REQUEST_LOG_WINDOW_MS),
            "a new window starts with a full budget",
        );
    }
}

#[cfg(test)]
mod oversized_body_tests {
    //! What a body the transport refused to carry becomes.
    //!
    //! The adapter marks the message and hands over an empty body
    //! ([`crate::streaming::META_REQ_BODY_TOO_LARGE`]); everything after that
    //! is here, on the real `handle_request`: the status, the shape of the
    //! terminal (which is what decides whether it stops the flow —
    //! `tests/oversized_body_flow.rs` pins that, and the headers the flow's
    //! middleware adds to it, against the real executor), and the audit row.

    use wafer_run::streams::output::TerminalNotResponse;

    use super::*;
    use crate::{
        features::AllEnabled,
        platform_state::request_logs,
        routing::{ExtraRoute, RouteAccess},
        streaming::{BODY_TOO_LARGE_VALUE, META_REQ_BODY_TOO_LARGE},
        test_support::{anon_msg, TestContext},
    };

    const UPLOAD_PATH: &str = "/b/storage/api/buckets/p/objects";

    /// A route declaration covering [`UPLOAD_PATH`], so the audit row keeps the
    /// path instead of collapsing to [`UNMATCHED_PATH_LABEL`] — this suite
    /// passes no `block_infos`, and an upload path nothing declares is exactly
    /// the traffic that collapse exists for.
    fn upload_route() -> Vec<ExtraRoute> {
        vec![ExtraRoute::new(
            "/b/storage/",
            "impresspress/files",
            RouteAccess::Public,
        )]
    }

    fn marked(path: &str) -> Message {
        let mut msg = anon_msg("create", path);
        msg.set_meta(META_REQ_BODY_TOO_LARGE, BODY_TOO_LARGE_VALUE);
        msg
    }

    async fn drive(ctx: &TestContext, msg: Message) -> OutputStream {
        handle_request(
            ctx,
            msg,
            InputStream::empty(),
            None,
            "test-secret",
            false,
            &AllEnabled,
            &[],
            &upload_route(),
        )
        .await
    }

    /// **Fails on the pre-fix tree**, where the adapters answered an oversized
    /// body themselves: Cloudflare returned a `worker::Error` that `run` turned
    /// into a 500 with a correlation id, and the browser failed the fetch with
    /// no status at all. It is a 413 naming the limit now.
    #[tokio::test]
    async fn a_marked_body_is_refused_with_413_and_the_enforced_limit() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let parts = http_codec::collect_http_response(drive(&ctx, marked(UPLOAD_PATH)).await).await;

        assert_eq!(parts.status, 413);
        let body: serde_json::Value =
            serde_json::from_slice(&parts.body).expect("the error envelope is JSON");
        assert_eq!(
            body["message"],
            crate::streaming::request_too_large_message(),
            "the client is told the number that was enforced"
        );
    }

    /// The refusal is an error terminal — not a plain response, which does
    /// not short-circuit a flow, so a later step would serve its own body over
    /// it. `tests/oversized_body_flow.rs` proves against the real executor
    /// that the flow's CORS and security headers reach the wire on it; this
    /// pins the terminal kind at the source.
    #[tokio::test]
    async fn the_refusal_is_an_error_terminal() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);

        match drive(&ctx, marked(UPLOAD_PATH))
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Error(error)) => {
                assert_eq!(http_codec::resolve_error_status(&error), 413);
            }
            other => panic!("expected an Error terminal, got {other:?}"),
        }
    }

    /// And it is audited like any other refusal — the adapters' own 413 wrote
    /// no `request_logs` row at all, so an operator could not see that an
    /// upload had been turned away.
    #[tokio::test]
    async fn the_refusal_is_logged_with_its_own_status() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let _ = drive(&ctx, marked(UPLOAD_PATH))
            .await
            .collect_buffered()
            .await;

        let rows = request_logs::paginated(&ctx, 1, 20, "", false)
            .await
            .expect("read request_logs")
            .rows;
        let row = rows
            .iter()
            .find(|r| r.path == UPLOAD_PATH)
            .expect("the refused upload must be audited");
        assert_eq!(row.status_code, 413);
    }

    /// An unmarked request is untouched — the check reads one meta key and
    /// nothing else, so an ordinary upload cannot be refused by it.
    #[tokio::test]
    async fn an_unmarked_request_is_not_refused() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let status = crate::test_support::output_http_status(
            drive(&ctx, anon_msg("create", UPLOAD_PATH)).await,
        )
        .await;
        assert_ne!(status, 413, "only the marker refuses");
    }
}

// Every case routes through `test_support::real_block_infos()`, the real
// route table the router's access gate reads, which is gated on the full
// block set.
#[cfg(all(
    test,
    feature = "block-files",
    feature = "block-messages",
    feature = "block-products",
    feature = "block-tickets",
    feature = "block-llm",
    feature = "block-vector"
))]
mod credential_check_tests {
    //! A credential whose check could not be completed — its database read
    //! failed — refuses the request instead of letting it continue as
    //! anonymous. Anonymous is the answer "sign in again": the router's gate
    //! redirects a page to the login form and refuses an API call, so a
    //! database blip would sign every user out of the UI.
    //!
    //! Every case drives `handle_request` with a real signed token (or a real
    //! key) in the `Authorization` header, so step 2 is what resolves it; the
    //! database fault is injected at the wire op the credential check sends.

    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use wafer_block_crypto::primitives;
    use wafer_run::{ErrorCode, InputStream, OutputStream, WaferError};

    use super::handle_request;
    use crate::{
        blocks::auth::repo::{api_keys, jwt_blocklist, users},
        features::AllEnabled,
        test_support::{
            anon_msg, real_block_infos, FailingDbOpContext, TestContext, TEST_JWT_SECRET,
        },
    };

    /// A user under a fresh id, so no earlier test has left its
    /// `auth_version` in the verify-side cache — a cache hit would answer the
    /// read this suite makes fail.
    async fn seed_user(ctx: &TestContext) -> String {
        users::insert(
            &ctx.fixture(),
            users::NewUser {
                email: format!("{}@example.com", uuid::Uuid::new_v4()),
                display_name: "Signed In".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: true,
                verification_token_hash: None,
            },
        )
        .await
        .expect("seed user")
        .id
    }

    /// An access token for `sub` carrying a `jti`, signed and issued the way
    /// `test_support::access_token_for` signs one, so step 2 reads the
    /// blocklist as well as `auth_version`.
    fn bearer(sub: &str) -> String {
        let derived = primitives::derive_block_key(
            TEST_JWT_SECRET.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );
        let mut claims = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!(sub));
        claims.insert("type".to_string(), serde_json::json!("access"));
        claims.insert(
            "iss".to_string(),
            serde_json::json!("http://localhost:5173"),
        );
        claims.insert("roles".to_string(), serde_json::json!(["user"]));
        claims.insert(
            "jti".to_string(),
            serde_json::json!(uuid::Uuid::new_v4().to_string()),
        );
        let token = primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes())
            .expect("test jwt_sign");
        format!("Bearer {token}")
    }

    /// `GET /b/auth/api/me` — a route the router admits only for a signed-in
    /// caller — as a browser page (`html`) or an API call, with
    /// `authorization`.
    async fn get_me(
        ctx: &dyn wafer_run::context::Context,
        authorization: &str,
        html: bool,
    ) -> OutputStream {
        let mut msg = anon_msg("retrieve", "/b/auth/api/me");
        msg.set_meta(
            "http.header.accept",
            if html {
                "text/html"
            } else {
                "application/json"
            },
        );
        handle_request(
            ctx,
            msg,
            InputStream::from_bytes(Vec::new()),
            Some(authorization),
            TEST_JWT_SECRET,
            false,
            &AllEnabled,
            &real_block_infos(),
            &[],
        )
        .await
    }

    /// Status, `Location` header and body of `out`, as an adapter would send
    /// them.
    async fn answer(out: OutputStream) -> (u16, String, String) {
        let response = wafer_block::http_codec::collect_http_response(out).await;
        let location = response
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("location"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let body = String::from_utf8_lossy(&response.body).into_owned();
        (response.status, location, body)
    }

    async fn signed_in_fixture() -> (TestContext, String) {
        let mut ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        ctx.set_config(crate::blocks::auth::JWT_SECRET_KEY, TEST_JWT_SECRET);
        ctx.register_block(
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
            Arc::new(crate::blocks::auth_ui::AuthUiBlock::new()),
        );
        let uid = seed_user(&ctx).await;
        (ctx, uid)
    }

    /// The control every case below departs from: with the database up, the
    /// same token reaches the handler and is answered as its user.
    #[tokio::test]
    async fn a_signed_in_caller_reaches_the_route_while_the_database_answers() {
        let (ctx, uid) = signed_in_fixture().await;
        let (status, _, body) = answer(get_me(&ctx, &bearer(&uid), false).await).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(&uid), "{body}");
    }

    /// The `auth_version` read failing is a 503 — not the router's
    /// "authentication required", and not, for a page, the redirect to the
    /// login form that signs the user out of the UI.
    #[tokio::test]
    async fn a_failed_auth_version_read_is_a_503_not_a_sign_out() {
        let (ctx, uid) = signed_in_fixture().await;
        let down = FailingDbOpContext::new(ctx, vec![("database.get", users::TABLE)]);

        let (status, location, body) = answer(get_me(&down, &bearer(&uid), false).await).await;
        assert_eq!(status, 503, "{body}");
        assert!(
            body.contains("Authentication is temporarily unavailable"),
            "the fault's own text stays in the log: {body}"
        );

        let (status, location_html, body) = answer(get_me(&down, &bearer(&uid), true).await).await;
        assert_eq!(status, 503, "{body}");
        assert!(
            location.is_empty() && location_html.is_empty(),
            "no redirect to the login form: {location} / {location_html}"
        );
    }

    /// The JWT blocklist read failing is refused the same way. Before, the
    /// lookup "failed closed" by reporting every token as blocklisted — so
    /// during an outage every signed-in caller looked logged out.
    #[tokio::test]
    async fn a_failed_blocklist_read_is_a_503_not_a_sign_out() {
        let (ctx, uid) = signed_in_fixture().await;
        let down = FailingDbOpContext::new(ctx, vec![("database.list", jwt_blocklist::TABLE)]);

        let (status, location, body) = answer(get_me(&down, &bearer(&uid), true).await).await;
        assert_eq!(status, 503, "{body}");
        assert!(
            location.is_empty(),
            "no redirect to the login form: {location}"
        );
        assert!(
            !body.contains("simulated database outage") && !body.contains(jwt_blocklist::TABLE),
            "the fault's own text stays in the log: {body}"
        );
    }

    /// A WRAP refusal keeps the code the database classifier gives it — the
    /// 403 "Access denied" — instead of becoming the login redirect.
    #[tokio::test]
    async fn a_refused_auth_version_read_keeps_its_403() {
        let (ctx, uid) = signed_in_fixture().await;
        let refused = FailingDbOpContext::failing_with(
            ctx,
            vec![("database.get", users::TABLE)],
            WaferError::new(
                ErrorCode::PermissionDenied,
                "WRAP: impresspress/router may not read the users table",
            ),
        );

        let (status, location, body) = answer(get_me(&refused, &bearer(&uid), true).await).await;
        assert_eq!(status, 403, "{body}");
        assert!(
            location.is_empty(),
            "no redirect to the login form: {location}"
        );
        assert!(body.contains("Access denied"), "{body}");
        assert!(
            !body.contains("impresspress/router"),
            "the refusal's grant and table stay in the log: {body}"
        );
    }

    /// `GET /b/auth/login` — a public page — with `authorization`, if any.
    async fn get_login_page(
        ctx: &dyn wafer_run::context::Context,
        authorization: Option<&str>,
    ) -> OutputStream {
        let mut msg = anon_msg("retrieve", "/b/auth/login");
        msg.set_meta("http.header.accept", "text/html");
        handle_request(
            ctx,
            msg,
            InputStream::from_bytes(Vec::new()),
            authorization,
            TEST_JWT_SECRET,
            false,
            &AllEnabled,
            &real_block_infos(),
            &[],
        )
        .await
    }

    /// A public route is refused too when it is presented a credential that
    /// cannot be checked — the request names a caller, and the page would
    /// otherwise be rendered for an anonymous one — while the same route
    /// without a credential has nothing to check and is served as ever.
    #[tokio::test]
    async fn a_public_route_is_refused_only_when_it_presents_a_credential() {
        let (ctx, uid) = signed_in_fixture().await;
        let down = FailingDbOpContext::new(ctx, vec![("database.get", users::TABLE)]);

        let (status, _, body) = answer(get_login_page(&down, None).await).await;
        assert_eq!(status, 200, "no credential, nothing to check: {body}");

        let (status, _, body) = answer(get_login_page(&down, Some(&bearer(&uid))).await).await;
        assert_eq!(status, 503, "{body}");
    }

    /// A real key for a fresh user, so the API-key cases fail a read the
    /// check actually reaches.
    async fn seed_key(ctx: &TestContext) -> String {
        let uid = seed_user(ctx).await;
        let raw = format!("sb_test_{}", uuid::Uuid::new_v4().simple());
        api_keys::insert(
            &ctx.fixture(),
            api_keys::NewApiKey {
                user_id: &uid,
                name: "test-key",
                key_hash: &crate::util::sha256_hex(raw.as_bytes()),
                key_prefix: "sb_test",
                expires_at: None,
            },
        )
        .await
        .expect("seed api key");
        format!("ApiKey {raw}")
    }

    /// The control for the API-key cases: with the database up, the key
    /// reaches the route as its user.
    #[tokio::test]
    async fn a_valid_api_key_reaches_the_route_while_the_database_answers() {
        let (ctx, _) = signed_in_fixture().await;
        let key = seed_key(&ctx).await;
        let (status, _, body) = answer(get_me(&ctx, &key, false).await).await;
        assert_eq!(status, 200, "{body}");
    }

    /// The key is found, and the read of its user fails: a 503, not the
    /// anonymous answer.
    #[tokio::test]
    async fn a_failed_api_key_user_lookup_is_a_503() {
        let (ctx, _) = signed_in_fixture().await;
        let key = seed_key(&ctx).await;
        let down = FailingDbOpContext::new(ctx, vec![("database.get", users::TABLE)]);

        let (status, _, body) = answer(get_me(&down, &key, false).await).await;
        assert_eq!(status, 503, "{body}");
    }

    /// The key and its user are found, and the roles read fails: a 503, not
    /// the anonymous answer and not an identity stamped with no roles.
    #[tokio::test]
    async fn a_failed_api_key_roles_lookup_is_a_503() {
        let (ctx, _) = signed_in_fixture().await;
        let key = seed_key(&ctx).await;
        let down = FailingDbOpContext::new(
            ctx,
            vec![("database.list", crate::platform_state::user_roles::TABLE)],
        );

        let (status, _, body) = answer(get_me(&down, &key, false).await).await;
        assert_eq!(status, 503, "{body}");
    }

    /// The API-key path is the same check over a different credential: a
    /// failed key lookup is a 503, not the anonymous answer that tells the
    /// key's holder it was revoked.
    #[tokio::test]
    async fn a_failed_api_key_lookup_is_a_503() {
        let (ctx, _) = signed_in_fixture().await;
        let down = FailingDbOpContext::new(ctx, vec![("database.list", api_keys::TABLE)]);

        let (status, _, body) = answer(get_me(&down, "ApiKey any-key", false).await).await;
        assert_eq!(status, 503, "{body}");
    }
}
