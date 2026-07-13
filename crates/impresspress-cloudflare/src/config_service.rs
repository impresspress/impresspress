use std::collections::HashMap;

use wafer_core::interfaces::config::service::ConfigService;

/// ConfigService backed by a pre-loaded HashMap (from D1 variables table).
/// Read-only in practice — `set()` is a no-op since CF workers are stateless.
pub struct HashMapConfigService {
    vars: HashMap<String, String>,
}

// Safety: wasm32-unknown-unknown is single-threaded.
unsafe impl Send for HashMapConfigService {}
unsafe impl Sync for HashMapConfigService {}

impl HashMapConfigService {
    pub fn new(vars: HashMap<String, String>) -> Self {
        Self { vars }
    }
}

impl ConfigService for HashMapConfigService {
    fn get(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }

    fn set(&self, _key: &str, _value: &str) {
        // No-op — CF workers are stateless, config is loaded per-request from D1.
    }
}
