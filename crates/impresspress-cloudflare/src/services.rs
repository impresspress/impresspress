//! The crate's public `make_*` service constructors, plus the Worker var only a
//! service constructor reads.
//!
//! Consumers (`impresspress-cloud`'s worker, the `impresspress build --target
//! cloudflare` shim) construct services through these helpers rather than
//! importing the concrete adapter types, so the adapter internals stay private.
//! `lib.rs` re-exports the public half.

use std::{collections::HashMap, sync::Arc};

use wafer_core::interfaces::{
    config::service::ConfigService, crypto::service::CryptoService,
    database::service::DatabaseService, logger::service::LoggerService,
    network::service::NetworkService, storage::service::StorageService,
};

use crate::{
    config_service, crypto_service, database, kv_cached_db, logger_service, network_service,
    request_services, storage,
};

/// Construct a D1-backed [`DatabaseService`] from a worker `Env` and the D1
/// binding name.
///
/// The binding name must match a `[[d1_databases]]` entry in the consumer's
/// `wrangler.toml` (e.g. `"DB"`). `queries` is the calling invocation's
/// [`D1QueryCount`](crate::database::D1QueryCount): create one per `fetch` or
/// `scheduled` invocation and pass it to every D1 service built in it, so each
/// reports what that invocation has left of D1's per-invocation query limit.
pub fn make_d1_database_service(
    env: &worker::Env,
    binding: &str,
    queries: &database::D1QueryCount,
) -> Result<Arc<dyn DatabaseService>, worker::Error> {
    let environment = crate::environment::CfEnvironment::capture(env);
    Ok(make_d1_database_service_concrete(
        env,
        &environment,
        binding,
        queries,
    )?)
}

/// Concrete-typed variant of [`make_d1_database_service`], for the internal
/// callers that already hold the request's environment capture: the
/// audit-row write in `run()` (one `DatabaseService::create_many`, which
/// reaches D1's native `batch()` through
/// [`DbExec::run_transaction`](wafer_core::interfaces::database::exec::DbExec::run_transaction))
/// and [`make_kv_cached_database_service_with_backend`], which hands the
/// concrete handle back beside the decorated one.
///
/// `environment` is a parameter rather than a capture of its own for the same
/// reason [`build_runtime`](crate::runtime_build::build_runtime) takes one:
/// every internal caller already holds the request's capture, and a
/// constructor that could reach for `env.var` itself would put the duplicate
/// per-request var reads `CfEnvironment` exists to stop back on the path. The
/// public wrappers above capture because a consumer hands them only an `Env`
/// — the same shape [`make_console_logger`] uses.
///
/// `queries` is the invocation's statement count, shared by every D1 service
/// built in it (see [`database::D1QueryCount`]).
pub(crate) fn make_d1_database_service_concrete(
    env: &worker::Env,
    environment: &crate::environment::CfEnvironment,
    binding: &str,
    queries: &database::D1QueryCount,
) -> Result<Arc<database::D1DatabaseService>, worker::Error> {
    Ok(Arc::new(
        d1_service(env.d1(binding)?, environment, binding, queries)
            .map_err(worker::Error::RustError)?,
    ))
}

/// The environment → adapter joint, split out from the binding lookup above so
/// it can be tested.
///
/// `env.d1(binding)` resolves a real Worker binding — a `dyn_into` that no
/// fake `Env` satisfies, and CI has no workerd D1 — so
/// [`make_d1_database_service_concrete`] as a whole cannot run under
/// `wasm-bindgen-test`. Everything it decides is here, where a test can hand
/// in a handle directly; what stays uncovered is the binding lookup and the
/// `Arc`. See `the_service_takes_its_strict_verdict_from_the_environment` and
/// `the_service_takes_its_query_limit_from_the_environment`.
///
/// Fails when the deploy's `IMPRESSPRESS_D1_QUERIES_PER_INVOCATION` is not a
/// usable limit.
pub(crate) fn d1_service(
    db: worker::D1Database,
    environment: &crate::environment::CfEnvironment,
    binding: &str,
    queries: &database::D1QueryCount,
) -> Result<database::D1DatabaseService, String> {
    Ok(database::D1DatabaseService::new(
        db,
        environment.strict_schema_enabled(),
        binding,
        queries.clone(),
        environment.d1_queries_per_invocation()?,
    ))
}

/// Construct a [`DatabaseService`] backed by D1 with a Cloudflare KV cache
/// layered on top of the read shapes
/// `impresspress_core::cache_key::read_key` recognizes — in practice today,
/// `block_settings`' eager full-table load.
///
/// The per-block `variables WHERE block=?` shape is still recognized, and
/// variables writes still invalidate its key, but `D1ConfigSource` — the
/// reader that shape was built for — now takes ONE unfiltered snapshot of
/// the variables table instead, which `read_key` deliberately refuses to
/// cache. See `cache_key::block_list_opts`.
///
/// The KV binding name must match a `[[kv_namespaces]]` entry in the
/// consumer's `wrangler.toml` (canonical name: `"CONFIG_CACHE"`).
///
/// Fails fast if the KV binding is missing — silent degradation would
/// mask a config-drift outage.
///
/// `queries` is the calling invocation's statement count, as for
/// [`make_d1_database_service`].
pub fn make_kv_cached_database_service(
    env: &worker::Env,
    d1_binding: &str,
    kv_binding: &str,
    queries: &database::D1QueryCount,
) -> Result<Arc<dyn DatabaseService>, worker::Error> {
    let environment = crate::environment::CfEnvironment::capture(env);
    let (db, _backend, _batch_db) = make_kv_cached_database_service_with_backend(
        env,
        &environment,
        d1_binding,
        kv_binding,
        kv_cached_db::CacheMode::default(),
        queries,
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

pub(crate) fn make_kv_cached_database_service_with_backend(
    env: &worker::Env,
    environment: &crate::environment::CfEnvironment,
    d1_binding: &str,
    kv_binding: &str,
    mode: kv_cached_db::CacheMode,
    queries: &database::D1QueryCount,
) -> Result<KvCachedDbServiceWithBackend, worker::Error> {
    let d1 = make_d1_database_service_concrete(env, environment, d1_binding, queries)?;
    let inner: Arc<dyn DatabaseService> = d1.clone();
    let backend = make_kv_backend(env, kv_binding)?;
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
    let Some(identity) = request_services::ReleaseAssetIdentity::from_environment(
        &crate::environment::CfEnvironment::capture(env),
    )
    .map_err(worker::Error::RustError)?
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

/// Construct the Worker's [`CryptoService`]: the wafer-block-crypto HS256
/// JWT engine (exp-required, per-block HKDF-derived keys — same policy as
/// native), with password hashing and verification sent to the
/// password-hasher Worker through `hasher` (see
/// [`crypto_service`](crate::crypto_service)).
///
/// `jwt_secret` is the HMAC master secret used to sign and verify JWTs.
/// It must be at least `wafer_block_crypto::primitives::MIN_JWT_SECRET_LEN`
/// bytes; a missing/short secret surfaces as an error on each sign/verify
/// rather than failing worker boot.
pub fn make_crypto_service(
    jwt_secret: String,
    hasher: crypto_service::PasswordHasher,
) -> Arc<dyn CryptoService> {
    Arc::new(crypto_service::ImpresspressCryptoService::new(
        jwt_secret, hasher,
    ))
}

/// Construct a [`NetworkService`] backed by the CF Worker global `fetch` API.
pub fn make_fetch_network_service() -> Arc<dyn NetworkService> {
    Arc::new(network_service::WorkerFetchService)
}

/// Construct a [`LoggerService`] that writes to `worker::console_log`.
///
/// The minimum emitted level comes from the `IMPRESSPRESS_CF_LOG_LEVEL` worker
/// var (set via `wrangler.toml [vars]` or the dashboard) — a runtime knob,
/// unlike the previous `option_env!` compile-time read, so an operator can
/// raise/lower verbosity per deployment without rebuilding. Falls back to the
/// compile-time default (Debug in dev builds, Info in release) when the var is
/// unset or unparseable.
///
/// This is the public entry for a consumer that holds only a `worker::Env`; it
/// captures one. The crate's own request path already has a
/// [`CfEnvironment`](crate::environment::CfEnvironment) and calls
/// [`console_logger`] directly, so `IMPRESSPRESS_CF_LOG_LEVEL` is still read
/// once per request there.
pub fn make_console_logger(env: &worker::Env) -> Arc<dyn LoggerService> {
    console_logger(crate::environment::CfEnvironment::capture(env).cf_log_level())
}

/// [`make_console_logger`] over an already-resolved level.
pub(crate) fn console_logger(level: Option<&str>) -> Arc<dyn LoggerService> {
    Arc::new(logger_service::ConsoleLoggerService::new(level))
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
pub(crate) fn resolved_log_level(
    environment: &crate::environment::CfEnvironment,
) -> impresspress_core::log_level::LogLevel {
    logger_service::resolve_level(environment.cf_log_level())
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

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::database::exec::DbExec;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;
    use crate::environment::test_support::empty_environment;

    /// A `D1Database` that is never queried — see `database::tests`, which
    /// uses the same handle for the same reason: `DbExec::strict_schema` is
    /// plain Rust state, so the `undefined` is never dereferenced.
    fn never_queried_handle() -> worker::D1Database {
        wasm_bindgen::JsCast::unchecked_into::<worker::D1Database>(
            wasm_bindgen::JsValue::undefined(),
        )
    }

    fn service(environment: &crate::environment::CfEnvironment) -> database::D1DatabaseService {
        d1_service(
            never_queried_handle(),
            environment,
            "DB",
            &database::D1QueryCount::new(),
        )
        .expect("a usable environment")
    }

    /// The joint between the environment and the adapter.
    ///
    /// `database::tests` proves the adapter honours whatever verdict it is
    /// constructed with, and `environment::tests` proves the environment reads
    /// the var the way wafer-core does. Neither sees whether this crate
    /// actually connects the two — a hardcoded `false` here would pass both.
    #[wasm_bindgen_test]
    fn the_service_takes_its_strict_verdict_from_the_environment() {
        let mut on = empty_environment();
        on.set_strict_schema_for_test("true");
        assert!(
            DbExec::strict_schema(&service(&on)),
            "a deploy that sets the var must get a strict service",
        );

        let off = empty_environment();
        assert!(
            !DbExec::strict_schema(&service(&off)),
            "and one that does not must not — a hardcoded `true` is as wrong \
             as a hardcoded `false`",
        );
    }

    /// The D1 service reports this deploy's query limit, not a constant: a
    /// lowered limit (50 here) must reach the budget the database handler
    /// admits writes against, and an unusable value must refuse to build a
    /// service.
    #[wasm_bindgen_test]
    fn the_service_takes_its_query_limit_from_the_environment() {
        use wafer_core::interfaces::database::service::StatementBudget;

        let limit = |environment: &crate::environment::CfEnvironment| match DbExec::statement_budget(
            &service(environment),
        ) {
            Ok(StatementBudget::Limited { limit, used: 0 }) => limit,
            other => panic!("a D1 service reports a limited budget: {other:?}"),
        };

        assert_eq!(limit(&empty_environment()), 1000, "unset is the default");
        let mut lowered = empty_environment();
        lowered.set_d1_queries_per_invocation_for_test("50");
        assert_eq!(limit(&lowered), 50);

        let mut malformed = empty_environment();
        malformed.set_d1_queries_per_invocation_for_test("fifty");
        assert!(d1_service(
            never_queried_handle(),
            &malformed,
            "DB",
            &database::D1QueryCount::new()
        )
        .is_err());
    }
}
