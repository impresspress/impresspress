//! The native impresspress server: the boot body the `impresspress` binary
//! runs, as a library a downstream native site runs too.
//!
//! [`run`] constructs the database service, seeds the admin variables /
//! block_settings tables pre-wafer through the shared `impresspress_core`
//! seeders, builds the WAFER runtime, registers the HTTP listener, and runs
//! the `serve_until_shutdown` loop. A consumer adds its own blocks, config
//! and flows through [`AppHooks`] — the same two hooks
//! `impresspress_cloudflare::run` takes — and names the flow the listener
//! dispatches to.
//!
//! The runtime construction itself lives in [`build_native_runtime`], shared
//! with the integration tests so the runtime they exercise is the one the
//! binary builds.

use std::{collections::HashMap, error::Error, path::Path, sync::Arc};

use anyhow::{anyhow, Context};
use impresspress_core::builder::{self, ImpresspressBuilder};
use impresspress_native::{
    collect_app_env_vars, init_tracing, load_dotenv, register_http_listener,
    register_observability_hooks, serve_until_shutdown, InfraConfig,
};
use impresspress_password::pepper::{self as password_pepper, PasswordPeppers};
use wafer_core::interfaces::{
    database::service::DatabaseService, storage::service::StorageService,
};
use wafer_run::Wafer;

/// The hook that configures the [`ImpresspressBuilder`] before `build()`:
/// the consumer's blocks, routes and block settings.
pub type RegisterBlocks =
    Box<dyn FnOnce(ImpresspressBuilder) -> Result<ImpresspressBuilder, Box<dyn Error>>>;

/// The hook that runs on the built runtime before boot: the consumer's
/// post-build blocks, block configs and flows. It is handed the platform
/// storage service.
pub type RegisterPostBuild =
    Box<dyn FnOnce(&mut Wafer, Arc<dyn StorageService>) -> Result<(), Box<dyn Error>>>;

/// What a consumer adds to the native runtime, in the shape
/// `impresspress_cloudflare::run` takes: `register_blocks` runs on the
/// builder after the platform services, config and block settings are
/// installed, and `register_post_build` on the built runtime.
pub struct AppHooks {
    /// Runs on the builder, just before `build()`.
    pub register_blocks: RegisterBlocks,
    /// Runs on the built runtime, before the listener is registered and the
    /// runtime boots.
    pub register_post_build: RegisterPostBuild,
}

impl AppHooks {
    /// No consumer additions: the runtime impresspress itself serves.
    pub fn none() -> Self {
        Self {
            register_blocks: Box::new(Ok),
            register_post_build: Box::new(|_, _| Ok(())),
        }
    }
}

/// The flow impresspress dispatches all HTTP traffic through
/// (`crates/impresspress-core/src/flows/site_main.rs`).
pub const IMPRESSPRESS_LISTENER_FLOW: &str = "site-main";

mod declared_keys;
pub use declared_keys::filter_to_declared_keys;

/// Boot the native server end-to-end and serve until shutdown.
///
/// `listener_flow` is the flow the HTTP listener dispatches every request to
/// ([`IMPRESSPRESS_LISTENER_FLOW`] for impresspress itself); `hooks` add the
/// consumer's blocks and flows.
///
/// `run_migrations` mirrors `impresspress serve --run-migrations`. When `true`
/// the boot path stamps `IMPRESSPRESS_RUN_MIGRATIONS=1` into the config
/// snapshot directly (so [`migration_helper::apply_if_blessed`] sees it),
/// instead of the prior `std::env::set_var` smuggle. Rust 2024 makes
/// process-env mutation `unsafe`, and the smuggle leaked into any child
/// process the boot path might spawn — neither was the right channel.
pub async fn run(
    repo_root: &Path,
    run_migrations: bool,
    listener_flow: &str,
    hooks: AppHooks,
) -> anyhow::Result<()> {
    // 1. Load .env file (before reading any env vars). Anchored to
    // `repo_root` so the boot path doesn't depend on the process cwd —
    // mutating cwd globally would leak into anything else this binary
    // (or a future caller) spawns.
    load_dotenv(repo_root);

    // 2. Initialize tracing / logging
    let log_format = std::env::var("IMPRESSPRESS_LOG_FORMAT").unwrap_or_else(|_| "text".into());
    init_tracing(&log_format).context("initialize tracing subscriber")?;
    tracing::info!("impresspress starting (Rust/WAFER runtime)");

    // 3. Read infrastructure config from IMPRESSPRESS_* env vars
    let infra = InfraConfig::from_env();
    tracing::info!(
        listen = %infra.listen,
        db = %infra.db_type,
        db_path = %infra.db_path,
        storage = %infra.storage_type,
        "infrastructure config loaded"
    );

    // 4. Collect app config vars from env (every key carrying `__`; the
    // declared ones are seeded into the variables table below).
    let app_env = collect_app_env_vars();

    // 4b. The password pepper, straight from the process environment into the
    // crypto service and nowhere else — see `password_peppers_from_env`.
    let password_peppers = password_peppers_from_env(|key| std::env::var(key).ok())?;

    // 5. Construct the platform database service up front. Native seeds the
    //    variables / block_settings tables BEFORE the wafer exists because its
    //    immutable crypto service + config snapshot need the JWT secret and the
    //    seeded values at `build()` time — exactly like the Cloudflare target
    //    reads its config pre-build. Boot then runs through the shared
    //    `impresspress_core::builder::boot` funnel (below), so the post-admin-init
    //    seed hook is a no-op. The same `Arc` is handed to the builder, so
    //    seeding and the runtime share one connection/pool.
    let database = impresspress_native::make_database_service(
        &infra.db_type,
        &infra.db_path,
        infra.db_url.as_deref(),
    )
    .await
    .context("construct database service")?;

    // 5b-7b. Seed, load, and build the runtime (shared with the tests).
    let wafer = start_native(
        &infra,
        database,
        &app_env,
        password_peppers,
        run_migrations,
        listener_flow,
        hooks,
    )
    .await?;

    // 13. Wait for shutdown signal, then graceful shutdown
    serve_until_shutdown(&wafer)
        .await
        .context("await shutdown signal")?;
    tracing::info!("impresspress shutdown complete");

    Ok(())
}

/// Build the runtime ([`build_native_runtime`]), point the HTTP listener at
/// `listener_flow`, boot it and bind its socket: everything [`run`] does
/// between reading its environment and serving until shutdown. The runtime
/// it returns is serving on `infra.listen`.
pub async fn start_native(
    infra: &InfraConfig,
    database: Arc<dyn DatabaseService>,
    app_env: &HashMap<String, String>,
    password_peppers: PasswordPeppers,
    run_migrations: bool,
    listener_flow: &str,
    hooks: AppHooks,
) -> anyhow::Result<Arc<Wafer>> {
    let mut wafer = build_native_runtime(
        infra,
        database,
        app_env,
        password_peppers,
        run_migrations,
        hooks,
    )
    .await?;

    // 8. Native-only: register http-listener on the consumer's flow.
    register_http_listener(&mut wafer, &infra.listen, listener_flow, &infra.listener);

    // 9. Register observability hooks
    register_observability_hooks(&mut wafer);

    // 11. Boot through the shared funnel, then run the native-only Start
    //     lifecycle + socket bind. `builder::boot` owns the invariant
    //     grants → seal → init_block(admin) → seed-hook → the rest
    //     ordering shared with the Cloudflare/browser targets, replacing the
    //     bespoke `start_with_priority(&[admin])`. Admin-first init guarantees
    //     admin's migrations (which create impresspress__admin__block_settings +
    //     the variables table) run before any other block's Init writes to
    //     block_settings via migration_helper. Without it, HashMap key-
    //     iteration order could put another block first, hit a hard
    //     'no such table' error (impresspress #182 made write_state propagate
    //     strictly), skip auth's bootstrap, and surface as a login 401 on the
    //     freshly-booted server in CI E2E.
    //
    //     Native then runs the Start lifecycle and binds the HTTP socket — the
    //     steps `boot` deliberately omits because the stateless targets
    //     dispatch per-request instead of binding (wafer-run #239 exposed them
    //     as `run_start_lifecycle` + `bind_all`).
    boot_native(&mut wafer).await?;
    wafer.run_start_lifecycle().await;
    let wafer = wafer.bind_all();
    tracing::info!("WAFER runtime started — all blocks resolved");
    Ok(wafer)
}

/// Build the native runtime over an already-constructed platform database
/// service: pre-wafer admin DDL, variable seeding, the block-settings
/// hash-gate load, admin-created WRAP grants, and `ImpresspressBuilder::build()`.
///
/// `app_env` is the process environment's app config — every key carrying
/// `__` (`collect_app_env_vars`). Its declared keys seed the variables table;
/// every export the seeder's checks accept is also the fallback the blocks'
/// `ConfigSource` resolves from, beneath the table (see the `config_source`
/// call below).
///
/// `password_peppers` go to the crypto service and nowhere else: neither
/// config surface carries them, so no block can read them
/// ([`password_peppers_from_env`] says why).
///
/// `hooks.register_blocks` runs on the builder once the platform services,
/// config and block settings are installed, and `hooks.register_post_build`
/// on the built runtime, with the platform storage service.
///
/// `run()` calls this with the service it built from `infra`; the integration
/// tests call it with a service they seeded first, so what they exercise is
/// the runtime the binary builds rather than a copy of these steps.
pub async fn build_native_runtime(
    infra: &InfraConfig,
    database: Arc<dyn DatabaseService>,
    app_env: &HashMap<String, String>,
    password_peppers: PasswordPeppers,
    run_migrations: bool,
    hooks: AppHooks,
) -> anyhow::Result<Wafer> {
    // Create the admin variables / block_settings tables pre-wafer by running
    // admin's migration-file SQL through the service (migration-file-runner
    // exception). Reuses the embedded `.sql` constants admin's gated `Init`
    // re-asserts later — single schema source, no hand-rolled CREATE TABLE.
    impresspress_core::migration_helper::apply_ddl_via_service(
        &database,
        impresspress_core::blocks::admin::migrations::ddl_files(&infra.db_type),
    )
    .await
    .map_err(|e| anyhow!("create admin tables pre-wafer: {e}"))?;

    // Seed env/auto-gen/JWT variables + run the #222 block-settings hash-gate,
    // all through the shared `impresspress_core` seeders over the service.
    let env_vars = filter_to_declared_keys(app_env.clone());
    let vars = impresspress_core::platform_state::variables::seed_and_load(&database, &env_vars)
        .await
        .map_err(|e| anyhow!("seed and load variables: {e}"))?;
    tracing::info!(vars = vars.len(), "variables loaded from database");

    // 6. Extract JWT secret and feature config from variables. An empty
    // JWT secret would silently fail-open every token verification; bail
    // explicitly so the operator sees the misconfiguration at boot.
    let jwt_secret = vars
        .get(impresspress_core::blocks::auth::JWT_SECRET_KEY)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "missing required variable `{}` — auto-generation should seed this; \
                 the variables table is unreadable or corrupted",
                impresspress_core::blocks::auth::JWT_SECRET_KEY
            )
        })?;
    if jwt_secret.is_empty() {
        return Err(anyhow!(
            "variable `{}` is set but empty — refusing to boot with an empty JWT secret",
            impresspress_core::blocks::auth::JWT_SECRET_KEY
        ));
    }
    // A read error here is always a genuine operational failure (backend
    // outage/corruption) — `block_settings::load_and_seed` never fabricates
    // "every block enabled" out of one. Bail rather than boot with a
    // security-relevant setting silently defaulted open.
    let features = impresspress_core::platform_state::block_settings::load_and_seed(
        &database,
        &impresspress_core::blocks::block_enabled_defaults(),
    )
    .await
    .map_err(|e| anyhow!("load block settings: {e}"))?;

    // `IMPRESSPRESS_REQUEST_LOG`: an operational flag read straight from the
    // process env. It is an infrastructure key
    // (`config_vars::is_infrastructure_key`), so `blocks::config` answers it
    // from the boot map whatever the `variables` table holds and `CONFIG_SET`
    // refuses to write it — the process environment is the only channel it
    // has. `collect_app_env_vars` drops it (no `__`), and
    // `filter_to_declared_keys` would too: it is nobody's declared
    // `ConfigVar`, deliberately, because it is an operator deploy decision
    // rather than an admin-editable runtime toggle.
    // `pipeline::write_request_log` reads it per request via `config_get`.
    let request_log = std::env::var(impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY).ok();

    // 7. Assemble both config surfaces once. `EnvConfigService` is the async
    // (`wafer-run/config`) read surface; the snapshot the builder installs is
    // the synchronous `ctx.config_get` surface. They must carry the same data
    // so `migration_helper::apply_if_blessed` (which reads
    // `BLOCK_SETTINGS_CONFIG_KEY` + `IMPRESSPRESS_RUN_MIGRATIONS` via
    // `config_get`) sees the boot values without a per-call DB hop.
    // Native has no divergence: every key below is `both`.
    let mut runtime_config = builder::RuntimeConfig::new();
    runtime_config
        // Exactly the three keys something must read SYNCHRONOUSLY, not a copy
        // of the variables table. `builder::registration` reads all three
        // during `build()`, before a runtime exists to await anything, and
        // `csrf`/`auth::service` read the secret per request off the snapshot.
        //
        // Copying the whole table here is what step 1 made redundant — and
        // worse than redundant: every admin-editable key sitting on a
        // boot-frozen surface is a stale read waiting for its first caller,
        // which is how the branding and OAuth defects happened. With only
        // these three present, a future `ctx.config_get` of an admin key finds
        // nothing rather than something out of date. `blocks::config` serves
        // the rest from the table.
        .both(
            impresspress_core::blocks::auth::JWT_SECRET_KEY,
            jwt_secret.clone(),
        )
        .both(
            impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY,
            vars.get(impresspress_core::config_vars::CORS_ALLOWED_ORIGINS_KEY)
                .cloned()
                .unwrap_or_default(),
        )
        .both(
            impresspress_core::config_vars::CSP_DIRECTIVES_KEY,
            vars.get(impresspress_core::config_vars::CSP_DIRECTIVES_KEY)
                .cloned()
                .unwrap_or_else(|| {
                    impresspress_core::config_vars::DEFAULT_CSP_DIRECTIVES.to_string()
                }),
        )
        // Fan-out block_settings so consumer blocks (e.g. userportal) can read
        // enablement state via `ctx.config_get` without re-querying the
        // `block_settings` table per request.
        .both(
            impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY,
            features.to_config_json(),
        )
        // Not a config value — a fact about the target. This is the one build
        // that hands `variables::seed_and_load` a real process environment
        // (Cloudflare never calls it, the browser calls it with `&[]`), so it
        // is the one that publishes the marker the admin Variables page reads
        // before offering to hand a key back to that environment. Absent means
        // "no", so no other target has to remember to say anything — and
        // `tests/boot_lifecycle.rs` asserts this line, because a fail-closed
        // marker has no other way of noticing it went missing.
        .both(
            impresspress_core::platform_state::variables::HAS_PROCESS_ENV_CONFIG_KEY,
            "1",
        );
    if run_migrations {
        runtime_config.both(impresspress_core::migration_helper::RUN_MIGRATIONS_KEY, "1");
    }
    if let Some(v) = request_log {
        runtime_config.both(impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY, v);
    }

    // Dispatch on the infra config: `IMPRESSPRESS_STORAGE_TYPE` (local|s3) selects
    // the platform storage service. An unsupported value, or a type whose
    // cargo feature is off, is a hard boot error — the boot path no longer
    // logs `storage = s3` while silently running local disk. (The database
    // service was already constructed above and reused here.)
    let storage =
        impresspress_native::make_storage_service(&infra.storage_type, &infra.storage_root)
            .await
            .context("construct storage service")?;

    // 10. Load admin-created WRAP grants through the platform database
    //     service — the same reader the Cloudflare target uses — so every
    //     backend is covered. (Reading `infra.db_path` as a SQLite file
    //     found nothing on Postgres, where that path is just the unused
    //     default.) The builder installs them before the runtime is sealed.
    let db_grants = impresspress_core::platform_state::wrap_grants::load(&database).await;
    if !db_grants.is_empty() {
        tracing::info!(
            count = db_grants.len(),
            "loaded custom WRAP grants from database"
        );
    }

    // The crypto service, with the password pepper `run()` read from the
    // process environment: the one place the pepper goes.
    tracing::info!("{}", password_pepper::describe(&password_peppers));
    let crypto = impresspress_native::make_jwt_crypto_service(jwt_secret, password_peppers)
        .context("construct crypto service")?;

    let (with_config, ()) = runtime_config.install(
        ImpresspressBuilder::new()
            .database(database)
            .storage(storage.clone()),
        |map| {
            let svc = wafer_core::service_blocks::config::EnvConfigService::new();
            (builder::fill_config_service(Arc::new(svc), map), ())
        },
    );

    // The blocks' declared keys resolve from the variables table, then from
    // the process environment. The table wins where it holds a row: the env
    // loop in `seed_and_load` has already applied the precedence for every
    // key impresspress declares. The environment answers the keys only
    // wafer-run's own service blocks declare — `WAFER_RUN__DATABASE__
    // STRICT_SCHEMA`, the `WAFER_RUN__NETWORK__*` limits — which
    // `filter_to_declared_keys` keeps out of the table (they are operator
    // deploy decisions, not admin-editable rows) and which those blocks read
    // from their `lifecycle(Init)` config alone — neither consults
    // `config_get` or the process environment itself. Only the exports the
    // seeder would accept take part (`usable_env_exports`), so one it refuses
    // — a blank value, a runtime-owned key, a value its key's declared rule
    // refuses — cannot reach a block's Init this way instead.
    let mut block_config =
        impresspress_core::platform_state::variables::usable_env_exports(app_env);
    block_config.extend(vars.iter().map(|(k, v)| (k.clone(), v.clone())));
    let builder = with_config
        .config_source(Arc::new(wafer_run::StaticConfigSource::new(block_config)))
        .crypto(crypto)
        .network(impresspress_native::make_fetch_network_service())
        .logger(impresspress_native::make_tracing_logger())
        .block_settings(features)
        .wrap_grants(db_grants)
        // Hand the SQLite path to the builder so the `native-embedding`
        // feature can open a dedicated connection for `SqliteVecService`.
        // Ignored when the feature is off.
        .sqlite_db_path(&infra.db_path)
        // Where `impresspress/fastembed`'s ONNX model is cached (the
        // `block-fastembed` feature, which `native-embedding` implies).
        // Ignored when the feature is off.
        .model_cache_dir(&infra.model_cache_dir);
    let builder = (hooks.register_blocks)(builder)
        .map_err(|e| anyhow!("register the application's blocks: {e}"))?;
    let mut wafer = builder.build().context("build impresspress runtime")?;
    (hooks.register_post_build)(&mut wafer, storage)
        .map_err(|e| anyhow!("register the application's post-build blocks: {e}"))?;

    Ok(wafer)
}

/// Read the password pepper from the process environment (`var` answers
/// `None` for an unset variable; `run` passes `std::env::var`).
///
/// The three `IMPRESSPRESS_PASSWORD_PEPPER_*` variables are infrastructure
/// keys, and the process environment — or the `.env` file `load_dotenv` reads
/// into it — is the only place they come from. `collect_app_env_vars` never
/// collects them (they carry no `__`), so they are never seeded into the
/// variables table, and they are never put on either config surface:
/// `blocks::config` serves an infrastructure key from the boot map only, so a
/// block asking for one through the config client finds nothing. A value that
/// does not parse fails the boot, naming the variable and never echoing a
/// key; see `impresspress_password::pepper` for the rules and for how to
/// generate, rotate and require a key.
pub fn password_peppers_from_env(
    var: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<PasswordPeppers> {
    password_pepper::password_peppers(
        var(password_pepper::PASSWORD_PEPPER_KEY_VAR).as_deref(),
        var(password_pepper::PASSWORD_PEPPER_PREVIOUS_KEYS_VAR).as_deref(),
        var(password_pepper::PASSWORD_PEPPER_REQUIRED_VAR).as_deref(),
    )
    .map_err(|e| anyhow!("{e}"))
}

/// The block a native server cannot run without: it binds the socket every
/// request arrives on.
const LISTENER_BLOCK: &str = "wafer-run/http-listener";

/// Boot the native runtime through the shared funnel, tolerantly, and refuse
/// a boot whose HTTP listener did not initialize.
///
/// Tolerant, because a long-lived server can be inspected and fixed in place,
/// so one broken feature block must not wedge the whole process. The listener
/// is the exception: its `Init` validates its settings (the `IMPRESSPRESS_*`
/// listener variables among them), and a listener that failed it binds
/// nothing — a process that went on would report itself started and serve no
/// request. So its failure fails the boot, naming the listener's error.
///
/// `build_native_runtime` reads the admin-created grants from the platform
/// database and hands them to `ImpresspressBuilder::wrap_grants` before
/// `build()`, because native seeds and reads everything pre-wafer: the grants
/// are `PreInstalled`.
pub async fn boot_native(wafer: &mut Wafer) -> anyhow::Result<builder::BootReport> {
    let report = builder::boot(
        wafer,
        &NativeBootHooks,
        builder::GrantSource::PreInstalled(
            "build_native_runtime loads them from the platform database into \
             ImpresspressBuilder::wrap_grants before build()",
        ),
        builder::InitPolicy::Tolerant,
    )
    .await
    .context("boot WAFER runtime")?;
    if let Some(listener) = report
        .blocks
        .iter()
        .find(|outcome| outcome.block == LISTENER_BLOCK && !outcome.ok)
    {
        return Err(anyhow!(
            "the HTTP listener did not start, so the server would serve nothing: {}",
            listener.error.as_deref().unwrap_or("its Init failed")
        ));
    }
    Ok(report)
}

/// Native [`BootHooks`](builder::BootHooks). Native seeds the variables /
/// block_settings tables pre-wafer (its immutable crypto service and config
/// snapshot need the values at `build()` time), so — like the Cloudflare hook
/// after its eager pre-build config reads — there is nothing left to seed once
/// admin's `Init` has run. The shared `boot` funnel still owns the admin-first
/// ordering; native only needs an
/// empty hook — [`builder::BootHooks`] is deliberately not an `Option`, so a
/// target with nothing to seed says so here rather than by omission — plus the
/// native-only `run_start_lifecycle` + `bind_all` steps it runs after `boot`
/// returns.
pub struct NativeBootHooks;

#[wafer_block::wafer_async_trait]
impl builder::BootHooks for NativeBootHooks {
    async fn seed_after_admin_init(&self, _wafer: &mut Wafer) -> Result<(), String> {
        Ok(())
    }
}
