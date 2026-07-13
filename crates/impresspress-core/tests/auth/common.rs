//! Shared test helpers for the `wafer-run/auth` integration tests.
//!
//! `MigrationTestCtx` routes:
//! - `call_block("wafer-run/database", ...)` to a real `DatabaseBlock` wrapping
//!   an in-memory SQLite service.
//! - `call_block("wafer-run/crypto", ...)` to a real `CryptoBlock` wrapping
//!   `Argon2JwtCryptoService`, so tests exercising `crypto::random_bytes` and
//!   `crypto::hash` see the same wire contract as production.
//!
//! Any other block call returns `NotFound` — including `wafer-run/config`,
//! which makes `config::get_default(..., "sqlite")` fall back to the default.

use std::sync::Arc;

use wafer_run::{context::Context, Block, InputStream, Message, OutputStream, WaferError};

#[derive(Clone)]
pub struct MigrationTestCtx {
    db_block: Arc<dyn Block>,
    crypto_block: Arc<dyn Block>,
}

impl MigrationTestCtx {
    /// Construct a test context with admin migrations pre-applied.
    ///
    /// Admin's migrations create `impresspress__admin__block_settings`, the
    /// tracking table every other block's `apply_if_blessed` upserts into.
    /// In production this is guaranteed by registration order
    /// (`register_all_static_blocks` puts admin first); here we enforce it
    /// in the fixture so auth tests can call `migrations::apply` without
    /// the call failing on a missing tracking table.
    pub async fn new() -> Self {
        let ctx = Self::raw();
        impresspress_core::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations (bootstraps block_settings)");
        ctx
    }

    fn raw() -> Self {
        let svc = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        );
        let db_block: Arc<dyn Block> = Arc::new(
            wafer_core::service_blocks::database::DatabaseBlock::new(svc),
        );
        let crypto_svc = Arc::new(
            wafer_block_crypto::service::Argon2JwtCryptoService::new(
                // ≥ 32 bytes for HMAC-SHA256 minimum-length check.
                "test-jwt-secret-padded-to-min-32-bytes-aaaa".to_string(),
            )
            .expect("test secret is long enough"),
        );
        let crypto_block: Arc<dyn Block> = Arc::new(
            wafer_core::service_blocks::crypto::CryptoBlock::new(crypto_svc),
        );
        Self {
            db_block,
            crypto_block,
        }
    }
}

#[async_trait::async_trait]
impl Context for MigrationTestCtx {
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        match block_name {
            "wafer-run/database" => self.db_block.handle(self, msg, input).await,
            "wafer-run/crypto" => self.crypto_block.handle(self, msg, input).await,
            _ => OutputStream::error(WaferError::new(
                wafer_run::ErrorCode::NotFound,
                format!("block '{block_name}' not registered in test ctx"),
            )),
        }
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    /// This fixture has no caller identity or WRAP grants, so there is
    /// nothing to enforce — explicitly permissive, overriding the
    /// fail-closed trait default (which exists so an enforcing runtime
    /// can never silently fall back to permissive). Mirrors the pre-WRAP
    /// behaviour of this harness; WRAP-behaviour tests use
    /// `impresspress_core::test_support::TestContext::with_wrap` instead.
    fn check_resource_access(
        &self,
        _resource: &str,
        _resource_type: wafer_run::ResourceType,
        _is_write: bool,
    ) -> Result<(), WaferError> {
        Ok(())
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}
