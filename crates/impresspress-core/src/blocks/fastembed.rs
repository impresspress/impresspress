//! `impresspress/fastembed` — native ONNX embedding block.
//!
//! Wraps a [`FastembedService`] from `wafer-block-fastembed` and exposes it
//! as a WAFER block speaking the `embedding@v1` service protocol. App blocks
//! (notably `impresspress/vector`) dispatch to it via
//! `ctx.call_block("impresspress/fastembed", ...)` whenever they need to embed
//! text with a locally-hosted model.
//!
//! This block is feature-gated behind `native-embedding` because the
//! underlying fastembed-rs crate pulls in ONNX Runtime (~100 MB of native
//! deps on most platforms). Consumers that only need remote embedding
//! providers — or the browser runtime where ONNX isn't applicable — should
//! not pay the build cost, so registration is conditional in `blocks::mod`.
//!
//! ## Lazy service construction
//!
//! `FastembedService::default_model()` triggers ONNX model download + load
//! (tens to hundreds of MB) — not cheap. The `info()` path in
//! `blocks::all_block_infos()` constructs every block just to read its
//! metadata, so we must *not* eagerly load the model in the constructor.
//! The service is built lazily on the first `handle()` call and cached in
//! a `OnceLock` for the lifetime of the singleton.

use std::sync::{Arc, OnceLock};

use wafer_block_fastembed::FastembedService;
use wafer_core::interfaces::vector::handler::handle_embedding_message;
use wafer_run::{
    context::Context, Block, BlockInfo, InputStream, InstanceMode, Message, OutputStream,
};

use crate::http::err_internal;

/// Native ONNX embedding block.
///
/// Singleton. The wrapped `FastembedService` is initialized on the first
/// `handle()` call — construction is free, no model weights are loaded
/// until someone asks to embed something. An init error surfaces to the
/// caller as an `Internal` error on that first request.
pub struct FastembedBlock {
    service: OnceLock<Arc<FastembedService>>,
}

impl FastembedBlock {
    /// Build a `FastembedBlock` with a lazy service.
    ///
    /// The model is loaded on first embed, not here — this is cheap enough
    /// to call from `blocks::all_block_infos()` without triggering an ONNX
    /// download.
    pub fn new() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }

    fn get_service(&self) -> Result<&FastembedService, String> {
        // OnceLock::get_or_init can't surface errors, so we do a
        // get-then-try-set dance. Whichever caller wins the `set` race owns
        // the value; the loser sees `Err` but the value is now present and
        // the next `get()` will return it.
        if let Some(svc) = self.service.get() {
            return Ok(svc.as_ref());
        }
        let built =
            FastembedService::default_model().map_err(|e| format!("fastembed init failed: {e}"))?;
        let _ = self.service.set(Arc::new(built));
        match self.service.get() {
            Some(svc) => Ok(svc.as_ref()),
            None => Err("fastembed service unavailable after init".to_string()),
        }
    }
}

impl Default for FastembedBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl FastembedBlock {
    /// The block's registered name. Mirrors the `BLOCK_NAME` const the
    /// `impresspress_feature_block!` macro generates for the zero-arg feature
    /// blocks, so the `(feature, name, constructor)` manifest in
    /// [`crate::blocks`] can reference `FastembedBlock::BLOCK_NAME` uniformly.
    pub const BLOCK_NAME: &'static str = "impresspress/fastembed";
}

#[wafer_block::wafer_async_trait]
impl Block for FastembedBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "impresspress/fastembed",
            "0.0.1",
            "embedding@v1",
            "Native ONNX text embedding via fastembed-rs",
        )
        // Singleton: the wrapped `FastembedService` lazily loads one ONNX
        // model and caches it in the `OnceLock` for the block's lifetime —
        // a per-node/per-flow instance would reload the model needlessly.
        // Kept deliberately in lockstep with `TransformersEmbedBlock`.
        .instance_mode(InstanceMode::Singleton)
        .category(wafer_run::BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        let svc = match self.get_service() {
            Ok(s) => s,
            Err(e) => return err_internal("fastembed service unavailable", e),
        };
        // Op validation (EMBEDDING_EMBED / EMBEDDING_COUNT_TOKENS, plus an
        // `Unimplemented` terminal for anything else) lives in
        // `handle_embedding_message`. Both embedding wrappers delegate the
        // whole message here — neither carries its own `ServiceOp` check.
        handle_embedding_message(svc, &msg, &body).await
    }
}
