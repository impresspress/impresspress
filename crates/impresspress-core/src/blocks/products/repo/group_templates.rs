//! Reusable group template definitions (admin-authored, seeded by Init).

use wafer_block::db::{ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

pub(crate) const TABLE: &str = "impresspress__products__group_templates";

/// Every group template, name-ascending. Read-only listing for users.
pub(crate) async fn list_by_name(
    ctx: &dyn Context,
    limit: u32,
) -> Result<db::RecordList, WaferError> {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "name".to_string(),
            desc: false,
        }],
        limit: Some(limit),
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts).await
}

/// The id of the `name = "default"` template the Init lifecycle seeds, so a
/// client-omitted `group_template_id` defaults to a real (UUIDv7) row rather
/// than the literal integer `1`, which never matches a seeded record.
///
/// `Ok(None)` is a database with no default template — an uninitialised one,
/// or one whose seed row was deleted — and the create it feeds may proceed
/// without a template, as it always did.
///
/// A failed read is `Err`. It used to be the same `None`, so an outage wrote
/// exactly the template-less row the seeded default exists to prevent, and
/// the row survived the outage looking like a deliberate choice.
pub(crate) async fn default_id(ctx: &dyn Context) -> Result<Option<String>, WaferError> {
    match db::get_by_field(ctx, TABLE, "name", serde_json::json!("default")).await {
        Ok(record) => Ok(Some(record.id)),
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
