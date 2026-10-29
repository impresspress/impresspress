//! A native restart with a bootstrap token still configured boots.
//!
//! `auth::bootstrap::run` runs at every auth `Init` while no account exists,
//! and every boot runs `Init`. Until 2026-10 each one inserted the token's
//! `bootstrap_tokens` row again; its `token_hash` is UNIQUE, so the second
//! boot of a deployment provisioned by token failed auth's `Init` and refused
//! to start until someone redeemed the token. Now the first boot installs the
//! row and every later one finds it and leaves it as it is, expiry included.
//!
//! Each boot is the runtime the server binary builds and boots
//! (`build_native_runtime` + `boot_native`) over a freshly opened database
//! service, as a restarted process opens it, with the token in its
//! environment.
//!
//! The SQLite test runs everywhere. The PostgreSQL one needs a server, so it
//! is `#[ignore]`d; CI's `test-postgres` job runs it with
//! `IMPRESSPRESS_TEST_POSTGRES_URL` naming a database it may create others
//! from.

use std::collections::HashMap;

use impresspress_core::blocks::auth::{
    config::BOOTSTRAP_ADMIN_TOKEN_KEY, repo::bootstrap_tokens, service::hash_token,
};
use impresspress_native::{InfraConfig, ListenerEnv};
use impresspress_server::{boot_native, build_native_runtime, AppHooks};
use wafer_block::db::ListOptions;

const TOKEN: &str = "a bootstrap token for the restart test";

fn infra(db_type: &str, db_path: &str, db_url: Option<&str>, storage_root: &str) -> InfraConfig {
    InfraConfig {
        listen: "127.0.0.1:0".to_string(),
        db_type: db_type.to_string(),
        db_path: db_path.to_string(),
        db_url: db_url.map(str::to_string),
        storage_type: "local".to_string(),
        storage_root: storage_root.to_string(),
        model_cache_dir: "data/models".to_string(),
        listener: ListenerEnv::default(),
    }
}

/// Build and boot the native runtime over `infra`'s database, with `token`
/// configured when there is one, require every block to initialize, and shut
/// it down.
async fn boot(infra: &InfraConfig, token: Option<&str>) {
    let database = impresspress_native::make_database_service(
        &infra.db_type,
        &infra.db_path,
        infra.db_url.as_deref(),
    )
    .await
    .expect("open the database");
    let app_env: HashMap<String, String> = token
        .map(|token| (BOOTSTRAP_ADMIN_TOKEN_KEY.to_string(), token.to_string()))
        .into_iter()
        .collect();
    let hooks = AppHooks {
        register_blocks: Box::new(Ok),
        register_post_build: Box::new(|_, _| Ok(())),
    };
    let mut wafer = build_native_runtime(infra, database, &app_env, Default::default(), hooks)
        .await
        .expect("build the native runtime");
    let report = boot_native(&mut wafer)
        .await
        .expect("boot the native runtime");
    assert!(report.ok, "every block initializes: {report:?}");
    wafer.shutdown().await;
}

/// Every `bootstrap_tokens` row, as `(token_hash, expires_at)`.
async fn token_rows(infra: &InfraConfig) -> Vec<(String, String)> {
    let database = impresspress_native::make_database_service(
        &infra.db_type,
        &infra.db_path,
        infra.db_url.as_deref(),
    )
    .await
    .expect("open the database");
    let text = |row: &wafer_core::interfaces::database::service::Record, column: &str| {
        row.data
            .get(column)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("{column} is text: {row:?}"))
            .to_string()
    };
    database
        .list(bootstrap_tokens::TABLE, &ListOptions::default())
        .await
        .expect("list bootstrap tokens")
        .records
        .iter()
        .map(|row| (text(row, "token_hash"), text(row, "expires_at")))
        .collect()
}

/// The first boot installs the token; two restarts leave its one row exactly
/// as the first boot wrote it.
async fn a_restart_keeps_the_first_boot_s_token(infra: &InfraConfig) {
    boot(infra, Some(TOKEN)).await;
    let first = token_rows(infra).await;
    assert_eq!(first.len(), 1, "the first boot installs the token");
    assert_eq!(
        first[0].0,
        impresspress_core::util::hex_encode(&hash_token(TOKEN)),
        "the row is the configured token's"
    );

    boot(infra, Some(TOKEN)).await;
    boot(infra, Some(TOKEN)).await;
    assert_eq!(
        token_rows(infra).await,
        first,
        "a restart leaves the first boot's row, expiry included"
    );
}

/// Two runtimes whose auth `Init`s install the same token at the same time —
/// two Cloudflare isolates building together, here two native runtimes over
/// one database, each with its own connections — both boot, and leave one
/// row. The first boot, with no token, has applied the migrations, so the two
/// race only on the install. (`start_native` would serialize two processes
/// on the database's migration lock; the runtimes are built directly to let
/// them overlap.)
async fn two_runtimes_installing_at_once_leave_one_row(infra: &InfraConfig) {
    boot(infra, None).await;
    assert!(token_rows(infra).await.is_empty());

    tokio::join!(boot(infra, Some(TOKEN)), boot(infra, Some(TOKEN)));
    assert_eq!(token_rows(infra).await.len(), 1);
}

/// A fresh SQLite database file and storage root under `tmp`.
fn sqlite_infra(tmp: &tempfile::TempDir) -> InfraConfig {
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let db_path = tmp.path().join("bootstrap.sqlite3");
    infra(
        "sqlite",
        db_path.to_str().expect("UTF-8 path"),
        None,
        storage_root.to_str().expect("UTF-8 path"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn two_runtimes_installing_the_token_at_once_boot_on_sqlite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    two_runtimes_installing_at_once_leave_one_row(&sqlite_infra(&tmp)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_with_the_token_still_configured_boots_on_sqlite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    a_restart_keeps_the_first_boot_s_token(&sqlite_infra(&tmp)).await;
}

/// A fresh PostgreSQL database named `name` on the server
/// `IMPRESSPRESS_TEST_POSTGRES_URL` names, and a storage root under `storage`.
async fn postgres_infra(name: &str, storage: &tempfile::TempDir) -> InfraConfig {
    let url = std::env::var("IMPRESSPRESS_TEST_POSTGRES_URL")
        .expect("IMPRESSPRESS_TEST_POSTGRES_URL must name a PostgreSQL database");
    let server = impresspress_native::make_database_service("postgres", "", Some(&url))
        .await
        .expect("connect to PostgreSQL");
    // Test-fixture setup: a database of its own, created fresh.
    server
        .exec_raw(&format!("DROP DATABASE IF EXISTS {name}"), &[])
        .await
        .expect("drop the test's database");
    server
        .exec_raw(&format!("CREATE DATABASE {name}"), &[])
        .await
        .expect("create the test's database");
    let (base, _) = url.rsplit_once('/').expect("a database URL");
    infra(
        "postgres",
        "",
        Some(&format!("{base}/{name}")),
        storage.path().to_str().expect("UTF-8 path"),
    )
}

/// Both scenarios against PostgreSQL, each on a database of its own.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn the_same_holds_on_postgres() {
    let storage = tempfile::tempdir().expect("tempdir");
    a_restart_keeps_the_first_boot_s_token(
        &postgres_infra("impresspress_bootstrap_token_restart", &storage).await,
    )
    .await;
    two_runtimes_installing_at_once_leave_one_row(
        &postgres_infra("impresspress_bootstrap_token_at_once", &storage).await,
    )
    .await;
}
