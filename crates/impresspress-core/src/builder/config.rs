//! [`RuntimeConfig`] — the one owner of the runtime's two config surfaces.
//!
//! A WAFER runtime carries the same configuration twice:
//!
//! - the **async** surface, an `Arc<dyn ConfigService>` behind the
//!   `wafer-run/config` service block, which blocks read through
//!   `ctx.config_get_async` / the config client; and
//! - the **synchronous snapshot**, `Wafer::set_config_snapshot`, which
//!   `ctx.config_get` reads with no I/O — the surface `migration_helper`'s
//!   gate, the CSRF check and every `lifecycle(Init)` on a stateless target
//!   actually consult.
//!
//! Every target used to fill both by hand, from separate literals, with a
//! comment saying "Both must carry the same data" as the only thing holding
//! them together. `RuntimeConfig` makes that structural: a key is written once
//! and lands in both, and the *only* way to hand a `ConfigService` to
//! [`ImpresspressBuilder`] is [`RuntimeConfig::install`], which takes the
//! snapshot in the same call — so a target cannot fill the async surface and
//! leave the snapshot empty. See [`RuntimeConfig::install`] for the one thing
//! that is still the target's own contract: using the map it is handed.
//!
//! The one legitimate divergence — Cloudflare's per-request application config,
//! which must reach the async service map but must never be baked into a
//! snapshot an isolate-cached runtime keeps across requests — is
//! [`RuntimeConfig::service_only`], which requires the reason as an argument.

use std::{collections::HashMap, sync::Arc};

use wafer_core::interfaces::config::service::ConfigService;
use wafer_run::Wafer;

use super::ImpresspressBuilder;

/// One key that is deliberately on the async service surface only, with the
/// reason it is not in the synchronous snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceOnlyKey {
    pub key: String,
    pub because: &'static str,
}

/// Both config surfaces, assembled once and installed together.
///
/// Build it with [`both`](Self::both) (the default) and
/// [`service_only`](Self::service_only) (the documented exception), then either
/// [`install`](Self::install) it into an [`ImpresspressBuilder`] before
/// `build()`, or [`republish`](Self::republish) it onto an already-built
/// runtime from a [`BootHooks`](super::BootHooks) seed step.
#[derive(Debug, Default, Clone)]
pub struct RuntimeConfig {
    service: HashMap<String, String>,
    snapshot: HashMap<String, String>,
    service_only: Vec<ServiceOnlyKey>,
}

impl RuntimeConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `key` on **both** surfaces. This is what almost every key wants.
    ///
    /// Promoting a key that was previously [`service_only`](Self::service_only)
    /// also retracts its recorded divergence, so
    /// [`service_only_keys`](Self::service_only_keys) never names a key that is
    /// in fact on both surfaces.
    pub fn both(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        let key = key.into();
        let value = value.into();
        self.service.insert(key.clone(), value.clone());
        self.service_only.retain(|entry| entry.key != key);
        self.snapshot.insert(key, value);
        self
    }

    /// [`both`](Self::both) for a whole map, in one call.
    pub fn extend_both<K, V>(&mut self, entries: impl IntoIterator<Item = (K, V)>) -> &mut Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        for (key, value) in entries {
            self.both(key, value);
        }
        self
    }

    /// Set `key` on the async `ConfigService` surface only, and record why it
    /// is kept out of the synchronous snapshot.
    ///
    /// `because` is a required argument rather than a comment so the exception
    /// is readable at the call site — the divergence it exists for used to be
    /// explained two hundred lines away from the code that caused it.
    pub fn service_only(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
        because: &'static str,
    ) -> &mut Self {
        let key = key.into();
        self.service.insert(key.clone(), value.into());
        self.snapshot.remove(&key);
        self.service_only.push(ServiceOnlyKey { key, because });
        self
    }

    /// Every key that is deliberately on one surface only, with its reason.
    /// Exposed so a test can assert that the set of divergences is the set
    /// somebody declared.
    pub fn service_only_keys(&self) -> &[ServiceOnlyKey] {
        &self.service_only
    }

    /// True when `key` is on the synchronous snapshot surface.
    pub fn snapshot_contains(&self, key: &str) -> bool {
        self.snapshot.contains_key(key)
    }

    /// The value on the async service surface, if any. Reading a key back is
    /// deliberately all a caller can do with that map — there is no accessor
    /// that hands out the whole thing, because owning it is what let a target
    /// build a `ConfigService` and forget the snapshot.
    pub fn service_get(&self, key: &str) -> Option<&str> {
        self.service.get(key).map(String::as_str)
    }

    /// Hand both surfaces to their owners: `make_service` turns the async map
    /// into the target's own `ConfigService` (an `EnvConfigService` filled by
    /// `set`, a map-backed Cloudflare service, or a shared handle the browser
    /// already holds), and the snapshot is stored on the builder, which
    /// installs it on the `Wafer` at the end of `build()`.
    ///
    /// `make_service` returns `(service_for_the_builder, kept)`, and `install`
    /// returns `(builder, kept)`. `kept` is whatever the target needs to hold
    /// on to besides the service the builder gets: Cloudflare builds the
    /// request-current concrete service from the map and hands the builder a
    /// stateless forwarder instead, and used to smuggle the concrete one back
    /// out through a captured `Option` plus an `expect` for a panic that could
    /// not happen. Targets with nothing to keep return `()`.
    ///
    /// # What this does, and does not, make impossible
    ///
    /// Enforced by the type system: this is the only way to give a builder a
    /// `ConfigService`, and it takes the snapshot in the same call — so a
    /// target cannot fill the async surface and leave the synchronous one
    /// empty.
    ///
    /// Not enforced: that `make_service` uses the map it is handed. A
    /// constructor that drops it fills the snapshot and leaves the async
    /// surface empty, which is the same defect facing the other way. That was
    /// not hypothetical — the browser's closure was `|_empty| config_svc`,
    /// correct only for as long as its `RuntimeConfig` stayed empty. Every
    /// target consumes the map today, and a target whose service is filled by
    /// `set` should use [`fill_config_service`] rather than write the loop
    /// again.
    pub fn install<T>(
        self,
        builder: ImpresspressBuilder,
        make_service: impl FnOnce(HashMap<String, String>) -> (Arc<dyn ConfigService>, T),
    ) -> (ImpresspressBuilder, T) {
        let (service, kept) = make_service(self.service);
        (builder.with_config_surfaces(service, self.snapshot), kept)
    }

    /// Publish both surfaces onto an already-built runtime — the post-admin-init
    /// seed step, where a target learns values that did not exist at `build()`
    /// time (the browser's seeded variables and JWT secret; Cloudflare's
    /// structural block settings).
    ///
    /// Merges into the existing snapshot rather than replacing it, so keys the
    /// builder installed survive.
    ///
    /// Note for targets whose `ConfigService` is immutable (Cloudflare's is
    /// constructed from a map and its `set` is a documented no-op): the write
    /// to the async surface is then a no-op and only the snapshot moves. That
    /// is a property of the service, not of this call — writing both is still
    /// the correct intent to express here.
    pub fn republish(self, wafer: &mut Wafer, service: &Arc<dyn ConfigService>) {
        for (key, value) in &self.service {
            service.set(key, value);
        }
        let mut snapshot = (**wafer.config_snapshot()).clone();
        snapshot.extend(self.snapshot);
        write_snapshot(wafer, snapshot);
    }
}

/// [`RuntimeConfig::install`]'s `make_service` for a target whose
/// `ConfigService` is filled by `set` rather than constructed from the map:
/// native's `EnvConfigService`, the browser's shared handle. Writes every key
/// through and hands the same service back.
///
/// Exists so "use the map you were given" is a call rather than a loop each
/// target writes for itself — the loop the browser did not write.
pub fn fill_config_service(
    service: Arc<dyn ConfigService>,
    map: HashMap<String, String>,
) -> Arc<dyn ConfigService> {
    for (key, value) in &map {
        service.set(key, value);
    }
    service
}

/// The single non-test `Wafer::set_config_snapshot` call site in the workspace.
/// Both entry points above funnel through here so there is exactly one place
/// that decides what the synchronous surface holds.
pub(super) fn write_snapshot(wafer: &mut Wafer, snapshot: HashMap<String, String>) {
    wafer.set_config_snapshot(snapshot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_writes_each_key_to_each_surface() {
        let mut config = RuntimeConfig::new();
        config.both("A", "1").both("B", "2");

        assert_eq!(config.service_get("A"), Some("1"));
        assert!(config.snapshot_contains("A"));
        assert_eq!(config.service_get("B"), Some("2"));
        assert!(config.snapshot_contains("B"));
        assert!(config.service_only_keys().is_empty());
    }

    #[test]
    fn service_only_writes_one_surface_and_records_the_reason() {
        let mut config = RuntimeConfig::new();
        config.service_only("A", "1", "request-scoped; must not be cached");

        assert_eq!(config.service_get("A"), Some("1"));
        assert!(
            !config.snapshot_contains("A"),
            "service_only must keep the key out of the snapshot",
        );
        assert_eq!(
            config.service_only_keys(),
            &[ServiceOnlyKey {
                key: "A".to_string(),
                because: "request-scoped; must not be cached",
            }],
        );
    }

    /// A key promoted from `service_only` to `both` must actually reach the
    /// snapshot — Cloudflare's prepare flag is exactly this: it arrives in the
    /// request config (service-only) and is then deliberately retained on the
    /// snapshot too.
    #[test]
    fn both_after_service_only_puts_the_key_back_on_the_snapshot() {
        let mut config = RuntimeConfig::new();
        config
            .service_only("A", "1", "request-scoped")
            .both("A", "1");

        assert!(config.snapshot_contains("A"));
        assert_eq!(config.service_get("A"), Some("1"));
        assert!(
            config.service_only_keys().is_empty(),
            "a promoted key must stop being reported as a divergence",
        );
    }

    #[test]
    fn extend_both_fans_a_map_into_both_surfaces() {
        let mut config = RuntimeConfig::new();
        config.extend_both([("A".to_string(), "1".to_string())]);

        assert_eq!(config.service_get("A"), Some("1"));
        assert!(config.snapshot_contains("A"));
    }
}
