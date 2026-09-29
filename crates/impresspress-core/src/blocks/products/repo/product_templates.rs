//! Reusable product template definitions (admin-authored, seeded by Init).

use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

pub(crate) const TABLE: &str = "impresspress__products__product_templates";

/// The id of the `name = "default"` template the Init lifecycle seeds. Same
/// contract as [`super::group_templates::default_id`], down to the reason:
/// `Ok(None)` is a database with no default template and the product create
/// it feeds may proceed without one, while a failed read is `Err` rather than
/// a template-less product row written during an outage.
pub(crate) async fn default_id(ctx: &dyn Context) -> Result<Option<String>, WaferError> {
    match db::get_by_field(ctx, TABLE, "name", serde_json::json!("default")).await {
        Ok(record) => Ok(Some(record.id)),
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
