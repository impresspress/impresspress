//! The single-row record of what the seed bundle said about this sandbox
//! (`impresspress__dev__seed_info`): which template seeded it, the prompt
//! the workspace page suggests, the site-authoring guide
//! `dev_read_reference` serves as `site_markdown`, and the sandbox's own
//! `llms.txt`, which the publisher serves at `/llms.txt` for a site that has
//! none.
//!
//! Written once, by `seed::import`, on the boot that seeds the instance;
//! read on every reference call, status poll, workspace page render and
//! publish. The
//! migration seeds the row with every column `NULL`, and a `NULL` template is
//! how the row says "this instance's seed carried no `sandbox` block" — an
//! exported bundle never does (`export` writes `sandbox: None`).

use wafer_block::db::{Filter, FilterOp, ListOptions};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::util::RecordExt;

pub const TABLE: &str = "impresspress__dev__seed_info";

/// Primary-key value of the one row this table holds.
const SINGLETON_COLUMN: &str = "singleton_id";
const SINGLETON_ID: i64 = 1;

/// What the seed's `sandbox` block carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedInfo {
    /// The template's name, e.g. `bootstrap`.
    pub template: String,
    /// The paragraph the workspace page offers for copying.
    pub suggested_prompt: String,
    /// The site-authoring guide, Markdown.
    pub guide_markdown: String,
    /// The sandbox's `llms.txt`. `None` only on a row an import wrote before
    /// the column existed (migration 004), until the next boot's
    /// `seed::repair_llms` fetches it.
    pub llms_text: Option<String>,
}

/// The row, or `None` until an import has written it.
pub async fn read(ctx: &dyn Context) -> Result<Option<SeedInfo>, WaferError> {
    let record = db::get_by_field(
        ctx,
        TABLE,
        SINGLETON_COLUMN,
        serde_json::json!(SINGLETON_ID),
    )
    .await?;
    let Some(template) = record.opt_str_field("template") else {
        return Ok(None);
    };
    Ok(Some(SeedInfo {
        template,
        suggested_prompt: record.str_field("suggested_prompt").to_string(),
        guide_markdown: record.str_field("guide_markdown").to_string(),
        llms_text: record.opt_str_field("llms_text"),
    }))
}

/// The seeding template's name, or `None` until an import has written it.
///
/// Reads only the `template` column: the status poll calls this a few times
/// a second, and the row's `guide_markdown` can run to hundreds of KiB.
/// A missing row is `NotFound`, as from [`read`].
pub async fn template(ctx: &dyn Context) -> Result<Option<String>, WaferError> {
    let rows = db::list(
        ctx,
        TABLE,
        &ListOptions {
            columns: Some(vec!["template".to_string()]),
            filters: vec![singleton_filter()],
            limit: Some(1),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    let record = rows
        .records
        .first()
        .ok_or_else(|| WaferError::new(ErrorCode::NotFound, "record not found"))?;
    Ok(record.opt_str_field("template"))
}

/// The columns the sandbox's `llms.txt` lives in, read without the guide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmsRow {
    /// The seeding template; `None` when the seed carried no sandbox block.
    pub template: Option<String>,
    /// The sandbox's `llms.txt`, when an import or the boot repair
    /// (`seed::repair_llms`) recorded one.
    pub text: Option<String>,
    /// The hash of the sandbox text the publisher has in the published
    /// folder at `llms.txt`, or `None` when it has none there — the site's
    /// own file holds the name, or nothing was ever written.
    pub published_sha256: Option<String>,
}

/// The three `llms.txt` columns.
///
/// Reads only those, for [`template`]'s reason: every publish and every boot
/// calls this, and the row's guide is beside them. A missing row is
/// `NotFound`, as from [`read`].
pub async fn llms(ctx: &dyn Context) -> Result<LlmsRow, WaferError> {
    let rows = db::list(
        ctx,
        TABLE,
        &ListOptions {
            columns: Some(vec![
                "template".to_string(),
                "llms_text".to_string(),
                "llms_published_sha256".to_string(),
            ]),
            filters: vec![singleton_filter()],
            limit: Some(1),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    let record = rows
        .records
        .first()
        .ok_or_else(|| WaferError::new(ErrorCode::NotFound, "record not found"))?;
    Ok(LlmsRow {
        template: record.opt_str_field("template"),
        text: record.opt_str_field("llms_text"),
        published_sha256: record.opt_str_field("llms_published_sha256"),
    })
}

/// Record the sandbox's `llms.txt` on a row that has none — the boot repair's
/// write. Nothing else on the row changes.
pub async fn record_llms_text(ctx: &dyn Context, text: &str) -> Result<(), WaferError> {
    let data = crate::util::json_map(serde_json::json!({ "llms_text": text }));
    db::update_by_filters(ctx, TABLE, vec![singleton_filter()], data).await
}

/// Record what the publisher now has at `llms.txt` in the published folder:
/// the hash of the sandbox text it wrote, or `None` once it is not there.
pub async fn set_llms_published(ctx: &dyn Context, sha256: Option<&str>) -> Result<(), WaferError> {
    let data = crate::util::json_map(serde_json::json!({ "llms_published_sha256": sha256 }));
    db::update_by_filters(ctx, TABLE, vec![singleton_filter()], data).await
}

fn singleton_filter() -> Filter {
    Filter {
        field: SINGLETON_COLUMN.to_string(),
        operator: FilterOp::Equal,
        value: serde_json::json!(SINGLETON_ID),
    }
}

/// Overwrite the row with `info`, stamping `imported_at`.
pub async fn write(ctx: &dyn Context, info: &SeedInfo) -> Result<(), WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "template": info.template,
        "suggested_prompt": info.suggested_prompt,
        "guide_markdown": info.guide_markdown,
        "llms_text": info.llms_text,
        "imported_at": super::now(),
    }));
    db::update_by_filters(ctx, TABLE, vec![singleton_filter()], data).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{blocks::dev::test_support::FakeControl, test_support::TestContext};

    /// The migration seeds the row empty; empty reads as "no sandbox block".
    #[tokio::test]
    async fn migration_seeds_an_empty_row_that_reads_as_none() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        assert_eq!(read(&ctx).await.expect("read"), None);
    }

    #[tokio::test]
    async fn write_then_read_round_trips_every_field() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let info = SeedInfo {
            template: "bootstrap".to_string(),
            suggested_prompt: "Build me a shop.".to_string(),
            guide_markdown: "# Guide\n\nWrite HTML.\n".to_string(),
            llms_text: Some("# Sandbox\n".to_string()),
        };
        write(&ctx, &info).await.expect("write");
        assert_eq!(read(&ctx).await.expect("read"), Some(info));
    }

    /// The columns are declared `TEXT`, so text that happens to look like JSON
    /// comes back as the same string — no backend decodes it behind our back.
    #[tokio::test]
    async fn json_shaped_text_round_trips_as_the_same_string() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let info = SeedInfo {
            template: "bootstrap".to_string(),
            suggested_prompt: "[1,2]".to_string(),
            guide_markdown: r#"{"a":1}"#.to_string(),
            llms_text: Some("[3]".to_string()),
        };
        write(&ctx, &info).await.expect("write");
        assert_eq!(read(&ctx).await.expect("read"), Some(info));
    }

    /// A missing row is `NotFound` from both readers, not `None` from one.
    #[tokio::test]
    async fn a_missing_row_is_not_found_from_read_and_template_alike() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        db::delete_by_filters(&ctx, TABLE, vec![singleton_filter()])
            .await
            .expect("delete the seeded row");
        let read_err = read(&ctx).await.expect_err("read of a missing row");
        let template_err = template(&ctx).await.expect_err("template of a missing row");
        assert_eq!(read_err.code, ErrorCode::NotFound);
        assert_eq!(template_err.code, ErrorCode::NotFound);
    }

    /// The narrow read, and the two single-column writes beside it.
    #[tokio::test]
    async fn the_llms_columns_are_read_and_written_without_the_rest_of_the_row() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let empty = LlmsRow {
            template: None,
            text: None,
            published_sha256: None,
        };
        assert_eq!(llms(&ctx).await.expect("llms"), empty);

        let info = SeedInfo {
            template: "bootstrap".to_string(),
            suggested_prompt: "Build me a shop.".to_string(),
            guide_markdown: "# Guide\n".to_string(),
            llms_text: None,
        };
        write(&ctx, &info).await.expect("write");
        assert_eq!(
            llms(&ctx).await.expect("llms"),
            LlmsRow {
                template: Some("bootstrap".to_string()),
                ..empty.clone()
            }
        );

        record_llms_text(&ctx, "# Sandbox\n").await.expect("text");
        set_llms_published(&ctx, Some("abc")).await.expect("set");
        assert_eq!(
            llms(&ctx).await.expect("llms"),
            LlmsRow {
                template: Some("bootstrap".to_string()),
                text: Some("# Sandbox\n".to_string()),
                published_sha256: Some("abc".to_string()),
            }
        );
        // The rest of the row is as the import wrote it.
        let row = read(&ctx).await.expect("read").expect("row");
        assert_eq!(row.guide_markdown, "# Guide\n");
        assert_eq!(row.llms_text.as_deref(), Some("# Sandbox\n"));

        set_llms_published(&ctx, None).await.expect("clear");
        assert_eq!(llms(&ctx).await.expect("llms").published_sha256, None);
    }

    #[tokio::test]
    async fn template_is_none_until_a_write_then_the_written_name() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        assert_eq!(template(&ctx).await.expect("template"), None);
        write(
            &ctx,
            &SeedInfo {
                template: "bootstrap".to_string(),
                suggested_prompt: "Build me a shop.".to_string(),
                guide_markdown: "# Guide\n".to_string(),
                llms_text: None,
            },
        )
        .await
        .expect("write");
        assert_eq!(
            template(&ctx).await.expect("template"),
            Some("bootstrap".to_string())
        );
    }
}
