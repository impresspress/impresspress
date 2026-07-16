//! Integration test for `impresspress_core::deploy_init::deploy_init` over a
//! real, file-backed SQLite `DatabaseService` — mirroring the native boot
//! path in `crates/impresspress/src/cli/server.rs::run` (steps 5-11), but
//! calling `deploy_init` instead of `builder::boot` so per-block outcomes
//! are captured into a report.
//!
//! Exercises the full ordering invariant end to end: seal → init_block
//! (admin) → seed hook (no-op on native, which seeds pre-wafer) → init
//! every other registered block → post_start. Then rebuilds a second
//! runtime over the *same* sqlite file (matching a redeploy) and asserts
//! the block-settings hash-gate makes the second `deploy_init` call an
//! all-ok no-op.

use std::{collections::HashMap, path::Path, sync::Arc};

use impresspress_core::{
    builder::{BootHooks, ImpresspressBuilder},
    deploy_init::deploy_init,
};
use wafer_core::interfaces::{config::service::ConfigService, database::service::DatabaseService};
use wafer_run::Wafer;

/// Native's `BootHooks`: native seeds the variables / block_settings tables
/// pre-wafer (see `build_runtime` below), so there is nothing left to seed
/// once admin's `Init` has run. Mirrors `impresspress::cli::server::NativeBootHooks`,
/// which is private to that module.
struct NoopBootHooks;

#[wafer_block::wafer_async_trait]
impl BootHooks for NoopBootHooks {
    async fn seed_after_admin_init(&self, _wafer: &Wafer) -> Result<(), String> {
        Ok(())
    }
}

/// Build one WAFER runtime over the sqlite file at `db_path`, mirroring
/// `impresspress::cli::server::run`'s steps 5-11 (database construction, the
/// pre-wafer admin-table DDL, variable seeding, block-settings hash-gate
/// load, and `ImpresspressBuilder::build()`), minus the native-only http-listener
/// bind / observability hooks / shutdown loop that `deploy_init` doesn't need.
///
/// Returns the sealed-but-not-yet-inited `Wafer`, its `ImpresspressStorageBlock`,
/// and the `DatabaseService` handle (so the test can inspect
/// `block_settings` rows directly afterwards).
async fn build_runtime(
    db_path: &Path,
    storage_root: &Path,
) -> (
    Wafer,
    Arc<impresspress_core::blocks::storage::ImpresspressStorageBlock>,
    Arc<dyn DatabaseService>,
) {
    let db_path_str = db_path.to_str().expect("db path is valid utf-8");

    let database = impresspress_native::make_database_service("sqlite", db_path_str, None)
        .await
        .expect("construct sqlite database service");

    // Pre-wafer admin DDL — same migration-file-runner exception `server.rs`
    // uses, so the variables / block_settings tables exist before
    // `seed_and_load_variables` and `load_and_seed_block_settings` read them.
    impresspress_core::migration_helper::apply_ddl_via_service(
        &database,
        impresspress_core::blocks::admin::migrations::ddl_files("sqlite"),
    )
    .await
    .expect("apply admin tables pre-wafer");

    // No process-env vars to seed in this harness; auto-generated secrets
    // (incl. the JWT secret) are still seeded by `seed_and_load_variables`.
    let vars = impresspress_core::boot::seed_and_load_variables(&database, &[])
        .await
        .expect("seed and load variables");

    let jwt_secret = vars
        .get(impresspress_core::blocks::auth::JWT_SECRET_KEY)
        .cloned()
        .expect("JWT secret auto-seeded");

    let features = impresspress_core::features::load_and_seed_block_settings(&database)
        .await
        .expect("load block settings");

    let config_service = wafer_core::service_blocks::config::EnvConfigService::new();
    for (key, value) in &vars {
        config_service.set(key, value);
    }
    config_service.set(
        impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY,
        &features.to_config_json(),
    );

    let mut snapshot: HashMap<String, String> = vars.clone();
    snapshot.insert(
        impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY.to_string(),
        features.to_config_json(),
    );

    let storage_root_str = storage_root.to_str().expect("storage root is valid utf-8");
    let storage = impresspress_native::make_storage_service("local", storage_root_str)
        .await
        .expect("construct local storage service");

    let (mut wafer, storage_block) = ImpresspressBuilder::new()
        .database(database.clone())
        .storage(storage)
        .config(Arc::new(config_service))
        .config_source(Arc::new(wafer_run::StaticConfigSource::new(vars.clone())))
        .crypto(
            impresspress_native::make_jwt_crypto_service(jwt_secret).expect("jwt crypto service"),
        )
        .network(impresspress_native::make_fetch_network_service().expect("network service"))
        .logger(impresspress_native::make_tracing_logger())
        .block_settings(features)
        .sqlite_db_path(db_path_str)
        .build()
        .expect("build impresspress runtime");

    wafer.set_config_snapshot(snapshot);

    (wafer, storage_block, database)
}

#[tokio::test]
async fn deploy_init_first_run_ok_and_second_run_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("deploy_init_test.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    // --- First run: fresh DB, everything must init ok. ---
    let (mut wafer, storage_block, db) = build_runtime(&db_path, &storage_root).await;
    let report = deploy_init(&mut wafer, &storage_block, &NoopBootHooks)
        .await
        .expect("seal");

    assert!(report.ok, "first deploy_init must succeed: {report:?}");
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
        limit: 10_000,
        skip_count: true,
        ..Default::default()
    };
    let rows = db
        .list(impresspress_core::admin_schema::BLOCK_SETTINGS_TABLE, &opts)
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
    let (mut wafer2, storage_block2, _db2) = build_runtime(&db_path, &storage_root).await;
    let report2 = deploy_init(&mut wafer2, &storage_block2, &NoopBootHooks)
        .await
        .expect("seal 2");

    assert!(
        report2.ok,
        "second deploy_init must be a clean no-op: {report2:?}"
    );
    assert!(
        report2
            .blocks
            .iter()
            .any(|b| b.block == impresspress_core::blocks::admin::ADMIN_BLOCK_ID && b.ok),
        "admin block must be ok on second run too: {:?}",
        report2.blocks
    );
}

/// `BootHooks` whose seed step always fails, to exercise `deploy_init`'s
/// capture-and-continue contract: a failing hook must NOT abort the funnel
/// (still `Ok(report)`), and every other block must still get initialized.
struct FailingBootHooks;

#[wafer_block::wafer_async_trait]
impl BootHooks for FailingBootHooks {
    async fn seed_after_admin_init(&self, _wafer: &Wafer) -> Result<(), String> {
        Err("boom".to_string())
    }
}

#[tokio::test]
async fn deploy_init_seed_failure_is_captured_not_aborted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("deploy_init_seed_failure_test.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let (mut wafer, storage_block, _db) = build_runtime(&db_path, &storage_root).await;
    let report = deploy_init(&mut wafer, &storage_block, &FailingBootHooks)
        .await
        .expect("deploy_init must still return Ok even when the seed hook errors");

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
