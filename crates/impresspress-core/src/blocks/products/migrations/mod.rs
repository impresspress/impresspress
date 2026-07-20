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
const SQL_005_SQLITE: &str = include_str!("005_commerce_v2.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_005_POSTGRES: &str = include_str!("005_commerce_v2.postgres.sql");
const SQL_006_SQLITE: &str = include_str!("006_payment_link_snapshots.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_006_POSTGRES: &str = include_str!("006_payment_link_snapshots.postgres.sql");
const SQL_007_SQLITE: &str = include_str!("007_provider_workflows.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_007_POSTGRES: &str = include_str!("007_provider_workflows.postgres.sql");
const SQL_008_SQLITE: &str = include_str!("008_refund_ledger.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_008_POSTGRES: &str = include_str!("008_refund_ledger.postgres.sql");
const SQL_009_SQLITE: &str = include_str!("009_commerce_subscription_state.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_009_POSTGRES: &str = include_str!("009_commerce_subscription_state.postgres.sql");
const SQL_010_SQLITE: &str = include_str!("010_guest_receipts.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_010_POSTGRES: &str = include_str!("010_guest_receipts.postgres.sql");
const SQL_011_SQLITE: &str = include_str!("011_webhook_leases.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_011_POSTGRES: &str = include_str!("011_webhook_leases.postgres.sql");
const SQL_012_SQLITE: &str = include_str!("012_payment_link_mode.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_012_POSTGRES: &str = include_str!("012_payment_link_mode.postgres.sql");
const SQL_013_SQLITE: &str = include_str!("013_order_shipping.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_013_POSTGRES: &str = include_str!("013_order_shipping.postgres.sql");
const SQL_014_SQLITE: &str = include_str!("014_subscription_event_order.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_014_POSTGRES: &str = include_str!("014_subscription_event_order.postgres.sql");
const SQL_015_SQLITE: &str = include_str!("015_dispute_ledger.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_015_POSTGRES: &str = include_str!("015_dispute_ledger.postgres.sql");
const SQL_016_SQLITE: &str = include_str!("016_payment_intent_state.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_016_POSTGRES: &str = include_str!("016_payment_intent_state.postgres.sql");
const SQL_017_SQLITE: &str = include_str!("017_refund_connect_event_order.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_017_POSTGRES: &str = include_str!("017_refund_connect_event_order.postgres.sql");
const SQL_018_SQLITE: &str = include_str!("018_provider_operation_leases.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_018_POSTGRES: &str = include_str!("018_provider_operation_leases.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
/// Order here is the apply order.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_products_schema", SQL_001_SQLITE),
    ("002_default_templates", SQL_002_SQLITE),
    ("003_stripe_events", SQL_003_SQLITE),
    ("004_strict_schema_columns", SQL_004_SQLITE),
    ("005_commerce_v2", SQL_005_SQLITE),
    ("006_payment_link_snapshots", SQL_006_SQLITE),
    ("007_provider_workflows", SQL_007_SQLITE),
    ("008_refund_ledger", SQL_008_SQLITE),
    ("009_commerce_subscription_state", SQL_009_SQLITE),
    ("010_guest_receipts", SQL_010_SQLITE),
    ("011_webhook_leases", SQL_011_SQLITE),
    ("012_payment_link_mode", SQL_012_SQLITE),
    ("013_order_shipping", SQL_013_SQLITE),
    ("014_subscription_event_order", SQL_014_SQLITE),
    ("015_dispute_ledger", SQL_015_SQLITE),
    ("016_payment_intent_state", SQL_016_SQLITE),
    ("017_refund_connect_event_order", SQL_017_SQLITE),
    ("018_provider_operation_leases", SQL_018_SQLITE),
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
    SQL_005_POSTGRES,
    SQL_006_POSTGRES,
    SQL_007_POSTGRES,
    SQL_008_POSTGRES,
    SQL_009_POSTGRES,
    SQL_010_POSTGRES,
    SQL_011_POSTGRES,
    SQL_012_POSTGRES,
    SQL_013_POSTGRES,
    SQL_014_POSTGRES,
    SQL_015_POSTGRES,
    SQL_016_POSTGRES,
    SQL_017_POSTGRES,
    SQL_018_POSTGRES,
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

    use super::{
        SQLITE_MIGRATIONS, SQL_004_SQLITE, SQL_005_POSTGRES, SQL_005_SQLITE, SQL_006_POSTGRES,
        SQL_006_SQLITE, SQL_007_POSTGRES, SQL_007_SQLITE, SQL_008_POSTGRES, SQL_008_SQLITE,
        SQL_009_POSTGRES, SQL_009_SQLITE, SQL_010_POSTGRES, SQL_010_SQLITE, SQL_011_POSTGRES,
        SQL_011_SQLITE, SQL_012_POSTGRES, SQL_012_SQLITE, SQL_013_POSTGRES, SQL_013_SQLITE,
        SQL_014_POSTGRES, SQL_014_SQLITE, SQL_015_POSTGRES, SQL_015_SQLITE, SQL_016_POSTGRES,
        SQL_016_SQLITE, SQL_017_POSTGRES, SQL_017_SQLITE, SQL_018_POSTGRES, SQL_018_SQLITE,
    };
    use crate::migration_helper::apply_ddl_via_service;

    fn pre_004_migrations_sql() -> Vec<&'static str> {
        SQLITE_MIGRATIONS
            .iter()
            .take_while(|(name, _)| *name != "004_strict_schema_columns")
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
        apply_ddl_via_service(&db, &pre_004_migrations_sql())
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

    #[tokio::test]
    async fn commerce_v2_creates_owned_tables_templates_and_strict_offer_shape() {
        let db: Arc<dyn DatabaseService> =
            Arc::new(SQLiteDatabaseService::open_in_memory().unwrap());
        let all_sql: Vec<&str> = SQLITE_MIGRATIONS.iter().map(|(_, sql)| *sql).collect();
        apply_ddl_via_service(&db, &all_sql)
            .await
            .expect("apply all products migrations");

        for table in [
            "impresspress__products__product_versions",
            "impresspress__products__offers",
            "impresspress__products__offer_components",
            "impresspress__products__checkout_presets",
            "impresspress__products__payment_links",
            "impresspress__products__seller_accounts",
            "impresspress__products__subscription_items",
            "impresspress__products__entitlements",
            "impresspress__products__provider_operations",
        ] {
            let rows = db
                .query_raw(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
                    &[json!(table)],
                )
                .await
                .unwrap_or_else(|error| panic!("querying {table}: {error}"));
            assert_eq!(rows.len(), 1, "{table} must be created by migration 005");
        }

        let templates = db
            .query_raw(
                "SELECT id FROM impresspress__products__product_templates WHERE id IN ('simple_product', 'simple_subscription', 'configurable_product', 'configurable_subscription')",
                &[],
            )
            .await
            .expect("query system templates");
        assert_eq!(templates.len(), 4);

        db.set_strict_schema(true);
        let mut offer = HashMap::new();
        offer.insert("id".to_string(), json!("offer_test"));
        offer.insert("product_id".to_string(), json!("product_test"));
        offer.insert("name".to_string(), json!("Monthly"));
        offer.insert("mode".to_string(), json!("subscription"));
        offer.insert("unit_amount_minor".to_string(), json!(2500));
        let record = db
            .create("impresspress__products__offers", offer)
            .await
            .expect("strict-schema offer insert");
        assert_eq!(
            record
                .data
                .get("unit_amount_minor")
                .and_then(|value| value.as_i64()),
            Some(2500)
        );
    }

    #[test]
    fn commerce_v2_postgres_mirrors_owned_tables_columns_and_indexes() {
        for fragment in [
            "impresspress__products__product_versions",
            "impresspress__products__offers",
            "impresspress__products__offer_components",
            "impresspress__products__checkout_presets",
            "impresspress__products__payment_links",
            "impresspress__products__seller_accounts",
            "impresspress__products__subscription_items",
            "impresspress__products__entitlements",
            "impresspress__products__provider_operations",
            "owner_kind TEXT NOT NULL",
            "unit_amount_minor BIGINT NOT NULL",
            "processing_owner TEXT NOT NULL",
            "products_stripe_product_idx",
            "provider_operations_idempotency_uniq",
        ] {
            assert!(
                SQL_005_POSTGRES.contains(fragment),
                "PostgreSQL commerce migration is missing {fragment}"
            );
        }

        assert_eq!(
            SQL_005_SQLITE.matches("CREATE TABLE IF NOT EXISTS").count(),
            SQL_005_POSTGRES
                .matches("CREATE TABLE IF NOT EXISTS")
                .count(),
            "SQLite and PostgreSQL must create the same number of commerce-owned tables"
        );
    }

    #[test]
    fn payment_link_snapshot_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "pricing_snapshot TEXT NOT NULL",
            "fee_basis_points INTEGER NOT NULL",
        ] {
            assert!(
                SQL_006_SQLITE.contains(fragment),
                "SQLite Payment Link snapshot migration is missing {fragment}"
            );
            assert!(
                SQL_006_POSTGRES.contains(fragment),
                "PostgreSQL Payment Link snapshot migration is missing {fragment}"
            );
        }
    }

    #[test]
    fn provider_workflow_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "livemode INTEGER NOT NULL",
            "country TEXT NOT NULL",
            "default_currency TEXT NOT NULL",
            "dashboard_type TEXT NOT NULL",
            "requirements_disabled_reason TEXT NOT NULL",
            "sync_error TEXT NOT NULL",
        ] {
            assert!(
                SQL_007_SQLITE.contains(fragment),
                "SQLite provider workflow migration is missing {fragment}"
            );
            assert!(
                SQL_007_POSTGRES.contains(fragment),
                "PostgreSQL provider workflow migration is missing {fragment}"
            );
        }
    }

    #[test]
    fn refund_ledger_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "impresspress__products__refunds",
            "provider_refund_id",
            "amount_minor",
            "target_refunded_total_minor",
            "refunds_idempotency_uniq",
            "refunds_active_purchase_uniq",
            "status IN ('pending', 'provider_succeeded')",
        ] {
            assert!(
                SQL_008_SQLITE.contains(fragment),
                "SQLite refund ledger migration is missing {fragment}"
            );
            assert!(
                SQL_008_POSTGRES.contains(fragment),
                "PostgreSQL refund ledger migration is missing {fragment}"
            );
        }
    }

    #[test]
    fn refund_connect_event_order_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "impresspress__products__refunds",
            "impresspress__products__seller_accounts",
            "stripe_event_created",
        ] {
            assert!(
                SQL_017_SQLITE.contains(fragment),
                "SQLite refund/Connect ordering migration is missing {fragment}"
            );
            assert!(
                SQL_017_POSTGRES.contains(fragment),
                "PostgreSQL refund/Connect ordering migration is missing {fragment}"
            );
        }
        assert_eq!(SQL_017_SQLITE.matches("ADD COLUMN").count(), 2);
        assert_eq!(SQL_017_POSTGRES.matches("ADD COLUMN").count(), 2);
    }

    #[test]
    fn provider_operation_lease_migration_matches_sqlite_and_postgres() {
        for fragment in ["processing_owner", "processing_started_at", "terminal_at"] {
            assert!(SQL_018_SQLITE.contains(fragment));
            assert!(SQL_018_POSTGRES.contains(fragment));
        }
        assert_eq!(SQL_018_SQLITE.matches("ADD COLUMN").count(), 3);
        assert_eq!(SQL_018_POSTGRES.matches("ADD COLUMN").count(), 3);
    }

    #[test]
    fn commerce_subscription_state_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "subscription_status TEXT NOT NULL",
            "subscription_current_period_end TEXT",
            "subscription_cancel_at_period_end INTEGER NOT NULL",
            "subscription_canceled_at TEXT",
            "subscription_last_synced_at TEXT",
            "purchases_subscription_status_idx",
        ] {
            assert!(
                SQL_009_SQLITE.contains(fragment),
                "SQLite commerce subscription migration is missing {fragment}"
            );
            assert!(
                SQL_009_POSTGRES.contains(fragment),
                "PostgreSQL commerce subscription migration is missing {fragment}"
            );
        }
    }

    #[test]
    fn subscription_event_order_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "subscription_event_created",
            "stripe_event_created",
            "NOT NULL DEFAULT 0",
        ] {
            assert!(
                SQL_014_SQLITE.contains(fragment),
                "SQLite subscription event-order migration is missing {fragment}"
            );
            assert!(
                SQL_014_POSTGRES.contains(fragment),
                "PostgreSQL subscription event-order migration is missing {fragment}"
            );
        }
    }

    #[test]
    fn dispute_ledger_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "impresspress__products__disputes",
            "provider_dispute_id",
            "payment_intent_id",
            "event_created",
            "disputes_provider_uniq",
            "disputes_status_idx",
        ] {
            assert!(
                SQL_015_SQLITE.contains(fragment),
                "SQLite dispute ledger migration is missing {fragment}"
            );
            assert!(
                SQL_015_POSTGRES.contains(fragment),
                "PostgreSQL dispute ledger migration is missing {fragment}"
            );
        }
    }

    #[test]
    fn payment_intent_state_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "provider_payment_status TEXT NOT NULL",
            "provider_payment_error_code TEXT NOT NULL",
            "provider_payment_error_message TEXT NOT NULL",
            "payment_intent_event_created",
            "NOT NULL DEFAULT 0",
        ] {
            assert!(
                SQL_016_SQLITE.contains(fragment),
                "SQLite PaymentIntent state migration is missing {fragment}"
            );
            assert!(
                SQL_016_POSTGRES.contains(fragment),
                "PostgreSQL PaymentIntent state migration is missing {fragment}"
            );
        }
    }

    #[test]
    fn guest_receipt_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "receipt_token_hash TEXT NOT NULL",
            "receipt_token_expires_at TEXT",
        ] {
            assert!(
                SQL_010_SQLITE.contains(fragment),
                "SQLite guest receipt migration is missing {fragment}"
            );
            assert!(
                SQL_010_POSTGRES.contains(fragment),
                "PostgreSQL guest receipt migration is missing {fragment}"
            );
        }
    }

    #[test]
    fn webhook_lease_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "payload_sha256 TEXT NOT NULL",
            "payload_base64 TEXT NOT NULL",
            "terminal_at TEXT",
        ] {
            assert!(
                SQL_011_SQLITE.contains(fragment),
                "SQLite webhook lease migration is missing {fragment}"
            );
            assert!(
                SQL_011_POSTGRES.contains(fragment),
                "PostgreSQL webhook lease migration is missing {fragment}"
            );
        }
    }

    #[test]
    fn payment_link_mode_migration_matches_sqlite_and_postgres() {
        let fragment = "livemode INTEGER NOT NULL";
        assert!(SQL_012_SQLITE.contains(fragment));
        assert!(SQL_012_POSTGRES.contains(fragment));
    }

    #[test]
    fn order_shipping_migration_matches_sqlite_and_postgres() {
        assert!(SQL_013_SQLITE.contains("shipping_cents INTEGER NOT NULL"));
        assert!(SQL_013_POSTGRES.contains("shipping_cents BIGINT NOT NULL"));
    }
}
