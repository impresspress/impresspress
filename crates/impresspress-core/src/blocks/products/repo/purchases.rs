//! Data access for the purchases header table and its line items.

use std::collections::{BTreeMap, HashMap};

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, WaferError};

use crate::{
    blocks::products::{
        contracts::{
            AnalyticsProduct, CheckoutPresentation, CommerceAnalytics, MoneyBreakdown, OfferMode,
            SellerFailureSummary,
        },
        money,
    },
    util::RecordExt,
};

/// Purchase header table — one row per checkout / order.
pub(crate) const PURCHASES_TABLE: &str = "impresspress__products__purchases";

/// Purchase line-item table — one row per product line in a purchase.
pub(crate) const LINE_ITEMS_TABLE: &str = "impresspress__products__line_items";

pub(crate) async fn recent_seller_failures(
    ctx: &dyn Context,
    seller_account_id: &str,
    limit: i64,
) -> Result<Vec<SellerFailureSummary>, WaferError> {
    let limit = limit.clamp(1, 50);
    let terminal = db::list(
        ctx,
        PURCHASES_TABLE,
        &ListOptions {
            filters: vec![
                Filter {
                    field: "seller_account_id".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(seller_account_id),
                },
                Filter {
                    field: "status".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!("failed"),
                },
            ],
            sort: vec![SortField {
                field: "created_at".to_string(),
                desc: true,
            }],
            limit,
            ..Default::default()
        },
    )
    .await?;
    let payment_attention = db::list(
        ctx,
        PURCHASES_TABLE,
        &ListOptions {
            filters: vec![
                Filter {
                    field: "seller_account_id".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(seller_account_id),
                },
                Filter {
                    field: "provider_payment_status".to_string(),
                    operator: FilterOp::In,
                    value: serde_json::json!(["payment_failed", "requires_action", "canceled"]),
                },
            ],
            sort: vec![SortField {
                field: "created_at".to_string(),
                desc: true,
            }],
            limit,
            ..Default::default()
        },
    )
    .await?;
    let mut records = terminal.records;
    for record in payment_attention.records {
        if !records.iter().any(|existing| existing.id == record.id) {
            records.push(record);
        }
    }
    records.sort_by(|left, right| {
        right
            .str_field("created_at")
            .cmp(left.str_field("created_at"))
            .then_with(|| left.id.cmp(&right.id))
    });
    records.truncate(limit as usize);
    Ok(records
        .into_iter()
        .map(|record| {
            let reconciliation_error = record.str_field("reconciliation_error");
            SellerFailureSummary {
                order_id: record.id.clone(),
                status: record.str_field("status").to_string(),
                currency: record.str_field("currency").to_string(),
                total_minor: record.i64_field("total_cents"),
                error: if reconciliation_error.is_empty() {
                    record.str_field("provider_payment_error_message")
                } else {
                    reconciliation_error
                }
                .to_string(),
                created_at: record.str_field("created_at").to_string(),
            }
        })
        .collect())
}

/// Immutable server-resolved line item written before a provider checkout is
/// created. All monetary values are stored as exact integer minor units.
pub(crate) struct CheckoutLineSnapshot {
    pub product_id: String,
    pub product_name: String,
    pub offer_id: String,
    pub offer_version: u32,
    pub component_id: String,
    pub quantity: u64,
    pub unit_amount_minor: i64,
    pub total_amount_minor: i64,
    pub input_snapshot: String,
    pub condition_snapshot: String,
}

/// Complete order snapshot for a commerce-v2 Checkout Session.
pub(crate) struct CheckoutOrderSnapshot {
    pub buyer_user_id: String,
    pub buyer_email: String,
    pub seller_account_id: String,
    pub stripe_account_id: String,
    pub presentation: CheckoutPresentation,
    pub mode: OfferMode,
    pub offer_id: String,
    pub offer_version: u32,
    pub livemode: bool,
    pub receipt_token_hash: String,
    pub receipt_token_expires_at: Option<String>,
    pub allowed_shipping_amounts_minor: Vec<i64>,
    pub amounts: MoneyBreakdown,
    pub items: Vec<CheckoutLineSnapshot>,
}

/// Provider fields required to reconcile a Checkout Session without trusting
/// metadata or browser-return state as proof of payment.
pub(crate) struct CheckoutSessionCompletion {
    pub session_id: String,
    pub client_reference_id: String,
    pub event_account: String,
    pub livemode: bool,
    pub mode: String,
    pub payment_status: String,
    pub currency: String,
    pub subtotal_minor: i64,
    pub discount_minor: i64,
    pub tax_minor: i64,
    pub shipping_minor: i64,
    pub total_minor: i64,
    pub offer_id: String,
    pub offer_version: u32,
    pub payment_intent_id: String,
    pub customer_id: String,
    pub subscription_id: String,
}

/// Supplemental PaymentIntent state for payment-mode Checkout orders. This
/// projection improves diagnostics and recovery but never fulfills an order;
/// only the exact Checkout Session reconciliation may do that.
pub(crate) struct PaymentIntentSnapshot {
    pub purchase_id: String,
    pub offer_id: String,
    pub offer_version: u32,
    pub payment_intent_id: String,
    pub stripe_account_id: String,
    pub livemode: bool,
    pub status: String,
    pub amount_minor: i64,
    pub currency: String,
    pub error_code: String,
    pub error_message: String,
    pub event_created: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CustomerContext {
    pub order_id: String,
    pub stripe_customer_id: String,
    pub stripe_account_id: String,
    pub livemode: bool,
}

fn wire_string<T: serde::Serialize>(value: &T) -> Result<String, WaferError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| {
            WaferError::new(
                wafer_run::ErrorCode::Internal,
                "commerce snapshot enum did not serialize as a string",
            )
        })
}

pub(crate) fn checkout_metadata(record: &Record) -> Option<serde_json::Value> {
    let value = match record.data.get("metadata")?.clone() {
        serde_json::Value::String(raw) => serde_json::from_str(&raw).ok()?,
        value => value,
    };
    value.is_object().then_some(value)
}

/// Create the order header and all resolved line snapshots. A partial write
/// is compensated inside this table-owning repository so it can never become
/// a checkout-eligible order with missing rows.
pub(crate) async fn create_checkout_order(
    ctx: &dyn Context,
    snapshot: CheckoutOrderSnapshot,
) -> Result<Record, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    let checkout_mode = wire_string(&snapshot.presentation)?;
    let offer_mode = wire_string(&snapshot.mode)?;
    let metadata = serde_json::to_string(&serde_json::json!({
        "schema_version": 1,
        "offer_id": snapshot.offer_id,
        "offer_version": snapshot.offer_version,
        "offer_mode": offer_mode,
        "allowed_shipping_amounts_minor": snapshot.allowed_shipping_amounts_minor,
    }))
    .map_err(|error| {
        WaferError::new(
            wafer_run::ErrorCode::Internal,
            format!("could not encode order snapshot: {error}"),
        )
    })?;
    let mut data = HashMap::from([
        (
            "user_id".to_string(),
            serde_json::json!(&snapshot.buyer_user_id),
        ),
        (
            "buyer_user_id".to_string(),
            serde_json::json!(&snapshot.buyer_user_id),
        ),
        (
            "buyer_email".to_string(),
            serde_json::json!(&snapshot.buyer_email),
        ),
        (
            "seller_account_id".to_string(),
            serde_json::json!(&snapshot.seller_account_id),
        ),
        (
            "stripe_account_id".to_string(),
            serde_json::json!(&snapshot.stripe_account_id),
        ),
        ("status".to_string(), serde_json::json!("pending")),
        (
            "total_cents".to_string(),
            serde_json::json!(snapshot.amounts.total_minor),
        ),
        (
            "amount_cents".to_string(),
            serde_json::json!(snapshot.amounts.total_minor),
        ),
        (
            "subtotal_cents".to_string(),
            serde_json::json!(snapshot.amounts.subtotal_minor),
        ),
        (
            "discount_cents".to_string(),
            serde_json::json!(snapshot.amounts.discount_minor),
        ),
        (
            "tax_cents".to_string(),
            serde_json::json!(snapshot.amounts.tax_minor),
        ),
        (
            "shipping_cents".to_string(),
            serde_json::json!(snapshot.amounts.shipping_minor),
        ),
        (
            "platform_fee_cents".to_string(),
            serde_json::json!(snapshot.amounts.platform_fee_minor),
        ),
        (
            "currency".to_string(),
            serde_json::json!(&snapshot.amounts.currency),
        ),
        ("livemode".to_string(), serde_json::json!(snapshot.livemode)),
        ("provider".to_string(), serde_json::json!("stripe")),
        (
            "checkout_mode".to_string(),
            serde_json::json!(checkout_mode),
        ),
        ("metadata".to_string(), serde_json::json!(metadata)),
        (
            "reconciliation_status".to_string(),
            serde_json::json!("pending"),
        ),
        (
            "receipt_token_hash".to_string(),
            serde_json::json!(&snapshot.receipt_token_hash),
        ),
        (
            "receipt_token_expires_at".to_string(),
            snapshot
                .receipt_token_expires_at
                .as_ref()
                .map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
        ),
        ("created_at".to_string(), serde_json::json!(&now)),
        ("updated_at".to_string(), serde_json::json!(&now)),
    ]);
    // Provider ids are deliberately empty until the external call succeeds.
    data.insert("provider_session_id".to_string(), serde_json::json!(""));
    let purchase = create(ctx, data).await?;
    for item in snapshot.items {
        let item_data = HashMap::from([
            ("purchase_id".to_string(), serde_json::json!(&purchase.id)),
            ("product_id".to_string(), serde_json::json!(item.product_id)),
            (
                "product_name".to_string(),
                serde_json::json!(item.product_name),
            ),
            ("offer_id".to_string(), serde_json::json!(item.offer_id)),
            (
                "offer_version".to_string(),
                serde_json::json!(item.offer_version),
            ),
            (
                "component_id".to_string(),
                serde_json::json!(item.component_id),
            ),
            ("quantity".to_string(), serde_json::json!(item.quantity)),
            (
                "unit_amount_minor".to_string(),
                serde_json::json!(item.unit_amount_minor),
            ),
            (
                "subtotal_minor".to_string(),
                serde_json::json!(item.total_amount_minor),
            ),
            ("discount_minor".to_string(), serde_json::json!(0)),
            ("tax_minor".to_string(), serde_json::json!(0)),
            (
                "total_minor".to_string(),
                serde_json::json!(item.total_amount_minor),
            ),
            (
                "input_snapshot".to_string(),
                serde_json::json!(item.input_snapshot),
            ),
            (
                "condition_snapshot".to_string(),
                serde_json::json!(item.condition_snapshot),
            ),
            ("created_at".to_string(), serde_json::json!(&now)),
            ("updated_at".to_string(), serde_json::json!(&now)),
        ]);
        if let Err(error) = add_line_item(ctx, item_data).await {
            let _ = delete_with_line_items(ctx, &purchase.id).await;
            return Err(error);
        }
    }
    Ok(purchase)
}

/// Delete a newly-created order and all owned line items (compensation path).
pub(crate) async fn delete_with_line_items(
    ctx: &dyn Context,
    purchase_id: &str,
) -> Result<(), WaferError> {
    db::delete_by_filters(
        ctx,
        LINE_ITEMS_TABLE,
        vec![Filter {
            field: "purchase_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(purchase_id),
        }],
    )
    .await?;
    delete(ctx, purchase_id).await
}

/// Record a deterministic provider-creation failure. Failed orders retain
/// their exact pricing snapshot for support and reconciliation, but can never
/// be claimed for another checkout.
pub(crate) async fn mark_checkout_failed(
    ctx: &dyn Context,
    purchase_id: &str,
    message: &str,
) -> Result<Record, WaferError> {
    update(
        ctx,
        purchase_id,
        HashMap::from([
            ("status".to_string(), serde_json::json!("failed")),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("provider_error"),
            ),
            (
                "reconciliation_error".to_string(),
                serde_json::json!(message),
            ),
            (
                "updated_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            ),
        ]),
    )
    .await
}

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

fn customer_context(record: &Record) -> Option<CustomerContext> {
    let stripe_customer_id = record.str_field("stripe_customer_id").to_string();
    if stripe_customer_id.is_empty() {
        return None;
    }
    Some(CustomerContext {
        order_id: record.id.clone(),
        stripe_customer_id,
        stripe_account_id: record.str_field("stripe_account_id").to_string(),
        livemode: record.bool_field("livemode"),
    })
}

/// Resolve the Stripe customer and connected-account context owned by a buyer.
/// If the buyer has customers in multiple account contexts they must choose an
/// order explicitly; silently picking one would open the wrong Billing Portal.
pub(crate) async fn customer_for_buyer(
    ctx: &dyn Context,
    user_id: &str,
    order_id: Option<&str>,
) -> Result<Option<CustomerContext>, WaferError> {
    if let Some(order_id) = order_id.filter(|value| !value.is_empty()) {
        let order = get(ctx, order_id).await?;
        let owner = if order.str_field("buyer_user_id").is_empty() {
            order.str_field("user_id")
        } else {
            order.str_field("buyer_user_id")
        };
        if owner != user_id {
            return Err(WaferError::new(
                wafer_run::ErrorCode::PermissionDenied,
                "order does not belong to the authenticated buyer",
            ));
        }
        return customer_context(&order).map(Some).ok_or_else(|| {
            WaferError::new(
                wafer_run::ErrorCode::FailedPrecondition,
                "order does not have a Stripe customer",
            )
        });
    }

    let mut records = db::list(
        ctx,
        PURCHASES_TABLE,
        &ListOptions {
            filters: vec![Filter {
                field: "buyer_user_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(user_id),
            }],
            sort: vec![SortField {
                field: "created_at".to_string(),
                desc: true,
            }],
            limit: 100,
            skip_count: true,
            ..Default::default()
        },
    )
    .await?
    .records;
    if records.is_empty() {
        records = db::list(
            ctx,
            PURCHASES_TABLE,
            &ListOptions {
                filters: vec![Filter {
                    field: "user_id".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(user_id),
                }],
                sort: vec![SortField {
                    field: "created_at".to_string(),
                    desc: true,
                }],
                limit: 100,
                skip_count: true,
                ..Default::default()
            },
        )
        .await?
        .records;
    }
    let mut contexts = Vec::new();
    for record in records {
        let Some(context) = customer_context(&record) else {
            continue;
        };
        if !contexts.iter().any(|existing: &CustomerContext| {
            existing.stripe_customer_id == context.stripe_customer_id
                && existing.stripe_account_id == context.stripe_account_id
        }) {
            contexts.push(context);
        }
    }
    if contexts.len() > 1 {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "multiple Stripe customer contexts exist; select an order_id",
        ));
    }
    Ok(contexts.pop())
}

/// Atomic checkout-completion: only transitions a purchase still in
/// `checkout_started`/`pending`. Returns rows affected (0 = already
/// completed/refunded). `checkout.session.completed`.
#[cfg(test)]
pub(crate) async fn complete_atomic(
    ctx: &dyn Context,
    purchase_id: &str,
    payment_intent: &str,
) -> Result<i64, WaferError> {
    complete_checkout_atomic(ctx, purchase_id, payment_intent, "", "", false).await
}

/// Complete a Checkout Session and persist the provider identities needed for
/// one-time payment, subscription, and reconciliation flows in one guarded
/// status transition.
pub(crate) async fn complete_checkout_atomic(
    ctx: &dyn Context,
    purchase_id: &str,
    payment_intent: &str,
    stripe_customer_id: &str,
    stripe_subscription_id: &str,
    livemode: bool,
) -> Result<i64, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("status".into(), serde_json::json!("completed"));
    data.insert(
        "provider_payment_intent_id".into(),
        serde_json::json!(payment_intent),
    );
    data.insert(
        "stripe_payment_intent_id".into(),
        serde_json::json!(payment_intent),
    );
    if !payment_intent.is_empty() {
        data.insert(
            "provider_payment_status".into(),
            serde_json::json!("succeeded"),
        );
        data.insert("provider_payment_error_code".into(), serde_json::json!(""));
        data.insert(
            "provider_payment_error_message".into(),
            serde_json::json!(""),
        );
    }
    data.insert(
        "stripe_customer_id".into(),
        serde_json::json!(stripe_customer_id),
    );
    data.insert(
        "stripe_subscription_id".into(),
        serde_json::json!(stripe_subscription_id),
    );
    if !stripe_subscription_id.is_empty() {
        data.insert("subscription_status".into(), serde_json::json!("active"));
        data.insert(
            "subscription_last_synced_at".into(),
            serde_json::json!(&now),
        );
    }
    data.insert("livemode".into(), serde_json::json!(livemode));
    data.insert(
        "reconciliation_status".into(),
        serde_json::json!("reconciled"),
    );
    data.insert("approved_at".into(), serde_json::json!(&now));
    data.insert("payment_at".into(), serde_json::json!(&now));
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

/// Reconcile a typed hosted/embedded Checkout Session against the exact local
/// order snapshot before making the terminal state transition. Promotions and
/// Stripe tax may change the final total, so the immutable subtotal is checked
/// first and Stripe's internally consistent discount/tax/total replaces only
/// the corresponding final amount fields.
pub(crate) async fn reconcile_checkout_session(
    ctx: &dyn Context,
    purchase_id: &str,
    completion: &CheckoutSessionCompletion,
) -> Result<i64, WaferError> {
    let purchase = get(ctx, purchase_id).await?;
    let invalid = |message: &str| {
        WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            format!("Checkout Session reconciliation failed: {message}"),
        )
    };
    if completion.session_id.is_empty() {
        return Err(invalid("session id does not match the order"));
    }
    // An EMPTY stored session id means the local write after creating the
    // provider session failed (the Stripe session exists and its metadata and
    // client_reference_id point at this order, but the id never landed
    // locally). The completed event arrives through signature verification
    // and every other identity/mode/currency/amount/offer cross-check below
    // still applies, so the order may ADOPT the session id — written only via
    // the guarded CAS at the end (from empty, alongside the status guard). A
    // DIFFERENT non-empty stored id remains a hard conflict.
    let stored_session_id = purchase.str_field("provider_session_id").to_string();
    let adopt_session = stored_session_id.is_empty();
    if !adopt_session && stored_session_id != completion.session_id {
        return Err(invalid("session id does not match the order"));
    }
    if completion.client_reference_id != purchase_id {
        return Err(invalid("client reference does not match the order"));
    }
    if purchase.str_field("stripe_account_id") != completion.event_account {
        return Err(invalid("Stripe account does not match the order"));
    }
    if purchase.bool_field("livemode") != completion.livemode {
        return Err(invalid("test/live mode does not match the order"));
    }
    if purchase.str_field("currency") != completion.currency.to_ascii_uppercase() {
        return Err(invalid("currency does not match the order"));
    }
    if purchase.i64_field("subtotal_cents") != completion.subtotal_minor {
        return Err(invalid("subtotal does not match the immutable quote"));
    }
    if completion.discount_minor < 0 || completion.tax_minor < 0 || completion.shipping_minor < 0 {
        return Err(invalid(
            "discount, tax, and shipping amounts must not be negative",
        ));
    }
    let metadata =
        checkout_metadata(&purchase).ok_or_else(|| invalid("order metadata is invalid"))?;
    let allowed_shipping = metadata
        .get("allowed_shipping_amounts_minor")
        .and_then(serde_json::Value::as_array)
        .map(|amounts| {
            amounts
                .iter()
                .filter_map(serde_json::Value::as_i64)
                .any(|amount| amount == completion.shipping_minor)
        })
        .unwrap_or(completion.shipping_minor == 0);
    if !allowed_shipping {
        return Err(invalid(
            "shipping amount is not allowed by the order snapshot",
        ));
    }
    let expected_total = completion
        .subtotal_minor
        .checked_sub(completion.discount_minor)
        .and_then(|value| value.checked_add(completion.tax_minor))
        .and_then(|value| value.checked_add(completion.shipping_minor))
        .ok_or_else(|| invalid("amount breakdown overflowed"))?;
    if completion.total_minor != expected_total {
        return Err(invalid("amount breakdown is inconsistent"));
    }
    if !matches!(
        completion.payment_status.as_str(),
        "paid" | "no_payment_required"
    ) {
        return Err(invalid("payment is not complete"));
    }

    let expected_mode = metadata
        .get("offer_mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let expected_offer = metadata
        .get("offer_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let expected_version = metadata
        .get("offer_version")
        .and_then(crate::util::json_as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default();
    if expected_mode != completion.mode {
        return Err(invalid("Checkout mode does not match the offer"));
    }
    if expected_offer != completion.offer_id || expected_version != completion.offer_version {
        return Err(invalid("offer identity/version does not match the order"));
    }
    match expected_mode {
        "payment"
            if completion.payment_intent_id.is_empty()
                && completion.payment_status != "no_payment_required" =>
        {
            return Err(invalid(
                "paid one-time Checkout is missing its PaymentIntent",
            ));
        }
        "subscription" if completion.subscription_id.is_empty() => {
            return Err(invalid("subscription Checkout is missing its Subscription"));
        }
        "payment" | "subscription" => {}
        _ => return Err(invalid("order has an unsupported Checkout mode")),
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut data = HashMap::from([
        ("status".to_string(), serde_json::json!("completed")),
        (
            "provider_payment_intent_id".to_string(),
            serde_json::json!(&completion.payment_intent_id),
        ),
        (
            "stripe_payment_intent_id".to_string(),
            serde_json::json!(&completion.payment_intent_id),
        ),
        (
            "stripe_customer_id".to_string(),
            serde_json::json!(&completion.customer_id),
        ),
        (
            "stripe_subscription_id".to_string(),
            serde_json::json!(&completion.subscription_id),
        ),
        (
            "discount_cents".to_string(),
            serde_json::json!(completion.discount_minor),
        ),
        (
            "tax_cents".to_string(),
            serde_json::json!(completion.tax_minor),
        ),
        (
            "shipping_cents".to_string(),
            serde_json::json!(completion.shipping_minor),
        ),
        (
            "total_cents".to_string(),
            serde_json::json!(completion.total_minor),
        ),
        (
            "amount_cents".to_string(),
            serde_json::json!(completion.total_minor),
        ),
        (
            "reconciliation_status".to_string(),
            serde_json::json!("reconciled"),
        ),
        ("reconciliation_error".to_string(), serde_json::json!("")),
        ("approved_at".to_string(), serde_json::json!(&now)),
        ("payment_at".to_string(), serde_json::json!(&now)),
        ("updated_at".to_string(), serde_json::json!(&now)),
    ]);
    if !completion.payment_intent_id.is_empty() {
        data.insert(
            "provider_payment_status".to_string(),
            serde_json::json!("succeeded"),
        );
        data.insert(
            "provider_payment_error_code".to_string(),
            serde_json::json!(""),
        );
        data.insert(
            "provider_payment_error_message".to_string(),
            serde_json::json!(""),
        );
    }
    if !completion.subscription_id.is_empty() {
        data.insert(
            "subscription_status".to_string(),
            serde_json::json!("active"),
        );
        data.insert(
            "subscription_last_synced_at".to_string(),
            serde_json::json!(&now),
        );
    }
    if adopt_session {
        data.insert(
            "provider_session_id".to_string(),
            serde_json::json!(&completion.session_id),
        );
    }
    db::update_by_filters_count(
        ctx,
        PURCHASES_TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(purchase_id),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::In,
                value: serde_json::json!(["checkout_started", "pending"]),
            },
            Filter {
                field: "provider_session_id".to_string(),
                operator: FilterOp::Equal,
                // Adoption only ever writes onto an order that still has no
                // session id; a concurrent writer landing another id first
                // makes this CAS a no-op instead of an overwrite.
                value: serde_json::json!(&stored_session_id),
            },
        ],
        data,
    )
    .await
}

/// Record a delayed Checkout payment failure only after the signed Session
/// identity is reconciled against the immutable order. This prevents a valid
/// event carrying stale or unrelated metadata from failing another order.
pub(crate) async fn reconcile_checkout_failure(
    ctx: &dyn Context,
    purchase_id: &str,
    completion: &CheckoutSessionCompletion,
    message: &str,
) -> Result<i64, WaferError> {
    let purchase = get(ctx, purchase_id).await?;
    let invalid = |message: &str| {
        WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            format!("Checkout Session failure reconciliation failed: {message}"),
        )
    };
    if purchase.str_field("provider_session_id").is_empty()
        || purchase.str_field("provider_session_id") != completion.session_id
    {
        return Err(invalid("session id does not match the order"));
    }
    if completion.client_reference_id != purchase_id {
        return Err(invalid("client reference does not match the order"));
    }
    if purchase.str_field("stripe_account_id") != completion.event_account {
        return Err(invalid("Stripe account does not match the order"));
    }
    if purchase.bool_field("livemode") != completion.livemode {
        return Err(invalid("test/live mode does not match the order"));
    }
    if purchase.str_field("currency") != completion.currency.to_ascii_uppercase() {
        return Err(invalid("currency does not match the order"));
    }
    if purchase.i64_field("subtotal_cents") != completion.subtotal_minor {
        return Err(invalid("subtotal does not match the immutable quote"));
    }
    let metadata =
        checkout_metadata(&purchase).ok_or_else(|| invalid("order metadata is invalid"))?;
    let expected_mode = metadata
        .get("offer_mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let expected_offer = metadata
        .get("offer_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let expected_version = metadata
        .get("offer_version")
        .and_then(crate::util::json_as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default();
    if expected_mode != completion.mode {
        return Err(invalid("Checkout mode does not match the offer"));
    }
    if expected_offer != completion.offer_id || expected_version != completion.offer_version {
        return Err(invalid("offer identity/version does not match the order"));
    }
    let now = chrono::Utc::now().to_rfc3339();
    db::update_by_filters_count(
        ctx,
        PURCHASES_TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(purchase_id),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::In,
                value: serde_json::json!(["checkout_started", "pending"]),
            },
            Filter {
                field: "provider_session_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(&completion.session_id),
            },
        ],
        HashMap::from([
            ("status".to_string(), serde_json::json!("failed")),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("provider_error"),
            ),
            (
                "reconciliation_error".to_string(),
                serde_json::json!(message),
            ),
            ("updated_at".to_string(), serde_json::json!(now)),
        ]),
    )
    .await
}

/// Reconcile PaymentIntent lifecycle diagnostics for a payment-mode Checkout
/// order. PaymentIntent events do not contain Checkout's authoritative
/// discount/tax/shipping breakdown, so this function deliberately never
/// changes the order's fulfillment status to `completed`.
pub(crate) async fn sync_payment_intent(
    ctx: &dyn Context,
    snapshot: &PaymentIntentSnapshot,
) -> Result<Option<Record>, WaferError> {
    let invalid = |message: &str| {
        WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            format!("PaymentIntent reconciliation failed: {message}"),
        )
    };
    if snapshot.payment_intent_id.is_empty()
        || snapshot.event_created < 0
        || snapshot.amount_minor < 0
        || !matches!(
            snapshot.status.as_str(),
            "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled"
        )
    {
        return Err(invalid(
            "id, supported status, non-negative amount, and event timestamp are required",
        ));
    }
    let currency = money::normalize_currency(&snapshot.currency)
        .map_err(|_| invalid("currency is invalid"))?;
    let purchase = if snapshot.purchase_id.is_empty() {
        match find_by_payment_intent(ctx, &snapshot.payment_intent_id).await {
            Ok(purchase) => purchase,
            Err(error) if error.code == wafer_run::ErrorCode::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    } else {
        match get(ctx, &snapshot.purchase_id).await {
            Ok(purchase) => purchase,
            Err(error) if error.code == wafer_run::ErrorCode::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    };
    let Some(metadata) = checkout_metadata(&purchase) else {
        return Ok(None);
    };
    if metadata
        .get("offer_mode")
        .and_then(serde_json::Value::as_str)
        != Some("payment")
    {
        return Err(invalid("order is not a payment-mode Checkout"));
    }
    if !snapshot.purchase_id.is_empty() {
        let expected_offer = metadata
            .get("offer_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let expected_version = metadata
            .get("offer_version")
            .and_then(crate::util::json_as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_default();
        if snapshot.offer_id.is_empty()
            || snapshot.offer_id != expected_offer
            || snapshot.offer_version != expected_version
        {
            return Err(invalid(
                "metadata offer identity/version does not match the order",
            ));
        }
    }
    if purchase.str_field("stripe_account_id") != snapshot.stripe_account_id {
        return Err(invalid("Stripe account does not match the order"));
    }
    if purchase.bool_field("livemode") != snapshot.livemode {
        return Err(invalid("test/live mode does not match the order"));
    }
    if purchase.str_field("currency") != currency {
        return Err(invalid("currency does not match the order"));
    }
    let current_intent = purchase.str_field("provider_payment_intent_id");
    if !current_intent.is_empty() && current_intent != snapshot.payment_intent_id {
        return Err(invalid("PaymentIntent id does not match the order"));
    }
    if purchase.i64_field("payment_intent_event_created") > snapshot.event_created {
        return Ok(Some(purchase));
    }

    let order_status = purchase.str_field("status");
    let paid = matches!(
        order_status,
        "completed" | "partially_refunded" | "refunded"
    );
    if paid {
        if snapshot.status != "succeeded" {
            // Checkout is the fulfillment authority. A late, non-success
            // PaymentIntent delivery cannot regress an already reconciled
            // paid order even when an old integration did not persist its PI
            // event timestamp.
            return Ok(Some(purchase));
        }
        if purchase.i64_field("total_cents") != snapshot.amount_minor {
            return Err(invalid("amount does not match the completed order"));
        }
    } else if order_status == "failed" && snapshot.status == "succeeded" {
        return Err(invalid(
            "a succeeded PaymentIntent conflicts with a terminal failed order",
        ));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut data = HashMap::from([
        (
            "provider_payment_intent_id".to_string(),
            serde_json::json!(&snapshot.payment_intent_id),
        ),
        (
            "stripe_payment_intent_id".to_string(),
            serde_json::json!(&snapshot.payment_intent_id),
        ),
        (
            "provider_payment_status".to_string(),
            serde_json::json!(&snapshot.status),
        ),
        (
            "provider_payment_error_code".to_string(),
            serde_json::json!(&snapshot.error_code),
        ),
        (
            "provider_payment_error_message".to_string(),
            serde_json::json!(&snapshot.error_message),
        ),
        (
            "payment_intent_event_created".to_string(),
            serde_json::json!(snapshot.event_created),
        ),
        ("updated_at".to_string(), serde_json::json!(&now)),
    ]);
    if !paid {
        let reconciliation_status = match snapshot.status.as_str() {
            "succeeded" => "payment_succeeded_awaiting_checkout",
            "payment_failed" => "payment_failed",
            "processing" => "payment_processing",
            "requires_action" => "payment_requires_action",
            "canceled" => "payment_canceled",
            _ => unreachable!("validated PaymentIntent status"),
        };
        data.insert(
            "reconciliation_status".to_string(),
            serde_json::json!(reconciliation_status),
        );
        data.insert(
            "reconciliation_error".to_string(),
            serde_json::json!(&snapshot.error_message),
        );
    }
    db::update_by_filters_count(
        ctx,
        PURCHASES_TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(&purchase.id),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(order_status),
            },
            Filter {
                field: "provider_payment_intent_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(current_intent),
            },
            Filter {
                field: "payment_intent_event_created".to_string(),
                operator: FilterOp::LessEqual,
                value: serde_json::json!(snapshot.event_created),
            },
        ],
        data,
    )
    .await?;
    Ok(Some(get(ctx, &purchase.id).await?))
}

/// Reconcile a subscription sold through a commerce order. Subscriptions that
/// belong to no commerce purchase (platform-plan subscriptions, or a delivery
/// racing ahead of its own Checkout completion) return `Ok(None)`; the
/// webhook decides whether that means "not ours" or "retry later". A matching
/// order must have the same connected-account and mode context as the event,
/// otherwise the webhook fails closed.
///
/// Stripe deliveries can arrive out of order — including different events in
/// the same `created` second (immediate cancellation emits
/// `customer.subscription.updated` and `customer.subscription.deleted`
/// together). Writes apply the shared transition rules
/// ([`super::subscription_transition_allowed`]) against the current
/// projection and compare-and-swap on the exact (timestamp, status) pair that
/// was read, so two workers racing with old/new events cannot let the older
/// or less-terminal projection win after both read the same previous row.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn sync_commerce_subscription(
    ctx: &dyn Context,
    stripe_subscription_id: &str,
    stripe_account_id: &str,
    livemode: bool,
    status: &str,
    current_period_end: Option<&str>,
    cancel_at_period_end: Option<bool>,
    canceled_at: Option<&str>,
    expected_current_status: Option<&str>,
    event_created: i64,
) -> Result<Option<Record>, WaferError> {
    if stripe_subscription_id.is_empty() || status.is_empty() || event_created < 0 {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "subscription id, status, and a valid event timestamp are required",
        ));
    }
    let mut purchase = match db::get_by_field(
        ctx,
        PURCHASES_TABLE,
        "stripe_subscription_id",
        serde_json::json!(stripe_subscription_id),
    )
    .await
    {
        Ok(purchase) => purchase,
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if purchase.str_field("stripe_account_id") != stripe_account_id {
        return Err(WaferError::new(
            wafer_run::ErrorCode::PermissionDenied,
            "subscription event Stripe account does not match the commerce order",
        ));
    }
    if purchase.bool_field("livemode") != livemode {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "subscription event mode does not match the commerce order",
        ));
    }
    let purchase_id = purchase.id.clone();
    for _ in 0..3 {
        let current_created = purchase.i64_field("subscription_event_created");
        let current_status = purchase.str_field("subscription_status").to_string();
        if !super::subscription_transition_allowed(
            &current_status,
            current_created,
            status,
            event_created,
        ) {
            return Ok(Some(purchase));
        }
        if let Some(expected) = expected_current_status {
            if current_status != expected {
                return Ok(Some(purchase));
            }
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut data = HashMap::from([
            ("subscription_status".to_string(), serde_json::json!(status)),
            (
                "subscription_last_synced_at".to_string(),
                serde_json::json!(&now),
            ),
            ("updated_at".to_string(), serde_json::json!(&now)),
            (
                "subscription_event_created".to_string(),
                serde_json::json!(event_created),
            ),
        ]);
        if let Some(value) = cancel_at_period_end {
            data.insert(
                "subscription_cancel_at_period_end".to_string(),
                serde_json::json!(value),
            );
        }
        if let Some(value) = current_period_end {
            data.insert(
                "subscription_current_period_end".to_string(),
                serde_json::json!(value),
            );
        }
        if let Some(value) = canceled_at {
            data.insert(
                "subscription_canceled_at".to_string(),
                serde_json::json!(value),
            );
        }
        let rows = db::update_by_filters_count(
            ctx,
            PURCHASES_TABLE,
            vec![
                Filter {
                    field: "id".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(&purchase_id),
                },
                Filter {
                    field: "subscription_event_created".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current_created),
                },
                Filter {
                    field: "subscription_status".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(&current_status),
                },
            ],
            data,
        )
        .await?;
        if rows == 1 {
            return Ok(Some(get(ctx, &purchase_id).await?));
        }
        purchase = get(ctx, &purchase_id).await?;
    }
    Err(WaferError::new(
        wafer_run::ErrorCode::FailedPrecondition,
        "commerce subscription state changed concurrently; retry the event",
    ))
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

/// Atomic admin refund: `completed` -> `refunded` with audit fields. Returns
/// rows affected (0 = not completed / already refunded).
#[cfg(test)]
pub(crate) async fn refund_atomic(
    ctx: &dyn Context,
    id: &str,
    refunded_by: &str,
    reason: &str,
) -> Result<i64, WaferError> {
    let purchase = get(ctx, id).await?;
    let total = purchase.i64_field("total_cents");
    let now = chrono::Utc::now().to_rfc3339();
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("status".into(), serde_json::json!("refunded"));
    data.insert("refunded_at".into(), serde_json::json!(&now));
    data.insert("refunded_by".into(), serde_json::json!(refunded_by));
    data.insert("refund_reason".into(), serde_json::json!(reason));
    data.insert("refunded_total_cents".into(), serde_json::json!(total));
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

/// Reconcile an authoritative cumulative refunded amount from a successful
/// provider response or webhook. The expected-current filter makes retries
/// idempotent and prevents a late smaller total from overwriting newer state.
pub(crate) async fn reconcile_refund_total(
    ctx: &dyn Context,
    id: &str,
    target_refunded_total: i64,
    refunded_by: &str,
    note: &str,
) -> Result<Record, WaferError> {
    let purchase = get(ctx, id).await?;
    let total = purchase.i64_field("total_cents");
    let current = purchase.i64_field("refunded_total_cents");
    if total <= 0 || target_refunded_total <= 0 || target_refunded_total > total {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "refunded total must be positive and no greater than the order total",
        ));
    }
    if current >= target_refunded_total {
        return Ok(purchase);
    }
    if !matches!(
        purchase.str_field("status"),
        "completed" | "partially_refunded"
    ) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "purchase is not in a refundable state",
        ));
    }
    let now = chrono::Utc::now().to_rfc3339();
    let mut data = HashMap::from([
        (
            "status".to_string(),
            serde_json::json!(if target_refunded_total == total {
                "refunded"
            } else {
                "partially_refunded"
            }),
        ),
        (
            "refunded_total_cents".to_string(),
            serde_json::json!(target_refunded_total),
        ),
        ("refunded_at".to_string(), serde_json::json!(&now)),
        ("updated_at".to_string(), serde_json::json!(&now)),
    ]);
    if !refunded_by.is_empty() {
        data.insert("refunded_by".to_string(), serde_json::json!(refunded_by));
    }
    if !note.is_empty() {
        data.insert("refund_reason".to_string(), serde_json::json!(note));
    }
    let rows = db::update_by_filters_count(
        ctx,
        PURCHASES_TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(id),
            },
            Filter {
                field: "refunded_total_cents".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(current),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::In,
                value: serde_json::json!(["completed", "partially_refunded"]),
            },
        ],
        data,
    )
    .await?;
    let reconciled = get(ctx, id).await?;
    if rows == 1 || reconciled.i64_field("refunded_total_cents") >= target_refunded_total {
        Ok(reconciled)
    } else {
        Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "purchase refund state changed concurrently",
        ))
    }
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

/// Find an order by its Stripe Checkout Session id. Used to make synthetic or
/// id-less Payment Link webhook retries idempotent in addition to event-id
/// deduplication.
pub(crate) async fn find_by_session(
    ctx: &dyn Context,
    provider_session_id: &str,
) -> Result<Option<Record>, WaferError> {
    match db::get_by_field(
        ctx,
        PURCHASES_TABLE,
        "provider_session_id",
        serde_json::Value::String(provider_session_id.to_string()),
    )
    .await
    {
        Ok(record) => Ok(Some(record)),
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Count all purchases (admin stats).
pub(crate) async fn count_all(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, PURCHASES_TABLE, &[]).await
}

#[derive(Default)]
struct AnalyticsAccumulator {
    gross_volume_minor: i128,
    refunded_volume_minor: i128,
    platform_fees_minor: i128,
    order_count: u128,
    paid_order_count: u128,
    refunded_order_count: u128,
    failed_order_count: u128,
    open_dispute_count: u128,
    open_disputed_volume_minor: i128,
    lost_dispute_count: u128,
    lost_disputed_volume_minor: i128,
    active_subscription_count: u128,
    trialing_subscription_count: u128,
    past_due_subscription_count: u128,
    canceled_subscription_count: u128,
    top_products: BTreeMap<(String, String), (u128, i128)>,
}

fn analytics_overflow(field: &str) -> WaferError {
    WaferError::new(
        wafer_run::ErrorCode::Internal,
        format!("commerce analytics overflowed {field}"),
    )
}

fn analytics_i64(value: i128, field: &str) -> Result<i64, WaferError> {
    i64::try_from(value).map_err(|_| analytics_overflow(field))
}

fn analytics_u64(value: u128, field: &str) -> Result<u64, WaferError> {
    u64::try_from(value).map_err(|_| analytics_overflow(field))
}

/// Build currency-separated commerce analytics from immutable order and line
/// snapshots. `seller_account_id` scopes every header before any totals or
/// top-product rows are considered, keeping seller analytics tenant-isolated.
/// Revenue is never added across currencies.
pub(crate) async fn commerce_analytics(
    ctx: &dyn Context,
    seller_account_id: Option<&str>,
) -> Result<Vec<CommerceAnalytics>, WaferError> {
    let filters = seller_account_id
        .filter(|value| !value.is_empty())
        .map(|value| {
            vec![Filter {
                field: "seller_account_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(value),
            }]
        })
        .unwrap_or_default();
    let orders = db::list_all(ctx, PURCHASES_TABLE, filters).await?;
    let mut by_currency: BTreeMap<String, AnalyticsAccumulator> = BTreeMap::new();
    let mut paid_order_currencies = HashMap::new();

    for order in orders {
        let currency =
            money::normalize_currency(order.str_field("currency")).map_err(|message| {
                WaferError::new(
                    wafer_run::ErrorCode::Internal,
                    format!("order {} has invalid currency: {message}", order.id),
                )
            })?;
        let aggregate = by_currency.entry(currency.clone()).or_default();
        aggregate.order_count += 1;
        let status = order.str_field("status");
        let paid = matches!(status, "completed" | "partially_refunded" | "refunded");
        if paid {
            let total = order.i64_field("total_cents");
            let refunded = order.i64_field("refunded_total_cents");
            let platform_fee = order.i64_field("platform_fee_cents");
            if total < 0 || refunded < 0 || refunded > total || platform_fee < 0 {
                return Err(WaferError::new(
                    wafer_run::ErrorCode::Internal,
                    format!("order {} has invalid analytics amounts", order.id),
                ));
            }
            aggregate.paid_order_count += 1;
            aggregate.gross_volume_minor += i128::from(total);
            aggregate.refunded_volume_minor += i128::from(refunded);
            aggregate.platform_fees_minor += i128::from(platform_fee);
            if refunded > 0 {
                aggregate.refunded_order_count += 1;
            }
            paid_order_currencies.insert(order.id.clone(), currency);
        } else if status == "failed" {
            aggregate.failed_order_count += 1;
        }

        if !order.str_field("stripe_subscription_id").is_empty() {
            match order.str_field("subscription_status") {
                "active" => aggregate.active_subscription_count += 1,
                "trialing" => aggregate.trialing_subscription_count += 1,
                "past_due" | "unpaid" | "paused" => aggregate.past_due_subscription_count += 1,
                "canceled" | "incomplete_expired" => aggregate.canceled_subscription_count += 1,
                _ => {}
            }
        }
    }

    for dispute in super::disputes::list_for_analytics(ctx, seller_account_id).await? {
        let currency =
            money::normalize_currency(dispute.str_field("currency")).map_err(|message| {
                WaferError::new(
                    wafer_run::ErrorCode::Internal,
                    format!("dispute {} has invalid currency: {message}", dispute.id),
                )
            })?;
        let amount = dispute.i64_field("amount_minor");
        if amount <= 0 {
            return Err(WaferError::new(
                wafer_run::ErrorCode::Internal,
                format!("dispute {} has an invalid amount", dispute.id),
            ));
        }
        let aggregate = by_currency.get_mut(&currency).ok_or_else(|| {
            WaferError::new(
                wafer_run::ErrorCode::Internal,
                format!(
                    "dispute {} has no matching order currency aggregate",
                    dispute.id
                ),
            )
        })?;
        match dispute.str_field("status") {
            "warning_needs_response"
            | "warning_under_review"
            | "needs_response"
            | "under_review" => {
                aggregate.open_dispute_count += 1;
                aggregate.open_disputed_volume_minor += i128::from(amount);
            }
            "lost" => {
                aggregate.lost_dispute_count += 1;
                aggregate.lost_disputed_volume_minor += i128::from(amount);
            }
            _ => {}
        }
    }

    let paid_ids: Vec<String> = paid_order_currencies.keys().cloned().collect();
    // Keep each IN query comfortably below common SQLite/D1 parameter limits.
    for chunk in paid_ids.chunks(200) {
        let lines = db::list_all(
            ctx,
            LINE_ITEMS_TABLE,
            vec![Filter {
                field: "purchase_id".to_string(),
                operator: FilterOp::In,
                value: serde_json::Value::Array(
                    chunk.iter().map(|id| serde_json::json!(id)).collect(),
                ),
            }],
        )
        .await?;
        for line in lines {
            let Some(currency) = paid_order_currencies.get(line.str_field("purchase_id")) else {
                continue;
            };
            let quantity = line.i64_field("quantity");
            let revenue = line.i64_field("total_minor");
            if quantity < 0 || revenue < 0 {
                return Err(WaferError::new(
                    wafer_run::ErrorCode::Internal,
                    format!("line item {} has invalid analytics amounts", line.id),
                ));
            }
            let aggregate = by_currency.get_mut(currency).ok_or_else(|| {
                WaferError::new(
                    wafer_run::ErrorCode::Internal,
                    "line item currency aggregate is missing",
                )
            })?;
            let key = (
                line.str_field("product_id").to_string(),
                line.str_field("product_name").to_string(),
            );
            let product = aggregate.top_products.entry(key).or_default();
            product.0 += quantity as u128;
            product.1 += i128::from(revenue);
        }
    }

    by_currency
        .into_iter()
        .map(|(currency, aggregate)| {
            let gross = analytics_i64(aggregate.gross_volume_minor, "gross volume")?;
            let refunded = analytics_i64(aggregate.refunded_volume_minor, "refunded volume")?;
            let mut top_products = aggregate
                .top_products
                .into_iter()
                .map(|((product_id, name), (quantity, revenue))| {
                    Ok(AnalyticsProduct {
                        product_id,
                        name,
                        quantity: analytics_u64(quantity, "top product quantity")?,
                        revenue_minor: analytics_i64(revenue, "top product revenue")?,
                    })
                })
                .collect::<Result<Vec<_>, WaferError>>()?;
            top_products.sort_by(|left, right| {
                right
                    .revenue_minor
                    .cmp(&left.revenue_minor)
                    .then_with(|| right.quantity.cmp(&left.quantity))
                    .then_with(|| left.name.cmp(&right.name))
                    .then_with(|| left.product_id.cmp(&right.product_id))
            });
            top_products.truncate(5);
            Ok(CommerceAnalytics {
                currency,
                gross_volume_minor: gross,
                refunded_volume_minor: refunded,
                net_volume_minor: gross - refunded,
                platform_fees_minor: analytics_i64(aggregate.platform_fees_minor, "platform fees")?,
                order_count: analytics_u64(aggregate.order_count, "order count")?,
                paid_order_count: analytics_u64(aggregate.paid_order_count, "paid order count")?,
                refunded_order_count: analytics_u64(
                    aggregate.refunded_order_count,
                    "refunded order count",
                )?,
                failed_order_count: analytics_u64(
                    aggregate.failed_order_count,
                    "failed order count",
                )?,
                open_dispute_count: analytics_u64(
                    aggregate.open_dispute_count,
                    "open dispute count",
                )?,
                open_disputed_volume_minor: analytics_i64(
                    aggregate.open_disputed_volume_minor,
                    "open disputed volume",
                )?,
                lost_dispute_count: analytics_u64(
                    aggregate.lost_dispute_count,
                    "lost dispute count",
                )?,
                lost_disputed_volume_minor: analytics_i64(
                    aggregate.lost_disputed_volume_minor,
                    "lost disputed volume",
                )?,
                active_subscription_count: analytics_u64(
                    aggregate.active_subscription_count,
                    "active subscription count",
                )?,
                trialing_subscription_count: analytics_u64(
                    aggregate.trialing_subscription_count,
                    "trialing subscription count",
                )?,
                past_due_subscription_count: analytics_u64(
                    aggregate.past_due_subscription_count,
                    "past-due subscription count",
                )?,
                canceled_subscription_count: analytics_u64(
                    aggregate.canceled_subscription_count,
                    "canceled subscription count",
                )?,
                top_products,
            })
        })
        .collect()
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
