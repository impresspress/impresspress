//! Resolve the `[cloudflare]` section from impresspress.toml + env vars.
//!
//! Resolution rule for env-overlayable fields: **env > toml > error**.
//! That makes clone-and-deploy work: a fresh checkout has no deployer
//! identifiers committed, and a `.env` supplies them at deploy time.
//!
//! Bindings (`d1.binding`, `r2.binding`) stay toml-only because they're
//! code contracts — the worker reads `env.DB` / `env.STORAGE` by exact
//! name, so changing one without changing the other breaks the worker.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::wrangler::{CloudflareConfig, D1Config, R2Config};

/// Toml-shaped, pre-resolution. Every field that can be supplied via env
/// is `Option<String>`.
#[derive(Debug, Deserialize)]
pub struct RawCloudflareConfig {
    pub account_id: Option<String>,
    pub worker_name: Option<String>,
    pub compatibility_date: Option<String>,
    pub d1: RawD1Config,
    pub r2: RawR2Config,
    pub wrangler_overrides_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub struct RawD1Config {
    pub binding: String,
    pub database_name: Option<String>,
    pub database_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RawR2Config {
    pub binding: String,
    pub bucket_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImpresspressTomlPartial {
    cloudflare: Option<RawCloudflareConfig>,
}

/// Parse `<repo_root>/impresspress.toml` and return the unresolved `[cloudflare]`
/// section.
///
/// # Errors
///
/// Returns an error if `impresspress.toml` cannot be read, cannot be parsed,
/// or does not declare a `[cloudflare]` table.
pub fn parse(repo_root: &Path) -> Result<RawCloudflareConfig> {
    let path = repo_root.join("impresspress.toml");
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let parsed: ImpresspressTomlPartial =
        toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    parsed.cloudflare.ok_or_else(|| {
        anyhow!(
            "{} is missing a [cloudflare] section — see \
             docs/superpowers/specs/2026-05-06-impresspress-cloudflare-cli-flow-design.md",
            path.display()
        )
    })
}

impl RawCloudflareConfig {
    /// Apply env-var overlay and validate every required field is present.
    /// `env` is a getter so callers (and tests) can inject any source.
    pub fn resolve<F: Fn(&str) -> Option<String>>(self, env: F) -> Result<CloudflareConfig> {
        let account_id = pick(
            env("CLOUDFLARE_ACCOUNT_ID"),
            self.account_id,
            "account_id",
            "CLOUDFLARE_ACCOUNT_ID",
        )?;
        let worker_name = pick(
            env("IMPRESSPRESS_CLOUDFLARE_WORKER_NAME"),
            self.worker_name,
            "worker_name",
            "IMPRESSPRESS_CLOUDFLARE_WORKER_NAME",
        )?;
        let compatibility_date = pick(
            env("IMPRESSPRESS_CLOUDFLARE_COMPATIBILITY_DATE"),
            self.compatibility_date,
            "compatibility_date",
            "IMPRESSPRESS_CLOUDFLARE_COMPATIBILITY_DATE",
        )?;
        let d1_database_name = pick(
            env("IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_NAME"),
            self.d1.database_name,
            "d1.database_name",
            "IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_NAME",
        )?;
        let d1_database_id = pick(
            env("IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID"),
            self.d1.database_id,
            "d1.database_id",
            "IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID",
        )?;
        let r2_bucket_name = pick(
            env("IMPRESSPRESS_CLOUDFLARE_R2_BUCKET_NAME"),
            self.r2.bucket_name,
            "r2.bucket_name",
            "IMPRESSPRESS_CLOUDFLARE_R2_BUCKET_NAME",
        )?;
        Ok(CloudflareConfig {
            account_id,
            worker_name,
            compatibility_date,
            d1: D1Config {
                binding: self.d1.binding,
                database_name: d1_database_name,
                database_id: d1_database_id,
            },
            r2: R2Config {
                binding: self.r2.binding,
                bucket_name: r2_bucket_name,
            },
            wrangler_overrides_path: self.wrangler_overrides_path,
        })
    }
}

fn pick(
    env_val: Option<String>,
    toml_val: Option<String>,
    toml_key: &str,
    env_var: &str,
) -> Result<String> {
    env_val.or(toml_val).ok_or_else(|| {
        anyhow!(
            "missing required cloudflare config: set [cloudflare.{toml_key}] in impresspress.toml \
             or env {env_var}"
        )
    })
}

/// Production entry point — parse + resolve via `std::env::var`.
///
/// # Errors
///
/// Returns an error from [`parse`] (missing/unparseable `impresspress.toml`)
/// or [`RawCloudflareConfig::resolve`] (missing required field after env
/// overlay).
pub fn load(repo_root: &Path) -> Result<CloudflareConfig> {
    let raw = parse(repo_root)?;
    raw.resolve(|name| std::env::var(name).ok())
}

/// Validate that `CLOUDFLARE_API_TOKEN` is set. Required for `deploy`,
/// not for `build`/`serve`.
///
/// # Errors
///
/// Returns an error if the `CLOUDFLARE_API_TOKEN` env var is not set.
pub fn require_api_token() -> Result<String> {
    std::env::var("CLOUDFLARE_API_TOKEN").map_err(|_| {
        anyhow!(
            "missing env CLOUDFLARE_API_TOKEN — required for `impresspress deploy --target cloudflare`"
        )
    })
}
