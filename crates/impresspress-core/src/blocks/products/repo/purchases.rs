//! Data access for the purchases header table and its line items.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, WaferError};

/// Purchase header table — one row per checkout / order.
pub(crate) const PURCHASES_TABLE: &str = "impresspress__products__purchases";

/// Purchase line-item table — one row per product line in a purchase.
pub(crate) const LINE_ITEMS_TABLE: &str = "impresspress__products__line_items";

/// Fetch a purchase header by id.
pub(crate) async fn get(ctx: &dyn Context, id: &str) -> Result<Record, WaferError> {
    db::get(ctx, PURCHASES_TABLE, id).await
}

/// Insert a purchase header. Caller supplies the full field map.
pub(crate) async fn create(
    ctx: &dyn Context,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    db::create(ctx, PURCHASES_TABLE, data).await
}

/// Insert a line item. Caller supplies the full field map.
pub(crate) async fn add_line_item(
    ctx: &dyn Context,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    db::create(ctx, LINE_ITEMS_TABLE, data).await
}

/// Delete a purchase header (rollback path).
pub(crate) async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, PURCHASES_TABLE, id).await
}

/// Apply an arbitrary field update to a purchase header by id.
pub(crate) async fn update(
    ctx: &dyn Context,
    id: &str,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    db::update(ctx, PURCHASES_TABLE, id, data).await
}

/// List a purchase's line items.
pub(crate) async fn list_line_items(
    ctx: &dyn Context,
    purchase_id: &str,
) -> Result<Vec<Record>, WaferError> {
    db::list_all(
        ctx,
        LINE_ITEMS_TABLE,
        vec![Filter {
            field: "purchase_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(purchase_id.to_string()),
        }],
    )
    .await
}

/// Paginated purchase list, newest first, with caller-supplied filters.
pub(crate) async fn list_paginated(
    ctx: &dyn Context,
    filters: Vec<Filter>,
    page: i64,
    page_size: i64,
) -> Result<RecordList, WaferError> {
    let sort = vec![SortField {
        field: "created_at".to_string(),
        desc: true,
    }];
    db::paginated_list(ctx, PURCHASES_TABLE, page, page_size, filters, sort).await
}

/// Atomic checkout-completion: only transitions a purchase still in
/// `checkout_started`/`pending`. Returns rows affected (0 = already
/// completed/refunded). `checkout.session.completed`.
pub(crate) async fn complete_atomic(
    ctx: &dyn Context,
    purchase_id: &str,
    payment_intent: &str,
) -> Result<i64, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("status".into(), serde_json::json!("completed"));
    data.insert(
        "provider_payment_intent_id".into(),
        serde_json::json!(payment_intent),
    );
    data.insert("approved_at".into(), serde_json::json!(&now));
    data.insert("updated_at".into(), serde_json::json!(&now));
    db::update_by_filters_count(
        ctx,
        PURCHASES_TABLE,
        vec![
            Filter {
                field: "id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(purchase_id),
            },
            Filter {
                field: "status".into(),
                operator: FilterOp::In,
                value: serde_json::json!(["checkout_started", "pending"]),
            },
        ],
        data,
    )
    .await
}

/// Atomic checkout claim: `pending` -> `checkout_started`. Returns rows
/// affected (0 = not pending / already in flight).
pub(crate) async fn claim_for_checkout(
    ctx: &dyn Context,
    purchase_id: &str,
) -> Result<i64, WaferError> {
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("status".into(), serde_json::json!("checkout_started"));
    data.insert(
        "updated_at".into(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    db::update_by_filters_count(
        ctx,
        PURCHASES_TABLE,
        vec![
            Filter {
                field: "id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(purchase_id),
            },
            Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("pending"),
            },
        ],
        data,
    )
    .await
}

/// Revert a checkout claim: `checkout_started` -> `pending` (Stripe API error
/// path). Returns rows affected.
pub(crate) async fn revert_checkout_claim(
    ctx: &dyn Context,
    purchase_id: &str,
) -> Result<i64, WaferError> {
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("status".into(), serde_json::json!("pending"));
    data.insert(
        "updated_at".into(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    db::update_by_filters_count(
        ctx,
        PURCHASES_TABLE,
        vec![
            Filter {
                field: "id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(purchase_id),
            },
            Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("checkout_started"),
            },
        ],
        data,
    )
    .await
}

/// Atomic admin refund: `completed` -> `refunded` with audit fields. Returns
/// rows affected (0 = not completed / already refunded).
pub(crate) async fn refund_atomic(
    ctx: &dyn Context,
    id: &str,
    refunded_by: &str,
    reason: &str,
) -> Result<i64, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("status".into(), serde_json::json!("refunded"));
    data.insert("refunded_at".into(), serde_json::json!(&now));
    data.insert("refunded_by".into(), serde_json::json!(refunded_by));
    data.insert("refund_reason".into(), serde_json::json!(reason));
    data.insert("updated_at".into(), serde_json::json!(&now));
    db::update_by_filters_count(
        ctx,
        PURCHASES_TABLE,
        vec![
            Filter {
                field: "id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(id),
            },
            Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("completed"),
            },
        ],
        data,
    )
    .await
}

/// Find a purchase by its provider payment-intent id (`charge.refunded`).
pub(crate) async fn find_by_payment_intent(
    ctx: &dyn Context,
    payment_intent: &str,
) -> Result<Record, WaferError> {
    db::get_by_field(
        ctx,
        PURCHASES_TABLE,
        "provider_payment_intent_id",
        serde_json::Value::String(payment_intent.to_string()),
    )
    .await
}

/// Mark a purchase refunded by id (webhook `charge.refunded` path — sets
/// status/refunded_at/updated_at, mirrors the original `db::update`).
pub(crate) async fn mark_refunded(ctx: &dyn Context, id: &str) -> Result<Record, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut data = HashMap::new();
    data.insert(
        "status".to_string(),
        serde_json::Value::String("refunded".to_string()),
    );
    data.insert(
        "refunded_at".to_string(),
        serde_json::Value::String(now.clone()),
    );
    data.insert("updated_at".to_string(), serde_json::Value::String(now));
    db::update(ctx, PURCHASES_TABLE, id, data).await
}

/// Select line-item product_ids for a purchase (checkout dependency probe).
pub(crate) async fn line_item_product_ids(
    ctx: &dyn Context,
    purchase_id: &str,
) -> Result<Vec<Record>, WaferError> {
    let rows = db::list(
        ctx,
        LINE_ITEMS_TABLE,
        &ListOptions {
            columns: Some(vec!["product_id".into()]),
            filters: vec![Filter {
                field: "purchase_id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(purchase_id),
            }],
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    Ok(rows.records)
}

/// Count all purchases (admin stats).
pub(crate) async fn count_all(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, PURCHASES_TABLE, &[]).await
}

/// Sum `total_cents` over completed purchases (admin revenue).
pub(crate) async fn sum_completed_cents(ctx: &dyn Context) -> Result<f64, WaferError> {
    db::sum(
        ctx,
        PURCHASES_TABLE,
        "total_cents",
        &[Filter {
            field: "status".into(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String("completed".to_string()),
        }],
    )
    .await
}

/// Ids of a user's completed purchases (ownership check).
pub(crate) async fn completed_purchase_ids(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Vec<Record>, WaferError> {
    let rows = db::list(
        ctx,
        PURCHASES_TABLE,
        &ListOptions {
            columns: Some(vec!["id".into()]),
            filters: vec![
                Filter {
                    field: "user_id".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(user_id),
                },
                Filter {
                    field: "status".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!("completed"),
                },
            ],
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    Ok(rows.records)
}

/// Probe whether any of `purchase_ids` contains `product_id` as a line item.
pub(crate) async fn line_item_exists_for_product(
    ctx: &dyn Context,
    purchase_ids: Vec<serde_json::Value>,
    product_id: &str,
) -> bool {
    if purchase_ids.is_empty() {
        return false;
    }
    let rows = db::list(
        ctx,
        LINE_ITEMS_TABLE,
        &ListOptions {
            columns: Some(vec!["id".into()]),
            filters: vec![
                Filter {
                    field: "purchase_id".into(),
                    operator: FilterOp::In,
                    value: serde_json::Value::Array(purchase_ids),
                },
                Filter {
                    field: "product_id".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(product_id),
                },
            ],
            limit: 1,
            skip_count: true,
            ..Default::default()
        },
    )
    .await;
    matches!(rows, Ok(rows) if !rows.records.is_empty())
}
