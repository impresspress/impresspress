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
// The .sql files are deliberately NOT edited to say so. Every database that
// applied a shipped migration records the hash of its statements, and a
// database from before the per-file ledger is matched by the hash of its
// files' whole text, comments included, so retouching a `--` line would turn
// every such deployment's adoption into the legacy replay. See
// `crate::migration_helper`'s "A shipped file's statements are immutable",
// which prescribes exactly this note. It is also why
// `scripts/check-doc-pointers.sh` skips `migrations/*.sql` — a guard cannot
// ask for an edit the runtime punishes. This file is not skipped.
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
// Re-runnable, as every frozen migration must be (`migration_helper`'s
// "Frozen files"): a frozen file that failed part-way runs again from its
// start, and the legacy replay runs admin's frozen list once more. Once the
// index exists the `DELETE` finds nothing and the `CREATE UNIQUE INDEX IF NOT
// EXISTS` is a no-op.
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
// Re-runnable, like the rest of admin's set (see 004's note): a second
// `ADD COLUMN` is swallowed as a duplicate column by the statement loop
// frozen files run through, and once no row holds `write = 2` the `UPDATE`
// matches nothing.
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
// Re-runnable, like the rest of admin's set: a second `ADD COLUMN` is
// swallowed as a duplicate column by the statement loop frozen files run
// through.
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
// Re-runnable: once no row holds an action name, every statement matches
// nothing, so a later re-run of admin's frozen list (the legacy replay) is a
// scan of `method` that changes nothing.
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
// Re-runnable, as a later re-run of admin's frozen list needs: a second
// `ADD COLUMN` is swallowed as a duplicate column by the statement loop
// frozen files run through, the `UPDATE` only touches `/b/` rows still at
// `''`, and the index is `IF NOT EXISTS`.
//
// This reasoning lives here rather than in the .sql files for the reason 002's
// note above gives.
const SQL_008_SQLITE: &str = include_str!("008_request_logs_block.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_008_POSTGRES: &str = include_str!("008_request_logs_block.postgres.sql");

// 009 empties `block_settings.blessed_hash` on every row, so a build from
// before the column was retired that is still serving reports schema drift
// instead of silently re-applying its own migrations.
//
// Builds before #145 gated a pending list with `should_apply = is_fresh ||
// run_requested || blessed_hash == code_hash`, and on success wrote
// `current_hash = blessed_hash = code_hash`. This build neither reads nor
// writes the column, so it kept the value the last pre-#145 deploy left. On
// Cloudflare the version being replaced keeps serving until the new one is
// promoted — and for good when a deploy fails — and once this build has
// recorded a list of its own, the old version's next cold build re-applies
// its whole list for every block whose stale `blessed_hash` still equals its
// own code hash: it overwrites the new version's `current_hash` records,
// bumps the KV config-version stamp (which leaves the candidate's prepared
// plan stale mid-deploy) and re-runs old data repairs; had auth's list
// changed, it would drop the refresh-token and session tables and sign every
// user out. That happens once per divergence when the re-apply succeeds (it
// re-records the block), and on every cold build while it keeps failing part
// way, as products' did at the D1 statement budget. With the column empty,
// the same old gate finds `blessed_hash != code_hash`, logs `schema drift;
// redeploy with --run-migrations` and boots on the schema it finds.
//
// Every row, not one block's: any block whose list a pre-#145 version ships
// is exposed. The column is emptied, not dropped: the old `write_state`
// names it in its upsert, so without it the old version's every `Init` would
// fail. It is added where it is missing, because #145 removed it from 001: a
// database created since never had it, and an `UPDATE` naming it would fail
// there. On SQLite/D1 that `ADD COLUMN` is swallowed as a duplicate column
// where the column exists; the PostgreSQL file says `ADD COLUMN IF NOT
// EXISTS`. Every database ends with the same schema, holding a column
// nothing in this build reads.
//
// **How the cleared rows reach the old version's KV row cache.** The old
// version reads its migration state from `cfg:v1:block_settings:__all__`,
// the cached whole table (24 h TTL, not version-scoped). On Cloudflare
// `db::ddl` reaches `KvCachedD1DatabaseService::exec_raw`, which passes this
// `UPDATE` straight to D1 and invalidates nothing. Two later steps of the
// deploy funnel bring the key in line with D1, and either alone would:
//
// - admin's stamp of its list, which `migration_helper::apply_pending`
//   writes once 009 has run and its ledger row is in (the ledger row is a raw
//   insert into a table the cache never holds), is a typed write to this
//   table, and drops the key (`cache_key::invalidate_keys` emits the all-rows
//   key for it);
// - the funnel reads the table back through the cache in read-through mode —
//   `CfDeployBootHooks`' `block_settings::load_and_seed` right after admin's
//   `Init` (`builder::boot` step 4), and `/_deploy/prepare`'s closing
//   `block_settings::load` — and each read rewrites the key from D1 whenever
//   it differs.
//
// `kv_cached_db`'s
// `admin_migration_009_tests::applying_admins_list_drops_the_all_rows_key_after_009`
// holds the first; `boot_hooks`'
// `the_deploy_funnel_leaves_the_all_rows_key_holding_the_cleared_rows` holds
// the outcome: after admin's `Init` and the seed step, the cached table has
// every `blessed_hash` empty.
//
// What it does not do: it protects against an old version only once a build
// carrying it has applied it — a live old version serving before that keeps
// re-applying. A rollback with migrations to a pre-#145 version re-blesses
// rows, and since the per-file ledger a roll-forward finds 009 recorded and
// does not run it again, so those rows stay blessed. So does a row an old
// version re-blesses after 009 ran because its cold build read the rows from
// before (D1 mid-prepare, or a colo's KV copy, which is eventually
// consistent). Keeping traffic off the old version during a deploy is what
// closes both.
//
// Re-runnable: the `ADD COLUMN` is a duplicate after the first run — and on
// a database created before #145, whose old 001 made the column, on the
// first run too, which is why 009 relies on the frozen-file loop
// (`migration_helper`'s "Frozen files") — and the `UPDATE` matches no row.
// Covered on both dialects by
// `impresspress-native/tests/admin_blessed_hash.rs`.
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

/// Basename of the `blessed_hash` clear, named once so the migration list and
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
    crate::migration_helper::apply_migrations(
        ctx,
        "impresspress/admin",
        SQLITE_MIGRATIONS,
        POSTGRES_MIGRATIONS,
    )
    .await
}

/// The admin migration list for the given `db_type`, in apply order — the
/// very list the block's `lifecycle_init` applies. `"postgres"`
/// (case-insensitive) selects the postgres dialect; everything else selects
/// SQLite, matching [`crate::migration_helper::db_backend`].
///
/// Exposed so the native server can apply it *before* the runtime exists (it
/// seeds the JWT secret + block_settings pre-build), through
/// [`crate::migration_helper::apply_pending_via_service`], which records it
/// as applied so admin's `Init` then skips it. Cloudflare and browser don't
/// need this — their seeders run after `init_block(admin)` has applied it.
pub fn migration_files(db_type: &str) -> &'static [(&'static str, &'static str)] {
    if db_type.eq_ignore_ascii_case("postgres") {
        POSTGRES_MIGRATIONS
    } else {
        SQLITE_MIGRATIONS
    }
}

/// The SQL of [`migration_files`], for test fixtures that build the admin
/// schema with the untracked
/// [`crate::migration_helper::apply_ddl_via_service`].
pub fn ddl_files(db_type: &str) -> Vec<&'static str> {
    migration_files(db_type)
        .iter()
        .map(|(_, sql)| *sql)
        .collect()
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
        migration_helper::apply_migrations(&ctx, ADMIN, before_004(), &[])
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
        migration_helper::apply_migrations(&ctx, ADMIN, SQLITE_MIGRATIONS, &[])
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
        migration_helper::apply_migrations(&ctx, ADMIN, SQLITE_MIGRATIONS, &[])
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

    /// A deployment from before the per-file ledger that applied 001 alone
    /// and recorded that list's hash, then rows, then the shipped list the
    /// way an upgrading deployment's first migrating boot applies it: 001 is
    /// adopted, and the backfill in 002 is what populates `block`, not
    /// anything the test runs itself.
    async fn upgrade_from_001(ctx: &mut TestContext) {
        migration_helper::apply_ddl_via_service(
            &ctx.database_service(),
            &up_to_002(false)
                .iter()
                .map(|(_, sql)| *sql)
                .collect::<Vec<_>>(),
        )
        .await
        .expect("001 applies");
        seed_pre_002(ctx).await;
        ctx.set_config(migration_helper::RUN_MIGRATIONS_KEY, "1");
        ctx.set_config(
            crate::features::BLOCK_SETTINGS_CONFIG_KEY,
            &serde_json::json!({
                ADMIN: { "migration": {
                    "current_hash": migration_helper::migration_set_hash(up_to_002(false))
                } }
            })
            .to_string(),
        );
        migration_helper::apply_migrations(ctx, ADMIN, SQLITE_MIGRATIONS, &[])
            .await
            .expect("002 applies to a database holding variables");
    }

    #[tokio::test]
    async fn migration_002_backfills_block_on_rows_written_before_it() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        upgrade_from_001(&mut ctx).await;

        assert_backfilled(&ctx).await;
    }

    /// 002 is in the frozen set the legacy replay runs once more over a
    /// database it may already have run on. Its `ADD COLUMN` then meets a
    /// column that already exists and its backfill meets rows it already
    /// filled; both must pass through the replay's untracked statement loop
    /// without failing it or disturbing a derived value.
    #[tokio::test]
    async fn migration_002_survives_a_re_run_of_the_whole_list() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        upgrade_from_001(&mut ctx).await;

        migration_helper::apply_ddl_via_service(
            &ctx.database_service(),
            &SQLITE_MIGRATIONS
                .iter()
                .map(|(_, sql)| *sql)
                .collect::<Vec<_>>(),
        )
        .await
        .expect("re-running the whole list succeeds");

        assert_backfilled(&ctx).await;
    }
}
