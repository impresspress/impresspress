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
// Compile-time `DatabaseService` conformance assertions for the D1 and
// KV-cached adapters (wafer-run #319 shared suite). Gated behind the
// off-by-default `conformance-check` feature so the suite never enters the
// production Worker wasm; CI checks it explicitly. See the module doc.
#[cfg(feature = "conformance-check")]
mod conformance;
pub mod convert;
pub mod crypto_service;
pub mod database;
pub mod helpers;
pub mod kv_cached_db;
pub mod logger_service;
pub mod network_service;
mod request_services;
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

/// Resolve a logical release-managed asset to its immutable R2 object key.
///
/// Fetches and digest-verifies the release key inventory from R2 on the
/// isolate's first call (cached thereafter). Returns `Ok(None)` only when no
/// release contract is configured or the key is not an inventory member. A
/// partial, malformed, or digest-mismatched contract fails closed so direct
/// R2 fast paths cannot silently downgrade to mutable logical objects.
pub async fn release_asset_object_key(
    env: &worker::Env,
    r2_binding: &str,
    logical_key: &str,
) -> worker::Result<Option<String>> {
    let Some(identity) =
        request_services::ReleaseAssetIdentity::from_env(env).map_err(worker::Error::RustError)?
    else {
        return Ok(None);
    };
    let storage = make_r2_storage_service(env, r2_binding)?;
    let (keys_folder, keys_name) = identity.keys_location();
    let inventory = impresspress_core::release_inventory::load_release_inventory(
        keys_folder,
        keys_name,
        identity.keys_sha256(),
        storage.as_ref(),
    )
    .await
    .map_err(|error| worker::Error::RustError(error.to_string()))?;
    let release = request_services::LoadedRelease {
        identity,
        inventory,
    };
    Ok(release.physical_object_key(logical_key))
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
///
/// The minimum emitted level is read at construction from the
/// `IMPRESSPRESS_CF_LOG_LEVEL` worker var (set via `wrangler.toml [vars]` or
/// the dashboard) — a runtime knob, unlike the previous `option_env!`
/// compile-time read, so an operator can raise/lower verbosity per
/// deployment without rebuilding. Falls back to the compile-time default
/// (Debug in dev builds, Info in release) when the var is unset or
/// unparseable. Resolved once per logger construction, which happens at
/// most once per per-isolate runtime build
/// (`runtime_cache::get_or_build`) — never on the request hot path.
pub fn make_console_logger(env: &worker::Env) -> Arc<dyn LoggerService> {
    Arc::new(logger_service::ConsoleLoggerService::new(
        cf_log_level_var(env).as_deref(),
    ))
}

/// Raw `IMPRESSPRESS_CF_LOG_LEVEL` worker var value, if set. Shared by
/// [`make_console_logger`] (logger construction) and [`resolved_log_level`]
/// (Server-Timing gating in `run_inner`) so both resolve from the same read
/// instead of two independent env lookups that could disagree.
fn cf_log_level_var(env: &worker::Env) -> Option<String> {
    env.var(CF_LOG_LEVEL_KEY).ok().map(|v| v.to_string())
}

/// Resolve the Cloudflare console logger's minimum level without needing to
/// downcast the type-erased `Arc<dyn LoggerService>` the runtime holds.
///
/// Used by `run_inner` to gate the `Server-Timing` response header: only
/// attached when this resolves to `Debug` (dev). An unconditional header
/// would disclose per-request cache/rebuild state — including the isolate
/// build counter, a signal for when a config bump landed — to every
/// anonymous client, which is a production fingerprinting concern, not a
/// dev debugging aid.
fn resolved_log_level(env: &worker::Env) -> impresspress_core::log_level::LogLevel {
    logger_service::resolve_level(cf_log_level_var(env).as_deref())
}

/// Name emitted by the generated Wrangler `[version_metadata]` binding.
const VERSION_METADATA_BINDING: &str = "CF_VERSION_METADATA";

#[derive(Clone)]
struct PreparedRuntimeIdentity {
    application_id: String,
    application_build_sha256: String,
    dependency_lock: impresspress_core::WaferLockIdentity,
    release_assets: impresspress_core::PreparedReleaseAssets,
}

thread_local! {
    /// Verified immutable structure only. No Env, binding, or request I/O
    /// object enters this isolate-local cache.
    ///
    /// [`IdentityCache`] rather than a `RefCell`: this cell is read by every
    /// request that reaches the prepared path, and it is populated by the
    /// single most expensive synchronous step on a cold prepared request
    /// (pull the plan module out of JS, SHA-256 it, parse it, canonicalize
    /// it, re-hash it). Cloudflare can hard-stop a request anywhere in that
    /// window without running a destructor, and a `RefCell` borrow stranded
    /// that way stays set for the life of the isolate — turning every
    /// subsequent request in it into a `panic` → `abort` → wasm trap taken
    /// inside `poll`, whose response promise is never settled. See
    /// `impresspress_core::isolate_cell`'s module documentation for the full
    /// mechanism, and `runtime_cache`'s `BUILD_LEASE_MS` for the same
    /// premise applied to a `Cell<bool>`.
    static PREPARED_PLAN_CACHE: impresspress_core::IdentityCache<impresspress_core::PreparedRuntimePlan> =
        const { impresspress_core::IdentityCache::new() };
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeReleaseManifest {
    schema_version: u32,
    asset_set_sha256: String,
    immutable_prefix: String,
    files: Vec<RuntimeReleaseAssetEntry>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeReleaseAssetEntry {
    logical_key: String,
    size: u64,
    sha256: String,
    content_type: String,
}

#[derive(serde::Serialize)]
struct RuntimeReleaseManifestIdentity<'a> {
    schema_version: u32,
    files: &'a [RuntimeReleaseAssetEntry],
}

fn prepared_runtime_identity(
    env: &worker::Env,
) -> Result<PreparedRuntimeIdentity, Box<dyn std::error::Error>> {
    let required_var = |name: &str| -> Result<String, Box<dyn std::error::Error>> {
        let value = env
            .var(name)
            .map_err(|e| format!("required prepared-runtime Worker var {name}: {e}"))?
            .to_string();
        if value.trim().is_empty() {
            return Err(format!("required prepared-runtime Worker var {name} is empty").into());
        }
        Ok(value)
    };
    let application_id = required_var(impresspress_core::PREPARED_APPLICATION_ID_VAR)?;
    let application_build_sha256 =
        required_var(impresspress_core::PREPARED_APPLICATION_BUILD_SHA256_VAR)?;
    let dependency_lock = serde_json::from_str(&required_var(
        impresspress_core::PREPARED_WAFER_LOCK_IDENTITY_JSON_VAR,
    )?)?;

    let release_assets = match env.var(request_services::RELEASE_ASSET_ID_VAR) {
        Ok(asset_id) if !asset_id.to_string().is_empty() => {
            let asset_id = asset_id.to_string();
            let asset_set_sha256 = if asset_id.starts_with("sha256:") {
                asset_id
            } else {
                format!("sha256:{asset_id}")
            };
            impresspress_core::PreparedReleaseAssets::present(
                asset_set_sha256,
                required_var(request_services::RELEASE_ASSET_PREFIX_VAR)?,
                required_var(request_services::RELEASE_ASSET_MANIFEST_VAR)?,
                required_var(impresspress_core::RELEASE_ASSET_MANIFEST_SHA256_VAR)?,
                required_var(impresspress_core::RELEASE_ASSET_KEYS_SHA256_VAR)?,
            )?
        }
        _ => impresspress_core::PreparedReleaseAssets::absent(),
    };

    Ok(PreparedRuntimeIdentity {
        application_id,
        application_build_sha256,
        dependency_lock,
        release_assets,
    })
}

/// Read the immutable Text-module payload installed by the final Worker shim.
/// The candidate upload intentionally has no such global and returns `None`.
#[cfg(target_arch = "wasm32")]
pub(crate) fn packaged_prepared_runtime_plan(
    env: &worker::Env,
) -> Result<Option<std::rc::Rc<impresspress_core::PreparedRuntimePlan>>, Box<dyn std::error::Error>>
{
    let value = js_sys::Reflect::get(
        &js_sys::global(),
        &wasm_bindgen::JsValue::from_str("__IMPRESSPRESS_PREPARED_RUNTIME_PLAN"),
    )
    .map_err(|e| format!("read prepared runtime Text module global: {e:?}"))?;
    // Candidate Workers have no Text-module global. Avoid requiring final-only
    // digest vars there, while stable final Workers can check their cheap
    // identity key before allocating the module string.
    if !value.is_string() {
        return Ok(None);
    }
    let expected_module_sha256 = env
        .var(impresspress_core::PREPARED_PLAN_MODULE_SHA256_VAR)
        .map_err(|e| format!("prepared plan module digest Worker var: {e}"))?
        .to_string();
    let expected_plan_hash = env
        .var(impresspress_core::PREPARED_PLAN_HASH_VAR)
        .map_err(|e| format!("prepared plan hash Worker var: {e}"))?
        .to_string();
    let worker_version = env
        .get_binding::<worker::WorkerVersionMetadata>(VERSION_METADATA_BINDING)
        .ok()
        .map(|metadata| metadata.id())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "no-version-metadata".to_string());
    let cache_key = format!("{worker_version}\n{expected_plan_hash}\n{expected_module_sha256}");
    // Decode OUTSIDE the cache's critical section, exactly as
    // `ReleaseAssetIdentity::from_env` does: look up, release, verify, store.
    // The integrity checks below are unchanged and unconditional — a cache
    // hit is only ever a value that already passed them under this same
    // Worker version, plan hash, and module digest.
    let plan = PREPARED_PLAN_CACHE.with(|cache| {
        cache.get_or_try_insert_with(cache_key, || {
            let json = value
                .as_string()
                .filter(|json| !json.trim().is_empty())
                .ok_or_else(|| "prepared runtime Text module is empty".to_string())?;
            impresspress_core::PreparedRuntimePlan::from_packaged_json(
                json.as_bytes(),
                &expected_plan_hash,
                &expected_module_sha256,
            )
            .map_err(|error| error.to_string())
        })
    })?;
    Ok(Some(plan))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn packaged_prepared_runtime_plan(
    _env: &worker::Env,
) -> Result<Option<std::rc::Rc<impresspress_core::PreparedRuntimePlan>>, Box<dyn std::error::Error>>
{
    Ok(None)
}

/// Request-current identity for every environment value captured while
/// constructing an isolate-cached runtime.
///
/// The Worker version ID is the deployed fast path: Cloudflare versions
/// capture bindings, secrets/config, code, and compatibility settings, so no
/// secret reads or hashing are needed on an ordinary warm request. The
/// explicit value hash is only a fallback for local development and
/// hand-written Wrangler configs that have not adopted the metadata binding
/// yet. Raw secret material never leaves this function.
pub(crate) fn runtime_environment_identity(
    env: &worker::Env,
    request_config: &HashMap<String, String>,
) -> String {
    let request_config_hash = config_identity_hash(request_config);
    if let Ok(metadata) = env.get_binding::<worker::WorkerVersionMetadata>(VERSION_METADATA_BINDING)
    {
        let version_id = metadata.id();
        if !version_id.is_empty() {
            return format!("worker-version:{version_id}:request-config:{request_config_hash}");
        }
    }

    let jwt_secret = env
        .secret(impresspress_core::blocks::auth::JWT_SECRET_KEY)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let strict_schema = env
        .var(wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let log_level = cf_log_level_var(env).unwrap_or_default();
    let cors = env
        .var(impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let csp = env
        .var(impresspress_core::config_vars::CSP_DIRECTIVES_KEY)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let release_id = env
        .var(request_services::RELEASE_ASSET_ID_VAR)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let release_prefix = env
        .var(request_services::RELEASE_ASSET_PREFIX_VAR)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let release_manifest = env
        .var(request_services::RELEASE_ASSET_MANIFEST_VAR)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let release_manifest_hash = env
        .var(impresspress_core::RELEASE_ASSET_MANIFEST_SHA256_VAR)
        .map(|value| value.to_string())
        .unwrap_or_default();
    let release_keys_hash = env
        .var(impresspress_core::RELEASE_ASSET_KEYS_SHA256_VAR)
        .map(|value| value.to_string())
        .unwrap_or_default();

    // Length prefixes make the encoding unambiguous even if values contain
    // separators. Hashing also keeps the JWT secret out of ReadyRuntime and
    // diagnostics.
    let components = [
        ("jwt-secret", jwt_secret.as_str()),
        ("strict-schema", strict_schema.as_str()),
        ("log-level", log_level.as_str()),
        ("cors", cors.as_str()),
        ("csp", csp.as_str()),
        ("release-id", release_id.as_str()),
        ("release-prefix", release_prefix.as_str()),
        ("release-manifest", release_manifest.as_str()),
        ("release-manifest-hash", release_manifest_hash.as_str()),
        ("release-keys-hash", release_keys_hash.as_str()),
        ("request-config", request_config_hash.as_str()),
    ];
    let mut encoded = Vec::new();
    for (name, value) in components {
        encoded.extend_from_slice(&(name.len() as u64).to_le_bytes());
        encoded.extend_from_slice(name.as_bytes());
        encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
        encoded.extend_from_slice(value.as_bytes());
    }
    impresspress_core::util::sha256_hex(&encoded)
}

fn config_identity_hash(config: &HashMap<String, String>) -> String {
    let mut entries: Vec<_> = config.iter().collect();
    entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
    let mut encoded = Vec::new();
    for (key, value) in entries {
        encoded.extend_from_slice(&(key.len() as u64).to_le_bytes());
        encoded.extend_from_slice(key.as_bytes());
        encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
        encoded.extend_from_slice(value.as_bytes());
    }
    impresspress_core::util::sha256_hex(&encoded)
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
    // `std::env` is stubbed to always-empty on `wasm32-unknown-unknown`, so
    // `impresspress_core::ui::assets::base_url()` can never observe
    // `IMPRESSPRESS_ASSET_BASE_URL` through it here. `worker::Env::var` is
    // the one channel that does carry a Worker `[vars]` entry, so read it
    // and push it into `base_url()`'s platform override before any code
    // path below can render a page (and therefore call `base_url()`).
    // Idempotent (see `set_base_url_override`'s doc) — safe to call on
    // every request, including the fresh-runtime `/_deploy/*` funnels.
    impresspress_core::ui::assets::set_base_url_override(
        env.var(impresspress_core::ui::assets::ASSET_BASE_URL_VAR)
            .ok()
            .map(|v| v.to_string()),
    );

    if req.path() == "/_deploy/verify" {
        return prepared_verify_endpoint(&req, &env).await;
    }
    if req.path() == "/_deploy/prepared" {
        return prepared_status_endpoint(&req, &env);
    }
    if req.path() == "/_deploy/init" || req.path() == "/_deploy/prepare" {
        let prepare_plan = req.path() == "/_deploy/prepare";
        return deploy_init_endpoint(
            req,
            env,
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
    if host_is_workers_dev(&req)? && !deploy_token_authorized(&req, &env) {
        let allowed = env
            .var(ALLOW_WORKERS_DEV_KEY)
            .ok()
            .map(|v| v.to_string())
            .as_deref()
            == Some("1");
        if !allowed || host_is_version_preview(&req, &env)? {
            return worker::Response::error("not found", 404);
        }
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

    // Isolate-scoped init (request-log mode) — no-op after the first call;
    // consumers with an #[event(start)] handler have already run it.
    init_isolate();
    let result = run_inner(
        req,
        &env,
        &request_config,
        register_blocks,
        register_post_build,
    )
    .await;

    // Persist any audit rows queued during this dispatch off the response
    // path. Derive the D1 handle from THIS request's Env instead of borrowing
    // one retained by the isolate-cached runtime.
    let rows = impresspress_core::pipeline::drain_queued_request_logs();
    if !rows.is_empty() {
        match make_d1_database_service_concrete(&env, runner::D1_BINDING) {
            Ok(batch_db) => {
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
                            // Structured metric line (not a Server-Timing header:
                            // this closure runs in `ctx.wait_until`, after the
                            // response has already been sent). See
                            // `impresspress_core::metrics`'s module doc.
                            let rows_str = n.to_string();
                            let err_str = e.to_string();
                            worker::console_log!(
                                "{}",
                                impresspress_core::metrics::metric_line(
                                    "audit_log_persist_failed",
                                    &[("table", table), ("rows", &rows_str), ("error", &err_str)],
                                )
                            );
                        }
                    }
                });
            }
            Err(e) => worker::console_log!(
                "{}",
                impresspress_core::metrics::metric_line(
                    "audit_log_persist_failed",
                    &[("error", &e.to_string())],
                )
            ),
        }
    }

    // A config-version KV PUT failed earlier in this dispatch (most likely
    // KV's 1-write/sec/key throttle). Retry through this request's Env.
    if let Some(stamp) = kv_cached_db::take_pending_version_retry() {
        match make_kv_backend(&env, runner::KV_BINDING) {
            Ok(kv) => {
                ctx.wait_until(async move {
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
                });
            }
            Err(e) => worker::console_log!(
                "{}",
                impresspress_core::metrics::metric_line(
                    "config_version_retry_failed",
                    &[("error", &e.to_string())],
                )
            ),
        }
    }

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

/// Deploy-time init: runs the full migrate+seed funnel once, invoked by
/// `impresspress deploy` against the freshly-uploaded version (pre-promote).
/// Auth: sha256-compare of `X-Deploy-Token` against the
/// [`DEPLOY_TOKEN_KEY`](impresspress_core::config_vars::DEPLOY_TOKEN_KEY)
/// wrangler secret (hash-then-compare sidesteps timing on raw bytes).
async fn deploy_init_endpoint<F, G>(
    req: worker::Request,
    env: worker::Env,
    mut request_config: HashMap<String, String>,
    prepare_plan: bool,
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
    if env
        .secret(impresspress_core::config_vars::DEPLOY_TOKEN_KEY)
        .is_err()
    {
        // Secret unset ⇒ endpoint disabled entirely.
        return worker::Response::error("not found", 404);
    }
    if !deploy_token_authorized(&req, &env) {
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

    if prepare_plan {
        request_config.insert(
            impresspress_core::deploy_init::PREPARE_RUNTIME_PLAN_KEY.to_string(),
            "1".to_string(),
        );
    }

    // Fresh runtime (never the request cache) with run_migrations forced on,
    // so slot-cached pre-migration outcomes can't leak into the funnel.
    let out = async {
        let mut built = build_runtime(
            &env,
            &request_config,
            None,
            register_blocks,
            register_post_build,
            true,
            kv_cached_db::CacheMode {
                read_through: true,
                bump_on_write: false,
            },
        )
        .await?;
        let services = built.services.clone();
        let report = request_services::scope(services, async {
            apply_db_wrap_grants(&mut built).await;
            impresspress_core::deploy_init::deploy_init(
                &mut built.wafer,
                &built.storage_block,
                &CfBootHooks {
                    db: built.db.clone(),
                    block_settings_handle: built.block_settings_handle.clone(),
                },
            )
            .await
            .map_err(|e| format!("deploy_init: {e}"))
        })
        .await?;
        let plan_draft = if prepare_plan && report.ok {
            let final_settings =
                impresspress_core::features::load_block_settings(&built.db).await?;
            let final_grants = impresspress_core::boot::load_wrap_grants_from_db(&built.db).await;
            built.plan_exporter.publish_block_settings(final_settings)?;
            built.plan_exporter.publish_wrap_grants(&final_grants)?;
            let identity = prepared_runtime_identity(&env)?;
            Some((built.plan_exporter.clone(), identity))
        } else {
            None
        };
        Ok::<_, Box<dyn std::error::Error>>((report, plan_draft))
    }
    .await;

    match out {
        Ok((report, plan_draft)) => {
            // Persist one exact generation after every successful funnel.
            // Plan hashing happens only after this write succeeds; there is
            // deliberately no later bump that could invalidate the plan at
            // the instant it is returned.
            let generation = match make_kv_backend(&env, runner::KV_BINDING) {
                Ok(kv) => match kv_cached_db::force_bump_config_version(kv.as_ref()).await {
                    Ok(generation) => generation,
                    Err(error) => {
                        worker::console_log!("post-funnel config-version bump failed: {error}");
                        return worker::Response::error(
                            "deploy_init: config generation persist failed",
                            500,
                        );
                    }
                },
                Err(error) => {
                    worker::console_log!("post-funnel bump failed (KV binding): {error}");
                    return worker::Response::error(
                        "deploy_init: config generation persist failed",
                        500,
                    );
                }
            };
            let plan = match plan_draft {
                Some((exporter, identity)) => match exporter
                    .prepare_runtime_plan_with_config_generation(
                        identity.application_id,
                        identity.application_build_sha256,
                        generation,
                        identity.dependency_lock,
                        identity.release_assets,
                    ) {
                    Ok(plan) => Some(plan),
                    Err(error) => {
                        worker::console_log!("prepared plan finalization failed: {error}");
                        return worker::Response::error(
                            "deploy_init: prepared plan finalization failed",
                            500,
                        );
                    }
                },
                None => None,
            };
            let status = if report.ok { 200 } else { 500 };
            let body = if let Some(plan) = plan {
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "init_report": report,
                    "plan": plan,
                }))
            } else {
                serde_json::to_string_pretty(&report)
            }
            .unwrap_or_else(|e| format!("{{\"serialize_error\":\"{e}\"}}"));
            Ok(worker::Response::ok(body)?.with_status(status))
        }
        Err(e) => {
            // A failed funnel may have committed a prefix of its mutations.
            // Invalidate every older plan even though no replacement plan is
            // emitted. This is the sole bump on this failure path.
            match make_kv_backend(&env, runner::KV_BINDING) {
                Ok(kv) => {
                    if let Err(error) = kv_cached_db::force_bump_config_version(kv.as_ref()).await {
                        worker::console_log!("failed-funnel config-version bump failed: {error}");
                    }
                }
                Err(error) => {
                    worker::console_log!("failed-funnel bump skipped (KV binding): {error}")
                }
            }
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
    /// Concrete request-current services used while building/sealing this
    /// disposable runtime. The cached [`runtime_cache::ReadyRuntime`] never
    /// copies this field; it retains only the Wafer whose service blocks are
    /// stateless forwarding proxies.
    services: std::rc::Rc<request_services::RequestServices>,
    plan_exporter: impresspress_core::builder::PreparedPlanExporter,
    /// Same settings snapshot the router owns. Deploy init updates it after
    /// admin migration + structural seeding so the disposable candidate
    /// runtime validates the exact state ordinary cold builds will read.
    block_settings_handle: Arc<std::sync::RwLock<impresspress_core::features::BlockSettings>>,
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
    request_config: &HashMap<String, String>,
    prepared_plan: Option<&impresspress_core::PreparedRuntimePlan>,
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
    let (db, _kv, _batch_db) = make_kv_cached_database_service_with_backend(
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
    // A read error here is a genuine operational failure (D1 outage,
    // corruption) — never the missing-table cold-start case, which
    // `DatabaseService::list` already tolerates internally by returning an
    // empty result. Propagate rather than fabricate "every block enabled":
    // the runtime build fails, so no requests are served with a fabricated
    // all-enabled snapshot.
    let block_settings = if let Some(plan) = prepared_plan {
        impresspress_core::features::BlockSettings::from_blocks(
            plan.structure
                .block_settings
                .iter()
                .map(|(name, state)| (name.clone(), state.clone()))
                .collect(),
        )
    } else {
        impresspress_core::features::load_block_settings(&db).await?
    };

    // 3. Build the ConfigService map. After dropping the D1 env_vars
    //    pre-load, this map only carries:
    //    - PROTECTED_ENV_KEYS pulled from worker::Env bindings (e.g.
    //      JWT secret managed via `wrangler secret put`). These never
    //      live in D1.
    //    - builder-time shared Worker vars such as CSP/CORS additions. The
    //      middleware flow is constructed before lazy block config exists,
    //      so these must be present in ConfigService before `builder.build()`.
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
    for key in BUILDER_WORKER_VAR_KEYS {
        if let Ok(var) = env.var(key) {
            cfg_svc_map.insert((*key).to_string(), var.to_string());
        }
    }
    // STRICT_SCHEMA (`WAFER_RUN__DATABASE__STRICT_SCHEMA`): thread the worker
    // var (wrangler.toml `[vars]`) into the config map so the shared
    // `wafer-run/database` block's Init lifecycle resolves it via
    // `ctx.config_get` — the sync snapshot surface — and calls
    // `set_strict_schema` on the DB service before it serves any query. Native
    // env-filters *every* declared config key into its snapshot; CF's snapshot
    // is deliberately minimal (secrets + block settings), so this one
    // operational knob is threaded explicitly rather than living in the D1
    // `variables` table (it's a deploy-time decision, not an admin-editable
    // runtime toggle). Absent var ⇒ key absent ⇒ default `false`.
    if let Ok(strict) = env.var(wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY)
    {
        cfg_svc_map.insert(
            wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY.to_string(),
            strict.to_string(),
        );
    }
    // Same reasoning as STRICT_SCHEMA above, and the same explicit thread-
    // through for the same reason: CF's config snapshot is deliberately
    // minimal, so an operational knob that is not a secret and not a block
    // setting has to be carried by hand. Absent var ⇒ key absent ⇒ `all`,
    // which is the behaviour every existing deployment already has.
    if let Ok(request_log) = env.var(impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY) {
        cfg_svc_map.insert(
            impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY.to_string(),
            request_log.to_string(),
        );
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

    // This is the only map retained by the cached Wafer. Explicit application
    // request config is added to the service map below, after this clone, so
    // secret A from one request can never become request B's sync snapshot.
    // JWT remains here as a bounded compatibility exception: CSRF currently
    // reads it synchronously through `Context::config_get`; Worker-version
    // identity forces a rebuild on rotation.
    let mut snapshot = cfg_svc_map.clone();
    retain_prepare_runtime_plan_flag(&mut snapshot, request_config);

    extend_with_request_config(&mut cfg_svc_map, &mut overlay, request_config);

    // 4. Construct remaining services.
    let bucket: Arc<dyn StorageService> = make_r2_storage_service(env, runner::R2_BINDING)
        .map_err(|e| format!("R2 binding {:?}: {e}", runner::R2_BINDING))?;
    let jwt_secret = cfg_svc_map
        .get(impresspress_core::blocks::auth::JWT_SECRET_KEY)
        .cloned()
        .unwrap_or_default();
    let crypto = make_jwt_crypto_service(jwt_secret);
    let network = make_fetch_network_service();
    let logger = make_console_logger(env);
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
    let services = request_services::RequestServices::new(
        env,
        db.clone(),
        bucket,
        cfg_svc,
        crypto,
        network,
        logger,
        cfg_source,
    );

    // The cached runtime receives only stateless forwarding proxies. Builder
    // construction is synchronous but reads ConfigService immediately, so it
    // runs under the same request scope as async dispatch.
    let database_proxy = request_services::database_proxy();
    let storage_proxy = request_services::storage_proxy();
    let config_proxy = request_services::config_proxy();
    let crypto_proxy = request_services::crypto_proxy();
    let network_proxy = request_services::network_proxy();
    let logger_proxy = request_services::logger_proxy();
    let config_source_proxy = request_services::config_source_proxy();
    let prepared_identity = prepared_plan
        .map(|_| prepared_runtime_identity(env))
        .transpose()?;
    let (wafer, storage_block, block_settings_handle, plan_exporter) =
        request_services::scope_sync(
            services.clone(),
            || -> Result<_, Box<dyn std::error::Error>> {
                let builder = ImpresspressBuilder::new()
                    .database(database_proxy)
                    .storage(storage_proxy.clone())
                    .config(config_proxy)
                    .crypto(crypto_proxy)
                    .network(network_proxy)
                    .logger(logger_proxy)
                    .block_settings(block_settings)
                    .config_source(config_source_proxy);

                // 5. Consumer registers its blocks.
                let builder = register_blocks(builder)?;
                let builder = match (prepared_plan, prepared_identity.as_ref()) {
                    (Some(plan), Some(identity)) => builder.apply_prepared_plan(
                        plan,
                        &identity.application_id,
                        &identity.application_build_sha256,
                        &identity.dependency_lock,
                        &identity.release_assets,
                    )?,
                    (None, None) => builder,
                    _ => return Err("prepared runtime identity invariant violated".into()),
                };
                let plan_exporter = builder.prepared_plan_exporter()?;
                let block_settings_handle = builder.block_settings_handle();

                // 6. Build runtime.
                let (mut wafer, storage_block) =
                    builder.build().map_err(|e| format!("builder.build: {e}"))?;

                // 6a. Wire the env-var snapshot into `RuntimeContext.config` so
                // blocks can read embedder-provided keys synchronously. This is
                // immutable configuration data, not a request-derived service
                // handle; Worker-version identity still forces a rebuild when it
                // changes.
                wafer.set_config_snapshot(snapshot);

                // 6b. The consumer receives the scoped storage proxy, never the
                // request's concrete R2 Bucket. A block may safely retain this
                // proxy in the isolate-cached runtime.
                register_post_build(&mut wafer, storage_proxy)
                    .map_err(|e| format!("register_post_build: {e}"))?;
                Ok((wafer, storage_block, block_settings_handle, plan_exporter))
            },
        )?;

    Ok(BuiltRuntime {
        wafer,
        storage_block,
        db,
        services,
        plan_exporter,
        block_settings_handle,
    })
}

/// Convert a worker request into a WAFER message (preserving the auth header)
/// and dispatch it through the `"site-main"` flow.
async fn dispatch(
    wafer: &wafer_run::Wafer,
    req: worker::Request,
    services: std::rc::Rc<request_services::RequestServices>,
) -> Result<worker::Response, Box<dyn std::error::Error>> {
    request_services::scope(services, async move {
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
    })
    .await
}

/// Construct concrete services for one warm request without performing I/O.
/// Binding lookup and service allocation are cheap; D1/KV/R2 operations stay
/// lazy until a block actually calls them.
fn request_services_for_dispatch(
    env: &worker::Env,
    structural_snapshot: &HashMap<String, String>,
    request_config: &HashMap<String, String>,
) -> Result<std::rc::Rc<request_services::RequestServices>, Box<dyn std::error::Error>> {
    let (db, _kv, _batch_db) = make_kv_cached_database_service_with_backend(
        env,
        runner::D1_BINDING,
        runner::KV_BINDING,
        kv_cached_db::CacheMode::default(),
    )?;
    let storage = make_r2_storage_service(env, runner::R2_BINDING)?;

    // Start from the runtime's structural snapshot, then overwrite every
    // framework-owned request-current Env value. The remaining snapshot is
    // pure data; no binding/client is retained in ReadyRuntime.
    let mut config_map = structural_snapshot.clone();
    let mut overlay = HashMap::new();
    extend_with_request_config(&mut config_map, &mut overlay, request_config);
    for key in PROTECTED_ENV_KEYS {
        if let Ok(secret) = env.secret(key) {
            let value = secret.to_string();
            config_map.insert((*key).to_string(), value.clone());
            overlay.insert((*key).to_string(), value);
        } else {
            config_map.remove(*key);
        }
    }
    for key in BUILDER_WORKER_VAR_KEYS {
        match env.var(key) {
            Ok(var) => {
                config_map.insert((*key).to_string(), var.to_string());
            }
            Err(_) => {
                config_map.remove(*key);
            }
        }
    }

    let jwt_secret = config_map
        .get(impresspress_core::blocks::auth::JWT_SECRET_KEY)
        .cloned()
        .unwrap_or_default();
    let config = make_config_service(config_map);
    let crypto = make_jwt_crypto_service(jwt_secret);
    let network = make_fetch_network_service();
    let logger = make_console_logger(env);
    #[allow(clippy::arc_with_non_send_sync)]
    let config_source: Arc<dyn wafer_run::ConfigSource> = Arc::new(
        config_source::D1ConfigSource::with_overlay(db.clone(), overlay),
    );

    Ok(request_services::RequestServices::new(
        env,
        db,
        storage,
        config,
        crypto,
        network,
        logger,
        config_source,
    ))
}

async fn run_inner<F, G>(
    req: worker::Request,
    env: &worker::Env,
    request_config: &HashMap<String, String>,
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
    let (rt, cache_outcome) =
        runtime_cache::get_or_build(env, request_config, register_blocks, register_post_build)
            .await?;
    let services = request_services_for_dispatch(env, rt.wafer.config_snapshot(), request_config)?;
    let mut response = dispatch(&rt.wafer, req, services).await?;

    // Cheap observability signal (2026-07-16 audit follow-up): one header
    // assembly from a value already computed by `get_or_build`. Gated to
    // Debug (dev) level — see `resolved_log_level`'s doc — so an
    // unconditional header doesn't disclose per-request cache/rebuild state
    // to anonymous clients on production deployments (which default to
    // Info). A failure to set it never fails the request.
    if resolved_log_level(env) == impresspress_core::log_level::LogLevel::Debug {
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

/// Worker `Env` bindings that override D1 variables (set via
/// `wrangler secret put`). Most config belongs in D1 so admins can
/// manage it through the dashboard — this list stays short.
const PROTECTED_ENV_KEYS: &[&str] = &[impresspress_core::blocks::auth::JWT_SECRET_KEY];

/// Shared configuration consumed synchronously while the builder constructs
/// middleware. Unlike block-scoped values, these cannot be deferred to the
/// D1-backed `ConfigSource` because the flow already exists by then.
const BUILDER_WORKER_VAR_KEYS: &[&str] = &[
    impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY,
    impresspress_core::config_vars::CSP_DIRECTIVES_KEY,
];

/// Add consumer-declared request config to the async service surfaces only.
/// Callers clone the structural Wafer snapshot before invoking this helper.
fn extend_with_request_config(
    config: &mut HashMap<String, String>,
    overlay: &mut HashMap<String, String>,
    request_config: &HashMap<String, String>,
) {
    for (key, value) in request_config {
        // Framework-owned protected/builder keys come from Env and must not be
        // shadowed by a consumer-provided duplicate.
        if PROTECTED_ENV_KEYS.contains(&key.as_str())
            || BUILDER_WORKER_VAR_KEYS.contains(&key.as_str())
        {
            continue;
        }
        config.insert(key.clone(), value.clone());
        overlay.insert(key.clone(), value.clone());
    }
}

/// Retain the one non-secret request value lifecycle code must read from the
/// synchronous config snapshot while preparing an isolated deploy candidate.
fn retain_prepare_runtime_plan_flag(
    snapshot: &mut HashMap<String, String>,
    request_config: &HashMap<String, String>,
) {
    let key = impresspress_core::deploy_init::PREPARE_RUNTIME_PLAN_KEY;
    if request_config.get(key).map(String::as_str) == Some("1") {
        snapshot.insert(key.to_string(), "1".to_string());
    }
}

/// Worker var (`env.var`) that opts a consumer out of the `*.workers.dev`
/// preview-host lockdown in [`run`]. Set to `"1"` to serve the full app on a
/// `workers.dev` host (e.g. consumers with no custom domain).
const ALLOW_WORKERS_DEV_KEY: &str = "IMPRESSPRESS_ALLOW_WORKERS_DEV";

/// Worker var (`env.var`) that sets the Cloudflare console logger's minimum
/// emitted level at runtime (`debug`/`info`/`warn`/`error`, case-insensitive
/// — see [`impresspress_core::log_level::LogLevel::parse`]). Unset or
/// unparseable falls back to the compile-time default. See
/// [`make_console_logger`].
const CF_LOG_LEVEL_KEY: &str = "IMPRESSPRESS_CF_LOG_LEVEL";

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

/// `true` when the request's host is a Workers *version preview* host
/// (`<8 hex>-<worker>.<subdomain>.workers.dev`) for this version: the prefix
/// is the first eight characters of the version id `CF_VERSION_METADATA`
/// reports. Without that binding (local `wrangler dev`, a deploy without the
/// binding) any eight-hex-plus-dash prefix counts — the conservative reading,
/// since a canonical worker name is not normally eight hex characters and a
/// dash.
fn host_is_version_preview(req: &worker::Request, env: &worker::Env) -> worker::Result<bool> {
    let host = req.url()?.host_str().unwrap_or("").to_ascii_lowercase();
    // `.filter(|id| !id.is_empty())` matters, and matches what the two other
    // readers of this binding already do: an empty id is `Some("")`, and
    // `"".starts_with(prefix)` is false for every prefix, so the host would
    // be judged NOT a version preview and the lockdown would fail OPEN — the
    // opposite of the conservative fallback below. Dropping it to `None`
    // falls back to the hex-prefix pattern, which locks.
    let version_id = env
        .get_binding::<worker::WorkerVersionMetadata>(VERSION_METADATA_BINDING)
        .ok()
        .map(|metadata| metadata.id())
        .filter(|id| !id.is_empty());
    Ok(is_version_preview_host(&host, version_id.as_deref()))
}

/// The pure half of [`host_is_version_preview`].
fn is_version_preview_host(host: &str, version_id: Option<&str>) -> bool {
    // `split` always yields at least one item (including for ""), so this is
    // total rather than a fallible lookup.
    let first_label = host.split('.').next().unwrap_or(host);
    let Some((prefix, _worker)) = first_label.split_once('-') else {
        return false;
    };
    let looks_like_preview_prefix =
        prefix.len() == 8 && prefix.bytes().all(|b| b.is_ascii_hexdigit());
    match version_id {
        Some(id) => looks_like_preview_prefix && id.to_ascii_lowercase().starts_with(prefix),
        None => looks_like_preview_prefix,
    }
}

/// Constant-time deploy-token authorization shared by control-plane endpoints
/// and the workers.dev preview bypass. The bypass applies to any normal route
/// only when the exact secret is supplied; unauthenticated preview traffic
/// remains a plain 404.
fn deploy_token_authorized(req: &worker::Request, env: &worker::Env) -> bool {
    let presented = req
        .headers()
        .get("X-Deploy-Token")
        .ok()
        .flatten()
        .unwrap_or_default();
    let expected = env
        .secret(impresspress_core::config_vars::DEPLOY_TOKEN_KEY)
        .map(|secret| secret.to_string())
        .unwrap_or_default();
    if presented.is_empty() || expected.is_empty() {
        return false;
    }
    let presented_digest = wafer_run::sha256(presented.as_bytes());
    let expected_digest = wafer_run::sha256(expected.as_bytes());
    wafer_block_crypto::primitives::constant_time_eq(&presented_digest, &expected_digest)
}

fn prepared_status_endpoint(
    req: &worker::Request,
    env: &worker::Env,
) -> worker::Result<worker::Response> {
    if req.method() != worker::Method::Get {
        return worker::Response::error("method not allowed", 405);
    }
    if !deploy_token_authorized(req, env) {
        return worker::Response::error("not found", 404);
    }
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let plan = packaged_prepared_runtime_plan(env)?
            .ok_or("prepared runtime plan Text module is not installed")?;
        let identity = prepared_runtime_identity(env)?;
        plan.verify_compatibility(
            &identity.application_id,
            &identity.application_build_sha256,
            &identity.dependency_lock,
            &identity.release_assets,
        )?;
        Ok(plan.summary()?)
    })();
    match result {
        Ok(summary) => worker::Response::ok(serde_json::to_string_pretty(&summary)?),
        Err(error) => {
            worker::console_log!("prepared runtime verification failed: {error}");
            worker::Response::error("prepared runtime verification failed", 500)
        }
    }
}

async fn prepared_verify_endpoint(
    req: &worker::Request,
    env: &worker::Env,
) -> worker::Result<worker::Response> {
    if req.method() != worker::Method::Post {
        return worker::Response::error("method not allowed", 405);
    }
    if !deploy_token_authorized(req, env) {
        return worker::Response::error("not found", 404);
    }
    let result = async {
        let plan = packaged_prepared_runtime_plan(env)?
            .ok_or("prepared runtime plan Text module is not installed")?;
        let identity = prepared_runtime_identity(env)?;
        plan.verify_compatibility(
            &identity.application_id,
            &identity.application_build_sha256,
            &identity.dependency_lock,
            &identity.release_assets,
        )?;
        // Verification is mutation-free: a missing generation is a hard
        // failure, never an invitation to mint one. This prevents a final
        // candidate from accepting a plan exported by an older isolate after
        // mutable structural/config state changed.
        let kv = make_kv_backend(env, runner::KV_BINDING)?;
        let observed_generation = kv
            .get(impresspress_core::cache_key::CONFIG_VERSION_KEY)
            .await
            .map_err(|error| format!("read current config generation: {error}"))?
            .ok_or("current config generation is missing")?;
        plan.verify_config_generation(&observed_generation)?;
        let summary = plan.summary()?;

        // Parse the O(1) release routing identity bound into the Worker
        // version. The key inventory itself is fetched and digest-verified
        // from R2 further below.
        let release_routing = request_services::ReleaseAssetIdentity::from_env(env)
            .map_err(|error| format!("release routing identity: {error}"))?;
        match (&identity.release_assets, release_routing.as_deref()) {
            (impresspress_core::PreparedReleaseAssets::Absent, None) => {}
            (
                impresspress_core::PreparedReleaseAssets::Present {
                    asset_set_sha256,
                    manifest_key,
                    ..
                },
                Some(routing),
            ) if asset_set_sha256.strip_prefix("sha256:") == Some(routing.id())
                && manifest_key == routing.manifest_key() => {}
            _ => return Err("release routing identity does not match prepared plan".into()),
        }

        let mut release_asset = serde_json::Value::Null;
        if let impresspress_core::PreparedReleaseAssets::Present {
            asset_set_sha256,
            immutable_prefix,
            manifest_key,
            manifest_sha256,
            logical_keys_sha256,
        } = &identity.release_assets
        {
            let storage = make_r2_storage_service(env, runner::R2_BINDING)?;
            let (manifest_folder, manifest_name) = manifest_key
                .rsplit_once('/')
                .ok_or("release manifest key has no folder component")?;
            let (manifest_bytes, _) = storage.get(manifest_folder, manifest_name).await?;
            let actual_manifest_sha256 = format!(
                "sha256:{}",
                impresspress_core::util::sha256_hex(&manifest_bytes)
            );
            if &actual_manifest_sha256 != manifest_sha256 {
                return Err(format!(
                    "release manifest digest mismatch: expected {manifest_sha256}, computed {actual_manifest_sha256}"
                )
                .into());
            }
            let manifest: RuntimeReleaseManifest = serde_json::from_slice(&manifest_bytes)?;
            if manifest.schema_version != 1
                || &manifest.immutable_prefix != immutable_prefix
                || format!("sha256:{}", manifest.asset_set_sha256) != *asset_set_sha256
                || manifest
                    .files
                    .windows(2)
                    .any(|pair| pair[0].logical_key >= pair[1].logical_key)
            {
                return Err("release manifest identity/order mismatch".into());
            }
            let asset_set_material = serde_json::to_vec(&RuntimeReleaseManifestIdentity {
                schema_version: manifest.schema_version,
                files: &manifest.files,
            })?;
            let computed_asset_set = format!(
                "sha256:{}",
                impresspress_core::util::sha256_hex(&asset_set_material)
            );
            if &computed_asset_set != asset_set_sha256 {
                return Err("release manifest asset-set digest mismatch".into());
            }
            let logical_keys = manifest
                .files
                .iter()
                .map(|entry| entry.logical_key.as_str())
                .collect::<Vec<_>>();
            let routing = release_routing
                .as_ref()
                .ok_or("prepared release has no Worker routing identity")?;
            // Parse the exact R2 KEYS_JSON inventory used by ordinary request
            // reads. Manifest verification alone is insufficient: a
            // self-consistent manifest cannot detect a different routing
            // inventory bound into the Worker version, or an R2 object that
            // has drifted from the digest the Worker is pinned to.
            if routing.keys_sha256() != logical_keys_sha256.as_str() {
                return Err("Worker keys digest differs from prepared plan".into());
            }
            let (keys_folder, keys_name) = routing.keys_location();
            let (keys_bytes, _) = storage.get(keys_folder, keys_name).await?;
            let inventory = impresspress_core::release_inventory::ReleaseInventory::from_json_bytes(
                &keys_bytes,
                logical_keys_sha256,
            )?;
            if inventory.logical_keys_sorted() != logical_keys {
                return Err("R2 key inventory differs from release manifest".into());
            }
            let logical_keys_json = serde_json::to_vec(&logical_keys)?;
            let computed_keys_sha256 = format!(
                "sha256:{}",
                impresspress_core::util::sha256_hex(&logical_keys_json)
            );
            if &computed_keys_sha256 != logical_keys_sha256 {
                return Err("release manifest logical-key digest mismatch".into());
            }

            if let Some(entry) = manifest.files.first() {
                let release = request_services::LoadedRelease {
                    identity: routing.clone(),
                    inventory: std::sync::Arc::new(inventory),
                };
                let immutable_key = release
                    .physical_object_key(&entry.logical_key)
                    .ok_or("representative asset is absent from the R2 key inventory")?;
                let (asset_folder, asset_name) = immutable_key
                    .rsplit_once('/')
                    .ok_or("representative immutable key has no folder component")?;
                let (bytes, _) = storage.get(asset_folder, asset_name).await?;
                let actual_sha256 = impresspress_core::util::sha256_hex(&bytes);
                if actual_sha256 != entry.sha256 {
                    return Err(format!(
                        "representative release asset digest mismatch: expected {}, computed {actual_sha256}",
                        entry.sha256
                    )
                    .into());
                }
                release_asset = serde_json::json!({
                    "logical_key": entry.logical_key,
                    "immutable_key": immutable_key,
                    "sha256": actual_sha256,
                });
            }
        }
        Ok::<_, Box<dyn std::error::Error>>(serde_json::json!({
            "schema_version": 1,
            "ok": true,
            "summary": summary,
            "release_asset_verified": true,
            "release_asset": release_asset,
        }))
    }
    .await;

    match result {
        Ok(report) => worker::Response::ok(serde_json::to_string_pretty(&report)?),
        Err(error) => {
            worker::console_log!("prepared runtime deep verification failed: {error}");
            worker::Response::error("prepared runtime deep verification failed", 500)
        }
    }
}

/// [`BootHooks`](impresspress_core::builder::BootHooks) impl for the Cloudflare
/// target. Ordinary request builds load block_settings read-only before build
/// (the router needs its enablement map up front). Deploy init additionally
/// seeds structural defaults after admin migration has created the table, then
/// publishes the resulting snapshot before any other block initializes. The
/// shared auto-generated-secret pass also belongs after admin migration 002 has
/// added the `variables.block` column.
///
/// Not constructed on the request path — the per-isolate cache seals without a
/// boot funnel. Constructed by `deploy_init_endpoint` (`/_deploy/init`), which
/// runs the full boot (migrations + seeds) on demand at deploy time.
struct CfBootHooks {
    db: Arc<dyn DatabaseService>,
    block_settings_handle: Arc<std::sync::RwLock<impresspress_core::features::BlockSettings>>,
}

#[wafer_block::wafer_async_trait]
impl impresspress_core::builder::BootHooks for CfBootHooks {
    async fn seed_after_admin_init(&self, wafer: &mut wafer_run::Wafer) -> Result<(), String> {
        impresspress_core::boot::seed_auto_generated(&self.db).await;

        let block_settings = impresspress_core::features::load_and_seed_block_settings(&self.db)
            .await
            .map_err(|e| format!("seed block_settings after admin init: {e}"))?;
        *self
            .block_settings_handle
            .write()
            .expect("BlockSettings RwLock poisoned during Cloudflare deploy init") =
            block_settings.clone();

        let mut snapshot = (**wafer.config_snapshot()).clone();
        snapshot.insert(
            impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY.to_string(),
            block_settings.to_config_json(),
        );
        wafer.set_config_snapshot(snapshot);
        Ok(())
    }
}

#[cfg(test)]
mod request_config_tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    /// The workers.dev opt-in must never open a version preview: the atomic
    /// deploy proves an unpromoted candidate is unreachable before promoting.
    #[wasm_bindgen_test]
    fn version_preview_host_is_recognised_by_its_own_version_prefix() {
        let id = Some("c89ac716-7f68-437c-8484-640b5b9f2b42");
        assert!(is_version_preview_host(
            "c89ac716-impresspress-webmcp-demo.jorissuppers.workers.dev",
            id
        ));
        assert!(!is_version_preview_host(
            "impresspress-webmcp-demo.jorissuppers.workers.dev",
            id
        ));
        // Another version's preview prefix is not this version's.
        assert!(!is_version_preview_host(
            "30fbdf19-impresspress-webmcp-demo.jorissuppers.workers.dev",
            id
        ));
        // A worker whose name happens to start with eight hex characters and
        // a dash is mistaken for a preview only when no version id is known.
        assert!(!is_version_preview_host(
            "deadbeef-shop.example.workers.dev",
            id
        ));
        assert!(is_version_preview_host(
            "deadbeef-shop.example.workers.dev",
            None
        ));
    }

    /// The lockdown must fail CLOSED when the version id is unusable.
    ///
    /// `CF_VERSION_METADATA` can be present and yield an empty id (wrangler
    /// dev, a hand-written `[version_metadata]` on an unversioned deploy).
    /// That reaches this function as `Some("")`, and `"".starts_with(prefix)`
    /// is false for every prefix — so before the `.filter(|id| !id.is_empty())`
    /// on the caller, an opt-in consumer served the full app anonymously on
    /// its unpromoted preview URL.
    #[wasm_bindgen_test]
    fn an_unusable_version_id_still_locks_the_preview_host() {
        // What the caller now passes for an empty id.
        assert!(is_version_preview_host(
            "c89ac716-impresspress-webmcp-demo.jorissuppers.workers.dev",
            None
        ));
        // And the shape that used to slip through, asserted directly.
        assert!(
            !is_version_preview_host(
                "c89ac716-impresspress-webmcp-demo.jorissuppers.workers.dev",
                Some("")
            ),
            "an empty id cannot match any prefix — the caller must not pass it"
        );
        // The canonical host stays open on both.
        assert!(!is_version_preview_host(
            "impresspress-webmcp-demo.jorissuppers.workers.dev",
            None
        ));
    }

    /// The function requires a pre-lowercased host — its only caller
    /// lowercases, and that invariant is invisible from the signature.
    #[wasm_bindgen_test]
    fn version_preview_matching_is_case_insensitive_on_the_id() {
        assert!(is_version_preview_host(
            "c89ac716-worker.example.workers.dev",
            Some("C89AC716-7F68-437C-8484-640B5B9F2B42")
        ));
    }

    #[wasm_bindgen_test]
    fn verified_identity_cache_loads_once_and_invalidates_on_identity_change() {
        let cache: impresspress_core::IdentityCache<u32> = impresspress_core::IdentityCache::new();
        let loads = std::cell::Cell::new(0);
        let first = cache
            .get_or_try_insert_with("worker-a/plan-a".to_string(), || {
                loads.set(loads.get() + 1);
                Ok::<_, ()>(11)
            })
            .unwrap();
        let second = cache
            .get_or_try_insert_with("worker-a/plan-a".to_string(), || {
                loads.set(loads.get() + 1);
                Ok::<_, ()>(22)
            })
            .unwrap();
        assert!(std::rc::Rc::ptr_eq(&first, &second));
        assert_eq!(loads.get(), 1);

        let replacement = cache
            .get_or_try_insert_with("worker-b/plan-a".to_string(), || {
                loads.set(loads.get() + 1);
                Ok::<_, ()>(33)
            })
            .unwrap();
        assert_eq!(*replacement, 33);
        assert_eq!(loads.get(), 2);
    }

    #[wasm_bindgen_test]
    fn explicit_request_secret_never_enters_cached_structural_snapshot() {
        let structural = HashMap::from([("ROUTES".to_string(), "v1".to_string())]);
        let cached_snapshot = structural.clone();

        let mut request_a_config = structural.clone();
        let mut request_a_overlay = HashMap::new();
        extend_with_request_config(
            &mut request_a_config,
            &mut request_a_overlay,
            &HashMap::from([("APP_SECRET".to_string(), "secret-a".to_string())]),
        );

        let mut request_b_config = cached_snapshot.clone();
        let mut request_b_overlay = HashMap::new();
        extend_with_request_config(
            &mut request_b_config,
            &mut request_b_overlay,
            &HashMap::from([("APP_SECRET".to_string(), "secret-b".to_string())]),
        );

        assert_eq!(cached_snapshot.get("APP_SECRET"), None);
        assert_eq!(
            request_a_config.get("APP_SECRET").map(String::as_str),
            Some("secret-a")
        );
        assert_eq!(
            request_b_config.get("APP_SECRET").map(String::as_str),
            Some("secret-b")
        );
        assert!(!request_b_config.values().any(|value| value == "secret-a"));
    }

    #[wasm_bindgen_test]
    fn deploy_prepare_flag_is_the_only_request_value_retained_for_lifecycle() {
        let mut snapshot = HashMap::from([("ROUTES".to_string(), "v1".to_string())]);
        let prepare_key = impresspress_core::deploy_init::PREPARE_RUNTIME_PLAN_KEY;
        let request = HashMap::from([
            (prepare_key.to_string(), "1".to_string()),
            ("APP_SECRET".to_string(), "do-not-retain".to_string()),
        ]);

        retain_prepare_runtime_plan_flag(&mut snapshot, &request);

        assert_eq!(snapshot.get(prepare_key).map(String::as_str), Some("1"));
        assert_eq!(snapshot.get("APP_SECRET"), None);
    }

    #[wasm_bindgen_test]
    fn request_config_identity_is_order_independent_and_value_sensitive() {
        let left = HashMap::from([
            ("B".to_string(), "2".to_string()),
            ("A".to_string(), "1".to_string()),
        ]);
        let right = HashMap::from([
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
        ]);
        let changed = HashMap::from([
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "3".to_string()),
        ]);
        assert_eq!(config_identity_hash(&left), config_identity_hash(&right));
        assert_ne!(config_identity_hash(&left), config_identity_hash(&changed));
    }
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
