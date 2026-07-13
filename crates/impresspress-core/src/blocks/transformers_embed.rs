//! `impresspress/transformers-embed` — browser-side (Transformers.js) embedding block.
//!
//! Mirror of `blocks/fastembed.rs`, but accepts an injected
//! `Arc<dyn EmbeddingService>` so the WASM-specific service in
//! `impresspress-browser` can be constructed by `impresspress-web` and passed in
//! via the `ImpresspressBuilder`. Compiled only on `wasm32`.

use std::sync::Arc;

use wafer_core::interfaces::vector::{
    handler::handle_embedding_message, service::EmbeddingService,
};
use wafer_run::{
    context::Context, Block, BlockInfo, InputStream, InstanceMode, Message, OutputStream,
};

/// Browser-side embedding block backed by an injected `EmbeddingService`.
///
/// The service (typically `BrowserEmbeddingService` from `impresspress-browser`) is
/// constructed and injected by `impresspress-web` via `ImpresspressBuilder::embedding_service`.
pub struct TransformersEmbedBlock {
    service: Arc<dyn EmbeddingService>,
}

impl TransformersEmbedBlock {
    pub fn new(service: Arc<dyn EmbeddingService>) -> Self {
        Self { service }
    }
}

#[wafer_block::wafer_async_trait]
impl Block for TransformersEmbedBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "impresspress/transformers-embed",
            "0.0.1",
            "embedding@v1",
            "Browser text embedding via Transformers.js",
        )
        // Singleton, in lockstep with `FastembedBlock`: the injected
        // `BrowserEmbeddingService` wraps a single Transformers.js model
        // instance and must not be re-created per node/flow. Previously this
        // declaration was omitted (defaulting to per-node) — the drift this
        // package closes.
        .instance_mode(InstanceMode::Singleton)
        .category(wafer_run::BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        // Delegate the whole message — `handle_embedding_message` validates the
        // op (EMBEDDING_EMBED / EMBEDDING_COUNT_TOKENS, `Unimplemented`
        // otherwise). The previous hand-rolled `ServiceOp::EMBEDDING_EMBED`-only
        // match rejected the valid `EMBEDDING_COUNT_TOKENS` op and diverged from
        // `FastembedBlock`; both wrappers now delegate identically.
        handle_embedding_message(self.service.as_ref(), &msg, &body).await
    }
}
