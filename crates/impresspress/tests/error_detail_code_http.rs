//! A block's detail code reaches a native HTTP client as the body's `code`.
//!
//! `impresspress_core::blocks::errors::error_response` attaches its precise
//! code (`invalid_credentials`, `email_not_verified`, …) with
//! `WaferError::with_detail_code`, and the JS SDK reads it from the error
//! body's `code` field (`packages/impresspress-js/src/http-client.ts`). On
//! native the body is rendered by `wafer-block-http-listener`, whose
//! `wafer_output_to_response` is `http_codec::collect_http_response` plus
//! axum glue, so this drives a real request through the runtime the binary
//! builds — `build_native_runtime`, the shared `boot`, the `site-main` flow,
//! the router and the auth-ui login handler — builds the request message with the
//! listener's own `http_codec::build_http_message`, and renders the answer
//! with the listener's own `collect_http_response`.

use std::{collections::HashMap, path::Path};

use impresspress_core::builder::{boot, GrantSource, InitPolicy};
use impresspress_native::InfraConfig;
use impresspress_server::{build_native_runtime, AppHooks, NativeBootHooks};
use wafer_block::{http_codec, InputStream};

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

/// **Fails on the previous wafer-run pin**, whose codec rendered every error
/// as `{"error", "message"}`: the detail code reached a client only through
/// the browser adapter, which added it by hand.
#[tokio::test]
async fn a_refused_login_is_answered_with_its_detail_code() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("error_detail_code.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let infra = infra_for(&db_path, &storage_root);
    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    let mut wafer = build_native_runtime(
        &infra,
        database,
        &HashMap::new(),
        Default::default(),
        false,
        AppHooks::none(),
    )
    .await
    .expect("build impresspress runtime");
    let report = boot(
        &mut wafer,
        &NativeBootHooks,
        GrantSource::PreInstalled(
            "build_native_runtime loads them from the platform database into \
             ImpresspressBuilder::wrap_grants before build()",
        ),
        InitPolicy::Reported,
    )
    .await
    .expect("seal");
    assert!(report.ok, "boot must succeed: {report:?}");
    wafer.run_start_lifecycle().await;
    let wafer = wafer.bind_all();

    let body = br#"{"email":"nobody@example.com","password":"not-the-password"}"#;
    let msg = http_codec::build_http_message(
        "POST",
        "/b/auth/api/login",
        "",
        "127.0.0.1",
        [
            ("host", "localhost"),
            ("origin", "http://localhost"),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/json"),
            ("accept", "application/json"),
        ],
    );
    let parts = http_codec::collect_http_response(
        wafer
            .run("site-main", msg, InputStream::from_bytes(body.to_vec()))
            .await,
    )
    .await;

    assert_eq!(
        parts.status,
        401,
        "body: {}",
        String::from_utf8_lossy(&parts.body)
    );
    let body: serde_json::Value =
        serde_json::from_slice(&parts.body).expect("an error body is JSON");
    assert_eq!(
        body,
        serde_json::json!({
            "error": "Unauthenticated",
            "message": "Invalid email or password",
            "code": "invalid_credentials",
        })
    );
}
