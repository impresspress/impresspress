//! Impresspress router block — delegates to the shared `impresspress_core` pipeline.
//!
//! This block replaces the individual per-feature flow definitions (auth, admin,
//! files, etc.) by routing all API requests through `crate::handle_request()`.
//! The WAFER flow engine still provides middleware (CORS, security headers) via
//! `wafer-run/infra`, but routing, feature gates, admin checks, and JWT validation
//! are handled by the shared pipeline.

use std::sync::{Arc, RwLock};

use wafer_run::{
    context::Context, Block, BlockInfo, InputStream, InstanceMode, LifecycleEvent, Message,
    OutputStream, WaferError,
};

use crate::{features::FeatureConfig, routing::ExtraRoute};

/// The impresspress router block — dispatches all API requests via the shared
/// `crate::handle_request()` pipeline.
pub struct ImpresspressRouterBlock {
    /// JWT signing/verify secret behind a shared lock so it can be populated
    /// *after* the runtime is built. The browser/OPFS build auto-generates
    /// `WAFER_RUN__AUTH__JWT_SECRET` into the variables table only during boot
    /// (after `build()`), so at build time this is empty and the real value is
    /// written through [`ImpresspressBuilder::jwt_secret_handle`] once seeding
    /// runs — the same shape as `block_settings`. Native/Cloudflare have the
    /// secret in config at build time and never rotate it. Read per request so
    /// the pipeline always verifies against the current value (matching the
    /// crypto service, which is rotated the same way).
    jwt_secret: Arc<RwLock<String>>,
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
        jwt_secret: Arc<RwLock<String>>,
        features: Arc<dyn FeatureConfig>,
        block_infos: Vec<BlockInfo>,
    ) -> Self {
        Self::with_extra_routes(jwt_secret, features, block_infos, Vec::new())
    }

    /// Construct a router with project-registered extra routes appended after
    /// the built-in `ROUTES` table.
    pub fn with_extra_routes(
        jwt_secret: Arc<RwLock<String>>,
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

        // Read the current secret through the lock: the browser build rotates
        // it after boot-time seeding (see the field doc), so a snapshot taken at
        // construction would verify against the wrong key.
        let jwt_secret = self
            .jwt_secret
            .read()
            .expect("router jwt_secret RwLock poisoned")
            .clone();

        crate::handle_request(
            ctx,
            msg,
            input,
            auth_value.as_deref(),
            &jwt_secret,
            self.features.as_ref(),
            &self.block_infos,
            &self.extra_routes,
        )
        .await
    }
}
