use wafer_block::db::{Filter, FilterOp};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::{
    contracts::{
        AdminPurchaseListQuery, BuyerOrderDetailResponse, BuyerOrderListResponse, BuyerOrderView,
        BuyerRefundView, DisputeView, LineItemView, OrderStatus, PageQuery, PurchaseDetailResponse,
        PurchaseListResponse, PurchaseView, RefundRequest, RefundResult, RefundResultStatus,
        RefundStatus, RefundView, SellerOrderDetailResponse, SellerOrderListQuery,
        SellerOrderListResponse, SellerOrderView,
    },
    repo, stripe_provider,
};
use crate::{
    blocks::crud,
    http::{err_bad_request, err_forbidden, err_internal, ok_json},
    util::RecordExt,
};

/// One page of order rows, newest first.
///
/// Every tier reads the same table through the same query; what differs is
/// the projection, so each caller below names the view its own tier is
/// allowed to see. That is the whole point of having three: the row is not
/// filtered at runtime, it is described by a different type, and the type is
/// what `/openapi.json` and the WebMCP manifest publish.
async fn order_page(
    ctx: &dyn Context,
    filters: Vec<Filter>,
    page: u32,
    page_size: u32,
) -> Result<wafer_core::clients::database::RecordList, OutputStream> {
    repo::purchases::list_paginated(ctx, filters, i64::from(page), i64::from(page_size))
        .await
        .map_err(|e| crud::db_error_internal(e, "Database error"))
}

pub async fn handle_list_user(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let query = PageQuery::from_message(msg);

    let filters = vec![Filter {
        field: "user_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id),
    }];
    match order_page(ctx, filters, query.page, query.page_size).await {
        Ok(result) => ok_json(&BuyerOrderListResponse::from_record_list(&result)),
        Err(out) => out,
    }
}

pub async fn handle_list_admin(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = AdminPurchaseListQuery::from_message(msg);

    let mut filters = Vec::new();
    if let Some(status) = &query.status {
        filters.push(Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(status.clone()),
        });
    }
    if let Some(user_id) = &query.user_id {
        filters.push(Filter {
            field: "user_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id.clone()),
        });
    }
    match order_page(ctx, filters, query.page, query.page_size).await {
        Ok(result) => ok_json(&PurchaseListResponse::from_record_list(&result)),
        Err(out) => out,
    }
}

pub async fn handle_list_seller(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let account = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
        Ok(Some(account)) => account,
        Ok(None) => return err_forbidden("Complete seller setup before viewing seller orders"),
        Err(error) => return crud::db_error_internal(error, "Database error"),
    };
    let query = SellerOrderListQuery::from_message(msg);
    let mut filters = vec![Filter {
        field: "seller_account_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::json!(account.id),
    }];
    if let Some(status) = &query.status {
        filters.push(Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(status),
        });
    }
    match order_page(ctx, filters, query.page, query.page_size).await {
        Ok(result) => ok_json(&SellerOrderListResponse::from_record_list(&result)),
        Err(out) => out,
    }
}

/// The rows that hang off one order. Shared by all three detail tiers; the
/// projection applied to them is not.
struct OrderRelations {
    line_items: Vec<wafer_core::clients::database::Record>,
    refunds: Vec<wafer_core::clients::database::Record>,
    disputes: Vec<wafer_core::clients::database::Record>,
}

async fn order_relations(
    ctx: &dyn Context,
    purchase_id: &str,
) -> Result<OrderRelations, OutputStream> {
    Ok(OrderRelations {
        line_items: repo::purchases::list_line_items(ctx, purchase_id)
            .await
            .map_err(|e| crud::db_error_internal(e, "Could not load purchase line items"))?,
        refunds: repo::refunds::list_for_purchase(ctx, purchase_id)
            .await
            .map_err(|e| crud::db_error_internal(e, "Could not load purchase refunds"))?,
        disputes: repo::disputes::list_for_purchase(ctx, purchase_id)
            .await
            .map_err(|e| crud::db_error_internal(e, "Could not load purchase disputes"))?,
    })
}

/// The child rows of one order, projected, or the 500 a row outside its
/// contract earns.
///
/// A *detail* response fails loudly where [`PurchaseListResponse::from_record_list`]
/// degrades: a page of orders that drops one unreadable row still answers the
/// question the caller asked, but an order detail that silently omits a refund
/// or a dispute answers it wrongly. The row is the response here.
fn child_rows<T>(
    rows: impl IntoIterator<Item = Result<T, wafer_run::WaferError>>,
    entity: &str,
) -> Result<Vec<T>, OutputStream> {
    rows.into_iter()
        .collect::<Result<Vec<T>, _>>()
        .map_err(|error| err_internal(&format!("{entity} row is outside the contract"), error))
}

/// The caller's own order. Disputes are not included: a dispute is a matter
/// between the seller, the platform and the provider.
async fn buyer_order_response(
    ctx: &dyn Context,
    purchase: wafer_core::clients::database::Record,
) -> OutputStream {
    let relations = match order_relations(ctx, &purchase.id).await {
        Ok(relations) => relations,
        Err(out) => return out,
    };
    let view = match BuyerOrderView::from_record(&purchase) {
        Ok(view) => view,
        Err(error) => return err_internal("Order row is outside the contract", error),
    };
    let refunds = match child_rows(
        relations.refunds.iter().map(BuyerRefundView::from_record),
        "Refund",
    ) {
        Ok(refunds) => refunds,
        Err(out) => return out,
    };
    let disputes = match child_rows(
        relations.disputes.iter().map(DisputeView::from_record),
        "Dispute",
    ) {
        Ok(disputes) => disputes,
        Err(out) => return out,
    };
    ok_json(&BuyerOrderDetailResponse {
        purchase: view,
        line_items: relations
            .line_items
            .iter()
            .map(LineItemView::from_record)
            .collect(),
        refunds,
        disputes,
    })
}

/// A seller-owned order, as the seller fulfilling it may read it.
async fn seller_order_response(
    ctx: &dyn Context,
    purchase: wafer_core::clients::database::Record,
) -> OutputStream {
    let relations = match order_relations(ctx, &purchase.id).await {
        Ok(relations) => relations,
        Err(out) => return out,
    };
    let view = match SellerOrderView::from_record(&purchase) {
        Ok(view) => view,
        Err(error) => return err_internal("Order row is outside the contract", error),
    };
    let refunds = match child_rows(
        relations.refunds.iter().map(RefundView::from_record),
        "Refund",
    ) {
        Ok(refunds) => refunds,
        Err(out) => return out,
    };
    let disputes = match child_rows(
        relations.disputes.iter().map(DisputeView::from_record),
        "Dispute",
    ) {
        Ok(disputes) => disputes,
        Err(out) => return out,
    };
    ok_json(&SellerOrderDetailResponse {
        purchase: view,
        line_items: relations
            .line_items
            .iter()
            .map(LineItemView::from_record)
            .collect(),
        refunds,
        disputes,
    })
}

/// The whole row. Admin is the one tier that legitimately reads every column.
async fn purchase_response(
    ctx: &dyn Context,
    purchase: wafer_core::clients::database::Record,
) -> OutputStream {
    let relations = match order_relations(ctx, &purchase.id).await {
        Ok(relations) => relations,
        Err(out) => return out,
    };
    let view = match PurchaseView::from_record(&purchase) {
        Ok(view) => view,
        Err(error) => return err_internal("Order row is outside the contract", error),
    };
    let refunds = match child_rows(
        relations.refunds.iter().map(RefundView::from_record),
        "Refund",
    ) {
        Ok(refunds) => refunds,
        Err(out) => return out,
    };
    let disputes = match child_rows(
        relations.disputes.iter().map(DisputeView::from_record),
        "Dispute",
    ) {
        Ok(disputes) => disputes,
        Err(out) => return out,
    };
    ok_json(&PurchaseDetailResponse {
        purchase: view,
        line_items: relations
            .line_items
            .iter()
            .map(LineItemView::from_record)
            .collect(),
        refunds,
        disputes,
    })
}

pub async fn handle_get(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Purchase") {
        Ok(value) => value,
        Err(response) => return response,
    };

    let purchase = match repo::purchases::get(ctx, id).await {
        Ok(p) => p,
        Err(e) => return crud::db_error(e, "Purchase not found", "Database error"),
    };

    // A buyer may read only their own order. An admin reading this same path
    // gets the buyer projection, which is the honest answer for an endpoint
    // that declares `BuyerOrderDetailResponse`; the admin surface is
    // `/b/products/api/admin/purchases/{id}`, which declares the full row.
    let purchase_user = if purchase.str_field("buyer_user_id").is_empty() {
        purchase.str_field("user_id")
    } else {
        purchase.str_field("buyer_user_id")
    };
    if purchase_user != msg.user_id() && !crate::util::is_admin(msg) {
        return err_forbidden("Access denied");
    }

    buyer_order_response(ctx, purchase).await
}

/// `GET /b/products/api/admin/purchases/{id}` — the full row.
///
/// Split from [`handle_get`] because the two endpoints declare different
/// output types: routing already gates this one at `AuthLevel::Admin`, so
/// there is no ownership check to make here.
pub async fn handle_get_admin(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Purchase") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let purchase = match repo::purchases::get(ctx, id).await {
        Ok(p) => p,
        Err(e) => return crud::db_error(e, "Purchase not found", "Database error"),
    };
    purchase_response(ctx, purchase).await
}

pub async fn handle_get_seller(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Purchase") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let account = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
        Ok(Some(account)) => account,
        Ok(None) => return err_forbidden("Complete seller setup before viewing seller orders"),
        Err(error) => return crud::db_error_internal(error, "Database error"),
    };
    let purchase = match repo::purchases::get(ctx, id).await {
        Ok(purchase) => purchase,
        Err(error) => return crud::db_error(error, "Purchase not found", "Database error"),
    };
    if purchase.str_field("seller_account_id") != account.id {
        return err_forbidden("Access denied");
    }
    seller_order_response(ctx, purchase).await
}

/// The refund ledger state of a row, or the response a value outside the
/// contract earns.
///
/// The ledger's own column, not the provider's: `provider_status` beside it
/// carries Stripe's vocabulary and stays a string.
fn refund_status(
    record: &wafer_core::clients::database::Record,
) -> Result<RefundStatus, OutputStream> {
    repo::refunds::status_of(record)
        .map_err(|error| err_internal("Refund row is outside the contract", error))
}

/// One refund's outcome as the client is told it, or the 500 an undecodable
/// ledger row earns.
fn refund_json(
    purchase: &wafer_core::clients::database::Record,
    refund: &wafer_core::clients::database::Record,
) -> OutputStream {
    match refund_result(purchase, refund) {
        Ok(result) => ok_json(&result),
        Err(out) => out,
    }
}

fn refund_result(
    purchase: &wafer_core::clients::database::Record,
    refund: &wafer_core::clients::database::Record,
) -> Result<RefundResult, OutputStream> {
    // Three ledger states collapse into two answers: the API's
    // `RefundResultStatus` says only whether the money is back, and
    // `provider_succeeded` — the provider has paid but the order's refunded
    // total has not been settled — is not yet "back". It has always been
    // reported as `pending`, and this match is where that stops being an
    // accident of a `_` arm. The `_` arm also matched `canceled`, which is a
    // value of the row's `provider_status` and never of its `status`.
    Ok(RefundResult {
        purchase_id: purchase.id.clone(),
        refund_id: refund.id.clone(),
        provider_refund_id: refund.str_field("provider_refund_id").to_string(),
        status: match refund_status(refund)? {
            RefundStatus::Succeeded => RefundResultStatus::Succeeded,
            RefundStatus::Failed => RefundResultStatus::Failed,
            RefundStatus::Pending | RefundStatus::ProviderSucceeded => RefundResultStatus::Pending,
        },
        provider_status: refund.str_field("provider_status").to_string(),
        amount_minor: refund.i64_field("amount_minor"),
        refunded_total_minor: purchase.i64_field("refunded_total_cents"),
        order_total_minor: purchase.i64_field("total_cents"),
        currency: purchase.str_field("currency").to_ascii_uppercase(),
        livemode: refund.bool_field("livemode"),
    })
}

fn manual_refund_result(
    purchase: &wafer_core::clients::database::Record,
    amount_minor: i64,
) -> RefundResult {
    RefundResult {
        purchase_id: purchase.id.clone(),
        refund_id: String::new(),
        provider_refund_id: String::new(),
        status: RefundResultStatus::Succeeded,
        provider_status: "manual".to_string(),
        amount_minor,
        refunded_total_minor: purchase.i64_field("refunded_total_cents"),
        order_total_minor: purchase.i64_field("total_cents"),
        currency: purchase.str_field("currency").to_ascii_uppercase(),
        livemode: false,
    }
}

fn valid_refund_operation_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub async fn handle_refund(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let id = match crud::path_id(msg, "Purchase") {
        Ok(value) => value.to_string(),
        Err(response) => return response,
    };
    refund_purchase(ctx, msg, input, id).await
}

pub async fn handle_seller_refund(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let id = match crud::path_id(msg, "Purchase") {
        Ok(value) => value.to_string(),
        Err(response) => return response,
    };
    let account = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
        Ok(Some(account)) => account,
        Ok(None) => return err_forbidden("Complete seller setup before refunding seller orders"),
        Err(error) => return crud::db_error_internal(error, "Database error"),
    };
    let purchase = match repo::purchases::get(ctx, &id).await {
        Ok(purchase) => purchase,
        Err(error) => return crud::db_error(error, "Purchase not found", "Database error"),
    };
    if purchase.str_field("seller_account_id") != account.id {
        return err_forbidden("Access denied");
    }
    refund_purchase(ctx, msg, input, id).await
}

async fn refund_purchase(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    id: String,
) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    // An absent body is a legitimate "no reason given" (every caller today
    // sends `{}` for that, but a genuinely empty body is treated the same
    // way defensively). A NON-empty body that fails to parse is malformed
    // input and must be rejected — it must not silently become "no reason",
    // which would hide a client bug (or a truncated/garbled request) behind
    // a refund whose reason was silently dropped.
    let body: RefundRequest = if raw.is_empty() {
        RefundRequest::default()
    } else {
        match serde_json::from_slice(&raw) {
            Ok(b) => b,
            Err(e) => return err_bad_request(&format!("Invalid request body: {e}")),
        }
    };

    let note = body.note.unwrap_or_default().trim().to_string();
    if note.chars().count() > 500 {
        return err_bad_request("Refund note must be 500 characters or fewer");
    }
    if body.amount_minor.is_some_and(|amount| amount <= 0) {
        return err_bad_request("Refund amount_minor must be positive");
    }
    let client_key = match body.idempotency_key.as_deref() {
        Some(value) if valid_refund_operation_key(value) => value.to_string(),
        Some(_) => {
            return err_bad_request(
                "idempotency_key must be 1-80 letters, numbers, underscores, or hyphens",
            )
        }
        None if body.amount_minor.is_none() => "full".to_string(),
        None => format!("amount_{}", body.amount_minor.unwrap_or_default()),
    };
    let idempotency_key = format!("impresspress_refund_{id}_{client_key}");
    if idempotency_key.len() > 255 {
        return err_bad_request("Refund idempotency key is too long");
    }

    let purchase = match repo::purchases::get(ctx, &id).await {
        Ok(purchase) => purchase,
        Err(error) => return crud::db_error(error, "Purchase not found", "Database error"),
    };
    let order_status = match OrderStatus::from_record(&purchase) {
        Ok(status) => status,
        Err(error) => return err_internal("Order row is outside the contract", error),
    };
    let has_payment_intent = !purchase.str_field("stripe_payment_intent_id").is_empty()
        || !purchase.str_field("provider_payment_intent_id").is_empty();
    if purchase.str_field("provider") != "stripe"
        && !has_payment_intent
        && !order_status.is_refundable()
    {
        return err_bad_request("Purchase is not in a refundable state");
    }
    let total = purchase.i64_field("total_cents");
    let refunded_total = purchase.i64_field("refunded_total_cents");
    if total <= 0 || refunded_total < 0 || refunded_total > total {
        return err_internal(
            "Purchase has invalid refund accounting",
            wafer_run::WaferError::new(ErrorCode::Internal, "invalid purchase refund totals"),
        );
    }

    let payment_intent_id = {
        let current = purchase.str_field("stripe_payment_intent_id");
        if current.is_empty() {
            purchase.str_field("provider_payment_intent_id").to_string()
        } else {
            current.to_string()
        }
    };
    let is_stripe = purchase.str_field("provider") == "stripe" || !payment_intent_id.is_empty();
    let refunded_by = msg.user_id().to_string();

    if !is_stripe {
        // Manual refunds go through the same refund ledger as Stripe refunds:
        // the ledger row pins the absolute refunded-total target at claim
        // time, so a retried delivery (same idempotency key) returns the
        // recorded outcome instead of deducting a second time.
        let existing = match repo::refunds::get_by_idempotency_key(ctx, &idempotency_key).await {
            Ok(existing) => existing,
            Err(error) => return crud::db_error_internal(error, "Could not inspect refund ledger"),
        };
        let claim = if let Some(existing) = existing {
            if existing.str_field("purchase_id") != id {
                return err_bad_request("Refund idempotency key belongs to another purchase");
            }
            if body
                .amount_minor
                .is_some_and(|amount| amount != existing.i64_field("amount_minor"))
                || (body.amount_minor.is_none()
                    && existing.i64_field("target_refunded_total_minor") != total)
                || (!note.is_empty() && note != existing.str_field("note"))
            {
                return err_bad_request(
                    "Refund idempotency key was already used for a different request",
                );
            }
            let existing_status = match refund_status(&existing) {
                Ok(status) => status,
                Err(out) => return out,
            };
            if existing_status == RefundStatus::Succeeded {
                let current = match repo::purchases::get(ctx, &id).await {
                    Ok(current) => current,
                    Err(error) => {
                        return crud::db_error_internal(error, "Could not load refunded purchase")
                    }
                };
                return ok_json(&manual_refund_result(
                    &current,
                    existing.i64_field("amount_minor"),
                ));
            }
            repo::refunds::RefundClaim {
                purchase_id: id.clone(),
                payment_intent_id: String::new(),
                stripe_account_id: String::new(),
                idempotency_key: idempotency_key.clone(),
                amount_minor: existing.i64_field("amount_minor"),
                target_refunded_total_minor: existing.i64_field("target_refunded_total_minor"),
                currency: existing.str_field("currency").to_string(),
                provider_reason: String::new(),
                note: existing.str_field("note").to_string(),
                refunded_by: existing.str_field("refunded_by").to_string(),
                livemode: existing.bool_field("livemode"),
            }
        } else {
            if !order_status.is_refundable() {
                return err_bad_request("Purchase is not in a refundable state");
            }
            let remaining = total - refunded_total;
            let amount = body.amount_minor.unwrap_or(remaining);
            if amount <= 0 || amount > remaining {
                return err_bad_request("Refund amount exceeds the remaining refundable amount");
            }
            repo::refunds::RefundClaim {
                purchase_id: id.clone(),
                payment_intent_id: String::new(),
                stripe_account_id: String::new(),
                idempotency_key: idempotency_key.clone(),
                amount_minor: amount,
                target_refunded_total_minor: refunded_total + amount,
                currency: purchase.str_field("currency").to_ascii_uppercase(),
                provider_reason: String::new(),
                note: note.clone(),
                refunded_by: refunded_by.clone(),
                livemode: purchase.bool_field("livemode"),
            }
        };
        let refund = match repo::refunds::claim(ctx, &claim).await {
            Ok(refund) => refund,
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::InvalidArgument | ErrorCode::FailedPrecondition
                ) =>
            {
                return err_bad_request(&error.message)
            }
            Err(error) => {
                return crud::db_error_internal(error, "Could not claim refund operation")
            }
        };
        // Apply against the claimed absolute target so a retry of an
        // interrupted operation converges instead of deducting again.
        return match repo::purchases::reconcile_refund_total(
            ctx,
            &id,
            refund.i64_field("target_refunded_total_minor"),
            &refunded_by,
            &note,
        )
        .await
        {
            Ok(updated) => {
                if let Err(error) = repo::refunds::mark_succeeded(ctx, &refund.id).await {
                    return crud::db_error_internal(
                        error,
                        "Could not record manual refund outcome",
                    );
                }
                ok_json(&manual_refund_result(
                    &updated,
                    refund.i64_field("amount_minor"),
                ))
            }
            Err(error) if error.code == ErrorCode::FailedPrecondition => {
                err_bad_request(&error.message)
            }
            Err(error) => crud::db_error_internal(error, "Could not record manual refund"),
        };
    }
    if payment_intent_id.is_empty() {
        return err_bad_request("Stripe purchase does not have a PaymentIntent");
    }

    let existing = match repo::refunds::get_by_idempotency_key(ctx, &idempotency_key).await {
        Ok(existing) => existing,
        Err(error) => return crud::db_error_internal(error, "Could not inspect refund ledger"),
    };
    let provider_reason = body
        .provider_reason
        .map(|reason| reason.as_str().to_string())
        .unwrap_or_default();
    let claim = if let Some(existing) = existing {
        if existing.str_field("purchase_id") != id {
            return err_bad_request("Refund idempotency key belongs to another purchase");
        }
        if body
            .amount_minor
            .is_some_and(|amount| amount != existing.i64_field("amount_minor"))
            || (body.amount_minor.is_none()
                && existing.i64_field("target_refunded_total_minor") != total)
            || (!provider_reason.is_empty()
                && provider_reason != existing.str_field("provider_reason"))
            || (!note.is_empty() && note != existing.str_field("note"))
        {
            return err_bad_request(
                "Refund idempotency key was already used for a different request",
            );
        }
        repo::refunds::RefundClaim {
            purchase_id: id.clone(),
            payment_intent_id: existing.str_field("payment_intent_id").to_string(),
            stripe_account_id: existing.str_field("stripe_account_id").to_string(),
            idempotency_key: idempotency_key.clone(),
            amount_minor: existing.i64_field("amount_minor"),
            target_refunded_total_minor: existing.i64_field("target_refunded_total_minor"),
            currency: existing.str_field("currency").to_string(),
            provider_reason: existing.str_field("provider_reason").to_string(),
            note: existing.str_field("note").to_string(),
            refunded_by: existing.str_field("refunded_by").to_string(),
            livemode: existing.bool_field("livemode"),
        }
    } else {
        if !order_status.is_refundable() {
            return err_bad_request("Purchase is not in a refundable state");
        }
        let remaining = total - refunded_total;
        let amount = body.amount_minor.unwrap_or(remaining);
        if amount <= 0 || amount > remaining {
            return err_bad_request("Refund amount exceeds the remaining refundable amount");
        }
        repo::refunds::RefundClaim {
            purchase_id: id.clone(),
            payment_intent_id: payment_intent_id.clone(),
            stripe_account_id: purchase.str_field("stripe_account_id").to_string(),
            idempotency_key: idempotency_key.clone(),
            amount_minor: amount,
            target_refunded_total_minor: refunded_total + amount,
            currency: purchase.str_field("currency").to_ascii_uppercase(),
            provider_reason,
            note: note.clone(),
            refunded_by: refunded_by.clone(),
            livemode: purchase.bool_field("livemode"),
        }
    };
    let mut refund = match repo::refunds::claim(ctx, &claim).await {
        Ok(refund) => refund,
        Err(error)
            if matches!(
                error.code,
                ErrorCode::InvalidArgument | ErrorCode::FailedPrecondition
            ) =>
        {
            return err_bad_request(&error.message)
        }
        Err(error) => return crud::db_error_internal(error, "Could not claim refund operation"),
    };
    let provider_operation = match repo::provider_operations::ensure(
        ctx,
        repo::provider_operations::REFUND_RECONCILE,
        "refund",
        &refund.id,
        refund.str_field("stripe_account_id"),
        refund.str_field("idempotency_key"),
        "{\"version\":1}",
    )
    .await
    {
        Ok(operation) => operation,
        Err(error) => {
            return crud::db_error_internal(error, "Could not enqueue refund reconciliation")
        }
    };

    let claimed_status = match refund_status(&refund) {
        Ok(status) => status,
        Err(out) => return out,
    };
    if claimed_status == RefundStatus::Succeeded {
        if let Err(error) = repo::provider_operations::resolve_unleased(
            ctx,
            &provider_operation.id,
            true,
            &refund.json_text_field("response_json"),
            "",
        )
        .await
        {
            return crud::db_error_internal(
                error,
                "Could not complete refund reconciliation operation",
            );
        }
        let current = match repo::purchases::get(ctx, &id).await {
            Ok(current) => current,
            Err(error) => {
                return crud::db_error_internal(error, "Could not load refunded purchase")
            }
        };
        return refund_json(&current, &refund);
    }
    if claimed_status == RefundStatus::ProviderSucceeded {
        let current = match repo::purchases::reconcile_refund_total(
            ctx,
            &id,
            refund.i64_field("target_refunded_total_minor"),
            refund.str_field("refunded_by"),
            refund.str_field("note"),
        )
        .await
        {
            Ok(current) => current,
            Err(error) => {
                return crud::db_error_internal(error, "Could not reconcile successful refund")
            }
        };
        refund = match repo::refunds::mark_succeeded(ctx, &refund.id).await {
            Ok(refund) => refund,
            Err(error) => {
                return crud::db_error_internal(error, "Could not complete refund ledger")
            }
        };
        if let Err(error) = repo::provider_operations::resolve_unleased(
            ctx,
            &provider_operation.id,
            true,
            &refund.json_text_field("response_json"),
            "",
        )
        .await
        {
            return crud::db_error_internal(
                error,
                "Could not complete refund reconciliation operation",
            );
        }
        return refund_json(&current, &refund);
    }
    if claimed_status == RefundStatus::Pending && !refund.str_field("provider_refund_id").is_empty()
    {
        return refund_json(&purchase, &refund);
    }

    let params = stripe_provider::StripeRefundParams {
        purchase_id: id.clone(),
        payment_intent_id: refund.str_field("payment_intent_id").to_string(),
        stripe_account_id: refund.str_field("stripe_account_id").to_string(),
        idempotency_key: refund.str_field("idempotency_key").to_string(),
        amount_minor: refund.i64_field("amount_minor"),
        provider_reason: refund.str_field("provider_reason").to_string(),
        refund_application_fee: !refund.str_field("stripe_account_id").is_empty()
            && purchase.i64_field("platform_fee_cents") > 0,
        expected_livemode: purchase.bool_field("livemode"),
    };
    let provider = match stripe_provider::create_refund(ctx, &params).await {
        Ok(provider) => provider,
        Err(error) => {
            let ledger_update = if error.code == ErrorCode::Internal {
                repo::refunds::mark_retryable_error(ctx, &refund.id, &error.message).await
            } else {
                repo::refunds::mark_failed(ctx, &refund.id, &error.message).await
            };
            if let Err(update_error) = ledger_update {
                tracing::error!(
                    error = %update_error,
                    refund_id = %refund.id,
                    "could not record Stripe refund failure"
                );
            }
            if error.code != ErrorCode::Internal {
                if let Err(update_error) = repo::provider_operations::resolve_unleased(
                    ctx,
                    &provider_operation.id,
                    false,
                    "{}",
                    &error.message,
                )
                .await
                {
                    tracing::error!(
                        error = %update_error,
                        operation_id = %provider_operation.id,
                        "could not terminally resolve rejected refund operation"
                    );
                }
            }
            return if matches!(
                error.code,
                ErrorCode::InvalidArgument | ErrorCode::FailedPrecondition
            ) {
                err_bad_request(&error.message)
            } else {
                err_internal("Stripe refund could not be completed", error)
            };
        }
    };
    let response_json = serde_json::json!({
        "id": provider.id,
        "status": provider.status,
        "amount_minor": provider.amount_minor,
        "livemode": provider.livemode,
    })
    .to_string();
    refund = match repo::refunds::record_provider_response(
        ctx,
        &refund.id,
        &provider.id,
        &provider.status,
        provider.livemode,
        &response_json,
    )
    .await
    {
        Ok(refund) => refund,
        Err(error) => {
            return crud::db_error_internal(error, "Could not record Stripe refund response")
        }
    };
    if provider.status != "succeeded" {
        if matches!(provider.status.as_str(), "failed" | "canceled") {
            if let Err(error) = repo::provider_operations::resolve_unleased(
                ctx,
                &provider_operation.id,
                false,
                &response_json,
                "Stripe refund failed or was canceled",
            )
            .await
            {
                return crud::db_error_internal(error, "Could not resolve failed refund operation");
            }
        }
        return refund_json(&purchase, &refund);
    }
    let updated = match repo::purchases::reconcile_refund_total(
        ctx,
        &id,
        refund.i64_field("target_refunded_total_minor"),
        refund.str_field("refunded_by"),
        refund.str_field("note"),
    )
    .await
    {
        Ok(updated) => updated,
        Err(error) => {
            return crud::db_error_internal(error, "Could not reconcile successful Stripe refund")
        }
    };
    refund = match repo::refunds::mark_succeeded(ctx, &refund.id).await {
        Ok(refund) => refund,
        Err(error) => return crud::db_error_internal(error, "Could not complete refund ledger"),
    };
    if let Err(error) = repo::provider_operations::resolve_unleased(
        ctx,
        &provider_operation.id,
        true,
        &response_json,
        "",
    )
    .await
    {
        return crud::db_error_internal(
            error,
            "Could not complete refund reconciliation operation",
        );
    }
    refund_json(&updated, &refund)
}
