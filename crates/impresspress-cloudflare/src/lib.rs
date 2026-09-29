//! Cloudflare Workers adapter for impresspress: D1 database service, R2 storage
//! service, wasm-compatible crypto/network services, and worker entry helpers.
//!
//! Consumed by:
//! - `impresspress-cloud`'s `impresspress-worker` (multi-tenant dispatch user worker).
//! - The `impresspress build --target cloudflare` flow (single-worker consumers
//!   like wafer-site).
//!
//! This crate is wasm-only; building for native targets is not supported.
//!
//! # Where things are
//!
//! This file is the Worker entry surface and nothing else — both entry
//! points. [`run`] / [`run_with_config`] are the `fetch` shim (`run_inner`,
//! `dispatch`, the `/b/static/` R2 read-through, and the error mapping that
//! turns a failed dispatch into a response); [`run_scheduled`] /
//! [`run_scheduled_with_config`] are the `scheduled` shim, which hydrates a
//! runtime through the same cache and runs the auth retention sweep on it.
//! Everything either funnel *calls* lives beside them:
//!
//! | module | what it owns |
//! |---|---|
//! | [`services`] | the public `make_*` service constructors |
//! | [`environment`] | the runtime-identity hash and the prepared-plan identity reads |
//! | [`runtime_build`] | `build_runtime`, the two config surfaces, the three boot funnels |
//! | [`boot_hooks`] | the three `BootHooks` impls those funnels pick between |
//! | [`runtime_cache`] | the per-isolate runtime cache and its probe policy |
//! | [`deploy_endpoints`] | `/_deploy/init`, `/_deploy/prepare`, `/_deploy/prepared`, `/_deploy/verify` |
//! | [`host_policy`] | the `*.workers.dev` preview lockdown |
//!
//! The release manifest `/_deploy/verify` re-reads is not one of them: it is
//! `impresspress_core::release_inventory::ReleaseManifest`, the same type
//! `impresspress deploy` writes.

// `clippy::arc_with_non_send_sync` is stated once here, crate-wide and
// target-scoped, rather than repeated at every `Arc::new`.
//
// wafer-run's service and block traits are bounded on
// `wafer_block::compat::{MaybeSend, MaybeSync}`. Those are `Send`/`Sync` on
// native, and on wasm32 they are *unbounded* blanket markers
// (`impl<T: ?Sized> MaybeSend for T`), so `dyn DatabaseService`,
// `dyn ConfigService`, `dyn ConfigSource`, `dyn Block` and every other such
// object is `!Send + !Sync` on this target by construction.
//
// The SMART POINTER is forced at every site the lint reaches — that is the
// claim this allow rests on, and it is narrower than "the code is all
// API-shaped". `wafer_run::Wafer::register_block`, the
// `wafer_core::service_blocks::*::register_with` constructors and
// `KvCachedD1DatabaseService::{new,with_mode}` all take `Arc<dyn _>` by value,
// so a value reaching one of them is either already such an `Arc` or a
// concrete type built as `Arc` purely to coerce into one. `Rc` is not an
// option there: `Rc<T> as Arc<dyn Trait>` does not compile (checked, E0605).
//
// The VALUE TYPE is not always the API's. Of the eleven sites only three are
// production wiring (`runtime_build` x2, `services` x1); the other eight sit
// in `#[cfg(test)]`. Seven of those eight build a double this crate defines —
// `RecordingDb`, `RecordingKv`, `CountingDb`, `MockKv`, `ProbeMockKv` — and
// the eighth wraps the production `KvCachedD1DatabaseService` in a fixture.
// Their `Arc` is forced all the same, by the same coercion: a test double has
// to satisfy the very `Arc<dyn DatabaseService>` / `Arc<dyn KvBackend>`
// parameter the production path passes.
//
// On a single-threaded target none of the eleven is making a cross-thread
// claim to be wrong about.
//
// Scoped to wasm32 even though this crate is wasm-only, because that is the
// actual precondition: were it ever built for a native target, the same bounds
// would resolve to real `Send + Sync`, the lint would be accurate again, and
// this allow must not silence it.
#![cfg_attr(
    target_arch = "wasm32",
    expect(
        clippy::arc_with_non_send_sync,
        reason = "on this single-threaded target the `Arc` is forced by the \
                  trait-object bounds, not chosen over an `Rc`"
    )
)]

mod boot_hooks;
pub mod config_service;
pub mod config_source;
// Compile-time `DatabaseService` conformance assertions for the D1 and
// KV-cached adapters (wafer-run #319 shared suite). Gated behind the
// off-by-default `conformance-check` feature so the suite never enters the
// production Worker wasm; CI checks it explicitly. See the module doc.
#[cfg(feature = "conformance-check")]
mod conformance;
pub mod convert;
pub mod crypto_service;
pub mod database;
mod deploy_endpoints;
mod environment;
pub mod helpers;
mod host_policy;
pub mod kv_cached_db;
pub mod logger_service;
pub mod network_service;
mod request_services;
mod runner;
mod runtime_build;
mod runtime_cache;
mod services;
pub mod storage;

// The `make_*` constructors are the crate's public service-construction API
// (`impresspress-cloud`'s worker and the `impresspress build --target
// cloudflare` shim both call them by these paths); `services` is private so
// that surface is exactly this list.
use std::{collections::HashMap, sync::Arc};

use impresspress_core::{
    after_response::{self, AfterResponse},
    builder::ImpresspressBuilder,
};
pub use services::{
    make_config_service, make_console_logger, make_crypto_service, make_d1_database_service,
    make_fetch_network_service, make_kv_cached_database_service, make_r2_storage_service,
    release_asset_object_key,
};
use wafer_core::interfaces::storage::service::StorageService;

use crate::{
    deploy_endpoints::{
        deploy_init_endpoint, deploy_token_authorized, prepared_status_endpoint,
        prepared_verify_endpoint,
    },
    environment::CfEnvironment,
    host_policy::{host_is_version_preview, host_is_workers_dev},
    runtime_build::warm_request_services,
    services::{make_d1_database_service_concrete, make_kv_backend, resolved_log_level},
};

thread_local! {
    static ISOLATE_INITIALIZED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// One-time isolate initialization: selects [`DeferMode::Queued`] (work a
/// handler defers until after its response drains into `ctx.wait_until` — see
/// `dispatch`; spawned any other way it would be cancelled with the
/// response). Audit rows need no isolate setting: `dispatch` queues each
/// request's rows in that request's own queue. Consumers should call this
/// from their worker's `#[event(start)]` handler; `run()` also invokes it
/// behind a once-per-isolate guard, so isolates stay correct either way and
/// repeat calls are no-ops.
///
/// [`DeferMode::Queued`]: impresspress_core::deferred::DeferMode::Queued
pub fn init_isolate() {
    ISOLATE_INITIALIZED.with(|done| {
        if !done.get() {
            impresspress_core::deferred::set_mode(impresspress_core::deferred::DeferMode::Queued);
            done.set(true);
        }
    });
}

/// Worker entry shim: load D1 vars, wire services, run the consumer's
/// block registrations, dispatch the request through WAFER.
///
/// Two consumer hooks:
/// - `register_blocks` runs against the `ImpresspressBuilder` after the 6
///   services are attached and before `builder.build()`. Use builder
///   methods (`extra_block`, `add_route`, `block_config`).
/// - `register_post_build` runs against `&mut Wafer` after build and
///   before start, and additionally receives the configured R2-backed
///   `StorageService` so consumers can register blocks that need direct
///   (un-namespaced) access to the bucket — for example, a static
///   asset-serving block that reads a fixed key prefix uploaded by
///   `impresspress deploy --target cloudflare`.
///
/// Binding names are hardcoded: D1 = `"DB"`, R2 = `"STORAGE"`. Consumers'
/// `wrangler.toml` must use these names.
///
/// On error in any step, returns a 500 response with the error message.
/// The error is also logged via `worker::console_log!`.
pub async fn run<F, G>(
    req: worker::Request,
    env: worker::Env,
    ctx: worker::Context,
    register_blocks: F,
    register_post_build: G,
) -> worker::Result<worker::Response>
where
    F: FnOnce(ImpresspressBuilder) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    run_with_config(
        req,
        env,
        ctx,
        HashMap::new(),
        register_blocks,
        register_post_build,
    )
    .await
}

/// Resolve a `/b/static/…` request path to the R2 object key and content
/// type for that asset, or `None` if the path is not a known asset.
///
/// The manifest lookup IS the security boundary: a filename absent from
/// `ASSETS` returns `None` before any key is constructed, so no request
/// input ever reaches a storage key — mirrors the exact-match discipline of
/// `impresspress_core::blocks::system`'s embedded-path lookup (no
/// prefix/suffix scanning), just resolved to an R2 object key instead of
/// `'static` bytes. `ASSETS` itself is unconditionally available (`build.rs`
/// always generates the manifest; only the bytes behind it are feature-gated
/// — see `impresspress_core::ui::assets`), so this works even though this
/// crate builds `impresspress-core` with `embed-assets` off.
#[cfg(not(feature = "embed-assets"))]
pub(crate) fn static_asset_target(path: &str) -> Option<(&'static str, &'static str)> {
    let filename = path.strip_prefix(impresspress_core::routing::STATIC_PREFIX)?;
    let e = impresspress_core::ui::assets::ASSETS
        .iter()
        .find(|e| e.filename == filename)?;
    Some((e.filename, e.content_type))
}

/// Stream a resolved `/b/static/` asset straight off the R2 bucket binding.
/// `key` and `content_type` come only from [`static_asset_target`]'s
/// manifest lookup — this never constructs a storage key from raw request
/// input. Headers match the embedded path (`impresspress-core`'s
/// `blocks::system`) exactly: the manifest's content type plus a one-year
/// immutable cache lifetime, safe because every filename carries a content
/// hash.
///
/// A miss in R2 (object absent) is a 404 — that should not happen for a key
/// straight from the manifest on a correctly deployed bucket, but the
/// request must not 500 if a deploy's R2 upload and Worker version somehow
/// drift.
#[cfg(not(feature = "embed-assets"))]
async fn serve_static_asset_from_r2(
    env: &worker::Env,
    key: &str,
    content_type: &str,
) -> worker::Result<worker::Response> {
    let bucket = env.bucket(runner::R2_BINDING)?;
    let Some(object) = bucket.get(key).execute().await? else {
        return worker::Response::error("not found", 404);
    };
    let body = object
        .body()
        .ok_or_else(|| worker::Error::RustError(format!("R2 object {key} has no body")))?;
    let bytes = body.bytes().await?;

    let mut response = worker::Response::from_bytes(bytes)?;
    let headers = response.headers_mut();
    headers.set("Content-Type", content_type)?;
    headers.set("Cache-Control", "public, max-age=31536000, immutable")?;
    Ok(response)
}

/// Variant of [`run`] with explicit request-current Worker configuration.
///
/// Workers cannot enumerate `Env`, so consumers pass the small allowlist of
/// application vars/secrets their blocks resolve through `wafer-run/config`.
/// These values enter only this request's ConfigService and lazy ConfigSource;
/// they are not copied into the isolate-cached Wafer snapshot. Their hash is
/// nevertheless part of runtime identity because a builder may consume one
/// structurally while registering middleware/routes.
pub async fn run_with_config<F, G>(
    req: worker::Request,
    env: worker::Env,
    ctx: worker::Context,
    request_config: HashMap<String, String>,
    register_blocks: F,
    register_post_build: G,
) -> worker::Result<worker::Response>
where
    F: FnOnce(ImpresspressBuilder) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    // Every `worker::Env` var and secret this request needs, read once, here.
    // `worker::Env` keeps travelling alongside it for the D1/KV/R2 *bindings*,
    // which are not var reads. See `environment`'s module doc.
    let environment = CfEnvironment::capture(&env);
    // The D1 statements this invocation sends, counted across every D1
    // service it builds (its request services, a runtime build, the audit-row
    // write it hands to `ctx.wait_until`), against D1's per-invocation query
    // limit. Created here, per invocation, and never kept: an isolate
    // interleaves concurrent requests, and each has its own limit. See
    // `database`'s module docs.
    let queries = database::D1QueryCount::new();

    // `std::env` is stubbed to always-empty on `wasm32-unknown-unknown`, so
    // `impresspress_core::ui::assets::base_url()` can never observe
    // `IMPRESSPRESS_ASSET_BASE_URL` through it here. `worker::Env::var` is
    // the one channel that does carry a Worker `[vars]` entry, so push the
    // captured value into `base_url()`'s platform override before any code
    // path below can render a page (and therefore call `base_url()`).
    // Idempotent (see `set_base_url_override`'s doc) — safe to call on
    // every request, including the fresh-runtime `/_deploy/*` funnels.
    impresspress_core::ui::assets::set_base_url_override(environment.asset_base_url());

    if req.path() == "/_deploy/verify" {
        return prepared_verify_endpoint(&req, &env, &environment).await;
    }
    if req.path() == "/_deploy/prepared" {
        return prepared_status_endpoint(&req, &environment);
    }
    if req.path() == "/_deploy/init" || req.path() == "/_deploy/prepare" {
        let prepare_plan = req.path() == "/_deploy/prepare";
        return deploy_init_endpoint(
            req,
            env,
            environment,
            &queries,
            request_config,
            prepare_plan,
            register_blocks,
            register_post_build,
        )
        .await;
    }

    // Lock down `*.workers.dev` preview hosts. Version preview URLs
    // (`https://<hash>-<worker>.<subdomain>.workers.dev`) expose the full app
    // on a public workers.dev host during the atomic deploy window; this guard
    // returns a plain 404 there so only the deploy endpoint is reachable.
    // Runs AFTER the `/_deploy/init` intercept above, so `impresspress deploy`'s
    // init gate still works on the preview host — that's the whole deploy flow.
    //
    // Consumers that legitimately serve on workers.dev — no custom domain —
    // opt in with the `IMPRESSPRESS_ALLOW_WORKERS_DEV=1` worker var. The opt-in
    // admits the worker's canonical host only; a *version preview* host stays
    // locked regardless, because `impresspress deploy` proves an unpromoted
    // candidate is unreachable before it promotes anything
    // (`smoke_preview_lockdown`), and an opt-in that opened previews would
    // make the atomic deploy impossible for exactly the consumers it exists
    // for. `wrangler dev` (localhost) is unaffected.
    if host_is_workers_dev(&req)?
        && !deploy_token_authorized(&req, &environment)
        && (!environment.allows_workers_dev() || host_is_version_preview(&req, &environment)?)
    {
        return worker::Response::error("not found", 404);
    }

    // Serve `/b/static/` assets straight from the R2 bucket binding,
    // bypassing Wafer block dispatch entirely — same shape as the
    // `/_deploy/*` special cases above, because this is the one place with a
    // live `env` and its R2 binding; `impresspress-core`'s `Context` has no
    // storage capability (see `static_asset_target`'s doc and this crate's
    // `embed-assets` feature comment in `Cargo.toml`). Runs AFTER the
    // workers.dev lockdown so a preview host keeps hiding CSS/JS along with
    // everything else, matching how the embedded path behaves on that host.
    #[cfg(not(feature = "embed-assets"))]
    if let Some((key, content_type)) = static_asset_target(&req.path()) {
        return serve_static_asset_from_r2(&env, key, content_type).await;
    }

    // Isolate-scoped init — no-op after the first call; consumers with an
    // #[event(start)] handler have already run it.
    init_isolate();
    // The request's own services leave the statements its audit row needs
    // unused (none when the policy writes no rows); the row is written below
    // with a handle that may use them, and then they go to the request's
    // deferred tasks (see `impresspress_core::after_response`).
    queries.reserve(after_response::audit_row_reservation(
        environment.config_value(impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY),
    ));
    let after = AfterResponse::new();
    let deferred: std::rc::Rc<std::cell::RefCell<Vec<BoxedTask>>> = std::rc::Rc::default();
    let collect = std::rc::Rc::clone(&deferred);
    let result = run_inner(
        req,
        &env,
        &environment,
        &queries,
        &request_config,
        register_blocks,
        register_post_build,
        &after,
        &move |task| collect.borrow_mut().push(task),
    )
    .await;

    // This request's post-response work, in one `wait_until`: its audit row
    // first, through a D1 handle derived from THIS request's Env and allowed
    // the reserved statements, then its deferred tasks on whatever the row
    // left.
    let audit_write =
        after.take_audit_row().and_then(|row| {
            match make_d1_database_service_concrete(
                &env,
                &environment,
                runner::D1_BINDING,
                &queries.for_reserved_work(),
            ) {
                Ok(db) => Some(Box::pin(async move {
                    if let Err(failure) = after_response::persist_audit_row(db.as_ref(), row).await
                    {
                        log_audit_row_not_written(failure.table, &failure.error);
                    }
                }) as BoxedTask),
                Err(e) => {
                    log_audit_row_not_written(row.table, &e.to_string());
                    None
                }
            }
        });
    let tasks = std::mem::take(&mut *deferred.borrow_mut());
    if audit_write.is_some() || !tasks.is_empty() {
        ctx.wait_until(after_response_work(audit_write, queries.clone(), tasks));
    }

    retry_pending_config_version(&env, |task| ctx.wait_until(task));

    match result {
        Ok(response) => Ok(response),
        Err(e)
            if e.downcast_ref::<runtime_cache::RuntimeBuildBusy>()
                .is_some() =>
        {
            worker::console_log!("impresspress-cloudflare runtime build busy; retrying is safe");
            let mut response = worker::Response::error("service temporarily unavailable", 503)?;
            response.headers_mut().set("Retry-After", "1")?;
            Ok(response)
        }
        Err(e) => {
            // Never return the real cause to the client — `e` can carry
            // SQL, binding, schema, or other configuration detail. Log it
            // (with a correlation id) and return an opaque 500; an operator
            // greps the isolate's console log for the same id to find the
            // real error.
            let correlation_id = uuid::Uuid::new_v4();
            worker::console_log!("impresspress-cloudflare run error [{correlation_id}]: {e}");
            worker::Response::error(
                format!("internal server error (reference: {correlation_id})"),
                500,
            )
        }
    }
}

/// A request's post-response work in the order its budget needs: the audit
/// row (if any) from the reserved statements, then the reservation released,
/// then the deferred tasks together. Run as tasks alongside the row, a task
/// would count the row's statements against a limit still lowered by the
/// reservation, and be refused while the invocation had room.
async fn after_response_work(
    audit_write: Option<BoxedTask>,
    queries: database::D1QueryCount,
    tasks: Vec<BoxedTask>,
) {
    if let Some(write) = audit_write {
        write.await;
    }
    queries.release_reservation();
    futures::future::join_all(tasks).await;
}

/// Log an audit row that could not be written, as a structured metric line
/// (not a Server-Timing header: the write runs in `ctx.wait_until`, after the
/// response has been sent). See `impresspress_core::metrics`'s module doc.
fn log_audit_row_not_written(table: &str, error: &str) {
    worker::console_log!(
        "{}",
        impresspress_core::metrics::metric_line(
            "audit_log_persist_failed",
            &[("table", table), ("error", error)],
        )
    );
}

/// Worker `scheduled` entry shim: the cron counterpart of [`run`].
///
/// Consumers call this from their `#[event(scheduled)]` handler, passing the
/// **same two registration hooks they pass to [`run`]**. That is not a style
/// preference: both entry points share one per-isolate runtime cache, so a
/// `scheduled` handler that registered a different block set would build a
/// runtime under this deployment's own identity and publish it for the next
/// fetch to serve.
///
/// One thing runs here — the auth retention sweep
/// (`impresspress_core::blocks::auth_ui::MAINTENANCE_MESSAGE_KIND`), a message
/// kind auth-ui already routes, so this adds an entry point rather than a code
/// path. Its counts are logged. Nothing else runs on the schedule.
///
/// The schedule itself is **opt-in and empty by default**: exporting this
/// entry point is one of the two steps, and setting `[cloudflare].crons` in
/// `impresspress.toml` is the other (`impresspress deploy`'s `DEFAULT_CRONS`
/// is `&[]`; `examples/webmcp-demo` does both). A deployment that sets neither,
/// or sets `crons = []`, never reaches this function.
pub async fn run_scheduled<F, G>(
    event: worker::ScheduledEvent,
    env: worker::Env,
    ctx: worker::ScheduleContext,
    register_blocks: F,
    register_post_build: G,
) where
    F: FnOnce(ImpresspressBuilder) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    run_scheduled_with_config(
        event,
        env,
        ctx,
        HashMap::new(),
        register_blocks,
        register_post_build,
    )
    .await
}

/// Variant of [`run_scheduled`] with explicit request-current Worker
/// configuration, for consumers that use [`run_with_config`] on the fetch side.
///
/// Pass the same map. `request_config` *is* part of runtime identity
/// (`CfEnvironment::identity`), which is what makes this survivable rather
/// than silent: the cache compares identities before serving, so a cron
/// passing an empty map into an isolate whose fetches pass a populated one
/// cannot hand the wrong runtime to a request. What it does instead is make
/// the two identities disagree permanently, so the cron rebuilds on every
/// invocation and the next fetch rebuilds again — a rebuild storm, each one
/// paying the full D1 read set, on an isolate that was already warm.
///
/// Contrast the registration hooks above, which are *not* part of the
/// identity: differing there really does publish the wrong runtime.
pub async fn run_scheduled_with_config<F, G>(
    event: worker::ScheduledEvent,
    env: worker::Env,
    ctx: worker::ScheduleContext,
    request_config: HashMap<String, String>,
    register_blocks: F,
    register_post_build: G,
) where
    F: FnOnce(ImpresspressBuilder) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    let environment = CfEnvironment::capture(&env);
    // This invocation's D1 statement count, for the reason `run_with_config`
    // gives.
    let queries = database::D1QueryCount::new();
    // Same reason as `run_with_config`: `std::env` is stubbed empty on wasm32,
    // so the Worker var is the only channel carrying an asset base URL. The
    // sweep renders no page, but the runtime this builds is published into the
    // isolate cache for the next fetch, which does.
    impresspress_core::ui::assets::set_base_url_override(environment.asset_base_url());
    init_isolate();

    // WHY THERE IS NO `after_response` SCOPE HERE, unlike `run`.
    //
    // Only the request pipeline writes audit rows and only the auth-ui mail
    // handlers defer, and the one thing that runs on this path
    // (`auth.maintenance`, dispatched by `run_block`) reaches neither. With no
    // scope installed, an audit row would be inserted inline, and a deferred
    // task would be dropped at once with an error line (`deferred::defer`) —
    // nothing is left in a queue nothing runs. A change that makes this path
    // defer must install a scope and hand its tasks to `ctx.wait_until`, as
    // `dispatch` does.
    let cron = event.cron();
    match run_scheduled_inner(
        &env,
        &environment,
        &queries,
        &request_config,
        register_blocks,
        register_post_build,
    )
    .await
    {
        Ok(sweep) => worker::console_log!(
            "{}",
            impresspress_core::metrics::metric_line(
                "auth_maintenance_sweep",
                &[
                    ("cron", &cron),
                    ("complete", &sweep.complete.to_string()),
                    ("sessions_deleted", &sweep.sessions_deleted.to_string()),
                    ("tokens_deleted", &sweep.tokens_deleted.to_string()),
                    (
                        "jwt_blocklist_deleted",
                        &sweep.jwt_blocklist_deleted.to_string()
                    ),
                    ("oauth_pkce_deleted", &sweep.oauth_pkce_deleted.to_string()),
                    ("errors", &sweep.errors.join(",")),
                ],
            )
        ),
        // A cron has no client to answer, so a failure that a fetch would turn
        // into a 500 can only be logged. It is logged in full rather than
        // behind a correlation id: nobody is receiving this text but the
        // operator reading the isolate's own log.
        Err(error) => worker::console_log!(
            "{}",
            impresspress_core::metrics::metric_line(
                "auth_maintenance_sweep_failed",
                &[("cron", &cron), ("error", &error.to_string())],
            )
        ),
    }

    retry_pending_config_version(&env, |task| ctx.wait_until(task));
}

/// Hydrate a runtime and run one retention pass on it.
async fn run_scheduled_inner<F, G>(
    env: &worker::Env,
    environment: &CfEnvironment,
    queries: &crate::database::D1QueryCount,
    request_config: &HashMap<String, String>,
    register_blocks: F,
    register_post_build: G,
) -> Result<impresspress_core::blocks::auth::maintenance::SweepResult, Box<dyn std::error::Error>>
where
    F: FnOnce(ImpresspressBuilder) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    // WHICH BOOT FUNNEL A CRON TAKES, and why it is this one.
    //
    // `get_or_build` is the request path's entry, and it picks between two of
    // the three funnels `runtime_build` declares: `boot_prepared_runtime` when
    // this Worker version carries a packaged plan, `boot_dynamic_request_
    // runtime` otherwise. Going through it puts a scheduled invocation on
    // whichever of those two this deployment's fetches already take. That is
    // the decision, made here, not a side effect of reusing a convenient
    // function — and it is also why the build is not wasted: a cold cron warms
    // the very cache the next fetch reads.
    //
    // The third funnel, `boot_deploy_runtime`, is the one a cron must NOT
    // take, and it is the one that looks affordable — no client is waiting, so
    // `InitPolicy::Reported` and the seeding hook seem free. They are not.
    // Seeding is a deploy-time mutation performed with an operator present. On
    // a schedule it would run migrations without consent on any database that
    // has not seen `/_deploy/init`, bump the KV config generation whenever it
    // did seed and so force a full dynamic rebuild across the fleet — daily —
    // and race a UNIQUE insert against whatever isolates are serving. Those
    // are exactly the three failure modes amended ruling 5.5 keeps off the
    // request path, and a cron has all three plus nobody watching.
    // `InitPolicy::Reported` is wrong for the same reason: nothing reads the
    // report, so a "reported" failure would publish a half-initialized runtime
    // into the isolate cache for the next fetch to serve. `Strict`, which is
    // what the request funnels use, refuses instead.
    //
    // A cron is a serving-time invocation that happens to have no client. It
    // belongs on the serving funnels.
    let (rt, _cache_outcome) = runtime_cache::get_or_build(
        env,
        environment,
        queries,
        request_config,
        register_blocks,
        register_post_build,
    )
    .await?;
    let services = warm_request_services(
        env,
        environment,
        queries,
        rt.wafer.config_snapshot(),
        request_config,
    )?;

    request_services::scope(services, async {
        let output = rt
            .wafer
            .run_block(
                impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID,
                impresspress_core::blocks::auth_ui::maintenance_message(),
                wafer_run::InputStream::empty(),
            )
            .await;
        // Decoding stays inside the poll scope, for the reason `dispatch`
        // states on the fetch path: an output stream may be consumed lazily
        // and reach back into a request service. Today the sweep's answer is a
        // fully materialised buffer, so this is not a live bug — but decoding
        // outside would turn "a service was touched after the scope closed"
        // into "the sweep returned a malformed answer", which is a diagnosis
        // pointing at the wrong half of the system.
        Ok(impresspress_core::blocks::auth_ui::sweep_result_from_output(output).await?)
    })
    .await
}

/// Re-attempt a config-version KV PUT that failed earlier in this invocation
/// (most likely KV's 1-write/sec/key throttle), through this invocation's own
/// `Env`.
///
/// Both entry points need it and neither can hold the other's context type:
/// `fetch` has a `worker::Context`, `scheduled` a `worker::ScheduleContext`,
/// and the two `wait_until` methods are inherent, not a shared trait. Hence
/// the closure — the alternative was a second verbatim copy of the retry in
/// [`run_scheduled_with_config`], which is exactly the shape that lets one
/// path silently stop retrying.
fn retry_pending_config_version(env: &worker::Env, defer: impl FnOnce(BoxedTask)) {
    let Some(stamp) = kv_cached_db::take_pending_version_retry() else {
        return;
    };
    match make_kv_backend(env, runner::KV_BINDING) {
        Ok(kv) => defer(Box::pin(async move {
            worker::Delay::from(std::time::Duration::from_millis(1_100)).await;
            if let Err(e) = kv
                .put(impresspress_core::cache_key::CONFIG_VERSION_KEY, &stamp)
                .await
            {
                // `e` here is already a `String` (KvBackend::put's error
                // type) — no `.to_string()` clone needed.
                worker::console_log!(
                    "{}",
                    impresspress_core::metrics::metric_line(
                        "config_version_retry_failed",
                        &[("error", &e)],
                    )
                );
            }
        })),
        Err(e) => worker::console_log!(
            "{}",
            impresspress_core::metrics::metric_line(
                "config_version_retry_failed",
                &[("error", &e.to_string())],
            )
        ),
    }
}

/// Work handed to a `wait_until`. Boxed because the two entry points' contexts
/// take it by different inherent methods and it has to cross a closure.
type BoxedTask = std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>;

/// Convert a worker request into a WAFER message (preserving the auth header)
/// and dispatch it through the `"site-main"` flow.
///
/// Work the handlers deferred ([`impresspress_core::deferred`]) is handed to
/// `defer` — `ctx.wait_until` — each task wrapped in this request's service
/// scope: the bindings its database, crypto and network calls reach are
/// request-scoped here, and outside a scope they are refused. It is handed
/// over whether or not the dispatch succeeded, so a failed conversion does
/// not drop a mail a handler already queued. A task another interleaved
/// request queued may be drained here instead; it then runs on this
/// request's bindings, which are the same deployment's.
async fn dispatch(
    wafer: &wafer_run::Wafer,
    req: worker::Request,
    services: std::rc::Rc<request_services::RequestServices>,
    after: &std::rc::Rc<AfterResponse>,
    defer: &dyn Fn(BoxedTask),
) -> Result<worker::Response, Box<dyn std::error::Error>> {
    let deferred_services = std::rc::Rc::clone(&services);
    // Both scopes are re-entered on every poll, so a request interleaved with
    // this one neither uses its services nor queues its audit row or
    // deferred tasks into this one's.
    let dispatched = after_response::scope(std::rc::Rc::clone(after), async move {
        // 7. Convert request → message; preserve auth header in meta.
        let auth_header = req.headers().get("authorization")?;
        let (mut msg, input) = convert::worker_request_to_message(&req).await?;
        if let Some(ref auth) = auth_header {
            msg.set_meta("http.header.authorization", auth);
        }

        // 8. Dispatch and convert response. Keeping conversion inside the
        // poll scope also covers lazily consumed service-backed streams.
        let output = wafer.run("site-main", msg, input).await;
        Ok(convert::output_to_response(output).await?)
    });
    let response = request_services::scope(services, dispatched).await;
    // This request's deferred tasks, and only its: they run under its own
    // services and so its own D1 budget.
    for task in after.take_tasks() {
        defer(Box::pin(request_services::scope(
            std::rc::Rc::clone(&deferred_services),
            task,
        )));
    }
    response
}

#[expect(
    clippy::too_many_arguments,
    reason = "the invocation's captured environment and D1 statement count travel as \
              parameters from the Worker entry that owns them"
)]
async fn run_inner<F, G>(
    req: worker::Request,
    env: &worker::Env,
    environment: &CfEnvironment,
    queries: &crate::database::D1QueryCount,
    request_config: &HashMap<String, String>,
    register_blocks: F,
    register_post_build: G,
    after: &std::rc::Rc<AfterResponse>,
    defer: &dyn Fn(BoxedTask),
) -> Result<worker::Response, Box<dyn std::error::Error>>
where
    F: FnOnce(ImpresspressBuilder) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    // Reuse the per-isolate runtime; rebuild only when the KV config-version
    // stamp has moved. No boot funnel here — migrations/seeds run at deploy
    // time via `/_deploy/init`, not on the request path.
    let (rt, cache_outcome) = runtime_cache::get_or_build(
        env,
        environment,
        queries,
        request_config,
        register_blocks,
        register_post_build,
    )
    .await?;
    let services = warm_request_services(
        env,
        environment,
        queries,
        rt.wafer.config_snapshot(),
        request_config,
    )?;
    let mut response = dispatch(&rt.wafer, req, services, after, defer).await?;

    // Cheap observability signal (2026-07-16 audit follow-up): one header
    // assembly from a value already computed by `get_or_build`. Gated to
    // Debug (dev) level — see `resolved_log_level`'s doc — so an
    // unconditional header doesn't disclose per-request cache/rebuild state
    // to anonymous clients on production deployments (which default to
    // Info). A failure to set it never fails the request.
    if resolved_log_level(environment) == impresspress_core::log_level::LogLevel::Debug {
        let server_timing = impresspress_core::metrics::server_timing_header(cache_outcome);
        if let Err(e) = response.headers_mut().set("Server-Timing", &server_timing) {
            worker::console_log!(
                "{}",
                impresspress_core::metrics::metric_line(
                    "server_timing_header_failed",
                    &[("error", &e.to_string())],
                )
            );
        }
    }

    Ok(response)
}
/// Tests for [`static_asset_target`] — the pure decision seam behind the
/// `/b/static/` R2 read-through in `run_with_config`. A worker `fetch`
/// handler is awkward to unit-test, so this covers only the manifest-lookup
/// security boundary; the R2 fetch itself is validated end-to-end by a real
/// Cloudflare deploy (same posture as this crate's other `worker::Env`-driven
/// paths — see `database.rs`'s module note).
#[cfg(all(test, not(feature = "embed-assets")))]
mod static_asset_target_tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    #[wasm_bindgen_test]
    fn static_asset_target_resolves_a_known_asset() {
        let e = impresspress_core::ui::assets::entry("app.css");
        let path = format!(
            "{}{}",
            impresspress_core::routing::STATIC_PREFIX,
            e.filename
        );
        let (key, ct) = static_asset_target(&path).expect("known asset must resolve");
        assert_eq!(
            key, e.filename,
            "R2 key is the flat hashed filename Task 4 uploads"
        );
        assert_eq!(ct, e.content_type);
    }

    #[wasm_bindgen_test]
    fn static_asset_target_rejects_unknown_and_traversal_before_building_a_key() {
        for p in [
            "/b/static/app-deadbeef.css",
            "/b/static/../../etc/passwd",
            "/b/static/",
            "/not-static/app.css",
        ] {
            assert!(static_asset_target(p).is_none(), "must not resolve: {p}");
        }
    }
}

/// The wasm32 half of the middleware-block invariant.
///
/// `impresspress-core`'s `use_static_blocks!` anchor list is the ONE place
/// the six `wafer-run/*` middleware blocks are named. Off wasm32 linkme
/// collects them and `WAFER_STATIC_BLOCKS` is empty; on wasm32 linkme writes
/// into a link section that does not exist, so the by-value list is the only
/// thing that registers them — and it is the half a hand-written second list
/// used to cover, with nothing keeping the two in step.
///
/// That is asserted here rather than beside the list because
/// `impresspress-core` cannot compile test code for wasm32 at all
/// (`--all-targets` pulls its tokio/mio dev-dependencies, which do not build
/// for that target), so a `cfg(target_arch = "wasm32")` assertion written
/// there is compiled by nothing. This crate has an executable wasm lane and a
/// CI job whose path filter covers the manifests that turn these blocks on.
#[cfg(test)]
mod middleware_blocks_tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn a_runtime_built_on_wasm32_carries_every_middleware_block() {
        let mut wafer =
            wafer_run::Wafer::new(std::sync::Arc::new(wafer_run::StaticConfigSource::default()))
                .expect("Wafer::new with no lockfile");

        // Self-guard: on wasm32 linkme collects nothing, so a bare `Wafer` has
        // none of the six. If that ever stopped being true the assertions
        // below would pass without `register_middleware_blocks` doing anything.
        for name in impresspress_core::builder::MIDDLEWARE_BLOCKS {
            assert!(
                !wafer.has_block(name),
                "{name} was already registered before                  `register_middleware_blocks` ran — this test would be vacuous"
            );
        }

        impresspress_core::builder::register_middleware_blocks(&mut wafer)
            .expect("register the middleware blocks");

        for name in impresspress_core::builder::MIDDLEWARE_BLOCKS {
            assert!(
                wafer.has_block(name),
                "{name} is not registered on wasm32 — is its crate still in \
                 `impresspress-core`'s `use_static_blocks!` anchor list, and \
                 does it still invoke `register_static_block!` under that name?"
            );
        }
    }
}

/// The deferred-work hand-off on the request path: work a handler queues
/// during a dispatch must reach `defer` (production's `ctx.wait_until`) and run
/// inside that request's service scope. Driven through the real `dispatch`
/// with a real `Wafer` running a `site-main` flow whose one block defers.
#[cfg(test)]
mod deferred_drain_tests {
    use std::{
        cell::RefCell,
        rc::Rc,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use wafer_run::{Block, BlockInfo, InputStream, Message, OutputStream};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    /// Answers every request, after queueing one task that records the
    /// marker of the service bundle it ran under.
    struct Defers(Arc<AtomicUsize>);

    #[wafer_block::wafer_async_trait]
    impl Block for Defers {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/defers", "0.0.1", "http-handler@v1", "defers one task")
        }
        async fn handle(
            &self,
            _ctx: &dyn wafer_run::context::Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            let seen = Arc::clone(&self.0);
            impresspress_core::deferred::defer(async move {
                seen.store(
                    request_services::current_marker().unwrap_or(0),
                    Ordering::SeqCst,
                );
            });
            impresspress_core::http::ok_json(&serde_json::json!({ "ok": true }))
        }
    }

    #[wasm_bindgen_test]
    async fn a_task_deferred_during_a_request_runs_in_that_requests_scope() {
        init_isolate();

        let seen = Arc::new(AtomicUsize::new(0));
        let mut wafer = wafer_run::Wafer::new(Arc::new(wafer_run::StaticConfigSource::default()))
            .expect("Wafer::new");
        wafer
            .register_block("test/defers", Arc::new(Defers(Arc::clone(&seen))))
            .expect("register");
        wafer
            .add_flow_json(
                r#"{"id":"site-main","name":"t","version":"0.1.0","description":"t",
                    "steps":[{"id":"defers","block":"test/defers"}],
                    "config":{"on_error":"stop"}}"#,
            )
            .expect("flow");
        wafer.seal().await.expect("seal");

        let handed: Rc<RefCell<Vec<BoxedTask>>> = Rc::default();
        let sink = Rc::clone(&handed);
        let req = worker::Request::new("https://example.test/anything", worker::Method::Get)
            .expect("request");
        let after = AfterResponse::new();
        let response = dispatch(
            &wafer,
            req,
            request_services::RequestServices::marker(7),
            &after,
            &move |task| sink.borrow_mut().push(task),
        )
        .await
        .expect("dispatch");
        assert_eq!(response.status_code(), 200);

        assert_eq!(
            seen.load(Ordering::SeqCst),
            0,
            "the task waits for the response"
        );
        let tasks = std::mem::take(&mut *handed.borrow_mut());
        assert_eq!(tasks.len(), 1, "the queued task was handed to wait_until");
        for task in tasks {
            task.await;
        }
        assert_eq!(
            seen.load(Ordering::SeqCst),
            7,
            "it ran, inside the dispatching request's service scope"
        );
        assert!(
            after.take_tasks().is_empty(),
            "the task was handed on, not left behind"
        );
    }

    /// A request's post-response work runs its audit row to completion,
    /// then releases the reservation, then runs its deferred tasks — so a
    /// task never meets a limit still lowered for a row already written.
    #[wasm_bindgen_test]
    async fn deferred_tasks_run_after_the_audit_row_with_the_reservation_released() {
        let queries = database::D1QueryCount::new();
        queries.reserve(impresspress_core::after_response::AUDIT_ROW_STATEMENTS);
        let order: Rc<RefCell<Vec<String>>> = Rc::default();

        let (log, count) = (Rc::clone(&order), queries.clone());
        let audit: BoxedTask = Box::pin(async move {
            yield_once().await;
            log.borrow_mut()
                .push(format!("row, reserved {}", count.reserved_for_test()));
        });
        let task = |name: &'static str| {
            let (log, count) = (Rc::clone(&order), queries.clone());
            Box::pin(async move {
                log.borrow_mut()
                    .push(format!("{name}, reserved {}", count.reserved_for_test()));
            }) as BoxedTask
        };
        after_response_work(Some(audit), queries.clone(), vec![task("a"), task("b")]).await;

        assert_eq!(
            *order.borrow(),
            [
                format!(
                    "row, reserved {}",
                    impresspress_core::after_response::AUDIT_ROW_STATEMENTS
                ),
                "a, reserved 0".to_string(),
                "b, reserved 0".to_string(),
            ]
        );
    }

    /// Yield once, so another request's future is polled before this one
    /// continues.
    async fn yield_once() {
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
    }

    /// Yields, defers one task recording the marker of the service bundle it
    /// runs under, yields again, then answers: two of these interleaved have
    /// both deferred before either dispatch finishes.
    struct DefersBetweenYields(Arc<std::sync::Mutex<Vec<usize>>>);

    #[wafer_block::wafer_async_trait]
    impl Block for DefersBetweenYields {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(
                "test/defers",
                "0.0.1",
                "http-handler@v1",
                "defers between yields",
            )
        }
        async fn handle(
            &self,
            _ctx: &dyn wafer_run::context::Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            yield_once().await;
            let seen = Arc::clone(&self.0);
            impresspress_core::deferred::defer(async move {
                seen.lock()
                    .unwrap()
                    .push(request_services::current_marker().unwrap_or(0));
            });
            yield_once().await;
            impresspress_core::http::ok_json(&serde_json::json!({ "ok": true }))
        }
    }

    /// Two requests interleaved in one isolate each hand `wait_until` their
    /// own deferred task, and each task runs under the services — and so the
    /// D1 budget — of the request that deferred it. The request that
    /// finishes first cannot take, run or pay for the other's task: a mail
    /// one request deferred is never left to a stranger's budget.
    #[wasm_bindgen_test]
    async fn interleaved_requests_each_run_only_their_own_deferred_tasks() {
        init_isolate();

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut wafer = wafer_run::Wafer::new(Arc::new(wafer_run::StaticConfigSource::default()))
            .expect("Wafer::new");
        wafer
            .register_block(
                "test/defers",
                Arc::new(DefersBetweenYields(Arc::clone(&seen))),
            )
            .expect("register");
        wafer
            .add_flow_json(
                r#"{"id":"site-main","name":"t","version":"0.1.0","description":"t",
                    "steps":[{"id":"defers","block":"test/defers"}],
                    "config":{"on_error":"stop"}}"#,
            )
            .expect("flow");
        wafer.seal().await.expect("seal");

        let handed_a: Rc<RefCell<Vec<BoxedTask>>> = Rc::default();
        let handed_b: Rc<RefCell<Vec<BoxedTask>>> = Rc::default();
        let (sink_a, sink_b) = (Rc::clone(&handed_a), Rc::clone(&handed_b));
        let request = || {
            worker::Request::new("https://example.test/anything", worker::Method::Get)
                .expect("request")
        };
        let (after_a, after_b) = (AfterResponse::new(), AfterResponse::new());
        let defer_a = move |task| sink_a.borrow_mut().push(task);
        let defer_b = move |task| sink_b.borrow_mut().push(task);
        let (a, b) = futures::join!(
            dispatch(
                &wafer,
                request(),
                request_services::RequestServices::marker(7),
                &after_a,
                &defer_a,
            ),
            dispatch(
                &wafer,
                request(),
                request_services::RequestServices::marker(8),
                &after_b,
                &defer_b,
            ),
        );
        assert_eq!(a.expect("dispatch a").status_code(), 200);
        assert_eq!(b.expect("dispatch b").status_code(), 200);

        let tasks_a = std::mem::take(&mut *handed_a.borrow_mut());
        let tasks_b = std::mem::take(&mut *handed_b.borrow_mut());
        assert_eq!(
            (tasks_a.len(), tasks_b.len()),
            (1, 1),
            "each request hands wait_until its own task, and only its own"
        );
        for task in tasks_a {
            task.await;
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [7],
            "a's task ran under a's services"
        );
        for task in tasks_b {
            task.await;
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [7, 8],
            "b's task ran under b's services"
        );
    }
}
