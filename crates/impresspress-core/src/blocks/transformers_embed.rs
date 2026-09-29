//! `impresspress/transformers-embed` — browser-side (Transformers.js) embedding block.
//!
//! Mirror of `blocks/fastembed.rs`, but accepts an injected
//! `Arc<dyn EmbeddingService>` so the WASM-specific service in
//! `impresspress-browser` can be constructed by `impresspress-web` and passed in
//! via the `ImpresspressBuilder`.
//!
//! Nothing here is target-specific: the block holds the injected service and
//! delegates to the shared `handle_embedding_message`. It is registered
//! wherever an embedding service was injected, on any target — see
//! `builder::registration`.

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
    /// The block's registered name, and the namespace of its embedding
    /// resources.
    pub const BLOCK_NAME: &'static str = "impresspress/transformers-embed";

    pub fn new(service: Arc<dyn EmbeddingService>) -> Self {
        Self { service }
    }
}

#[wafer_block::wafer_async_trait]
impl Block for TransformersEmbedBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            Self::BLOCK_NAME,
            "0.0.1",
            "embedding@v1",
            "Browser text embedding via Transformers.js",
        )
        // Singleton, in lockstep with `FastembedBlock`: the injected
        // `BrowserEmbeddingService` wraps a single Transformers.js model
        // instance and must not be re-created per node/flow.
        .instance_mode(InstanceMode::Singleton)
        .category(wafer_run::BlockCategory::Service)
        .grants(vec![super::embedding_grant(Self::BLOCK_NAME)])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        // Delegate the whole message — `handle_embedding_message` validates the
        // op (EMBEDDING_EMBED / EMBEDDING_COUNT_TOKENS, `Unimplemented`
        // otherwise) and authorizes the caller against this block's
        // namespace, exactly as `FastembedBlock` does.
        handle_embedding_message(self.service.as_ref(), ctx, Self::BLOCK_NAME, &msg, &body).await
    }
}

/// The grant the embedding blocks declare, checked by the real embedding
/// handler behind a fixture that enforces WRAP. `impresspress/vector` is the
/// block that embeds and counts tokens through them.
#[cfg(test)]
mod grant_tests {
    use std::sync::Arc;

    use wafer_block::{
        codec,
        wire::vector::{CountTokensRequest, CountTokensResponse},
        ServiceOp,
    };
    use wafer_core::interfaces::vector::service::{EmbeddingService, VectorError};
    use wafer_run::{ErrorCode, InputStream, Message};

    use super::TransformersEmbedBlock;
    use crate::test_support::TestContext;

    struct Stub;

    #[wafer_block::wafer_async_trait]
    impl EmbeddingService for Stub {
        fn model(&self) -> &str {
            "stub"
        }
        fn dimensions(&self) -> u32 {
            1
        }
        async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, VectorError> {
            Ok(texts.iter().map(|_| vec![0.0]).collect())
        }
    }

    /// `embedding.count_tokens` called by `caller` through `call_block`,
    /// under the deployment's grants — the ones the embedding block declares.
    async fn count_tokens_as(caller: &str) -> Result<u64, wafer_run::WaferError> {
        let block = Arc::new(TransformersEmbedBlock::new(Arc::new(Stub)));
        let mut ctx = TestContext::new().await;
        ctx.register_block(TransformersEmbedBlock::BLOCK_NAME, block);
        let ctx = ctx.running_as(caller);
        let body = codec::encode(&CountTokensRequest {
            text: "two words".into(),
        })
        .expect("encode");
        let out = wafer_run::context::Context::call_block(
            &ctx,
            TransformersEmbedBlock::BLOCK_NAME,
            Message::new(ServiceOp::EMBEDDING_COUNT_TOKENS),
            InputStream::from_bytes(body),
        )
        .await
        .collect_buffered()
        .await
        .map_err(wafer_run::WaferError::from)?;
        Ok(codec::decode::<CountTokensResponse>(&out.body)
            .expect("decode")
            .tokens)
    }

    #[tokio::test]
    async fn the_vector_block_may_embed_and_no_other_block_may() {
        assert_eq!(
            count_tokens_as("impresspress/vector")
                .await
                .expect("granted"),
            2
        );
        let refused = count_tokens_as("impresspress/files")
            .await
            .expect_err("an ungranted block is refused");
        assert_eq!(refused.code, ErrorCode::PermissionDenied);
    }
}
