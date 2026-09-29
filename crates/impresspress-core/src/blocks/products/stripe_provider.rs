//! Typed Stripe provider operations outside Checkout/webhook orchestration.
//!
//! This module is deliberately the only new code in this slice that knows
//! Stripe's account, Account Link, login-link, and connection-health wire
//! shapes. HTTP handlers consume provider-neutral contracts and never expose
//! credentials or raw Stripe error bodies.

use std::collections::BTreeMap;

use serde_json::Value;
use wafer_core::clients::config;
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{
    config::{
        platform_country, seller_fee_bps, CHECKOUT_ALLOWED_ORIGINS, STRIPE_API_VERSION,
        STRIPE_PUBLISHABLE_KEY, STRIPE_SECRET_KEY, STRIPE_WEBHOOK_SECRET,
    },
    contracts::{
        BillingPortalRequest, ProviderReconcileResult, ProviderRedirect, RefundStatus,
        SellerAccount, SellerApproval, SellerCapabilities, SellerOnboardingRequest,
        SellerOnboardingResponse, SellerStatus, StripeConnectionState, StripeConnectionStatus,
    },
    repo,
    stripe_client::{publishable_livemode, secret_livemode, StripeClient, DEFAULT_API_VERSION},
    stripe_secret_operations_allowed,
};
use crate::{config_vars::FRONTEND_URL_KEY, util::RecordExt};

fn bool_at(value: &Value, pointer: &str) -> bool {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn string_at(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn account_snapshot(
    value: &Value,
    livemode: bool,
) -> Result<repo::seller_accounts::StripeSellerSnapshot, WaferError> {
    let stripe_account_id = string_at(value, "/id");
    if !stripe_account_id.starts_with("acct_") {
        return Err(WaferError::new(
            ErrorCode::Internal,
            "Stripe account response is missing its account id",
        ));
    }
    let requirements = value
        .get("requirements")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(repo::seller_accounts::StripeSellerSnapshot {
        stripe_account_id,
        livemode,
        details_submitted: bool_at(value, "/details_submitted"),
        charges_enabled: bool_at(value, "/charges_enabled"),
        payouts_enabled: bool_at(value, "/payouts_enabled"),
        disabled_reason: string_at(value, "/requirements/disabled_reason"),
        requirements,
        country: string_at(value, "/country").to_ascii_uppercase(),
        default_currency: string_at(value, "/default_currency").to_ascii_uppercase(),
        dashboard_type: string_at(value, "/controller/stripe_dashboard/type"),
    })
}

fn capabilities(value: &Value) -> BTreeMap<String, String> {
    value
        .get("capabilities")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|object| object.iter())
        .map(|(key, value)| {
            let status = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            (key.clone(), status)
        })
        .collect()
}

async fn connection_base(ctx: &dyn Context) -> Result<(String, bool, bool, String), WaferError> {
    let publishable = config::get_default(ctx, STRIPE_PUBLISHABLE_KEY, "").await?;
    let webhook = config::get_default(ctx, STRIPE_WEBHOOK_SECRET, "").await?;
    let api_version = config::get_default(ctx, STRIPE_API_VERSION, DEFAULT_API_VERSION).await?;
    Ok((
        publishable.clone(),
        !publishable.trim().is_empty(),
        !webhook.trim().is_empty(),
        api_version,
    ))
}

fn connection_error(
    configured: bool,
    livemode: bool,
    publishable_key_configured: bool,
    webhook_secret_configured: bool,
    api_version: String,
    error: impl Into<String>,
) -> StripeConnectionStatus {
    StripeConnectionStatus {
        state: if configured {
            StripeConnectionState::Misconfigured
        } else {
            StripeConnectionState::NotConfigured
        },
        configured,
        livemode,
        account_id: String::new(),
        country: String::new(),
        default_currency: String::new(),
        business_name: String::new(),
        charges_enabled: false,
        payouts_enabled: false,
        details_submitted: false,
        capabilities: BTreeMap::new(),
        publishable_key_configured,
        webhook_secret_configured,
        api_version,
        error: error.into(),
    }
}

/// The Stripe connection as the admin sees it. A failed settings read is
/// returned rather than drawn as "not configured".
pub(crate) async fn connection_status(
    ctx: &dyn Context,
) -> Result<StripeConnectionStatus, WaferError> {
    let secret = config::get_default(ctx, STRIPE_SECRET_KEY, "").await?;
    let (publishable, publishable_configured, webhook_configured, api_version) =
        connection_base(ctx).await?;
    if !stripe_secret_operations_allowed(ctx) {
        return Ok(connection_error(
            !secret.trim().is_empty(),
            false,
            publishable_configured,
            false,
            api_version,
            "Stripe secret-key operations are disabled in the browser runtime; use a trusted remote commerce API or pre-created Payment Links",
        ));
    }
    if secret.trim().is_empty() {
        return Ok(connection_error(
            false,
            false,
            publishable_configured,
            webhook_configured,
            api_version,
            "Stripe secret key is not configured",
        ));
    }
    let Some(livemode) = secret_livemode(&secret) else {
        return Ok(connection_error(
            true,
            false,
            publishable_configured,
            webhook_configured,
            api_version,
            "Stripe secret key format is invalid",
        ));
    };
    if publishable_configured && publishable_livemode(&publishable) != Some(livemode) {
        return Ok(connection_error(
            true,
            livemode,
            publishable_configured,
            webhook_configured,
            api_version,
            "Stripe secret and publishable keys are from different modes",
        ));
    }
    let client = match StripeClient::load(ctx).await {
        Ok(client) => client,
        Err(error) => {
            return Ok(connection_error(
                true,
                livemode,
                publishable_configured,
                webhook_configured,
                api_version,
                error.message,
            ))
        }
    };
    let account = match client
        .request_json(ctx, "GET", "/v1/account", None, None, None)
        .await
    {
        Ok(account) => account,
        Err(error) => {
            return Ok(connection_error(
                true,
                livemode,
                publishable_configured,
                webhook_configured,
                api_version,
                error.message,
            ))
        }
    };
    let account_id = string_at(&account, "/id");
    if !account_id.starts_with("acct_") {
        return Ok(connection_error(
            true,
            livemode,
            publishable_configured,
            webhook_configured,
            api_version,
            "Stripe credential check returned an incomplete account",
        ));
    }
    Ok(StripeConnectionStatus {
        state: if livemode {
            StripeConnectionState::ConnectedLive
        } else {
            StripeConnectionState::ConnectedTest
        },
        configured: true,
        livemode,
        account_id,
        country: string_at(&account, "/country").to_ascii_uppercase(),
        default_currency: string_at(&account, "/default_currency").to_ascii_uppercase(),
        business_name: string_at(&account, "/business_profile/name"),
        charges_enabled: bool_at(&account, "/charges_enabled"),
        payouts_enabled: bool_at(&account, "/payouts_enabled"),
        details_submitted: bool_at(&account, "/details_submitted"),
        capabilities: capabilities(&account),
        publishable_key_configured: publishable_configured,
        webhook_secret_configured: webhook_configured,
        api_version,
        error: String::new(),
    })
}

async fn validate_redirect(ctx: &dyn Context, url: &str) -> Result<(), WaferError> {
    let frontend = config::get_default(ctx, FRONTEND_URL_KEY, "http://localhost:5173").await?;
    let allowed = config::get_default(ctx, CHECKOUT_ALLOWED_ORIGINS, "").await?;
    if url.trim().is_empty() || !super::stripe::is_allowed_checkout_url(url, &frontend, &allowed) {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "redirect URL must use a configured application origin",
        ));
    }
    Ok(())
}

fn not_started_account(user_id: &str, fee_basis_points: u16) -> SellerAccount {
    SellerAccount {
        id: String::new(),
        user_id: user_id.to_string(),
        status: SellerStatus::NotStarted,
        approval_status: SellerApproval::Approved,
        stripe_account_id: String::new(),
        capabilities: SellerCapabilities {
            details_submitted: false,
            charges_enabled: false,
            payouts_enabled: false,
            requirements_due: Vec::new(),
        },
        fee_basis_points: fee_basis_points.into(),
        livemode: false,
        country: String::new(),
        default_currency: String::new(),
        dashboard_type: String::new(),
        disabled_reason: String::new(),
        sync_error: String::new(),
        last_synced_at: String::new(),
    }
}

pub(crate) async fn seller_status(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<SellerAccount, WaferError> {
    let fee = seller_fee_bps(ctx).await?;
    let Some(local) = repo::seller_accounts::get_for_user(ctx, user_id).await? else {
        return Ok(not_started_account(user_id, fee));
    };
    let account_id = local.str_field("stripe_account_id").to_string();
    if account_id.is_empty() || repo::seller_accounts::is_suspended_record(&local)? {
        return repo::seller_accounts::to_contract(&local, fee);
    }
    let Ok(client) = StripeClient::load(ctx).await else {
        let stale = repo::seller_accounts::mark_sync_error(
            ctx,
            &local.id,
            "Stripe account status could not be refreshed",
        )
        .await?;
        return repo::seller_accounts::to_contract(&stale, fee);
    };
    let path = format!("/v1/accounts/{}", crate::util::url_path_encode(&account_id));
    let Ok(remote) = client
        .request_json(ctx, "GET", &path, None, None, None)
        .await
    else {
        let stale = repo::seller_accounts::mark_sync_error(
            ctx,
            &local.id,
            "Stripe account status could not be refreshed",
        )
        .await?;
        return repo::seller_accounts::to_contract(&stale, fee);
    };
    let snapshot = account_snapshot(&remote, client.livemode)?;
    let record = repo::seller_accounts::sync_account(ctx, &local.id, &snapshot).await?;
    repo::seller_accounts::to_contract(&record, fee)
}

pub(crate) async fn start_seller_onboarding(
    ctx: &dyn Context,
    user_id: &str,
    request: &SellerOnboardingRequest,
) -> Result<SellerOnboardingResponse, WaferError> {
    validate_redirect(ctx, &request.return_url).await?;
    validate_redirect(ctx, &request.refresh_url).await?;
    let fee = seller_fee_bps(ctx).await?;
    let client = StripeClient::load(ctx).await?;
    let mut local = repo::seller_accounts::ensure_for_user(ctx, user_id).await?;
    if repo::seller_accounts::is_suspended_record(&local)? {
        return Err(WaferError::new(
            ErrorCode::PermissionDenied,
            "seller account is suspended",
        ));
    }
    let account_id = local.str_field("stripe_account_id").to_string();
    if account_id.is_empty() {
        let country = platform_country(ctx).await?;
        let mut form = vec![
            ("type".to_string(), "express".to_string()),
            (
                "capabilities[card_payments][requested]".to_string(),
                "true".to_string(),
            ),
            (
                "capabilities[transfers][requested]".to_string(),
                "true".to_string(),
            ),
            (
                "metadata[impresspress_user_id]".to_string(),
                user_id.to_string(),
            ),
        ];
        // Omitted when the platform has no configured country: Stripe then
        // infers the connected account's country from onboarding, which is
        // what this path has always done for a blank value.
        if let Some(country) = country {
            form.push(("country".to_string(), country.as_str().to_string()));
        }
        let remote = client
            .request_json(
                ctx,
                "POST",
                "/v1/accounts",
                None,
                Some(&format!("impresspress_connect_account_{}", local.id)),
                Some(form),
            )
            .await?;
        let snapshot = account_snapshot(&remote, client.livemode)?;
        local = repo::seller_accounts::sync_account(ctx, &local.id, &snapshot).await?;
    } else {
        let remote = client
            .request_json(
                ctx,
                "GET",
                &format!("/v1/accounts/{}", crate::util::url_path_encode(&account_id)),
                None,
                None,
                None,
            )
            .await?;
        let snapshot = account_snapshot(&remote, client.livemode)?;
        local = repo::seller_accounts::sync_account(ctx, &local.id, &snapshot).await?;
    }
    let stripe_account_id = local.str_field("stripe_account_id").to_string();
    let link = client
        .request_json(
            ctx,
            "POST",
            "/v1/account_links",
            None,
            Some(&format!(
                "impresspress_account_link_{}",
                uuid::Uuid::now_v7()
            )),
            Some(vec![
                ("account".to_string(), stripe_account_id),
                ("refresh_url".to_string(), request.refresh_url.clone()),
                ("return_url".to_string(), request.return_url.clone()),
                ("type".to_string(), "account_onboarding".to_string()),
                (
                    "collection_options[fields]".to_string(),
                    "eventually_due".to_string(),
                ),
            ]),
        )
        .await?;
    let url = string_at(&link, "/url");
    let expires_at = link.get("expires_at").and_then(Value::as_i64).unwrap_or(0);
    if !url.starts_with("https://") || expires_at <= 0 {
        return Err(WaferError::new(
            ErrorCode::Internal,
            "Stripe returned an incomplete onboarding link",
        ));
    }
    Ok(SellerOnboardingResponse {
        account: repo::seller_accounts::to_contract(&local, fee)?,
        url,
        expires_at,
    })
}

pub(crate) async fn seller_dashboard_link(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<ProviderRedirect, WaferError> {
    let account = seller_status(ctx, user_id).await?;
    if account.stripe_account_id.is_empty() {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "seller must start Stripe onboarding first",
        ));
    }
    if account.dashboard_type != "express" {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "seller account does not use the Stripe Express dashboard",
        ));
    }
    let client = StripeClient::load(ctx).await?;
    let link = client
        .request_json(
            ctx,
            "POST",
            &format!(
                "/v1/accounts/{}/login_links",
                crate::util::url_path_encode(&account.stripe_account_id)
            ),
            None,
            Some(&format!("impresspress_login_link_{}", uuid::Uuid::now_v7())),
            Some(Vec::new()),
        )
        .await?;
    let url = string_at(&link, "/url");
    if !url.starts_with("https://") {
        return Err(WaferError::new(
            ErrorCode::Internal,
            "Stripe returned an incomplete dashboard link",
        ));
    }
    Ok(ProviderRedirect { url })
}

pub(crate) async fn billing_portal_link(
    ctx: &dyn Context,
    user_id: &str,
    request: &BillingPortalRequest,
) -> Result<ProviderRedirect, WaferError> {
    validate_redirect(ctx, &request.return_url).await?;
    let client = StripeClient::load(ctx).await?;
    let order_context =
        repo::purchases::customer_for_buyer(ctx, user_id, request.order_id.as_deref()).await?;
    let context = order_context.ok_or_else(|| {
        WaferError::new(
            ErrorCode::FailedPrecondition,
            "no commerce order with a Stripe customer exists for this buyer",
        )
    })?;
    let customer = context.stripe_customer_id;
    let stripe_account = context.stripe_account_id;
    if context.livemode != client.livemode {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "buyer customer belongs to a different Stripe mode",
        ));
    }
    let session = client
        .request_json(
            ctx,
            "POST",
            "/v1/billing_portal/sessions",
            Some(&stripe_account),
            Some(&format!(
                "impresspress_billing_portal_{}",
                uuid::Uuid::now_v7()
            )),
            Some(vec![
                ("customer".to_string(), customer),
                ("return_url".to_string(), request.return_url.clone()),
            ]),
        )
        .await?;
    let url = string_at(&session, "/url");
    if !url.starts_with("https://billing.stripe.com/") {
        return Err(WaferError::new(
            ErrorCode::Internal,
            "Stripe returned an incomplete Billing Portal session",
        ));
    }
    Ok(ProviderRedirect { url })
}

pub(crate) struct StripeRefundParams {
    pub purchase_id: String,
    pub payment_intent_id: String,
    pub stripe_account_id: String,
    pub idempotency_key: String,
    pub amount_minor: i64,
    pub provider_reason: String,
    pub refund_application_fee: bool,
    pub expected_livemode: bool,
}

pub(crate) struct StripeRefundResponse {
    pub id: String,
    pub status: String,
    pub amount_minor: i64,
    pub livemode: bool,
}

fn decode_refund_response(
    value: &Value,
    params: &StripeRefundParams,
    expected_id: Option<&str>,
    client_livemode: bool,
) -> Result<StripeRefundResponse, WaferError> {
    let id = string_at(value, "/id");
    let status = string_at(value, "/status");
    let amount_minor = value
        .pointer("/amount")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let response_intent = string_at(value, "/payment_intent");
    let livemode = value
        .pointer("/livemode")
        .and_then(Value::as_bool)
        .unwrap_or(client_livemode);
    if !id.starts_with("re_")
        || expected_id.is_some_and(|expected| expected != id)
        || !matches!(
            status.as_str(),
            "pending" | "requires_action" | "succeeded" | "failed" | "canceled"
        )
        || amount_minor != params.amount_minor
        || (!response_intent.is_empty() && response_intent != params.payment_intent_id)
        || livemode != client_livemode
        || livemode != params.expected_livemode
    {
        return Err(WaferError::new(
            ErrorCode::Internal,
            "Stripe returned an inconsistent refund response",
        ));
    }
    Ok(StripeRefundResponse {
        id,
        status,
        amount_minor,
        livemode,
    })
}

/// Create an exact PaymentIntent refund in the same platform/connected-account
/// and test/live context as the original order. The caller owns its durable
/// claim and only commits refunded business state after this returns success.
pub(crate) async fn create_refund(
    ctx: &dyn Context,
    params: &StripeRefundParams,
) -> Result<StripeRefundResponse, WaferError> {
    let client = StripeClient::load(ctx).await?;
    if params.expected_livemode != client.livemode {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "purchase belongs to a different Stripe mode",
        ));
    }
    if params.amount_minor <= 0 || params.payment_intent_id.is_empty() {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "refund requires a positive amount and Stripe PaymentIntent",
        ));
    }
    let mut form = vec![
        (
            "payment_intent".to_string(),
            params.payment_intent_id.clone(),
        ),
        ("amount".to_string(), params.amount_minor.to_string()),
        (
            "metadata[impresspress_purchase_id]".to_string(),
            params.purchase_id.clone(),
        ),
        (
            "metadata[impresspress_idempotency_key]".to_string(),
            params.idempotency_key.clone(),
        ),
    ];
    if !params.provider_reason.is_empty() {
        form.push(("reason".to_string(), params.provider_reason.clone()));
    }
    if params.refund_application_fee {
        form.push(("refund_application_fee".to_string(), "true".to_string()));
    }
    let value = client
        .request_json(
            ctx,
            "POST",
            "/v1/refunds",
            Some(&params.stripe_account_id),
            Some(&params.idempotency_key),
            Some(form),
        )
        .await?;
    decode_refund_response(&value, params, None, client.livemode)
}

pub(crate) async fn retrieve_refund(
    ctx: &dyn Context,
    params: &StripeRefundParams,
    refund_id: &str,
) -> Result<StripeRefundResponse, WaferError> {
    if !refund_id.starts_with("re_") {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "refund reconciliation requires a Stripe refund id",
        ));
    }
    let client = StripeClient::load(ctx).await?;
    if params.expected_livemode != client.livemode {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "refund belongs to a different Stripe mode",
        ));
    }
    let value = client
        .request_json(
            ctx,
            "GET",
            &format!("/v1/refunds/{}", crate::util::url_path_encode(refund_id)),
            Some(&params.stripe_account_id),
            None,
            None,
        )
        .await?;
    decode_refund_response(&value, params, Some(refund_id), client.livemode)
}

/// What one run of a claimed provider operation achieved, whatever kind it
/// is: completed, worth another attempt, or finished in a state only an
/// operator can move.
enum OperationOutcome {
    Succeeded(String),
    Retry(String),
    Terminal(String, String),
}

async fn reconcile_refund_operation(
    ctx: &dyn Context,
    operation: &wafer_core::clients::database::Record,
) -> Result<OperationOutcome, WaferError> {
    let refund = repo::refunds::get(ctx, operation.str_field("aggregate_id")).await?;
    if repo::refunds::status_of(&refund)? == RefundStatus::Succeeded {
        return Ok(OperationOutcome::Succeeded(
            refund.json_text_field("response_json"),
        ));
    }
    let purchase = repo::purchases::get(ctx, refund.str_field("purchase_id")).await?;
    if purchase.str_field("stripe_account_id") != refund.str_field("stripe_account_id")
        || purchase.bool_field("livemode") != refund.bool_field("livemode")
        || !purchase
            .str_field("currency")
            .eq_ignore_ascii_case(refund.str_field("currency"))
    {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "refund reconciliation snapshot no longer matches its purchase",
        ));
    }
    let params = StripeRefundParams {
        purchase_id: refund.str_field("purchase_id").to_string(),
        payment_intent_id: refund.str_field("payment_intent_id").to_string(),
        stripe_account_id: refund.str_field("stripe_account_id").to_string(),
        idempotency_key: refund.str_field("idempotency_key").to_string(),
        amount_minor: refund.i64_field("amount_minor"),
        provider_reason: refund.str_field("provider_reason").to_string(),
        refund_application_fee: !refund.str_field("stripe_account_id").is_empty()
            && purchase.i64_field("platform_fee_cents") > 0,
        expected_livemode: refund.bool_field("livemode"),
    };
    let provider = if refund.str_field("provider_refund_id").is_empty() {
        create_refund(ctx, &params).await?
    } else {
        retrieve_refund(ctx, &params, refund.str_field("provider_refund_id")).await?
    };
    let response_json = serde_json::json!({
        "id": provider.id,
        "status": provider.status,
        "amount_minor": provider.amount_minor,
        "livemode": provider.livemode,
        "source": "provider_reconciliation"
    })
    .to_string();
    let mut ledger = if refund.str_field("provider_refund_id").is_empty() {
        repo::refunds::record_provider_response(
            ctx,
            &refund.id,
            &provider.id,
            &provider.status,
            provider.livemode,
            &response_json,
        )
        .await?
    } else {
        repo::refunds::record_webhook_response(
            ctx,
            &refund.id,
            &provider.id,
            &provider.status,
            provider.livemode,
            &response_json,
            chrono::Utc::now().timestamp(),
        )
        .await?
        .record
    };
    match provider.status.as_str() {
        "succeeded" => {
            repo::purchases::reconcile_refund_total(
                ctx,
                ledger.str_field("purchase_id"),
                ledger.i64_field("target_refunded_total_minor"),
                ledger.str_field("refunded_by"),
                ledger.str_field("note"),
            )
            .await?;
            ledger = repo::refunds::mark_succeeded(ctx, &ledger.id).await?;
            Ok(OperationOutcome::Succeeded(
                ledger.json_text_field("response_json"),
            ))
        }
        "failed" | "canceled" => {
            repo::refunds::mark_failed(
                ctx,
                &ledger.id,
                "Stripe reports that the refund is terminal and was not completed",
            )
            .await?;
            Ok(OperationOutcome::Terminal(
                "Stripe refund failed or was canceled".to_string(),
                response_json,
            ))
        }
        _ => Ok(OperationOutcome::Retry(format!(
            "Stripe refund remains {}",
            provider.status
        ))),
    }
}

/// Claim and reconcile a bounded batch. It is safe to invoke from an
/// authenticated scheduler or the administrator recovery panel; leases prevent
/// overlapping workers and Stripe mutations retain their original idempotency
/// key. A write that fails for one operation is logged and counted in
/// `unrecorded`; the rest of the batch still runs.
pub(crate) async fn reconcile_provider_operations(
    ctx: &dyn Context,
    limit: usize,
) -> Result<ProviderReconcileResult, WaferError> {
    let batch = repo::provider_operations::claim_due(ctx, limit.clamp(1, 100)).await?;
    let mut result = ProviderReconcileResult {
        claimed: batch.claims.len() as u64,
        dead_letter: batch.dead_lettered,
        unrecorded: batch.failures.len() as u64,
        ..ProviderReconcileResult::default()
    };
    for (id, error) in &batch.failures {
        tracing::error!(
            operation_id = %id,
            error = %error,
            "could not claim or dead-letter a due provider operation"
        );
    }
    for claim in batch.claims {
        match record_operation_outcome(ctx, &claim).await {
            Ok(Recorded::Succeeded) => result.succeeded += 1,
            Ok(Recorded::RetryScheduled) => result.retry_scheduled += 1,
            Ok(Recorded::DeadLettered) => result.dead_letter += 1,
            Err(error) => {
                tracing::error!(
                    operation_id = %claim.record.id,
                    attempts = claim.attempts,
                    error = %error,
                    "could not record a provider operation's outcome"
                );
                result.unrecorded += 1;
            }
        }
    }
    Ok(result)
}

/// Take down the Payment Link of the row this operation names. The row id is
/// the aggregate, so a takedown queued by a deactivation, by an archived
/// offer or by a create whose row was retired mid-flight is the same
/// operation under the same Stripe idempotency key.
async fn take_down_payment_link_operation(
    ctx: &dyn Context,
    operation: &wafer_core::clients::database::Record,
) -> Result<OperationOutcome, WaferError> {
    match super::stripe::take_down_payment_link(ctx, operation.str_field("aggregate_id")).await? {
        super::stripe::PaymentLinkTakedown::Settled => {
            Ok(OperationOutcome::Succeeded("{}".to_string()))
        }
        super::stripe::PaymentLinkTakedown::Unresolvable(reason) => {
            Ok(OperationOutcome::Terminal(reason, "{}".to_string()))
        }
    }
}

/// Where [`record_operation_outcome`] left one claimed operation.
enum Recorded {
    Succeeded,
    RetryScheduled,
    DeadLettered,
}

/// Run one claimed operation and write its outcome under the claim's lease.
/// A failure of the operation itself is an outcome (retry or dead letter);
/// only a failed bookkeeping write is an `Err`.
async fn record_operation_outcome(
    ctx: &dyn Context,
    claim: &repo::provider_operations::OperationClaim,
) -> Result<Recorded, WaferError> {
    let outcome = match claim.record.str_field("operation_type") {
        repo::provider_operations::REFUND_RECONCILE => {
            reconcile_refund_operation(ctx, &claim.record).await
        }
        repo::provider_operations::PAYMENT_LINK_DEACTIVATE => {
            take_down_payment_link_operation(ctx, &claim.record).await
        }
        other => Ok(OperationOutcome::Terminal(
            format!("unsupported provider operation type: {other}"),
            "{}".to_string(),
        )),
    };
    match outcome {
        Ok(OperationOutcome::Succeeded(response_json)) => {
            repo::provider_operations::mark_completed(
                ctx,
                &claim.record.id,
                &claim.owner,
                &response_json,
            )
            .await?;
            Ok(Recorded::Succeeded)
        }
        Ok(OperationOutcome::Terminal(message, response_json)) => {
            repo::provider_operations::resolve_unleased(
                ctx,
                &claim.record.id,
                false,
                &response_json,
                &message,
            )
            .await?;
            Ok(Recorded::DeadLettered)
        }
        Ok(OperationOutcome::Retry(message)) | Err(WaferError { message, .. }) => {
            match repo::provider_operations::mark_retry(
                ctx,
                &claim.record.id,
                &claim.owner,
                claim.attempts,
                &message,
            )
            .await?
            {
                repo::provider_operations::RetryRecorded::Scheduled => Ok(Recorded::RetryScheduled),
                repo::provider_operations::RetryRecorded::DeadLettered => {
                    Ok(Recorded::DeadLettered)
                }
            }
        }
    }
}

pub(crate) async fn sync_connected_account(
    ctx: &dyn Context,
    account: &Value,
    livemode: bool,
    event_created: i64,
) -> Result<bool, WaferError> {
    let account_id = string_at(account, "/id");
    if account_id.is_empty() {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "account.updated event is missing its account id",
        ));
    }
    let Some(local) = repo::seller_accounts::get_by_stripe_account(ctx, &account_id).await? else {
        return Ok(false);
    };
    let snapshot = account_snapshot(account, livemode)?;
    repo::seller_accounts::sync_account_event(ctx, &local.id, &snapshot, event_created).await?;
    Ok(true)
}
