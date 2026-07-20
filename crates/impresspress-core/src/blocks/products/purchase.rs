use wafer_block::db::{Filter, FilterOp};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::{
    contracts::{RefundRequest, RefundResult, RefundResultStatus},
    repo, stripe_provider,
};
use crate::{
    http::{err_bad_request, err_forbidden, err_internal, err_not_found, ok_json},
    util::RecordExt,
};

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

pub async fn handle_list_seller(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let account = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
        Ok(Some(account)) => account,
        Ok(None) => return err_forbidden("Complete seller setup before viewing seller orders"),
        Err(error) => return err_internal("Database error", error),
    };
    let (page, page_size, _) = msg.pagination_params(20);
    let mut filters = vec![Filter {
        field: "seller_account_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::json!(account.id),
    }];
    let status = msg.query("status").to_string();
    if !status.is_empty() && status != "all" {
        filters.push(Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(status),
        });
    }
    match repo::purchases::list_paginated(ctx, filters, page as i64, page_size as i64).await {
        Ok(result) => ok_json(&result),
        Err(error) => err_internal("Database error", error),
    }
}

async fn purchase_response(
    ctx: &dyn Context,
    purchase: wafer_core::clients::database::Record,
) -> OutputStream {
    let line_items = match repo::purchases::list_line_items(ctx, &purchase.id).await {
        Ok(line_items) => line_items,
        Err(error) => return err_internal("Could not load purchase line items", error),
    };
    let refunds = match repo::refunds::list_for_purchase(ctx, &purchase.id).await {
        Ok(refunds) => refunds,
        Err(error) => return err_internal("Could not load purchase refunds", error),
    };
    let disputes = match repo::disputes::list_for_purchase(ctx, &purchase.id).await {
        Ok(disputes) => disputes,
        Err(error) => return err_internal("Could not load purchase disputes", error),
    };
    ok_json(&serde_json::json!({
        "purchase": purchase,
        "line_items": line_items,
        "refunds": refunds,
        "disputes": disputes
    }))
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

    // Verify access: a buyer can only view their own order; admin can view all.
    let purchase_user = if purchase.str_field("buyer_user_id").is_empty() {
        purchase.str_field("user_id")
    } else {
        purchase.str_field("buyer_user_id")
    };
    if purchase_user != msg.user_id() && !crate::util::is_admin(msg) {
        return err_forbidden("Access denied");
    }

    purchase_response(ctx, purchase).await
}

pub async fn handle_get_seller(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = msg.var("id");
    if id.is_empty() {
        return err_bad_request("Missing purchase ID");
    }
    let account = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
        Ok(Some(account)) => account,
        Ok(None) => return err_forbidden("Complete seller setup before viewing seller orders"),
        Err(error) => return err_internal("Database error", error),
    };
    let purchase = match repo::purchases::get(ctx, id).await {
        Ok(purchase) => purchase,
        Err(error) if error.code == ErrorCode::NotFound => {
            return err_not_found("Purchase not found")
        }
        Err(error) => return err_internal("Database error", error),
    };
    if purchase.str_field("seller_account_id") != account.id {
        return err_forbidden("Access denied");
    }
    purchase_response(ctx, purchase).await
}

fn refund_result(
    purchase: &wafer_core::clients::database::Record,
    refund: &wafer_core::clients::database::Record,
) -> RefundResult {
    RefundResult {
        purchase_id: purchase.id.clone(),
        refund_id: refund.id.clone(),
        provider_refund_id: refund.str_field("provider_refund_id").to_string(),
        status: match refund.str_field("status") {
            "succeeded" => RefundResultStatus::Succeeded,
            "failed" | "canceled" => RefundResultStatus::Failed,
            _ => RefundResultStatus::Pending,
        },
        provider_status: refund.str_field("provider_status").to_string(),
        amount_minor: refund.i64_field("amount_minor"),
        refunded_total_minor: purchase.i64_field("refunded_total_cents"),
        order_total_minor: purchase.i64_field("total_cents"),
        currency: purchase.str_field("currency").to_ascii_uppercase(),
        livemode: refund.bool_field("livemode"),
    }
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

fn admin_refund_id(msg: &Message) -> String {
    // `/admin/b/products/purchases/{id}/refund` — prefer the matcher-bound
    // `{id}`, falling back to prefix/suffix stripping for hand-built tests.
    {
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
    }
}

pub async fn handle_refund(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let id = admin_refund_id(msg);
    if id.is_empty() {
        return err_bad_request("Missing purchase ID");
    }
    refund_purchase(ctx, msg, input, id).await
}

pub async fn handle_seller_refund(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let id = msg.var("id").to_string();
    if id.is_empty() {
        return err_bad_request("Missing purchase ID");
    }
    let account = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
        Ok(Some(account)) => account,
        Ok(None) => return err_forbidden("Complete seller setup before refunding seller orders"),
        Err(error) => return err_internal("Database error", error),
    };
    let purchase = match repo::purchases::get(ctx, &id).await {
        Ok(purchase) => purchase,
        Err(error) if error.code == ErrorCode::NotFound => {
            return err_not_found("Purchase not found")
        }
        Err(error) => return err_internal("Database error", error),
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
    let raw = input.collect_to_bytes().await;
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
        Err(error) if error.code == ErrorCode::NotFound => {
            return err_not_found("Purchase not found")
        }
        Err(error) => return err_internal("Database error", error),
    };
    let has_payment_intent = !purchase.str_field("stripe_payment_intent_id").is_empty()
        || !purchase.str_field("provider_payment_intent_id").is_empty();
    if purchase.str_field("provider") != "stripe"
        && !has_payment_intent
        && !matches!(
            purchase.str_field("status"),
            "completed" | "partially_refunded"
        )
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
        if !matches!(
            purchase.str_field("status"),
            "completed" | "partially_refunded"
        ) {
            return err_bad_request("Purchase is not in a refundable state");
        }
        let remaining = total - refunded_total;
        let amount = body.amount_minor.unwrap_or(remaining);
        if amount <= 0 || amount > remaining {
            return err_bad_request("Refund amount exceeds the remaining refundable amount");
        }
        return match repo::purchases::reconcile_refund_total(
            ctx,
            &id,
            refunded_total + amount,
            &refunded_by,
            &note,
        )
        .await
        {
            Ok(updated) => ok_json(&manual_refund_result(&updated, amount)),
            Err(error) if error.code == ErrorCode::FailedPrecondition => {
                err_bad_request(&error.message)
            }
            Err(error) => err_internal("Could not record manual refund", error),
        };
    }
    if payment_intent_id.is_empty() {
        return err_bad_request("Stripe purchase does not have a PaymentIntent");
    }

    let existing = match repo::refunds::get_by_idempotency_key(ctx, &idempotency_key).await {
        Ok(existing) => existing,
        Err(error) => return err_internal("Could not inspect refund ledger", error),
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
        if !matches!(
            purchase.str_field("status"),
            "completed" | "partially_refunded"
        ) {
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
        Err(error) => return err_internal("Could not claim refund operation", error),
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
        Err(error) => return err_internal("Could not enqueue refund reconciliation", error),
    };

    if refund.str_field("status") == "succeeded" {
        if let Err(error) = repo::provider_operations::resolve_unleased(
            ctx,
            &provider_operation.id,
            true,
            refund.str_field("response_json"),
            "",
        )
        .await
        {
            return err_internal("Could not complete refund reconciliation operation", error);
        }
        let current = match repo::purchases::get(ctx, &id).await {
            Ok(current) => current,
            Err(error) => return err_internal("Could not load refunded purchase", error),
        };
        return ok_json(&refund_result(&current, &refund));
    }
    if refund.str_field("status") == "provider_succeeded" {
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
            Err(error) => return err_internal("Could not reconcile successful refund", error),
        };
        refund = match repo::refunds::mark_succeeded(ctx, &refund.id).await {
            Ok(refund) => refund,
            Err(error) => return err_internal("Could not complete refund ledger", error),
        };
        if let Err(error) = repo::provider_operations::resolve_unleased(
            ctx,
            &provider_operation.id,
            true,
            refund.str_field("response_json"),
            "",
        )
        .await
        {
            return err_internal("Could not complete refund reconciliation operation", error);
        }
        return ok_json(&refund_result(&current, &refund));
    }
    if refund.str_field("status") == "pending" && !refund.str_field("provider_refund_id").is_empty()
    {
        return ok_json(&refund_result(&purchase, &refund));
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
        Err(error) => return err_internal("Could not record Stripe refund response", error),
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
                return err_internal("Could not resolve failed refund operation", error);
            }
        }
        return ok_json(&refund_result(&purchase, &refund));
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
        Err(error) => return err_internal("Could not reconcile successful Stripe refund", error),
    };
    refund = match repo::refunds::mark_succeeded(ctx, &refund.id).await {
        Ok(refund) => refund,
        Err(error) => return err_internal("Could not complete refund ledger", error),
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
        return err_internal("Could not complete refund reconciliation operation", error);
    }
    ok_json(&refund_result(&updated, &refund))
}
