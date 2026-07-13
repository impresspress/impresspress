//! Worker-binding name constants for `impresspress_cloudflare::run()`.
//!
//! Binding-name convention (matches `impresspress-cloud/crates/impresspress-worker`):
//! - D1 binding: `"DB"`
//! - R2 binding: `"STORAGE"`
//! - KV binding: `"CONFIG_CACHE"`
//!
//! Consumers must use these names in their `wrangler.toml`. The CLI's
//! `helpers::cloudflare::wrangler` generator emits them as defaults.
//! v2 may take a `RunConfig` parameter for custom binding names.
//!
//! The block-settings load + hash-gated seed and the auto-generated-secret
//! seeder that used to live here now live in `impresspress-core`
//! (`features::load_and_seed_block_settings`, `boot::seed_auto_generated`) so
//! all three targets share one implementation. This module is now just the
//! binding-name registry.

pub(crate) const D1_BINDING: &str = "DB";
pub(crate) const R2_BINDING: &str = "STORAGE";
pub(crate) const KV_BINDING: &str = "CONFIG_CACHE";
