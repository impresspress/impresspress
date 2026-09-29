//! Network platform-service factory for native targets.

use std::sync::Arc;

use wafer_core::interfaces::network::service::{NetworkLimits, NetworkService};

/// Construct an HTTP network service backed by `reqwest`.
///
/// Built under the default limits. The response-size cap and the connect,
/// read, request and stream timeouts (`WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES`,
/// `…__CONNECT_TIMEOUT_SECS`, `…__READ_TIMEOUT_SECS`,
/// `…__REQUEST_TIMEOUT_SECS`, `…__STREAM_TIMEOUT_SECS`) are the
/// `wafer-run/network` block's declared config: the block reads them from its
/// `lifecycle(Init)` config, which native resolves from the variables table
/// and then the process environment (`impresspress_server::build_native_runtime`), and
/// applies them through `NetworkService::configure`. An invalid value fails
/// that block's Init, naming the key.
pub fn make_fetch_network_service() -> Arc<dyn NetworkService> {
    Arc::new(wafer_block_network::service::HttpNetworkService::new(
        NetworkLimits::default(),
    ))
}
