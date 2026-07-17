//! Auth block migrations. Applied from the framework auth service's
//! `AuthService::init` (the framework `wafer-run/auth` block's
//! `lifecycle(Init)` delegates to it) via
//! [`crate::migration_helper::apply_migrations`].
//!
//! Hash-gated apply — runs only when the SQL hash differs from the recorded
//! `current_hash` in `impresspress__admin__block_settings`. Concatenated SQL of
//! all migration scripts is hashed and tracked.

const SQL_001_SQLITE: &str = include_str!("001_auth_schema.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_001_POSTGRES: &str = include_str!("001_auth_schema.postgres.sql");
const SQL_002_SQLITE: &str = include_str!("002_reserved_orgs.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_002_POSTGRES: &str = include_str!("002_reserved_orgs.postgres.sql");
const SQL_003_SQLITE: &str = include_str!("003_oauth_pkce_states.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_003_POSTGRES: &str = include_str!("003_oauth_pkce_states.postgres.sql");
const SQL_004_SQLITE: &str = include_str!("004_refresh_tokens.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_004_POSTGRES: &str = include_str!("004_refresh_tokens.postgres.sql");
const SQL_005_SQLITE: &str = include_str!("005_jwt_blocklist.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_005_POSTGRES: &str = include_str!("005_jwt_blocklist.postgres.sql");
const SQL_006_SQLITE: &str = include_str!("006_user_extended_fields.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_006_POSTGRES: &str = include_str!("006_user_extended_fields.postgres.sql");
const SQL_007_SQLITE: &str = include_str!("007_api_keys.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_007_POSTGRES: &str = include_str!("007_api_keys.postgres.sql");
const SQL_008_SQLITE: &str = include_str!("008_rate_limits.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_008_POSTGRES: &str = include_str!("008_rate_limits.postgres.sql");
const SQL_009_SQLITE: &str = include_str!("009_auth_version.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_009_POSTGRES: &str = include_str!("009_auth_version.postgres.sql");
const SQL_010_SQLITE: &str = include_str!("010_strict_schema_columns.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_010_POSTGRES: &str = include_str!("010_strict_schema_columns.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle(Init)` apply path (auth's `init`).
/// Order here is the apply order.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_auth_schema", SQL_001_SQLITE),
    ("002_reserved_orgs", SQL_002_SQLITE),
    ("003_oauth_pkce_states", SQL_003_SQLITE),
    ("004_refresh_tokens", SQL_004_SQLITE),
    ("005_jwt_blocklist", SQL_005_SQLITE),
    ("006_user_extended_fields", SQL_006_SQLITE),
    ("007_api_keys", SQL_007_SQLITE),
    ("008_rate_limits", SQL_008_SQLITE),
    ("009_auth_version", SQL_009_SQLITE),
    ("010_strict_schema_columns", SQL_010_SQLITE),
];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`] one
/// for one. Selected at runtime by `apply_migrations`. Empty when the
/// `postgres` feature is off — see `files::migrations`'s doc for the
/// rationale (Cloudflare/D1 never selects postgres; don't embed dead SQL).
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[
    SQL_001_POSTGRES,
    SQL_002_POSTGRES,
    SQL_003_POSTGRES,
    SQL_004_POSTGRES,
    SQL_005_POSTGRES,
    SQL_006_POSTGRES,
    SQL_007_POSTGRES,
    SQL_008_POSTGRES,
    SQL_009_POSTGRES,
    SQL_010_POSTGRES,
];
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[];

/// Apply the auth schema through the shared migration-state gate.
///
/// Production no longer calls this: the framework auth service applies these
/// migrations inside [`AuthService::init`](super::service) (via
/// `apply_migrations` directly, because it needs an `AuthError` return).
/// This thin forwarder exists for the `tests/auth/*` integration suite, which
/// applies the auth schema against an in-memory fixture before exercising the
/// repo layer — test-fixture setup is an explicit exception to the
/// no-raw-migration-runner rule (CLAUDE.md).
pub async fn apply(ctx: &dyn wafer_run::context::Context) -> Result<(), String> {
    let sqlite: Vec<&str> = SQLITE_MIGRATIONS.iter().map(|(_, sql)| *sql).collect();
    crate::migration_helper::apply_migrations(ctx, "wafer-run/auth", &sqlite, POSTGRES_MIGRATIONS)
        .await
}

#[cfg(test)]
mod strict_upgrade_tests {
    //! Existing-table upgrade path for the `010_strict_schema_columns` ALTER
    //! migration.
    //!
    //! Every `with_auth()` fixture already covers the fresh-install path (010's
    //! ALTERs run right after the base CREATEs). This test covers the path a
    //! LIVE database actually takes when this change deploys: auth tables that
    //! ALREADY exist WITHOUT the `id`/`created_at`/`updated_at` columns
    //! `db::create` writes (they previously existed only because the runtime's
    //! lazy column-add materialised them), then the 010 ALTER lands, then
    //! STRICT_SCHEMA turns lazy column-add OFF. An in-place `CREATE TABLE IF NOT
    //! EXISTS` edit is a no-op on an existing table, so without 010's ALTER the
    //! first strict-mode write would fail `no such column` — the regression this
    //! guards.

    use std::{collections::HashMap, sync::Arc};

    use serde_json::json;
    use wafer_block_sqlite::service::SQLiteDatabaseService;
    use wafer_core::interfaces::database::service::DatabaseService;

    use super::{SQLITE_MIGRATIONS, SQL_010_SQLITE};
    use crate::migration_helper::apply_ddl_via_service;

    /// SQL of every auth migration BEFORE 010 — the pre-upgrade on-disk schema.
    fn base_migrations_sql() -> Vec<&'static str> {
        SQLITE_MIGRATIONS
            .iter()
            .filter(|(name, _)| *name != "010_strict_schema_columns")
            .map(|(_, sql)| *sql)
            .collect()
    }

    async fn has_column(db: &Arc<dyn DatabaseService>, table: &str, column: &str) -> bool {
        db.query_raw(&format!("PRAGMA table_info({table})"), &[])
            .await
            .unwrap()
            .iter()
            .any(|r| r.data.get("name").and_then(|v| v.as_str()) == Some(column))
    }

    #[tokio::test]
    async fn strict_writes_succeed_after_010_alter_on_preexisting_tables() {
        let db: Arc<dyn DatabaseService> =
            Arc::new(SQLiteDatabaseService::open_in_memory().unwrap());

        // 1. Pre-upgrade schema: base auth tables WITHOUT the 010 columns.
        apply_ddl_via_service(&db, &base_migrations_sql())
            .await
            .expect("apply base (pre-010) migrations");

        // Precondition: the drift really exists — updated_at is absent on the
        // old local_credentials / sessions tables.
        assert!(
            !has_column(&db, "wafer_run__auth__local_credentials", "updated_at").await,
            "precondition: pre-010 local_credentials must lack updated_at"
        );
        assert!(
            !has_column(&db, "wafer_run__auth__sessions", "id").await,
            "precondition: pre-010 sessions must lack id"
        );

        // 2. A PRE-EXISTING row written with the old column set (no updated_at),
        //    mirroring rows a live DB already holds.
        db.exec_raw(
            "INSERT INTO wafer_run__auth__users \
             (id, email, display_name, role, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
            &[
                json!("u1"),
                json!("u1@example.com"),
                json!("U1"),
                json!("user"),
                json!("2026-01-01T00:00:00Z"),
                json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .expect("seed pre-existing user");
        db.exec_raw(
            "INSERT INTO wafer_run__auth__local_credentials \
             (id, user_id, password_hash, must_reset, created_at) \
             VALUES (?, ?, ?, ?, ?)",
            &[
                json!("lc1"),
                json!("u1"),
                json!("hash"),
                json!(0),
                json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .expect("seed pre-existing credential (old schema, no updated_at)");

        // 3. Apply the 010 ALTER migration (the fix) against the existing tables.
        apply_ddl_via_service(&db, &[SQL_010_SQLITE])
            .await
            .expect("apply 010 ALTER migration");

        // The columns are now materialised on the pre-existing tables.
        assert!(
            has_column(&db, "wafer_run__auth__local_credentials", "updated_at").await,
            "010 must add updated_at to the existing local_credentials table"
        );
        assert!(
            has_column(&db, "wafer_run__auth__sessions", "id").await
                && has_column(&db, "wafer_run__auth__sessions", "updated_at").await,
            "010 must add id + updated_at to the existing sessions table"
        );

        // 4. STRICT_SCHEMA on — the shared executor no longer lazily ADD-COLUMNs.
        db.set_strict_schema(true);

        // 5. `create` stamps id/created_at/updated_at (the same shape `db::create`
        //    produces); under strict it INSERTs those columns literally. Before
        //    010 this failed `no such column: updated_at`. A second user avoids
        //    local_credentials' UNIQUE(user_id).
        db.exec_raw(
            "INSERT INTO wafer_run__auth__users \
             (id, email, display_name, role, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
            &[
                json!("u2"),
                json!("u2@example.com"),
                json!("U2"),
                json!("user"),
                json!("2026-01-02T00:00:00Z"),
                json!("2026-01-02T00:00:00Z"),
            ],
        )
        .await
        .expect("seed second user");

        let mut cred = HashMap::new();
        cred.insert("user_id".to_string(), json!("u2"));
        cred.insert("password_hash".to_string(), json!("hash2"));
        cred.insert("must_reset".to_string(), json!(0));
        let rec = db
            .create("wafer_run__auth__local_credentials", cred)
            .await
            .expect("strict-mode create on local_credentials must succeed after 010");
        assert!(
            rec.data.contains_key("updated_at"),
            "the stamped updated_at must round-trip on the created row"
        );

        // A session write exercises the synthesized `id` + `updated_at` on a
        // table whose real PK is token_hash.
        let mut sess = HashMap::new();
        sess.insert("token_hash".to_string(), json!("deadbeef"));
        sess.insert("user_id".to_string(), json!("u1"));
        sess.insert("created_at".to_string(), json!("2026-01-03T00:00:00Z"));
        sess.insert("last_used_at".to_string(), json!("2026-01-03T00:00:00Z"));
        sess.insert("expires_at".to_string(), json!("2099-01-01T00:00:00Z"));
        db.create("wafer_run__auth__sessions", sess)
            .await
            .expect("strict-mode create on sessions must succeed after 010");
    }
}
