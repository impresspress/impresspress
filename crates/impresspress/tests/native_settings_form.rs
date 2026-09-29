//! An admin settings page renders its form on the runtime the binary builds.
//!
//! `ui::settings_form` reads the current values through
//! `blocks::config::get_many`, an operation on the config block this repo
//! registers in place of wafer-core's. A `call_block` is validated against the
//! TARGET's declared interface (`RuntimeContext::dispatch_call` →
//! `runtime::validation::check_action_interface`), and under `config@v1` —
//! whose action map is exactly `config.get` and `config.set` — the runtime
//! refused that call before the block saw it, so all five settings pages
//! answered 500 on a server while every unit test rendered them.
//!
//! `TestContext::call_block` runs the same check now, but against a
//! hand-maintained spec set (`test_support::interface_specs`) and the block
//! object a test registered by hand. Only here does the page meet the
//! runtime's own set: the specs `Wafer::register_interface` collected, the
//! blocks `register_block` collected, both frozen by `seal()`, and the
//! validator reading what that boot produced. A spec the fixture lists but
//! `builder::registration` never registers is invisible to every unit test
//! and decisive here.
//!
//! This drives the page through `Wafer::run_block` — the entry point the HTTP
//! listener uses — on a runtime from `build_native_runtime`, the binary's own.

use std::{collections::HashMap, path::Path};

use impresspress_core::builder::{boot, GrantSource, InitPolicy};
use impresspress_native::InfraConfig;
use impresspress_server::{build_native_runtime, AppHooks, NativeBootHooks};
use wafer_run::{InputStream, Message};

/// Native reads the admin-created grants out of the platform database and
/// installs them before `build()`; see `native_wrap_grants.rs`.
const NATIVE_GRANTS: GrantSource<'static> = GrantSource::PreInstalled(
    "build_native_runtime loads them from the platform database into \
     ImpresspressBuilder::wrap_grants before build()",
);

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

#[tokio::test]
async fn the_admin_email_settings_page_renders_its_form_on_the_real_runtime() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("native_settings_form.sqlite3");
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
    boot(
        &mut wafer,
        &NativeBootHooks,
        NATIVE_GRANTS,
        InitPolicy::Reported,
    )
    .await
    .expect("boot");

    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", "/b/admin/settings/email");
    msg.set_meta("http.header.accept", "text/html");
    msg.set_meta("auth.user_id", "admin_1");
    msg.set_meta("auth.user_roles", "admin");

    let out = wafer
        .run_block("impresspress/admin", msg, InputStream::empty())
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let html = String::from_utf8(parts.body).expect("UTF-8 body");

    assert_eq!(parts.status, 200, "{html}");
    assert!(
        html.contains(r#"<form id="settings-form""#),
        "the settings form must render: {html}"
    );
    assert!(
        html.contains(r#"name="IMPRESSPRESS__EMAIL__MAILGUN_DOMAIN""#),
        "with its declared fields: {html}"
    );
}
