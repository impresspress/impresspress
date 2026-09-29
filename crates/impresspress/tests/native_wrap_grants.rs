//! The native boot must read admin-created WRAP grants through the platform
//! `DatabaseService`, never by opening `IMPRESSPRESS_DB_PATH` as a SQLite
//! file. On a Postgres deployment `db_path` keeps its SQLite default and
//! names no database at all; a reader that opens that path finds nothing
//! (and leaves a stray empty file behind), so the runtime boots with no
//! dynamic grants. This test reproduces that shape locally: the service is
//! built on one file while `infra.db_path` points somewhere else.

use std::collections::HashMap;

use impresspress_native::InfraConfig;
use impresspress_server::{build_native_runtime, AppHooks};

#[tokio::test]
async fn wrap_grants_are_read_through_the_database_service() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let real_db = tmp.path().join("real.sqlite3");
    // Where `IMPRESSPRESS_DB_PATH` would point on a Postgres deployment:
    // the SQLite default, which names no database.
    let stray_path = tmp.path().join("data").join("impresspress.db");

    let infra = InfraConfig {
        listen: "127.0.0.1:0".to_string(),
        db_type: "sqlite".to_string(),
        db_path: stray_path
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
    };

    let database = impresspress_native::make_database_service(
        "sqlite",
        real_db.to_str().expect("db path is valid utf-8"),
        None,
    )
    .await
    .expect("construct sqlite service");
    impresspress_core::migration_helper::apply_ddl_via_service(
        &database,
        impresspress_core::blocks::admin::migrations::ddl_files("sqlite"),
    )
    .await
    .expect("apply admin tables");

    // Seed one grant the way the admin UI does: through the service.
    let mut grant: HashMap<String, serde_json::Value> = HashMap::new();
    grant.insert("grantee".into(), serde_json::json!("impresspress/files"));
    grant.insert(
        "resource".into(),
        serde_json::json!("impresspress__files__objects"),
    );
    grant.insert("write".into(), serde_json::json!(1));
    grant.insert("resource_type".into(), serde_json::json!("db"));
    database
        .create(impresspress_core::platform_state::wrap_grants::TABLE, grant)
        .await
        .expect("seed grant");

    let wafer = build_native_runtime(
        &infra,
        database,
        &HashMap::new(),
        Default::default(),
        false,
        AppHooks::none(),
    )
    .await
    .expect("build impresspress runtime");

    let grants = wafer.wrap_grants();
    assert!(
        grants.iter().any(|g| g.grantee == "impresspress/files"
            && g.resource == "impresspress__files__objects"
            && g.write == wafer_block::GrantWrite::Full),
        "a grant seeded through the database service must be installed on the runtime, got: {grants:?}"
    );
    assert!(
        !stray_path.exists(),
        "the boot must not open `db_path` as a SQLite file on its own"
    );
}
