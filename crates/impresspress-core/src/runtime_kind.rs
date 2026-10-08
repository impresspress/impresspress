//! Which runtime this instance is: a server, or a browser's service worker.
//!
//! The browser adapter publishes `__IMPRESSPRESS_RUNTIME_KIND__ = "browser"`
//! on the synchronous `config_get` snapshot (`impresspress-web`'s
//! `RuntimeConfig::both`); a server publishes nothing and is the default.
//! The key is runtime-owned (`__…__`), never served from the variables table,
//! so no database or admin value can make a browser look like a server.

use wafer_run::context::Context;

/// The config key the browser adapter sets to `"browser"`.
///
/// Adapter-injected runtime identity. The browser service-worker adapter
/// publishes it on its in-memory config after loading persisted variables,
/// so an admin database value cannot accidentally turn a public browser
/// runtime into a trusted secret holder. Native and Cloudflare leave it unset
/// and retain the server default. Double-underscore brackets mark the key as
/// internal (same convention as `BLOCK_SETTINGS_CONFIG_KEY`): it is never set
/// via env var or the variables table, so it must not claim the
/// admin-writable `WAFER_RUN_SHARED__` prefix.
pub const RUNTIME_KIND_CONFIG_KEY: &str = "__IMPRESSPRESS_RUNTIME_KIND__";

/// Whether this runtime runs inside a browser, where nothing secret can be
/// held. Read off the synchronous `config_get` snapshot, never through the
/// config client: see `products::stripe_secret_operations_allowed` for why a
/// client read of this key is refused.
pub fn is_browser(ctx: &dyn Context) -> bool {
    ctx.config_get(RUNTIME_KIND_CONFIG_KEY) == Some("browser")
}
