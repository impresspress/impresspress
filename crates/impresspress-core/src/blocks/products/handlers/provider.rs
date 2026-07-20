//! HTTP handlers for Stripe connection health and seller Connect workflows.

use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream, WaferError};

use crate::{
    blocks::products::{
        contracts::{
            BillingPortalRequest, ProviderOperationList, ProviderOperationSummary,
            SellerOnboardingRequest,
        },
        repo, stripe, stripe_provider,
    },
    http::{
        err_bad_request, err_forbidden, err_internal, err_not_found, err_unauthorized, ok_json,
    },
    util::{path_param, RecordExt},
};

fn optional_string(record: &wafer_core::clients::database::Record, field: &str) -> Option<String> {
    record
        .data
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn provider_operation_summary(
    record: wafer_core::clients::database::Record,
) -> ProviderOperationSummary {
    ProviderOperationSummary {
        id: record.id.clone(),
        operation_type: record.str_field("operation_type").to_string(),
        aggregate_type: record.str_field("aggregate_type").to_string(),
        aggregate_id: record.str_field("aggregate_id").to_string(),
        stripe_account_id: record.str_field("stripe_account_id").to_string(),
        status: record.str_field("status").to_string(),
        attempts: record.u64_field("attempts"),
        processing_started_at: optional_string(&record, "processing_started_at"),
        next_attempt_at: optional_string(&record, "next_attempt_at"),
        last_error: record.str_field("last_error").to_string(),
        completed_at: optional_string(&record, "completed_at"),
        terminal_at: optional_string(&record, "terminal_at"),
        created_at: record.str_field("created_at").to_string(),
        updated_at: record.str_field("updated_at").to_string(),
    }
}

fn provider_error(message: &str, error: WaferError) -> OutputStream {
    match error.code {
        ErrorCode::InvalidArgument | ErrorCode::FailedPrecondition => {
            err_bad_request(&error.message)
        }
        ErrorCode::PermissionDenied => err_forbidden(&error.message),
        ErrorCode::NotFound => err_not_found(&error.message),
        _ => err_internal(message, error),
    }
}

pub(super) async fn connection_status(ctx: &dyn Context) -> OutputStream {
    ok_json(&stripe_provider::connection_status(ctx).await)
}

pub(super) async fn webhook_events(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let status = msg.query("status").trim().to_string();
    if !status.is_empty()
        && !matches!(
            status.as_str(),
            "pending" | "processing" | "failed" | "processed" | "dead_letter"
        )
    {
        return err_bad_request("invalid webhook event status filter");
    }
    let (page, page_size, _) = msg.pagination_params(20);
    match stripe::list_webhook_events(
        ctx,
        (!status.is_empty()).then_some(status.as_str()),
        page as i64,
        page_size.min(100) as i64,
    )
    .await
    {
        Ok(events) => ok_json(&events),
        Err(error) => provider_error("Could not list Stripe webhook events", error),
    }
}

pub(super) async fn replay_webhook_event(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let event_id = path_param(msg, "id", "/admin/b/products/webhook-events/").trim();
    if event_id.is_empty() {
        return err_bad_request("webhook event id is required");
    }
    match stripe::replay_webhook_event(ctx, event_id).await {
        Ok(output) => output,
        Err(error) => provider_error("Could not replay Stripe webhook event", error),
    }
}

pub(super) async fn provider_operations(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let status = msg.query("status").trim().to_string();
    if !status.is_empty()
        && !matches!(
            status.as_str(),
            "pending" | "processing" | "failed" | "succeeded" | "dead_letter"
        )
    {
        return err_bad_request("invalid provider operation status filter");
    }
    let (page, page_size, _) = msg.pagination_params(20);
    match repo::provider_operations::list(
        ctx,
        (!status.is_empty()).then_some(status.as_str()),
        page as i64,
        page_size.min(100) as i64,
    )
    .await
    {
        Ok(result) => ok_json(&ProviderOperationList {
            records: result
                .records
                .into_iter()
                .map(provider_operation_summary)
                .collect(),
            total_count: result.total_count,
            page: result.page,
            page_size: result.page_size,
        }),
        Err(error) => provider_error("Could not list provider operations", error),
    }
}

pub(super) async fn reconcile_provider_operations(
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let limit = if msg.query("limit").trim().is_empty() {
        20
    } else {
        match msg.query("limit").trim().parse::<usize>() {
            Ok(limit @ 1..=100) => limit,
            _ => return err_bad_request("limit must be an integer from 1 to 100"),
        }
    };
    match stripe_provider::reconcile_provider_operations(ctx, limit).await {
        Ok(result) => ok_json(&result),
        Err(error) => provider_error("Could not reconcile provider operations", error),
    }
}

pub(super) async fn seller_status(ctx: &dyn Context, msg: &Message) -> OutputStream {
    if msg.user_id().is_empty() {
        return err_unauthorized("Authentication required");
    }
    match stripe_provider::seller_status(ctx, msg.user_id()).await {
        Ok(account) => ok_json(&account),
        Err(error) => provider_error("Could not load seller Stripe account", error),
    }
}

pub(super) async fn seller_onboarding(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    if msg.user_id().is_empty() {
        return err_unauthorized("Authentication required");
    }
    let raw = input.collect_to_bytes().await;
    let request: SellerOnboardingRequest = match serde_json::from_slice(&raw) {
        Ok(request) => request,
        Err(error) => return err_bad_request(&format!("Invalid request body: {error}")),
    };
    match stripe_provider::start_seller_onboarding(ctx, msg.user_id(), &request).await {
        Ok(response) => ok_json(&response),
        Err(error) => provider_error("Could not start Stripe onboarding", error),
    }
}

pub(super) async fn seller_dashboard(ctx: &dyn Context, msg: &Message) -> OutputStream {
    if msg.user_id().is_empty() {
        return err_unauthorized("Authentication required");
    }
    match stripe_provider::seller_dashboard_link(ctx, msg.user_id()).await {
        Ok(response) => ok_json(&response),
        Err(error) => provider_error("Could not open the Stripe dashboard", error),
    }
}

pub(super) async fn billing_portal(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    if msg.user_id().is_empty() {
        return err_unauthorized("Authentication required");
    }
    let raw = input.collect_to_bytes().await;
    let request: BillingPortalRequest = match serde_json::from_slice(&raw) {
        Ok(request) => request,
        Err(error) => return err_bad_request(&format!("Invalid request body: {error}")),
    };
    match stripe_provider::billing_portal_link(ctx, msg.user_id(), &request).await {
        Ok(response) => ok_json(&response),
        Err(error) => provider_error("Could not create Billing Portal session", error),
    }
}
