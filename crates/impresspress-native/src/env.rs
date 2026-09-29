//! Environment-variable bootstrap helpers for native WAFER apps.

use std::{collections::HashMap, path::Path};

/// Load `.env` from `dir`. Honors `IMPRESSPRESS_ENV_FILE` for an explicit
/// path (absolute or relative to the current process cwd); otherwise
/// looks for `<dir>/.env`. Failures on the explicit-path form are logged
/// to stderr but do not abort. The default form silently skips when no
/// `.env` file is present, matching `dotenvy::dotenv` semantics.
///
/// Taking the dir explicitly (instead of relying on `dotenvy::dotenv`'s
/// implicit cwd walk) lets the CLI pin the env-file lookup to the
/// detected repo root without mutating global process state via
/// `std::env::set_current_dir`.
pub fn load_dotenv(dir: &Path) {
    if let Ok(path) = std::env::var("IMPRESSPRESS_ENV_FILE") {
        match dotenvy::from_filename(&path) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("warning: failed to load env file '{path}': {e}");
            }
        }
        return;
    }
    let candidate = dir.join(".env");
    if candidate.is_file() {
        let _ = dotenvy::from_path(&candidate);
    }
}

/// Collect env vars that look like app config — i.e. any key containing
/// `__`. The workspace convention (per CLAUDE.md) is:
///
/// - `WAFER_RUN_SHARED__*` — shared app config (any block reads it)
/// - `{ORG}__{BLOCK}__*` — block-scoped (only the owner block + admin)
/// - `IMPRESSPRESS_*` (no `__`) — infrastructure, never seeded into the DB
///
/// The presence of `__` is the discriminator: every app/block config key
/// contains it, infra keys never do.
///
/// Consumers who want additional filtering (e.g., only env vars that
/// match declared config var keys) should apply their own filter on top
/// of this result.
pub fn collect_app_env_vars() -> HashMap<String, String> {
    filter_app_env_vars(std::env::vars())
}

/// Pure filter: keeps any pair whose key contains `__`.
///
/// Split out so tests can exercise the filter without mutating the
/// process environment (which is `unsafe` in Rust 2024 and races with
/// parallel test runs).
pub(crate) fn filter_app_env_vars<I>(iter: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    iter.into_iter().filter(|(k, _)| k.contains("__")).collect()
}

#[cfg(test)]
mod tests {
    use impresspress_core::blocks::auth::{config::BOOTSTRAP_ADMIN_EMAIL_KEY, JWT_SECRET_KEY};

    use super::*;

    #[test]
    fn filter_keeps_shared_and_block_scoped_drops_infra_and_plain() {
        let input = vec![
            // Shared app config — keep.
            (
                BOOTSTRAP_ADMIN_EMAIL_KEY.to_string(),
                "admin@example.com".to_string(),
            ),
            // Block-scoped — keep.
            (JWT_SECRET_KEY.to_string(), "abc".to_string()),
            // Infra — drop.
            (LISTEN_VAR.to_string(), "0.0.0.0:8090".to_string()),
            (DB_PATH_VAR.to_string(), "data/impresspress.db".to_string()),
            // Plain env vars without `__` — drop.
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/home/joris".to_string()),
        ];
        let out = filter_app_env_vars(input);
        assert!(out.contains_key(BOOTSTRAP_ADMIN_EMAIL_KEY));
        assert!(out.contains_key(JWT_SECRET_KEY));
        assert!(!out.contains_key(LISTEN_VAR));
        assert!(!out.contains_key(DB_PATH_VAR));
        assert!(!out.contains_key("PATH"));
        assert!(!out.contains_key("HOME"));
    }

    #[test]
    fn filter_app_env_vars_empty_iterator_returns_empty_map() {
        let out = filter_app_env_vars(std::iter::empty());
        assert!(out.is_empty());
    }
}

/// The `IMPRESSPRESS_*` process environment variables [`InfraConfig`] reads.
const LISTEN_VAR: &str = "IMPRESSPRESS_LISTEN";
const DB_TYPE_VAR: &str = "IMPRESSPRESS_DB_TYPE";
const DB_PATH_VAR: &str = "IMPRESSPRESS_DB_PATH";
const DB_URL_VAR: &str = "IMPRESSPRESS_DB_URL";
const STORAGE_TYPE_VAR: &str = "IMPRESSPRESS_STORAGE_TYPE";
const STORAGE_ROOT_VAR: &str = "IMPRESSPRESS_STORAGE_ROOT";
const MODEL_CACHE_DIR_VAR: &str = "IMPRESSPRESS_MODEL_CACHE_DIR";
const TRUSTED_PROXIES_VAR: &str = "IMPRESSPRESS_TRUSTED_PROXIES";
const HEADER_READ_TIMEOUT_VAR: &str = "IMPRESSPRESS_HEADER_READ_TIMEOUT_SECS";
const BODY_READ_TIMEOUT_VAR: &str = "IMPRESSPRESS_BODY_READ_TIMEOUT_SECS";
const MAX_CONNECTIONS_VAR: &str = "IMPRESSPRESS_MAX_CONNECTIONS";
const WRITE_TIMEOUT_VAR: &str = "IMPRESSPRESS_WRITE_TIMEOUT_SECS";
const SHUTDOWN_GRACE_VAR: &str = "IMPRESSPRESS_SHUTDOWN_GRACE_SECS";

/// The `wafer-run/http-listener` settings an operator sets through
/// `IMPRESSPRESS_*` variables, one field per listener config key.
///
/// A field is `None` when its variable is unset, and the listener's own
/// default applies: `register_http_listener` passes only the set ones, so
/// the listener stays the single source of its defaults and of their
/// validation. A set value is passed as written; the listener checks it at
/// `Init`, and a bad value fails boot naming the key.
///
/// - `IMPRESSPRESS_TRUSTED_PROXIES` → `trusted_proxies`: comma-separated IPs
///   or CIDR ranges whose `X-Forwarded-For` is honored. Unset trusts none,
///   so behind a reverse proxy every client shares the proxy's address.
/// - `IMPRESSPRESS_HEADER_READ_TIMEOUT_SECS` → `header_read_timeout_secs`
/// - `IMPRESSPRESS_BODY_READ_TIMEOUT_SECS` → `body_read_timeout_secs`
/// - `IMPRESSPRESS_MAX_CONNECTIONS` → `max_connections`
/// - `IMPRESSPRESS_WRITE_TIMEOUT_SECS` → `write_timeout_secs`
/// - `IMPRESSPRESS_SHUTDOWN_GRACE_SECS` → `shutdown_grace_secs`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListenerEnv {
    pub trusted_proxies: Option<String>,
    pub header_read_timeout_secs: Option<String>,
    pub body_read_timeout_secs: Option<String>,
    pub max_connections: Option<String>,
    pub write_timeout_secs: Option<String>,
    pub shutdown_grace_secs: Option<String>,
}

impl ListenerEnv {
    /// Read the variables through `var`, which answers `None` for an unset
    /// one. [`InfraConfig::from_env`] passes the process environment.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            trusted_proxies: var(TRUSTED_PROXIES_VAR),
            header_read_timeout_secs: var(HEADER_READ_TIMEOUT_VAR),
            body_read_timeout_secs: var(BODY_READ_TIMEOUT_VAR),
            max_connections: var(MAX_CONNECTIONS_VAR),
            write_timeout_secs: var(WRITE_TIMEOUT_VAR),
            shutdown_grace_secs: var(SHUTDOWN_GRACE_VAR),
        }
    }

    /// The set settings as `(listener config key, value)` pairs.
    pub fn set(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("trusted_proxies", &self.trusted_proxies),
            ("header_read_timeout_secs", &self.header_read_timeout_secs),
            ("body_read_timeout_secs", &self.body_read_timeout_secs),
            ("max_connections", &self.max_connections),
            ("write_timeout_secs", &self.write_timeout_secs),
            ("shutdown_grace_secs", &self.shutdown_grace_secs),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.as_deref().map(|value| (key, value)))
    }
}

/// Infrastructure config read from `IMPRESSPRESS_*` env vars.
///
/// `Debug` is hand-written: `db_url` carries the database password, so it is
/// shown only as [`crate::database::postgres_target`] describes it.
pub struct InfraConfig {
    pub listen: String,
    pub db_type: String,
    pub db_path: String,
    pub db_url: Option<String>,
    pub storage_type: String,
    pub storage_root: String,
    /// Where the native ONNX embedding model's weights are cached
    /// (`IMPRESSPRESS_MODEL_CACHE_DIR`), handed to
    /// `ImpresspressBuilder::model_cache_dir`. Read only by a build with
    /// the `block-fastembed` feature (which `native-embedding` implies).
    pub model_cache_dir: String,
    pub listener: ListenerEnv,
}

impl InfraConfig {
    pub fn from_env() -> Self {
        Self {
            listen: env_or(LISTEN_VAR, "0.0.0.0:8090"),
            db_type: env_or(DB_TYPE_VAR, "sqlite"),
            db_path: env_or(DB_PATH_VAR, "data/impresspress.db"),
            db_url: std::env::var(DB_URL_VAR).ok(),
            storage_type: env_or(STORAGE_TYPE_VAR, "local"),
            storage_root: env_or(STORAGE_ROOT_VAR, "data/storage"),
            model_cache_dir: env_or(MODEL_CACHE_DIR_VAR, "data/models"),
            listener: ListenerEnv::from_vars(|key| std::env::var(key).ok()),
        }
    }
}

impl std::fmt::Debug for InfraConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InfraConfig")
            .field("listen", &self.listen)
            .field("db_type", &self.db_type)
            .field("db_path", &self.db_path)
            .field(
                "db_url",
                &self.db_url.as_deref().map(crate::database::postgres_target),
            )
            .field("storage_type", &self.storage_type)
            .field("storage_root", &self.storage_root)
            .field("model_cache_dir", &self.model_cache_dir)
            .field("listener", &self.listener)
            .finish()
    }
}

#[cfg(test)]
mod infra_config_tests {
    use super::InfraConfig;

    #[test]
    fn debug_never_shows_the_db_password() {
        let infra = InfraConfig {
            listen: "0.0.0.0:8090".into(),
            db_type: "postgres".into(),
            db_path: "data/impresspress.db".into(),
            db_url: Some("postgres://app:hunter2@db.internal:5432/prod".into()),
            storage_type: "local".into(),
            storage_root: "data/storage".into(),
            model_cache_dir: "data/models".into(),
            listener: super::ListenerEnv::default(),
        };
        let shown = format!("{infra:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("db.internal:5432/prod"), "{shown}");
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
