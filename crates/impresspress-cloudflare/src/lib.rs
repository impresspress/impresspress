//! Cloudflare Workers adapter for impresspress: D1 database service, R2 storage
//! service, wasm-compatible crypto/network services, and worker entry helpers.
//!
//! Consumed by:
//! - `impresspress-cloud`'s `impresspress-worker` (multi-tenant dispatch user worker).
//! - The `impresspress build --target cloudflare` flow (single-worker consumers
//!   like wafer-site).
//!
//! This crate is wasm-only; building for native targets is not supported.

pub mod config_service;
pub mod config_source;
pub mod convert;
pub mod crypto_service;
pub mod database;
pub mod helpers;
pub mod kv_cached_db;
pub mod logger_service;
pub mod network_service;
mod runner;
mod runtime_cache;
pub mod storage;

// ---------------------------------------------------------------------------
// Public `make_*` constructors — mirrors `impresspress-native`'s API surface.
// Consumers construct services through these helpers rather than importing
// internal types directly.
// ---------------------------------------------------------------------------

use std::{collections::HashMap, sync::Arc};

use wafer_core::interfaces::{
    config::service::ConfigService, crypto::service::CryptoService,
    database::service::DatabaseService, logger::service::LoggerService,
    network::service::NetworkService, storage::service::StorageService,
};

/// Construct a D1-backed [`DatabaseService`] from a worker `Env` and the D1
/// binding name.
///
/// The binding name must match a `[[d1_databases]]` entry in the consumer's
/// `wrangler.toml` (e.g. `"DB"`).
pub fn make_d1_database_service(
    env: &worker::Env,
    binding: &str,
) -> Result<Arc<dyn DatabaseService>, worker::Error> {
    Ok(make_d1_database_service_concrete(env, binding)?)
}

/// Concrete-typed variant of [`make_d1_database_service`]. Used internally
/// where a caller needs D1-specific capabilities beyond the
/// `DatabaseService` trait object — e.g. the audit-log batch-insert path in
/// `run()`, which needs D1's native `batch()` API via
/// [`database::D1DatabaseService::create_many`].
fn make_d1_database_service_concrete(
    env: &worker::Env,
    binding: &str,
) -> Result<Arc<database::D1DatabaseService>, worker::Error> {
    let db = env.d1(binding)?;
    Ok(Arc::new(database::D1DatabaseService::new(db)))
}

/// Construct a [`DatabaseService`] backed by D1 with a Cloudflare KV cache
/// layered on top of the per-block read paths (`variables WHERE block=?`
/// and `block_settings WHERE block_name=?`).
///
/// The KV binding name must match a `[[kv_namespaces]]` entry in the
/// consumer's `wrangler.toml` (canonical name: `"CONFIG_CACHE"`).
///
/// Fails fast if the KV binding is missing — silent degradation would
/// mask a config-drift outage.
///
/// Spec: `docs/superpowers/specs/2026-05-22-kv-cached-d1-config-source-design.md`.
pub fn make_kv_cached_database_service(
    env: &worker::Env,
    d1_binding: &str,
    kv_binding: &str,
) -> Result<Arc<dyn DatabaseService>, worker::Error> {
    let (db, _backend, _batch_db) = make_kv_cached_database_service_with_backend(
        env,
        d1_binding,
        kv_binding,
        kv_cached_db::CacheMode::default(),
    )?;
    Ok(db)
}

/// Internals of [`make_kv_cached_database_service`], additionally returning
/// the `KvBackend` handle it constructs — the per-isolate runtime cache
/// (task-7) needs the backend itself (not just the `DatabaseService` it's
/// wrapped into) so it can probe the KV config-version stamp without re-deriving
/// a `KvStore` handle from `env` on every request — and the concrete D1
/// handle underneath the KV-cache wrapper, which the audit-log batch-insert
/// path (`run()`'s `waitUntil` drain) needs for D1's native `batch()` API
/// (`request_logs` is never a KV-cached table, so going around the wrapper
/// for this one write path is equivalent to going through it). The
/// `/_deploy/init` endpoint re-derives its own KV handle via
/// `make_kv_backend` for its post-funnel config-version bump.
/// Return type of [`make_kv_cached_database_service_with_backend`]: the
/// wrapped `DatabaseService`, the raw `KvBackend` it was built from, and the
/// concrete D1 handle underneath it.
type KvCachedDbServiceWithBackend = (
    Arc<dyn DatabaseService>,
    Arc<dyn impresspress_core::kv::KvBackend>,
    Arc<database::D1DatabaseService>,
);

fn make_kv_cached_database_service_with_backend(
    env: &worker::Env,
    d1_binding: &str,
    kv_binding: &str,
    mode: kv_cached_db::CacheMode,
) -> Result<KvCachedDbServiceWithBackend, worker::Error> {
    let d1 = make_d1_database_service_concrete(env, d1_binding)?;
    let inner: Arc<dyn DatabaseService> = d1.clone();
    let backend = make_kv_backend(env, kv_binding)?;
    // `DatabaseService` only requires `MaybeSend + MaybeSync` (real
    // `Send + Sync` on native, a no-op marker on wasm32 — see
    // wafer_block::compat), so this `Arc` doesn't promise cross-thread
    // safety; this crate only ever targets wasm32, which is single-threaded.
    #[allow(clippy::arc_with_non_send_sync)]
    let db = Arc::new(kv_cached_db::KvCachedD1DatabaseService::with_mode(
        inner,
        backend.clone(),
        mode,
    ));
    Ok((db, backend, d1))
}

/// Construct a raw [`KvBackend`](impresspress_core::kv::KvBackend) from a worker
/// `Env` and a KV binding name. Single construction path shared by the
/// KV-cached DB factory above and the per-isolate runtime cache's
/// config-version probe (`runtime_cache::get_or_build`), so both derive the
/// `KvStore` handle the same way.
pub(crate) fn make_kv_backend(
    env: &worker::Env,
    binding: &str,
) -> Result<Arc<dyn impresspress_core::kv::KvBackend>, worker::Error> {
    let kv_store = env.kv(binding)?;
    Ok(Arc::new(kv_cached_db::WorkerKvBackend(kv_store)))
}

/// Construct an R2-backed [`StorageService`] from a worker `Env` and the R2
/// bucket binding name.
///
/// The binding name must match a `[[r2_buckets]]` entry in the consumer's
/// `wrangler.toml` (e.g. `"STORAGE"`).
pub fn make_r2_storage_service(
    env: &worker::Env,
    binding: &str,
) -> Result<Arc<dyn StorageService>, worker::Error> {
    let bucket = env.bucket(binding)?;
    Ok(Arc::new(storage::R2StorageService::new(bucket)))
}

/// Construct a wasm-compatible [`CryptoService`]: the wafer-block-crypto
/// HS256 JWT engine (exp-required, per-block HKDF-derived keys — same
/// policy as native) with Workers-constrained argon2id password hashing.
///
/// `jwt_secret` is the HMAC master secret used to sign and verify JWTs.
/// It must be at least `wafer_block_crypto::primitives::MIN_JWT_SECRET_LEN`
/// bytes; a missing/short secret surfaces as an error on each sign/verify
/// rather than failing worker boot.
pub fn make_jwt_crypto_service(jwt_secret: String) -> Arc<dyn CryptoService> {
    Arc::new(crypto_service::ImpresspressCryptoService::new(jwt_secret))
}

/// Construct a [`NetworkService`] backed by the CF Worker global `fetch` API.
pub fn make_fetch_network_service() -> Arc<dyn NetworkService> {
    Arc::new(network_service::WorkerFetchService)
}

/// Construct a [`LoggerService`] that writes to `worker::console_log`.
pub fn make_console_logger() -> Arc<dyn LoggerService> {
    Arc::new(logger_service::ConsoleLoggerService)
}

/// Construct a [`ConfigService`] from a pre-loaded key/value map.
///
/// In a CF Worker, callers typically load variables from the D1 `variables`
/// table (and merge any protected worker env bindings) before calling this
/// function.  The returned service is read-only; `set()` is a no-op because
/// CF Workers are stateless.
pub fn make_config_service(vars: HashMap<String, String>) -> Arc<dyn ConfigService> {
    Arc::new(config_service::HashMapConfigService::new(vars))
}

thread_local! {
    static ISOLATE_INITIALIZED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// One-time isolate initialization: selects [`RequestLogMode::Queued`]
/// (audit rows drain into `ctx.wait_until` off the response path — see
/// `run`). Consumers should call this from their worker's
/// `#[event(start)]` handler; `run()` also invokes it behind a
/// once-per-isolate guard, so isolates stay correct either way and repeat
/// calls are no-ops.
///
/// [`RequestLogMode::Queued`]: impresspress_core::pipeline::RequestLogMode::Queued
pub fn init_isolate() {
    ISOLATE_INITIALIZED.with(|done| {
        if !done.get() {
            impresspress_core::pipeline::set_request_log_mode(
                impresspress_core::pipeline::RequestLogMode::Queued,
            );
            done.set(true);
        }
    });
}

use impresspress_core::builder::ImpresspressBuilder;

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
    if req.path() == "/_deploy/init" {
        return deploy_init_endpoint(req, env, register_blocks, register_post_build).await;
    }

    // Lock down `*.workers.dev` preview hosts. Version preview URLs
    // (`https://<hash>-<worker>.<subdomain>.workers.dev`) expose the full app
    // on a public workers.dev host during the atomic deploy window; this guard
    // returns a plain 404 there so only the deploy endpoint is reachable.
    // Runs AFTER the `/_deploy/init` intercept above, so `impresspress deploy`'s
    // init gate still works on the preview host — that's the whole deploy flow.
    // Consumers that legitimately serve on workers.dev opt out with the
    // `IMPRESSPRESS_ALLOW_WORKERS_DEV=1` worker var. `wrangler dev` (localhost) is
    // unaffected.
    if host_is_workers_dev(&req)?
        && env
            .var(ALLOW_WORKERS_DEV_KEY)
            .ok()
            .map(|v| v.to_string())
            .as_deref()
            != Some("1")
    {
        return worker::Response::error("not found", 404);
    }

    // Isolate-scoped init (request-log mode) — no-op after the first call;
    // consumers with an #[event(start)] handler have already run it.
    init_isolate();
    let result = run_inner(req, env, register_blocks, register_post_build).await;

    if let Some(rt) = runtime_cache::peek() {
        // Persist any audit rows queued during this dispatch off the
        // response path. Rows are self-contained data; attaching them to
        // *this* request's waitUntil is correct even if they were queued by
        // an interleaved one. Batched via D1's native `batch()` API
        // (`create_many`) — one D1 round trip per table instead of one
        // `create()` (one prepare+run) per row — and any persistence
        // failure is logged rather than silently dropped.
        let rows = impresspress_core::pipeline::drain_queued_request_logs();
        if !rows.is_empty() {
            let batch_db = rt.batch_db.clone();
            ctx.wait_until(async move {
                let mut by_table: std::collections::HashMap<
                    &'static str,
                    Vec<std::collections::HashMap<String, serde_json::Value>>,
                > = std::collections::HashMap::new();
                for row in rows {
                    by_table.entry(row.table).or_default().push(row.data);
                }
                for (table, rows) in by_table {
                    let n = rows.len();
                    if let Err(e) = batch_db.create_many(table, rows).await {
                        worker::console_log!(
                            "audit-log batch persistence failed ({n} rows into {table}): {e}"
                        );
                    }
                }
            });
        }

        // A config-version KV PUT failed earlier in this dispatch (most
        // likely KV's 1-write/sec/key throttle) and was queued for a
        // delayed retry instead of retrying immediately into the same
        // throttle window — see kv_cached_db::bump_config_version. Retry it
        // off the response path, after a delay comfortably past that
        // window.
        if let Some(stamp) = kv_cached_db::take_pending_version_retry() {
            let kv = rt.kv.clone();
            ctx.wait_until(async move {
                worker::Delay::from(std::time::Duration::from_millis(1_100)).await;
                if let Err(e) = kv
                    .put(impresspress_core::cache_key::CONFIG_VERSION_KEY, &stamp)
                    .await
                {
                    worker::console_log!("delayed config-version retry failed: {e}");
                }
            });
        }
    }

    match result {
        Ok(response) => Ok(response),
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

/// Deploy-time init: runs the full migrate+seed funnel once, invoked by
/// `impresspress deploy` against the freshly-uploaded version (pre-promote).
/// Auth: sha256-compare of `X-Deploy-Token` against the
/// [`DEPLOY_TOKEN_KEY`](impresspress_core::config_vars::DEPLOY_TOKEN_KEY)
/// wrangler secret (hash-then-compare sidesteps timing on raw bytes).
async fn deploy_init_endpoint<F, G>(
    req: worker::Request,
    env: worker::Env,
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
    if req.method() != worker::Method::Post {
        return worker::Response::error("method not allowed", 405);
    }
    let Ok(secret) = env.secret(impresspress_core::config_vars::DEPLOY_TOKEN_KEY) else {
        // Secret unset ⇒ endpoint disabled entirely.
        return worker::Response::error("not found", 404);
    };
    let presented = req.headers().get("x-deploy-token")?.unwrap_or_default();
    // Hash both sides (sidesteps timing on the raw secret length/content),
    // then compare the fixed-size digest *bytes* in constant time — hex
    // strings compared with `!=` short-circuit on the first differing
    // character, which leaks a per-character timing oracle on the digest
    // (and, transitively, on the secret) despite the hash step.
    let presented_digest = wafer_run::sha256(presented.as_bytes());
    let expected_digest = wafer_run::sha256(secret.to_string().as_bytes());
    if presented.is_empty()
        || !wafer_block_crypto::primitives::constant_time_eq(&presented_digest, &expected_digest)
    {
        return worker::Response::error("unauthorized", 401);
    }

    // Deploy-time JWT guard: fail fast with an actionable error when the
    // JWT secret is missing or too short, rather than letting the funnel
    // run and every downstream auth op start failing. Read the secret the
    // same way `build_runtime` does (`env.secret(JWT_SECRET_KEY)`, empty
    // default on error).
    //
    // This is deliberately narrower than the request path: `crypto_service`'s
    // `jwt()` surfaces a missing/short secret per-operation instead of at
    // worker boot, because a broken-auth deployment beats a boot-looping
    // one (see crypto_service.rs:34-38). That adjudication is unchanged
    // here — this guard only runs inside the operator-driven `/_deploy/init`
    // funnel (a production deploy or `impresspress serve --target cloudflare`),
    // where the operator is watching and fail-fast is the native-parity
    // point; it never runs on the request path.
    let jwt_secret_len = env
        .secret(impresspress_core::blocks::auth::JWT_SECRET_KEY)
        .map(|s| s.to_string().len())
        .unwrap_or(0);
    if jwt_secret_len < wafer_block_crypto::primitives::MIN_JWT_SECRET_LEN {
        return worker::Response::error(
            format!(
                "{} is missing or too short ({jwt_secret_len} bytes, need at least {}); \
                 set it with: wrangler secret put {}",
                impresspress_core::blocks::auth::JWT_SECRET_KEY,
                wafer_block_crypto::primitives::MIN_JWT_SECRET_LEN,
                impresspress_core::blocks::auth::JWT_SECRET_KEY,
            ),
            500,
        );
    }

    // Fresh runtime (never the request cache) with run_migrations forced on,
    // so slot-cached pre-migration outcomes can't leak into the funnel.
    let out = async {
        let mut built = build_runtime(
            &env,
            register_blocks,
            register_post_build,
            true,
            kv_cached_db::CacheMode {
                read_through: true,
                bump_on_write: false,
            },
        )
        .await?;
        apply_db_wrap_grants(&mut built).await;
        let report = impresspress_core::deploy_init::deploy_init(
            &mut built.wafer,
            &built.storage_block,
            &CfBootHooks {
                db: built.db.clone(),
            },
        )
        .await
        .map_err(|e| format!("deploy_init: {e}"))?;
        Ok::<_, Box<dyn std::error::Error>>(report)
    }
    .await;

    // One explicit config-version bump for the whole funnel (per-write bumps
    // are suppressed via CacheMode). Runs on failure too: a partial funnel
    // may already have written rows, and a spurious bump only costs each
    // live isolate one rebuild.
    match make_kv_backend(&env, runner::KV_BINDING) {
        Ok(kv) => {
            if let Err(e) = kv_cached_db::force_bump_config_version(kv.as_ref()).await {
                worker::console_log!("post-funnel config-version bump failed: {e}");
            }
        }
        Err(e) => worker::console_log!("post-funnel bump skipped (KV binding): {e}"),
    }

    match out {
        Ok(report) => {
            let status = if report.ok { 200 } else { 500 };
            let body = serde_json::to_string_pretty(&report)
                .unwrap_or_else(|e| format!("{{\"serialize_error\":\"{e}\"}}"));
            Ok(worker::Response::ok(body)?.with_status(status))
        }
        Err(e) => {
            worker::console_log!("deploy_init failed: {e}");
            worker::Response::error(format!("deploy_init: {e}"), 500)
        }
    }
}

/// Everything [`build_runtime`] produces: a built-but-not-sealed-or-booted
/// runtime plus the service handles Tasks 7-8 need (per-isolate build
/// caching, the `/_deploy/init` endpoint) that would otherwise be locked
/// inside its function-local scope.
struct BuiltRuntime {
    wafer: wafer_run::Wafer,
    storage_block: Arc<impresspress_core::blocks::storage::ImpresspressStorageBlock>,
    db: Arc<dyn DatabaseService>,
    /// Concrete D1 handle underneath `db`'s KV-cache wrapper — used by the
    /// audit-log batch-insert path (`run()`'s `waitUntil` drain) for D1's
    /// native `batch()` API. See `database::D1DatabaseService::create_many`.
    batch_db: Arc<database::D1DatabaseService>,
    /// KV backend the DB service is cached through — reused by the per-isolate
    /// runtime cache (`runtime_cache`) to probe the config-version stamp
    /// without re-deriving a second `KvStore` handle from `env`.
    kv: Arc<dyn impresspress_core::kv::KvBackend>,
}

/// Register any admin-created WRAP grants loaded from D1 onto the built
/// runtime. MUST run BEFORE `wafer.seal()` — grants added after seal are
/// ignored. Shared by the request-path per-isolate cache
/// (`runtime_cache::get_or_build`) and the `/_deploy/init` boot funnel.
pub(crate) async fn apply_db_wrap_grants(built: &mut BuiltRuntime) {
    let db_grants = impresspress_core::boot::load_wrap_grants_from_db(&built.db).await;
    if !db_grants.is_empty() {
        built.wafer.add_wrap_grants(db_grants);
    }
}

/// Build (but do not seal or boot) the WAFER runtime for a request: wire the
/// D1/KV/R2/crypto/network/logger services, run the consumer's block
/// registrations, and build + config-snapshot the runtime.
///
/// `force_run_migrations` inserts `IMPRESSPRESS_RUN_MIGRATIONS=1` into the config
/// snapshot — set only by the `/_deploy/init` endpoint to force a migration
/// pass. Migrations on CF run exclusively through that funnel (both a
/// production deploy and local `impresspress serve --target cloudflare` POST it);
/// the normal request path always passes `false` and never migrates.
///
/// `cache_mode` selects KV row-cache read-through and write-bump behavior for
/// this runtime's DB handle.
///
/// Missing-table tolerance is an invariant here: on a first-ever deploy
/// `/_deploy/init` builds this runtime BEFORE any migration has run, so every
/// eager D1 read in this function MUST tolerate a not-yet-created table
/// (`block_settings` → default map; `wrap_grants` → empty vec, applied in the
/// callers via [`apply_db_wrap_grants`]). A non-tolerant eager read would
/// error out and deadlock first deploys before migrations can create the
/// tables.
async fn build_runtime<F, G>(
    env: &worker::Env,
    register_blocks: F,
    register_post_build: G,
    force_run_migrations: bool,
    cache_mode: kv_cached_db::CacheMode,
) -> Result<BuiltRuntime, Box<dyn std::error::Error>>
where
    F: FnOnce(ImpresspressBuilder) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    // 1. Construct D1 service (with KV cache) first — env vars live in D1.
    let (db, kv, batch_db) = make_kv_cached_database_service_with_backend(
        env,
        runner::D1_BINDING,
        runner::KV_BINDING,
        cache_mode,
    )
    .map_err(|e| {
        format!(
            "DB/KV bindings (D1={:?}, KV={:?}): {e}",
            runner::D1_BINDING,
            runner::KV_BINDING
        )
    })?;

    // 2. Load block settings (enablement + migration state) eagerly —
    //    this is the only D1 read at cold start now. The per-block
    //    env-config pre-load is gone; D1ConfigSource resolves declared
    //    config keys lazily on first block init via an indexed lookup
    //    on `variables.block`. block_settings still needs an eager load
    //    because the ImpresspressRouter consumes the enablement map up front
    //    when wiring routes (it can't defer to a per-block init event).
    let block_settings = impresspress_core::features::load_and_seed_block_settings(&db).await;

    // 3. Build the ConfigService map. After dropping the D1 env_vars
    //    pre-load, this map only carries:
    //    - PROTECTED_ENV_KEYS pulled from worker::Env bindings (e.g.
    //      JWT secret managed via `wrangler secret put`). These never
    //      live in D1.
    //    - The synthetic BLOCK_SETTINGS_CONFIG_KEY → JSON entry so
    //      consumer blocks (userportal, migration_helper) can read
    //      block enablement / migration state via `ctx.config_get`
    //      without a separate D1 query per request.
    let mut cfg_svc_map: HashMap<String, String> = HashMap::new();
    let mut overlay: HashMap<String, String> = HashMap::new();
    for key in PROTECTED_ENV_KEYS {
        if let Ok(secret) = env.secret(key) {
            let v = secret.to_string();
            cfg_svc_map.insert((*key).to_string(), v.clone());
            overlay.insert((*key).to_string(), v);
        }
    }
    cfg_svc_map.insert(
        impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY.to_string(),
        block_settings.to_config_json(),
    );
    // Migrations on CF run exclusively through the `/_deploy/init` funnel —
    // a production deploy and local `impresspress serve --target cloudflare`
    // both POST it (see `cli/flows/embed_cloudflare.rs`), which builds this
    // runtime with `force_run_migrations = true`. Request-path builds never
    // migrate; there is no worker env var to honor here (unlike native
    // `server.rs`'s `--run-migrations` flag, which is a real per-boot CLI
    // choice).
    if force_run_migrations {
        cfg_svc_map.insert(
            impresspress_core::migration_helper::RUN_MIGRATIONS_KEY.to_string(),
            "1".to_string(),
        );
    }

    // 4. Construct remaining services.
    let bucket: Arc<dyn StorageService> = make_r2_storage_service(env, runner::R2_BINDING)
        .map_err(|e| format!("R2 binding {:?}: {e}", runner::R2_BINDING))?;
    let jwt_secret = cfg_svc_map
        .get(impresspress_core::blocks::auth::JWT_SECRET_KEY)
        .cloned()
        .unwrap_or_default();
    let crypto = make_jwt_crypto_service(jwt_secret);
    let network = make_fetch_network_service();
    let logger = make_console_logger();
    // Clone the map for the snapshot below before `make_config_service`
    // consumes it — the snapshot and the async ConfigService must carry
    // identical data (see comment at the `set_config_snapshot` call site).
    let snapshot = cfg_svc_map.clone();
    let cfg_svc = make_config_service(cfg_svc_map);

    // 5. ConfigSource: D1-backed lazy per-block fetch. The overlay layers
    //    worker::Env secrets (PROTECTED_ENV_KEYS) on top of D1 rows so
    //    secrets never need to be mirrored into the variables table.
    // `ConfigSource` only requires `MaybeSend + MaybeSync` (real
    // `Send + Sync` on native, a no-op marker on wasm32 — see
    // wafer_block::compat), so this `Arc` doesn't promise cross-thread
    // safety; this crate only ever targets wasm32, which is single-threaded.
    #[allow(clippy::arc_with_non_send_sync)]
    let cfg_source: Arc<dyn wafer_run::ConfigSource> = Arc::new(
        config_source::D1ConfigSource::with_overlay(db.clone(), overlay),
    );
    let builder = ImpresspressBuilder::new()
        .database(db.clone())
        .storage(bucket.clone())
        .config(cfg_svc)
        .crypto(crypto)
        .network(network)
        .logger(logger)
        .block_settings(block_settings)
        .config_source(cfg_source);

    // 5. Consumer registers its blocks.
    let builder = register_blocks(builder)?;

    // 6. Build runtime.
    let (mut wafer, storage_block) = builder.build().map_err(|e| format!("builder.build: {e}"))?;

    // 6a. Wire the env-var snapshot into `RuntimeContext.config` so blocks
    //     can read embedder-provided keys via `ctx.config_get` synchronously
    //     (no D1 round-trip per lookup). This is the missing wiring that
    //     left `migration_helper::apply_if_blessed` reading "{}" for
    //     `BLOCK_SETTINGS_CONFIG_KEY` on every cold isolate — see the
    //     2026-05-14 config-snapshot spec. Same data as `make_config_service`;
    //     the snapshot is the synchronous read path, the config block is
    //     the async write surface.
    wafer.set_config_snapshot(snapshot);

    // 6b. Consumer post-build hook (override flows / configs before start).
    //     Receives the R2-backed StorageService (moved in — the builder above
    //     already holds its own clone) so consumers can register blocks that
    //     need direct un-namespaced bucket access.
    register_post_build(&mut wafer, bucket).map_err(|e| format!("register_post_build: {e}"))?;

    Ok(BuiltRuntime {
        wafer,
        storage_block,
        db,
        batch_db,
        kv,
    })
}

/// Convert a worker request into a WAFER message (preserving the auth header)
/// and dispatch it through the `"site-main"` flow.
async fn dispatch(
    wafer: &wafer_run::Wafer,
    req: worker::Request,
) -> Result<worker::Response, Box<dyn std::error::Error>> {
    // 7. Convert request → message; preserve auth header in meta.
    let auth_header = req.headers().get("authorization")?;
    let (mut msg, input) = convert::worker_request_to_message(&req).await?;
    if let Some(ref auth) = auth_header {
        msg.set_meta("http.header.authorization", auth);
    }

    // 8. Dispatch and convert response.
    let output = wafer.run("site-main", msg, input).await;
    Ok(convert::output_to_response(output).await?)
}

async fn run_inner<F, G>(
    req: worker::Request,
    env: worker::Env,
    register_blocks: F,
    register_post_build: G,
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
    let rt = runtime_cache::get_or_build(&env, register_blocks, register_post_build).await?;
    dispatch(&rt.wafer, req).await
}

/// Worker `Env` bindings that override D1 variables (set via
/// `wrangler secret put`). Most config belongs in D1 so admins can
/// manage it through the dashboard — this list stays short.
const PROTECTED_ENV_KEYS: &[&str] = &[impresspress_core::blocks::auth::JWT_SECRET_KEY];

/// Worker var (`env.var`) that opts a consumer out of the `*.workers.dev`
/// preview-host lockdown in [`run`]. Set to `"1"` to serve the full app on a
/// `workers.dev` host (e.g. consumers with no custom domain).
const ALLOW_WORKERS_DEV_KEY: &str = "IMPRESSPRESS_ALLOW_WORKERS_DEV";

/// True when the request's host is a `*.workers.dev` host (ASCII
/// case-insensitive). Drives the preview-host lockdown in [`run`]. Two
/// distinct failure modes: a malformed request URL fails via `?` and
/// propagates as an error (the caller turns it into a 500); a well-formed
/// URL with no host (`host_str()` returns `None`) fails open to normal
/// handling — a hostless request can't be a public preview URL.
fn host_is_workers_dev(req: &worker::Request) -> worker::Result<bool> {
    Ok(req
        .url()?
        .host_str()
        .map(|h| h.to_ascii_lowercase().ends_with(".workers.dev"))
        .unwrap_or(false))
}

/// [`BootHooks`](impresspress_core::builder::BootHooks) impl for the Cloudflare
/// target. block_settings is loaded eagerly before build (the router needs its
/// enablement map at build time); the only post-admin-init seed step is the
/// shared auto-generated-secret pass, which must run after admin migration 002
/// has added the `variables.block` column.
///
/// Not constructed on the request path — the per-isolate cache seals without a
/// boot funnel. Constructed by `deploy_init_endpoint` (`/_deploy/init`), which
/// runs the full boot (migrations + seeds) on demand at deploy time.
struct CfBootHooks {
    db: Arc<dyn DatabaseService>,
}

#[wafer_block::wafer_async_trait]
impl impresspress_core::builder::BootHooks for CfBootHooks {
    async fn seed_after_admin_init(&self, _wafer: &wafer_run::Wafer) -> Result<(), String> {
        impresspress_core::boot::seed_auto_generated(&self.db).await;
        Ok(())
    }
}
