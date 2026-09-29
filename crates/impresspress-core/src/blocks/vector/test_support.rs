//! Shared stubs for the vector block's route tests: a scripted
//! `wafer-run/vector` service block and a scripted embedding block.
//!
//! Both answer only the ops a contract test needs and error loudly on
//! anything else, so a test cannot silently exercise an op it did not
//! script.

use std::collections::HashMap;

use wafer_block::{
    common::ServiceOp,
    wire::vector::{
        CountRequest, CountResponse, DescribeIndexRequest, DescribeIndexResponse, EmbedRequest,
        EmbedResponse, ListIndexesResponse, QueryResponse, VectorMatch,
    },
};
use wafer_run::{
    context::Context, Block, BlockCategory, BlockInfo, ErrorCode, InputStream, LifecycleEvent,
    Message, OutputStream, WaferError,
};

/// Stub `wafer-run/vector` block. Writes (`create_index`, `delete_index`,
/// `upsert`, `delete`) acknowledge with an empty body; `list_indexes`
/// returns `indexes` (storage stems, prefix retained); `count` answers
/// from `counts` (0 for an unknown index); `describe_index` reports an index
/// in `indexes` as existing with no columns, any other as absent; `query`
/// returns `matches`.
#[derive(Default)]
pub(crate) struct StubVectorBlock {
    pub(crate) indexes: Vec<String>,
    pub(crate) counts: HashMap<String, u64>,
    pub(crate) matches: Vec<VectorMatch>,
}

#[async_trait::async_trait]
impl Block for StubVectorBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/vector",
            "0.0.1",
            "vector@v1",
            "stub vector block for contract tests",
        )
        .category(BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        match msg.kind.as_str() {
            ServiceOp::VECTOR_CREATE_INDEX
            | ServiceOp::VECTOR_DELETE_INDEX
            | ServiceOp::VECTOR_UPSERT
            | ServiceOp::VECTOR_DELETE => OutputStream::respond(Vec::new()),
            ServiceOp::VECTOR_LIST_INDEXES => {
                let resp = ListIndexesResponse {
                    indexes: self.indexes.clone(),
                };
                OutputStream::respond(wafer_block::codec::encode(&resp).expect("encode"))
            }
            ServiceOp::VECTOR_COUNT => {
                let bytes = match input.collect_to_bytes().await {
                    Ok(bytes) => bytes,
                    Err(e) => return OutputStream::error(e),
                };
                let req: CountRequest = wafer_block::codec::decode(&bytes).expect("count request");
                let resp = CountResponse {
                    count: self.counts.get(&req.index).copied().unwrap_or(0),
                };
                OutputStream::respond(wafer_block::codec::encode(&resp).expect("encode"))
            }
            ServiceOp::VECTOR_DESCRIBE_INDEX => {
                let bytes = match input.collect_to_bytes().await {
                    Ok(bytes) => bytes,
                    Err(e) => return OutputStream::error(e),
                };
                let req: DescribeIndexRequest =
                    wafer_block::codec::decode(&bytes).expect("describe request");
                let resp = DescribeIndexResponse {
                    exists: self.indexes.contains(&req.index),
                    columns: Vec::new(),
                    keyword_search: false,
                };
                OutputStream::respond(wafer_block::codec::encode(&resp).expect("encode"))
            }
            ServiceOp::VECTOR_QUERY => {
                let resp = QueryResponse {
                    matches: self.matches.clone(),
                };
                OutputStream::respond(wafer_block::codec::encode(&resp).expect("encode"))
            }
            other => OutputStream::error(WaferError::new(
                ErrorCode::Unimplemented,
                format!("StubVectorBlock: unhandled op {other}"),
            )),
        }
    }

    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
}

/// Run `msg` through the block's own route table so `{name}` / `{index}` /
/// `{id}` are bound the way they are on the wire, then hand the message to
/// a handler directly. Panics when no row matches: a test that sends an
/// unroutable path would otherwise exercise the handler's "missing name"
/// branch by accident.
pub(super) fn routed(mut msg: Message) -> Message {
    let route = crate::endpoint_match::dispatch(&mut msg, super::ROUTES);
    assert!(
        route.is_some(),
        "no vector route matches {} {}",
        msg.action(),
        msg.path()
    );
    msg
}

/// Stub embedding block: answers `embedding.embed` with one
/// `[0.5; dimensions]` vector per input text, labelled `model`.
pub(super) struct StubEmbeddingBlock {
    pub(super) model: &'static str,
    pub(super) dimensions: u32,
}

#[async_trait::async_trait]
impl Block for StubEmbeddingBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "impresspress/fastembed",
            "0.0.1",
            "embedding@v1",
            "stub embedding block for contract tests",
        )
        .category(BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        match msg.kind.as_str() {
            ServiceOp::EMBEDDING_EMBED => {
                let bytes = match input.collect_to_bytes().await {
                    Ok(bytes) => bytes,
                    Err(e) => return OutputStream::error(e),
                };
                let req: EmbedRequest = wafer_block::codec::decode(&bytes).expect("embed request");
                let resp = EmbedResponse {
                    model: self.model.to_string(),
                    dimensions: self.dimensions,
                    vectors: req
                        .texts
                        .iter()
                        .map(|_| vec![0.5f32; self.dimensions as usize])
                        .collect(),
                };
                OutputStream::respond(wafer_block::codec::encode(&resp).expect("encode"))
            }
            other => OutputStream::error(WaferError::new(
                ErrorCode::Unimplemented,
                format!("StubEmbeddingBlock: unhandled op {other}"),
            )),
        }
    }

    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
}
