//! Database platform-service factories for native targets.
//!
//! - `make_database_service(db_type, db_path, db_url)` — dispatches on the
//!   `IMPRESSPRESS_DB_TYPE` value (`sqlite` | `postgres`).
//! - `make_sqlite_database_service(path)` — wraps `wafer-block-sqlite`
//!   `SQLiteDatabaseService`.
//! - `make_postgres_database_service(url)` — feature-gated on `postgres`.
//!
//! Both services come from wafer-run and classify a write that duplicates a
//! primary or unique key as `DatabaseError::AlreadyExists` from the driver's
//! own code (`SQLITE_CONSTRAINT_UNIQUE`/`_PRIMARYKEY`, SQLSTATE `23505`). That
//! is part of the `DatabaseService` contract: `impresspress_core::blocks::crud`
//! answers `AlreadyExists` as a 409 and never re-reads a key to find out what
//! a refused write meant. wafer-run's `run_conformance` pins it for both;
//! `a_duplicate_insert_is_already_exists` below pins it for the SQLite service
//! as this crate opens it.

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
    let url = db_url.ok_or_else(|| {
        anyhow!("IMPRESSPRESS_DB_TYPE=postgres requires IMPRESSPRESS_DB_URL to be set")
    })?;
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
/// Returns an error if the connection cannot be established. The error names
/// the target by [`postgres_target`] only — never the URL itself, which
/// carries the password (`IMPRESSPRESS_DB_URL` reaches the boot log verbatim
/// otherwise).
#[cfg(feature = "postgres")]
pub async fn make_postgres_database_service(url: &str) -> Result<Arc<dyn DatabaseService>> {
    let svc = wafer_block_postgres::service::PostgresDatabaseService::connect(url)
        .await
        .with_context(|| format!("connect to Postgres at {}", postgres_target(url)))?;
    Ok(Arc::new(svc))
}

/// Describe a Postgres connection URL as `host[:port]/database` for error
/// messages, dropping the user info and the query string (either can carry
/// the password: `postgres://user:pw@host/db`, `...?password=pw`).
///
/// Anything this cannot split with certainty is described as an unparseable
/// URL rather than echoed: a password holding an unescaped `/`, `?`, `#` or
/// `@` moves the delimiters, so an `@` anywhere past the authority means the
/// user info may not end where the parse thinks it does. An authority with a
/// `:` but no `@` is refused too: `user:secret` (the `@host` forgotten) and
/// `host:port` cannot be told apart when the secret is numeric.
pub(crate) fn postgres_target(url: &str) -> String {
    const UNPARSEABLE: &str = "<unparseable URL, not shown>";
    let Some((_scheme, rest)) = url.split_once("://") else {
        return UNPARSEABLE.to_string();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    if tail.contains('@') {
        return UNPARSEABLE.to_string();
    }
    let host = match authority.rsplit_once('@') {
        Some((_, host)) => host,
        None if authority.contains(':') => return UNPARSEABLE.to_string(),
        None => authority,
    };
    let database = tail
        .strip_prefix('/')
        .map_or("", |p| p.split(['?', '#']).next().unwrap_or(""));
    if host.is_empty() {
        return UNPARSEABLE.to_string();
    }
    format!("{host}/{database}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wafer_core::interfaces::database::service::DatabaseError;

    use super::{make_sqlite_database_service, postgres_target};

    /// **The `DatabaseService` contract on the SQLite service this crate
    /// opens.** A create that repeats a primary key, or a `UNIQUE` column, is
    /// `AlreadyExists` — the 409 `crud` answers — and not `Internal`, the
    /// 500 a fault is.
    #[tokio::test]
    async fn a_duplicate_insert_is_already_exists() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("dup.db");
        let svc =
            make_sqlite_database_service(path.to_str().expect("utf-8 path")).expect("open sqlite");
        svc.exec_raw(
            "CREATE TABLE dup_t (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE)",
            &[],
        )
        .await
        .expect("create table");
        let row = |id: &str, name: &str| {
            HashMap::from([
                ("id".to_string(), serde_json::json!(id)),
                ("name".to_string(), serde_json::json!(name)),
            ])
        };

        svc.create("dup_t", row("a", "first"))
            .await
            .expect("the first row lands");
        for (what, taken) in [
            ("primary key", row("a", "second")),
            ("unique column", row("b", "first")),
        ] {
            let err = svc.create("dup_t", taken).await.expect_err(what);
            assert!(
                matches!(err, DatabaseError::AlreadyExists(_)),
                "{what}: {err:?}"
            );
        }
    }

    #[test]
    fn postgres_target_drops_user_info_and_query() {
        assert_eq!(
            postgres_target("postgres://app:hunter2@db.internal:5432/prod?sslmode=require"),
            "db.internal:5432/prod"
        );
        assert_eq!(
            postgres_target("postgresql://db.internal/prod?password=hunter2"),
            "db.internal/prod"
        );
        assert_eq!(postgres_target("postgres://app@[::1]:5432"), "[::1]:5432/");
        // No user info: a bare host is shown; a `host:port` is refused, the
        // price of never echoing `user:secret` with its `@host` forgotten.
        assert_eq!(
            postgres_target("postgres://db.internal/prod"),
            "db.internal/prod"
        );
        assert_eq!(
            postgres_target("postgres://db.internal:5432/prod"),
            "<unparseable URL, not shown>"
        );
    }

    #[test]
    fn postgres_target_never_echoes_a_url_it_cannot_split() {
        for url in [
            "postgres://app:hun/ter2@db/prod",
            "postgres://app:hun?ter2@db/prod",
            "postgres://app:hun#ter2@db/prod",
            "postgres://app:hun@ter2@db/prod",
            "host=db password=hunter2",
            "postgres://",
            "postgres://app:hunter2",
            "postgres://app:hunter2/prod",
        ] {
            let shown = postgres_target(url);
            assert!(
                !shown.contains("hunter2") && !shown.contains("hun"),
                "{url} -> {shown}"
            );
        }
    }

    /// The real factory, driven to a refused connection: the error chain the
    /// boot path prints (`{e:#}`) must not carry the password.
    #[cfg(feature = "postgres")]
    #[tokio::test]
    async fn connect_error_does_not_leak_the_password() {
        let Err(e) =
            super::make_postgres_database_service("postgres://u:hunter2@127.0.0.1:1/db").await
        else {
            panic!("nothing listens on port 1; the connect must fail");
        };
        let msg = format!("{e:#}");
        assert!(!msg.contains("hunter2"), "{msg}");
        assert!(msg.contains("127.0.0.1:1/db"), "{msg}");
    }
}
