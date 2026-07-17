//! Products block migrations. Applied from the block's `Init` lifecycle via
//! [`crate::migration_helper::lifecycle_init`].
//!
//! Mirrors the auth/files migration pattern. SQL is embedded via
//! `include_str!`. Backend selection reads the
//! `WAFER_RUN_SHARED__DATABASE__BACKEND` config key
//! (`sqlite` | `postgres`). Falls back to `sqlite` when the config block
//! is not registered.
//!
//! Application is gated by [`crate::migration_helper::apply_if_blessed`]:
//! the helper handles statement splitting + the `current_hash` /
//! `blessed_hash` / `IMPRESSPRESS_RUN_MIGRATIONS` gate, and stamps a row in
//! `impresspress__admin__block_settings` once applied.

const SQL_001_SQLITE: &str = include_str!("001_products_schema.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_001_POSTGRES: &str = include_str!("001_products_schema.postgres.sql");
const SQL_002_SQLITE: &str = include_str!("002_default_templates.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_002_POSTGRES: &str = include_str!("002_default_templates.postgres.sql");
const SQL_003_SQLITE: &str = include_str!("003_stripe_events.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_003_POSTGRES: &str = include_str!("003_stripe_events.postgres.sql");
const SQL_004_SQLITE: &str = include_str!("004_strict_schema_columns.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_004_POSTGRES: &str = include_str!("004_strict_schema_columns.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
/// Order here is the apply order.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_products_schema", SQL_001_SQLITE),
    ("002_default_templates", SQL_002_SQLITE),
    ("003_stripe_events", SQL_003_SQLITE),
    ("004_strict_schema_columns", SQL_004_SQLITE),
];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`]. Empty
/// when the `postgres` feature is off — see `files::migrations`'s doc for the
/// rationale (Cloudflare/D1 never selects postgres; don't embed dead SQL).
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[
    SQL_001_POSTGRES,
    SQL_002_POSTGRES,
    SQL_003_POSTGRES,
    SQL_004_POSTGRES,
];
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[];

#[cfg(test)]
mod strict_upgrade_tests {
    //! Existing-table upgrade path for `004_strict_schema_columns` — the same
    //! guard as the auth block's `010`, for `stripe_events.updated_at`. Covers
    //! the live path (an already-created `stripe_events` table without
    //! `updated_at`) that an in-place `CREATE TABLE IF NOT EXISTS` edit could
    //! never fix. Before 004 the next Stripe webhook (`db::create`, which stamps
    //! `updated_at`) failed `no such column: updated_at` under STRICT_SCHEMA.

    use std::{collections::HashMap, sync::Arc};

    use serde_json::json;
    use wafer_block_sqlite::service::SQLiteDatabaseService;
    use wafer_core::interfaces::database::service::DatabaseService;

    use super::{SQLITE_MIGRATIONS, SQL_004_SQLITE};
    use crate::migration_helper::apply_ddl_via_service;

    fn base_migrations_sql() -> Vec<&'static str> {
        SQLITE_MIGRATIONS
            .iter()
            .filter(|(name, _)| *name != "004_strict_schema_columns")
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
    async fn strict_stripe_event_write_succeeds_after_004_alter_on_preexisting_table() {
        let db: Arc<dyn DatabaseService> =
            Arc::new(SQLiteDatabaseService::open_in_memory().unwrap());

        // 1. Pre-upgrade schema (001-003), stripe_events WITHOUT updated_at.
        apply_ddl_via_service(&db, &base_migrations_sql())
            .await
            .expect("apply base (pre-004) products migrations");
        assert!(
            !has_column(&db, "impresspress__products__stripe_events", "updated_at").await,
            "precondition: pre-004 stripe_events must lack updated_at"
        );

        // 2. A pre-existing row via the old column set (no updated_at).
        db.exec_raw(
            "INSERT INTO impresspress__products__stripe_events \
             (id, event_type, status, created_at) VALUES (?, ?, ?, ?)",
            &[
                json!("evt_old"),
                json!("checkout.session.completed"),
                json!("processed"),
                json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .expect("seed pre-existing stripe_events row");

        // 3. Apply the 004 ALTER (the fix).
        apply_ddl_via_service(&db, &[SQL_004_SQLITE])
            .await
            .expect("apply 004 ALTER migration");
        assert!(
            has_column(&db, "impresspress__products__stripe_events", "updated_at").await,
            "004 must add updated_at to the existing stripe_events table"
        );

        // 4. STRICT_SCHEMA on, then the webhook write (create stamps updated_at).
        db.set_strict_schema(true);
        let mut row = HashMap::new();
        row.insert("id".to_string(), json!("evt_new"));
        row.insert(
            "event_type".to_string(),
            json!("checkout.session.completed"),
        );
        row.insert("status".to_string(), json!("pending"));
        let rec = db
            .create("impresspress__products__stripe_events", row)
            .await
            .expect("strict-mode stripe_events create must succeed after 004");
        assert!(rec.data.contains_key("updated_at"));
    }
}
