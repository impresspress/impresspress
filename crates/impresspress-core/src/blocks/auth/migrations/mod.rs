//! Auth block migrations. Applied from the framework auth service's
//! `AuthService::init` (the framework `wafer-run/auth` block's
//! `lifecycle(Init)` delegates to it) via
//! [`crate::migration_helper::apply_migrations`].
//!
//! Each file runs once, as one transaction with its row in the per-file
//! migration ledger (`impresspress__admin__applied_migrations`); a release
//! runs only the files the ledger does not record. `migration_helper`'s
//! module docs describe the runner.

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
const SQL_016_SQLITE: &str = include_str!("016_roles_are_grants.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_016_POSTGRES: &str = include_str!("016_roles_are_grants.postgres.sql");
const SQL_017_SQLITE: &str = include_str!("017_reset_token_columns.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_017_POSTGRES: &str = include_str!("017_reset_token_columns.postgres.sql");
const SQL_018_SQLITE: &str = include_str!("018_drop_jwt_blocklist.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_018_POSTGRES: &str = include_str!("018_drop_jwt_blocklist.postgres.sql");

/// Basename of the API-key expiry repair, named once so the migration list
/// and the test that drives it cannot drift apart.
pub(crate) const API_KEY_EXPIRY_CANONICAL: &str = "015_api_key_expiry_canonical";

/// Basename of the migration that makes a role a grant row only.
///
/// # The 016 note
///
/// An account's role used to live in two places: the inline
/// `wafer_run__auth__users.role` column, written once at account creation,
/// and `impresspress__admin__user_roles` rows, written by admin's IAM. Every
/// reader unioned them, while IAM could edit only the rows, so an inline
/// `admin` — every bootstrapped admin — could not be demoted. 016 copies each
/// inline role into a grant and drops the column; from then on an account's
/// roles are its grant rows, which account creation writes in the account's
/// own batch (`repo::users::insert_with_ops`).
///
/// - **Runs once.** The ledger records it, and nothing runs it again. As it
///   shipped (until the ledger release), it opened with an `ADD COLUMN` that
///   recreated the column empty, so that the runner of the time, which re-ran
///   the whole list on every change and tolerated a duplicate `ADD COLUMN`,
///   could re-run it; that `ADD` was a duplicate on every database 016 had
///   not run on, a fresh one included, so it could not apply as one
///   transaction. The ledger release removed it: the file is now its copy
///   and its `DROP`, and applies atomically from empty. A database that ran
///   the old text is adopted from the history table
///   (`crate::migration_history`), which pins both texts.
/// - **The `NOT EXISTS`** skips an account that already holds the grant, and
///   so also covers a browser install created before admin's 004 built the
///   `(user_id, role)` unique index.
/// - **`DROP COLUMN` support.** Nothing but 001's `CREATE TABLE` names the
///   column: no index, view or trigger. SQLite added `DROP COLUMN` in 3.35;
///   native runs the SQLite rusqlite bundles, the browser sql.js 3.45, and D1
///   current SQLite.
/// - **Admin's schema must already exist.** The `INSERT` names admin's
///   `user_roles`, so the auth list fails with `no such table` on a database
///   admin's 001 has not built. What guarantees it is the boot order: every
///   target initialises admin before the other blocks
///   (`builder::boot`'s `init_block(admin)`), and the test fixtures apply
///   admin's list first (`TestContext::with_auth`). It is not that auth's
///   migration state lives in admin's `block_settings`: a missing
///   `block_settings` table is tolerated. `the_auth_list_needs_admins_schema`
///   pins this, so a boot reorder fails a test.
/// - **Ids.** 016 mints `ur_<32 hex>` ids; a grant written at runtime takes
///   the id the database mints (a UUIDv7). Nothing parses a grant id:
///   `UserRoleRow::from_record` requires only a non-empty `user_id` and
///   `role`.
/// - **`assigned_by` is empty**, as on every grant no admin made.
/// - **An auth migration writing admin's table.** It runs through `db::ddl`,
///   which is not table-scoped today. A future wafer-run change that scopes
///   `ddl` must keep migration files as the stated exception; 014's `UPDATE`
///   already relies on it, and at runtime auth may append to the table
///   (`blocks/admin/mod.rs`'s grants).
/// - **What it cost.** Adding the file re-ran the auth list (the runner from
///   before the per-file ledger). 004 no longer
///   drops the refresh-token table, and since the family-keyed sessions
///   layout moved into 001, 012 no longer drops the session table either, so
///   a re-run by a binary carrying both keeps every session. A binary
///   carrying 016 with 012's old DROP emptied the device list, which signed
///   nobody out then (issuance re-created a missing row) and signs every
///   device out at its next refresh under the session-row liveness rule
///   (RELEASE.md, the 016 note's correction). Inline admins keep admin, as
///   grants. A dev data snapshot
///   carrying users must be re-exported (`data_snapshot::SCHEMA_VERSION` 2).
///
/// The upgrade itself, on both dialects, is tested in
/// `crates/impresspress-native/tests/auth_roles_are_grants.rs`.
pub const ROLES_ARE_GRANTS: &str = "016_roles_are_grants";

/// Basename of the migration that declares the password-reset columns.
///
/// # The 017 note
///
/// `repo::users`' reset-token functions write `reset_token` (the SHA-256 hex
/// of the emailed token) and `reset_token_expires` on the users row, and no
/// migration declared either. Without strict schema the runtime created them
/// on the first reset request; under it (every Cloudflare deployment) that
/// request failed, and forgot-password answered its generic success with no
/// mail sent. 017 declares both, nullable TEXT, the definitions lazy
/// column-add gave them.
///
/// - **A plain `ADD COLUMN`, not a rebuild or a new name.** `users` is the
///   parent of six `ON DELETE CASCADE` foreign keys, so a rebuild's
///   `DROP TABLE` would delete every credential, session and grant (SQLite
///   cascades on the implicit `DELETE`, and D1 enforces foreign keys
///   always). And the original names are kept, by the user's decision
///   (2026-10-29): there are no production users.
/// - **What that costs.** A native or browser database that served a reset
///   before strict schema became the only behaviour already holds both
///   columns, created lazily. There
///   017 fails whole inside its transaction, nothing of it runs, and the
///   runner refuses the database by name as drifted schema
///   (`Refusal::DriftedSchema`); such a database is recreated. RELEASE.md
///   names it, local-run included.
/// - **Outstanding reset links** need nothing: on a database where 017 runs
///   no reset was ever stored.
pub const RESET_TOKEN_COLUMNS: &str = "017_reset_token_columns";

/// Basename of the migration that drops the retired JWT blocklist.
///
/// # The 018 note
///
/// `wafer_run__auth__jwt_blocklist` (005) recorded the `jti` of each access
/// token ended by logout or by revoking a device. It became a second record
/// of the same end: an access token authenticates only while its login
/// family's session row is live (`crypto::verify_access_token` reads
/// `repo::sessions::is_live` and the account's `auth_version`), and both of
/// the blocklist's writers deleted that row first. Nothing has read or
/// written the table since (RELEASE.md, "Auth: the JWT blocklist is
/// retired"), and access tokens carry no `jti`. 018 drops it, and its index
/// with it.
///
/// - **Nobody is signed out.** Every blocklisted `jti` belongs to a family
///   whose session row is gone and cannot return, so dropping the rows
///   un-revokes nothing. No foreign key names the table in either direction
///   (005's `user_id` has no `REFERENCES`), so SQLite's implicit `DELETE`
///   cascades nowhere; no session, refresh token, API key or account row
///   changes.
/// - **A plain `DROP TABLE`, no `IF EXISTS`.** The ledger runs 018 only on a
///   database that records 001–017, directly or by adoption from a supported
///   history state, and 005 created the table on every one of them: a fresh
///   install, a ledger-era database, and every `Applied` state of the
///   history table, whose compositions all carry 005's `wafer_run__` text.
///   A database without it was adopted wrongly or edited by hand outside the
///   app. There the file fails whole inside its transaction, writes no
///   ledger row and leaves `current_hash` alone, and the runner reports it as
///   a failed file, not a refusal, carrying the backend's own error (`no
///   such table: …` on SQLite, `table "…" does not exist` on PostgreSQL).
///   D1 does not say which statement failed, so there the operator reads the
///   message rather than a statement. `IF EXISTS` would record 018 as `ran`
///   over a schema the ledger does not describe.
/// - **005 and 010 stay as shipped.** A shipped file is immutable: the ledger
///   pins each file's content hash, so 005 still creates the table and 010
///   still adds its `id`, `created_at` and `updated_at` columns before 018
///   drops it on a fresh install.
/// - **The `suppers_ai__auth__jwt_blocklist` residue** on databases that
///   crossed from a state before the `wafer_run__` rename is not 018's
///   concern: no supported state holds it, and the crossing record
///   (`migration-history-crossing.txt`) already names it harmless residue.
///
/// Tested on both dialects by `test_support::jwt_blocklist_drop`.
pub const JWT_BLOCKLIST_DROPPED: &str = "018_drop_jwt_blocklist";

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle(Init)` apply path (auth's `init`).
/// Order here is the apply order.
pub const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
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
    (ROLES_ARE_GRANTS, SQL_016_SQLITE),
    (RESET_TOKEN_COLUMNS, SQL_017_SQLITE),
    (JWT_BLOCKLIST_DROPPED, SQL_018_SQLITE),
];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`] one
/// for one. Selected at runtime by `apply_migrations`. Empty when the
/// `postgres` feature is off — see `files::migrations`'s doc for the
/// rationale (Cloudflare/D1 never selects postgres; don't embed dead SQL).
#[cfg(feature = "postgres")]
pub const POSTGRES_MIGRATIONS: &[(&str, &str)] = &[
    ("001_auth_schema", SQL_001_POSTGRES),
    ("002_reserved_orgs", SQL_002_POSTGRES),
    ("003_oauth_pkce_states", SQL_003_POSTGRES),
    ("004_refresh_tokens", SQL_004_POSTGRES),
    ("005_jwt_blocklist", SQL_005_POSTGRES),
    ("006_user_extended_fields", SQL_006_POSTGRES),
    ("007_api_keys", SQL_007_POSTGRES),
    ("008_rate_limits", SQL_008_POSTGRES),
    ("009_auth_version", SQL_009_POSTGRES),
    ("010_strict_schema_columns", SQL_010_POSTGRES),
    ("011_rate_limit_retention", SQL_011_POSTGRES),
    ("012_sessions_family", SQL_012_POSTGRES),
    ("013_email_proof", SQL_013_POSTGRES),
    ("014_clear_provider_access_tokens", SQL_014_POSTGRES),
    (API_KEY_EXPIRY_CANONICAL, SQL_015_POSTGRES),
    (ROLES_ARE_GRANTS, SQL_016_POSTGRES),
    (RESET_TOKEN_COLUMNS, SQL_017_POSTGRES),
    (JWT_BLOCKLIST_DROPPED, SQL_018_POSTGRES),
];
#[cfg(not(feature = "postgres"))]
pub const POSTGRES_MIGRATIONS: &[(&str, &str)] = &[];

/// This block's migrations as its `lifecycle(Init)` applies them, and as
/// [`crate::blocks::all_block_migrations`] lists them: both dialects' files
/// and both dialects' pre-ledger history (`crate::migration_history`).
pub const MIGRATIONS: crate::migration_helper::BlockMigrations =
    crate::migration_helper::BlockMigrations {
        block: super::AUTH_BLOCK_ID,
        sqlite: SQLITE_MIGRATIONS,
        postgres: POSTGRES_MIGRATIONS,
        sqlite_history: crate::migration_history::auth::SQLITE,
        postgres_history: crate::migration_history::auth::POSTGRES,
    };

/// Apply the auth schema through the shared migration-state gate.
///
/// Production no longer calls this: the framework auth service applies these
/// migrations inside [`AuthService::init`](super::service) (via
/// `apply_migrations` directly, because it needs an `AuthError` return).
/// This thin forwarder exists for the `tests/auth/*` integration suite, which
/// applies the auth schema against an in-memory fixture before exercising the
/// repo layer — test-fixture setup is an explicit exception to the
/// no-raw-migration-runner rule (CLAUDE.md).
pub async fn apply(
    ctx: &dyn wafer_run::context::Context,
) -> Result<(), crate::migration_helper::MigrationError> {
    crate::migration_helper::apply_migrations(ctx, &MIGRATIONS).await
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
    //! `db::create` writes (they existed only because the runtime's lazy
    //! column-add, since removed, materialised them), then the 010 ALTER lands.
    //! An in-place `CREATE TABLE IF NOT EXISTS` edit is a no-op on an existing
    //! table, so without 010's ALTER the first write would fail `no such
    //! column` — the regression this guards.

    use std::{collections::HashMap, sync::Arc};

    use serde_json::json;
    use wafer_block_sqlite::service::SQLiteDatabaseService;
    use wafer_core::interfaces::database::service::DatabaseService;

    use super::SQLITE_MIGRATIONS;
    use crate::{blocks::auth::AUTH_BLOCK_ID, test_support::migrate_sqlite};

    /// Where 010 sits: every auth file before it is the pre-upgrade on-disk
    /// schema 010 upgrades, and only those.
    fn at_010() -> usize {
        SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == "010_strict_schema_columns")
            .expect("010 is wired")
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
        migrate_sqlite(&db, AUTH_BLOCK_ID, &SQLITE_MIGRATIONS[..at_010()])
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

        // 3. Apply the 010 ALTER migration (the fix) against the existing
        //    tables, as the upgrade's ledger run does.
        migrate_sqlite(&db, AUTH_BLOCK_ID, &SQLITE_MIGRATIONS[..=at_010()])
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

        // 4. `create` stamps id/created_at/updated_at (the same shape `db::create`
        //    produces) and INSERTs those columns literally. Before
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
        // table whose real PK is `family`.
        let mut sess = HashMap::new();
        sess.insert("family".to_string(), json!("fam-1"));
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
    //! `014_clear_provider_access_tokens` on the path a live database takes
    //! through it: link rows already holding provider tokens, then the
    //! upgrade's ledger run applies 014 and on.

    use std::sync::Arc;

    use serde_json::json;
    use wafer_block_sqlite::service::SQLiteDatabaseService;
    use wafer_core::interfaces::database::service::DatabaseService;

    use super::SQLITE_MIGRATIONS;
    use crate::{blocks::auth::AUTH_BLOCK_ID, test_support::migrate_sqlite};

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
    async fn the_upgrade_clears_stored_provider_tokens() {
        let db: Arc<dyn DatabaseService> =
            Arc::new(SQLiteDatabaseService::open_in_memory().unwrap());
        let before = SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == "014_clear_provider_access_tokens")
            .expect("014 is wired");
        // Admin's list first, as every boot applies it: 016 copies roles into
        // admin's `user_roles` (the 016 note on `ROLES_ARE_GRANTS`).
        migrate_sqlite(&db, AUTH_BLOCK_ID, &SQLITE_MIGRATIONS[..before])
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

        // The upgrade: the ledger runs 014 and on.
        migrate_sqlite(&db, AUTH_BLOCK_ID, SQLITE_MIGRATIONS)
            .await
            .expect("the upgrade succeeds");
        assert_eq!(stored_tokens(&db).await, vec!["", ""]);
    }
}

#[cfg(test)]
mod roles_are_grants_tests {
    //! The ordering 016 relies on (the 016 note on [`super::ROLES_ARE_GRANTS`]).
    //! The upgrade itself is tested on both dialects in
    //! `crates/impresspress-native/tests/auth_roles_are_grants.rs`.

    use std::sync::Arc;

    use wafer_block_sqlite::service::SQLiteDatabaseService;
    use wafer_core::interfaces::database::service::DatabaseService;
    use wafer_sql_utils::Backend;

    use super::SQLITE_MIGRATIONS;
    use crate::{
        blocks::auth::AUTH_BLOCK_ID, migration_helper::apply_pending_via_service,
        test_support::migrate_sqlite,
    };

    /// 016 writes admin's `user_roles`, so the auth list applies only where
    /// admin's schema already exists — which every boot guarantees by
    /// initialising admin first. Pinned so a boot reorder fails here, not in
    /// a deployment whose auth migrations stop at 016.
    #[tokio::test]
    async fn the_auth_list_needs_admins_schema() {
        let bare: Arc<dyn DatabaseService> =
            Arc::new(SQLiteDatabaseService::open_in_memory().unwrap());
        let err = apply_pending_via_service(
            &bare,
            Backend::Sqlite,
            AUTH_BLOCK_ID,
            SQLITE_MIGRATIONS,
            &[],
        )
        .await
        .expect_err("the auth list cannot apply before admin's")
        .to_string();
        assert!(
            err.contains("no such table: impresspress__admin__"),
            "it is admin's tables the auth list needs: {err}"
        );

        // `migrate_sqlite` applies admin's whole list first, as boot does.
        let ordered: Arc<dyn DatabaseService> =
            Arc::new(SQLiteDatabaseService::open_in_memory().unwrap());
        migrate_sqlite(&ordered, AUTH_BLOCK_ID, SQLITE_MIGRATIONS)
            .await
            .expect("after admin's, the auth list applies");
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
    //! Driven through `apply_pending` — the path a migrating deploy or
    //! boot takes — over a database whose auth list stops before the repair,
    //! and read back through `repo::api_keys`, so what is asserted is what
    //! the block sees.

    use std::collections::HashMap;

    use serde_json::{json, Value};

    use super::{API_KEY_EXPIRY_CANONICAL, SQLITE_MIGRATIONS};
    use crate::{blocks::auth::repo::api_keys, migration_helper, test_support::TestContext};

    /// The shipped list before the repair, sliced out by name: an unwired
    /// repair would leave the whole list here and trip this assert instead of
    /// silently testing nothing.
    fn before_the_repair() -> &'static [(&'static str, &'static str)] {
        let at = SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == API_KEY_EXPIRY_CANONICAL)
            .unwrap_or_else(|| {
                panic!(
                    "{API_KEY_EXPIRY_CANONICAL} must be wired into SQLITE_MIGRATIONS to reach a \
                     deployed database"
                )
            });
        &SQLITE_MIGRATIONS[..at]
    }

    /// A deployment whose auth list stops before the repair, holding a user,
    /// in a build that applies migrations, as every deploy and native boot
    /// is.
    async fn upgrading_deployment() -> TestContext {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        migration_helper::apply_pending(
            &ctx,
            crate::blocks::auth::AUTH_BLOCK_ID,
            before_the_repair(),
            &[],
        )
        .await
        .expect("the auth list before the repair");
        ctx.set_config(migration_helper::RUN_MIGRATIONS_KEY, "1");
        ctx.seed_auth_user("owner").await;
        ctx
    }

    /// The upgrade: the shipped list, whose pending files are the repair and
    /// what follows it.
    async fn apply_the_repair(ctx: &TestContext) {
        migration_helper::apply_pending(
            ctx,
            crate::blocks::auth::AUTH_BLOCK_ID,
            SQLITE_MIGRATIONS,
            &[],
        )
        .await
        .expect("apply the api-key expiry repair");
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

    /// Before the ledger, the runner re-ran every file whenever the list
    /// changed, so a database that kept upgrading ran this repair more than
    /// once: `migration-history.toml` declares it `rerun_safe`, and the
    /// history's verdicts for those states rest on that. Pinned here with the
    /// runner from before the ledger: a second pass changes nothing — in
    /// particular it does not push the revoked row's expiry forward again.
    #[tokio::test]
    async fn a_second_pass_changes_nothing() {
        let ctx = upgrading_deployment().await;
        let broken = seed_key(&ctx, "never", Some("never")).await;
        let utc = seed_key(&ctx, "utc", Some("2026-06-01T12:00:00+00:00")).await;

        apply_the_repair(&ctx).await;
        let after_first = (row(&ctx, &broken).await, row(&ctx, &utc).await);

        let (_, repair) = SQLITE_MIGRATIONS
            .iter()
            .find(|(name, _)| *name == API_KEY_EXPIRY_CANONICAL)
            .expect("the repair is wired");
        migration_helper::legacy_runner::apply(
            &ctx.database_service(),
            &[(API_KEY_EXPIRY_CANONICAL, repair)],
        )
        .await
        .expect("the old runner's second pass");

        assert_eq!(
            (row(&ctx, &broken).await, row(&ctx, &utc).await),
            after_first
        );
    }
}
