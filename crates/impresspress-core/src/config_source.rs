//! Impresspress's [`ConfigSource`] impls: how a block's declared
//! [`ConfigVar`] keys are resolved at its first `lifecycle(Init)`.
//!
//! Blocks declare their keys via `BlockInfo::config_keys`; the runtime calls
//! [`ConfigSource::load_for_block`] once per block to resolve them, and a
//! required key it cannot resolve is `InitError::Permanent` — the block never
//! initialises and every one of its routes answers `412`. So the source a
//! target installs is not a convenience: it is what decides which blocks that
//! target can run at all.
//!
//! Two impls live here, one per shape of "when does this target know its
//! config":
//!
//! * [`EnvConfigSource`] — reads `std::env`. Not used by the native server
//!   itself (it seeds pre-wafer and passes a
//!   [`wafer_run::StaticConfigSource`] built from the loaded variables), but
//!   the right source for a process whose config really is its environment.
//! * [`SharedConfigSource`] — an empty map at build time, filled once the
//!   variables are known. The browser target's, because it cannot know them
//!   until after the runtime is sealed.
//!
//! Cloudflare's `D1ConfigSource` (in `impresspress-cloudflare`) is the third
//! shape: it queries the variables table on demand.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use wafer_block::ConfigVar;
use wafer_run::{ConfigError, ConfigSource, EnvBlockConfig, StaticConfigSource};

/// Reads block-declared config keys from `std::env`, falling back to each
/// [`ConfigVar`]'s `default` when the env var is unset.
///
/// Returns [`ConfigError::MissingRequired`] for keys with `optional = false`
/// where neither the env var nor a non-empty default is available. Optional
/// keys with no value and no default are skipped silently — block code's
/// `EnvBlockConfig::get()` then returns `None`.
#[derive(Debug, Default)]
pub struct EnvConfigSource;

impl EnvConfigSource {
    pub fn new() -> Self {
        Self
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl ConfigSource for EnvConfigSource {
    async fn load_for_block(
        &self,
        block: &str,
        declared_keys: &[ConfigVar],
    ) -> Result<EnvBlockConfig, ConfigError> {
        let mut out = HashMap::with_capacity(declared_keys.len());
        for var in declared_keys {
            let resolved = std::env::var(&var.key).ok().or_else(|| {
                if var.default.is_empty() {
                    None
                } else {
                    Some(var.default.clone())
                }
            });

            match resolved {
                Some(v) => {
                    out.insert(var.key.clone(), v);
                }
                None if !var.optional => {
                    // optional == false means required.
                    return Err(ConfigError::MissingRequired {
                        block: block.to_string(),
                        key: var.key.clone(),
                    });
                }
                None => {
                    // optional + no default + no env: skip; caller's
                    // EnvBlockConfig::get() will return None.
                }
            }
        }
        Ok(EnvBlockConfig::new(out))
    }
}

/// A [`ConfigSource`] whose map is published *after* the runtime is built.
///
/// Every target resolves a block's declared [`ConfigVar`]s from the same
/// place — the seeded `impresspress__admin__variables` rows — but they reach
/// it at different times. Native seeds pre-wafer and hands the loaded map
/// straight to [`wafer_run::StaticConfigSource::new`]; Cloudflare's
/// `D1ConfigSource` queries D1 on demand. The browser can do neither: the
/// variables table only exists once the admin block's `lifecycle(Init)` has
/// run its migration, and admin cannot run until the wafer is built and
/// sealed. So the browser has to build first and learn its config second.
///
/// This is the source that lets it. The runtime is built with an empty one;
/// the post-admin-init boot hook fills it with the map it just seeded, in the
/// same breath as it publishes into the `ConfigService`, the block-settings
/// handle, the JWT-secret handle and the config snapshot. Every block whose
/// `lifecycle(Init)` runs after that point — which is every block except
/// admin itself — then resolves its declared keys exactly as it would on
/// native.
///
/// Without it, a browser bundle's blocks resolve against an empty map, and a
/// block declaring a **required** key with an empty default fails
/// `InitError::Permanent` before its `lifecycle(Init)` is ever called — no
/// matter that the block would have read the value through
/// `wafer-run/config` at request time. That is not hypothetical: it is what
/// took `impresspress/products` (whose auto-generated
/// `IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET` is declared required) out of
/// every browser bundle, answering `412 FailedPrecondition` on every
/// products route.
///
/// Resolution order is [`StaticConfigSource`]'s because it *is*
/// [`StaticConfigSource`]: this type is a published-later wrapper around one,
/// not a second implementation of the ladder. The ladder (published value,
/// then non-empty declared default, then `MissingRequired` for a required key
/// / silently absent for an optional one) lives in wafer-run, on the other
/// side of a repo boundary, and a hand-copied duplicate here would drift the
/// moment wafer-run changed it — silently, because the copy would go on
/// passing its own tests.
#[derive(Debug, Default)]
pub struct SharedConfigSource {
    /// Behind an `Arc` so [`Self::load_for_block`] can take a cheap handle out
    /// from under the lock instead of cloning the whole variable map on every
    /// block's first init. `publish` swaps the whole `Arc`, so a load already
    /// in flight keeps reading a consistent snapshot.
    inner: std::sync::RwLock<Arc<StaticConfigSource>>,
}

impl SharedConfigSource {
    /// An empty source — what a runtime is built with.
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish the seeded variables. Replaces whatever was there: the caller
    /// has just loaded the full set back from the database, so a merge would
    /// keep values a previous boot published and this one deliberately did
    /// not (an operator deleting a variable row, say).
    pub fn publish(&self, vars: HashMap<String, String>) {
        *self
            .inner
            .write()
            .expect("SharedConfigSource RwLock poisoned") = Arc::new(StaticConfigSource::new(vars));
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl ConfigSource for SharedConfigSource {
    async fn load_for_block(
        &self,
        block: &str,
        declared_keys: &[ConfigVar],
    ) -> Result<EnvBlockConfig, ConfigError> {
        // Take the handle and drop the guard in one statement, before the
        // `await`: an `RwLockReadGuard` is not `Send`, and this trait method
        // is `Send` on every target but wasm32.
        let source = Arc::clone(
            &self
                .inner
                .read()
                .expect("SharedConfigSource RwLock poisoned"),
        );
        source.load_for_block(block, declared_keys).await
    }
}

#[cfg(test)]
mod shared_config_source_tests {
    use super::*;

    fn required(key: &str, default: &str) -> ConfigVar {
        ConfigVar::new(key, "d", default)
    }

    fn optional(key: &str) -> ConfigVar {
        ConfigVar::new(key, "d", "").optional()
    }

    /// The defect this type exists for: an empty source refuses a required
    /// key with no default, and that refusal is `InitError::Permanent` in the
    /// runtime — the whole block never initialises.
    #[tokio::test]
    async fn empty_refuses_a_required_key_with_no_default() {
        let source = SharedConfigSource::new();
        let err = source
            .load_for_block("impresspress/products", &[required("A__B__SECRET", "")])
            .await
            .expect_err("a required key with no value and no default must refuse");
        assert!(matches!(
            err,
            ConfigError::MissingRequired { ref key, .. } if key == "A__B__SECRET"
        ));
    }

    /// …and publishing the seeded map is what makes the same block init.
    #[tokio::test]
    async fn a_published_value_satisfies_a_required_key() {
        let source = SharedConfigSource::new();
        source.publish(HashMap::from([(
            "A__B__SECRET".to_string(),
            "deadbeef".to_string(),
        )]));
        let cfg = source
            .load_for_block("impresspress/products", &[required("A__B__SECRET", "")])
            .await
            .expect("published value resolves");
        assert_eq!(cfg.get("A__B__SECRET"), Some("deadbeef"));
    }

    /// The resolution ladder, pinned end to end: published value first,
    /// declared default second, and an optional key with neither simply
    /// absent rather than an error. The rungs are wafer-run's — this type
    /// delegates to `StaticConfigSource` rather than restating them — so this
    /// is the test that notices if a pin bump changes what a block sees.
    #[tokio::test]
    async fn published_beats_default_and_optional_keys_may_be_absent() {
        let source = SharedConfigSource::new();
        source.publish(HashMap::from([("A__B__X".to_string(), "live".to_string())]));
        let cfg = source
            .load_for_block(
                "b",
                &[
                    required("A__B__X", "declared"),
                    required("A__B__Y", "declared"),
                    optional("A__B__Z"),
                ],
            )
            .await
            .expect("no required key is missing");
        assert_eq!(cfg.get("A__B__X"), Some("live"));
        assert_eq!(cfg.get("A__B__Y"), Some("declared"));
        assert_eq!(cfg.get("A__B__Z"), None);
    }

    /// `publish` replaces rather than merges, so a variable an operator
    /// removed does not survive the next boot.
    #[tokio::test]
    async fn publish_replaces_the_whole_map() {
        let source = SharedConfigSource::new();
        source.publish(HashMap::from([("A__B__X".to_string(), "one".to_string())]));
        source.publish(HashMap::from([("A__B__Y".to_string(), "two".to_string())]));
        let err = source
            .load_for_block("b", &[required("A__B__X", "")])
            .await
            .expect_err("the replaced key is gone");
        assert!(matches!(err, ConfigError::MissingRequired { .. }));
    }
}
