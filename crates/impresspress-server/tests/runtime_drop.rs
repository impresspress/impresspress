//! A booted runtime that has served auth checks is freed when it is dropped.
//!
//! The probe is a `Weak` on the registered `wafer-run/auth` block. The
//! runtime's context owns every registered block, so anything that keeps a
//! context of this runtime alive keeps that block alive too. Until 2026-10
//! `AuthServiceImpl::init` stored its `Init` context in the service, which the
//! auth block owns: a reference cycle, and no runtime was ever freed (about
//! 13 MB of Wasm memory per build on Cloudflare). `AuthService` now hands
//! the service the `ctx` of each call instead.
//!
//! Before the drop, the runtime answers `require_user` for a caller block
//! through the real dispatch: the service verifies an access JWT in the
//! auth block's frame for that call, reading the JWT secret through that
//! frame's `config_get` (the caller's per-call config over the runtime's) and
//! the user's `auth_version` through its `call_block`. A token signed with
//! another secret is refused, so the secret it read is the deployment's.
//!
//! The runtime is the one the server binary builds and boots
//! (`build_native_runtime` + `boot_native`), with this crate's features: the
//! default `impresspress-core` block set, which is every block a native build
//! ships. The same probe runs under wasm-bindgen-test in
//! `impresspress-browser` (`boot/tests.rs`), over the boot funnel the browser
//! shares with native.

use std::{collections::HashMap, sync::Arc, time::Duration};

use impresspress_core::blocks::{
    auth::{
        config::{BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY},
        JWT_SECRET_KEY,
    },
    auth_ui::AUTH_UI_BLOCK_ID,
};
use impresspress_native::{InfraConfig, ListenerEnv};
use impresspress_server::{boot_native, build_native_runtime, AppHooks};
use wafer_block::{db::ListOptions, streams::output::TerminalNotResponse};
use wafer_run::{context::Context, InputStream, LifecycleEvent, Message, OutputStream, WaferError};

const ADMIN_EMAIL: &str = "admin@runtime-drop.test";
/// The block that asks `wafer-run/auth` who its caller is.
const CALLER: &str = "test/whoami";

/// Answers every request with the user `require_user` resolves its
/// credential to, through the typed auth client, as any feature block does.
struct WhoAmI;

#[wafer_block::wafer_async_trait]
impl wafer_run::Block for WhoAmI {
    fn info(&self) -> wafer_run::BlockInfo {
        wafer_run::BlockInfo::new(CALLER, "0.0.1", "http-handler@v1", "resolves its caller")
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        match wafer_core::clients::auth::require_user(ctx, &msg).await {
            Ok(id) => OutputStream::respond(id.into_bytes()),
            Err(e) => OutputStream::error(e),
        }
    }
}

/// An access JWT for `sub`, signed the way auth-ui signs one: with the key
/// derived from `master_secret` for `impresspress/auth-ui`.
fn access_token(master_secret: &str, sub: &str) -> String {
    use wafer_block_crypto::primitives;

    let key = primitives::derive_block_key(master_secret.as_bytes(), AUTH_UI_BLOCK_ID);
    let claims = std::collections::BTreeMap::from([
        ("sub".to_string(), serde_json::json!(sub)),
        ("type".to_string(), serde_json::json!("access")),
        // `crypto::expected_issuer`'s default: no frontend URL is configured.
        (
            "iss".to_string(),
            serde_json::json!("http://localhost:5173"),
        ),
        ("roles".to_string(), serde_json::json!(["admin"])),
    ]);
    primitives::jwt_sign(claims, Duration::from_secs(3600), key.as_bytes()).expect("sign")
}

/// What `require_user` answers `CALLER` for a request carrying `token`.
async fn whoami(wafer: &wafer_run::Wafer, token: &str) -> Result<String, String> {
    let mut msg = Message::new("whoami");
    msg.set_meta("http.header.authorization", format!("Bearer {token}"));
    let out = wafer
        .run_block(CALLER, msg, InputStream::from_bytes(Vec::new()))
        .await;
    match out.collect_buffered().await {
        Ok(resp) => Ok(String::from_utf8(resp.body).expect("UTF-8 user id")),
        Err(TerminalNotResponse::Error(e)) => Err(format!("{:?}: {}", e.code, e.message)),
        Err(other) => panic!("no response or error: {other:?}"),
    }
}

#[tokio::test]
async fn a_runtime_that_served_auth_checks_is_freed_when_dropped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let infra = InfraConfig {
        listen: "127.0.0.1:0".to_string(),
        db_type: "sqlite".to_string(),
        db_path: tmp
            .path()
            .join("drop.sqlite3")
            .to_str()
            .expect("UTF-8 path")
            .to_string(),
        db_url: None,
        storage_type: "local".to_string(),
        storage_root: storage_root.to_str().expect("UTF-8 path").to_string(),
        model_cache_dir: "data/models".to_string(),
        listener: ListenerEnv::default(),
    };
    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    let app_env = HashMap::from([
        (
            BOOTSTRAP_ADMIN_EMAIL_KEY.to_string(),
            ADMIN_EMAIL.to_string(),
        ),
        (
            BOOTSTRAP_ADMIN_PASSWORD_KEY.to_string(),
            "a long bootstrap password for the drop test".to_string(),
        ),
    ]);
    let hooks = AppHooks {
        register_blocks: Box::new(|builder| Ok(builder.extra_block(CALLER, Arc::new(WhoAmI)))),
        register_post_build: Box::new(|_, _| Ok(())),
    };

    let mut wafer = build_native_runtime(
        &infra,
        database.clone(),
        &app_env,
        Default::default(),
        hooks,
    )
    .await
    .expect("build the native runtime");
    let report = boot_native(&mut wafer)
        .await
        .expect("boot the native runtime");
    assert!(report.ok, "every block initializes: {report:?}");

    // The deployment's JWT master secret, generated when the variables were
    // seeded, and the bootstrap admin auth's `Init` created.
    let variables = database
        .list("impresspress__admin__variables", &ListOptions::default())
        .await
        .expect("list variables");
    let secret = variables
        .records
        .iter()
        .find(|r| r.data.get("key") == Some(&serde_json::json!(JWT_SECRET_KEY)))
        .and_then(|r| r.data.get("value")?.as_str())
        .expect("the JWT secret is seeded")
        .to_string();
    let users = database
        .list("wafer_run__auth__users", &ListOptions::default())
        .await
        .expect("list users");
    let admin = users
        .records
        .iter()
        .find(|r| r.data.get("email") == Some(&serde_json::json!(ADMIN_EMAIL)))
        .expect("the bootstrap admin exists");

    assert_eq!(
        whoami(&wafer, &access_token(&secret, &admin.id)).await,
        Ok(admin.id.clone()),
        "a token signed with the deployment's secret resolves to its user"
    );
    let foreign = whoami(
        &wafer,
        &access_token("some-other-secret-0123456789abcdef", &admin.id),
    )
    .await;
    assert!(
        foreign.is_err(),
        "a token signed with another secret must not authenticate: {foreign:?}"
    );

    let probe = Arc::downgrade(
        &wafer
            .lookup_block("wafer-run/auth")
            .expect("the auth block is registered")
            .1,
    );
    drop(wafer);
    assert!(
        probe.upgrade().is_none(),
        "the auth block outlived its runtime: something keeps a context of it"
    );
}
