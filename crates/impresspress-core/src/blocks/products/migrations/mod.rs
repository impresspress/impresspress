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
#[cfg(any(feature = "postgres", test))]
const SQL_001_POSTGRES: &str = include_str!("001_products_schema.postgres.sql");
const SQL_002_SQLITE: &str = include_str!("002_default_templates.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_002_POSTGRES: &str = include_str!("002_default_templates.postgres.sql");
const SQL_003_SQLITE: &str = include_str!("003_stripe_events.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_003_POSTGRES: &str = include_str!("003_stripe_events.postgres.sql");
const SQL_004_SQLITE: &str = include_str!("004_strict_schema_columns.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
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
const SQL_019_SQLITE: &str = include_str!("019_offer_draft_revision.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_019_POSTGRES: &str = include_str!("019_offer_draft_revision.postgres.sql");
const SQL_020_SQLITE: &str = include_str!("020_normalize_blank_deleted_at.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_020_POSTGRES: &str = include_str!("020_normalize_blank_deleted_at.postgres.sql");
const SQL_021_SQLITE: &str = include_str!("021_payment_link_request.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_021_POSTGRES: &str = include_str!("021_payment_link_request.postgres.sql");
const SQL_022_SQLITE: &str = include_str!("022_canonical_subscription_status.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_022_POSTGRES: &str = include_str!("022_canonical_subscription_status.postgres.sql");

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
    ("019_offer_draft_revision", SQL_019_SQLITE),
    ("020_normalize_blank_deleted_at", SQL_020_SQLITE),
    ("021_payment_link_request", SQL_021_SQLITE),
    ("022_canonical_subscription_status", SQL_022_SQLITE),
];

/// Ordered PostgreSQL migration scripts, one per entry in
/// [`SQLITE_MIGRATIONS`] and in the same order.
///
/// Declared under `test` as well as `postgres` so the parity assertion in
/// `strict_upgrade_tests` runs in an ordinary `cargo test` build. It has to
/// be *this* literal and not a test-only copy: a copy would agree with itself
/// while the shipped list stayed one migration short. [`POSTGRES_MIGRATIONS`]
/// is an alias for it, so the only list a postgres deployment can run is the
/// one the test checks.
#[cfg(any(feature = "postgres", test))]
const POSTGRES_MIGRATION_FILES: &[&str] = &[
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
    SQL_019_POSTGRES,
    SQL_020_POSTGRES,
    SQL_021_POSTGRES,
    SQL_022_POSTGRES,
];

/// The PostgreSQL scripts a deployment actually applies. Empty when the
/// `postgres` feature is off — see `files::migrations`'s doc for the
/// rationale (Cloudflare/D1 never selects postgres; don't embed dead SQL).
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = POSTGRES_MIGRATION_FILES;
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
        POSTGRES_MIGRATION_FILES, SQLITE_MIGRATIONS, SQL_001_POSTGRES, SQL_001_SQLITE,
        SQL_002_POSTGRES, SQL_002_SQLITE, SQL_003_POSTGRES, SQL_003_SQLITE, SQL_004_POSTGRES,
        SQL_004_SQLITE, SQL_005_POSTGRES, SQL_005_SQLITE, SQL_006_POSTGRES, SQL_006_SQLITE,
        SQL_007_POSTGRES, SQL_007_SQLITE, SQL_008_POSTGRES, SQL_008_SQLITE, SQL_009_POSTGRES,
        SQL_009_SQLITE, SQL_010_POSTGRES, SQL_010_SQLITE, SQL_011_POSTGRES, SQL_011_SQLITE,
        SQL_012_POSTGRES, SQL_012_SQLITE, SQL_013_POSTGRES, SQL_013_SQLITE, SQL_014_POSTGRES,
        SQL_014_SQLITE, SQL_015_POSTGRES, SQL_015_SQLITE, SQL_016_POSTGRES, SQL_016_SQLITE,
        SQL_017_POSTGRES, SQL_017_SQLITE, SQL_018_POSTGRES, SQL_018_SQLITE, SQL_019_POSTGRES,
        SQL_019_SQLITE, SQL_020_POSTGRES, SQL_020_SQLITE, SQL_021_POSTGRES, SQL_021_SQLITE,
        SQL_022_POSTGRES, SQL_022_SQLITE,
    };
    use crate::migration_helper::apply_ddl_via_service;

    fn pre_004_migrations_sql() -> Vec<&'static str> {
        SQLITE_MIGRATIONS
            .iter()
            .take_while(|(name, _)| *name != "004_strict_schema_columns")
            .map(|(_, sql)| *sql)
            .collect()
    }

    const NORMALIZE_BLANK: &str = "020_normalize_blank_deleted_at";

    fn pre_020_migrations_sql() -> Vec<&'static str> {
        SQLITE_MIGRATIONS
            .iter()
            .take_while(|(name, _)| *name != NORMALIZE_BLANK)
            .map(|(_, sql)| *sql)
            .collect()
    }

    // Sliced out of `SQLITE_MIGRATIONS` rather than read from
    // `SQL_020_SQLITE` directly, so the callers cover the wiring as well as
    // the SQL: an unwired migration yields an empty slice and trips the
    // assert here instead of silently testing nothing.
    fn from_020_migrations_sql() -> Vec<&'static str> {
        let sql: Vec<&str> = SQLITE_MIGRATIONS
            .iter()
            .skip_while(|(name, _)| *name != NORMALIZE_BLANK)
            .map(|(_, sql)| *sql)
            .collect();
        assert!(
            !sql.is_empty(),
            "020 must be wired into SQLITE_MIGRATIONS to reach a deployed database"
        );
        sql
    }

    async fn ids_matching(db: &Arc<dyn DatabaseService>, predicate: &str) -> Vec<String> {
        db.query_raw(
            &format!(
                "SELECT id FROM impresspress__products__products \
                 WHERE {predicate} ORDER BY id"
            ),
            &[],
        )
        .await
        .unwrap_or_else(|error| panic!("query `{predicate}`: {error}"))
        .iter()
        .filter_map(|row| {
            row.data
                .get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .collect()
    }

    async fn seed_products(db: &Arc<dyn DatabaseService>, rows: &[(&str, &str, Option<&str>)]) {
        for (id, slug, deleted_at) in rows {
            let mut row = HashMap::new();
            row.insert("id".to_string(), json!(id));
            row.insert("name".to_string(), json!(id));
            row.insert("slug".to_string(), json!(slug));
            if let Some(stamp) = deleted_at {
                row.insert("deleted_at".to_string(), json!(stamp));
            }
            db.create("impresspress__products__products", row)
                .await
                .unwrap_or_else(|error| panic!("seed {id}: {error}"));
        }
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

    /// `deleted_at` is NULL-or-timestamp by invariant, but `''` was reachable
    /// historically: until the product handlers began refusing a body that
    /// names an `UNSETTABLE_FIELDS` column, all four create/update paths
    /// forwarded the request body verbatim. Once `is_deleted` became the exact per-record twin of
    /// `live_filter`'s `deleted_at IS NULL`, such a row reads as DELETED
    /// everywhere — out of the public catalog, the admin list and the seller
    /// cap, with no admin action and nothing logged. 020 repairs those rows.
    ///
    /// Drives the real migration harness (`apply_ddl_via_service` over
    /// `SQLITE_MIGRATIONS`), the same shape as
    /// `strict_stripe_event_write_succeeds_after_004_alter_on_preexisting_table`:
    /// pre-migration schema, a pre-existing row the new migration has to
    /// repair, then the migration.
    #[tokio::test]
    async fn blank_deleted_at_is_normalized_to_null_by_020() {
        let db: Arc<dyn DatabaseService> =
            Arc::new(SQLiteDatabaseService::open_in_memory().unwrap());

        apply_ddl_via_service(&db, &pre_020_migrations_sql())
            .await
            .expect("apply pre-020 products migrations");

        // Every value the column can hold: the historical `''` a
        // pass-through create body produced, a live NULL row, and a genuine
        // soft delete. The last two are here to prove 020 is targeted.
        //
        // The slugs are distinct and NON-EMPTY on purpose. 005's unique index
        // is partial on `slug <> ''`, so a fixture that let `slug` take its
        // `''` default would sit outside the index and never exercise the
        // repair's interaction with it. Distinct slugs take the in-index path
        // with nothing to collide against; `slug_collision_cannot_fail_020`
        // takes the same path with a collision waiting.
        seed_products(
            &db,
            &[
                ("blank", "blank-slug", Some("")),
                ("live", "live-slug", None),
                ("gone", "gone-slug", Some("2026-01-01T00:00:00Z")),
            ],
        )
        .await;

        let blanks = |db: Arc<dyn DatabaseService>| async move {
            db.query_raw(
                "SELECT id FROM impresspress__products__products WHERE deleted_at = ''",
                &[],
            )
            .await
            .expect("query blank deleted_at")
            .len()
        };
        assert_eq!(
            blanks(db.clone()).await,
            1,
            "precondition: the pre-020 schema stores an empty-string deleted_at as-is"
        );

        apply_ddl_via_service(&db, &from_020_migrations_sql())
            .await
            .expect("apply 020 normalization migration");

        assert_eq!(
            blanks(db.clone()).await,
            0,
            "020 must leave no empty-string deleted_at behind"
        );

        // Repaired to NULL — i.e. live — not merely to some other non-blank
        // value, and the row that was really deleted is untouched.
        let live = db
            .query_raw(
                "SELECT id FROM impresspress__products__products \
                 WHERE deleted_at IS NULL ORDER BY id",
                &[],
            )
            .await
            .expect("query live rows");
        let live_ids: Vec<&str> = live
            .iter()
            .filter_map(|row| row.data.get("id").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(
            live_ids,
            vec!["blank", "live"],
            "the repaired row must read as live, alongside the row that always was"
        );

        let still_deleted = db
            .query_raw(
                "SELECT id FROM impresspress__products__products \
                 WHERE deleted_at = '2026-01-01T00:00:00Z'",
                &[],
            )
            .await
            .expect("query the genuinely deleted row");
        assert_eq!(
            still_deleted.len(),
            1,
            "020 must not touch a row that carries a real deletion stamp"
        );
    }

    /// 020's own repair can re-create a slug collision, and an unguarded
    /// repair turns that into a permanent outage.
    ///
    /// 005's unique index is partial on `slug <> '' AND deleted_at IS NULL`,
    /// so a `deleted_at = ''` row sits OUTSIDE it and cannot stop a second
    /// product from claiming the same `(owner_kind, owner_id, slug)` —
    /// and `owner_kind`/`owner_id` default to `'platform'`/`''`, so every
    /// platform product shares the key. Setting the blank row to NULL pulls
    /// it back INTO the index against a slug someone else now holds.
    ///
    /// An unguarded `UPDATE … SET deleted_at = NULL` raises `UNIQUE
    /// constraint failed` there, and that is fatal rather than tolerated:
    /// `migration_helper::apply_if_blessed` forgives only a duplicate
    /// `ALTER … ADD COLUMN`, so the error propagates, `write_state` never
    /// stamps the hash, and every later boot re-runs and re-fails. On
    /// Cloudflare `builder::InitPolicy::Strict` turns a block Init failure
    /// into `Err`, and `IMPRESSPRESS_RUN_MIGRATIONS` is baked into the
    /// deployment — so every request 500s until someone hand-edits D1.
    /// Natively `InitPolicy::Tolerant` logs and continues, but SQLite
    /// rolls the whole statement back, so not one row gets repaired while
    /// `is_deleted` has already made them all invisible.
    ///
    /// Both collision shapes are covered: a blank row against a LIVE row, and
    /// two blank rows against each other (repairing both would collide them
    /// with one another). Rows with nothing to collide against must still be
    /// repaired in the same pass — the guard is targeted, not a blanket skip.
    #[tokio::test]
    async fn slug_collision_cannot_fail_020() {
        let db: Arc<dyn DatabaseService> =
            Arc::new(SQLiteDatabaseService::open_in_memory().unwrap());
        apply_ddl_via_service(&db, &pre_020_migrations_sql())
            .await
            .expect("apply pre-020 products migrations");

        // (id, slug, deleted_at). `owner_kind`/`owner_id` take their column
        // defaults, which is what every platform product carries — so the
        // slug alone decides the index key.
        seed_products(
            &db,
            &[
                // Was live under `jacket`; a pass-through update body wrote
                // `deleted_at: ""`, dropping it out of the index. A second
                // product then took the slug it had vacated.
                ("blank_jacket", "jacket", Some("")),
                ("live_jacket", "jacket", None),
                // Two blanks holding one slug: repairing both would collide
                // them with each other, so neither may be repaired.
                ("blank_hat_a", "hat", Some("")),
                ("blank_hat_b", "hat", Some("")),
                // Nothing to collide against — must still be repaired.
                ("blank_scarf", "scarf", Some("")),
                // The index is partial on `slug <> ''`, so any number of
                // empty-slug rows coexist and the repair is always safe —
                // `live_noslug` is here so a guard that forgot to exempt the
                // empty slug would wrongly skip `blank_noslug`.
                ("blank_noslug", "", Some("")),
                ("live_noslug", "", None),
                // A genuine soft delete is outside the index too, so it does
                // not block the blank row that shares its slug.
                ("gone_boots", "boots", Some("2026-01-01T00:00:00Z")),
                ("blank_boots", "boots", Some("")),
            ],
        )
        .await;

        // The seed itself is the proof that the state is reachable: the index
        // accepted `live_jacket` while `blank_jacket` already held that slug.
        assert_eq!(
            ids_matching(&db, "deleted_at = ''").await,
            [
                "blank_boots",
                "blank_hat_a",
                "blank_hat_b",
                "blank_jacket",
                "blank_noslug",
                "blank_scarf",
            ],
            "precondition: six rows carry the historical empty-string stamp"
        );

        apply_ddl_via_service(&db, &from_020_migrations_sql())
            .await
            .expect("020 must never fail the deploy over a slug collision");

        assert_eq!(
            ids_matching(&db, "deleted_at IS NULL").await,
            [
                "blank_boots",
                "blank_noslug",
                "blank_scarf",
                "live_jacket",
                "live_noslug",
            ],
            "every blank row whose slug no live row claims must be repaired"
        );
        assert_eq!(
            ids_matching(&db, "deleted_at = ''").await,
            ["blank_hat_a", "blank_hat_b", "blank_jacket"],
            "a blank row whose slug is already claimed stays exactly as it \
             was — the half-state is recoverable, a failed deploy is not"
        );
        assert_eq!(
            ids_matching(&db, "deleted_at = '2026-01-01T00:00:00Z'").await,
            ["gone_boots"],
            "020 must not touch a row that carries a real deletion stamp"
        );
    }

    /// The repair AND its collision guard have to reach both dialects. The
    /// guard is what keeps 020 from aborting a deploy (see
    /// `slug_collision_cannot_fail_020`); a PostgreSQL file that kept the bare
    /// `UPDATE` would carry the outage the SQLite file no longer has.
    #[test]
    fn normalize_blank_deleted_at_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "impresspress__products__products",
            "SET deleted_at = NULL",
            "WHERE deleted_at = ''",
            // The guard: exempt the empty slug (outside 005's partial index),
            // and otherwise repair only a row no other row can collide with.
            "slug = ''",
            "OR NOT EXISTS (",
            "FROM impresspress__products__products AS claimant",
            "claimant.id <> impresspress__products__products.id",
            "claimant.slug = impresspress__products__products.slug",
            "AND (claimant.deleted_at IS NULL OR claimant.deleted_at = '')",
        ] {
            assert!(
                SQL_020_SQLITE.contains(fragment),
                "SQLite deleted_at normalization migration is missing {fragment}"
            );
            assert!(
                SQL_020_POSTGRES.contains(fragment),
                "PostgreSQL deleted_at normalization migration is missing {fragment}"
            );
        }
    }

    const CANONICAL_STATUS: &str = "022_canonical_subscription_status";

    async fn subscription_statuses(db: &Arc<dyn DatabaseService>) -> Vec<(String, String)> {
        db.query_raw(
            "SELECT id, status FROM impresspress__products__subscriptions ORDER BY id",
            &[],
        )
        .await
        .expect("query subscription statuses")
        .iter()
        .map(|row| {
            let text = |key: &str| {
                row.data
                    .get(key)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            (text("id"), text("status"))
        })
        .collect()
    }

    /// A platform subscription stored under the British `cancelled` — the
    /// spelling `cancel_and_reset_addons` wrote before 022 — is rewritten to
    /// Stripe's `canceled`, the only spelling `SubscriptionStatus` still
    /// decodes. Every other status is left exactly as it was.
    ///
    /// The block re-applies its WHOLE migration set whenever the combined hash
    /// changes, so the second half replays every file over the migrated rows:
    /// nothing is dropped, and the rewrite has nothing left to match.
    #[tokio::test]
    async fn a_stored_cancelled_subscription_is_rewritten_to_canceled_by_022() {
        let db: Arc<dyn DatabaseService> =
            Arc::new(SQLiteDatabaseService::open_in_memory().unwrap());
        let before: Vec<&str> = SQLITE_MIGRATIONS
            .iter()
            .take_while(|(name, _)| *name != CANONICAL_STATUS)
            .map(|(_, sql)| *sql)
            .collect();
        let from: Vec<&str> = SQLITE_MIGRATIONS
            .iter()
            .skip_while(|(name, _)| *name != CANONICAL_STATUS)
            .map(|(_, sql)| *sql)
            .collect();
        assert!(
            !from.is_empty(),
            "022 must be wired into SQLITE_MIGRATIONS to reach a deployed database"
        );
        apply_ddl_via_service(&db, &before)
            .await
            .expect("apply pre-022 products migrations");

        for (id, status) in [
            ("sub_active", "active"),
            ("sub_british", "cancelled"),
            ("sub_expired", "incomplete_expired"),
            ("sub_past_due", "past_due"),
            ("sub_stripe", "canceled"),
        ] {
            let row = HashMap::from([
                ("id".to_string(), json!(id)),
                ("user_id".to_string(), json!(format!("user_{id}"))),
                ("status".to_string(), json!(status)),
            ]);
            db.create("impresspress__products__subscriptions", row)
                .await
                .unwrap_or_else(|error| panic!("seed {id}: {error}"));
        }

        let expected: Vec<(String, String)> = [
            ("sub_active", "active"),
            ("sub_british", "canceled"),
            ("sub_expired", "incomplete_expired"),
            ("sub_past_due", "past_due"),
            ("sub_stripe", "canceled"),
        ]
        .into_iter()
        .map(|(id, status)| (id.to_string(), status.to_string()))
        .collect();

        apply_ddl_via_service(&db, &from).await.expect("apply 022");
        assert_eq!(subscription_statuses(&db).await, expected);

        let all: Vec<&str> = SQLITE_MIGRATIONS.iter().map(|(_, sql)| *sql).collect();
        apply_ddl_via_service(&db, &all)
            .await
            .expect("replay the whole products migration set");
        assert_eq!(
            subscription_statuses(&db).await,
            expected,
            "a replay of every products migration must keep every row and change nothing"
        );
    }

    #[test]
    fn canonical_subscription_status_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "UPDATE impresspress__products__subscriptions",
            "SET status = 'canceled'",
            "WHERE status = 'cancelled'",
        ] {
            assert!(
                SQL_022_SQLITE.contains(fragment),
                "SQLite subscription status migration is missing {fragment}"
            );
            assert!(
                SQL_022_POSTGRES.contains(fragment),
                "PostgreSQL subscription status migration is missing {fragment}"
            );
        }
    }

    /// Every SQLite migration must have a PostgreSQL twin in the list a
    /// postgres deployment actually applies.
    ///
    /// `POSTGRES_MIGRATIONS` is `#[cfg(feature = "postgres")]` and empty
    /// otherwise, so forgetting an entry compiles clean and fails no SQLite
    /// test. CI's `products-postgres` job would not catch it either: it globs
    /// `*.postgres.sql` off disk and feeds them to `psql`, which proves the
    /// SQL parses and runs but says nothing about whether the Rust list
    /// references it. Asserting against `POSTGRES_MIGRATION_FILES` — the same
    /// literal `POSTGRES_MIGRATIONS` aliases — puts the check in every
    /// ordinary `cargo test` build instead of a postgres-only one, and holds
    /// for every migration added after 020.
    #[test]
    fn every_sqlite_migration_has_a_wired_postgres_twin() {
        assert_eq!(
            SQLITE_MIGRATIONS.len(),
            POSTGRES_MIGRATION_FILES.len(),
            "{} SQLite migrations against {} PostgreSQL: a migration was added \
             to one dialect's list and not the other",
            SQLITE_MIGRATIONS.len(),
            POSTGRES_MIGRATION_FILES.len(),
        );

        // Repeating a const keeps the two lengths equal while silently
        // dropping a migration, so distinctness is part of the parity.
        let mut seen = std::collections::HashSet::new();
        for (index, sql) in POSTGRES_MIGRATION_FILES.iter().enumerate() {
            assert!(
                seen.insert(*sql),
                "the PostgreSQL entry for {} repeats an earlier script — its \
                 own file is not wired in",
                SQLITE_MIGRATIONS[index].0,
            );
        }
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
    fn payment_link_request_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "stripe_request TEXT NOT NULL",
            "stripe_request_at TEXT NOT NULL",
        ] {
            assert!(
                SQL_021_SQLITE.contains(fragment),
                "SQLite Payment Link request migration is missing {fragment}"
            );
            assert!(
                SQL_021_POSTGRES.contains(fragment),
                "PostgreSQL Payment Link request migration is missing {fragment}"
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
    fn offer_draft_revision_migration_matches_sqlite_and_postgres() {
        for fragment in [
            "impresspress__products__offers",
            "draft_revision",
            "draft_updating",
            "NOT NULL DEFAULT 0",
        ] {
            assert!(
                SQL_019_SQLITE.contains(fragment),
                "SQLite offer draft-revision migration is missing {fragment}"
            );
            assert!(
                SQL_019_POSTGRES.contains(fragment),
                "PostgreSQL offer draft-revision migration is missing {fragment}"
            );
        }
        assert_eq!(SQL_019_SQLITE.matches("ADD COLUMN").count(), 2);
        assert_eq!(SQL_019_POSTGRES.matches("ADD COLUMN").count(), 2);
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

    /// "INTEGER (not BOOLEAN) is used for boolean-like columns to match the
    /// JSON-value round-trips used by block code" (001/005 header comment).
    /// A BOOLEAN column on one backend would round-trip `true`/`false` while
    /// every other table and the other backend round-trip `0`/`1`.
    #[test]
    fn boolean_like_columns_use_integer_on_every_backend() {
        for (name, sql) in [
            ("001 sqlite", SQL_001_SQLITE),
            ("001 postgres", SQL_001_POSTGRES),
            ("002 sqlite", SQL_002_SQLITE),
            ("002 postgres", SQL_002_POSTGRES),
            ("003 sqlite", SQL_003_SQLITE),
            ("003 postgres", SQL_003_POSTGRES),
            ("004 sqlite", SQL_004_SQLITE),
            ("004 postgres", SQL_004_POSTGRES),
            ("005 sqlite", SQL_005_SQLITE),
            ("005 postgres", SQL_005_POSTGRES),
            ("006 sqlite", SQL_006_SQLITE),
            ("006 postgres", SQL_006_POSTGRES),
            ("007 sqlite", SQL_007_SQLITE),
            ("007 postgres", SQL_007_POSTGRES),
            ("008 sqlite", SQL_008_SQLITE),
            ("008 postgres", SQL_008_POSTGRES),
            ("009 sqlite", SQL_009_SQLITE),
            ("009 postgres", SQL_009_POSTGRES),
            ("010 sqlite", SQL_010_SQLITE),
            ("010 postgres", SQL_010_POSTGRES),
            ("011 sqlite", SQL_011_SQLITE),
            ("011 postgres", SQL_011_POSTGRES),
            ("012 sqlite", SQL_012_SQLITE),
            ("012 postgres", SQL_012_POSTGRES),
            ("013 sqlite", SQL_013_SQLITE),
            ("013 postgres", SQL_013_POSTGRES),
            ("014 sqlite", SQL_014_SQLITE),
            ("014 postgres", SQL_014_POSTGRES),
            ("015 sqlite", SQL_015_SQLITE),
            ("015 postgres", SQL_015_POSTGRES),
            ("016 sqlite", SQL_016_SQLITE),
            ("016 postgres", SQL_016_POSTGRES),
            ("017 sqlite", SQL_017_SQLITE),
            ("017 postgres", SQL_017_POSTGRES),
            ("018 sqlite", SQL_018_SQLITE),
            ("018 postgres", SQL_018_POSTGRES),
            ("019 sqlite", SQL_019_SQLITE),
            ("019 postgres", SQL_019_POSTGRES),
            ("020 sqlite", SQL_020_SQLITE),
            ("020 postgres", SQL_020_POSTGRES),
        ] {
            let declares_boolean = sql.lines().any(|line| {
                let line = line.trim();
                !line.starts_with("--") && line.to_ascii_uppercase().contains("BOOLEAN")
            });
            assert!(
                !declares_boolean,
                "products migration {name} declares a BOOLEAN column; use INTEGER"
            );
        }
    }
}
