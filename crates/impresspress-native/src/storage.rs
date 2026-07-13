//! Storage platform-service factories for native targets.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use wafer_core::interfaces::storage::service::StorageService;

/// Construct the storage service selected by `IMPRESSPRESS_STORAGE_TYPE`.
///
/// - `"local"` (the default) roots a local-filesystem store at
///   `storage_root` (`IMPRESSPRESS_STORAGE_ROOT`).
/// - `"s3"` builds an S3-compatible store from the `IMPRESSPRESS_S3_*` env vars
///   (`BUCKET` required; `PREFIX`/`ENDPOINT`/`REGION` optional). Requires the
///   `s3` cargo feature; without it this is a hard boot error rather than a
///   silent fallback to local disk.
/// - any other value is a hard error.
///
/// Centralises the `storage_type` → factory dispatch so the boot path can't
/// log `storage = s3` while silently writing to local disk.
///
/// # Errors
///
/// Returns an error when the type is unknown, when `s3` is requested but the
/// feature is off / `IMPRESSPRESS_S3_BUCKET` is missing, or when the underlying
/// factory fails.
pub async fn make_storage_service(
    storage_type: &str,
    storage_root: &str,
) -> Result<Arc<dyn StorageService>> {
    match storage_type {
        "local" => make_local_storage_service(storage_root),
        "s3" => make_s3_storage_service_dispatch().await,
        other => Err(anyhow!(
            "unsupported IMPRESSPRESS_STORAGE_TYPE `{other}` (expected `local` or `s3`)"
        )),
    }
}

/// S3 branch of [`make_storage_service`], split out so the feature-gate +
/// env-var sourcing live in one place. Reads `IMPRESSPRESS_S3_BUCKET` (required),
/// `IMPRESSPRESS_S3_PREFIX`, `IMPRESSPRESS_S3_ENDPOINT`, and `IMPRESSPRESS_S3_REGION`.
#[cfg(feature = "s3")]
async fn make_s3_storage_service_dispatch() -> Result<Arc<dyn StorageService>> {
    let bucket = std::env::var("IMPRESSPRESS_S3_BUCKET").map_err(|_| {
        anyhow!("IMPRESSPRESS_STORAGE_TYPE=s3 requires IMPRESSPRESS_S3_BUCKET to be set")
    })?;
    let config = S3Config {
        bucket,
        prefix: std::env::var("IMPRESSPRESS_S3_PREFIX").unwrap_or_default(),
        endpoint: std::env::var("IMPRESSPRESS_S3_ENDPOINT").ok(),
        region: std::env::var("IMPRESSPRESS_S3_REGION").ok(),
    };
    make_s3_storage_service(config).await
}

/// Feature-off branch: s3 was requested but the binary wasn't built with the
/// `s3` feature. Fail loudly instead of writing to local disk.
#[cfg(not(feature = "s3"))]
async fn make_s3_storage_service_dispatch() -> Result<Arc<dyn StorageService>> {
    Err(anyhow!(
        "IMPRESSPRESS_STORAGE_TYPE=s3 but this binary was built without the `s3` \
         feature; rebuild with `--features impresspress-native/s3` (or set \
         IMPRESSPRESS_STORAGE_TYPE=local)"
    ))
}

/// Initialise a local-filesystem storage service rooted at `root`.
///
/// # Errors
///
/// Returns an error if the storage root cannot be created or accessed.
pub fn make_local_storage_service(root: &str) -> Result<Arc<dyn StorageService>> {
    let svc = wafer_block_local_storage::service::LocalStorageService::new(root)
        .with_context(|| format!("init local storage at {root}"))?;
    Ok(Arc::new(svc))
}

/// Configuration for an S3-compatible storage backend.
#[cfg(feature = "s3")]
pub struct S3Config {
    pub bucket: String,
    pub prefix: String,
    /// Optional custom endpoint URL (for MinIO, Tigris, Cloudflare R2, etc.).
    pub endpoint: Option<String>,
    /// AWS region; defaults to `us-east-1` when using a custom endpoint.
    pub region: Option<String>,
}

/// Construct an S3-backed storage service. Feature-gated.
///
/// Uses `S3StorageService::with_endpoint` when `config.endpoint` is set,
/// otherwise falls back to default AWS config (env vars / IAM role).
///
/// # Errors
///
/// Returns an error if the S3 client cannot be constructed (e.g. invalid
/// endpoint, missing credentials).
#[cfg(feature = "s3")]
pub async fn make_s3_storage_service(config: S3Config) -> Result<Arc<dyn StorageService>> {
    use wafer_block_s3::service::S3StorageService;

    let svc = match config.endpoint {
        Some(ref endpoint) => {
            let region = config.region.as_deref().unwrap_or("us-east-1");
            S3StorageService::with_endpoint(&config.bucket, &config.prefix, endpoint, region).await
        }
        None => S3StorageService::new(&config.bucket, &config.prefix).await,
    }
    .context("init S3 storage")?;

    Ok(Arc::new(svc))
}
