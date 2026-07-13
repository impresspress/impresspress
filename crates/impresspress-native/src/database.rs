//! Database platform-service factories for native targets.
//!
//! - `make_database_service(db_type, db_path, db_url)` — dispatches on the
//!   `IMPRESSPRESS_DB_TYPE` value (`sqlite` | `postgres`).
//! - `make_sqlite_database_service(path)` — wraps `wafer-block-sqlite`
//!   `SQLiteDatabaseService`.
//! - `make_postgres_database_service(url)` — feature-gated on `postgres`.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use wafer_core::interfaces::database::service::DatabaseService;

/// Construct the database service selected by `IMPRESSPRESS_DB_TYPE`.
///
/// - `"sqlite"` (the default) opens a SQLite database at `db_path`.
/// - `"postgres"` connects to `db_url` (`IMPRESSPRESS_DB_URL`). Requires the
///   `postgres` cargo feature; without it (or with no `db_url`) this is a
///   hard boot error rather than a silent fallback to SQLite.
/// - any other value is a hard error.
///
/// Centralises the `db_type` → factory dispatch so the boot path can't log
/// `db = postgres` while silently running SQLite.
///
/// # Errors
///
/// Returns an error when the type is unknown, when `postgres` is requested
/// but the feature is off / `db_url` is missing, or when the underlying
/// factory fails.
pub async fn make_database_service(
    db_type: &str,
    db_path: &str,
    db_url: Option<&str>,
) -> Result<Arc<dyn DatabaseService>> {
    match db_type {
        "sqlite" => make_sqlite_database_service(db_path),
        "postgres" => make_postgres_database_service_dispatch(db_url).await,
        other => Err(anyhow!(
            "unsupported IMPRESSPRESS_DB_TYPE `{other}` (expected `sqlite` or `postgres`)"
        )),
    }
}

/// Postgres branch of [`make_database_service`], split out so the
/// feature-gate + missing-url error live in one place.
#[cfg(feature = "postgres")]
async fn make_postgres_database_service_dispatch(
    db_url: Option<&str>,
) -> Result<Arc<dyn DatabaseService>> {
    let url = db_url
        .ok_or_else(|| anyhow!("IMPRESSPRESS_DB_TYPE=postgres requires IMPRESSPRESS_DB_URL to be set"))?;
    make_postgres_database_service(url).await
}

/// Feature-off branch: postgres was requested but the binary wasn't built
/// with the `postgres` feature. Fail loudly instead of running SQLite.
#[cfg(not(feature = "postgres"))]
async fn make_postgres_database_service_dispatch(
    _db_url: Option<&str>,
) -> Result<Arc<dyn DatabaseService>> {
    Err(anyhow!(
        "IMPRESSPRESS_DB_TYPE=postgres but this binary was built without the \
         `postgres` feature; rebuild with `--features impresspress-native/postgres` \
         (or set IMPRESSPRESS_DB_TYPE=sqlite)"
    ))
}

/// Open a SQLite database at `path` and wrap it in `Arc<dyn DatabaseService>`.
///
/// # Errors
///
/// Returns an error if the underlying file cannot be opened/created or if
/// the SQLite handle fails to initialise.
pub fn make_sqlite_database_service(path: &str) -> Result<Arc<dyn DatabaseService>> {
    let svc = wafer_block_sqlite::service::SQLiteDatabaseService::open(path)
        .with_context(|| format!("open SQLite database at {path}"))?;
    Ok(Arc::new(svc))
}

/// Open a PostgreSQL connection via `url` and wrap it in
/// `Arc<dyn DatabaseService>`. Feature-gated.
///
/// Async because `PostgresDatabaseService::connect` is async.
///
/// # Errors
///
/// Returns an error if the connection cannot be established.
#[cfg(feature = "postgres")]
pub async fn make_postgres_database_service(url: &str) -> Result<Arc<dyn DatabaseService>> {
    let svc = wafer_block_postgres::service::PostgresDatabaseService::connect(url)
        .await
        .with_context(|| format!("connect to Postgres at {url}"))?;
    Ok(Arc::new(svc))
}
