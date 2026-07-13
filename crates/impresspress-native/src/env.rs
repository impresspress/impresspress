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
    use super::*;

    #[test]
    fn filter_keeps_shared_and_block_scoped_drops_infra_and_plain() {
        let input = vec![
            // Shared app config — keep.
            (
                "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL".to_string(),
                "admin@example.com".to_string(),
            ),
            // Block-scoped — keep.
            (
                "WAFER_RUN__AUTH__JWT_SECRET".to_string(),
                "abc".to_string(),
            ),
            // Infra — drop.
            ("IMPRESSPRESS_LISTEN".to_string(), "0.0.0.0:8090".to_string()),
            (
                "IMPRESSPRESS_DB_PATH".to_string(),
                "data/impresspress.db".to_string(),
            ),
            // Plain env vars without `__` — drop.
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/home/joris".to_string()),
        ];
        let out = filter_app_env_vars(input);
        assert!(out.contains_key("WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL"));
        assert!(out.contains_key("WAFER_RUN__AUTH__JWT_SECRET"));
        assert!(!out.contains_key("IMPRESSPRESS_LISTEN"));
        assert!(!out.contains_key("IMPRESSPRESS_DB_PATH"));
        assert!(!out.contains_key("PATH"));
        assert!(!out.contains_key("HOME"));
    }

    #[test]
    fn filter_app_env_vars_empty_iterator_returns_empty_map() {
        let out = filter_app_env_vars(std::iter::empty());
        assert!(out.is_empty());
    }
}

/// Infrastructure config read from `IMPRESSPRESS_*` env vars.
#[derive(Debug)]
pub struct InfraConfig {
    pub listen: String,
    pub db_type: String,
    pub db_path: String,
    pub db_url: Option<String>,
    pub storage_type: String,
    pub storage_root: String,
}

impl InfraConfig {
    pub fn from_env() -> Self {
        Self {
            listen: env_or("IMPRESSPRESS_LISTEN", "0.0.0.0:8090"),
            db_type: env_or("IMPRESSPRESS_DB_TYPE", "sqlite"),
            db_path: env_or("IMPRESSPRESS_DB_PATH", "data/impresspress.db"),
            db_url: std::env::var("IMPRESSPRESS_DB_URL").ok(),
            storage_type: env_or("IMPRESSPRESS_STORAGE_TYPE", "local"),
            storage_root: env_or("IMPRESSPRESS_STORAGE_ROOT", "data/storage"),
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
