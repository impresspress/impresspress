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
/// `cors_allowed_origins` and `csp_directives` are the operator-configured
/// values of [`crate::config_vars::CORS_ALLOWED_ORIGINS_KEY`] and
/// [`crate::config_vars::CSP_DIRECTIVES_KEY`], resolved from config by the
/// builder. They configure the `wafer-run/cors` and `wafer-run/security-
/// headers` middleware steps of this flow — the two infrastructure blocks
/// have no other config channel, so without this a native/Cloudflare deploy
/// denies every cross-origin request and blocks embedded Stripe.js.
///
/// # Errors
///
/// Returns the underlying `RuntimeError` if the runtime rejects the
/// generated route config or the embedded `site_main::JSON` (a build-time
/// invariant — failure here means the bundled flow JSON drifted from the
/// runtime's flow schema).
pub fn register_site_main(
    w: &mut Wafer,
    cors_allowed_origins: &str,
    csp_directives: &str,
) -> Result<(), RuntimeError> {
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

    // Feed the CORS allow-list to the middleware step. Empty stays fail-closed
    // (the block denies all cross-origin requests); a value or `*` opens it.
    if !cors_allowed_origins.is_empty() {
        w.add_block_config(
            "wafer-run/cors",
            serde_json::json!({ "allowed_origins": cors_allowed_origins }),
        );
    }

    // Feed extra CSP directives to the security-headers step. The block merges
    // these over its hard baseline (widen-only), so this can grant the Stripe
    // origins embedded Checkout needs without weakening the default policy.
    if !csp_directives.is_empty() {
        w.add_block_config(
            "wafer-run/security-headers",
            serde_json::json!({ "csp": csp_directives }),
        );
    }

    w.add_flow_json(site_main::JSON)
}
