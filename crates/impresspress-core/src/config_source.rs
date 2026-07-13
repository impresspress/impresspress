//! EnvConfigSource — native target's [`ConfigSource`] impl.
//!
//! Reads from `std::env`. Impresspress blocks declare their [`ConfigVar`] keys
//! via `BlockInfo::config_keys`; the runtime calls
//! [`ConfigSource::load_for_block`] on first init per block to resolve them.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §2

use std::collections::HashMap;

use async_trait::async_trait;
use wafer_block::ConfigVar;
use wafer_run::{ConfigError, ConfigSource, EnvBlockConfig};

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
