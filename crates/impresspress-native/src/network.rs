//! Network platform-service factory for native targets.

use std::sync::Arc;

use wafer_core::interfaces::network::service::NetworkService;

/// Construct an HTTP network service backed by `reqwest`.
pub fn make_fetch_network_service() -> Arc<dyn NetworkService> {
    Arc::new(wafer_block_network::service::HttpNetworkService::new())
}
