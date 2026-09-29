//! What a failed database or storage call answers on the files routes.
//!
//! A WRAP `PermissionDenied` — a [`wafer_run::ResourceGrant`] the deployment
//! is missing — is a 403, and a quota is a 429, the same as on every other
//! block (`crud::db_error_internal`, `crud::db_error_page`). Each test drives
//! a real route through [`FilesBlock`]'s own dispatch over a context that
//! refuses the one call the site under test makes, so every earlier step of
//! the route runs for real.
//!
//! A JSON route must end in the door's own "Access denied": the status alone
//! would also match a route gate's refusal, which carries its own message. A
//! full page must be the styled 403 `ui::refused_response` draws ("Go home"),
//! with none of the denial's own text in it.

use wafer_block::ServiceOp;
use wafer_run::{
    context::Context, streams::output::TerminalNotResponse, Block, ErrorCode, InputStream, Message,
    OutputStream, WaferError,
};

use super::{
    repo,
    test_support::{share_ctx, upload},
    FilesBlock,
};
use crate::test_support::{
    admin_msg, anon_msg, auth_msg, FailingDbOpContext, FailingServiceOpContext, TestContext,
};

/// The refusal WRAP answers a call its caller holds no grant for. Its text
/// names the grant, which is deployment topology: logged, never shown.
fn wrap_denial() -> WaferError {
    WaferError::new(
        ErrorCode::PermissionDenied,
        "WRAP: impresspress/files holds no grant on this resource",
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

/// `ops` on `wafer-run/storage`, refused the way WRAP refuses them.
fn storage_denied(ctx: &TestContext, ops: Vec<&'static str>) -> FailingServiceOpContext {
    FailingServiceOpContext::failing_with(ctx.clone(), "wafer-run/storage", ops, wrap_denial())
}

/// `msg` with `body` through the block's own `handle`.
async fn api(ctx: &dyn Context, msg: Message, body: &[u8]) -> OutputStream {
    let mut msg = msg;
    msg.set_meta("http.header.accept", "application/json");
    FilesBlock::new()
        .handle(ctx, msg, InputStream::from_bytes(body.to_vec()))
        .await
}

/// Records a miss unless the request ended in the door's WRAP denial:
/// `PermissionDenied` with its own "Access denied".
async fn expect_wrap_denial(misses: &mut Vec<String>, out: OutputStream, site: &str) {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(error))
            if (error.code, error.message.as_str())
                == (ErrorCode::PermissionDenied, "Access denied") => {}
        Err(TerminalNotResponse::Error(error)) => {
            misses.push(format!("{site}: {:?} {:?}", error.code, error.message))
        }
        Ok(_) => misses.push(format!("{site}: a response, not a WRAP denial")),
        Err(_) => misses.push(format!("{site}: another terminal, not a WRAP denial")),
    }
}

/// Records a miss unless a browser `GET` of `msg` is the styled 403 a
/// refused read gets, with none of the denial's text.
async fn expect_refused_page(misses: &mut Vec<String>, ctx: &dyn Context, msg: Message) {
    let path = msg.path().to_string();
    let mut msg = msg;
    msg.set_meta("http.header.accept", "text/html");
    let out = FilesBlock::new()
        .handle(ctx, msg, InputStream::empty())
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let html = String::from_utf8_lossy(&parts.body);
    if parts.status != 403 || !html.contains("Go home") || html.contains("holds no grant") {
        misses.push(format!("{path}: {} {html}", parts.status));
    }
}

fn report(misses: Vec<String>) {
    assert!(
        misses.is_empty(),
        "expected the door's WRAP denial at every site:\n{}",
        misses.join("\n")
    );
}

/// `alice` owning bucket `photos` holding `a.png`, with the crypto and
/// storage blocks a share round trip needs.
async fn with_a_file() -> TestContext {
    let ctx = share_ctx("photos", "alice").await;
    upload(&ctx, "photos", "a.png", b"PNG", "image/png", "alice").await;
    ctx
}

fn user(action: &str, path: &str) -> Message {
    auth_msg(action, path, "alice")
}

/// The share, quota and access-log API reads and writes in `cloud.rs`.
#[tokio::test]
async fn refused_cloud_api_calls_are_403() {
    let ctx = with_a_file().await;
    let shares = repo::shares::TABLE;
    let mut misses = Vec::new();

    let sites: Vec<(&str, FailingDbOpContext, Message, &[u8])> = vec![
        (
            "GET /b/cloudstorage/shares",
            denied(&ctx, shares),
            user("retrieve", "/b/cloudstorage/shares"),
            b"",
        ),
        (
            "POST /b/cloudstorage/shares (the insert)",
            FailingDbOpContext::failing_with(
                ctx.clone(),
                vec![(ServiceOp::DATABASE_CREATE, shares)],
                wrap_denial(),
            ),
            user("create", "/b/cloudstorage/shares"),
            br#"{"bucket":"photos","key":"a.png"}"#,
        ),
        (
            "GET /b/cloudstorage/quota (the quota)",
            denied(&ctx, repo::quota::TABLE),
            user("retrieve", "/b/cloudstorage/quota"),
            b"",
        ),
        (
            "GET /b/cloudstorage/quota (the usage)",
            denied(&ctx, repo::objects::TABLE),
            user("retrieve", "/b/cloudstorage/quota"),
            b"",
        ),
        (
            "GET /b/cloudstorage/admin/shares",
            denied(&ctx, shares),
            admin_msg("retrieve", "/b/cloudstorage/admin/shares"),
            b"",
        ),
        (
            "GET /b/cloudstorage/admin/access-logs",
            denied(&ctx, repo::shares::ACCESS_LOGS_TABLE),
            admin_msg("retrieve", "/b/cloudstorage/admin/access-logs"),
            b"",
        ),
        (
            "GET /b/cloudstorage/admin/quotas",
            denied(&ctx, repo::quota::TABLE),
            admin_msg("retrieve", "/b/cloudstorage/admin/quotas"),
            b"",
        ),
        (
            "PATCH /b/cloudstorage/admin/quotas/{id}",
            denied(&ctx, repo::quota::TABLE),
            admin_msg("update", "/b/cloudstorage/admin/quotas/alice"),
            br#"{"max_files_per_bucket":5}"#,
        ),
    ];
    for (site, failing, msg, body) in sites {
        expect_wrap_denial(&mut misses, api(&failing, msg, body).await, site).await;
    }
    report(misses);
}

/// The bucket, search, recent and upload API sites in `storage/`.
#[tokio::test]
async fn refused_storage_api_calls_are_403() {
    let ctx = with_a_file().await;
    let mut misses = Vec::new();

    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, repo::buckets::TABLE),
            user("retrieve", "/b/storage/api/buckets"),
            b"",
        )
        .await,
        "GET /b/storage/api/buckets",
    )
    .await;
    expect_wrap_denial(
        &mut misses,
        api(
            &storage_denied(&ctx, vec![ServiceOp::STORAGE_CREATE_FOLDER]),
            user("create", "/b/storage/api/buckets"),
            br#"{"name":"docs"}"#,
        )
        .await,
        "POST /b/storage/api/buckets (the storage folder)",
    )
    .await;

    let mut search = user("retrieve", "/b/storage/api/search");
    search.set_meta("req.query.q", "a");
    expect_wrap_denial(
        &mut misses,
        api(&denied(&ctx, repo::objects::TABLE), search, b"").await,
        "GET /b/storage/api/search",
    )
    .await;
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, repo::views::TABLE),
            user("retrieve", "/b/storage/api/recent"),
            b"",
        )
        .await,
        "GET /b/storage/api/recent",
    )
    .await;

    let upload = || {
        let mut msg = user("create", "/b/storage/api/buckets/photos/objects");
        msg.set_meta("req.query.key", "b.png");
        msg.set_meta("req.content_type", "image/png");
        msg
    };
    expect_wrap_denial(
        &mut misses,
        api(&denied(&ctx, repo::quota::TABLE), upload(), b"PNG").await,
        "POST …/objects (the quota lookup)",
    )
    .await;
    expect_wrap_denial(
        &mut misses,
        api(
            &storage_denied(
                &ctx,
                vec![ServiceOp::STORAGE_PUT, ServiceOp::STORAGE_PUT_STREAMING],
            ),
            upload(),
            b"PNG",
        )
        .await,
        "POST …/objects (the blob write)",
    )
    .await;

    report(misses);
}

/// A public share link spends one access before it serves the file; a
/// refused spend is a 403, not a 500.
#[tokio::test]
async fn a_refused_share_access_spend_is_403() {
    let ctx = with_a_file().await;
    repo::shares::insert(
        &ctx,
        repo::shares::NewShare {
            token: "share-token-1",
            bucket: "photos",
            key: "a.png",
            created_by: "alice",
            created_at: "2026-09-05T00:00:00Z",
            expires_at: "2099-09-05T00:00:00Z",
            max_access_count: None,
        },
    )
    .await
    .expect("seed share");
    let mut misses = Vec::new();
    // The token lookup is the first call on the table and passes; the spend
    // is the second.
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, repo::shares::TABLE).after_passing(1),
            anon_msg("retrieve", "/b/storage/direct/share-token-1"),
            b"",
        )
        .await,
        "GET /b/storage/direct/{token} (the access spend)",
    )
    .await;
    report(misses);
}

/// Every server-rendered files page answers a refused read with the 403 page.
#[tokio::test]
async fn refused_page_reads_are_the_403_page() {
    let ctx = with_a_file().await;
    let mut misses = Vec::new();

    for (table, path) in [
        (repo::buckets::TABLE, "/b/storage/admin/"),
        (repo::buckets::TABLE, "/b/storage/admin/buckets"),
        (repo::shares::TABLE, "/b/storage/admin/shares"),
        (repo::quota::TABLE, "/b/storage/admin/quotas"),
    ] {
        expect_refused_page(
            &mut misses,
            &denied(&ctx, table),
            admin_msg("retrieve", path),
        )
        .await;
    }
    for (table, path) in [
        (repo::buckets::TABLE, "/b/storage/"),
        (repo::objects::TABLE, "/b/storage/photos/"),
        (repo::shares::TABLE, "/b/cloudstorage/"),
        (repo::quota::TABLE, "/b/cloudstorage/"),
    ] {
        expect_refused_page(&mut misses, &denied(&ctx, table), user("retrieve", path)).await;
    }
    report(misses);
}

/// The bucket-ownership read is the authorization on every bucket-scoped
/// route. A refused read is the door's WRAP denial — not "you do not own this
/// bucket" (the JSON 403 with its own message), and not the portal's 404.
#[tokio::test]
async fn a_refused_ownership_read_is_the_doors_denial() {
    let ctx = with_a_file().await;
    let buckets = || denied(&ctx, repo::buckets::TABLE);
    let mut misses = Vec::new();

    expect_wrap_denial(
        &mut misses,
        api(
            &buckets(),
            user("create", "/b/cloudstorage/shares"),
            br#"{"bucket":"photos","key":"a.png"}"#,
        )
        .await,
        "POST /b/cloudstorage/shares (the ownership read)",
    )
    .await;
    expect_wrap_denial(
        &mut misses,
        api(
            &buckets(),
            user("retrieve", "/b/storage/api/buckets/photos/objects"),
            b"",
        )
        .await,
        "GET …/objects (the ownership read)",
    )
    .await;
    expect_refused_page(
        &mut misses,
        &buckets(),
        user("retrieve", "/b/storage/photos/"),
    )
    .await;
    report(misses);
}

/// The config service, refusing every call the way WRAP refuses one, under
/// the declaration of the block it stands in for.
struct RefusingConfig(std::sync::Arc<dyn Block>);

#[wafer_block::wafer_async_trait]
impl Block for RefusingConfig {
    fn info(&self) -> wafer_run::BlockInfo {
        self.0.info()
    }

    async fn handle(&self, _: &dyn Context, _: Message, _: InputStream) -> OutputStream {
        OutputStream::error(wrap_denial())
    }
}

/// A share's lifetime is capped by a configured ceiling. A ceiling that could
/// not be read is not the default: a deployment that lowered it would hand
/// out the longer default link while its config store is unreachable. The
/// share is refused through the door instead, as an upload is when its quota
/// cannot be read.
#[tokio::test]
async fn an_unreadable_share_expiry_ceiling_refuses_the_share() {
    let mut ctx = with_a_file().await;
    let config = ctx
        .blocks
        .lock()
        .expect("blocks")
        .get("wafer-run/config")
        .cloned()
        .expect("the fixture registers a config block");
    ctx.register_block(
        "wafer-run/config",
        std::sync::Arc::new(RefusingConfig(config)),
    );
    let mut misses = Vec::new();
    expect_wrap_denial(
        &mut misses,
        api(
            &ctx,
            user("create", "/b/cloudstorage/shares"),
            br#"{"bucket":"photos","key":"a.png"}"#,
        )
        .await,
        "POST /b/cloudstorage/shares (the expiry ceiling)",
    )
    .await;
    report(misses);
}
