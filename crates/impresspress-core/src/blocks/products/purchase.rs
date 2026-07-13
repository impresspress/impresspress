use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::{repo, PRODUCTS_TABLE};
use crate::{
    http::{
        err_bad_request, err_forbidden, err_internal, err_not_found, err_unauthorized, ok_json,
    },
    util::RecordExt,
};

pub async fn handle_create(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct CreateReq {
        items: Vec<PurchaseItem>,
        currency: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct PurchaseItem {
        product_id: String,
        quantity: i64,
        #[serde(default)]
        variables: HashMap<String, f64>,
    }

    let raw = input.collect_to_bytes().await;
    let body: CreateReq = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    if body.items.is_empty() {
        return err_bad_request("No items in purchase");
    }

    let currency = body.currency.unwrap_or_else(|| "USD".to_string());
    let now = chrono::Utc::now().to_rfc3339();
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthorized("Authentication required to create a purchase");
    }

    // Calculate totals
    let mut total_amount = 0.0;
    let mut line_items_data = Vec::new();

    for item in &body.items {
        if item.quantity <= 0 {
            return err_bad_request("Quantity must be positive");
        }
        let Ok(product) = db::get(ctx, PRODUCTS_TABLE, &item.product_id).await else {
            return err_not_found(&format!("Product {} not found", item.product_id));
        };

        // Reject draft/deleted/inactive products
        let product_status = product
            .data
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("active");
        if product_status != "active" {
            return err_bad_request(&format!(
                "Product {} is not available for purchase (status: {})",
                item.product_id, product_status
            ));
        }
        if !product
            .data
            .get("deleted_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty()
        {
            return err_not_found(&format!("Product {} not found", item.product_id));
        }

        let product_name = product
            .data
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();

        // Resolve unit price via the shared resolver: apply the product's
        // pricing template when set, otherwise fall back to `base_price`. A
        // missing template row falls back to `base_price` (so a stale reference
        // can't block a sale); the returned price has already passed
        // `validate_price`, which rejects price manipulation via variables and
        // zero-priced freebies.
        let unit_price = match super::pricing::resolve_unit_price(
            ctx,
            &product,
            &item.variables,
            super::pricing::MissingTemplate::FallBackToBase,
        )
        .await
        {
            Ok(resolved) => resolved.unit_price,
            Err(e) => {
                return err_bad_request(&format!(
                    "Invalid price for product {}: {e}",
                    item.product_id
                ))
            }
        };

        let line_total = unit_price * item.quantity as f64;
        total_amount += line_total;

        line_items_data.push((
            item.product_id.clone(),
            product_name,
            item.quantity,
            unit_price,
            line_total,
            &item.variables,
        ));
    }

    // `as i64` saturates on overflow / NaN — both would silently produce a
    // bogus i64. Validate first so a pathological formula result (NaN, ±inf,
    // > i64::MAX cents ≈ $9.2e16) can't sneak through.
    let total_cents_f = total_amount * 100.0;
    if !total_cents_f.is_finite() {
        return err_bad_request("Purchase total is not a finite number");
    }
    let rounded = total_cents_f.round();
    if rounded < 1.0 || rounded > i64::MAX as f64 {
        return err_bad_request("Purchase total is out of range");
    }
    let total_cents = rounded as i64;
    if total_cents <= 0 {
        return err_bad_request("Purchase total must be greater than zero");
    }

    // Create purchase
    let mut purchase_data = HashMap::new();
    purchase_data.insert(
        "user_id".to_string(),
        serde_json::Value::String(user_id.clone()),
    );
    purchase_data.insert(
        "status".to_string(),
        serde_json::Value::String("pending".to_string()),
    );
    purchase_data.insert("total_cents".to_string(), serde_json::json!(total_cents));
    purchase_data.insert("amount_cents".to_string(), serde_json::json!(total_cents));
    purchase_data.insert("currency".to_string(), serde_json::Value::String(currency));
    purchase_data.insert(
        "provider".to_string(),
        serde_json::Value::String("manual".to_string()),
    );
    purchase_data.insert(
        "created_at".to_string(),
        serde_json::Value::String(now.clone()),
    );
    purchase_data.insert(
        "updated_at".to_string(),
        serde_json::Value::String(now.clone()),
    );

    let purchase = match repo::purchases::create(ctx, purchase_data).await {
        Ok(p) => p,
        Err(e) => return err_internal("Failed to create purchase", e),
    };

    // Create line items — roll back purchase on failure
    for (product_id, product_name, qty, unit_price, line_total, variables) in &line_items_data {
        let mut item_data = HashMap::new();
        item_data.insert(
            "purchase_id".to_string(),
            serde_json::Value::String(purchase.id.clone()),
        );
        item_data.insert(
            "product_id".to_string(),
            serde_json::Value::String(product_id.clone()),
        );
        item_data.insert(
            "product_name".to_string(),
            serde_json::Value::String(product_name.clone()),
        );
        item_data.insert("quantity".to_string(), serde_json::json!(qty));
        item_data.insert("unit_price".to_string(), serde_json::json!(unit_price));
        item_data.insert("total_price".to_string(), serde_json::json!(line_total));
        item_data.insert("variables".to_string(), serde_json::json!(variables));
        item_data.insert(
            "created_at".to_string(),
            serde_json::Value::String(now.clone()),
        );
        if let Err(e) = repo::purchases::add_line_item(ctx, item_data).await {
            rollback_purchase(ctx, &purchase.id).await;
            return err_internal("Failed to create line item", e);
        }
    }

    ok_json(&serde_json::json!({
        "id": purchase.id,
        "status": "pending",
        "total_cents": total_cents,
        "item_count": line_items_data.len()
    }))
}

/// Roll back a partially-created purchase after a line-item insert fails.
///
/// Tries to delete the purchase header first; if the delete fails (transient
/// DB error, foreign-key constraints from already-inserted siblings, etc.),
/// falls through to marking the purchase `failed` so it can never proceed to
/// checkout. A dangling `pending` purchase with partial line items is the
/// worst case, so this best-effort path is intentionally infallible from the
/// caller's perspective (failures are logged, not propagated).
async fn rollback_purchase(ctx: &dyn Context, purchase_id: &str) {
    if let Err(del_err) = repo::purchases::delete(ctx, purchase_id).await {
        tracing::warn!(
            error = %del_err,
            purchase_id = %purchase_id,
            "rollback delete failed; marking purchase as failed"
        );
        let mut fail_data = HashMap::new();
        fail_data.insert(
            "status".to_string(),
            serde_json::Value::String("failed".to_string()),
        );
        fail_data.insert(
            "updated_at".to_string(),
            serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
        );
        if let Err(mark_err) = repo::purchases::update(ctx, purchase_id, fail_data).await {
            tracing::error!(
                error = %mark_err,
                purchase_id = %purchase_id,
                "could not mark partial purchase as failed; manual cleanup required"
            );
        }
    }
}

pub async fn handle_list_user(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let (page, page_size, _) = msg.pagination_params(20);

    let filters = vec![Filter {
        field: "user_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id),
    }];

    match repo::purchases::list_paginated(ctx, filters, page as i64, page_size as i64).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

pub async fn handle_list_admin(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (page, page_size, _) = msg.pagination_params(20);

    let mut filters = Vec::new();
    let status = msg.query("status").to_string();
    if !status.is_empty() {
        filters.push(Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(status),
        });
    }
    let user_id = msg.query("user_id").to_string();
    if !user_id.is_empty() {
        filters.push(Filter {
            field: "user_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id),
        });
    }

    match repo::purchases::list_paginated(ctx, filters, page as i64, page_size as i64).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

pub async fn handle_get(ctx: &dyn Context, msg: &Message) -> OutputStream {
    // Prefer the router-populated `{id}` path var (set by the endpoint
    // matcher), falling back to stripping the known prefixes for hand-built
    // test messages.
    let id = {
        let var = msg.var("id");
        if !var.is_empty() {
            var
        } else {
            msg.path()
                .strip_prefix("/admin/b/products/purchases/")
                .or_else(|| msg.path().strip_prefix("/b/products/purchases/"))
                .unwrap_or("")
                .trim_matches('/')
        }
    };
    if id.is_empty() {
        return err_bad_request("Missing purchase ID");
    }

    let purchase = match repo::purchases::get(ctx, id).await {
        Ok(p) => p,
        Err(e) if e.code == ErrorCode::NotFound => return err_not_found("Purchase not found"),
        Err(e) => return err_internal("Database error", e),
    };

    // Verify access: user can only view their own, admin can view all
    let purchase_user = purchase.str_field("user_id");
    if purchase_user != msg.user_id() && !crate::util::is_admin(msg) {
        return err_forbidden("Access denied");
    }

    // Get line items
    let line_items = repo::purchases::list_line_items(ctx, id)
        .await
        .unwrap_or_default();

    ok_json(&serde_json::json!({
        "purchase": purchase,
        "line_items": line_items
    }))
}

pub async fn handle_refund(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    // `/admin/b/products/purchases/{id}/refund` — prefer the matcher-bound
    // `{id}`, falling back to prefix/suffix stripping for hand-built tests.
    let id = {
        let var = msg.var("id");
        if !var.is_empty() {
            var.to_string()
        } else {
            msg.path()
                .strip_prefix("/admin/b/products/purchases/")
                .and_then(|s| s.strip_suffix("/refund"))
                .unwrap_or("")
                .to_string()
        }
    };
    if id.is_empty() {
        return err_bad_request("Missing purchase ID");
    }

    #[derive(serde::Deserialize, Default)]
    struct RefundReq {
        reason: Option<String>,
    }
    let raw = input.collect_to_bytes().await;
    let body: RefundReq = serde_json::from_slice(&raw).unwrap_or_default();

    // Verify purchase exists
    if let Err(e) = repo::purchases::get(ctx, &id).await {
        if e.code == ErrorCode::NotFound {
            return err_not_found("Purchase not found");
        }
        return err_internal("Database error", e);
    }

    // Atomic status transition: completed → refunded (prevents double-refund race)
    let refunded_by = msg.user_id().to_string();
    let reason_val = body.reason.unwrap_or_default();

    let rows = repo::purchases::refund_atomic(ctx, &id, &refunded_by, &reason_val)
        .await
        .unwrap_or(0);

    if rows == 0 {
        return err_bad_request(
            "Can only refund completed purchases (status may have changed concurrently)",
        );
    }

    // Fetch the updated record for the response
    match repo::purchases::get(ctx, &id).await {
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("Database error", e),
    }
}
