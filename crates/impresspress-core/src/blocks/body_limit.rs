//! The request-body ceiling, enforced before anything routes.
//!
//! An adapter that reads a request body larger than
//! [`crate::streaming::MAX_REQUEST_BODY_BYTES`] drops it and marks the message
//! ([`crate::streaming::META_REQ_BODY_TOO_LARGE`]). Something then has to turn
//! that marker into a 413, and *where* it happens decides which requests it
//! covers.
//!
//! It cannot be the router's business. `wafer-run/router` sends `/`, `/b/**`,
//! `/health`, `/openapi.json` and `/.well-known/agent.json` to
//! `impresspress/router` — and everything else to `wafer-run/web`, which knows
//! nothing about the marker and answers a POST to an unclaimed path with
//! `index.html` and a 200. A refusal that lives past that fork therefore turns
//! an oversized upload to the wrong path into a **success** whose body was
//! thrown away, which is worse than the opaque 500 this whole change set out
//! to remove.
//!
//! So it is a flow step, ahead of the router: every request passes through it,
//! whatever route it would have matched. It sits after `wafer-run/cors` and
//! `wafer-run/security-headers` so the refusal carries their headers — they
//! annotate the message, and the flow executor carries a middleware step's
//! response headers onto the error that stops the flow (see
//! [`crate::pipeline::payload_too_large_error`], which is also where the
//! "why an error, not a plain response" is written down).
//!
//! One consequence is deliberate and worth naming: this runs *before*
//! `impresspress/router`, so a refused request has not been through JWT
//! validation or the CSRF origin policy. Its audit row therefore carries no
//! user id, and a cross-site oversized POST is answered 413 rather than the
//! 403 the CSRF check would have given it. Nothing is mutated either way — the
//! body is already gone and no block is dispatched — and the alternative is
//! the 200 above.

use std::sync::Arc;

use wafer_run::{
    context::Context, Block, BlockCategory, BlockInfo, InputStream, InstanceMode, LifecycleEvent,
    Message, OutputStream, WaferError,
};

use crate::routing::ExtraRoute;

/// The block name the site-main flow names in its `body-limit` step.
pub const BLOCK_NAME: &str = "impresspress/body-limit";

/// Refuses a marked (oversized-body) request with a 413; passes everything
/// else through untouched.
///
/// It carries the route declarations for one reason: the audit row. A refused
/// upload to `/b/storage/api/buckets/x/objects` should be readable as that,
/// and `pipeline`'s `<unmatched>` collapse — which exists so junk paths cannot
/// mint arbitrary rows — needs the route table to tell the two apart. Same
/// snapshot `impresspress/router` is built with.
pub struct BodyLimitBlock {
    block_infos: Vec<BlockInfo>,
    extra_routes: Arc<Vec<ExtraRoute>>,
}

impl BodyLimitBlock {
    /// Construct the block with the routes its audit rows are judged against.
    pub fn new(block_infos: Vec<BlockInfo>, extra_routes: Arc<Vec<ExtraRoute>>) -> Self {
        Self {
            block_infos,
            extra_routes,
        }
    }
}

#[wafer_block::wafer_async_trait]
impl Block for BodyLimitBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            BLOCK_NAME,
            "0.0.1",
            "http-middleware@v1",
            "Refuses a request whose body exceeded the transport limit with 413",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        if crate::streaming::body_too_large(&msg) {
            return crate::pipeline::refuse_oversized_body(
                ctx,
                &msg,
                &self.block_infos,
                &self.extra_routes,
            )
            .await;
        }
        OutputStream::continue_with(msg)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use wafer_block::http_codec;

    use super::*;
    use crate::{
        platform_state::request_logs,
        routing::RouteAccess,
        streaming::{BODY_TOO_LARGE_VALUE, META_REQ_BODY_TOO_LARGE},
        test_support::{anon_msg, TestContext},
    };

    fn block() -> BodyLimitBlock {
        BodyLimitBlock::new(Vec::new(), Arc::new(Vec::new()))
    }

    fn marked(path: &str) -> Message {
        let mut msg = anon_msg("create", path);
        msg.set_meta(META_REQ_BODY_TOO_LARGE, BODY_TOO_LARGE_VALUE);
        msg
    }

    /// The marker is refused whatever the path — including one the router
    /// would have handed to `wafer-run/web`, which is the case that used to
    /// answer 200 with an `index.html` body.
    #[tokio::test]
    async fn a_marked_request_is_refused_on_any_path() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::body_limit::BLOCK_NAME);
        for path in ["/b/storage/api/buckets/p/objects", "/anything/else", "/"] {
            let out = block()
                .handle(&ctx, marked(path), InputStream::empty())
                .await;
            let parts = http_codec::collect_http_response(out).await;
            assert_eq!(parts.status, 413, "path {path} must be refused");
        }
    }

    /// The refusal is audited, and the path survives `pipeline`'s
    /// `<unmatched>` collapse — which is the only reason this block is built
    /// with the route declarations at all. An operator looking into a failed
    /// upload needs to see which upload.
    #[tokio::test]
    async fn the_refusal_is_audited_under_its_own_path() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::body_limit::BLOCK_NAME);
        let block = BodyLimitBlock::new(
            Vec::new(),
            Arc::new(vec![ExtraRoute::new(
                "/b/storage/",
                "impresspress/files",
                RouteAccess::Public,
            )]),
        );

        let out = block
            .handle(
                &ctx,
                marked("/b/storage/api/buckets/p/objects"),
                InputStream::empty(),
            )
            .await;
        let _ = out.collect_buffered().await;

        let rows = request_logs::paginated(&ctx.fixture(), 1, 20, "", false)
            .await
            .expect("read request_logs")
            .rows;
        let row = rows
            .iter()
            .find(|r| r.path == "/b/storage/api/buckets/p/objects")
            .expect("the refused upload is audited under its own path");
        assert_eq!(row.status_code, 413);
    }

    /// An ordinary request continues to the next step untouched — the block is
    /// a gate, not a handler, so it must not answer anything itself.
    #[tokio::test]
    async fn an_unmarked_request_continues() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::body_limit::BLOCK_NAME);
        let out = block()
            .handle(&ctx, anon_msg("create", "/b/storage"), InputStream::empty())
            .await;

        match out.collect_buffered().await {
            Err(wafer_run::streams::output::TerminalNotResponse::Continue(msg)) => {
                assert_eq!(msg.path(), "/b/storage", "the message passes through");
            }
            other => panic!("expected Continue, got {other:?}"),
        }
    }
}
