//! Building a Cloudflare runtime: the services, both config surfaces, and the
//! three post-build funnels.
//!
//! [`build_runtime`] wires this request's D1/KV/R2/crypto/network/logger
//! services into an `ImpresspressBuilder`, hands both config surfaces over in
//! one `RuntimeConfig::install` call, runs the consumer's registrations and
//! returns a [`BuiltRuntime`] — built, but neither sealed nor booted.
//!
//! Sealing and booting is `impresspress_core::builder::boot`, reached through
//! exactly three named funnels, one per Cloudflare path:
//! [`boot_deploy_runtime`], [`boot_dynamic_request_runtime`] and
//! [`boot_prepared_runtime`]. They differ in where WRAP grants come from, which
//! `BootHooks` impl runs, and which `InitPolicy` applies; each says why at its
//! own definition. Picking a funnel is picking a function, never composing a
//! `(grants, hooks, policy)` triple at a call site — the three paths are not
//! interchangeable.

use std::{collections::HashMap, sync::Arc};

use impresspress_core::builder::{
    BootReport, GrantSource, ImpresspressBuilder, InitPolicy, RuntimeConfig,
    PREPARE_RUNTIME_PLAN_KEY,
};
use wafer_core::interfaces::{
    database::service::DatabaseService, storage::service::StorageService,
};

use crate::{
    boot_hooks::{CfDeployBootHooks, CfRequestBootHooks, PreparedPlanBootHooks},
    config_source,
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
}

/// Run the post-build lifecycle over the **deploy funnel's** dynamically built
/// runtime (`/_deploy/init`, reached by a production deploy and by local
/// `impresspress serve --target cloudflare`).
///
/// Everything that used to be three hand-copied statement sequences — load the
/// D1 grants, seal, init admin, init the rest, inject the grants into the
/// storage block — is now `impresspress_core::builder::boot`'s ordering, with
/// this function supplying only the Cloudflare-specific answers:
///
/// - **Grants come from D1.** `runtime_cache::get_or_build`'s stored build
///   remembered to load them and `hydrate_transient_dynamic_runtime` had to
///   repeat the call; nothing but a comment recorded that the prepared path's
///   omission was deliberate. `GrantSource` makes it an argument the compiler
///   demands.
/// - **The seeding hook runs here and only here.** Seeding — structural
///   `block_settings` rows and auto-generated secrets — is a deploy-time
///   mutation, and this is the deploy. Its cost is two D1 reads plus, on a
///   first deploy only, one insert per seeded block.
/// - **`InitPolicy::Reported`**, because a deploy funnel's whole product is
///   the per-step report; it captures every outcome instead of aborting on
///   the first failure.
pub(crate) async fn boot_deploy_runtime(built: &mut BuiltRuntime) -> Result<BootReport, String> {
    let hooks = CfDeployBootHooks {
        db: built.db.clone(),
        block_settings_handle: built.block_settings_handle.clone(),
        config: request_services::config_proxy(),
        seed_defaults: impresspress_core::blocks::block_enabled_defaults(),
    };
    boot_dynamic(built, &hooks, InitPolicy::Reported).await
}

/// Run the same lifecycle over a **dynamically built request-path** runtime —
/// the stored per-isolate build and the request-local transient one.
///
/// Same grant source as the deploy funnel, and a deliberately different hook:
/// [`CfRequestBootHooks`] re-reads `block_settings` and republishes it, and
/// **writes nothing**. A request is not a deploy; see that type for the three
/// failure modes a seeding hook has here that it does not have in the funnel.
///
/// - **`InitPolicy::Strict`**, so a failure fails the build rather than
///   publishing a Wafer with a failed lazy-init slot (concurrent requests
///   would then wait on one another's init future, which is not a valid
///   execution model for a request-isolated platform).
/// - **A hook failure is fatal, deliberately.** `Strict` couples the two, and
///   for this hook that is the right coupling rather than an accident of the
///   policy: the only way [`CfRequestBootHooks`] can fail is a genuine
///   operational failure of the same `block_settings` read `build_runtime`
///   already performed and already treats as fatal a few hundred lines above
///   (a missing table is `Ok(empty)` inside `DatabaseService::list`, never an
///   `Err`). Serving requests off a half-known enablement map is exactly what
///   that read fails closed to prevent, and the hook is the authoritative
///   post-admin-init read of it.
pub(crate) async fn boot_dynamic_request_runtime(
    built: &mut BuiltRuntime,
) -> Result<BootReport, String> {
    let hooks = CfRequestBootHooks {
        db: built.db.clone(),
        block_settings_handle: built.block_settings_handle.clone(),
        config: request_services::config_proxy(),
    };
    boot_dynamic(built, &hooks, InitPolicy::Strict).await
}

/// The shared body of the two dynamic paths: D1 grants, the caller's hook,
/// the caller's policy. Private, so the choice of hook is made by picking one
/// of the two functions above rather than by passing an argument — the two
/// paths are not interchangeable and the type system now says so.
async fn boot_dynamic(
    built: &mut BuiltRuntime,
    hooks: &dyn impresspress_core::builder::BootHooks,
    policy: InitPolicy,
) -> Result<BootReport, String> {
    let db = built.db.clone();
    request_services::scope(built.services.clone(), async {
        impresspress_core::builder::boot(
            &mut built.wafer,
            hooks,
            GrantSource::Database(&db),
            policy,
        )
        .await
        .map_err(|error| format!("boot: {error}"))
    })
    .await
}

/// Run the same lifecycle over a runtime **hydrated from a verified prepared
/// plan**, which is the one path that answers both questions differently.
///
/// - Grants are `PreInstalled`: `ImpresspressBuilder::apply_prepared_plan`
///   copies `structure.wrap_grants` and `structure.deployment_wrap_grants` out
///   of the plan into the builder, and `build()` registers them before this
///   function is reached. Reading D1 here would answer the same question a
///   second time, over the network.
/// - The seed hook is [`PreparedPlanBootHooks`], a written no-op. Even the
///   read-only [`CfRequestBootHooks`] the two dynamic request builds carry
///   would be wrong here: they re-read `block_settings` because their own
///   pre-`build()` read predates admin's migration, whereas this path takes
///   its settings from the plan and performs **no** D1 structural read at
///   all. Adding one would put back exactly what the plan exists to remove
///   (~132us hydration versus a measured up-to-8.4s dynamic build), on every
///   cold isolate, to re-derive state the deploy funnel already sealed into
///   the plan.
pub(crate) async fn boot_prepared_runtime(built: &mut BuiltRuntime) -> Result<BootReport, String> {
    request_services::scope(built.services.clone(), async {
        impresspress_core::builder::boot(
            &mut built.wafer,
            &PreparedPlanBootHooks,
            GrantSource::PreInstalled(
                "ImpresspressBuilder::apply_prepared_plan installs the verified \
                 plan's wrap_grants and deployment_wrap_grants before build()",
            ),
            InitPolicy::Strict,
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
/// `/_deploy/init` builds this runtime BEFORE any migration has run, so the
/// one eager D1 read in this function — `block_settings` → default map — MUST
/// tolerate a not-yet-created table. A non-tolerant eager read would error out
/// and deadlock first deploys before migrations can create the tables. The
/// same tolerance is owed by the `wrap_grants` read the callers make, which is
/// now `GrantSource::Database` inside `impresspress_core::builder::boot`
/// rather than a call in this crate.
///
/// Both reads own that tolerance explicitly, through
/// `DatabaseService::schema_table_exists`. Neither may go back to leaning on
/// `DbExec::list`'s table-existence guard, because STRICT_SCHEMA — which the
/// generated `wrangler.toml` sets on every deploy, and which this crate now
/// applies from construction rather than only at `Init` — disables exactly
/// that guard: `table_present_for_op` returns `Ok(true)` unconditionally in
/// strict mode, so the SELECT reaches the backend and gets "no such table".
/// `block_settings`' own
/// `read_rows` documents the probe; `repro_cold_start_under_strict_schema`
/// pins it.
///
/// Every other read on this pre-migration window is already safe by ordering,
/// not by luck: `D1ConfigSource` is reached only through
/// `ConfigSource::load_for_block`, which short-circuits for a block declaring
/// no config keys — and `boot` initializes `impresspress/admin`, which
/// declares none, before any block that does, so the `variables` table exists
/// by the time anything reads it. `migration_helper::read_state` reads the
/// config snapshot, not the database.
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
    //    env-config pre-load is gone; D1ConfigSource resolves declared
    //    config keys later, on first init of a block that declares any,
    //    from ONE unfiltered read of the whole variables table grouped by
    //    `block` in memory. block_settings still needs an eager load
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

    // 5. ConfigSource: D1-backed, resolving each block against one lazily
    //    fetched unfiltered snapshot of the variables table. The overlay layers
    //    the Env-owned keys (PROTECTED_ENV_KEYS secrets, BUILDER_WORKER_VAR_KEYS
    //    vars) on top of D1 rows so they never need to be mirrored into the
    //    variables table.
    let cfg_source: Arc<dyn wafer_run::ConfigSource> = Arc::new(
        config_source::D1ConfigSource::with_overlay(db.clone(), overlay),
    );
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
    })
}

/// Every value the structural config pass takes out of `worker::Env`, read in
/// one place so the pass itself is a pure function.
///
/// The split exists for testability and pays for itself: a `wasm_bindgen_test`
/// has no `worker::Env`, so before it the only thing a test could do about the
/// two config surfaces was build its own `RuntimeConfig` and assert that
/// `both()` works — which is a `builder::config` unit test wearing a
/// Cloudflare hat, and would not have noticed a structural key here being
/// switched to `service_only`. The reads themselves are now
/// [`CfEnvironment::capture`]'s, so the split is between "what the environment
/// holds" and "what this build makes of it".
struct StructuralConfigInputs {
    /// [`PROTECTED_ENV_KEYS`] that are actually bound as worker secrets.
    secrets: Vec<(&'static str, String)>,
    /// [`BUILDER_WORKER_VAR_KEYS`], for those actually bound as worker vars.
    worker_vars: Vec<(&'static str, String)>,
    /// `BlockSettings::to_config_json` for this build.
    block_settings_json: String,
    /// Set only by the `/_deploy/init` funnel.
    run_migrations: bool,
}

/// Split [`StructuralConfigInputs`] out of an already-captured environment.
///
/// STRICT_SCHEMA (`WAFER_RUN__DATABASE__STRICT_SCHEMA`) is a worker var
/// (wrangler.toml `[vars]`), not a row in the D1 `variables` table: it is a
/// deploy-time decision, not an admin-editable runtime toggle. The shared
/// `wafer-run/database` block declares it and reads it from its
/// `lifecycle(Init)` config, which the `ConfigSource` resolves — so it is
/// layered over the D1 rows with the other Env-owned keys (see
/// [`structural_runtime_config`]), and `Init` calls `set_strict_schema` on the
/// DB service before it serves any query. Absent var ⇒ the block's declared
/// default, `false`.
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

/// Build the ConfigService map. After dropping the D1 env_vars pre-load, this
/// map only carries:
///
/// - `PROTECTED_ENV_KEYS` pulled from `worker::Env` bindings (e.g. the JWT
///   secret managed via `wrangler secret put`). These never live in D1.
/// - the Worker vars (`BUILDER_WORKER_VAR_KEYS`). CSP/CORS additions are
///   builder-time input: the middleware flow is constructed before lazy block
///   config exists, so they must be present in ConfigService before
///   `builder.build()`. STRICT_SCHEMA rides along as an Env-owned key; the
///   block that applies it reads it from the `ConfigSource` overlay instead.
/// - the synthetic `BLOCK_SETTINGS_CONFIG_KEY` → JSON entry so consumer blocks
///   (userportal, migration_helper) can read block enablement / migration
///   state via `ctx.config_get` without a separate D1 query per request.
/// - `IMPRESSPRESS_RUN_MIGRATIONS`, on the deploy funnel only. Migrations on
///   CF run exclusively through `/_deploy/init` — a production deploy and
///   local `impresspress serve --target cloudflare` both POST it (see
///   `cli/flows/embed_cloudflare.rs`). Request-path builds never migrate;
///   there is no worker env var to honor here, unlike native `impresspress_server::build_native_runtime`'s
///   `--run-migrations` flag, which is a real per-boot CLI choice.
///
/// Both surfaces are assembled in ONE `RuntimeConfig`: the async
/// `ConfigService` map and the synchronous `ctx.config_get` snapshot used to be
/// two literals kept in step by a comment. Every key here is written with
/// `both`, so it lands on each; the one deliberate divergence
/// (`service_only`) belongs to request config, which
/// [`add_request_config`] adds afterwards and which carries its reason as an
/// argument.
///
/// Returns the config alongside the `ConfigSource` overlay: every Env-owned
/// key — the secrets and the worker vars — is layered over the D1 `variables`
/// rows, so a key the Worker environment owns resolves from that environment
/// for a block's `lifecycle(Init)` config too, and never has to be mirrored
/// into the table. The one such key a block declares today is STRICT_SCHEMA
/// (`wafer-run/database`); the others are read by no block's `Init`.
fn structural_runtime_config(
    inputs: StructuralConfigInputs,
) -> (RuntimeConfig, HashMap<String, String>) {
    let mut config = RuntimeConfig::new();
    let mut overlay: HashMap<String, String> = HashMap::new();
    for (key, value) in inputs.secrets {
        config.both(key, value.clone());
        overlay.insert(key.to_string(), value);
    }
    for (key, value) in inputs.worker_vars {
        config.both(key, value.clone());
        overlay.insert(key.to_string(), value);
    }
    config.both(
        impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY,
        inputs.block_settings_json,
    );
    if inputs.run_migrations {
        config.both(impresspress_core::migration_helper::RUN_MIGRATIONS_KEY, "1");
    }
    (config, overlay)
}

/// The config surfaces a **warm** request's services read through: the map
/// behind this request's `ConfigService`, and the `ConfigSource` overlay
/// layered over the D1 `variables` rows.
///
/// There is no third surface here. A warm request has no snapshot to write —
/// the isolate-cached runtime already carries one, and one request's values
/// must never be baked into it — so this is a pair of maps rather than a
/// [`RuntimeConfig`], whose whole purpose is to couple the async surface to a
/// snapshot.
///
/// Every key [`CfEnvironment`] owns is taken from THIS request's capture and
/// from nowhere else; the cached runtime's structural snapshot contributes only
/// the keys the environment does *not* own, which today is the D1-derived
/// `IMPRESSPRESS_BLOCK_SETTINGS`. A binding removed since the runtime was built
/// is therefore absent from the map by construction.
///
/// That last part used to be a removal branch. The previous code cloned the
/// whole snapshot and then deleted `PROTECTED_ENV_KEYS` and
/// `BUILDER_WORKER_VAR_KEYS` back out of it by name when their bindings were
/// gone — the right answer for the keys somebody had listed, and no answer at
/// all for `WAFER_RUN__DATABASE__STRICT_SCHEMA`, which was on neither list: a
/// deployment that dropped that var kept serving with strict schema on until
/// something else forced a rebuild. Deriving the owned set from the struct that
/// captured it removes the class instead of the instance.
fn request_config_surfaces(
    environment: &CfEnvironment,
    structural_snapshot: &HashMap<String, String>,
    request_config: &HashMap<String, String>,
) -> (HashMap<String, String>, HashMap<String, String>) {
    let mut config_map: HashMap<String, String> = structural_snapshot
        .iter()
        .filter(|(key, _)| !CfEnvironment::owns_config_key(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    config_map.extend(environment.config_map());

    let mut overlay = HashMap::new();
    extend_with_request_config(&mut config_map, &mut overlay, request_config);
    // Every Env-owned key is layered over the D1 `variables` rows, as the cold
    // fill does it: a block's `lifecycle(Init)` config then resolves it from
    // the environment this request sees, and it never has to be mirrored into
    // that table.
    overlay.extend(environment.config_map());
    (config_map, overlay)
}

/// Construct concrete services for one warm request without performing I/O.
/// Binding lookup and service allocation are cheap; D1/KV/R2 operations stay
/// lazy until a block actually calls them.
///
/// The cold path's equivalent is [`build_runtime`], which builds the same six
/// services and then a Wafer around them. Both take their Env-derived config
/// from the same [`CfEnvironment`] method, so the two paths cannot disagree
/// about what the environment currently says.
pub(crate) fn warm_request_services(
    env: &worker::Env,
    environment: &CfEnvironment,
    queries: &crate::database::D1QueryCount,
    structural_snapshot: &HashMap<String, String>,
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
    let config_source: Arc<dyn wafer_run::ConfigSource> = Arc::new(
        config_source::D1ConfigSource::with_overlay(db.clone(), overlay),
    );

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

/// Add consumer-declared request config to the async service surfaces only.
/// Both fills call this: the cold one through [`add_request_config`], the warm
/// one through [`request_config_surfaces`].
fn extend_with_request_config(
    config: &mut HashMap<String, String>,
    overlay: &mut HashMap<String, String>,
    request_config: &HashMap<String, String>,
) {
    for (key, value) in request_config {
        // Framework-owned protected/builder keys come from Env and must not be
        // shadowed by a consumer-provided duplicate.
        if CfEnvironment::owns_config_key(key) {
            continue;
        }
        config.insert(key.clone(), value.clone());
        overlay.insert(key.clone(), value.clone());
    }
}

/// Add consumer-declared request config to a runtime's async service surface
/// and to the `ConfigSource` overlay, and to neither snapshot: one request's
/// secret must never be baked into the isolate-cached synchronous surface.
///
/// `RuntimeConfig::service_only` takes that reason as an argument, so the
/// divergence is stated where it is created rather than inferred from a
/// missing line.
///
/// A key the structural pass has already installed is skipped outright, and
/// that rule is derived rather than listed: `service_only` REMOVES its key
/// from the snapshot, so a collision does not shadow the structural value, it
/// deletes it. Before `RuntimeConfig` the snapshot was cloned ahead of this
/// merge and a collision was harmless by construction; the two name lists
/// `extend_with_request_config` checks (`PROTECTED_ENV_KEYS`,
/// `BUILDER_WORKER_VAR_KEYS`) cover the Env-owned families but not the keys
/// this pass installs itself — `IMPRESSPRESS_BLOCK_SETTINGS`,
/// `IMPRESSPRESS_RUN_MIGRATIONS`, `WAFER_RUN__DATABASE__STRICT_SCHEMA`. Asking
/// the `RuntimeConfig` what it already holds covers all of them, and covers
/// the next structural key without anyone remembering to add it to a list.
fn add_request_config(
    config: &mut RuntimeConfig,
    overlay: &mut HashMap<String, String>,
    request_config: &HashMap<String, String>,
) {
    let consumer_owned: HashMap<String, String> = request_config
        .iter()
        .filter(|(key, _)| !config.snapshot_contains(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut service_map = HashMap::new();
    extend_with_request_config(&mut service_map, overlay, &consumer_owned);
    for (key, value) in service_map {
        config.service_only(
            key,
            value,
            "consumer request config is request-current: the isolate-cached \
             snapshot must not carry one request's values into the next",
        );
    }
}

/// Promote the one non-secret request value lifecycle code must read from the
/// synchronous config snapshot while preparing an isolated deploy candidate.
/// Runs after [`add_request_config`], which has already put it on the async
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
    ///
    /// `WAFER_RUN__DATABASE__STRICT_SCHEMA` is the case that used to survive.
    /// The old code cloned the cached runtime's snapshot and then deleted
    /// `PROTECTED_ENV_KEYS` and `BUILDER_WORKER_VAR_KEYS` back out of it by
    /// name; strict schema was on neither list, so a deployment that dropped
    /// the var kept serving with strict schema on — skipping the table-exists
    /// probe and the lazy column-add — until something else forced a rebuild.
    #[wasm_bindgen_test]
    fn a_removed_worker_binding_cannot_survive_in_a_warm_requests_config() {
        let strict_schema = wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY;
        let jwt = impresspress_core::blocks::auth::JWT_SECRET_KEY;
        let block_settings = impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY;

        // What the isolate-cached runtime was built with.
        let snapshot = HashMap::from([
            (jwt.to_string(), "old-secret".to_string()),
            (strict_schema.to_string(), "1".to_string()),
            (
                impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY.to_string(),
                "https://old.test".to_string(),
            ),
            (block_settings.to_string(), "{}".to_string()),
        ]);

        // What the Worker's bindings say NOW: the secret rotated, strict schema
        // and CORS were removed.
        let mut environment = empty_environment();
        environment.set_jwt_secret_for_test("new-secret");

        let (config, overlay) = request_config_surfaces(&environment, &snapshot, &HashMap::new());

        assert_eq!(
            config.get(jwt).map(String::as_str),
            Some("new-secret"),
            "a rotated secret must reach this request",
        );
        assert!(
            !config.contains_key(strict_schema),
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
        assert!(
            !overlay.contains_key(strict_schema),
            "a removed worker var must not reach a block's Init config either",
        );
    }

    /// The warm-request overlay carries the worker vars this request's
    /// environment binds, as the cold one does. `wafer-run/database` reads
    /// STRICT_SCHEMA from its `lifecycle(Init)` config — resolved through the
    /// `ConfigSource` this overlay feeds, not from the `ConfigService` map — and
    /// a block first initialised on a warm request resolves it here.
    #[wasm_bindgen_test]
    fn a_bound_worker_var_reaches_a_warm_requests_config_source_overlay() {
        let strict_schema = wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY;
        let mut environment = empty_environment();
        environment.set_strict_schema_for_test("true");

        let (_config, overlay) =
            request_config_surfaces(&environment, &HashMap::new(), &HashMap::new());

        assert_eq!(
            overlay.get(strict_schema).map(String::as_str),
            Some("true"),
            "a bound STRICT_SCHEMA var must reach the database block's Init config",
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

    /// The structural half of Cloudflare's fill: every key `build_runtime`
    /// installs reaches BOTH surfaces with the same value.
    ///
    /// Goes through the production assembly [`structural_runtime_config`]
    /// rather than building a `RuntimeConfig` of its own — an earlier version
    /// of this test did the latter and was a `builder::config` unit test
    /// wearing a Cloudflare hat: it asserted that `both()` works, which
    /// `config.rs` already pins, and would have kept passing if a key here
    /// were switched to `service_only` and dropped off the isolate-cached
    /// snapshot. `build_runtime` itself needs a `worker::Env` no wasm test can
    /// produce, which is why the env reads are
    /// [`CfEnvironment::capture`](crate::environment::CfEnvironment::capture)'s
    /// and the assembly is this pure function
    /// ([`structural_config_inputs`] only selects from an already-captured
    /// environment and reads nothing itself).
    #[wasm_bindgen_test]
    fn structural_keys_reach_both_config_surfaces() {
        let strict_schema = wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY;
        let (config, overlay) = structural_runtime_config(StructuralConfigInputs {
            secrets: vec![(
                impresspress_core::blocks::auth::JWT_SECRET_KEY,
                "jwt".to_string(),
            )],
            worker_vars: vec![
                (
                    impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY,
                    "*".to_string(),
                ),
                (
                    impresspress_core::config_vars::CSP_DIRECTIVES_KEY,
                    "default-src 'self'".to_string(),
                ),
                (strict_schema, "true".to_string()),
            ],
            block_settings_json: "{}".to_string(),
            run_migrations: true,
        });

        for (key, value) in [
            (impresspress_core::blocks::auth::JWT_SECRET_KEY, "jwt"),
            (
                impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY,
                "*",
            ),
            (
                impresspress_core::config_vars::CSP_DIRECTIVES_KEY,
                "default-src 'self'",
            ),
            (strict_schema, "true"),
            (impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY, "{}"),
            (impresspress_core::migration_helper::RUN_MIGRATIONS_KEY, "1"),
        ] {
            assert_eq!(
                config.service_get(key),
                Some(value),
                "{key} on the service map"
            );
            assert!(config.snapshot_contains(key), "{key} on the snapshot");
        }
        assert!(
            config.service_only_keys().is_empty(),
            "no structural key diverges",
        );
        // Every Env-owned key is also layered over the D1 `variables` rows the
        // `ConfigSource` resolves against. STRICT_SCHEMA is the one that
        // matters there: `wafer-run/database` reads it from its Init config,
        // not from either surface above, so without the overlay a Worker var
        // turning it on would be overridden by the block's declared `false`.
        for (key, value) in [
            (impresspress_core::blocks::auth::JWT_SECRET_KEY, "jwt"),
            (
                impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY,
                "*",
            ),
            (strict_schema, "true"),
        ] {
            assert_eq!(overlay.get(key).map(String::as_str), Some(value), "{key}");
        }
    }

    /// The `run_migrations` flag is the funnel's alone: a request-path build
    /// passes `false` and the key must be absent, not present-and-empty —
    /// `migration_helper` gates on `== Some("1")`, but an absent key is also
    /// what keeps the two builds' config identity distinct.
    #[wasm_bindgen_test]
    fn a_request_path_build_carries_no_run_migrations_key() {
        let (config, _overlay) = structural_runtime_config(StructuralConfigInputs {
            secrets: Vec::new(),
            worker_vars: Vec::new(),
            block_settings_json: "{}".to_string(),
            run_migrations: false,
        });

        assert_eq!(
            config.service_get(impresspress_core::migration_helper::RUN_MIGRATIONS_KEY),
            None,
        );
        assert!(!config.snapshot_contains(impresspress_core::migration_helper::RUN_MIGRATIONS_KEY),);
    }

    /// A consumer request key that collides with a framework structural key
    /// must not DELETE that key from the snapshot.
    ///
    /// `RuntimeConfig::service_only` is a `remove` on the snapshot surface, so
    /// a collision is not "the request value shadows the structural one" — it
    /// is "the structural one is gone". On `main` the snapshot was cloned
    /// before the request merge and a collision left the structural value
    /// intact; the guard list that replaced that accident covered
    /// `PROTECTED_ENV_KEYS` and `BUILDER_WORKER_VAR_KEYS` but not the three
    /// keys the structural pass installs itself. Any key the structural pass
    /// already put on the snapshot is now framework-owned, whatever its name.
    #[wasm_bindgen_test]
    fn request_config_cannot_delete_a_structural_key_from_the_snapshot() {
        let structural = [
            impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY,
            impresspress_core::migration_helper::RUN_MIGRATIONS_KEY,
            wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY,
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
}
