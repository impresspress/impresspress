//! Impresspress router block — delegates to the shared `impresspress_core` pipeline.
//!
//! This block replaces the individual per-feature flow definitions (auth, admin,
//! files, etc.) by routing all API requests through `crate::handle_request()`.
//! The WAFER flow engine still provides middleware (CORS, security headers) via
//! `wafer-run/infra`, but routing, feature gates, admin checks, and JWT validation
//! are handled by the shared pipeline.

use std::sync::Arc;

use wafer_run::{
    context::Context, Block, BlockInfo, InputStream, InstanceMode, LifecycleEvent, Message,
    OutputStream, WaferError,
};

use crate::{features::FeatureConfig, routing::ExtraRoute};

/// The impresspress router block — dispatches all API requests via the shared
/// `crate::handle_request()` pipeline.
pub struct ImpresspressRouterBlock {
    jwt_secret: String,
    features: Arc<dyn FeatureConfig>,
    /// BlockInfo for all registered impresspress blocks — used by the discovery
    /// endpoints (`/openapi.json`, `/.well-known/agent.json`). Populated from
    /// `Wafer::block_infos()` after all blocks are registered.
    block_infos: Vec<BlockInfo>,
    /// Runtime-added routes from downstream projects (see `ImpresspressBuilder::add_route`).
    /// Built-in `ROUTES` take priority — see `routing::route_to_block`.
    extra_routes: Arc<Vec<ExtraRoute>>,
}

impl ImpresspressRouterBlock {
    /// Construct a router with no extra routes (backward-compatible).
    pub fn new(
        jwt_secret: String,
        features: Arc<dyn FeatureConfig>,
        block_infos: Vec<BlockInfo>,
    ) -> Self {
        Self::with_extra_routes(jwt_secret, features, block_infos, Vec::new())
    }

    /// Construct a router with project-registered extra routes appended after
    /// the built-in `ROUTES` table.
    pub fn with_extra_routes(
        jwt_secret: String,
        features: Arc<dyn FeatureConfig>,
        block_infos: Vec<BlockInfo>,
        extra_routes: Vec<ExtraRoute>,
    ) -> Self {
        Self {
            jwt_secret,
            features,
            block_infos,
            extra_routes: Arc::new(extra_routes),
        }
    }
}

#[wafer_block::wafer_async_trait]
impl Block for ImpresspressRouterBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "impresspress/router",
            "0.0.1",
            "http-handler@v1",
            "Impresspress shared router — delegates to impresspress-core pipeline",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(wafer_run::BlockCategory::Infrastructure)
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(()) // No-op — individual blocks handle their own lifecycle
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        // Resolve auth token from Authorization header or auth_token cookie.
        let auth_header = msg.header("authorization");
        let auth_value = if !auth_header.is_empty() {
            Some(auth_header.to_string())
        } else {
            let cookie_token = msg.cookie("auth_token");
            if !cookie_token.is_empty() {
                Some(format!("Bearer {cookie_token}"))
            } else {
                None
            }
        };

        crate::handle_request(
            ctx,
            msg,
            input,
            auth_value.as_deref(),
            &self.jwt_secret,
            self.features.as_ref(),
            &self.block_infos,
            &self.extra_routes,
        )
        .await
    }
}
