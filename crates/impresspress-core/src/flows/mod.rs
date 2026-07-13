//! Flow definitions for Impresspress.
//!
//! All API routing is handled by the `impresspress/router` block, which delegates
//! to `impresspress-core`'s shared pipeline. The only flow needed is `site-main`,
//! which dispatches API paths to the router and serves the SPA for everything
//! else. The wafer-core base flows (wafer-run/infra) provide middleware.

pub mod site_main;

use wafer_run::{RuntimeError, Wafer};

/// Register the site-main flow (used with impresspress/router).
///
/// # Errors
///
/// Returns the underlying `RuntimeError` if the runtime rejects the
/// generated route config or the embedded `site_main::JSON` (a build-time
/// invariant — failure here means the bundled flow JSON drifted from the
/// runtime's flow schema).
pub fn register_site_main(w: &mut Wafer) -> Result<(), RuntimeError> {
    // Inject default routes into the router block config
    w.add_block_config(
        "wafer-run/router",
        serde_json::json!({ "routes": site_main::default_routes() }),
    );

    // Configure the web block to serve from the "site" storage bucket as an SPA
    w.add_block_config(
        "wafer-run/web",
        serde_json::json!({ "web_root": "site", "web_spa": "true", "web_index": "index.html" }),
    );

    w.add_flow_json(site_main::JSON)
}
