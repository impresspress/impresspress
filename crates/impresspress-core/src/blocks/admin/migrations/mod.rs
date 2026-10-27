//! Admin block migrations. Applied from the block's `Init` lifecycle via
//! [`crate::migration_helper::lifecycle_init`].
//!
//! SQL files are embedded with `include_str!`. Backend dispatch, the
//! per-file ledger and the `current_hash` / `IMPRESSPRESS_RUN_MIGRATIONS`
//! gate all live in [`crate::migration_helper::apply_migrations`]. Earlier
//! versions of this module called `db::ddl` directly in a loop, bypassing
//! the gate and re-running every DDL on every cold isolate (~2,800 D1
//! queries/day on wafer.run — the measurement that motivated the gate).

const SQL_001_SQLITE: &str = include_str!("001_admin_schema.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_001_POSTGRES: &str = include_str!("001_admin_schema.postgres.sql");
// 002's header comment — in BOTH dialect files — says the `block` column is
// "for indexed per-block lookup by D1ConfigSource". That was true when the
// migration shipped and is not any more: `D1ConfigSource` reads the whole
// variables table once and groups by `block` in memory, issuing no
// `WHERE block = ?` at all (see that module's doc). The column and the index
// this migration creates are still live — `block` is exactly what the
// in-memory grouping keys on — so only the query strategy moved on.
//
// Both dialect files ALSO end their header with a `Spec:` line naming a
// design document that does not exist in this repository and never did. Read
// the paragraph above instead; it is the whole of what that pointer was
// standing in for.
//
// The .sql files are not edited to say so here. A comment edit leaves the
// file's ledger content hash alone (comments are not statements), but it
// moves the whole-text hash a database from before the ledger recorded, so
// every history state naming that text needs its old text pinned as a
// fixture (`examples/migration-history.rs --write` copies it, and
// `tests/migration_history.rs` says which state needs one). That is its own
// change, with its own review of the regenerated table. See
// `crate::migration_helper`'s "A shipped file's statements are immutable".
// It is also why `scripts/check-doc-pointers.sh` skips `migrations/*.sql`.
// This file is not skipped.
const SQL_002_SQLITE: &str = include_str!("002_variables_block_column.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_002_POSTGRES: &str = include_str!("002_variables_block_column.postgres.sql");
// 003 adds `block_settings.seed_defaults_hash`, which lets
// `crate::blocks::admin::settings::seed_defaults` skip its bulk `variables`
// read when the declared shared config has not changed since the last seed.
// The gate is the same shape `crate::migration_helper::apply_pending` uses
// for DDL: hash the payload, compare against the stored digest, return early
// on a match. `seed_defaults` computes its side through `seed_payload_hash`
// over `crate::config_vars::shared_config_vars()` and stamps the column on a
// miss. Both directions are covered in `admin::settings`'s own tests —
// `second_call_with_matching_snapshot_hash_short_circuits` and
// `mismatched_snapshot_hash_re_runs_seed` — because what the gate does is a
// property of that function, not of this DDL.
//
// Both dialect files end their header on a dangling `Spec:` pointer of their
// own — a different document from 002's, and equally absent from this
// repository — left in place for the same immutability reason. What it stood
// for is the paragraph above. The sqlite file additionally credits the D1
// read volume it removed to "PR 2 of the 2026-05-14 config-snapshot spec";
// that document is not here either. The reads it names came from the bulk
// `list_all` that work added to `seed_defaults` — exactly what this column
// lets the function skip.
const SQL_003_SQLITE: &str = include_str!("003_block_settings_seed_hash.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_003_POSTGRES: &str = include_str!("003_block_settings_seed_hash.postgres.sql");
// 004 makes `user_roles` hold at most one grant per `(user_id, role)`.
//
// `platform_state::user_roles::assign` is a read-then-insert, so two callers
// can race it: two admins assigning one role at once, and — when this
// migration was written — two logins of the bootstrap-admin address, which
// the login path then granted `admin` (no sign-in grants a role any more).
// Both racers could find no grant and both insert one. A duplicate is not
// harmless. `get_user_roles` folds the twins into one role, while `iam::handle_remove_role` deletes ONE row by id —
// so a revoke answered `{"deleted": true}`, bumped the auth version and wrote
// its audit row, and the surviving twin kept granting the role on the very
// next token. The index makes the insert itself the claim: the losing racer
// is refused, and `assign` reads that refusal back as "already assigned".
//
// Rows that already repeat a grant are collapsed onto one deterministic
// survivor per pair: the least `(created_at, id)` in the database's own text
// ordering, so the result does not depend on row order. Both dialect files
// say "keeping the earliest" in their header; that holds under SQLite's
// byte-order comparison of these ISO-8601 stamps, but a Postgres column under
// a non-C collation orders text by locale rules, which need not be
// chronological. The header is left as shipped (see 002's note on why), and
// nothing depends on which twin survives: every deleted row names a
// `(user_id, role)` pair its survivor still grants, so no user's effective
// roles change — what goes is the second row a revoke could miss. `RELEASE.md`
// carries the operator-facing version.
//
// Where it runs: the first native boot, Cloudflare deploy (`/_deploy/init`)
// or browser boot of a build that carries it — each is a build that applies
// pending migrations, and 004 is pending where the database has not
// recorded it.
//
// The ledger runs it once, in one transaction. It was written to be re-run,
// because the runner before the ledger re-ran admin's whole list whenever it
// changed: once the index exists the `DELETE` finds nothing and the `CREATE
// UNIQUE INDEX IF NOT EXISTS` is a no-op. The history table's verdicts for
// the lists that re-ran it rest on that.
//
// This reasoning lives here rather than in the .sql files for the reason 002's
// note above gives.
const SQL_004_SQLITE: &str = include_str!("004_user_roles_unique.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_004_POSTGRES: &str = include_str!("004_user_roles_unique.postgres.sql");
// 005 gives an append-only WRAP grant a column of its own.
//
// `wrap_grants` used to spell append-only as `write = 2`. A binary built
// before append grants existed decodes `write` as a flag — any non-zero value
// is read-write — so rolling back past that release, after an append row had
// been written, would have turned every append-only grant into a read-write
// one. Stored as `write = 0, append = 1`, the same row reads as read-only to
// such a binary, which knows nothing of the new column: a rollback narrows the
// grant instead of widening it. `platform_state::wrap_grants` is the one codec
// for the pair, and refuses a row that sets both.
//
// The `UPDATE` moves any `write = 2` row onto the new form. No shipped writer
// produced one — the permissions form offers read-only and read-write — so on
// a real deployment it normally finds nothing; it is there for a row written
// by hand (the admin SQL explorer), so none is left in a spelling the codec
// now refuses.
//
// The ledger runs it once, in one transaction. Before the ledger it was
// re-run with the rest of admin's list (see 004's note): the old runner took
// the second `ADD COLUMN`'s duplicate-column error as applied, and once no
// row holds `write = 2` the `UPDATE` matches nothing.
//
// This reasoning lives here rather than in the .sql files for the reason 002's
// note above gives.
const SQL_005_SQLITE: &str = include_str!("005_wrap_grants_append_column.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_005_POSTGRES: &str = include_str!("005_wrap_grants_append_column.postgres.sql");

// 006 gives a storage access its duration as a column of its own.
//
// The storage block wrote how long an access took into the status text
// ("OK (10ms)"), so the logs page could only show the two run together or
// parse one out of the other. The column is nullable: a request refused
// before it reached a backend took no measurable time, and rows written
// before this migration keep their old status text with no duration.
//
// The ledger runs it once, in one transaction. Before the ledger the old
// runner re-ran it with admin's list and took the second `ADD COLUMN`'s
// duplicate-column error as applied.
//
// This reasoning lives here rather than in the .sql files for the reason 002's
// note above gives.
const SQL_006_SQLITE: &str = include_str!("006_storage_access_logs_duration.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_006_POSTGRES: &str = include_str!("006_storage_access_logs_duration.postgres.sql");

// 007 names every logged request by its HTTP method.
//
// Rows used to store the router action (`retrieve`, `create`, `update`,
// `delete`, `execute`); the pipeline now stores the method from the request
// head (`GET`, `POST`, …). Left as they were, the network page listed
// `RETRIEVE /x` beside `GET /x` as two routes, and a route's detail, matched
// on the method, found only one of them.
//
// `create` is exactly `POST` and `delete` exactly `DELETE`, and are
// rewritten. `retrieve` stood for `GET` and `HEAD`; it becomes `GET`, so an
// old `HEAD` reads as a `GET` — acceptable for diagnostic rows, and the only
// reading the row allows. `update` stood for `PUT` and `PATCH`, and `execute`
// for every other method (`OPTIONS`, `TRACE`, anything unrecognised): no row
// says which, so those rows are deleted rather than guessed at. They are
// diagnostics, on deployments with no production users yet.
//
// The ledger runs it once, in one transaction. Before the ledger it was
// re-run with admin's list: once no row holds an action name, every
// statement matches nothing, so a re-run is a scan of `method` that changes
// nothing.
//
// This reasoning lives here rather than in the .sql files for the reason 002's
// note above gives.
const SQL_007_SQLITE: &str = include_str!("007_request_logs_http_method.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_007_POSTGRES: &str = include_str!("007_request_logs_http_method.postgres.sql");

// 008 stores the block each logged request was addressed to.
//
// The network page groups and totals routes by block, and pages them, in
// SQL; that needs the block as a column rather than a prefix of `path`. New
// rows get it from `request_logs::NewRequestLog::to_data`
// (`request_logs::owning_block`); the `UPDATE` gives existing rows the same
// value — the segment after `/b/`, up to the next `/` — and leaves every other
// path (`/`, the unmatched-route label) at `''`.
//
// The ledger runs it once, in one transaction. Before the ledger it was
// re-run with admin's list: the old runner took the second `ADD COLUMN`'s
// duplicate-column error as applied, the `UPDATE` only touches `/b/` rows
// still at `''`, and the index is `IF NOT EXISTS`.
//
// This reasoning lives here rather than in the .sql files for the reason 002's
// note above gives.
const SQL_008_SQLITE: &str = include_str!("008_request_logs_block.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_008_POSTGRES: &str = include_str!("008_request_logs_block.postgres.sql");

// 009 retires `block_settings.blessed_hash`: every database ends with no
// such column, whichever `001` built the table.
//
// Builds before #145 gated a pending list with `should_apply = is_fresh ||
// run_requested || blessed_hash == code_hash`, and on success wrote
// `current_hash = blessed_hash = code_hash`. #145 removed the column from
// `001` and stopped reading or writing it, so two populations share admin's
// 2026-10-07 record (`84c038a6` on SQLite): databases created before #145,
// whose `001` made the column and kept it, and databases created since,
// which never had it. No record tells them apart, so the file's result must
// not depend on which it meets:
//
// - **SQLite / D1** rebuild the table from the columns every database has,
//   copied by name: create `_v2` with today's columns, copy the rows, drop
//   the old table with whatever column it held, rename `_v2`, recreate the
//   unique index on `block_name`. Five statements, one transaction (on D1,
//   one `db.batch()`).
// - **PostgreSQL** says `DROP COLUMN IF EXISTS`. The `IF EXISTS` is
//   declarative, for the same two populations, not a tolerated error.
//
// The file as first shipped (2026-10-15, `56ab1250`) added the column where
// it was missing and emptied it, relying on the old runner's tolerance of a
// duplicate `ADD COLUMN`, which no transaction can give. The ledger release
// replaced its text before any database recorded it in a ledger; a database
// that ran the first text (`0f4bad57`) is marked by the history table as not
// having applied 009, so today's text runs there too and drops the column.
// The basename stays: it is the file's key in the ledger.
//
// What the column was for: with it emptied, a pre-#145 version still serving
// (a Cloudflare version not yet replaced, or a rollback) found `blessed_hash
// != code_hash` and reported drift instead of re-applying its whole list.
// Dropped, that version's `Init` fails instead (its upsert names the
// column), which stops it the same way. No deployment runs a pre-#145
// version, so this costs no live case.
//
// **How the rebuilt table reaches the KV row cache on Cloudflare.** A
// version's migration state is read from `cfg:v1:block_settings:__all__`, the
// cached whole table (24 h TTL, not version-scoped). The rebuild is raw DDL
// that invalidates nothing, but admin's stamp of its list, which
// `migration_helper::apply_pending` writes once 009 and its ledger row are
// in, is a typed write to this table and drops the key
// (`cache_key::invalidate_keys` emits the all-rows key for it); the deploy
// funnel's later reads through the cache rewrite it from D1. `kv_cached_db`'s
// `admin_migration_009_tests` holds the first.
//
// Covered on both dialects by `impresspress-server/tests/boot_migrations.rs`
// (both populations, and the first text's database, through a native boot),
// and by the history convergence tests.
//
// This reasoning lives here rather than in the .sql files for the reason 002's
// note above gives.
const SQL_009_SQLITE: &str = include_str!("009_block_settings_clear_blessed_hash.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_009_POSTGRES: &str = include_str!("009_block_settings_clear_blessed_hash.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
/// Order here is the apply order.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_admin_schema", SQL_001_SQLITE),
    (VARIABLES_BLOCK_COLUMN, SQL_002_SQLITE),
    ("003_block_settings_seed_hash", SQL_003_SQLITE),
    (USER_ROLES_UNIQUE, SQL_004_SQLITE),
    (WRAP_GRANTS_APPEND_COLUMN, SQL_005_SQLITE),
    ("006_storage_access_logs_duration", SQL_006_SQLITE),
    (REQUEST_LOGS_HTTP_METHOD, SQL_007_SQLITE),
    (REQUEST_LOGS_BLOCK, SQL_008_SQLITE),
    (BLOCK_SETTINGS_CLEAR_BLESSED_HASH, SQL_009_SQLITE),
];

/// Basename of the request-log method rewrite, named once so the migration
/// list and the test that re-applies it cannot drift apart.
pub(crate) const REQUEST_LOGS_HTTP_METHOD: &str = "007_request_logs_http_method";

/// Basename of the request-log `block` column, named once for the same
/// reason.
pub(crate) const REQUEST_LOGS_BLOCK: &str = "008_request_logs_block";

/// Basename of the `blessed_hash` retirement, named once so the migration list and
/// the tests that slice it — here and in other crates — cannot drift apart.
pub const BLOCK_SETTINGS_CLEAR_BLESSED_HASH: &str = "009_block_settings_clear_blessed_hash";

/// Basename of the `variables.block` column + backfill, named once so the
/// migration list and the test that slices it cannot drift apart.
pub(crate) const VARIABLES_BLOCK_COLUMN: &str = "002_variables_block_column";

/// Basename of the grant-uniqueness repair, named once so the migration list
/// and the test that slices it cannot drift apart.
pub(crate) const USER_ROLES_UNIQUE: &str = "004_user_roles_unique";

/// Basename of the append-grant column, named once so the migration list and
/// the test that slices it cannot drift apart.
pub(crate) const WRAP_GRANTS_APPEND_COLUMN: &str = "005_wrap_grants_append_column";

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`] one
/// for one. Selected at runtime by `apply_migrations` and by
/// [`migration_files`] for the native server's pre-build apply. Empty when the `postgres`
/// feature is off — see `files::migrations`'s doc for the rationale
/// (Cloudflare/D1 never selects postgres; don't embed dead SQL).
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[(&str, &str)] = &[
    ("001_admin_schema", SQL_001_POSTGRES),
    (VARIABLES_BLOCK_COLUMN, SQL_002_POSTGRES),
    ("003_block_settings_seed_hash", SQL_003_POSTGRES),
    (USER_ROLES_UNIQUE, SQL_004_POSTGRES),
    (WRAP_GRANTS_APPEND_COLUMN, SQL_005_POSTGRES),
    ("006_storage_access_logs_duration", SQL_006_POSTGRES),
    (REQUEST_LOGS_HTTP_METHOD, SQL_007_POSTGRES),
    (REQUEST_LOGS_BLOCK, SQL_008_POSTGRES),
    (BLOCK_SETTINGS_CLEAR_BLESSED_HASH, SQL_009_POSTGRES),
];
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[(&str, &str)] = &[];

/// This block's migrations as its `lifecycle(Init)` applies them, and as
/// [`crate::blocks::all_block_migrations`] lists them: both dialects' files
/// and both dialects' pre-ledger history (`crate::migration_history`).
///
/// Public because the native server applies it *before* the runtime exists
/// (it seeds its variables from admin's tables pre-build), through
/// [`crate::migration_helper::apply_pending_via_service`]; the ledger then
/// tells admin's `Init` it is applied. Cloudflare and the browser need no
/// such step on admin's behalf: their seeders run after `init_block(admin)`
/// (the browser's pre-build pass runs every block's list).
pub const MIGRATIONS: crate::migration_helper::BlockMigrations =
    crate::migration_helper::BlockMigrations {
        block: super::ADMIN_BLOCK_ID,
        sqlite: SQLITE_MIGRATIONS,
        postgres: POSTGRES_MIGRATIONS,
        sqlite_history: crate::migration_history::admin::SQLITE,
        postgres_history: crate::migration_history::admin::POSTGRES,
    };

/// Apply the admin schema through the shared migration-state gate.
///
/// Production no longer calls this: `AdminBlock::lifecycle(Init)` applies the
/// admin schema via [`crate::migration_helper::lifecycle_init`]. This thin
/// forwarder exists for the `tests/admin/*` + `tests/auth/*` integration
/// suites, which bootstrap `impresspress__admin__block_settings` (the tracking
/// table every other block's gate upserts into) in their fixtures —
/// test-fixture setup is an explicit exception to the no-raw-migration-runner
/// rule (CLAUDE.md).
pub async fn apply(
    ctx: &dyn wafer_run::context::Context,
) -> Result<(), crate::migration_helper::MigrationError> {
    crate::migration_helper::apply_migrations(ctx, &MIGRATIONS).await
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "postgres")]
    use super::{
        SQL_001_POSTGRES, SQL_002_POSTGRES, SQL_003_POSTGRES, SQL_004_POSTGRES, SQL_005_POSTGRES,
    };
    use super::{SQL_001_SQLITE, SQL_002_SQLITE, SQL_003_SQLITE, SQL_004_SQLITE, SQL_005_SQLITE};

    #[test]
    fn sqlite_migrations_contain_expected_ddl() {
        // 001 schema (variables UNIQUE INDEX)
        assert!(SQL_001_SQLITE.contains("impresspress__admin__variables_key_uniq"));
        // 002 follow-up (ALTER TABLE ADD COLUMN block + index)
        assert!(SQL_002_SQLITE.contains("ADD COLUMN block"));
        assert!(SQL_002_SQLITE.contains("impresspress__admin__variables_block_idx"));
        // 003 follow-up (ADD COLUMN seed_defaults_hash)
        assert!(SQL_003_SQLITE.contains("ADD COLUMN seed_defaults_hash"));
        // 004 grant uniqueness
        assert!(SQL_004_SQLITE.contains("CREATE UNIQUE INDEX IF NOT EXISTS"));
        // 005 append-grant column
        assert!(SQL_005_SQLITE.contains("ADD COLUMN append"));
    }

    #[test]
    #[cfg(feature = "postgres")]
    fn postgres_migrations_contain_expected_ddl() {
        assert!(SQL_001_POSTGRES.contains("impresspress__admin__variables_key_uniq"));
        assert!(SQL_002_POSTGRES.contains("ADD COLUMN"));
        assert!(SQL_003_POSTGRES.contains("seed_defaults_hash"));
        assert!(SQL_004_POSTGRES.contains("CREATE UNIQUE INDEX IF NOT EXISTS"));
        assert!(SQL_005_POSTGRES.contains("ADD COLUMN IF NOT EXISTS append"));
    }
}

#[cfg(test)]
mod user_roles_unique_tests {
    //! What `004_user_roles_unique` does to a deployment that already holds a
    //! repeated grant — the only database its `DELETE` has anything to do on.
    //! Every other fixture applies 001-004 together against an empty table,
    //! so an error in the repair would go unnoticed until a real deployment
    //! tried to upgrade, where it fails the whole batch and re-fails on every
    //! later boot.

    use wafer_core::clients::database as db;

    use super::{SQLITE_MIGRATIONS, USER_ROLES_UNIQUE};
    use crate::{
        migration_helper,
        platform_state::user_roles::{self, UserRoleRow},
        test_support::TestContext,
    };

    const ADMIN: &str = "impresspress/admin";

    /// The migrations before 004, sliced out of the shipped list by name so
    /// an unwired 004 cannot pass as applied.
    fn before_004() -> &'static [(&'static str, &'static str)] {
        let at = SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == USER_ROLES_UNIQUE)
            .expect("004 is wired into SQLITE_MIGRATIONS");
        &SQLITE_MIGRATIONS[..at]
    }

    fn grant(id: &str, user_id: &str, role: &str, created_at: &str) -> UserRoleRow {
        UserRoleRow {
            id: id.to_string(),
            user_id: user_id.to_string(),
            role: role.to_string(),
            assigned_at: Some(created_at.to_string()),
            assigned_by: String::new(),
            created_at: created_at.to_string(),
            updated_at: created_at.to_string(),
        }
    }

    #[tokio::test]
    async fn migration_004_collapses_repeated_grants_and_the_index_then_refuses_one() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        migration_helper::apply_pending(&ctx, ADMIN, before_004(), &[])
            .await
            .expect("001-003 apply");

        // What two racing bootstrap-admin logins left behind, plus a pair
        // tied on `created_at` so the `id` tie-break is what decides, plus a
        // grant that repeats nothing and must be left alone.
        for row in [
            grant("ur_b_first", "alice", "admin", "2026-01-01T00:00:00Z"),
            grant("ur_a_second", "alice", "admin", "2026-02-01T00:00:00Z"),
            grant("ur_tie_b", "bob", "editor", "2026-03-01T00:00:00Z"),
            grant("ur_tie_a", "bob", "editor", "2026-03-01T00:00:00Z"),
            grant("ur_other", "bob", "admin", "2026-04-01T00:00:00Z"),
        ] {
            // Straight to the table: the fixture plants twins, which the
            // only writer (`user_roles::assign`) now refuses to make.
            db::create(&ctx, user_roles::TABLE, row.to_data())
                .await
                .expect("seed the repeated grant");
        }

        ctx.set_config(migration_helper::RUN_MIGRATIONS_KEY, "1");
        migration_helper::apply_pending(&ctx, ADMIN, SQLITE_MIGRATIONS, &[])
            .await
            .expect("004 applies to a database holding repeated grants");

        let mut ids: Vec<String> = user_roles::list_all(&ctx)
            .await
            .expect("list grants")
            .rows
            .into_iter()
            .map(|row| row.id)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["ur_b_first", "ur_other", "ur_tie_a"],
            "each repeated grant collapses onto one survivor, and nothing \
             else is touched"
        );

        assert!(
            db::create(
                &ctx,
                user_roles::TABLE,
                grant("ur_again", "alice", "admin", "2026-05-01T00:00:00Z").to_data(),
            )
            .await
            .is_err(),
            "the index is in place, so the grant cannot be repeated again"
        );
    }
}

#[cfg(test)]
mod variables_block_column_tests {
    //! What `002_variables_block_column` does to a deployment whose
    //! `variables` table already holds rows — the only database its backfill
    //! `UPDATE` has anything to do on. Every other fixture applies the whole
    //! list against an empty table, so a backfill that derived the wrong
    //! prefix, or none at all, would pass there and first show up on a real
    //! upgrade as block-scoped variables grouped under no block.
    //!
    //! Rows are written and read back through `platform_state::variables`,
    //! the module that owns the table, so what is asserted is the `block`
    //! the config loader sees.

    use wafer_core::clients::database as db;

    use super::{SQLITE_MIGRATIONS, VARIABLES_BLOCK_COLUMN};
    use crate::{
        migration_helper,
        platform_state::variables::{self, NewVariable},
        test_support::TestContext,
    };

    const ADMIN: &str = "impresspress/admin";

    /// The shipped migrations before 002 (`through: false`) or up to and
    /// including it (`through: true`), sliced out of the list by name so an
    /// unwired 002 cannot pass as applied.
    fn up_to_002(through: bool) -> &'static [(&'static str, &'static str)] {
        let at = SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == VARIABLES_BLOCK_COLUMN)
            .expect("002 is wired into SQLITE_MIGRATIONS");
        let end = if through { at + 1 } else { at };
        &SQLITE_MIGRATIONS[..end]
    }

    /// The key shapes the backfill has to tell apart, with the `block` each
    /// must end up with: two block-scoped keys (`{ORG}__{BLOCK}__NAME`), one
    /// with a single `__` (the shape of the `WAFER_RUN_SHARED__` namespace),
    /// one with none, and two whose runs of underscores make the second
    /// separator easy to misplace. Invented names, so that no declared key is
    /// respelled here.
    const CASES: [(&str, Option<&str>); 6] = [
        ("ACME__WIDGETS__API_TOKEN", Some("ACME__WIDGETS")),
        ("ACME__STORE__DB_PATH", Some("ACME__STORE")),
        ("ACME_SHARED__SITE_TITLE", None),
        ("NO_DOUBLE_UNDERSCORE", None),
        ("ACME____EMPTY_BLOCK", Some("ACME__")),
        ("ACME___ODD__NAME", Some("ACME___ODD")),
    ];

    /// A row as a pre-002 build wrote it: no `block`, which 001 has no
    /// column for.
    async fn seed_pre_002(ctx: &TestContext) {
        for (key, _) in CASES {
            variables::insert(
                ctx,
                NewVariable {
                    key: key.to_string(),
                    value: "v".to_string(),
                    name: String::new(),
                    description: String::new(),
                    warning: String::new(),
                    sensitive: false,
                    updated_by: String::new(),
                    block: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("seed {key}: {e}"));
        }
    }

    /// Every row carries the block its key names — and the same block the
    /// Rust derivation gives a row seeded after the upgrade, which
    /// `config_vars::key_block_prefix` promises is byte-for-byte this SQL.
    async fn assert_backfilled(ctx: &TestContext) {
        for (key, expected) in CASES {
            let row = variables::get_by_key(ctx, key)
                .await
                .expect("read variable")
                .unwrap_or_else(|| panic!("row {key} is gone"));
            assert_eq!(row.block.as_deref(), expected, "block for {key}");
            assert_eq!(
                row.block,
                variables::block_for_key(key),
                "the backfill and block_for_key disagree on {key}"
            );
        }
    }

    #[tokio::test]
    async fn migration_002_adds_block_column_and_index() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        migration_helper::apply_pending(&ctx, ADMIN, SQLITE_MIGRATIONS, &[])
            .await
            .expect("apply migrations");

        let cols = db::query_raw(
            &ctx,
            "PRAGMA table_info(impresspress__admin__variables)",
            &[],
        )
        .await
        .expect("pragma table_info");
        let col_names: Vec<String> = cols
            .iter()
            .filter_map(|r| {
                r.data
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .collect();
        assert!(
            col_names.contains(&"block".to_string()),
            "expected `block` column in variables, got: {col_names:?}"
        );

        let idx = db::query_raw(
            &ctx,
            "SELECT name FROM sqlite_master WHERE type='index' \
             AND name='impresspress__admin__variables_block_idx'",
            &[],
        )
        .await
        .expect("query sqlite_master for index");
        assert_eq!(idx.len(), 1, "expected the block index to exist");
    }

    /// A deployment that applied 001 alone, then rows, then 002: the
    /// backfill in 002 is what populates `block`, not anything the test runs
    /// itself.
    ///
    /// No database this build upgrades is at that state: every admin list
    /// that stopped at 001 is from before 2026-07-13, which the history table
    /// refuses (`crate::migration_history::admin`), and the ledger runner
    /// applies admin's list whole. So the deployment is built, and 002 run,
    /// by the runner from before the ledger
    /// (`migration_helper::legacy_runner`), which is how 002 met populated
    /// tables when it shipped.
    async fn upgrade_from_001(ctx: &TestContext) {
        let db = ctx.database_service();
        migration_helper::legacy_runner::apply(&db, up_to_002(false))
            .await
            .expect("001 applies");
        seed_pre_002(ctx).await;
        migration_helper::legacy_runner::apply(&db, &up_to_002(true)[1..])
            .await
            .expect("002 applies to a database holding variables");
    }

    #[tokio::test]
    async fn migration_002_backfills_block_on_rows_written_before_it() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        upgrade_from_001(&ctx).await;

        assert_backfilled(&ctx).await;
    }
}
