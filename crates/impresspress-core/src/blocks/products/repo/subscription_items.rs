//! General product subscription items, separate from platform-plan addons.

use wafer_block::wire::database::OnConflict;
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::purchases;
use crate::util::RecordExt;

pub(crate) const TABLE: &str = "impresspress__products__subscription_items";

/// Idempotently materialize the immutable order lines as local subscription
/// items when Checkout reports a Stripe subscription id. Provider item ids
/// can be filled by later subscription events without losing the purchased
/// offer/component snapshot.
pub(crate) async fn snapshot_from_purchase(
    ctx: &dyn Context,
    purchase_id: &str,
    stripe_subscription_id: &str,
) -> Result<(), WaferError> {
    let lines = purchases::list_line_items(ctx, purchase_id).await?;
    let now = chrono::Utc::now().to_rfc3339();
    for line in lines {
        let id = format!("subscription_item_{}_{}", stripe_subscription_id, line.id);
        db::upsert(
            ctx,
            TABLE,
            vec![
                ("id".to_string(), serde_json::json!(id)),
                (
                    "subscription_id".to_string(),
                    serde_json::json!(stripe_subscription_id),
                ),
                ("purchase_id".to_string(), serde_json::json!(purchase_id)),
                (
                    "product_id".to_string(),
                    serde_json::json!(line.str_field("product_id")),
                ),
                (
                    "offer_id".to_string(),
                    serde_json::json!(line.str_field("offer_id")),
                ),
                (
                    "component_id".to_string(),
                    serde_json::json!(line.str_field("component_id")),
                ),
                (
                    "stripe_price_id".to_string(),
                    serde_json::json!(line.str_field("stripe_price_id")),
                ),
                (
                    "quantity".to_string(),
                    serde_json::json!(line.i64_field("quantity")),
                ),
                ("status".to_string(), serde_json::json!("active")),
                (
                    "metadata".to_string(),
                    serde_json::json!(
                        serde_json::json!({
                            "offer_version": line.i64_field("offer_version"),
                            "unit_amount_minor": line.i64_field("unit_amount_minor"),
                            "total_minor": line.i64_field("total_minor"),
                            "input_snapshot": line.data.get("input_snapshot").cloned().unwrap_or_default(),
                        })
                        .to_string()
                    ),
                ),
                ("created_at".to_string(), serde_json::json!(&now)),
                ("updated_at".to_string(), serde_json::json!(&now)),
            ],
            vec!["id".to_string()],
            OnConflict::SetColumns(vec![
                "status".to_string(),
                "updated_at".to_string(),
            ]),
        )
        .await?;
    }
    Ok(())
}
