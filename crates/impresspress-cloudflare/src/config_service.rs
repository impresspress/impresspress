use std::collections::HashMap;

use wafer_core::interfaces::config::service::ConfigService;

/// ConfigService backed by a pre-loaded HashMap (from D1 variables table).
/// Read-only in practice — `set()` is a no-op since CF workers are stateless.
pub struct HashMapConfigService {
    vars: HashMap<String, String>,
}

// No `unsafe impl Send/Sync`: a `HashMap<String, String>` is already both, so
// the compiler derives them. See the note in `logger_service`.

impl HashMapConfigService {
    pub fn new(vars: HashMap<String, String>) -> Self {
        Self { vars }
    }
}

impl ConfigService for HashMapConfigService {
    fn get(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }

    /// Deliberately a no-op, and deliberately silent about it.
    ///
    /// A Worker's config is loaded per request from D1; there is no isolate
    /// state a `set` could usefully write to, and the durable copy is written
    /// through the admin block's variables repo, not through this trait.
    ///
    /// It cannot report the refusal either: `ConfigService::set` returns `()`
    /// upstream (`wafer-core`'s `interfaces/config/service.rs`), so making it a
    /// `Result` is a producer change that lands on every other consumer of that
    /// library for one adapter's benefit. Recorded rather than done — see spec
    /// 2.10. Logging on every call was considered and rejected: nothing in the
    /// tree calls `set` on this service, so the line would be noise waiting for
    /// a caller that does not exist.
    fn set(&self, _key: &str, _value: &str) {}
}
