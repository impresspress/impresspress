//! Product groups (categories / bundles).
//!
//! The table name lived in `handlers/mod.rs` until this module existed, which
//! is why the admin overview page, the admin stats endpoint and the groups
//! page each built their own `db::count` / `db::list` against it. Every query
//! against the table is now in this file.

use wafer_block::db::{Filter, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, WaferError};

pub(crate) const TABLE: &str = "impresspress__products__groups";

/// Count the groups matching `filters` (`&[]` for the whole catalog).
///
/// Both counting call sites — the admin overview page and the admin stats
/// endpoint — surface a failure as a 500 rather than rendering `0`, so this
/// returns `Result` and never a fabricated count.
pub(crate) async fn count(ctx: &dyn Context, filters: &[Filter]) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, filters).await
}

/// The groups matching `filters`, name-ascending, capped at `limit`.
///
/// The one shape both listing call sites want: the admin groups page (whole
/// catalog, 100) and a user's own groups (`user_id` filter, 1000). Sorting
/// belongs here rather than at the call site — "groups, by name" is what the
/// table is read for.
pub(crate) async fn list_by_name(
    ctx: &dyn Context,
    filters: Vec<Filter>,
    limit: u32,
) -> Result<db::RecordList, WaferError> {
    let opts = ListOptions {
        filters,
        sort: vec![SortField {
            field: "name".to_string(),
            desc: false,
        }],
        limit: Some(limit),
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts).await
}

/// One group by id. `Err(NotFound)` when there is no such row — the caller
/// decides what that means (creating a product against a missing group is a
/// 400, not a 404).
pub(crate) async fn get(ctx: &dyn Context, id: &str) -> Result<Record, WaferError> {
    db::get(ctx, TABLE, id).await
}
