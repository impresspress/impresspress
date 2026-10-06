//! Legalpages block migrations. Applied from the block's `Init` lifecycle via
//! [`crate::migration_helper::lifecycle_init`].

const SQL_001_SQLITE: &str = include_str!("001_legalpages_schema.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_001_POSTGRES: &str = include_str!("001_legalpages_schema.postgres.sql");
const SQL_002_SQLITE: &str = include_str!("002_legalpages_version_numbers.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_002_POSTGRES: &str = include_str!("002_legalpages_version_numbers.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_legalpages_schema", SQL_001_SQLITE),
    ("002_legalpages_version_numbers", SQL_002_SQLITE),
];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`]. Empty
/// when the `postgres` feature is off — see `files::migrations`'s doc for the
/// rationale (Cloudflare/D1 never selects postgres; don't embed dead SQL).
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[SQL_001_POSTGRES, SQL_002_POSTGRES];
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::legalpages::{
        contracts::{DocumentStatus, DocumentType},
        repo::documents::{self, NewDraft},
    };

    /// Migration 002 over data written before it: drafts stored as v1 become
    /// unnumbered, and where publishes reused a number the published row (else
    /// the most recently updated one) keeps it — after which the unique index
    /// can be built and holds.
    #[tokio::test]
    async fn version_numbers_migration_unnumbers_drafts_and_resolves_clashes() {
        let mut ctx = crate::test_support::TestContext::with_admin()
            .await
            .running_as(crate::blocks::legalpages::LegalPagesBlock::BLOCK_NAME);
        let block = "impresspress/legalpages";
        crate::migration_helper::apply_migrations(&ctx, block, &[SQL_001_SQLITE], &[])
            .await
            .expect("apply 001");

        // Rows as the pre-002 code wrote them, oldest first (each insert's
        // `updated_at` is later than the one before).
        let mut ids = Vec::new();
        for (status, version) in [
            (DocumentStatus::Draft, 1),
            (DocumentStatus::Archived, 1),
            // v2 used twice: the published row keeps it.
            (DocumentStatus::Archived, 2),
            (DocumentStatus::Published, 2),
            // v3 used twice by archived rows: the later one keeps it.
            (DocumentStatus::Archived, 3),
            (DocumentStatus::Archived, 3),
        ] {
            ids.push(legacy_row(&ctx, status, version).await);
        }

        ctx.set_config(crate::migration_helper::RUN_MIGRATIONS_KEY, "1");
        crate::migration_helper::apply_migrations(
            &ctx,
            block,
            &[SQL_001_SQLITE, SQL_002_SQLITE],
            &[],
        )
        .await
        .expect("apply 002");

        let mut versions = Vec::new();
        for id in &ids {
            let row = documents::get(&ctx, id).await.expect("read").expect("row");
            versions.push((row.status, row.version));
        }
        assert_eq!(
            versions,
            vec![
                (DocumentStatus::Draft, 0),
                (DocumentStatus::Archived, 1),
                (DocumentStatus::Archived, 0),
                (DocumentStatus::Published, 2),
                (DocumentStatus::Archived, 0),
                (DocumentStatus::Archived, 3),
            ]
        );

        // The index now refuses a second numbered row of the type …
        let draft = documents::insert_draft(&ctx, new_draft())
            .await
            .expect("draft");
        let taken = documents::set_state_for_test(&ctx, &draft.id, DocumentStatus::Archived, 3)
            .await
            .expect_err("v3 is taken");
        assert_eq!(taken.code, wafer_run::ErrorCode::AlreadyExists, "{taken:?}");
        // … and leaves drafts, all unnumbered, alone.
        documents::insert_draft(&ctx, new_draft())
            .await
            .expect("another unnumbered draft");
    }

    /// Publishing does not depend on migration 002 having run: on a table
    /// without its unique index (a deployment that took the code but not the
    /// schema change), a number already held is still not taken twice.
    #[tokio::test]
    async fn publishing_keeps_numbers_unique_without_the_index() {
        let ctx = crate::test_support::TestContext::with_admin()
            .await
            .running_as(crate::blocks::legalpages::LegalPagesBlock::BLOCK_NAME);
        crate::migration_helper::apply_migrations(
            &ctx,
            "impresspress/legalpages",
            &[SQL_001_SQLITE],
            &[],
        )
        .await
        .expect("apply 001 only");
        let live = legacy_row(&ctx, DocumentStatus::Published, 2).await;

        let outcome = documents::publish(
            &ctx,
            DocumentType::Terms,
            2,
            documents::PublishSource::New {
                title: "Terms",
                content: "another",
                created_by: "seed",
            },
        )
        .await
        .expect("the write runs");

        assert!(matches!(outcome, documents::PublishOutcome::NumberTaken));
        let row = documents::get(&ctx, &live)
            .await
            .expect("read")
            .expect("row");
        assert_eq!((row.status, row.version), (DocumentStatus::Published, 2));
        assert_eq!(documents::count(&ctx).await.expect("count"), 1);
    }

    fn new_draft() -> NewDraft<'static> {
        NewDraft {
            doc_type: DocumentType::Terms,
            title: "Terms",
            content: "body",
            created_by: "seed",
        }
    }

    /// A row in `status` as `version`, written the way the pre-002 code could
    /// leave one.
    async fn legacy_row(
        ctx: &crate::test_support::TestContext,
        status: DocumentStatus,
        version: i64,
    ) -> String {
        let draft = documents::insert_draft(ctx, new_draft())
            .await
            .expect("insert");
        documents::set_state_for_test(ctx, &draft.id, status, version)
            .await
            .expect("set legacy state")
            .id
    }
}
