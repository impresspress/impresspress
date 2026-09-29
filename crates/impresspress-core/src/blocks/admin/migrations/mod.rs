//! Admin block migrations. Applied from the block's `Init` lifecycle via
//! [`crate::migration_helper::lifecycle_init`].
//!
//! SQL files are embedded with `include_str!`. Backend dispatch + concat +
//! the `current_hash` / `blessed_hash` / `IMPRESSPRESS_RUN_MIGRATIONS` gate
//! all live in [`crate::migration_helper::apply_migrations`]. Earlier
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
// The .sql files are deliberately NOT edited to say so. A shipped migration
// is hash-addressed over its whole text, comments included, so retouching a
// `--` line logs `schema drift` on every boot of every deployment that
// already applied it and needs a `--run-migrations` redeploy to clear. See
// `crate::migration_helper`'s "A shipped .sql file is immutable, comments
// included", which prescribes exactly this note. It is also why
// `scripts/check-doc-pointers.sh` skips `migrations/*.sql` — a guard cannot
// ask for an edit the runtime punishes. This file is not skipped.
const SQL_002_SQLITE: &str = include_str!("002_variables_block_column.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_002_POSTGRES: &str = include_str!("002_variables_block_column.postgres.sql");
// 003 adds `block_settings.seed_defaults_hash`, which lets
// `crate::blocks::admin::settings::seed_defaults` skip its bulk `variables`
// read when the declared shared config has not changed since the last seed.
// The gate is the same shape `crate::migration_helper::apply_if_blessed` uses
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
// `platform_state::user_roles::assign` is a read-then-insert, and the login
// path runs it concurrently: `auth::helpers::TokenGrant::resolve` grants `admin`
// on every login of the bootstrap-admin address that does not already hold
// it, so two logins racing on a fresh account could both find no grant and
// both insert one. A duplicate is not harmless. `get_user_roles` folds the
// twins into one role, while `iam::handle_remove_role` deletes ONE row by id —
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
// Where it runs: native applies it before every boot, and a Cloudflare deploy
// applies it through `/_deploy/init`. A browser install applies it on the
// first boot of a bundle that carries it: the browser sets
// `IMPRESSPRESS_RUN_MIGRATIONS` on every boot, since loading a new bundle is
// its deploy, and the gate then re-runs admin's set once because its hash
// changed.
//
// Re-runnable, which admin's migrations must be twice over: the gate re-runs
// the whole concatenated set from 001 whenever its hash changes, and the
// native CLI runs `ddl_files` ungated on every boot, before the wafer exists
// (`migration_helper::apply_ddl_via_service`). Once the index exists the
// `DELETE` finds nothing and the `CREATE UNIQUE INDEX IF NOT EXISTS` is a
// no-op.
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
// `ADD COLUMN` is swallowed as a duplicate column by both migration runners,
// and once no row holds `write = 2` the `UPDATE` matches nothing.
//
// This reasoning lives here rather than in the .sql files for the reason 002's
// note above gives.
const SQL_005_SQLITE: &str = include_str!("005_wrap_grants_append_column.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_005_POSTGRES: &str = include_str!("005_wrap_grants_append_column.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
/// Order here is the apply order.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_admin_schema", SQL_001_SQLITE),
    (VARIABLES_BLOCK_COLUMN, SQL_002_SQLITE),
    ("003_block_settings_seed_hash", SQL_003_SQLITE),
    (USER_ROLES_UNIQUE, SQL_004_SQLITE),
    (WRAP_GRANTS_APPEND_COLUMN, SQL_005_SQLITE),
];

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
/// for one. Selected at runtime by `apply_migrations` and reused by
/// [`ddl_files`] for the pre-wafer native CLI path. Empty when the `postgres`
/// feature is off — see `files::migrations`'s doc for the rationale
/// (Cloudflare/D1 never selects postgres; don't embed dead SQL).
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[
    SQL_001_POSTGRES,
    SQL_002_POSTGRES,
    SQL_003_POSTGRES,
    SQL_004_POSTGRES,
    SQL_005_POSTGRES,
];
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[];

/// Apply the admin schema through the shared migration-state gate.
///
/// Production no longer calls this: `AdminBlock::lifecycle(Init)` applies the
/// admin schema via [`crate::migration_helper::lifecycle_init`]. This thin
/// forwarder exists for the `tests/admin/*` + `tests/auth/*` integration
/// suites, which bootstrap `impresspress__admin__block_settings` (the tracking
/// table every other block's gate upserts into) in their fixtures —
/// test-fixture setup is an explicit exception to the no-raw-migration-runner
/// rule (CLAUDE.md).
pub async fn apply(ctx: &dyn wafer_run::context::Context) -> Result<(), String> {
    let sqlite: Vec<&str> = SQLITE_MIGRATIONS.iter().map(|(_, sql)| *sql).collect();
    crate::migration_helper::apply_migrations(
        ctx,
        "impresspress/admin",
        &sqlite,
        POSTGRES_MIGRATIONS,
    )
    .await
}

/// The admin migration SQL files for the given `db_type`, in apply order —
/// the same constants the gated `lifecycle_init` runner feeds. `"postgres"`
/// (case-insensitive) selects the postgres dialect; everything else selects
/// SQLite, matching [`crate::migration_helper::db_backend`].
///
/// Exposed so the native CLI can create the admin tables *before* the wafer
/// exists (it seeds the JWT secret + block_settings pre-build), via
/// [`crate::migration_helper::apply_ddl_via_service`]. Cloudflare and browser
/// don't need this — their seeders run after the gated apply has already
/// created the tables at `init_block(admin)`.
pub fn ddl_files(db_type: &str) -> &'static [&'static str] {
    if db_type.eq_ignore_ascii_case("postgres") {
        POSTGRES_MIGRATIONS
    } else {
        &[
            SQL_001_SQLITE,
            SQL_002_SQLITE,
            SQL_003_SQLITE,
            SQL_004_SQLITE,
            SQL_005_SQLITE,
        ]
    }
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
    fn before_004() -> Vec<&'static str> {
        let at = SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == USER_ROLES_UNIQUE)
            .expect("004 is wired into SQLITE_MIGRATIONS");
        SQLITE_MIGRATIONS[..at]
            .iter()
            .map(|(_, sql)| *sql)
            .collect()
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
        migration_helper::apply_migrations(&ctx, ADMIN, &before_004(), &[])
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
        let all: Vec<&str> = SQLITE_MIGRATIONS.iter().map(|(_, sql)| *sql).collect();
        migration_helper::apply_migrations(&ctx, ADMIN, &all, &[])
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
    fn up_to_002(through: bool) -> Vec<&'static str> {
        let at = SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == VARIABLES_BLOCK_COLUMN)
            .expect("002 is wired into SQLITE_MIGRATIONS");
        let end = if through { at + 1 } else { at };
        SQLITE_MIGRATIONS[..end]
            .iter()
            .map(|(_, sql)| *sql)
            .collect()
    }

    fn all() -> Vec<&'static str> {
        SQLITE_MIGRATIONS.iter().map(|(_, sql)| *sql).collect()
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
        migration_helper::apply_migrations(&ctx, ADMIN, &all(), &[])
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

    /// 001 alone, then rows, then the shipped list the way an operator
    /// upgrading with `--run-migrations` applies it: the backfill in 002 is
    /// what populates `block`, not anything the test runs itself.
    #[tokio::test]
    async fn migration_002_backfills_block_on_rows_written_before_it() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        migration_helper::apply_migrations(&ctx, ADMIN, &up_to_002(false), &[])
            .await
            .expect("001 applies");
        seed_pre_002(&ctx).await;

        ctx.set_config(migration_helper::RUN_MIGRATIONS_KEY, "1");
        migration_helper::apply_migrations(&ctx, ADMIN, &all(), &[])
            .await
            .expect("002 applies to a database holding variables");

        assert_backfilled(&ctx).await;
    }

    /// The gate re-runs the whole concatenated list from 001 whenever its
    /// hash changes — here, a deployment that stopped at 002 upgrading to the
    /// full list. 002's `ADD COLUMN` then meets a column that already exists
    /// and its backfill meets rows it already filled; both must pass through
    /// without failing the batch or disturbing a derived value.
    #[tokio::test]
    async fn migration_002_survives_a_re_run_of_the_whole_list() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        migration_helper::apply_migrations(&ctx, ADMIN, &up_to_002(false), &[])
            .await
            .expect("001 applies");
        seed_pre_002(&ctx).await;
        ctx.set_config(migration_helper::RUN_MIGRATIONS_KEY, "1");
        migration_helper::apply_migrations(&ctx, ADMIN, &up_to_002(true), &[])
            .await
            .expect("001-002 apply");

        migration_helper::apply_migrations(&ctx, ADMIN, &all(), &[])
            .await
            .expect("re-running 001-002 inside the full list succeeds");

        assert_backfilled(&ctx).await;
    }
}
