//! A native server whose HTTP listener cannot start does not boot.
//!
//! A native boot refuses to start when any block's `Init` fails. The listener
//! is the block that binds the socket, and its `Init` is where an
//! `IMPRESSPRESS_*` listener setting is validated: a process that went on
//! would report itself started and serve nothing. This drives the binary's own boot
//! (`build_native_runtime`, `register_http_listener`, `boot_native`) with a
//! listener setting the listener refuses.

use std::{collections::HashMap, path::Path};

use impresspress_native::{register_http_listener, InfraConfig, ListenerEnv};
use impresspress_server::{boot_native, build_native_runtime, AppHooks};

fn infra_for(db_path: &Path, storage_root: &Path, listener: ListenerEnv) -> InfraConfig {
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
        listener,
    }
}

async fn boot_with(listener: ListenerEnv) -> anyhow::Result<()> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("listener_boot.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let infra = infra_for(&db_path, &storage_root, listener);
    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    let mut wafer = build_native_runtime(
        &infra,
        database,
        &HashMap::new(),
        Default::default(),
        AppHooks::none(),
    )
    .await
    .expect("build impresspress runtime");
    register_http_listener(&mut wafer, &infra.listen, "site-main", &infra.listener);
    boot_native(&mut wafer).await.map(|_| ())
}

#[tokio::test]
async fn a_listener_setting_the_listener_refuses_fails_the_boot() {
    let error = boot_with(ListenerEnv {
        max_connections: Some("0".into()),
        ..Default::default()
    })
    .await
    .expect_err("a listener that cannot start must fail the boot");
    let message = format!("{error:#}");
    assert!(message.contains("refusing to start"), "{message}");
    assert!(message.contains("`wafer-run/http-listener`"), "{message}");
    assert!(message.contains("max_connections"), "{message}");
}

#[tokio::test]
async fn the_default_listener_boots() {
    boot_with(ListenerEnv::default())
        .await
        .expect("the listener starts on its defaults");
}
