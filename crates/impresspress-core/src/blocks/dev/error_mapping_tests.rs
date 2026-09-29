//! What a failed storage or database call answers on the `/b/dev` routes.
//!
//! A WRAP `PermissionDenied` is a 403 and a quota a 429, as on every other
//! block, and — this block's own rule (design §12) — the refusal still
//! carries `Cache-Control: no-store` (`super::no_store_db_error_internal`).
//! Each test drives a real route through [`DevBlock`]'s own dispatch over a
//! context that refuses the one call the site under test makes, so every
//! earlier step of the route runs for real.
//!
//! The body must be the door's own "Access denied": the status alone would
//! also match a route gate's refusal, which carries its own message.

use serde_json::json;
use wafer_block::ServiceOp;
use wafer_run::{context::Context, Block, ErrorCode, InputStream, WaferError};

use super::{
    repo,
    test_support::{dev_post, hello_info, FakeControl},
    DevBlock,
};
use crate::test_support::{
    admin_msg, output_json, FailingDbOpContext, FailingServiceOpContext, TestContext,
};

/// The refusal WRAP answers a call its caller holds no grant for. Its text
/// names the grant, which is deployment topology: logged, never shown.
fn wrap_denial() -> WaferError {
    WaferError::new(
        ErrorCode::PermissionDenied,
        "WRAP: impresspress/dev holds no grant on this resource",
    )
}

/// Every database op on `table`, refused the way WRAP refuses it.
fn denied(ctx: &TestContext, table: &'static str) -> FailingDbOpContext {
    FailingDbOpContext::failing_with(
        ctx.clone(),
        ServiceOp::DATABASE_OPS
            .iter()
            .map(|op| (*op, table))
            .collect(),
        wrap_denial(),
    )
}

/// `op` on `wafer-run/storage`, refused the way WRAP refuses it.
fn storage_denied(ctx: &TestContext, op: &'static str) -> FailingServiceOpContext {
    FailingServiceOpContext::failing_with(ctx.clone(), "wafer-run/storage", vec![op], wrap_denial())
}

/// A fresh sandbox whose compiled guests validate as `site/hello`.
async fn sandbox() -> TestContext {
    let control = FakeControl::new();
    control.set_validated_info(hello_info("site/hello"));
    TestContext::with_dev(control).await
}

/// Records a miss unless `method path` with `body`, through the block's own
/// `handle` over `ctx`, answers the door's WRAP denial: a `no-store` 403
/// whose message is "Access denied".
async fn expect_wrap_denial(
    misses: &mut Vec<String>,
    fixture: &TestContext,
    ctx: &dyn Context,
    (action, path, body): (&str, &str, serde_json::Value),
    site: &str,
) {
    let mut msg = admin_msg(action, path);
    msg.set_meta("http.header.accept", "application/json");
    let bytes = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(&body).expect("encode body")
    };
    let out = DevBlock::with_workspace(fixture.dev_shared())
        .handle(ctx, msg, InputStream::from_bytes(bytes))
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let message = serde_json::from_slice::<serde_json::Value>(&parts.body)
        .ok()
        .and_then(|body| body["message"].as_str().map(str::to_string));
    let no_store = parts
        .headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("cache-control") && value == "no-store");
    if (parts.status, message.as_deref(), no_store) != (403, Some("Access denied"), true) {
        misses.push(format!(
            "{site}: {} no-store={no_store} {}",
            parts.status,
            String::from_utf8_lossy(&parts.body)
        ));
    }
}

fn report(misses: Vec<String>) {
    assert!(
        misses.is_empty(),
        "expected the door's no-store WRAP denial at every site:\n{}",
        misses.join("\n")
    );
}

const INDEX: &str = "site/index.html";

/// Write `content` to `path` through the real route, returning its sha256.
async fn write(ctx: &TestContext, path: &str, content: &str) -> String {
    let written = output_json(
        dev_post(
            ctx,
            "/b/dev/api/files/write",
            json!({"path": path, "content": content}),
        )
        .await,
    )
    .await;
    written["sha256"]
        .as_str()
        .unwrap_or_else(|| panic!("write {path} failed: {written}"))
        .to_string()
}

/// The workspace file routes: every manifest load, blob read, blob write and
/// manifest save.
#[tokio::test]
async fn refused_workspace_storage_is_403_on_every_file_route() {
    let ctx = sandbox().await;
    let sha = write(&ctx, INDEX, "<p>hi</p>").await;
    let mut misses = Vec::new();

    let get = || storage_denied(&ctx, ServiceOp::STORAGE_GET);
    let put = || storage_denied(&ctx, ServiceOp::STORAGE_PUT);
    let read = || ("create", "/b/dev/api/files/read", json!({"path": INDEX}));
    let write_new = || {
        (
            "create",
            "/b/dev/api/files/write",
            json!({"path": "site/new.html", "content": "<p>new</p>"}),
        )
    };
    let delete = || {
        (
            "create",
            "/b/dev/api/files/delete",
            json!({"path": INDEX, "expected_sha256": sha}),
        )
    };

    let sites: Vec<(
        FailingServiceOpContext,
        (&str, &str, serde_json::Value),
        &str,
    )> = vec![
        (
            get(),
            ("retrieve", "/b/dev/api/files", serde_json::Value::Null),
            "list (the manifest load)",
        ),
        (get(), read(), "read (the manifest load)"),
        (get().after_passing(1), read(), "read (the blob read)"),
        (get(), write_new(), "write (the manifest load)"),
        (put(), write_new(), "write (the blob write)"),
        (
            put().after_passing(1),
            write_new(),
            "write (the manifest save)",
        ),
        (get(), delete(), "delete (the manifest load)"),
        (put(), delete(), "delete (the manifest save)"),
    ];
    for (failing, request, site) in sites {
        expect_wrap_denial(&mut misses, &ctx, &failing, request, site).await;
    }
    report(misses);
}

/// Scaffolding a block loads the manifest, writes each template file's blob,
/// then saves the manifest.
#[tokio::test]
async fn refused_workspace_storage_is_403_on_scaffold() {
    let create = || {
        (
            "create",
            "/b/dev/api/blocks",
            json!({"name": "greeter", "template": "hello"}),
        )
    };

    // How many puts a scaffold makes on a fresh workspace: one per new blob,
    // then the manifest. The save is the last of them.
    let counted = sandbox().await;
    let before = counted.storage_ops().len();
    let created = output_json(dev_post(&counted, create().1, create().2).await).await;
    assert_eq!(created["name"], "greeter", "{created}");
    let puts = counted.storage_ops()[before..]
        .iter()
        .filter(|op| op.starts_with("put "))
        .count();
    assert!(puts >= 2, "a blob and the manifest at least: {puts}");

    let ctx = sandbox().await;
    let mut misses = Vec::new();
    for (failing, site) in [
        (
            storage_denied(&ctx, ServiceOp::STORAGE_GET),
            "scaffold (the manifest load)",
        ),
        (
            storage_denied(&ctx, ServiceOp::STORAGE_PUT),
            "scaffold (the blob write)",
        ),
        (
            storage_denied(&ctx, ServiceOp::STORAGE_PUT).after_passing(puts - 1),
            "scaffold (the manifest save)",
        ),
    ] {
        expect_wrap_denial(&mut misses, &ctx, &failing, create(), site).await;
    }
    report(misses);
}

/// The status poll, the export, staging a build and removing a block each
/// read or write the ledger; a refused one is a 403.
#[tokio::test]
async fn refused_ledger_calls_are_403() {
    let ctx = sandbox().await;
    let mut misses = Vec::new();
    let state = || denied(&ctx, repo::runtime_state::TABLE);

    expect_wrap_denial(
        &mut misses,
        &ctx,
        &state(),
        ("retrieve", "/b/dev/api/status", serde_json::Value::Null),
        "status",
    )
    .await;
    expect_wrap_denial(
        &mut misses,
        &ctx,
        &state(),
        ("retrieve", "/b/dev/api/export", serde_json::Value::Null),
        "export",
    )
    .await;
    expect_wrap_denial(
        &mut misses,
        &ctx,
        &state(),
        (
            "create",
            "/b/dev/api/blocks/hello/remove",
            serde_json::Value::Null,
        ),
        "block remove",
    )
    .await;
    expect_wrap_denial(
        &mut misses,
        &ctx,
        &FailingDbOpContext::failing_with(
            ctx.clone(),
            vec![(ServiceOp::DATABASE_CREATE, repo::builds::TABLE)],
            wrap_denial(),
        ),
        (
            "create",
            "/b/dev/api/builds/stage",
            json!({
                "block_name": "hello",
                "artifact_base64": "AGFzbQEAAAA=",
                "compiler_version": "test",
                "diagnostics": [],
            }),
        ),
        "build stage (the build row)",
    )
    .await;
    report(misses);
}
