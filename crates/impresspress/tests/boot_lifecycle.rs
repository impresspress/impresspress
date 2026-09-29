//! Integration tests for `impresspress_core::builder::boot` over a real,
//! file-backed SQLite `DatabaseService`, built through the same
//! `impresspress_server::build_native_runtime` the binary uses.
//!
//! `boot` is the one post-`build()` lifecycle for every target; these tests
//! drive it under [`InitPolicy::Reported`], the policy `/_deploy/init` uses,
//! because that is the one that captures per-block outcomes into a
//! [`BootReport`] a test can read. The ordering it exercises end to end is the
//! invariant one: grants → seal → `init_block(admin)` → seed hook (a no-op on
//! native, which seeds pre-wafer) → every other registered block → the WRAP
//! grants into the storage block.
//!
//! The first test then rebuilds a second runtime over the *same* sqlite file
//! (matching a redeploy) and asserts the block-settings hash-gate makes the
//! second `boot` an all-ok no-op.

use std::{collections::HashMap, path::Path, sync::Arc};

use impresspress_core::builder::{boot, BootHooks, GrantSource, InitPolicy};
use impresspress_native::InfraConfig;
use impresspress_server::{build_native_runtime, AppHooks, NativeBootHooks};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::{InputStream, Message, Wafer};

/// The reason native passes [`GrantSource::PreInstalled`]: `build_native_runtime`
/// reads the admin-created rows out of the platform database and hands them to
/// `ImpresspressBuilder::wrap_grants` before `build()`, because native seeds and
/// reads everything pre-wafer. Spelled once here so every call below states the
/// same thing the binary states.
const NATIVE_GRANTS: GrantSource<'static> = GrantSource::PreInstalled(
    "build_native_runtime loads them from the platform database into \
     ImpresspressBuilder::wrap_grants before build()",
);

/// The infra config `run()` would read from the environment, pointed at
/// the test's temp paths. `listen` is unused here: `boot` never binds.
fn infra_for(db_path: &Path, storage_root: &Path) -> InfraConfig {
    InfraConfig {
        listen: "127.0.0.1:0".to_string(),
        db_type: "sqlite".to_string(),
        db_path: db_path
            .to_str()
            .expect("db path is valid utf-8")
            .to_string(),
        db_url: None,
        storage_type: "local".to_string(),
        storage_root: storage_root
            .to_str()
            .expect("storage root is valid utf-8")
            .to_string(),
        model_cache_dir: "data/models".to_string(),
        listener: Default::default(),
    }
}

/// Build one WAFER runtime over the sqlite file at `db_path` through the
/// binary's own `build_native_runtime` (no process-env vars to seed in this
/// harness; auto-generated secrets, including the JWT secret, are still
/// seeded). Returns the built-but-not-yet-inited `Wafer` and the
/// `DatabaseService` handle so the test can inspect `block_settings` rows
/// directly afterwards.
async fn build_runtime(db_path: &Path, storage_root: &Path) -> (Wafer, Arc<dyn DatabaseService>) {
    build_runtime_with_env(db_path, storage_root, &HashMap::new()).await
}

/// [`build_runtime`] with an explicit app environment, the map `run()` builds
/// with `collect_app_env_vars()`. Passed as an argument rather than exported
/// into the process environment: env mutation is `unsafe` in Rust 2024 and
/// races every other test in the binary.
async fn build_runtime_with_env(
    db_path: &Path,
    storage_root: &Path,
    app_env: &HashMap<String, String>,
) -> (Wafer, Arc<dyn DatabaseService>) {
    let infra = infra_for(db_path, storage_root);
    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");

    let wafer = build_native_runtime(
        &infra,
        database.clone(),
        app_env,
        Default::default(),
        false,
        AppHooks::none(),
    )
    .await
    .expect("build impresspress runtime");

    (wafer, database)
}

#[tokio::test]
async fn boot_first_run_ok_and_second_run_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("boot_lifecycle_test.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    // --- First run: fresh DB, everything must init ok. ---
    let (mut wafer, db) = build_runtime(&db_path, &storage_root).await;
    let report = boot(
        &mut wafer,
        &NativeBootHooks,
        NATIVE_GRANTS,
        InitPolicy::Reported,
    )
    .await
    .expect("seal");

    assert!(report.ok, "first boot must succeed: {report:?}");
    assert!(report.sealed);
    assert!(
        report
            .blocks
            .iter()
            .any(|b| b.block == impresspress_core::blocks::admin::ADMIN_BLOCK_ID && b.ok),
        "admin block must be present and ok: {:?}",
        report.blocks
    );
    // More than just admin got initialized (the default feature set
    // registers several other feature blocks).
    assert!(
        report.blocks.len() > 1,
        "expected more than admin to be initialized: {:?}",
        report.blocks
    );

    // --- Stamp format: block_settings rows carry 64-hex current_hash == blessed_hash. ---
    let opts = wafer_block::db::ListOptions {
        limit: Some(10_000),
        skip_count: true,
        ..Default::default()
    };
    let rows = db
        .list(
            impresspress_core::platform_state::block_settings::TABLE,
            &opts,
        )
        .await
        .expect("list block_settings")
        .records;
    let admin_row = rows
        .iter()
        .find(|r| {
            r.data["block_name"]
                == serde_json::json!(impresspress_core::blocks::admin::ADMIN_BLOCK_ID)
        })
        .expect("admin row stamped");
    let cur = admin_row.data["current_hash"]
        .as_str()
        .expect("current_hash is a string");
    assert_eq!(cur.len(), 64, "raw sha256 hex, got: {cur}");
    assert!(
        cur.chars().all(|c| c.is_ascii_hexdigit()),
        "current_hash must be hex: {cur}"
    );
    assert_eq!(
        admin_row.data["current_hash"],
        admin_row.data["blessed_hash"]
    );

    // --- Idempotency: second run over the same DB, via a REBUILT runtime, is all-ok. ---
    let (mut wafer2, _db2) = build_runtime(&db_path, &storage_root).await;
    let report2 = boot(
        &mut wafer2,
        &NativeBootHooks,
        NATIVE_GRANTS,
        InitPolicy::Reported,
    )
    .await
    .expect("seal 2");

    assert!(report2.ok, "second boot must be a clean no-op: {report2:?}");
    assert!(
        report2
            .blocks
            .iter()
            .any(|b| b.block == impresspress_core::blocks::admin::ADMIN_BLOCK_ID && b.ok),
        "admin block must be ok on second run too: {:?}",
        report2.blocks
    );
}

/// `BootHooks` whose seed step always fails, to exercise
/// [`InitPolicy::Reported`]'s capture-and-continue contract: a failing hook
/// must NOT abort the funnel (still `Ok(report)`), and every other block must
/// still get initialized.
struct FailingBootHooks;

#[wafer_block::wafer_async_trait]
impl BootHooks for FailingBootHooks {
    async fn seed_after_admin_init(&self, _wafer: &mut Wafer) -> Result<(), String> {
        Err("boom".to_string())
    }
}

#[tokio::test]
async fn reported_boot_captures_a_seed_failure_without_aborting() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("boot_seed_failure_test.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let (mut wafer, _db) = build_runtime(&db_path, &storage_root).await;
    let report = boot(
        &mut wafer,
        &FailingBootHooks,
        NATIVE_GRANTS,
        InitPolicy::Reported,
    )
    .await
    .expect("a Reported boot must still return Ok when the seed hook errors");

    assert!(
        !report.ok,
        "overall report must be not-ok when the seed hook fails: {report:?}"
    );
    assert!(
        !report.seed.ok,
        "seed step outcome must be not-ok: {:?}",
        report.seed
    );
    assert_eq!(report.seed.error.as_deref(), Some("boom"));

    // Seed failure must not prevent the rest of the funnel: blocks still
    // get initialized.
    assert!(
        !report.blocks.is_empty(),
        "blocks must still be initialized after a seed failure: {:?}",
        report.blocks
    );
    assert!(
        report.blocks.iter().all(|b| b.ok),
        "every block must still init ok despite the seed failure: {:?}",
        report.blocks
    );
}

/// The same seed failure under the policies the long-lived and the
/// request-isolated targets use is fatal instead: only `/_deploy/init`, whose
/// product is the report, keeps going.
#[tokio::test]
async fn a_seed_failure_aborts_a_tolerant_or_strict_boot() {
    for policy in [InitPolicy::Tolerant, InitPolicy::Strict] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("boot_seed_fatal_test.sqlite3");
        let storage_root = tmp.path().join("storage");
        std::fs::create_dir_all(&storage_root).expect("create storage root");

        let (mut wafer, _db) = build_runtime(&db_path, &storage_root).await;
        let error = boot(&mut wafer, &FailingBootHooks, NATIVE_GRANTS, policy)
            .await
            .expect_err(&format!("a seed failure must abort under {policy:?}"));
        assert!(error.to_string().contains("boom"), "{policy:?}: {error}");
    }
}

/// The Cloudflare prepared-hydration path passes `GrantSource::PreInstalled`
/// because `ImpresspressBuilder::apply_prepared_plan` has already installed the
/// plan's grants — this pins that the claim is true through `build()` and
/// `seal()`, on the only runtime a host test can actually build.
///
/// `boot` cannot check this for the caller: after `seal()` a grant cannot be
/// added at all, so a prepared path that dropped its grants would seal a
/// runtime whose storage block enforces nothing across blocks, and every
/// cross-block read would start refusing. The `PreInstalled` variant is a
/// claim; this is the test that the claim holds.
#[tokio::test]
async fn a_prepared_plans_grants_reach_the_sealed_runtime() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("prepared_grants_test.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let grant = wafer_run::ResourceGrant::read_write(
        "impresspress/files",
        "impresspress__admin__variables",
    );

    // Export a plan carrying one deployment grant, exactly the way
    // `/_deploy/init` does after seeding (`publish_wrap_grants`).
    let source = impresspress_core::builder::ImpresspressBuilder::new();
    let exporter = source
        .prepared_plan_exporter()
        .expect("prepared plan exporter");
    exporter
        .publish_wrap_grants(std::slice::from_ref(&grant))
        .expect("publish wrap grants");
    let build_sha = format!("sha256:{}", "b".repeat(64));
    let lock = impresspress_core::prepared_plan::WaferLockIdentity::absent();
    let assets = impresspress_core::prepared_plan::PreparedReleaseAssets::absent();
    let plan = exporter
        .prepare_runtime_plan("app", build_sha.clone(), lock.clone(), assets.clone())
        .expect("prepare runtime plan");
    assert_eq!(plan.structure.deployment_wrap_grants.len(), 1);

    // Hydrate a real runtime from it, then boot it the way the prepared
    // Cloudflare path does.
    let infra = infra_for(&db_path, &storage_root);
    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    let storage = impresspress_native::make_storage_service("local", &infra.storage_root)
        .await
        .expect("construct local storage service");

    let (builder, ()) = impresspress_core::builder::RuntimeConfig::new().install(
        impresspress_core::builder::ImpresspressBuilder::new()
            .database(database)
            .storage(storage),
        |map| {
            (
                impresspress_core::builder::fill_config_service(
                    Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
                    map,
                ),
                (),
            )
        },
    );
    let builder = builder
        .crypto(
            impresspress_native::make_jwt_crypto_service(
                "prepared-grants-test-jwt-secret-value".to_string(),
                Default::default(),
            )
            .expect("jwt crypto service"),
        )
        .network(impresspress_native::make_fetch_network_service())
        .logger(impresspress_native::make_tracing_logger())
        .apply_prepared_plan(&plan, "app", &build_sha, &lock, &assets)
        .expect("apply prepared plan");

    let mut wafer = builder.build().expect("build impresspress runtime");
    boot(
        &mut wafer,
        &NativeBootHooks,
        GrantSource::PreInstalled(
            "ImpresspressBuilder::apply_prepared_plan installs the verified \
             plan's wrap_grants and deployment_wrap_grants before build()",
        ),
        InitPolicy::Reported,
    )
    .await
    .expect("boot the hydrated runtime");

    let rendered = format!("{:?}", wafer.wrap_grants());
    assert!(
        rendered.contains("impresspress__admin__variables"),
        "the plan's grant must be registered before seal: {rendered}"
    );
}

/// A deployment grant the runtime refuses to install fails the build, loudly.
/// `Wafer::add_wrap_grants` rejects the whole set when one grant fails
/// `ResourceGrant::check_shape` and installs none of it, so a build that
/// carried on would seal a runtime missing every deployment grant.
#[tokio::test]
async fn a_malformed_deployment_grant_fails_the_build() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("malformed_grant_test.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let infra = infra_for(&db_path, &storage_root);
    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    let storage = impresspress_native::make_storage_service("local", &infra.storage_root)
        .await
        .expect("construct local storage service");
    let (builder, ()) = impresspress_core::builder::RuntimeConfig::new().install(
        impresspress_core::builder::ImpresspressBuilder::new()
            .database(database)
            .storage(storage),
        |map| {
            (
                impresspress_core::builder::fill_config_service(
                    Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
                    map,
                ),
                (),
            )
        },
    );
    let builder = builder
        .crypto(
            impresspress_native::make_jwt_crypto_service(
                "malformed-grant-test-jwt-secret-value".to_string(),
                Default::default(),
            )
            .expect("jwt crypto service"),
        )
        .network(impresspress_native::make_fetch_network_service())
        .logger(impresspress_native::make_tracing_logger())
        // An append-only grant is a database-collection grant; typed
        // Storage it is one the runtime will not install.
        .wrap_grants(vec![
            wafer_run::ResourceGrant::read("impresspress/files", "impresspress__admin__variables"),
            wafer_run::ResourceGrant::append("impresspress/files", "impresspress/admin/exports")
                .typed(wafer_run::ResourceType::Storage),
        ]);

    let Err(error) = builder.build() else {
        panic!("a build carrying a malformed deployment grant must fail");
    };
    assert!(error.to_string().contains("append"), "{error}");
}

/// Native fills both config surfaces from one `RuntimeConfig`, so anything the
/// async `ConfigService` carries is readable synchronously through
/// `ctx.config_get` as well. Before `RuntimeConfig` these were two literals in
/// `build_native_runtime` held together by a comment.
#[tokio::test]
async fn the_native_build_fills_the_synchronous_config_surface() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("native_config_surfaces.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let (wafer, _db) = build_runtime(&db_path, &storage_root).await;
    let snapshot: &HashMap<String, String> = wafer.config_snapshot();

    assert!(
        snapshot.contains_key(impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY),
        "block settings must reach the synchronous surface: {:?}",
        snapshot.keys().collect::<Vec<_>>()
    );
    assert!(
        !snapshot.contains_key(impresspress_core::migration_helper::RUN_MIGRATIONS_KEY),
        "this harness builds with run_migrations = false",
    );
    // The JWT secret is seeded pre-wafer by `build_native_runtime` and is one
    // of the variables it fans into both surfaces.
    assert!(
        snapshot.contains_key(impresspress_core::blocks::auth::JWT_SECRET_KEY),
        "seeded variables must reach the synchronous surface: {:?}",
        snapshot.keys().collect::<Vec<_>>()
    );
    // The marker that says this target boots from a process environment.
    //
    // Native is the ONLY publisher — the key fails closed, so nothing catches a
    // build that stops publishing it except an assertion here. Without it the
    // admin Variables page renders no "Reset to environment" control, and the
    // boot WARN that names that control is pointing at nothing: exactly the
    // defect this branch shipped the markup to fix, reintroduced silently.
    assert_eq!(
        snapshot
            .get(impresspress_core::platform_state::variables::HAS_PROCESS_ENV_CONFIG_KEY)
            .map(String::as_str),
        Some("1"),
        "the native build must declare that it has a process environment: {:?}",
        snapshot.keys().collect::<Vec<_>>()
    );
}

/// The 2026-09-10 live-server finding, end to end over the binary's own boot
/// path: an operator sets `WAFER_RUN_SHARED__APP_NAME` on a deployment whose
/// database already exists, and the value they set must be the one that boots.
///
/// It used to be discarded from the second boot on — env vars were seeded with
/// `INSERT OR IGNORE`, so they only ever landed on a virgin database. This
/// drives two boots over the *same* sqlite file with two different values to
/// cover exactly that, and hands the environment to `build_native_runtime`,
/// which shapes the seed batch with `filter_to_declared_keys` as it does in
/// production.
///
/// Both boots here are a FRESH database's, so the first one creates the row and
/// records the one-time upgrade transition, and the second is the steady state.
/// The upgrade boot of a database that predates edit tracking is a different
/// case — it keeps a disagreeing row and says so — and is covered by
/// `platform_state::variables`' own tests, which can stage an unmarked row.
///
/// The `IMPRESSPRESS_DEPLOY_TOKEN` assertion below pins that FILTER, not
/// `seed_and_load`'s own `is_runtime_owned_key` guard: an infrastructure key
/// carries no `__`, so `collect_app_env_vars` and `filter_to_declared_keys`
/// both drop it long before the seeder runs, and this assertion would pass
/// with that guard deleted. The guard's own coverage is a unit test in
/// `platform_state::variables`, which is the only place able to reach it.
#[tokio::test]
async fn a_process_env_var_wins_over_the_row_a_previous_boot_stored() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("env_precedence.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    const KEY: &str = "WAFER_RUN_SHARED__APP_NAME";

    // The operator's environment, as `build_native_runtime` receives it.
    let environment = |app_name: &str| {
        HashMap::from([
            (KEY.to_string(), app_name.to_string()),
            // Infrastructure: never a variables-table row.
            (
                impresspress_core::config_vars::DEPLOY_TOKEN_KEY.to_string(),
                "deploy-token".to_string(),
            ),
        ])
    };

    // --- First boot: fresh database, the operator's value lands. ---
    let (mut wafer, db) =
        build_runtime_with_env(&db_path, &storage_root, &environment("First")).await;
    boot(
        &mut wafer,
        &NativeBootHooks,
        NATIVE_GRANTS,
        InitPolicy::Reported,
    )
    .await
    .expect("first boot");
    assert_eq!(stored(&db, KEY).await.as_deref(), Some("First"));

    // --- Second boot over the same file with a changed environment. ---
    let (mut wafer2, db2) =
        build_runtime_with_env(&db_path, &storage_root, &environment("Second")).await;
    boot(
        &mut wafer2,
        &NativeBootHooks,
        NATIVE_GRANTS,
        InitPolicy::Reported,
    )
    .await
    .expect("second boot");

    assert_eq!(
        stored(&db2, KEY).await.as_deref(),
        Some("Second"),
        "the environment an operator set for THIS boot has to be the one in effect"
    );
    assert_eq!(
        stored(&db2, impresspress_core::config_vars::DEPLOY_TOKEN_KEY).await,
        None,
        "`filter_to_declared_keys` must keep an infrastructure key out of the batch"
    );
}

/// `WAFER_RUN_SHARED__SITE_URL` is no longer declared, but every deployment
/// that booted an older release holds a row for it: `seed_defaults` wrote the
/// declared default into the `variables` table on first boot. That leftover
/// row must not stop a boot, must not break the admin Variables page, and must
/// be removable by an operator — and an exported `SITE_URL` must stop reaching
/// the table, since nothing declares it any more.
#[tokio::test]
async fn a_leftover_row_for_a_retired_var_boots_lists_and_deletes() {
    const RETIRED: &str = "WAFER_RUN_SHARED__SITE_URL";
    const OLD_DEFAULT: &str = "https://impresspress.org";

    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("retired_var.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    // --- A deployment that booted before the var was retired. ---
    let (mut wafer, db) = build_runtime(&db_path, &storage_root).await;
    boot(
        &mut wafer,
        &NativeBootHooks,
        NATIVE_GRANTS,
        InitPolicy::Reported,
    )
    .await
    .expect("first boot");
    // The row exactly as an older `seed_defaults` wrote it.
    assert!(
        impresspress_core::platform_state::variables::seed_if_absent(
            &db,
            RETIRED,
            OLD_DEFAULT,
            "Site URL",
            "Marketing site URL for docs and pricing links",
            false,
        )
        .await
        .expect("stage the leftover row"),
        "the fresh boot must not have seeded the retired var itself"
    );

    // --- Upgrade boot, with the retired var still exported. ---
    let environment = HashMap::from([(RETIRED.to_string(), "https://env.example".to_string())]);
    let (mut wafer, db) = build_runtime_with_env(&db_path, &storage_root, &environment).await;
    let report = boot(
        &mut wafer,
        &NativeBootHooks,
        NATIVE_GRANTS,
        InitPolicy::Reported,
    )
    .await
    .expect("a leftover row for a retired var must not fail the boot");
    assert!(
        report.ok,
        "no step may fail over the leftover row: {report:?}"
    );
    assert_eq!(
        stored(&db, RETIRED).await.as_deref(),
        Some(OLD_DEFAULT),
        "an undeclared key never reaches the table from the environment"
    );

    // --- The admin Variables page still renders, and lists the row. ---
    let admin = |action: &str, resource: &str| {
        let mut msg = Message::new("http.request");
        msg.set_meta("req.action", action);
        msg.set_meta("req.resource", resource);
        msg.set_meta("http.header.accept", "text/html");
        msg.set_meta("auth.user_id", "admin_1");
        msg.set_meta("auth.user_roles", "admin");
        msg
    };
    let out = wafer
        .run_block(
            "impresspress/admin",
            admin("retrieve", "/b/admin/settings/variables"),
            InputStream::empty(),
        )
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let html = String::from_utf8(parts.body).expect("UTF-8 body");
    assert_eq!(parts.status, 200, "{html}");
    assert!(html.contains(RETIRED), "the leftover row is listed: {html}");

    // --- And an operator can delete it. ---
    let out = wafer
        .run_block(
            "impresspress/admin",
            admin("delete", &format!("/b/admin/api/settings/{RETIRED}")),
            InputStream::empty(),
        )
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    assert!(
        (200..300).contains(&parts.status),
        "deleting the leftover row: {} {}",
        parts.status,
        String::from_utf8_lossy(&parts.body)
    );
    assert_eq!(stored(&db, RETIRED).await, None);
}

/// The value stored in the variables table for `key`, if any.
async fn stored(db: &Arc<dyn DatabaseService>, key: &str) -> Option<String> {
    impresspress_core::platform_state::variables::find_by_key(db, key)
        .await
        .expect("read the variables table")
        .map(|row| row.value)
}

/// Boot a fresh runtime over `app_env` and hand back its report and database.
async fn boot_fresh_with_env(
    name: &str,
    app_env: HashMap<String, String>,
) -> (
    impresspress_core::builder::BootReport,
    Arc<dyn DatabaseService>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join(format!("{name}.sqlite3"));
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let (mut wafer, db) = build_runtime_with_env(&db_path, &storage_root, &app_env).await;
    let report = boot(
        &mut wafer,
        &NativeBootHooks,
        NATIVE_GRANTS,
        InitPolicy::Reported,
    )
    .await
    .expect("seal");
    (report, db, tmp)
}

/// A write naming a column the table does not have: added lazily unless
/// STRICT_SCHEMA is on, refused when it is.
async fn write_with_an_undeclared_column(db: &Arc<dyn DatabaseService>) -> bool {
    let mut data = HashMap::new();
    data.insert("key".to_string(), serde_json::json!("P4_STRICT_PROBE"));
    data.insert("value".to_string(), serde_json::json!("x"));
    data.insert(
        "column_no_migration_declares".to_string(),
        serde_json::json!("x"),
    );
    db.create(impresspress_core::platform_state::variables::TABLE, data)
        .await
        .is_ok()
}

/// `WAFER_RUN__DATABASE__STRICT_SCHEMA` is an operator's process-env knob
/// that `wafer-run/database` reads from its `lifecycle(Init)` config. Native
/// keeps it out of the variables table (nothing impresspress declares), so
/// the block's `ConfigSource` must resolve it from the process environment —
/// or the block applies its declared `false` and the export does nothing.
#[tokio::test]
async fn the_strict_schema_env_var_reaches_the_database_blocks_init() {
    let (report, db, _tmp) = boot_fresh_with_env(
        "strict_schema_env",
        HashMap::from([(
            wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY.to_string(),
            "true".to_string(),
        )]),
    )
    .await;
    assert!(report.ok, "a strict boot must succeed: {report:?}");
    assert!(
        !write_with_an_undeclared_column(&db).await,
        "STRICT_SCHEMA was exported, so the database must refuse to add a column lazily"
    );

    // The control: without the export the same write adds its column, so the
    // refusal above is STRICT_SCHEMA's and not the write's.
    let (report, db, _tmp) = boot_fresh_with_env("strict_schema_unset", HashMap::new()).await;
    assert!(report.ok, "{report:?}");
    assert!(
        write_with_an_undeclared_column(&db).await,
        "without STRICT_SCHEMA a write adds the column it names"
    );
}

/// The `wafer-run/network` limits are the block's declared config, read at
/// its `lifecycle(Init)`: an operator's process-env value must reach it, and
/// an invalid one must fail that block's Init naming the key rather than be
/// replaced by the default.
#[tokio::test]
async fn a_network_limit_env_var_reaches_the_network_blocks_init() {
    const KEY: &str = wafer_core::interfaces::network::service::MAX_RESPONSE_BYTES_KEY;
    let (report, _db, _tmp) = boot_fresh_with_env(
        "network_limit_env",
        HashMap::from([(KEY.to_string(), "not-a-number".to_string())]),
    )
    .await;
    let network = report
        .blocks
        .iter()
        .find(|b| b.block == "wafer-run/network")
        .unwrap_or_else(|| panic!("wafer-run/network must be initialised: {report:?}"));
    assert!(
        !network.ok,
        "an invalid exported limit must fail the network block's Init: {network:?}"
    );
    assert!(
        network.error.as_deref().is_some_and(|e| e.contains(KEY)),
        "the Init error must name the key: {network:?}"
    );
}

/// The key [`InitConfigProbe`] declares, under its registration name's
/// prefix (`BlockInfo::validate` refuses any other).
const PROBE_KEY: &str = "TEST__INIT_CONFIG_PROBE__MODE";

/// [`PROBE_KEY`]'s declared default.
const PROBE_DEFAULT: &str = "declared-default";

/// A block declaring [`PROBE_KEY`] that records what its `lifecycle(Init)`
/// config carried for it.
struct InitConfigProbe(Arc<std::sync::Mutex<Option<String>>>);

#[wafer_block::wafer_async_trait]
impl wafer_run::Block for InitConfigProbe {
    fn info(&self) -> wafer_run::BlockInfo {
        wafer_run::BlockInfo::new(
            "test/init-config-probe",
            "0.0.1",
            "http-handler@v1",
            "records the Init config it is handed",
        )
        .config_keys(vec![wafer_run::ConfigVar::new(
            PROBE_KEY,
            "the key under test",
            PROBE_DEFAULT,
        )])
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_run::context::Context,
        event: wafer_run::LifecycleEvent,
    ) -> Result<(), wafer_run::WaferError> {
        if event.event_type == wafer_run::LifecycleType::Init {
            let config = wafer_block::BlockConfig::from_event(&event);
            *self.0.lock().expect("probe lock") = Some(config.str(PROBE_KEY).to_string());
        }
        Ok(())
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_run::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> wafer_run::OutputStream {
        wafer_run::OutputStream::respond(Vec::new())
    }
}

/// Boot a fresh native runtime whose process environment exports
/// [`PROBE_KEY`]` = value`, with an [`InitConfigProbe`] registered, and return
/// what the probe's Init config carried for the key.
async fn what_a_blocks_init_sees_for(name: &str, value: &str) -> String {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join(format!("{name}.sqlite3"));
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let app_env = HashMap::from([(PROBE_KEY.to_string(), value.to_string())]);
    let (mut wafer, _db) = build_runtime_with_env(&db_path, &storage_root, &app_env).await;
    let seen = Arc::new(std::sync::Mutex::new(None));
    wafer
        .register_block(
            "test/init-config-probe",
            Arc::new(InitConfigProbe(seen.clone())),
        )
        .expect("register the probe");
    let report = boot(
        &mut wafer,
        &NativeBootHooks,
        NATIVE_GRANTS,
        InitPolicy::Reported,
    )
    .await
    .expect("seal");
    assert!(report.ok, "{report:?}");
    let seen = seen.lock().expect("probe lock").clone();
    seen.expect("the probe's Init must have run")
}

/// The environment beneath the variables table reaches a block's Init only
/// through the checks the variables seeder applies to the same export
/// (`variables::usable_env_exports`). An empty export is the one a
/// test-registered block can observe: blank means unset to the seeder, so it
/// must not beat the key's declared default at Init either — the
/// `ConfigSource` would otherwise hand the block the empty string, which
/// `resolve_declared` takes as a value. The declared value rules the same
/// check applies are covered in `platform_state::variables`' unit tests; every
/// rule today is on a `WAFER_RUN_SHARED__*` key, which no block may declare.
#[tokio::test]
async fn an_env_export_the_seeder_refuses_does_not_reach_a_blocks_init() {
    assert_eq!(
        what_a_blocks_init_sees_for("empty_env_export", "").await,
        PROBE_DEFAULT,
        "an empty export must leave the declared default in place"
    );

    // The control: an export the checks accept does reach Init, so the
    // default above is the refusal's and not the probe's.
    assert_eq!(
        what_a_blocks_init_sees_for("accepted_env_export", "from-env").await,
        "from-env",
    );
}
