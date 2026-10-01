//! The single-row record of what the seed bundle said about this sandbox
//! (`impresspress__dev__seed_info`): which template seeded it, the prompt
//! the workspace page suggests, and the site-authoring guide
//! `dev_read_reference` serves as `site_markdown`.
//!
//! Written once, by `seed::import`, on the boot that seeds the instance;
//! read on every reference call, status poll and workspace page render. The
//! migration seeds the row with every column `NULL`, and a `NULL` template is
//! how the row says "this instance's seed carried no `sandbox` block" — an
//! exported bundle never does (`export` writes `sandbox: None`).

use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

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
    }))
}

/// Overwrite the row with `info`, stamping `imported_at`.
pub async fn write(ctx: &dyn Context, info: &SeedInfo) -> Result<(), WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "template": info.template,
        "suggested_prompt": info.suggested_prompt,
        "guide_markdown": info.guide_markdown,
        "imported_at": super::now(),
    }));
    db::update_by_filters(
        ctx,
        TABLE,
        vec![Filter {
            field: SINGLETON_COLUMN.to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(SINGLETON_ID),
        }],
        data,
    )
    .await
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
        };
        write(&ctx, &info).await.expect("write");
        assert_eq!(read(&ctx).await.expect("read"), Some(info));
    }
}
