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
const SQL_011_SQLITE: &str = include_str!("011_rate_limit_retention.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_011_POSTGRES: &str = include_str!("011_rate_limit_retention.postgres.sql");
const SQL_012_SQLITE: &str = include_str!("012_sessions_family.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_012_POSTGRES: &str = include_str!("012_sessions_family.postgres.sql");
const SQL_013_SQLITE: &str = include_str!("013_email_proof.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_013_POSTGRES: &str = include_str!("013_email_proof.postgres.sql");
const SQL_014_SQLITE: &str = include_str!("014_clear_provider_access_tokens.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_014_POSTGRES: &str = include_str!("014_clear_provider_access_tokens.postgres.sql");
const SQL_015_SQLITE: &str = include_str!("015_api_key_expiry_canonical.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_015_POSTGRES: &str = include_str!("015_api_key_expiry_canonical.postgres.sql");

/// Basename of the API-key expiry repair, named once so the migration list
/// and the test that drives it cannot drift apart.
pub(crate) const API_KEY_EXPIRY_CANONICAL: &str = "015_api_key_expiry_canonical";

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
    ("011_rate_limit_retention", SQL_011_SQLITE),
    ("012_sessions_family", SQL_012_SQLITE),
    ("013_email_proof", SQL_013_SQLITE),
    ("014_clear_provider_access_tokens", SQL_014_SQLITE),
    (API_KEY_EXPIRY_CANONICAL, SQL_015_SQLITE),
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
    SQL_011_POSTGRES,
    SQL_012_POSTGRES,
    SQL_013_POSTGRES,
    SQL_014_POSTGRES,
    SQL_015_POSTGRES,
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

    /// SQL of every auth migration BEFORE 010 — the pre-upgrade on-disk
    /// schema. `take_while`, not a filter: 012 comes *after* 010 and creates
    /// the sessions table with the columns 010 adds, so excluding only 010
    /// would build a schema that already has them and the precondition below
    /// would be vacuous.
    fn base_migrations_sql() -> Vec<&'static str> {
        SQLITE_MIGRATIONS
            .iter()
            .take_while(|(name, _)| *name != "010_strict_schema_columns")
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

#[cfg(test)]
mod provider_token_clearing_tests {
    //! `014_clear_provider_access_tokens` on the path a live database takes:
    //! link rows already holding provider tokens, then the upgrade re-runs
    //! every auth migration because the SQL hash changed.

    use std::sync::Arc;

    use serde_json::json;
    use wafer_block_sqlite::service::SQLiteDatabaseService;
    use wafer_core::interfaces::database::service::DatabaseService;

    use super::SQLITE_MIGRATIONS;
    use crate::migration_helper::apply_ddl_via_service;

    fn migrations_before_014() -> Vec<&'static str> {
        SQLITE_MIGRATIONS
            .iter()
            .take_while(|(name, _)| *name != "014_clear_provider_access_tokens")
            .map(|(_, sql)| *sql)
            .collect()
    }

    fn all_migrations() -> Vec<&'static str> {
        SQLITE_MIGRATIONS.iter().map(|(_, sql)| *sql).collect()
    }

    async fn stored_tokens(db: &Arc<dyn DatabaseService>) -> Vec<String> {
        db.query_raw(
            "SELECT access_token FROM wafer_run__auth__provider_links ORDER BY id",
            &[],
        )
        .await
        .expect("read link rows")
        .iter()
        .map(|r| {
            r.data
                .get("access_token")
                .and_then(|v| v.as_str())
                .expect("access_token is a string")
                .to_string()
        })
        .collect()
    }

    #[tokio::test]
    async fn the_upgrade_clears_stored_provider_tokens_and_re_runs_cleanly() {
        let db: Arc<dyn DatabaseService> =
            Arc::new(SQLiteDatabaseService::open_in_memory().unwrap());
        let before = migrations_before_014();
        assert_eq!(
            SQLITE_MIGRATIONS[before.len()].0,
            "014_clear_provider_access_tokens",
            "precondition: 014 is in the list, and `before` stops right at it"
        );
        apply_ddl_via_service(&db, &before)
            .await
            .expect("apply the pre-014 schema");

        db.exec_raw(
            "INSERT INTO wafer_run__auth__users \
             (id, email, display_name, role, created_at, updated_at) \
             VALUES ('u1', 'u1@example.com', 'U1', 'user', \
             '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            &[],
        )
        .await
        .expect("seed user");
        for (id, provider, token) in [
            ("l1", "google", "ya29.live-google-token"),
            ("l2", "github", "gho_live_github_token"),
        ] {
            db.exec_raw(
                "INSERT INTO wafer_run__auth__provider_links \
                 (id, provider, provider_ref, user_id, provider_login, access_token, linked_at) \
                 VALUES (?, ?, ?, 'u1', 'u1', ?, '2026-01-01T00:00:00Z')",
                &[json!(id), json!(provider), json!(id), json!(token)],
            )
            .await
            .expect("seed a link row holding a token");
        }
        assert_eq!(
            stored_tokens(&db).await,
            vec!["ya29.live-google-token", "gho_live_github_token"],
            "precondition: the rows hold tokens"
        );

        // The upgrade: the hash changed, so every auth migration runs again.
        apply_ddl_via_service(&db, &all_migrations())
            .await
            .expect("the upgrade re-run succeeds");
        assert_eq!(stored_tokens(&db).await, vec!["", ""]);

        // And the next hash change re-runs it again without failing.
        apply_ddl_via_service(&db, &all_migrations())
            .await
            .expect("a second re-run succeeds");
        assert_eq!(stored_tokens(&db).await, vec!["", ""]);
    }
}

#[cfg(test)]
mod api_key_expiry_tests {
    //! What `015_api_key_expiry_canonical` does to the `expires_at` values a
    //! deployment already holds.
    //!
    //! `POST /b/auth/api/api-keys` stored the caller's string as it stood,
    //! so the column holds whatever was sent. The reader is fail-closed now,
    //! which is what makes those rows safe; this repair is what makes the
    //! column one format, and a key whose expiry names no instant visibly
    //! revoked rather than quietly inert.
    //!
    //! Driven through `apply_migrations` — the path `--run-migrations`
    //! takes — and read back through `repo::api_keys`, so what is asserted
    //! is what the block sees.

    use std::collections::HashMap;

    use serde_json::{json, Value};

    use super::{API_KEY_EXPIRY_CANONICAL, SQLITE_MIGRATIONS};
    use crate::{blocks::auth::repo::api_keys, migration_helper, test_support::TestContext};

    /// A fixture with the auth schema in place whose operator has opted into
    /// migrations, as `--run-migrations` does.
    async fn upgrading_deployment() -> TestContext {
        let mut ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        ctx.set_config(migration_helper::RUN_MIGRATIONS_KEY, "1");
        ctx.seed_auth_user("owner").await;
        ctx
    }

    /// The repair onwards, sliced out of the shipped list rather than read
    /// from `SQL_015_SQLITE` directly: an unwired migration yields an empty
    /// slice and trips this assert instead of silently testing nothing.
    fn the_repair() -> Vec<&'static str> {
        let sql: Vec<&str> = SQLITE_MIGRATIONS
            .iter()
            .skip_while(|(name, _)| *name != API_KEY_EXPIRY_CANONICAL)
            .map(|(_, sql)| *sql)
            .collect();
        assert!(
            !sql.is_empty(),
            "{API_KEY_EXPIRY_CANONICAL} must be wired into SQLITE_MIGRATIONS to reach a deployed \
             database"
        );
        sql
    }

    /// `block_name` keys the recorded-hash row, so naming a different one is
    /// how a re-run is forced: the shipped set re-runs in full whenever any
    /// auth migration changes, and this repair has to be a no-op the second
    /// time through.
    async fn apply_the_repair_as(ctx: &TestContext, block_name: &str) {
        migration_helper::apply_migrations(ctx, block_name, &the_repair(), &[])
            .await
            .expect("apply the api-key expiry repair");
    }

    async fn apply_the_repair(ctx: &TestContext) {
        apply_the_repair_as(ctx, "wafer-run/auth").await;
    }

    /// Write an `api_keys` row with `expires_at` exactly as given. Nothing in
    /// the crate can write one any more — `NewApiKey::expires_at` is an
    /// instant — so the pre-upgrade row has to be built here.
    async fn seed_key(ctx: &TestContext, key_hash: &str, expires_at: Option<&str>) -> String {
        let mut row: HashMap<String, Value> = HashMap::new();
        row.insert("user_id".into(), json!("owner"));
        row.insert("name".into(), json!(key_hash));
        row.insert("key_hash".into(), json!(key_hash));
        row.insert("key_prefix".into(), json!("sb_legacy"));
        row.insert("created_at".into(), json!("2026-01-01T00:00:00Z"));
        if let Some(expiry) = expires_at {
            row.insert("expires_at".into(), json!(expiry));
        }
        wafer_core::clients::database::create(ctx, api_keys::TABLE, row)
            .await
            .unwrap_or_else(|e| panic!("seed {key_hash}: {e}"))
            .id
    }

    async fn row(ctx: &TestContext, id: &str) -> api_keys::ApiKeyRow {
        api_keys::find_by_id(ctx, id)
            .await
            .unwrap_or_else(|e| panic!("read {id}: {e}"))
            .unwrap_or_else(|| panic!("{id} is gone"))
    }

    /// `Z`, `z`, `+00:00`, `-00:00` and a space separator all name the same
    /// instant; the column keeps one spelling of it.
    #[tokio::test]
    async fn a_utc_expiry_is_respelled_without_moving_the_instant() {
        let ctx = upgrading_deployment().await;
        let ids = [
            ("plus-zero", "2026-06-01T12:00:00+00:00"),
            ("minus-zero", "2026-06-01T12:00:00-00:00"),
            ("lower-z", "2026-06-01T12:00:00z"),
            ("spaced", "2026-06-01 12:00:00Z"),
            ("lower-t", "2026-06-01t12:00:00Z"),
            ("already-canonical", "2026-06-01T12:00:00Z"),
        ];
        let mut seeded = Vec::new();
        for (hash, stored) in ids {
            seeded.push((hash, seed_key(&ctx, hash, Some(stored)).await));
        }

        apply_the_repair(&ctx).await;

        for (hash, id) in seeded {
            let key = row(&ctx, &id).await;
            assert_eq!(
                key.expires_at.as_deref(),
                Some("2026-06-01T12:00:00Z"),
                "{hash}"
            );
            assert!(
                !key.is_revoked(),
                "{hash} names an instant, so it is not revoked"
            );
        }
    }

    /// An expiry that names no instant is what the string comparison got
    /// most wrong — `never` sorted after every clock reading. The key is
    /// already dead to the reader; the repair records that as a revocation.
    #[tokio::test]
    async fn an_expiry_that_names_no_instant_is_revoked() {
        let ctx = upgrading_deployment().await;
        let mut seeded = Vec::new();
        // The last two are timestamp-shaped and still name no instant: RFC
        // 3339 requires an offset, and `+0900` is ISO 8601's basic form,
        // which `repo::parse_iso` rejects.
        for stored in [
            "never",
            "2026-06-01",
            "0",
            "2026-06-01T12:00:00",
            "2026-06-01T12:00:00+0900",
        ] {
            seeded.push((stored, seed_key(&ctx, stored, Some(stored)).await));
        }

        apply_the_repair(&ctx).await;

        for (stored, id) in seeded {
            let key = row(&ctx, &id).await;
            assert!(key.is_revoked(), "{stored:?} names no instant");
            assert_eq!(
                key.expires_at.as_deref(),
                Some(stored),
                "the stored text is the only record of why the key was revoked"
            );
            assert!(key.is_expired(chrono::Utc::now()));
        }
    }

    /// SQLite's `substr`/`length` are character functions that stop at the
    /// first NUL, so a value whose readable prefix is a clean timestamp
    /// passes every shape test while the row is something else entirely.
    /// Respelling it to the prefix would resurrect a key `parse_iso`
    /// refuses.
    #[tokio::test]
    async fn an_expiry_with_an_embedded_nul_is_not_respelled_into_a_clean_one() {
        let ctx = upgrading_deployment().await;
        let stored = "2026-06-01T12:00:00Z\u{0}junk";
        let id = seed_key(&ctx, "nul", Some(stored)).await;
        // Non-vacuity: the NUL has to survive the round trip, or the row
        // under test is not the row this guards against.
        assert_eq!(
            row(&ctx, &id).await.expires_at.as_deref(),
            Some(stored),
            "the seeded NUL must reach the column"
        );

        apply_the_repair(&ctx).await;

        let key = row(&ctx, &id).await;
        assert_ne!(
            key.expires_at.as_deref(),
            Some("2026-06-01T12:00:00Z"),
            "the prefix is not the value"
        );
        assert!(
            key.is_revoked(),
            "the byte guard sends it to the revoke arm"
        );
        assert!(key.is_expired(chrono::Utc::now()));
    }

    /// Every case in `015_api_key_expiry_canonical.cases.tsv`, held to the
    /// reader first and to the repair second. The migration's line between
    /// "respell" and "revoke" is drawn by hand in SQL, so the one thing it
    /// must never do is disagree with `repo::parse_iso`: a key the reader
    /// refuses left looking active in the admin tab, or a key the reader
    /// accepts revoked. The cases sit at every edge of chrono's RFC 3339
    /// grammar — the calendar, each field's range, the fraction, the offset
    /// and what may follow it. CI's PostgreSQL job runs the same file
    /// through the PostgreSQL dialect.
    #[tokio::test]
    async fn every_case_is_revoked_exactly_when_the_reader_refuses_it() {
        let cases: Vec<(&str, &str, bool)> = include_str!("015_api_key_expiry_canonical.cases.tsv")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| match line.split('\t').collect::<Vec<_>>()[..] {
                [stored, after, "active"] => (stored, after, false),
                [stored, after, "revoked"] => (stored, after, true),
                _ => panic!("malformed case line {line:?}"),
            })
            .collect();

        for &(stored, after, revoked) in &cases {
            let read = crate::blocks::auth::repo::parse_iso(stored);
            assert_eq!(
                read.is_none(),
                revoked,
                "case {stored:?}: `revoked` must be exactly what parse_iso refuses"
            );
            if let Some(instant) = read {
                assert_eq!(
                    crate::blocks::auth::repo::parse_iso(after),
                    Some(instant),
                    "case {stored:?}: a respelling must name the same instant"
                );
            } else {
                assert_eq!(
                    after, stored,
                    "case {stored:?}: a revoked row keeps its text"
                );
            }
        }

        let ctx = upgrading_deployment().await;
        let mut seeded = Vec::new();
        for &(stored, after, revoked) in &cases {
            seeded.push((
                stored,
                after,
                revoked,
                seed_key(&ctx, stored, Some(stored)).await,
            ));
        }

        apply_the_repair(&ctx).await;

        let mut wrong = Vec::new();
        for (stored, after, revoked, id) in seeded {
            let key = row(&ctx, &id).await;
            if key.expires_at.as_deref() != Some(after) || key.is_revoked() != revoked {
                wrong.push(format!(
                    "{stored:?}: expires_at {:?}, revoked {} (want {after:?}, revoked {revoked})",
                    key.expires_at,
                    key.is_revoked()
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "015 disagrees with the reader on:\n{}",
            wrong.join("\n")
        );
    }

    /// What `<input type="date">` and `<input type="datetime-local">` post.
    /// RFC 3339 requires an offset, so neither is readable, and both are
    /// revoked rather than silently inert.
    #[tokio::test]
    async fn the_shapes_an_html_date_field_posts_are_revoked() {
        let ctx = upgrading_deployment().await;
        let mut seeded = Vec::new();
        for stored in ["2027-01-31", "2027-01-31T09:00"] {
            seeded.push((stored, seed_key(&ctx, stored, Some(stored)).await));
        }

        apply_the_repair(&ctx).await;

        for (stored, id) in seeded {
            let key = row(&ctx, &id).await;
            assert!(key.is_revoked(), "{stored} carries no offset");
        }
    }

    /// A key with no expiry at all is not a key with a broken one. Both
    /// spellings of "unset" survive the repair untouched.
    #[tokio::test]
    async fn a_key_with_no_expiry_is_left_alone() {
        let ctx = upgrading_deployment().await;
        let absent = seed_key(&ctx, "absent", None).await;
        let empty = seed_key(&ctx, "empty", Some("")).await;

        apply_the_repair(&ctx).await;

        for id in [&absent, &empty] {
            let key = row(&ctx, id).await;
            assert!(!key.is_revoked());
            assert!(key.expires_at.as_deref().unwrap_or("").is_empty());
            assert!(!key.is_expired(chrono::Utc::now()));
        }
    }

    /// The two shapes the repair deliberately does not touch: respelling
    /// either needs arithmetic, and the reader reads both correctly as they
    /// stand.
    #[tokio::test]
    async fn an_offset_or_fractional_expiry_is_kept_as_it_is() {
        let ctx = upgrading_deployment().await;
        let offset = seed_key(&ctx, "offset", Some("2026-06-01T20:00:00+09:00")).await;
        let fraction = seed_key(&ctx, "fraction", Some("2026-06-01T12:00:00.123456+00:00")).await;

        apply_the_repair(&ctx).await;

        let offset = row(&ctx, &offset).await;
        assert_eq!(
            offset.expires_at.as_deref(),
            Some("2026-06-01T20:00:00+09:00")
        );
        assert!(!offset.is_revoked());
        // 20:00+09:00 is 11:00Z, and that is the instant the reader uses.
        assert!(offset.is_expired(
            crate::blocks::auth::repo::parse_iso("2026-06-01T11:00:01Z").expect("test timestamp")
        ));

        let fraction = row(&ctx, &fraction).await;
        assert_eq!(
            fraction.expires_at.as_deref(),
            Some("2026-06-01T12:00:00.123456+00:00")
        );
        assert!(!fraction.is_revoked());
    }

    /// Auth migrations re-run in full whenever any of them changes, so the
    /// second pass must change nothing — in particular it must not push the
    /// revoked row's expiry forward again.
    #[tokio::test]
    async fn a_second_pass_changes_nothing() {
        let ctx = upgrading_deployment().await;
        let broken = seed_key(&ctx, "never", Some("never")).await;
        let utc = seed_key(&ctx, "utc", Some("2026-06-01T12:00:00+00:00")).await;

        apply_the_repair(&ctx).await;
        let after_first = (row(&ctx, &broken).await, row(&ctx, &utc).await);

        apply_the_repair_as(&ctx, "wafer-run/auth#rerun").await;

        assert_eq!(
            (row(&ctx, &broken).await, row(&ctx, &utc).await),
            after_first
        );
    }
}

#[cfg(test)]
mod re_run_survival_tests {
    //! What a full re-run of the auth set does to live rows.
    //!
    //! The set re-runs whenever ANY auth migration's SQL changes, so every
    //! statement in it runs again on a database full of real rows. A `DROP
    //! TABLE` in that set is not a one-time upgrade step — it is a delete
    //! that fires on every later schema change.
    //!
    //! Seeded and read back through the repo doors, and re-applied through
    //! `apply_migrations` — the path `--run-migrations` takes — so what is
    //! asserted is what the block sees. The second apply is given its own
    //! state key because the hash gate would otherwise skip identical SQL;
    //! in production the hash differs precisely because a migration changed.

    use super::SQLITE_MIGRATIONS;
    use crate::{
        blocks::auth::repo::{
            sessions::{self, NewSession},
            tokens,
        },
        migration_helper,
        test_support::TestContext,
    };

    async fn upgrading_deployment() -> TestContext {
        let mut ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        ctx.set_config(migration_helper::RUN_MIGRATIONS_KEY, "1");
        ctx.seed_auth_user("u1").await;
        ctx
    }

    async fn re_run_the_whole_set(ctx: &TestContext) {
        let sql: Vec<&str> = SQLITE_MIGRATIONS.iter().map(|(_, s)| *s).collect();
        migration_helper::apply_migrations(ctx, "wafer-run/auth#rerun", &sql, &[])
            .await
            .expect("re-apply the auth schema");
    }

    /// `004_refresh_tokens` opened with `DROP TABLE IF EXISTS
    /// wafer_run__auth__tokens`. `auth_ui::api::refresh` refuses a token
    /// whose row is gone, so adding any auth migration logged every
    /// signed-in user out within one access-token lifetime — on an upgrade
    /// that never mentioned tokens.
    #[tokio::test]
    async fn refresh_tokens_survive_a_full_re_run() {
        let ctx = upgrading_deployment().await;
        tokens::insert(
            &ctx,
            "u1",
            "raw-refresh-token",
            "fam-1",
            0,
            "2099-01-01T00:00:00Z",
        )
        .await
        .expect("seed the refresh token");

        re_run_the_whole_set(&ctx).await;

        let row = tokens::find_by_token(&ctx, "raw-refresh-token")
            .await
            .expect("the tokens table must still be readable");
        assert!(
            row.is_some(),
            "a live refresh token must survive an auth schema change"
        );
    }

    /// The one drop the set still carries, stated rather than discovered:
    /// `012_sessions_family` recreates `wafer_run__auth__sessions`. Nothing
    /// authenticates against that table — it is the device list, and
    /// `record_login_family` re-inserts a device on its next token refresh —
    /// so emptying it signs nobody out. RELEASE.md says so on both notes.
    #[tokio::test]
    async fn the_device_list_is_the_only_thing_a_re_run_empties() {
        let ctx = upgrading_deployment().await;
        sessions::insert(
            &ctx,
            NewSession {
                family: "fam-1".into(),
                user_id: "u1".into(),
                auth_method: "password".into(),
                expires_at: "2099-01-01T00:00:00Z".into(),
            },
        )
        .await
        .expect("seed the device row");
        assert_eq!(sessions::list_for_user(&ctx, "u1").await.unwrap().len(), 1);

        re_run_the_whole_set(&ctx).await;

        assert!(sessions::list_for_user(&ctx, "u1")
            .await
            .unwrap()
            .is_empty());
    }
}
