//! A native boot whose admin-created WRAP grants cannot be read refuses to
//! start. A server built without them would serve, look healthy, and deny
//! every access those grants exist to allow, so a failed read is a refused
//! start rather than "no grants".
//!
//! The boot is `start_native`, which `impresspress serve` runs. The database
//! is the platform SQLite service behind `D1Shaped`, whose fault switch fails
//! every statement naming the grants table the way an outage does. Admin's
//! migrations are applied before the switch is turned on, so the only read
//! the fault meets is the grants load.

use std::{collections::HashMap, sync::Arc};

use impresspress_core::{
    blocks::admin,
    migration_helper,
    platform_state::wrap_grants,
    test_support::d1_shaped::{D1Shaped, Fault, Ledger},
};
use impresspress_native::{InfraConfig, ListenerEnv};
use impresspress_server::{start_native, AppHooks};
use wafer_core::interfaces::database::service::DatabaseService;

#[tokio::test]
async fn a_failed_grants_read_refuses_to_start() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("grants.sqlite3");
    let storage_root = dir.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let infra = InfraConfig {
        listen: "127.0.0.1:0".to_string(),
        db_type: "sqlite".to_string(),
        db_path: db_path.to_str().expect("UTF-8 path").to_string(),
        db_url: None,
        storage_type: "local".to_string(),
        storage_root: storage_root.to_str().expect("UTF-8 path").to_string(),
        model_cache_dir: "data/models".to_string(),
        listener: ListenerEnv::default(),
    };

    let shaped = Arc::new(D1Shaped::open(&db_path, Arc::new(Ledger::default())));
    let database: Arc<dyn DatabaseService> = shaped.clone();
    // Admin's list, applied the way `build_native_runtime` applies it, so
    // the boot finds it applied and sends no statement naming the table
    // before the grants load.
    let migrations = &admin::migrations::MIGRATIONS;
    let backend = migration_helper::backend_named(&infra.db_type);
    let (files, history) = migrations.for_backend(backend);
    migration_helper::apply_pending_via_service(
        &database,
        backend,
        migrations.block,
        files,
        history,
    )
    .await
    .expect("apply admin migrations");
    wrap_grants::load(&database)
        .await
        .expect("the grants table reads before the fault is switched on");
    shaped.fail_table(wrap_grants::TABLE, Fault::Unavailable);

    let error = match start_native(
        &infra,
        database,
        &HashMap::new(),
        Default::default(),
        "test-main",
        AppHooks::none(),
    )
    .await
    {
        Ok(_) => panic!("a server whose grants could not be read must not start"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        error.contains("load the admin-created WRAP grants") && error.contains("injected outage"),
        "{error}"
    );
}
