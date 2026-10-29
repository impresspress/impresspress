//! Files block migrations. Applied from the block's `Init` lifecycle via
//! [`crate::migration_helper::lifecycle_init`].

const SQL_001_SQLITE: &str = include_str!("001_initial_schema.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_001_POSTGRES: &str = include_str!("001_initial_schema.postgres.sql");
// 002 makes `buckets.name` unique.
//
// A bucket name is not a label: it IS the blob-namespace folder name in
// `wafer-run/storage` (`store::create_folder(name)`, `store::put(name, key)`),
// and `repo::buckets::find_owned` grants access on the `(name, created_by)`
// pair. Without the index a second user could insert a row for a name someone
// else already held — `StorageService::create_folder` is idempotent on every
// backend, so nothing refused the duplicate — and that row gave them
// `find_owned` access to the first owner's folder: list it, read every object
// in it, overwrite them, and on `DELETE /b/storage/api/buckets/{name}` wipe
// the folder outright.
//
// With the index the metadata row is the atomic claim on the name, which is
// why `storage::buckets::handle_create_bucket` inserts the row BEFORE it
// creates the folder and answers 409 when the insert is refused. Creating the
// folder first cannot be made safe: the idempotent `create_folder` succeeds
// against the existing folder, and the compensating `delete_folder` that would
// run when the metadata insert fails would then delete the first owner's data.
//
// Rows that already collide are resolved in favour of the earliest creator
// (`created_at`, `id` as the tie-break so the result does not depend on row
// order): the later duplicates are deleted, which is exactly the access they
// should never have had. Their object-metadata rows are left alone — the blobs
// they name are real, still in the folder, and still charged to whoever
// uploaded them; only the second claim on the folder goes away. `RELEASE.md`
// carries the operator-facing version, including that this costs the losing
// user access to files they are still charged for.
//
// This reasoning lives here rather than in the .sql files because a shipped
// migration is hash-addressed over its whole text, comments included: a rename
// of any Rust item named above would make a comment in the .sql file false and
// uncorrectable without a re-bless. See `crate::migration_helper`'s "A shipped
// .sql file is immutable, comments included".
const SQL_002_SQLITE: &str = include_str!("002_bucket_name_unique.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_002_POSTGRES: &str = include_str!("002_bucket_name_unique.postgres.sql");
const SQL_003_SQLITE: &str = include_str!("003_legacy_share_token_expiry.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_003_POSTGRES: &str = include_str!("003_legacy_share_token_expiry.postgres.sql");
// 004 gives each upload reservation of an object row its own token.
//
// `repo::objects::reserve_upload` takes over an existing row (a stored object
// being replaced, or an orphaned reservation) with a write conditional on the
// row being unchanged since it was read. That condition used to be the row's
// `updated_at` — a timestamp, and the claim's own stamp is a timestamp too, so
// a second upload that read the row before the first one's claim landed could
// find the claim's stamp equal to the one it read (same millisecond, or two
// isolates whose clocks disagree) and take the row as well. `claim_id` is a
// fresh random value per reservation, so the condition is exact, and
// `repo::objects::mark_complete` / `release_reservation` act only on the
// reservation that is still theirs.
//
// Rows written before 004 have a NULL `claim_id`; the take-over condition
// matches that NULL exactly as it matches a token. The column is nullable and
// has no default for that reason: a default would give every old row the same
// value.
//
// The file runs once, recorded in the migration ledger, so it needs no
// re-run safety of its own. (`ADD COLUMN IF NOT EXISTS` on PostgreSQL dates
// from the runner that re-ran whole lists; the file's text is shipped and
// stays.)
//
// Until 004 runs, every reservation fails: taking over a row filters on
// `claim_id`, and an upload's data names it. Every target applies the block's
// pending migrations before it serves a release (a Cloudflare deploy in its
// prepare funnel, a native server at boot, the browser before it builds its
// runtime), so no build runs without it.
const SQL_004_SQLITE: &str = include_str!("004_object_claim_id.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_004_POSTGRES: &str = include_str!("004_object_claim_id.postgres.sql");
// 005 records, on every object row, the storage key its bytes live under.
//
// An upload used to store its bytes at the object's own key, so every upload
// of one key wrote the same blob. The claim token (004) made the ROW exact,
// but not the blob: an upload whose reservation outlived its TTL and was
// taken over could still land its bytes after the upload that took over had
// finished, and overwrite them — leaving that upload's row serving the first
// upload's content. Each reservation now stores its bytes under a key of its
// own (`repo::objects::claim_blob_key`), and the row names the blob it serves
// in `blob_key`; every reader resolves the bytes through the row.
//
// Rows written before 005 have a NULL `blob_key`, and NULL means exactly what
// those rows were written under: the bytes are at the object's own key. No
// row is rewritten and no blob is copied — an old object keeps being served
// from where it is until an upload replaces it, which then deletes it.
//
// The file runs once, recorded in the migration ledger, as 004 does; every
// target applies it before it serves the code that names `blob_key`.
const SQL_005_SQLITE: &str = include_str!("005_object_blob_key.sqlite.sql");
#[cfg(any(feature = "postgres", test))]
const SQL_005_POSTGRES: &str = include_str!("005_object_blob_key.postgres.sql");

// The unused `cloud_quotas.reset_period_days` column.
//
// 001 declares it; no code reads or writes it. Nothing enforces a reset
// period, so `QuotaConfig`, the admin quota PATCH whitelist
// (`cloud::handle_update_quota`) and the published contract do not carry it.
// The column stays because, when this was decided, `DROP COLUMN` was unsafe
// under this block's runner: it re-ran the whole joined migration SQL
// whenever its hash changed and tolerated only a duplicate `ADD COLUMN`, so a
// `DROP COLUMN` would fail with "no such column" on the next re-run on
// SQLite/D1 and fail the block's `Init`. A file added after the per-file
// ledger runs once, so a `DROP COLUMN` there would be safe; nothing touches
// the column, so it has not been worth one. It is
// `NOT NULL DEFAULT 0`, so a row written without it is still accepted.

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
/// Order here is the apply order.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_initial_schema", SQL_001_SQLITE),
    ("002_bucket_name_unique", SQL_002_SQLITE),
    (LEGACY_SHARE_TOKEN_EXPIRY, SQL_003_SQLITE),
    ("004_object_claim_id", SQL_004_SQLITE),
    ("005_object_blob_key", SQL_005_SQLITE),
];

/// Basename of the share-expiry repair, named once so the migration list and
/// the tests that slice it cannot drift apart.
pub(crate) const LEGACY_SHARE_TOKEN_EXPIRY: &str = "003_legacy_share_token_expiry";

/// The PostgreSQL scripts, one per entry in [`SQLITE_MIGRATIONS`] and in the
/// same order.
///
/// Declared under `test` as well as `postgres` so the parity assertion in the
/// share-expiry tests runs in an ordinary `cargo test` build, against the
/// literals a postgres deployment really applies rather than a test-only copy
/// of them.
#[cfg(any(feature = "postgres", test))]
const POSTGRES_MIGRATION_FILES: &[(&str, &str)] = &[
    ("001_initial_schema", SQL_001_POSTGRES),
    ("002_bucket_name_unique", SQL_002_POSTGRES),
    (LEGACY_SHARE_TOKEN_EXPIRY, SQL_003_POSTGRES),
    ("004_object_claim_id", SQL_004_POSTGRES),
    ("005_object_blob_key", SQL_005_POSTGRES),
];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`]. Empty
/// when the `postgres` feature is off — e.g. Cloudflare/D1 never selects the
/// postgres dialect at runtime, so keeping the `.postgres.sql` files out of
/// that build entirely (rather than embedding-then-ignoring them) drops dead
/// SQL bytes from the wasm binary.
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[(&str, &str)] = POSTGRES_MIGRATION_FILES;
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[(&str, &str)] = &[];

/// This block's migrations as its `lifecycle(Init)` applies them, and as
/// [`crate::blocks::all_block_migrations`] lists them: both dialects' files
/// and both dialects' pre-ledger history (`crate::migration_history`).
pub(crate) const MIGRATIONS: crate::migration_helper::BlockMigrations =
    crate::migration_helper::BlockMigrations {
        block: super::FILES_BLOCK_ID,
        sqlite: SQLITE_MIGRATIONS,
        postgres: POSTGRES_MIGRATIONS,
        sqlite_history: crate::migration_history::files::SQLITE,
        postgres_history: crate::migration_history::files::POSTGRES,
    };

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{blocks::files::repo, test_support::TestContext};

    /// Migration 002's repair half, on the only database that can need it:
    /// one that already holds two rows for one bucket name.
    ///
    /// Every other fixture applies 001 and 002 together against an empty
    /// table, so the `DELETE` never has a duplicate to find and a SQL error
    /// in it would go unnoticed until a real deployment tried to upgrade —
    /// where the file's transaction rolls back, leaves the index uncreated,
    /// and fails again on every later boot (the runner swallows no error).
    ///
    /// So this applies 001 alone, plants the takeover the index exists to
    /// stop, and then applies the real migration list the way an upgrading
    /// deployment's first migrating boot or deploy does.
    #[tokio::test]
    async fn migration_002_repairs_a_database_that_already_holds_a_duplicate_name() {
        let mut ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::files::FilesBlock::BLOCK_NAME);
        crate::migration_helper::apply_pending(
            &ctx,
            "impresspress/files",
            &SQLITE_MIGRATIONS[..1],
            &[],
        )
        .await
        .expect("001 applies");

        // Alice's bucket, then mallory's row for the same folder — accepted
        // before the index existed, and what an upgrading deployment holds.
        for (owner, created_at) in [
            ("alice", "2026-01-01T00:00:00Z"),
            ("mallory", "2026-06-01T00:00:00Z"),
        ] {
            repo::buckets::seed(
                &ctx,
                crate::util::json_map(json!({
                    "name": "assets",
                    "public": false,
                    "created_by": owner,
                    "created_at": created_at,
                })),
            )
            .await
            .expect("seed the duplicate");
        }

        ctx.set_config(crate::migration_helper::RUN_MIGRATIONS_KEY, "1");
        crate::migration_helper::apply_pending(&ctx, "impresspress/files", SQLITE_MIGRATIONS, &[])
            .await
            .expect("002 applies to a database holding a duplicate");

        assert!(
            repo::buckets::find_owned(&ctx, "assets", "alice")
                .await
                .expect("bucket lookup")
                .is_some(),
            "the earliest creator keeps the bucket",
        );
        assert!(
            repo::buckets::find_owned(&ctx, "assets", "mallory")
                .await
                .expect("bucket lookup")
                .is_none(),
            "the later claim on someone else's folder is removed",
        );
        assert!(
            repo::buckets::insert(&ctx, "assets", false, "mallory")
                .await
                .is_err(),
            "and the index is in place, so it cannot be made again",
        );
    }
}

#[cfg(test)]
mod legacy_share_expiry_tests {
    //! What `003_legacy_share_token_expiry` does to the rows of a deployment
    //! upgraded into the opaque-token scheme.
    //!
    //! A share link used to be gated by its token's own 30-day JWT expiry,
    //! checked before the row was read. Now only the row can end a link — so
    //! a legacy row with no expiry would become a permanently live public
    //! link on upgrade. This repair gives those rows the expiry their token
    //! used to impose. The end-to-end half — the repaired row refused by
    //! `share::handle_direct_access` — lives beside that handler.
    //!
    //! The repair is driven through `apply_pending`, the path an upgrading
    //! deployment's first migrating boot or deploy takes, over a database
    //! whose files list stops before it, and the rows are read back through
    //! `repo::shares`, so what is asserted is what the block sees.

    use std::collections::HashMap;

    use serde_json::json;

    use super::{LEGACY_SHARE_TOKEN_EXPIRY, POSTGRES_MIGRATION_FILES, SQLITE_MIGRATIONS};
    use crate::{blocks::files::repo, migration_helper, test_support::TestContext};

    /// A token minted under the old scheme: three dot-separated segments.
    const LEGACY_TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.eyJ0eXBlIjoic2hhcmUifQ.sig";
    /// A second legacy-shaped token, for a test that needs two: the
    /// `token` column is unique.
    const LEGACY_TOKEN_2: &str = "eyJhbGciOiJIUzI1NiJ9.eyJ0eXBlIjoic2hhcmUiLCJuIjoyfQ.sig2";
    /// A token minted under the new one: 32 bytes, hex, no dots.
    const OPAQUE_TOKEN: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    /// The shipped list before the repair, sliced out by name: an unwired
    /// repair would leave the whole list here and trip this assert instead of
    /// silently testing nothing.
    fn before_the_repair() -> &'static [(&'static str, &'static str)] {
        let at = SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == LEGACY_SHARE_TOKEN_EXPIRY)
            .unwrap_or_else(|| {
                panic!(
                    "{LEGACY_SHARE_TOKEN_EXPIRY} must be wired into SQLITE_MIGRATIONS to reach \
                     a deployed database"
                )
            });
        &SQLITE_MIGRATIONS[..at]
    }

    /// A deployment whose files list stops before the repair, in a build that
    /// applies migrations, as every deploy and native boot is.
    async fn upgrading_deployment() -> TestContext {
        let mut ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::files::FilesBlock::BLOCK_NAME);
        migration_helper::apply_pending(
            &ctx,
            crate::blocks::files::FilesBlock::BLOCK_NAME,
            before_the_repair(),
            &[],
        )
        .await
        .expect("the files list before the repair");
        ctx.set_config(migration_helper::RUN_MIGRATIONS_KEY, "1");
        ctx
    }

    /// The upgrade: the shipped list, whose pending files are the repair and
    /// what follows it.
    async fn apply_the_repair(ctx: &TestContext) {
        migration_helper::apply_pending(
            ctx,
            crate::blocks::files::FilesBlock::BLOCK_NAME,
            SQLITE_MIGRATIONS,
            &[],
        )
        .await
        .expect("apply the legacy-expiry repair");
    }

    /// Seed one share row exactly as the old code left it.
    async fn seed_share(
        ctx: &TestContext,
        id: &str,
        token: &str,
        created_at: &str,
        expires_at: Option<&str>,
    ) -> String {
        let mut row: HashMap<String, serde_json::Value> = HashMap::new();
        row.insert("id".into(), json!(id));
        row.insert("token".into(), json!(token));
        row.insert("bucket".into(), json!("photos"));
        row.insert("key".into(), json!("a.png"));
        row.insert("created_by".into(), json!("alice"));
        row.insert("created_at".into(), json!(created_at));
        if let Some(expiry) = expires_at {
            row.insert("expires_at".into(), json!(expiry));
        }
        repo::shares::seed(ctx, row)
            .await
            .unwrap_or_else(|e| panic!("seed {id}: {e}"))
            .id
    }

    async fn expires_at(ctx: &TestContext, id: &str) -> Option<String> {
        repo::shares::find_by_id(ctx, id)
            .await
            .unwrap_or_else(|e| panic!("read {id}: {e}"))
            .expires_at
    }

    /// The instant SEC-055 shortened the share JWT from 365 days to 30
    /// (commit `ecf4dee5`). Rows either side of it get different lifetimes,
    /// because the token's own life is what the repair reproduces.
    const TTL_CHANGED_AT: &str = "2026-05-14T05:43:03Z";

    /// A share minted while the JWT ran for a year gets a year — stamping
    /// it 30 days out would revoke a link that works today.
    #[tokio::test]
    async fn a_share_from_the_one_year_era_gets_a_year() {
        let ctx = upgrading_deployment().await;
        let id = seed_share(&ctx, "old", LEGACY_TOKEN, "2026-04-20T10:00:00Z", None).await;

        apply_the_repair(&ctx).await;

        assert_eq!(
            expires_at(&ctx, &id).await.as_deref(),
            Some("2027-04-20T10:00:00Z"),
            "a link minted before {TTL_CHANGED_AT} lived a year, and still does"
        );
    }

    /// A share minted after the TTL was shortened gets the 30 days its JWT
    /// carried — the link is dead today and stays dead.
    #[tokio::test]
    async fn a_share_from_the_thirty_day_era_gets_thirty_days() {
        let ctx = upgrading_deployment().await;
        let id = seed_share(&ctx, "recent", LEGACY_TOKEN, "2026-06-01T00:00:00Z", None).await;

        apply_the_repair(&ctx).await;

        assert_eq!(
            expires_at(&ctx, &id).await.as_deref(),
            Some("2026-07-01T00:00:00Z"),
            "a link minted after {TTL_CHANGED_AT} lived 30 days"
        );
    }

    /// The boundary second falls the same way in both dialects.
    ///
    /// A row minted inside the second SEC-055 landed in is the one place
    /// the two spellings of the cutoff could disagree: SQLite normalizes
    /// and truncates the stored stamp with `strftime`, PostgreSQL casts it
    /// to `timestamptz`. Both put `…05:43:03.5` on the 30-day arm — the
    /// same row, the same answer — and the PostgreSQL job asserts the twin
    /// of this. A raw TEXT compare would put it on the 365-day arm here and
    /// the 30-day arm there.
    #[tokio::test]
    async fn the_boundary_second_falls_on_the_thirty_day_arm() {
        let ctx = upgrading_deployment().await;
        let inside = seed_share(
            &ctx,
            "inside",
            LEGACY_TOKEN,
            "2026-05-14T05:43:03.5+00:00",
            None,
        )
        .await;
        let just_before = seed_share(
            &ctx,
            "just_before",
            LEGACY_TOKEN_2,
            "2026-05-14T05:43:02.999+00:00",
            None,
        )
        .await;

        apply_the_repair(&ctx).await;

        assert_eq!(
            expires_at(&ctx, &inside).await.as_deref(),
            Some("2026-06-13T05:43:03Z"),
            "the boundary second is not before the cutoff: 30 days"
        );
        assert_eq!(
            expires_at(&ctx, &just_before).await.as_deref(),
            Some("2027-05-14T05:43:02Z"),
            "the second before it is: 365 days"
        );
    }

    /// The stamp is written in the format the handler parses, from the
    /// format production rows are written in.
    ///
    /// A row's `created_at` is `chrono::Utc::now().to_rfc3339()` — nine
    /// fractional digits and a `+00:00` offset, not the tidy `…Z` a
    /// hand-written fixture uses. A row this deployment minted ten days ago
    /// is the one case where the repair's arms differ observably: it is
    /// recent enough to take the 30-day arm and recent enough that 30 days
    /// is still in the future, so a `created_at` the engine could not read
    /// (falling through to the "now" arm) would kill a live link, and the
    /// assertion below is what catches it.
    #[tokio::test]
    async fn a_recent_legacy_share_keeps_the_rest_of_its_thirty_days() {
        let ctx = upgrading_deployment().await;
        let minted = chrono::Utc::now() - chrono::Duration::days(10);
        let id = seed_share(&ctx, "recent", LEGACY_TOKEN, &minted.to_rfc3339(), None).await;

        apply_the_repair(&ctx).await;

        let stamped = expires_at(&ctx, &id).await.expect("an expiry was written");
        let parsed = chrono::DateTime::parse_from_rfc3339(&stamped)
            .expect("the stamp must be RFC 3339, as the handler parses it")
            .with_timezone(&chrono::Utc);
        assert!(
            parsed > chrono::Utc::now(),
            "a link minted 10 days ago has 20 of its 30 days left, got {stamped}"
        );
        assert!(
            (parsed - (minted + chrono::Duration::days(30)))
                .num_seconds()
                .abs()
                <= 1,
            "the expiry must be 30 days after the row was minted, got {stamped}"
        );
    }

    /// The repair is targeted: it touches neither a token minted under the
    /// new scheme nor a row whose owner chose an expiry.
    #[tokio::test]
    async fn the_repair_leaves_opaque_tokens_and_chosen_expiries_alone() {
        let ctx = upgrading_deployment().await;
        let opaque = seed_share(&ctx, "opaque", OPAQUE_TOKEN, "2026-01-01T00:00:00Z", None).await;
        let chosen = seed_share(
            &ctx,
            "chosen",
            LEGACY_TOKEN,
            "2026-01-01T00:00:00Z",
            Some("2027-06-01T00:00:00Z"),
        )
        .await;

        apply_the_repair(&ctx).await;

        assert_eq!(
            expires_at(&ctx, &opaque).await,
            None,
            "a token minted under the new scheme is not a legacy link"
        );
        assert_eq!(
            expires_at(&ctx, &chosen).await.as_deref(),
            Some("2027-06-01T00:00:00Z"),
            "an expiry its owner chose is the binding one"
        );
    }

    /// A legacy row whose `created_at` cannot be read still stops working:
    /// it gets the migration's own instant rather than keeping a NULL that
    /// would leave the link live forever.
    #[tokio::test]
    async fn a_legacy_token_with_an_unreadable_created_at_still_expires() {
        let ctx = upgrading_deployment().await;
        let id = seed_share(&ctx, "garbled", LEGACY_TOKEN, "not a timestamp", None).await;

        apply_the_repair(&ctx).await;

        let stamped = expires_at(&ctx, &id)
            .await
            .expect("a legacy row must not be left unexpiring");
        let parsed = chrono::DateTime::parse_from_rfc3339(&stamped)
            .expect("the stamp must be RFC 3339, as the handler parses it");
        assert!(
            parsed <= chrono::Utc::now(),
            "an already-dead legacy link must not be revived: got {stamped}"
        );
    }

    /// Running the repair's statements a second time moves nothing.
    ///
    /// Before the ledger, the runner re-ran every file whenever the list
    /// changed, so a deployment that kept upgrading ran this repair more than
    /// once: `migration-history.toml` declares it `rerun_safe`, and the
    /// history's verdicts for those states rest on that. The second pass is
    /// the runner from before the ledger; today's runs the file once.
    #[tokio::test]
    async fn the_repair_is_idempotent() {
        let ctx = upgrading_deployment().await;
        let old = seed_share(&ctx, "old", LEGACY_TOKEN, "2026-04-20T10:00:00Z", None).await;
        let recent = seed_share(&ctx, "recent", LEGACY_TOKEN_2, "2026-06-01T00:00:00Z", None).await;

        apply_the_repair(&ctx).await;
        let after_first = (
            expires_at(&ctx, &old).await,
            expires_at(&ctx, &recent).await,
        );
        let (_, repair) = SQLITE_MIGRATIONS
            .iter()
            .find(|(name, _)| *name == LEGACY_SHARE_TOKEN_EXPIRY)
            .expect("the repair is wired");
        migration_helper::legacy_runner::apply(
            &ctx.database_service(),
            &[(LEGACY_SHARE_TOKEN_EXPIRY, repair)],
        )
        .await
        .expect("the old runner's second pass");

        assert_eq!(
            (
                expires_at(&ctx, &old).await,
                expires_at(&ctx, &recent).await
            ),
            after_first,
            "a second pass must move no expiry it already set"
        );
        assert_eq!(
            after_first.0.as_deref(),
            Some("2027-04-20T10:00:00Z"),
            "and the first pass must have set the one-year arm"
        );
    }

    /// A postgres deployment runs one script per SQLite script, in order,
    /// and both spell the same repair.
    #[test]
    fn both_dialects_ship_the_same_migrations() {
        assert_eq!(
            SQLITE_MIGRATIONS.len(),
            POSTGRES_MIGRATION_FILES.len(),
            "every SQLite migration needs its postgres twin"
        );
        let repair = |sql: &&str| {
            assert!(
                sql.contains("LIKE '%.%.%'"),
                "the repair must select legacy JWT-shaped tokens"
            );
            assert!(
                sql.contains("expires_at IS NULL OR expires_at = ''"),
                "the repair must leave a chosen expiry alone"
            );
        };
        let at = SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == LEGACY_SHARE_TOKEN_EXPIRY)
            .expect("a repair is shipped");
        repair(&SQLITE_MIGRATIONS[at].1);
        repair(&POSTGRES_MIGRATION_FILES[at].1);
    }
}
