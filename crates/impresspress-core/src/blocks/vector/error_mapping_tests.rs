//! What a failed vector-service, embedding or registry call answers on the
//! vector routes.
//!
//! The vector service and the embedding blocks are WRAP-authorized like the
//! database, and their refusals carry the same codes: a
//! `PermissionDenied` is a 403 and a quota a 429 (`crud::db_error_internal`),
//! not the sanitized 500. Each test
//! drives a real route through [`VectorBlock`]'s own dispatch over a context
//! that refuses the one call the site under test makes.
//!
//! A JSON route must end in the door's own "Access denied"; a full page must
//! be the styled 403 `ui::refused_response` draws ("Go home"), with none of
//! the denial's own text in it.

use std::sync::Arc;

use wafer_block::{
    common::ServiceOp,
    wire::{
        database::OnConflict,
        vector::{CountResponse, ListIdsResponse, ListIndexesResponse},
    },
};
use wafer_core::{clients::database as db, interfaces::vector::DEFAULT_MODEL};
use wafer_run::{
    context::Context, streams::output::TerminalNotResponse, Block, BlockCategory, BlockInfo,
    ErrorCode, InputStream, LifecycleEvent, Message, OutputStream, WaferError,
};

use super::{
    service::{prefixed_index_name, REGISTRY_TABLE},
    VectorBlock,
};
use crate::test_support::{admin_msg, FailingDbOpContext, TestContext};

/// The refusal WRAP answers a call its caller holds no grant for. Its text
/// names the grant, which is deployment topology: logged, never shown.
fn wrap_denial() -> WaferError {
    WaferError::new(
        ErrorCode::PermissionDenied,
        "WRAP: impresspress/vector holds no grant on this target",
    )
}

/// A service block named `name`, implementing `interface`, that answers the
/// ops in `refuse` (every op, for [`ALL`]) with [`wrap_denial`], and otherwise
/// acknowledges the vector writes, lists `docs` as its one index holding
/// three vectors, and lists no prior chunks.
struct Service {
    name: &'static str,
    interface: &'static str,
    refuse: &'static [&'static str],
}

/// Every op refused.
const ALL: &[&str] = &["*"];

#[async_trait::async_trait]
impl Block for Service {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.0.1", self.interface, "scripted service")
            .category(BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        if self.refuse.iter().any(|op| *op == "*" || *op == msg.kind) {
            return OutputStream::error(wrap_denial());
        }
        match msg.kind.as_str() {
            ServiceOp::VECTOR_CREATE_INDEX
            | ServiceOp::VECTOR_DELETE_INDEX
            | ServiceOp::VECTOR_UPSERT
            | ServiceOp::VECTOR_DELETE => OutputStream::respond(Vec::new()),
            ServiceOp::VECTOR_LIST_INDEXES => OutputStream::respond(
                wafer_block::codec::encode(&ListIndexesResponse {
                    indexes: vec![prefixed_index_name("docs")],
                })
                .expect("encode"),
            ),
            ServiceOp::VECTOR_COUNT => OutputStream::respond(
                wafer_block::codec::encode(&CountResponse { count: 3 }).expect("encode"),
            ),
            ServiceOp::VECTOR_LIST_IDS => OutputStream::respond(
                wafer_block::codec::encode(&ListIdsResponse { ids: Vec::new() }).expect("encode"),
            ),
            other => OutputStream::error(WaferError::new(
                ErrorCode::Unimplemented,
                format!("scripted service: unhandled op {other}"),
            )),
        }
    }

    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
}

/// A vector fixture whose `wafer-run/vector` refuses `refuse_vector`, with
/// an embedding block that refuses every op.
async fn fixture(refuse_vector: &'static [&'static str]) -> TestContext {
    let mut ctx = TestContext::with_vector().await;
    ctx.register_block(
        "wafer-run/vector",
        Arc::new(Service {
            name: "wafer-run/vector",
            interface: "vector@v1",
            refuse: refuse_vector,
        }),
    );
    ctx.register_block(
        "impresspress/fastembed",
        Arc::new(Service {
            name: "impresspress/fastembed",
            interface: "embedding@v1",
            refuse: ALL,
        }),
    );
    ctx
}

/// `ctx` with `ops` on the registry table refused the way WRAP refuses them.
fn registry_denied(ctx: &TestContext, ops: &[&'static str]) -> FailingDbOpContext {
    FailingDbOpContext::failing_with(
        ctx.clone(),
        ops.iter().map(|op| (*op, REGISTRY_TABLE)).collect(),
        wrap_denial(),
    )
}

/// Register `docs` so the routes that read the registry find a row.
async fn seed_docs(ctx: &TestContext) {
    db::upsert(
        ctx,
        REGISTRY_TABLE,
        vec![
            (
                "prefixed_name".to_string(),
                serde_json::json!(prefixed_index_name("docs")),
            ),
            ("model".to_string(), serde_json::json!(DEFAULT_MODEL)),
            ("dimensions".to_string(), serde_json::json!(384)),
            ("keyword_search".to_string(), serde_json::json!(0)),
        ],
        vec!["prefixed_name".to_string()],
        OnConflict::SetColumns(vec!["model".to_string()]),
    )
    .await
    .expect("seed the registry row");
}

async fn api(ctx: &dyn Context, msg: Message, body: &str) -> OutputStream {
    let mut msg = msg;
    msg.set_meta("http.header.accept", "application/json");
    VectorBlock::new()
        .handle(ctx, msg, InputStream::from_bytes(body.as_bytes().to_vec()))
        .await
}

/// Records a miss unless the request ended in the door's WRAP denial.
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

/// Records a miss unless the page is the styled 403 a refused read gets.
async fn expect_refused_page(misses: &mut Vec<String>, ctx: &dyn Context, path: &str) {
    let mut msg = admin_msg("retrieve", path);
    msg.set_meta("http.header.accept", "text/html");
    let out = VectorBlock::new()
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

const CREATE_DOCS: &str = r#"{"name":"docs"}"#;

/// Every JSON route whose vector-service call is refused.
#[tokio::test]
async fn a_refused_vector_service_is_403() {
    let ctx = fixture(ALL).await;
    let mut misses = Vec::new();
    for (msg, body, site) in [
        (
            admin_msg("create", "/b/vector/api/indexes"),
            CREATE_DOCS,
            "POST /b/vector/api/indexes",
        ),
        (
            admin_msg("retrieve", "/b/vector/api/indexes"),
            "",
            "GET /b/vector/api/indexes",
        ),
        (
            admin_msg("retrieve", "/b/vector/api/stats"),
            "",
            "GET /b/vector/api/stats",
        ),
    ] {
        expect_wrap_denial(&mut misses, api(&ctx, msg, body).await, site).await;
    }
    report(misses);
}

/// Creating an index writes the registry row, and the admin modal's htmx
/// submit then re-reads the registry for the refreshed list. Either refused
/// is a 403.
#[tokio::test]
async fn a_refused_registry_write_or_refresh_is_403() {
    let ctx = fixture(&[]).await;
    let mut misses = Vec::new();

    expect_wrap_denial(
        &mut misses,
        api(
            &registry_denied(&ctx, &[ServiceOp::DATABASE_UPSERT]),
            admin_msg("create", "/b/vector/api/indexes"),
            CREATE_DOCS,
        )
        .await,
        "POST /b/vector/api/indexes (registry write)",
    )
    .await;

    let mut htmx = admin_msg("create", "/b/vector/api/indexes");
    htmx.set_meta("http.header.hx-request", "true");
    expect_wrap_denial(
        &mut misses,
        api(
            &registry_denied(&ctx, &[ServiceOp::DATABASE_LIST]),
            htmx,
            CREATE_DOCS,
        )
        .await,
        "POST /b/vector/api/indexes (htmx refresh)",
    )
    .await;

    report(misses);
}

/// The embed route and the ingest's embed step both call the embedding
/// block, and both answer its refusal with the door's 403.
#[tokio::test]
async fn a_refused_embedding_block_is_403() {
    let ctx = fixture(&[]).await;
    seed_docs(&ctx).await;
    let mut misses = Vec::new();
    expect_wrap_denial(
        &mut misses,
        api(
            &ctx,
            admin_msg("create", "/b/vector/api/embed"),
            r#"{"texts":["hello"]}"#,
        )
        .await,
        "POST /b/vector/api/embed",
    )
    .await;
    expect_wrap_denial(
        &mut misses,
        api(
            &ctx,
            admin_msg("create", "/b/vector/api/ingest"),
            r#"{"index":"docs","document_id":"d1","text":"hello world"}"#,
        )
        .await,
        "POST /b/vector/api/ingest (embed)",
    )
    .await;
    report(misses);
}

/// A refused registry read is the 403 page on the index list — not "No
/// vector indexes yet" — and on the detail page — not a 404.
#[tokio::test]
async fn refused_registry_reads_are_the_403_page() {
    let ctx = fixture(&[]).await;
    seed_docs(&ctx).await;
    let every_op = ServiceOp::DATABASE_OPS;
    let mut misses = Vec::new();
    expect_refused_page(&mut misses, &registry_denied(&ctx, every_op), "/b/vector/").await;
    expect_refused_page(
        &mut misses,
        &registry_denied(&ctx, every_op),
        "/b/vector/docs/",
    )
    .await;
    report(misses);
}

/// A query reads the index's registry row for the model its text is embedded
/// with. A refused read is answered with its code — never a fall-through to
/// `DEFAULT_MODEL`, which could embed the query with a model the index was
/// not built with.
#[tokio::test]
async fn a_refused_query_metadata_read_is_403() {
    let ctx = fixture(&[]).await;
    seed_docs(&ctx).await;
    let mut misses = Vec::new();
    expect_wrap_denial(
        &mut misses,
        api(
            &registry_denied(&ctx, &[ServiceOp::DATABASE_LIST]),
            admin_msg("create", "/b/vector/api/query"),
            r#"{"index":"docs","vector":[0.5,0.5]}"#,
        )
        .await,
        "POST /b/vector/api/query (registry read)",
    )
    .await;
    report(misses);
}

/// A refused count is never "0 vectors": the stats API answers the door's
/// 403, and the list and detail pages the 403 page. A refused describe is
/// never an empty schema.
#[tokio::test]
async fn a_refused_count_or_describe_is_never_drawn_as_empty() {
    let mut misses = Vec::new();

    let ctx = fixture(&[ServiceOp::VECTOR_COUNT]).await;
    seed_docs(&ctx).await;
    expect_wrap_denial(
        &mut misses,
        api(&ctx, admin_msg("retrieve", "/b/vector/api/stats"), "").await,
        "GET /b/vector/api/stats (count)",
    )
    .await;
    expect_refused_page(&mut misses, &ctx, "/b/vector/").await;
    expect_refused_page(&mut misses, &ctx, "/b/vector/docs/").await;

    let ctx = fixture(&[ServiceOp::VECTOR_DESCRIBE_INDEX]).await;
    seed_docs(&ctx).await;
    expect_refused_page(&mut misses, &ctx, "/b/vector/docs/").await;

    report(misses);
}

/// Deleting an index clears its registry row; a refused delete is answered,
/// not acknowledged over a row that keeps listing an index that is gone.
#[tokio::test]
async fn a_refused_registry_delete_is_403() {
    let ctx = fixture(&[]).await;
    seed_docs(&ctx).await;
    let mut misses = Vec::new();
    expect_wrap_denial(
        &mut misses,
        api(
            &registry_denied(&ctx, ServiceOp::DATABASE_OPS),
            admin_msg("delete", "/b/vector/api/indexes/docs"),
            "",
        )
        .await,
        "DELETE /b/vector/api/indexes/docs (registry delete)",
    )
    .await;
    report(misses);
}
