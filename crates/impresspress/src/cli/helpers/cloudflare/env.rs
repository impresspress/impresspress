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

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use super::wrangler::{self, CloudflareConfig, D1Config, PasswordHasherConfig, R2Config};

/// Default Workers Observability head sampling rate when neither
/// `IMPRESSPRESS_CLOUDFLARE_HEAD_SAMPLING_RATE` nor `impresspress.toml`'s
/// `[cloudflare].head_sampling_rate` is set. `1.0` (100%) preserves the
/// previous hardcoded behavior for existing consumers.
const DEFAULT_HEAD_SAMPLING_RATE: f64 = 1.0;

/// Environment variables that override the matching `impresspress.toml`
/// `[cloudflare]` field.
const WORKER_NAME_VAR: &str = "IMPRESSPRESS_CLOUDFLARE_WORKER_NAME";
const COMPATIBILITY_DATE_VAR: &str = "IMPRESSPRESS_CLOUDFLARE_COMPATIBILITY_DATE";
const D1_DATABASE_NAME_VAR: &str = "IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_NAME";
const D1_DATABASE_ID_VAR: &str = "IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID";
const R2_BUCKET_NAME_VAR: &str = "IMPRESSPRESS_CLOUDFLARE_R2_BUCKET_NAME";
const HEAD_SAMPLING_RATE_VAR: &str = "IMPRESSPRESS_CLOUDFLARE_HEAD_SAMPLING_RATE";

/// Safe application-agnostic deploy smoke path for consumers that have not
/// opted into representative dynamic routes.
const DEFAULT_DEPLOY_SMOKE_PATH: &str = "/health";

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
    /// Workers Observability head sampling rate, `0.0..=1.0`. Optional —
    /// defaults to [`DEFAULT_HEAD_SAMPLING_RATE`] when unset by both toml
    /// and env.
    pub head_sampling_rate: Option<f64>,
    /// D1 queries one Worker invocation may run: 1000 on Workers Free and
    /// Paid alike, lower only for a Worker whose `limits.subrequests` is
    /// lower (see
    /// [`impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_KEY`]).
    /// TOML-only, because the value is
    /// written into the generated config as the Worker var the runtime reads
    /// ([`impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_KEY`]);
    /// defaults to
    /// [`impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT`].
    pub d1_queries_per_invocation: Option<u64>,
    /// Cloudflare cron expressions for the Worker's `scheduled` handler.
    /// TOML-only; defaults to [`wrangler::DEFAULT_CRONS`], which is empty.
    /// Setting it is the second of the two steps that turn the sweep on — the
    /// first is exporting a `scheduled` entry point.
    pub crons: Option<Vec<String>>,
    /// Ordinary application paths exercised before promotion. TOML-only;
    /// defaults to `/health` for existing consumers.
    pub deploy_smoke_paths: Option<Vec<String>>,
    /// The password-hasher Worker. Optional: every field has a default.
    pub password_hasher: Option<RawPasswordHasherConfig>,
}

/// `[cloudflare.password_hasher]`: the second Worker that hashes and verifies
/// passwords in a Durable Object (see `impresspress_password::protocol`).
/// TOML-only.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPasswordHasherConfig {
    /// The hasher Worker's script name. Defaults to the main Worker's name
    /// with `impresspress_password::protocol::WORKER_NAME_SUFFIX` appended.
    pub worker_name: Option<String>,
    /// How many Durable Object instances hashing is spread across. Defaults
    /// to `impresspress_password::protocol::DEFAULT_SHARDS`.
    pub shards: Option<u32>,
    /// Whether the hasher refuses a stored hash without a pepper
    /// (`IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED`). Defaults to `false`. Set it
    /// only once every stored hash is peppered.
    pub pepper_required: Option<bool>,
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
    pub release_assets_dir: Option<PathBuf>,
    pub release_assets_prefix: Option<PathBuf>,
    /// Glob patterns, matched against staged logical keys, whose files are
    /// kept out of the release asset set entirely.
    pub release_assets_exclude: Option<Vec<String>>,
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
            "{} is missing a [cloudflare] section. It must declare d1.binding and \
             r2.binding, plus account_id, worker_name, compatibility_date, \
             d1.database_name, d1.database_id and r2.bucket_name — those six may \
             come from the environment instead (CLOUDFLARE_ACCOUNT_ID, \
             IMPRESSPRESS_CLOUDFLARE_WORKER_NAME, \
             IMPRESSPRESS_CLOUDFLARE_COMPATIBILITY_DATE, \
             IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_NAME, \
             IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID, \
             IMPRESSPRESS_CLOUDFLARE_R2_BUCKET_NAME).",
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
            env(WORKER_NAME_VAR),
            self.worker_name,
            "worker_name",
            WORKER_NAME_VAR,
        )?;
        let compatibility_date = pick(
            env(COMPATIBILITY_DATE_VAR),
            self.compatibility_date,
            "compatibility_date",
            COMPATIBILITY_DATE_VAR,
        )?;
        let d1_database_name = pick(
            env(D1_DATABASE_NAME_VAR),
            self.d1.database_name,
            "d1.database_name",
            D1_DATABASE_NAME_VAR,
        )?;
        let d1_database_id = pick(
            env(D1_DATABASE_ID_VAR),
            self.d1.database_id,
            "d1.database_id",
            D1_DATABASE_ID_VAR,
        )?;
        let r2_bucket_name = pick(
            env(R2_BUCKET_NAME_VAR),
            self.r2.bucket_name,
            "r2.bucket_name",
            R2_BUCKET_NAME_VAR,
        )?;
        let release_assets_dir = self.r2.release_assets_dir;
        let release_assets_prefix = self.r2.release_assets_prefix.unwrap_or_default();
        if release_assets_dir.is_none() && !release_assets_prefix.as_os_str().is_empty() {
            bail!("cloudflare.r2.release_assets_prefix requires cloudflare.r2.release_assets_dir");
        }
        if let Some(path) = &release_assets_dir {
            validate_relative_path(path, "cloudflare.r2.release_assets_dir", false)?;
        }
        validate_relative_path(
            &release_assets_prefix,
            "cloudflare.r2.release_assets_prefix",
            true,
        )?;
        // Compile the globs here so an unusable pattern is a config error at
        // parse time, not a silently-ineffective filter at stage time.
        let release_assets_exclude = self
            .r2
            .release_assets_exclude
            .unwrap_or_default()
            .into_iter()
            .map(|pattern| {
                glob::Pattern::new(&pattern).with_context(|| {
                    format!("cloudflare.r2.release_assets_exclude entry {pattern:?} is not a valid glob")
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let head_sampling_rate =
            resolve_head_sampling_rate(env(HEAD_SAMPLING_RATE_VAR), self.head_sampling_rate)?;
        let d1_queries_per_invocation =
            resolve_d1_queries_per_invocation(self.d1_queries_per_invocation)?;
        let crons = resolve_crons(self.crons)?;
        let deploy_smoke_paths = resolve_deploy_smoke_paths(self.deploy_smoke_paths)?;
        let password_hasher =
            resolve_password_hasher(self.password_hasher.unwrap_or_default(), &worker_name)?;
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
                release_assets_dir,
                release_assets_prefix,
                release_assets_exclude,
            },
            wrangler_overrides_path: self.wrangler_overrides_path,
            head_sampling_rate,
            d1_queries_per_invocation,
            crons,
            deploy_smoke_paths,
            password_hasher,
        })
    }
}

fn validate_relative_path(path: &Path, key: &str, allow_empty: bool) -> Result<()> {
    use std::path::Component;

    if !allow_empty && path.as_os_str().is_empty() {
        bail!("{key} must not be empty");
    }
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("{key} must be a clean repository-relative path");
    }
    Ok(())
}

/// Resolve the Workers Observability head sampling rate: env > toml >
/// [`DEFAULT_HEAD_SAMPLING_RATE`] (unlike the required identifiers `pick()`
/// handles, an unset sampling rate is not an error — 100% is a reasonable
/// default, just no longer a hardcoded one).
fn resolve_head_sampling_rate(env_val: Option<String>, toml_val: Option<f64>) -> Result<f64> {
    let rate = match env_val {
        Some(s) => s.trim().parse::<f64>().map_err(|_| {
            anyhow!("{HEAD_SAMPLING_RATE_VAR}={s:?} is not a valid number (expected 0.0-1.0)")
        })?,
        None => toml_val.unwrap_or(DEFAULT_HEAD_SAMPLING_RATE),
    };
    if !(0.0..=1.0).contains(&rate) {
        bail!(
            "cloudflare.head_sampling_rate = {rate} is out of range — \
             must be between 0.0 and 1.0"
        );
    }
    Ok(rate)
}

/// Resolve `[cloudflare].d1_queries_per_invocation`: the stated limit, or
/// the default 1000 when unset. Checked here with the parser the Worker applies
/// to the var, so a value out of range — above D1's documented maximum of
/// 1000 (<https://developers.cloudflare.com/d1/platform/limits/>), or too
/// small to leave room past the audit-row reservation — fails the build
/// rather than every request of the deployed Worker.
fn resolve_d1_queries_per_invocation(toml_val: Option<u64>) -> Result<u64> {
    use impresspress_core::config_vars::{
        parse_d1_queries_per_invocation, D1_QUERIES_PER_INVOCATION_DEFAULT,
    };
    let Some(limit) = toml_val else {
        return Ok(D1_QUERIES_PER_INVOCATION_DEFAULT);
    };
    parse_d1_queries_per_invocation(&limit.to_string())
        .map_err(|e| anyhow!("cloudflare.d1_queries_per_invocation = {limit}: {e}"))
}

/// Resolve `[cloudflare].crons`: an explicit list (empty included, which
/// disables the schedule) or [`wrangler::DEFAULT_CRONS`], which is empty — the
/// scheduled sweep is opt-in, see that constant.
///
/// Each entry is checked for **the five whitespace-separated fields**
/// Cloudflare's cron parser requires, and for nothing else. A four-field
/// expression, a six-field one, and `@daily` are refused here; an out-of-range
/// or otherwise malformed field (`99 3 * * *`) is not, and Cloudflare rejects
/// it when `wrangler triggers deploy` runs — after promotion, at the very end
/// of a two-stage deployment. Catching the shape errors before anything is
/// built is still worth the eleven lines; it is not a validation of the
/// expression.
fn resolve_crons(crons: Option<Vec<String>>) -> Result<Vec<String>> {
    let crons = crons.unwrap_or_else(|| {
        wrangler::DEFAULT_CRONS
            .iter()
            .map(|cron| (*cron).to_string())
            .collect()
    });
    for (index, cron) in crons.iter().enumerate() {
        let fields = cron.split_whitespace().count();
        if fields != 5 {
            bail!(
                "cloudflare.crons[{index}] must be a five-field cron expression \
                 (minute hour day-of-month month day-of-week), found {fields} \
                 field(s): {cron:?}"
            );
        }
    }
    Ok(crons)
}

fn resolve_deploy_smoke_paths(paths: Option<Vec<String>>) -> Result<Vec<String>> {
    let paths = paths.unwrap_or_else(|| vec![DEFAULT_DEPLOY_SMOKE_PATH.to_string()]);
    if paths.is_empty() {
        bail!("cloudflare.deploy_smoke_paths must contain at least one path");
    }
    for (index, path) in paths.iter().enumerate() {
        if !path.starts_with('/') || path.starts_with("//") {
            bail!(
                "cloudflare.deploy_smoke_paths[{index}] must start with exactly one '/': {path:?}"
            );
        }
        if path.contains("://") {
            bail!(
                "cloudflare.deploy_smoke_paths[{index}] must be a path, not a scheme or host: \
                 {path:?}"
            );
        }
        if path.contains('?') {
            bail!(
                "cloudflare.deploy_smoke_paths[{index}] must not contain a query string: {path:?}"
            );
        }
        if path.contains('#') {
            bail!("cloudflare.deploy_smoke_paths[{index}] must not contain a fragment: {path:?}");
        }
    }
    Ok(paths)
}

/// Resolve `[cloudflare.password_hasher]` against the resolved main Worker
/// name.
fn resolve_password_hasher(
    raw: RawPasswordHasherConfig,
    main_worker_name: &str,
) -> Result<PasswordHasherConfig> {
    use impresspress_password::protocol::{validate_shards, DEFAULT_SHARDS, WORKER_NAME_SUFFIX};
    let worker_name = raw
        .worker_name
        .unwrap_or_else(|| format!("{main_worker_name}{WORKER_NAME_SUFFIX}"));
    validate_worker_name(&worker_name).with_context(|| {
        format!(
            "cloudflare.password_hasher.worker_name {worker_name:?} (defaults to \
             worker_name + {WORKER_NAME_SUFFIX:?}; set it explicitly if that is too long)"
        )
    })?;
    if worker_name == main_worker_name {
        bail!(
            "cloudflare.password_hasher.worker_name must differ from cloudflare.worker_name: \
             the hasher is a Worker of its own"
        );
    }
    let shards = validate_shards(raw.shards.unwrap_or(DEFAULT_SHARDS))
        .map_err(|e| anyhow!("cloudflare.password_hasher.shards: {e}"))?;
    Ok(PasswordHasherConfig {
        worker_name,
        shards,
        pepper_required: raw.pepper_required.unwrap_or(false),
    })
}

/// Cloudflare's rule for a Worker script name: 1-63 characters of lowercase
/// letters, digits and dashes, not starting or ending with a dash.
fn validate_worker_name(name: &str) -> Result<()> {
    let valid_chars = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if name.is_empty()
        || name.len() > 63
        || !valid_chars
        || name.starts_with('-')
        || name.ends_with('-')
    {
        bail!(
            "a Worker name must be 1-63 lowercase letters, digits and dashes, not starting or \
             ending with a dash"
        );
    }
    Ok(())
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
