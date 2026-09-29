//! `ImpresspressBuilder::vector_service` and `::embedding_service` each
//! register the block their service backs, on its own.
//!
//! `wafer-run/vector` serves only `vector@v1`; embedding is an `embedding@v1`
//! block's (`impresspress/transformers-embed` over an injected embedding
//! service). So an injected vector store is registered whether or not an
//! embedder was injected beside it, and an injected embedder without a store
//! still serves embeddings. Built through the real `build()` over a
//! file-backed SQLite database, with the services the native binary uses.

use std::sync::Arc;

use impresspress_core::builder::{fill_config_service, ImpresspressBuilder, RuntimeConfig};
use wafer_core::interfaces::vector::service::{
    EmbeddingService, MetadataFilter, Result as VectorResult, SearchMode, VectorEntry,
    VectorIndexConfig, VectorMatch, VectorService,
};

/// A vector store no test calls: only its registration is under test.
struct UnusedVectorStore;

#[wafer_block::wafer_async_trait]
impl VectorService for UnusedVectorStore {
    async fn create_index(&self, _config: VectorIndexConfig) -> VectorResult<()> {
        unreachable!("registration only")
    }
    async fn delete_index(&self, _name: &str) -> VectorResult<()> {
        unreachable!("registration only")
    }
    async fn upsert(&self, _index: &str, _entries: Vec<VectorEntry>) -> VectorResult<()> {
        unreachable!("registration only")
    }
    async fn query(
        &self,
        _index: &str,
        _vector: Vec<f32>,
        _top_k: usize,
        _filter: Option<MetadataFilter>,
        _mode: SearchMode,
        _keyword_query: Option<String>,
    ) -> VectorResult<Vec<VectorMatch>> {
        unreachable!("registration only")
    }
    async fn delete(&self, _index: &str, _ids: Vec<String>) -> VectorResult<()> {
        unreachable!("registration only")
    }
    async fn count(&self, _index: &str) -> VectorResult<u64> {
        unreachable!("registration only")
    }
    async fn rename_index(&self, _from: &str, _to: &str) -> VectorResult<()> {
        unreachable!("registration only")
    }
}

/// An embedder no test calls: only its registration is under test.
struct UnusedEmbedder;

#[wafer_block::wafer_async_trait]
impl EmbeddingService for UnusedEmbedder {
    fn model(&self) -> &str {
        "unused"
    }
    fn dimensions(&self) -> u32 {
        3
    }
    async fn embed(&self, _texts: Vec<String>) -> VectorResult<Vec<Vec<f32>>> {
        unreachable!("registration only")
    }
}

/// A builder carrying the six required services, over a SQLite file in `dir`.
async fn builder(dir: &std::path::Path) -> ImpresspressBuilder {
    let db_path = dir.join("vector_services.sqlite3");
    let storage_root = dir.join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let database =
        impresspress_native::make_database_service("sqlite", db_path.to_str().unwrap(), None)
            .await
            .expect("construct sqlite database service");
    let storage =
        impresspress_native::make_storage_service("local", storage_root.to_str().unwrap())
            .await
            .expect("construct local storage service");
    let (builder, ()) = RuntimeConfig::new().install(
        ImpresspressBuilder::new()
            .database(database)
            .storage(storage),
        |map| {
            (
                fill_config_service(
                    Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
                    map,
                ),
                (),
            )
        },
    );
    builder
        .crypto(
            impresspress_native::make_jwt_crypto_service(
                "vector-services-test-jwt-secret-value".to_string(),
                Default::default(),
            )
            .expect("jwt crypto service"),
        )
        .network(impresspress_native::make_fetch_network_service())
        .logger(impresspress_native::make_tracing_logger())
}

#[tokio::test]
async fn an_injected_vector_store_alone_registers_the_vector_block() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wafer = builder(tmp.path())
        .await
        .vector_service(Arc::new(UnusedVectorStore))
        .build()
        .expect("build");

    let names = wafer.block_names();
    assert!(
        names.iter().any(|n| n == "wafer-run/vector"),
        "the injected vector store backs wafer-run/vector: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "impresspress/transformers-embed"),
        "no embedder was injected: {names:?}"
    );
}

#[tokio::test]
async fn an_injected_embedder_alone_registers_the_embedding_block() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wafer = builder(tmp.path())
        .await
        .embedding_service(Arc::new(UnusedEmbedder))
        .build()
        .expect("build");

    let names = wafer.block_names();
    assert!(
        names.iter().any(|n| n == "impresspress/transformers-embed"),
        "the injected embedder backs impresspress/transformers-embed: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "wafer-run/vector"),
        "no vector store was injected: {names:?}"
    );
}
