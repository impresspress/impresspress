//! Building a Cloudflare runtime: the services, both config surfaces, and the
//! Worker's wrappers around the hosted boot funnels.
//!
//! [`build_runtime`] wires this request's D1/KV/R2/crypto/network/logger
//! services into an `ImpresspressBuilder`, hands both config surfaces over in
//! one `RuntimeConfig::install` call, runs the consumer's registrations and
//! returns a [`BuiltRuntime`] — built, but neither sealed nor booted.
//!
//! Sealing and booting is `impresspress_core::builder`'s: the deploy funnel
//! (`boot_deploy_runtime`) and the two request-path builds
//! (`boot_request_runtime`, picked by `RequestBuild`), each documented at its
//! own definition there. [`boot_deploy_runtime`] and [`boot_request_runtime`]
//! here only run them under this request's services. Picking a funnel is
//! picking a function, never composing a `(grants, hooks, policy)` triple at a
//! call site — the paths are not interchangeable.

use std::{collections::HashMap, sync::Arc};

use impresspress_core::{
    builder::{
        add_request_config, request_config_source, structural_runtime_config, BootReport,
        ImpresspressBuilder, RequestBuild, RuntimeConfig, StructuralConfigInputs,
        PREPARE_RUNTIME_PLAN_KEY,
    },
    config_source::{RetryMode, VariablesSnapshot},
};
use wafer_core::interfaces::{
    database::service::DatabaseService, storage::service::StorageService,
};

use crate::{
    environment::{CfEnvironment, BUILDER_WORKER_VAR_KEYS, PROTECTED_ENV_KEYS},
    kv_cached_db, request_services, runner,
    services::{
        console_logger, make_config_service, make_crypto_service, make_fetch_network_service,
        make_kv_cached_database_service_with_backend, make_r2_storage_service,
    },
};

/// Everything [`build_runtime`] produces: a built-but-not-sealed-or-booted
/// runtime plus the service handles Tasks 7-8 need (per-isolate build
/// caching, the `/_deploy/init` endpoint) that would otherwise be locked
/// inside its function-local scope.
pub(crate) struct BuiltRuntime {
    pub(crate) wafer: wafer_run::Wafer,
    pub(crate) db: Arc<dyn DatabaseService>,
    /// Concrete request-current services used while building/sealing this
    /// disposable runtime. The cached [`runtime_cache::ReadyRuntime`] never
    /// copies this field; it retains only the Wafer whose service blocks are
    /// stateless forwarding proxies.
    pub(crate) services: std::rc::Rc<request_services::RequestServices>,
    pub(crate) plan_exporter: impresspress_core::builder::PreparedPlanExporter,
    /// Same settings snapshot the router owns. Deploy init updates it after
    /// admin migration + structural seeding so the disposable candidate
    /// runtime validates the exact state ordinary cold builds will read.
    pub(crate) block_settings_handle:
        Arc<std::sync::RwLock<impresspress_core::features::BlockSettings>>,
    /// The runtime's read of the `variables` table, shared by its config block
    /// and by every config source built for it — this build's, and each warm
    /// request's ([`warm_request_services`]). Data and memo only, no handle,
    /// so the cached runtime may keep it.
    pub(crate) variables_snapshot: Arc<VariablesSnapshot>,
}

/// Run the deploy funnel (`impresspress_core::builder::boot_deploy_runtime`)
/// over the runtime `/_deploy/init` and `/_deploy/prepare` built, under this
/// request's services. The seeded settings are published through the
/// stateless config forwarder, as the cached runtime's are.
pub(crate) async fn boot_deploy_runtime(built: &mut BuiltRuntime) -> Result<BootReport, String> {
    let db = built.db.clone();
    let block_settings = built.block_settings_handle.clone();
    request_services::scope(built.services.clone(), async {
        impresspress_core::builder::boot_deploy_runtime(
            &mut built.wafer,
            &db,
            &block_settings,
            request_services::config_proxy(),
        )
        .await
        .map_err(|error| format!("boot: {error}"))
    })
    .await
}

/// Run a request-path build (`impresspress_core::builder::boot_request_runtime`)
/// under this request's services: [`RequestBuild::Database`] for the
/// per-isolate dynamic build `runtime_cache::get_or_build` publishes,
/// [`RequestBuild::Prepared`] for a runtime hydrated from this Worker
/// version's verified plan. Under the services because the config source
/// may still read per block while the remaining blocks initialize, and
/// keeping those reads here prevents cross-request lazy-init waiters.
pub(crate) async fn boot_request_runtime(
    built: &mut BuiltRuntime,
    build: RequestBuild,
) -> Result<BootReport, String> {
    let db = built.db.clone();
    let block_settings = built.block_settings_handle.clone();
    request_services::scope(built.services.clone(), async {
        impresspress_core::builder::boot_request_runtime(
            &mut built.wafer,
            &db,
            &block_settings,
            request_services::config_proxy(),
            build,
        )
        .await
        .map_err(|error| format!("boot: {error}"))
    })
    .await
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
/// read this window makes must tolerate a not-yet-created table, or first
/// deploys deadlock before migrations can create the tables. Each read owns
/// that tolerance at its own definition, and says how:
/// `platform_state::block_settings::load` (this function's eager read),
/// `platform_state::wrap_grants::load` (the funnel's `GrantSource::Database`),
/// and `DatabaseConfigSource::load_for_block` (reached only after admin's
/// `Init` has created the variables table, for the reason `boot` gives at its
/// admin-first step). `migration_helper::read_state` reads the config
/// snapshot, not the database.
// Nine arguments. The captured environment is deliberately a parameter rather
// than something re-derived from `env` here: reading a var twice per request is
// exactly what `CfEnvironment` exists to stop, and a function that could reach
// for `env.var` on its own would put that back. The invocation's D1 statement
// count is one for the same reason: it is created once at the Worker entry and
// only travels.
#[expect(
    clippy::too_many_arguments,
    reason = "the captured environment travels as a parameter so nothing here can \
              re-read `env.var` per request"
)]
pub(crate) async fn build_runtime<F, G>(
    env: &worker::Env,
    environment: &CfEnvironment,
    queries: &crate::database::D1QueryCount,
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
        environment,
        runner::D1_BINDING,
        runner::KV_BINDING,
        cache_mode,
        queries,
    )
    .map_err(|e| {
        format!(
            "DB/KV bindings (D1={:?}, KV={:?}): {e}",
            runner::D1_BINDING,
            runner::KV_BINDING
        )
    })?;

    // 2. Load block settings (enablement + migration state) eagerly —
    //    the only database read this build issues itself. The per-block
    //    env-config pre-load is gone; the config source resolves declared
    //    config keys later, on first init of a block that declares any,
    //    from ONE unfiltered read of the whole variables table grouped by
    //    `block` in memory. block_settings still needs an eager load
    //    because the ImpresspressRouter consumes the enablement map up front
    //    when wiring routes (it can't defer to a per-block init event).
    // An error here is a genuine operational failure (D1 outage, corruption)
    // — never the missing-table cold-start case, which `block_settings::load`
    // answers with no rows (see its doc). Propagate rather than fabricate
    // "every block enabled": the runtime build fails, so no requests are
    // served with a fabricated all-enabled snapshot.
    let block_settings = if let Some(plan) = prepared_plan {
        impresspress_core::features::BlockSettings::from_blocks(
            plan.structure
                .block_settings
                .iter()
                .map(|(name, state)| (name.clone(), state.clone()))
                .collect(),
        )
    } else {
        impresspress_core::platform_state::block_settings::load(&db).await?
    };

    // 3. The structural half of both config surfaces — see
    //    `structural_runtime_config`, which is where every key and its reason
    //    now lives.
    let (mut runtime_config, mut overlay) = structural_runtime_config(structural_config_inputs(
        environment,
        block_settings.to_config_json(),
        force_run_migrations,
    ));

    // Explicit application request config reaches the async service surface
    // (and the ConfigSource overlay) but NOT the snapshot the cached Wafer
    // retains, so secret A from one request can never become request B's sync
    // read. The JWT secret above is a bounded exception that IS on both: CSRF
    // currently reads it synchronously through `Context::config_get`, and
    // Worker-version identity forces a rebuild on rotation.
    add_request_config(&mut runtime_config, &mut overlay, request_config);
    retain_prepare_runtime_plan_flag(&mut runtime_config, request_config);

    // 4. Construct remaining services.
    let bucket: Arc<dyn StorageService> = make_r2_storage_service(env, runner::R2_BINDING)
        .map_err(|e| format!("R2 binding {:?}: {e}", runner::R2_BINDING))?;
    let jwt_secret = runtime_config
        .service_get(impresspress_core::blocks::auth::JWT_SECRET_KEY)
        .unwrap_or_default()
        .to_string();
    let crypto = request_crypto_service(env, jwt_secret, environment)?;
    let network = make_fetch_network_service();
    let logger = console_logger(environment.cf_log_level());

    // Hand both surfaces over at once. The concrete map-backed service is what
    // THIS request reads through; the builder receives the stateless forwarder,
    // because the isolate-cached Wafer must never retain a request's concrete
    // service (see 6a below). `install` is the only way in, so a target cannot
    // fill the service map and forget the snapshot.
    //
    // The concrete service comes back as `install`'s second return value —
    // that this path builds a service the builder does not get is a fact about
    // Cloudflare, so it is in the types rather than in an `Option` captured by
    // the closure and unwrapped with an `expect` for a panic that could not
    // happen.
    let (config_installed, cfg_svc) = runtime_config.install(ImpresspressBuilder::new(), |map| {
        let concrete = make_config_service(map);
        (request_services::config_proxy(), concrete)
    });

    // 5. ConfigSource: see `request_config_source`. Over the runtime's
    //    variables snapshot, created here because the source is consulted
    //    while blocks initialize — before the config block, which the builder
    //    hands the same snapshot, is guaranteed to be. The runtime keeps it
    //    (`BuiltRuntime::variables_snapshot`), so a rebuild starts clean.
    //
    //    `Backoff`: nothing else retries a failed read on this target yet. The
    //    outage plan's retry window (its IP-B) rebuilds the runtime instead and
    //    switches this to `RetryMode::Rebuild`.
    let variables_snapshot = Arc::new(VariablesSnapshot::new(RetryMode::Backoff));
    let cfg_source = request_config_source(db.clone(), overlay, variables_snapshot.clone());
    let services = request_services::RequestServices::new(
        environment,
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
    let crypto_proxy = request_services::crypto_proxy();
    let network_proxy = request_services::network_proxy();
    let logger_proxy = request_services::logger_proxy();
    let config_source_proxy = request_services::config_source_proxy();
    let prepared_identity = prepared_plan
        .map(|_| environment.prepared_runtime_identity())
        .transpose()?;
    let (wafer, block_settings_handle, plan_exporter) = request_services::scope_sync(
        services.clone(),
        || -> Result<_, Box<dyn std::error::Error>> {
            let builder = config_installed
                .database(database_proxy)
                .storage(storage_proxy.clone())
                .crypto(crypto_proxy)
                .network(network_proxy)
                .logger(logger_proxy)
                .block_settings(block_settings)
                .config_source(config_source_proxy)
                .variables_snapshot(variables_snapshot.clone());

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

            // 6. Build runtime. `build()` installs the synchronous
            // `ctx.config_get` snapshot that `RuntimeConfig::install`
            // handed over alongside the service map, so blocks can read
            // embedder-provided keys with no I/O. That is immutable
            // configuration data, not a request-derived service handle;
            // Worker-version identity still forces a rebuild when it
            // changes.
            let mut wafer = builder.build().map_err(|e| format!("builder.build: {e}"))?;

            // 6a. The consumer receives the scoped storage proxy, never the
            // request's concrete R2 Bucket. A block may safely retain this
            // proxy in the isolate-cached runtime.
            register_post_build(&mut wafer, storage_proxy)
                .map_err(|e| format!("register_post_build: {e}"))?;
            Ok((wafer, block_settings_handle, plan_exporter))
        },
    )?;

    Ok(BuiltRuntime {
        wafer,
        db,
        services,
        plan_exporter,
        block_settings_handle,
        variables_snapshot,
    })
}

/// Every value the structural config pass takes out of `worker::Env`, split
/// out of an already-captured environment so the pass itself
/// (`impresspress_core::builder::structural_runtime_config`) is a pure
/// function a native test can drive with the same inputs.
fn structural_config_inputs(
    environment: &CfEnvironment,
    block_settings_json: String,
    run_migrations: bool,
) -> StructuralConfigInputs {
    let bound = |keys: &'static [&'static str]| -> Vec<(&'static str, String)> {
        keys.iter()
            .filter_map(|key| {
                environment
                    .config_value(key)
                    .map(|value| (*key, value.to_string()))
            })
            .collect()
    };
    StructuralConfigInputs {
        secrets: bound(PROTECTED_ENV_KEYS),
        worker_vars: bound(BUILDER_WORKER_VAR_KEYS),
        block_settings_json,
        run_migrations,
    }
}

/// The config surfaces a **warm** request's services read through, from this
/// request's capture of the environment: see
/// `impresspress_core::builder::warm_config_surfaces`.
///
/// Every key [`CfEnvironment`] owns is taken from THIS request's capture and
/// from nowhere else. That used to be a removal branch: the previous code
/// cloned the whole snapshot and then deleted `PROTECTED_ENV_KEYS` and
/// `BUILDER_WORKER_VAR_KEYS` back out of it by name when their bindings were
/// gone — the right answer for the keys somebody had listed, and no answer at
/// all for an Env-owned key on neither list. Deriving the owned set from the
/// capture removes the class instead of the instance.
fn request_config_surfaces(
    environment: &CfEnvironment,
    structural_snapshot: &HashMap<String, String>,
    request_config: &HashMap<String, String>,
) -> (HashMap<String, String>, HashMap<String, String>) {
    impresspress_core::builder::warm_config_surfaces(
        &environment.config_map(),
        structural_snapshot,
        request_config,
    )
}

/// Construct concrete services for one warm request without performing I/O.
/// Binding lookup and service allocation are cheap; D1/KV/R2 operations stay
/// lazy until a block actually calls them.
///
/// The cold path's equivalent is [`build_runtime`], which builds the same six
/// services and then a Wafer around them. Both take their Env-derived config
/// from the same [`CfEnvironment`] method, so the two paths cannot disagree
/// about what the environment currently says.
///
/// `variables_snapshot` is the cached runtime's: this request's config source
/// is new, but reads the table through the runtime's memo, so resolving a
/// block's config on a warm request sends nothing.
pub(crate) fn warm_request_services(
    env: &worker::Env,
    environment: &CfEnvironment,
    queries: &crate::database::D1QueryCount,
    structural_snapshot: &HashMap<String, String>,
    variables_snapshot: &Arc<VariablesSnapshot>,
    request_config: &HashMap<String, String>,
) -> Result<std::rc::Rc<request_services::RequestServices>, Box<dyn std::error::Error>> {
    let (db, _kv, _batch_db) = make_kv_cached_database_service_with_backend(
        env,
        environment,
        runner::D1_BINDING,
        runner::KV_BINDING,
        kv_cached_db::CacheMode::default(),
        queries,
    )?;
    let storage = make_r2_storage_service(env, runner::R2_BINDING)?;

    let (config_map, overlay) =
        request_config_surfaces(environment, structural_snapshot, request_config);

    let config = make_config_service(config_map);
    let crypto = request_crypto_service(env, environment.jwt_secret().to_string(), environment)?;
    let network = make_fetch_network_service();
    let logger = console_logger(environment.cf_log_level());
    let config_source = request_config_source(db.clone(), overlay, variables_snapshot.clone());

    Ok(request_services::RequestServices::new(
        environment,
        db,
        storage,
        config,
        crypto,
        network,
        logger,
        config_source,
    ))
}

/// The crypto service both fills build: `jwt_secret` for tokens, and the
/// password-hasher binding, spread across the shard count this request's
/// capture of the Worker's vars names.
///
/// A shard count that does not parse fails the build. A missing binding does
/// not: each password operation fails instead (see
/// [`crate::crypto_service::PasswordHasher`]), so the rest of the site serves.
fn request_crypto_service(
    env: &worker::Env,
    jwt_secret: String,
    environment: &CfEnvironment,
) -> Result<Arc<dyn wafer_core::interfaces::crypto::service::CryptoService>, String> {
    Ok(make_crypto_service(
        jwt_secret,
        crate::crypto_service::PasswordHasher::from_env(env, environment.password_hasher_shards()?),
    ))
}

/// Promote the one non-secret request value lifecycle code must read from the
/// synchronous config snapshot while preparing an isolated deploy candidate.
/// Runs after `add_request_config`, which has already put it on the async
/// surface as a service-only key; `both` moves it onto the snapshot as well
/// and retracts the recorded divergence.
fn retain_prepare_runtime_plan_flag(
    config: &mut RuntimeConfig,
    request_config: &HashMap<String, String>,
) {
    if request_config
        .get(PREPARE_RUNTIME_PLAN_KEY)
        .map(String::as_str)
        == Some("1")
    {
        config.both(PREPARE_RUNTIME_PLAN_KEY, "1");
    }
}

#[cfg(test)]
mod tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;
    use crate::{
        crypto_service::test_support::FakeHasher, environment::test_support::empty_environment,
    };

    /// The crypto service both fills build hashes through the password-hasher
    /// binding, across the shard count the Worker var names.
    #[wasm_bindgen_test]
    async fn the_request_crypto_service_hashes_through_the_hasher_binding() {
        let fake = FakeHasher::running(Default::default());
        let mut environment = empty_environment();
        environment.set_password_hasher_shards_for_test("2");
        let crypto =
            request_crypto_service(&fake.env, String::new(), &environment).expect("valid shards");

        let hash = crypto.hash("correct horse").await.expect("hash");
        assert!(
            hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "the Worker's hashes come from the hasher: {hash}"
        );
        crypto
            .compare_hash("correct horse", &hash)
            .await
            .expect("verifies");
        let shards = fake.shards.borrow();
        assert_eq!(shards.len(), 2);
        assert!(
            shards.iter().all(|s| s == "shard-0" || s == "shard-1"),
            "{shards:?}"
        );
    }

    /// A shard count that does not parse fails the build.
    #[wasm_bindgen_test]
    fn a_malformed_shard_count_fails_the_crypto_service() {
        let fake = FakeHasher::running(Default::default());
        let mut environment = empty_environment();
        environment.set_password_hasher_shards_for_test("0");
        let Err(err) = request_crypto_service(&fake.env, String::new(), &environment) else {
            panic!("a shard count of 0 must fail the build");
        };
        assert!(
            err.contains(impresspress_password::protocol::SHARDS_VAR),
            "{err}"
        );
    }

    /// The warm-request fill takes every Env-owned key from THIS request's
    /// capture, so a binding removed since the runtime was built cannot survive
    /// in the map the request's `ConfigService` reads.
    #[wasm_bindgen_test]
    fn a_removed_worker_binding_cannot_survive_in_a_warm_requests_config() {
        let csp = impresspress_core::config_vars::CSP_DIRECTIVES_KEY;
        let jwt = impresspress_core::blocks::auth::JWT_SECRET_KEY;
        let block_settings = impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY;

        // What the isolate-cached runtime was built with.
        let snapshot = HashMap::from([
            (jwt.to_string(), "old-secret".to_string()),
            (csp.to_string(), "default-src 'self'".to_string()),
            (
                impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY.to_string(),
                "https://old.test".to_string(),
            ),
            (block_settings.to_string(), "{}".to_string()),
        ]);

        // What the Worker's bindings say NOW: the secret rotated, CSP and CORS
        // were removed.
        let mut environment = empty_environment();
        environment.set_jwt_secret_for_test("new-secret");

        let (config, overlay) = request_config_surfaces(&environment, &snapshot, &HashMap::new());

        assert_eq!(
            config.get(jwt).map(String::as_str),
            Some("new-secret"),
            "a rotated secret must reach this request",
        );
        assert!(
            !config.contains_key(csp),
            "a removed worker var must not be inherited from the cached snapshot",
        );
        assert!(
            !config.contains_key(impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY),
            "a removed worker var must not be inherited from the cached snapshot",
        );
        assert_eq!(
            config.get(block_settings).map(String::as_str),
            Some("{}"),
            "the D1-derived key the environment does NOT own still comes from \
             the runtime that loaded it",
        );
        assert_eq!(
            overlay.get(jwt).map(String::as_str),
            Some("new-secret"),
            "secrets are layered over the D1 variables rows, as on the cold path",
        );
    }

    /// The warm-request overlay carries the secrets this request's environment
    /// binds and no worker var, as the cold one does: a bound worker var
    /// reaches the `ConfigService` map, the builder-time surface, and never a
    /// block's `lifecycle(Init)` config, where it would override the admin's
    /// saved row.
    #[wasm_bindgen_test]
    fn a_bound_worker_var_stays_out_of_a_warm_requests_config_source_overlay() {
        let csp = impresspress_core::config_vars::CSP_DIRECTIVES_KEY;
        let mut environment = empty_environment();
        environment.set_csp_directives_for_test("default-src 'self'");

        let (config, overlay) =
            request_config_surfaces(&environment, &HashMap::new(), &HashMap::new());

        assert_eq!(
            config.get(csp).map(String::as_str),
            Some("default-src 'self'"),
            "a bound worker var reaches the request's ConfigService",
        );
        assert!(
            !overlay.contains_key(csp),
            "a worker var is builder-time input, not a block's Init config",
        );
    }

    /// Consumer request config reaches the warm surfaces, and cannot shadow a
    /// framework-owned key.
    #[wasm_bindgen_test]
    fn warm_request_config_reaches_both_surfaces_but_never_shadows_a_worker_key() {
        let jwt = impresspress_core::blocks::auth::JWT_SECRET_KEY;
        let mut environment = empty_environment();
        environment.set_jwt_secret_for_test("real");

        let request_config = HashMap::from([
            ("APP_TOKEN".to_string(), "t".to_string()),
            (jwt.to_string(), "forged".to_string()),
        ]);
        let (config, overlay) =
            request_config_surfaces(&environment, &HashMap::new(), &request_config);

        assert_eq!(config.get("APP_TOKEN").map(String::as_str), Some("t"));
        assert_eq!(overlay.get("APP_TOKEN").map(String::as_str), Some("t"));
        assert_eq!(
            config.get(jwt).map(String::as_str),
            Some("real"),
            "consumer request config must not shadow an Env-owned key",
        );
    }

    /// A consumer request key that collides with a framework structural key
    /// must not DELETE that key from the snapshot.
    ///
    /// `RuntimeConfig::service_only` is a `remove` on the snapshot surface, so
    /// a collision is not "the request value shadows the structural one" — it
    /// is "the structural one is gone". On `main` the snapshot was cloned
    /// before the request merge and a collision left the structural value
    /// intact; the guard list that replaced that accident covered
    /// `PROTECTED_ENV_KEYS` and `BUILDER_WORKER_VAR_KEYS` but not the keys
    /// the structural pass installs itself. Any key the structural pass
    /// already put on the snapshot is now framework-owned, whatever its name.
    #[wasm_bindgen_test]
    fn request_config_cannot_delete_a_structural_key_from_the_snapshot() {
        let structural = [
            impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY,
            impresspress_core::migration_helper::RUN_MIGRATIONS_KEY,
            impresspress_core::blocks::auth::JWT_SECRET_KEY,
            impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY,
        ];

        let mut config = RuntimeConfig::new();
        for key in structural {
            config.both(key, "structural");
        }

        let mut overlay = HashMap::new();
        let hostile: HashMap<String, String> = structural
            .iter()
            .map(|key| ((*key).to_string(), "from-the-request".to_string()))
            .chain([("APP_SECRET".to_string(), "secret".to_string())])
            .collect();
        add_request_config(&mut config, &mut overlay, &hostile);

        for key in structural {
            assert!(
                config.snapshot_contains(key),
                "{key} must survive a colliding request key on the snapshot",
            );
            assert_eq!(
                config.service_get(key),
                Some("structural"),
                "{key} must keep its structural value on the async surface too",
            );
            assert!(
                !overlay.contains_key(key),
                "{key} must not be shadowed in the ConfigSource overlay either",
            );
        }
        // The consumer's own key still gets through, service-only as before.
        assert_eq!(config.service_get("APP_SECRET"), Some("secret"));
        assert!(!config.snapshot_contains("APP_SECRET"));
        let declared: Vec<&str> = config
            .service_only_keys()
            .iter()
            .map(|entry| entry.key.as_str())
            .collect();
        assert_eq!(declared, vec!["APP_SECRET"]);
    }

    #[wasm_bindgen_test]
    fn explicit_request_secret_never_enters_cached_structural_snapshot() {
        let mut request_a = RuntimeConfig::new();
        request_a.both("ROUTES", "v1");
        let mut request_a_overlay = HashMap::new();
        add_request_config(
            &mut request_a,
            &mut request_a_overlay,
            &HashMap::from([("APP_SECRET".to_string(), "secret-a".to_string())]),
        );

        let mut request_b = RuntimeConfig::new();
        request_b.both("ROUTES", "v1");
        let mut request_b_overlay = HashMap::new();
        add_request_config(
            &mut request_b,
            &mut request_b_overlay,
            &HashMap::from([("APP_SECRET".to_string(), "secret-b".to_string())]),
        );

        assert_eq!(request_a.service_get("APP_SECRET"), Some("secret-a"));
        assert_eq!(request_b.service_get("APP_SECRET"), Some("secret-b"));
        assert!(
            !request_a.snapshot_contains("APP_SECRET"),
            "the snapshot the isolate caches must not carry a request value",
        );
        assert!(!request_b.snapshot_contains("APP_SECRET"));
        assert!(request_a.snapshot_contains("ROUTES"));
        // The divergence is declared, with its reason, not inferred.
        let declared: Vec<&str> = request_a
            .service_only_keys()
            .iter()
            .map(|entry| entry.key.as_str())
            .collect();
        assert_eq!(declared, vec!["APP_SECRET"]);
        assert!(request_a.service_only_keys()[0]
            .because
            .contains("request-current"));
        // And the overlay the D1 ConfigSource layers over its rows gets it too.
        assert_eq!(
            request_a_overlay.get("APP_SECRET").map(String::as_str),
            Some("secret-a")
        );
    }

    #[wasm_bindgen_test]
    fn deploy_prepare_flag_is_the_only_request_value_retained_for_lifecycle() {
        let mut config = RuntimeConfig::new();
        config.both("ROUTES", "v1");
        let request = HashMap::from([
            (PREPARE_RUNTIME_PLAN_KEY.to_string(), "1".to_string()),
            ("APP_SECRET".to_string(), "do-not-retain".to_string()),
        ]);

        let mut overlay = HashMap::new();
        add_request_config(&mut config, &mut overlay, &request);
        retain_prepare_runtime_plan_flag(&mut config, &request);

        assert_eq!(config.service_get(PREPARE_RUNTIME_PLAN_KEY), Some("1"));
        assert!(config.snapshot_contains(PREPARE_RUNTIME_PLAN_KEY));
        assert!(!config.snapshot_contains("APP_SECRET"));
        let declared: Vec<&str> = config
            .service_only_keys()
            .iter()
            .map(|entry| entry.key.as_str())
            .collect();
        assert_eq!(
            declared,
            vec!["APP_SECRET"],
            "promoting the prepare flag to both surfaces retracts its divergence",
        );
    }

    /// A D1 binding over a migrated variables table holding `rows` (a JS
    /// array literal of row objects), answering the executor's introspection
    /// as SQLite answers it. Every statement it runs is pushed onto `log` as
    /// `[sql, params]` JSON, in order. A write, a batch or a `raw()` read
    /// rejects: the config source sends none.
    fn variables_d1(log: &js_sys::Array, rows: &str) -> worker::D1Database {
        let handle = js_sys::Function::new_with_args(
            "log",
            &format!(
                "const rows = {rows};
                const columns = ['id', 'key', 'name', 'description', 'value', 'warning',
                                 'sensitive', 'updated_by', 'created_at', 'updated_at', 'block'];
                const meta = {{ rows_read: 0, rows_written: 0, changes: 0 }};
                return {{
                    prepare(sql) {{
                        const stmt = {{
                            params: [],
                            bind(...params) {{ stmt.params = params; return stmt; }},
                            all() {{
                                log.push(JSON.stringify([sql, stmt.params]));
                                let results = rows;
                                if (sql.includes('pragma_table_info') && sql.includes('pk > 0')) {{
                                    results = [{{ name: 'id' }}];
                                }} else if (sql.includes('pragma_table_info')) {{
                                    results = columns.map((name) => ({{
                                        name,
                                        decl_type: name === 'sensitive' ? 'INTEGER' : 'TEXT',
                                    }}));
                                }}
                                return Promise.resolve({{ success: true, results, meta }});
                            }},
                            run() {{ return Promise.reject(new Error('the config source writes nothing')); }},
                            raw() {{ return Promise.reject(new Error('the config source reads through all()')); }},
                        }};
                        return stmt;
                    }},
                    batch() {{ return Promise.reject(new Error('the config source sends no batch')); }},
                }};"
            ),
        )
        .call1(&wasm_bindgen::JsValue::NULL, log)
        .expect("the fake binding builds");
        wasm_bindgen::JsCast::unchecked_into::<worker::D1Database>(handle)
    }

    /// The rows [`variables_d1`] serves in these tests: two of auth's, one of
    /// email's, and a shared row that belongs to no block.
    const VARIABLES: &str = "[
        { id: 'v1', key: 'WAFER_RUN__AUTH__A', name: '', description: '', value: 'from-d1',
          warning: '', sensitive: 0, updated_by: '', created_at: 't', updated_at: 't',
          block: 'WAFER_RUN__AUTH' },
        { id: 'v2', key: 'WAFER_RUN__AUTH__JWT_SECRET', name: '', description: '',
          value: 'stored-secret', warning: '', sensitive: 1, updated_by: '', created_at: 't',
          updated_at: 't', block: 'WAFER_RUN__AUTH' },
        { id: 'v3', key: 'IMPRESSPRESS__EMAIL__FROM', name: '', description: '',
          value: 'from@d1.test', warning: '', sensitive: 0, updated_by: '', created_at: 't',
          updated_at: 't', block: 'IMPRESSPRESS__EMAIL' },
        { id: 'v4', key: 'WAFER_RUN_SHARED__APP_NAME', name: '', description: '',
          value: 'shared', warning: '', sensitive: 0, updated_by: '', created_at: 't',
          updated_at: 't', block: null }
    ]";

    /// A KV namespace that counts what reaches it and holds nothing.
    #[derive(Default)]
    struct CountingKv {
        gets: std::cell::Cell<usize>,
        writes: std::cell::Cell<usize>,
    }

    #[async_trait::async_trait(?Send)]
    impl impresspress_core::kv::KvBackend for CountingKv {
        async fn get(&self, _key: &str) -> Result<Option<String>, String> {
            self.gets.set(self.gets.get() + 1);
            Ok(None)
        }
        async fn put_with_ttl(&self, _key: &str, _value: &str, _ttl: u64) -> Result<(), String> {
            self.writes.set(self.writes.get() + 1);
            Ok(())
        }
        async fn put(&self, _key: &str, _value: &str) -> Result<(), String> {
            self.writes.set(self.writes.get() + 1);
            Ok(())
        }
        async fn delete(&self, _key: &str) -> Result<(), String> {
            self.writes.set(self.writes.get() + 1);
            Ok(())
        }
    }

    /// The database service both fills hand the config source, over `d1`:
    /// the D1 adapter behind the KV row cache, on a cold isolate.
    fn request_database(d1: worker::D1Database, kv: Arc<CountingKv>) -> Arc<dyn DatabaseService> {
        crate::database::forget_isolate_schema();
        let d1 = crate::database::D1DatabaseService::new(
            d1,
            "DB",
            crate::database::D1QueryCount::new(),
            1000,
        );
        Arc::new(crate::kv_cached_db::KvCachedD1DatabaseService::new(
            Arc::new(d1),
            kv,
        ))
    }

    fn statements(log: &js_sys::Array) -> Vec<String> {
        log.iter()
            .map(|entry| entry.as_string().expect("a logged statement"))
            .collect()
    }

    /// What a cold build's config resolution sends D1: the variables table's
    /// primary key and columns (a cold isolate's schema cache), then ONE
    /// unfiltered, bounded read of the whole table —
    /// for every config-declaring block, and nothing for a block declaring no
    /// keys. Nothing reaches KV: the whole-table shape is deliberately not a
    /// cacheable one.
    ///
    /// The statements are pinned verbatim; they are the ones the
    /// Cloudflare-only `D1ConfigSource` sent before the source moved into
    /// core.
    #[wasm_bindgen_test]
    async fn a_cold_builds_config_resolution_sends_d1_one_unfiltered_variables_read() {
        let log = js_sys::Array::new();
        let kv = Arc::new(CountingKv::default());
        let source = request_config_source(
            request_database(variables_d1(&log, VARIABLES), kv.clone()),
            HashMap::new(),
            Arc::default(),
        );

        let auth = source
            .load_for_block(
                "wafer-run/auth",
                &[
                    wafer_block::ConfigVar::new("WAFER_RUN__AUTH__A", "doc", ""),
                    wafer_block::ConfigVar::new("WAFER_RUN_SHARED__APP_NAME", "doc", "").optional(),
                ],
            )
            .await
            .expect("auth resolves");
        assert_eq!(auth.get("WAFER_RUN__AUTH__A"), Some("from-d1"));
        assert_eq!(
            auth.get("WAFER_RUN_SHARED__APP_NAME"),
            None,
            "a shared row belongs to no block"
        );
        let email = source
            .load_for_block(
                "impresspress/email",
                &[wafer_block::ConfigVar::new(
                    "IMPRESSPRESS__EMAIL__FROM",
                    "doc",
                    "",
                )],
            )
            .await
            .expect("email resolves");
        assert_eq!(email.get("IMPRESSPRESS__EMAIL__FROM"), Some("from@d1.test"));
        source
            .load_for_block("wafer-run/cors", &[])
            .await
            .expect("a block declaring nothing resolves");

        assert_eq!(
            statements(&log),
            [
                r#"["SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk",["impresspress__admin__variables"]]"#,
                r#"["SELECT name, type AS decl_type FROM pragma_table_info(?1) ORDER BY cid",["impresspress__admin__variables"]]"#,
                r#"["SELECT * FROM \"impresspress__admin__variables\" ORDER BY \"id\" ASC LIMIT ?",[10000]]"#,
            ],
        );
        assert_eq!(kv.gets.get(), 0, "no KV read for the variables snapshot");
        assert_eq!(kv.writes.get(), 0, "no KV write for the variables snapshot");
    }

    /// A warm request's config source is new — its own database handle and
    /// overlay — but reads through the runtime's variables snapshot, so a
    /// block config it resolves sends D1 nothing: the build's read serves it.
    /// That is why `warm_request_services` takes the cached runtime's snapshot
    /// rather than letting each source keep its own memo.
    #[wasm_bindgen_test]
    async fn a_warm_requests_source_reads_through_the_runtimes_snapshot() {
        let snapshot = Arc::new(VariablesSnapshot::new(RetryMode::Backoff));
        let declared = [wafer_block::ConfigVar::new("WAFER_RUN__AUTH__A", "doc", "")];

        let build_log = js_sys::Array::new();
        let build_source = request_config_source(
            request_database(variables_d1(&build_log, VARIABLES), Arc::default()),
            HashMap::new(),
            snapshot.clone(),
        );
        build_source
            .load_for_block("wafer-run/auth", &declared)
            .await
            .expect("the build resolves");
        assert_eq!(statements(&build_log).len(), 3, "the build reads the table");

        let warm_log = js_sys::Array::new();
        let warm_source = request_config_source(
            request_database(variables_d1(&warm_log, VARIABLES), Arc::default()),
            HashMap::new(),
            snapshot,
        );
        let auth = warm_source
            .load_for_block("wafer-run/auth", &declared)
            .await
            .expect("the warm request resolves");
        assert_eq!(auth.get("WAFER_RUN__AUTH__A"), Some("from-d1"));
        assert!(
            statements(&warm_log).is_empty(),
            "a warm request reads the runtime's snapshot, not D1: {:?}",
            statements(&warm_log)
        );
    }

    /// The overlay wiring, end to end: what the warm fill assembles — the
    /// Worker's secret binding and a consumer's request config — wins over
    /// the D1 row for the same key, through the source the fill builds.
    #[wasm_bindgen_test]
    async fn the_overlay_the_fill_assembles_wins_over_the_d1_row() {
        let jwt = impresspress_core::blocks::auth::JWT_SECRET_KEY;
        let mut environment = empty_environment();
        environment.set_jwt_secret_for_test("bound-secret");
        let request_config =
            HashMap::from([("WAFER_RUN__AUTH__A".to_string(), "from-request".to_string())]);
        let (_config, overlay) =
            request_config_surfaces(&environment, &HashMap::new(), &request_config);

        let log = js_sys::Array::new();
        let source = request_config_source(
            request_database(variables_d1(&log, VARIABLES), Arc::default()),
            overlay,
            Arc::default(),
        );
        let auth = source
            .load_for_block(
                "wafer-run/auth",
                &[
                    wafer_block::ConfigVar::new(jwt, "doc", ""),
                    wafer_block::ConfigVar::new("WAFER_RUN__AUTH__A", "doc", ""),
                ],
            )
            .await
            .expect("auth resolves");
        assert_eq!(
            auth.get(jwt),
            Some("bound-secret"),
            "the Worker's secret binding beats the stored row"
        );
        assert_eq!(
            auth.get("WAFER_RUN__AUTH__A"),
            Some("from-request"),
            "a consumer's request config beats the stored row"
        );
    }
}
