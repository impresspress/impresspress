//! Data access for the platform-billing subscriptions table.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    blocks::products::contracts::{SubscriptionStatus, SubscriptionView},
    util::{enum_column, RecordExt},
};

/// Platform-billing subscription table — one row per user.
pub(crate) const SUBSCRIPTIONS_TABLE: &str = "impresspress__products__subscriptions";

/// The metadata key marking a Stripe subscription item as an add-on. An item
/// carrying it on neither its own metadata nor its price's is the base plan,
/// and none of [`ADDON_TOTALS`] is read from it.
///
/// It sits beside the value keys because the two halves are one contract: this
/// decides whether an item counts at all, and those say what it contributes.
/// It is also what picks the object to read the values from, so the reader in
/// `stripe.rs` tests for it on each in turn.
pub(crate) const ADDON_ITEM_MARKER: &str = "addon_id";

/// Each add-on total: the Stripe metadata key that carries it, and the column
/// [`set_addon_totals`] writes it to.
///
/// The metadata keys are names the platform stamps on its own Stripe objects,
/// not operator settings — they arrive inside every
/// `customer.subscription.updated` payload, on the subscription item or on its
/// price, whichever the platform stamped. Renaming one means rewriting those
/// objects in Stripe, which no configuration value can do. Keeping the pair in
/// one table is what makes the reader and the writer below agree on order.
///
/// Which object the platform stamps is not decidable here: nothing in this
/// repository creates add-on subscription items any more. The one thing that
/// did — `products/addons.rs`, deleted in `1f6489d8` because it depended on a
/// plan table — posted `metadata[addon_id]` to `/v1/subscription_items`, so
/// item-level is the convention this reader was written against, and the
/// per-price form is the other shape it has always claimed to accept. Both are
/// read, because the block cannot see which one the platform chose.
pub(crate) const ADDON_TOTALS: [(&str, &str); 4] = [
    ("extra_projects", "addon_projects"),
    ("extra_requests", "addon_requests"),
    ("extra_r2_bytes", "addon_r2_bytes"),
    ("extra_d1_bytes", "addon_d1_bytes"),
];

fn platform_update_data(
    stripe_customer_id: &str,
    stripe_subscription_id: &str,
    plan: &str,
    event_created: i64,
    now: &str,
) -> HashMap<String, serde_json::Value> {
    HashMap::from([
        (
            "stripe_customer_id".to_string(),
            serde_json::json!(stripe_customer_id),
        ),
        (
            "stripe_subscription_id".to_string(),
            serde_json::json!(stripe_subscription_id),
        ),
        ("plan".to_string(), serde_json::json!(plan)),
        (
            "status".to_string(),
            serde_json::json!(SubscriptionStatus::Active),
        ),
        ("grace_period_end".to_string(), serde_json::Value::Null),
        (
            "stripe_event_created".to_string(),
            serde_json::json!(event_created),
        ),
        ("updated_at".to_string(), serde_json::json!(now)),
    ])
}

async fn update_platform_if_current(
    ctx: &dyn Context,
    user_id: &str,
    stripe_customer_id: &str,
    stripe_subscription_id: &str,
    plan: &str,
    event_created: i64,
) -> Result<i64, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    db::update_by_filters_count(
        ctx,
        SUBSCRIPTIONS_TABLE,
        vec![
            Filter {
                field: "user_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(user_id),
            },
            Filter {
                field: "stripe_event_created".to_string(),
                operator: FilterOp::LessEqual,
                value: serde_json::json!(event_created),
            },
        ],
        platform_update_data(
            stripe_customer_id,
            stripe_subscription_id,
            plan,
            event_created,
            &now,
        ),
    )
    .await
}

/// Insert or timestamp-conditionally update the platform subscription for a
/// user. The deterministic row id and unique `user_id` index serialize races;
/// a loser retries through the same atomic timestamp predicate. Late Checkout
/// deliveries therefore cannot overwrite a newer invoice/subscription event.
/// Returns rows affected.
pub(crate) async fn upsert_platform(
    ctx: &dyn Context,
    user_id: &str,
    stripe_customer_id: &str,
    stripe_subscription_id: &str,
    plan: &str,
    event_created: i64,
) -> Result<i64, WaferError> {
    match db::get_by_field(
        ctx,
        SUBSCRIPTIONS_TABLE,
        "user_id",
        serde_json::json!(user_id),
    )
    .await
    {
        Ok(_) => {
            return update_platform_if_current(
                ctx,
                user_id,
                stripe_customer_id,
                stripe_subscription_id,
                plan,
                event_created,
            )
            .await;
        }
        Err(error) if error.code == ErrorCode::NotFound => {}
        Err(error) => return Err(error),
    }

    let now = chrono::Utc::now().to_rfc3339();
    let sub_id = format!("sub_{user_id}");
    let mut data = platform_update_data(
        stripe_customer_id,
        stripe_subscription_id,
        plan,
        event_created,
        &now,
    );
    data.insert("id".to_string(), serde_json::json!(sub_id));
    data.insert("user_id".to_string(), serde_json::json!(user_id));
    data.insert("created_at".to_string(), serde_json::json!(&now));
    match db::create(ctx, SUBSCRIPTIONS_TABLE, data).await {
        Ok(_) => Ok(1),
        Err(create_error) => {
            // Another worker may have inserted the deterministic/unique row
            // after our lookup. Only treat that race as recoverable when the
            // row now exists; otherwise preserve the original database error.
            match db::get_by_field(
                ctx,
                SUBSCRIPTIONS_TABLE,
                "user_id",
                serde_json::json!(user_id),
            )
            .await
            {
                Ok(_) => {
                    update_platform_if_current(
                        ctx,
                        user_id,
                        stripe_customer_id,
                        stripe_subscription_id,
                        plan,
                        event_created,
                    )
                    .await
                }
                Err(error) if error.code == ErrorCode::NotFound => Err(create_error),
                Err(error) => Err(error),
            }
        }
    }
}

/// Fetch the platform subscription row referencing a Stripe subscription id,
/// or `None` when no row references it.
async fn get_by_stripe_sub(
    ctx: &dyn Context,
    stripe_subscription_id: &str,
) -> Result<Option<db::Record>, WaferError> {
    let rows = db::list(
        ctx,
        SUBSCRIPTIONS_TABLE,
        &ListOptions {
            filters: vec![Filter {
                field: "stripe_subscription_id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(stripe_subscription_id),
            }],
            limit: Some(1),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    Ok(rows.records.into_iter().next())
}

/// Whether any platform-billing subscription row references this Stripe
/// subscription id. Unlike [`find_user_by_stripe_sub`], database failures
/// surface as `Err` instead of collapsing into "not found", because the
/// webhook uses this answer to decide between sealing an event as processed
/// and scheduling a retry. Every neighbouring lookup follows the same rule
/// now; this one got there first.
pub(crate) async fn platform_subscription_exists(
    ctx: &dyn Context,
    stripe_subscription_id: &str,
) -> Result<bool, WaferError> {
    Ok(get_by_stripe_sub(ctx, stripe_subscription_id)
        .await?
        .is_some())
}

/// Sync status (and optionally plan) from a `customer.subscription.updated`
/// event, matched by Stripe subscription id. Applies the shared transition
/// rules ([`super::subscription_transition_allowed`]): strictly older events
/// never apply, nothing leaves a terminal status, and an equal-second
/// delivery may only move toward a more-terminal status (immediate
/// cancellation emits `updated` and `deleted` with the same `created` second
/// — the deletion stays authoritative regardless of delivery order). Writes
/// compare-and-swap on the exact (timestamp, status) pair that was read.
///
/// A `status` of [`SubscriptionStatus::Unset`] means the event reported no
/// status: the stored status is kept, and the transition rules judge the
/// event as if it restated that status, so only the ordering rule can refuse
/// it. The plan (when given) and the event timestamp are still applied —
/// including over a terminal row, which such an event does not try to move
/// away from and so is no longer refused by the terminal rule.
/// Returns rows affected (0 = no matching row, or the event was refused).
pub(crate) async fn update_status_plan(
    ctx: &dyn Context,
    stripe_subscription_id: &str,
    status: SubscriptionStatus,
    plan: Option<&str>,
    event_created: i64,
) -> Result<i64, WaferError> {
    let Some(mut current) = get_by_stripe_sub(ctx, stripe_subscription_id).await? else {
        return Ok(0);
    };
    for _ in 0..3 {
        let current_created = current.i64_field("stripe_event_created");
        let current_status: SubscriptionStatus = enum_column(&current, "status")?;
        let incoming_status = if status == SubscriptionStatus::Unset {
            current_status
        } else {
            status
        };
        if !super::subscription_transition_allowed(
            current_status,
            current_created,
            incoming_status,
            event_created,
        ) {
            return Ok(0);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        // An event without a status leaves the column as it is.
        if status != SubscriptionStatus::Unset {
            data.insert("status".into(), serde_json::json!(status));
        }
        data.insert("updated_at".into(), serde_json::json!(&now));
        data.insert(
            "stripe_event_created".into(),
            serde_json::json!(event_created),
        );
        if let Some(plan) = plan {
            data.insert("plan".into(), serde_json::json!(plan));
        }
        let rows = db::update_by_filters_count(
            ctx,
            SUBSCRIPTIONS_TABLE,
            vec![
                Filter {
                    field: "stripe_subscription_id".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(stripe_subscription_id),
                },
                Filter {
                    field: "stripe_event_created".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current_created),
                },
                Filter {
                    field: "status".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current_status),
                },
            ],
            data,
        )
        .await?;
        if rows == 1 {
            return Ok(1);
        }
        current = match get_by_stripe_sub(ctx, stripe_subscription_id).await? {
            Some(record) => record,
            None => return Ok(0),
        };
    }
    Err(WaferError::new(
        ErrorCode::FailedPrecondition,
        "platform subscription state changed concurrently; retry the event",
    ))
}

/// Mark a subscription past-due with a 7-day grace window
/// (`invoice.payment_failed`). A failed payment on a leftover open invoice
/// after `customer.subscription.deleted` must not move a terminal row back to
/// `past_due` and grant a fresh grace window, so the write requires an
/// equal-or-newer event timestamp AND a non-terminal current status
/// (mirroring [`recover_from_paid_invoice`]'s status guard). Returns rows
/// affected (0 = no matching row, or the event was refused).
pub(crate) async fn mark_past_due(
    ctx: &dyn Context,
    stripe_subscription_id: &str,
    event_created: i64,
) -> Result<i64, WaferError> {
    let Some(mut current) = get_by_stripe_sub(ctx, stripe_subscription_id).await? else {
        return Ok(0);
    };
    for _ in 0..3 {
        let current_created = current.i64_field("stripe_event_created");
        let current_status: SubscriptionStatus = enum_column(&current, "status")?;
        if !super::subscription_transition_allowed(
            current_status,
            current_created,
            SubscriptionStatus::PastDue,
            event_created,
        ) {
            return Ok(0);
        }
        let now = chrono::Utc::now();
        let grace_end = (now + chrono::Duration::days(7)).to_rfc3339();
        let now = now.to_rfc3339();
        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        data.insert(
            "status".into(),
            serde_json::json!(SubscriptionStatus::PastDue),
        );
        data.insert("grace_period_end".into(), serde_json::json!(&grace_end));
        data.insert("updated_at".into(), serde_json::json!(&now));
        data.insert(
            "stripe_event_created".into(),
            serde_json::json!(event_created),
        );
        let rows = db::update_by_filters_count(
            ctx,
            SUBSCRIPTIONS_TABLE,
            vec![
                Filter {
                    field: "stripe_subscription_id".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(stripe_subscription_id),
                },
                Filter {
                    field: "stripe_event_created".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current_created),
                },
                Filter {
                    field: "status".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current_status),
                },
            ],
            data,
        )
        .await?;
        if rows == 1 {
            return Ok(1);
        }
        current = match get_by_stripe_sub(ctx, stripe_subscription_id).await? {
            Some(record) => record,
            None => return Ok(0),
        };
    }
    Err(WaferError::new(
        ErrorCode::FailedPrecondition,
        "platform subscription state changed concurrently; retry the event",
    ))
}

/// Restore a subscription only when a newer successful invoice follows a
/// local `past_due` state. A paid final invoice must not resurrect an already
/// canceled subscription.
pub(crate) async fn recover_from_paid_invoice(
    ctx: &dyn Context,
    stripe_subscription_id: &str,
    event_created: i64,
) -> Result<i64, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    db::update_by_filters_count(
        ctx,
        SUBSCRIPTIONS_TABLE,
        vec![
            Filter {
                field: "stripe_subscription_id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(stripe_subscription_id),
            },
            Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(SubscriptionStatus::PastDue),
            },
            Filter {
                field: "stripe_event_created".into(),
                operator: FilterOp::LessEqual,
                value: serde_json::json!(event_created),
            },
        ],
        HashMap::from([
            (
                "status".into(),
                serde_json::json!(SubscriptionStatus::Active),
            ),
            ("grace_period_end".into(), serde_json::Value::Null),
            (
                "stripe_event_created".into(),
                serde_json::json!(event_created),
            ),
            ("updated_at".into(), serde_json::json!(now)),
        ]),
    )
    .await
}

/// Cancel a subscription and reset every addon column to 0
/// (`customer.subscription.deleted`). Returns rows affected.
pub(crate) async fn cancel_and_reset_addons(
    ctx: &dyn Context,
    stripe_subscription_id: &str,
    event_created: i64,
) -> Result<i64, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert(
        "status".into(),
        serde_json::json!(SubscriptionStatus::Canceled),
    );
    for (_, column) in ADDON_TOTALS {
        data.insert(column.into(), serde_json::json!(0));
    }
    data.insert("updated_at".into(), serde_json::json!(&now));
    data.insert(
        "stripe_event_created".into(),
        serde_json::json!(event_created),
    );
    db::update_by_filters_count(
        ctx,
        SUBSCRIPTIONS_TABLE,
        vec![
            Filter {
                field: "stripe_subscription_id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(stripe_subscription_id),
            },
            Filter {
                field: "stripe_event_created".into(),
                operator: FilterOp::LessEqual,
                value: serde_json::json!(event_created),
            },
        ],
        data,
    )
    .await
}

/// Set the add-on column totals on a user's subscription, in the order of
/// [`ADDON_TOTALS`]. The caller (stripe.rs) sums Stripe subscription-item
/// metadata into the totals; this writes them. Returns rows affected.
///
/// Two predicates guard the write, and a same-second cancellation — which
/// emits `updated` and `deleted` with one `created` — wants both:
///
/// * **Not a terminal row.** [`SubscriptionStatus::is_terminal`] is "this row
///   can never go live again", because Stripe issues a new subscription id for
///   a resubscription. Writing quota onto one is writing it to an account that
///   no longer has a subscription; in the `canceled` case it would also undo
///   the zeroing [`cancel_and_reset_addons`] just did. Every non-terminal
///   status is written, including `trialing` and `past_due`: these columns are
///   a projection of what Stripe reports, and which of those states earns the
///   quota is the reading platform's decision, not this block's. Filtering to
///   `active` instead lost an add-on bought during a trial until the next
///   `updated` delivery, and one bought while past due until whenever the item
///   set next changed.
/// * **Not older than the row.** `event_created` filters on
///   `stripe_event_created`, the same ordering predicate every other write to
///   this table carries: a failed delivery that Stripe retries after a newer
///   one has landed must not put the older payload's totals back. It is a
///   filter only — the column is deliberately not stamped here, because
///   [`update_status_plan`] compare-and-swaps on the exact value it read and a
///   second writer moving it would make that CAS miss.
pub(crate) async fn set_addon_totals(
    ctx: &dyn Context,
    user_id: &str,
    totals: [i64; ADDON_TOTALS.len()],
    event_created: i64,
) -> Result<i64, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    for ((_, column), total) in ADDON_TOTALS.iter().zip(totals) {
        data.insert((*column).into(), serde_json::json!(total));
    }
    data.insert("updated_at".into(), serde_json::json!(now));
    let mut filters = vec![
        Filter {
            field: "user_id".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!(user_id),
        },
        Filter {
            field: "stripe_event_created".into(),
            operator: FilterOp::LessEqual,
            value: serde_json::json!(event_created),
        },
    ];
    filters.extend(
        SubscriptionStatus::ALL
            .into_iter()
            .filter(|status| status.is_terminal())
            .map(|status| Filter {
                field: "status".into(),
                operator: FilterOp::NotEqual,
                value: serde_json::json!(status),
            }),
    );
    db::update_by_filters_count(ctx, SUBSCRIPTIONS_TABLE, filters, data).await
}

/// Look up the user_id owning a Stripe subscription. `Ok(None)` is "no row
/// references this Stripe subscription".
///
/// Errors used to collapse into that same `None`, which the two webhook
/// callers read as "this subscription is unowned": a database blip skipped
/// the addon-total sync and the outbound `products.subscription.updated`
/// while the delivery still reported success to Stripe, so nothing was
/// retried and nothing was logged. Same rule as
/// [`platform_subscription_exists`], right above.
pub(crate) async fn find_user_by_stripe_sub(
    ctx: &dyn Context,
    stripe_subscription_id: &str,
) -> Result<Option<String>, WaferError> {
    let rows = db::list(
        ctx,
        SUBSCRIPTIONS_TABLE,
        &ListOptions {
            columns: Some(vec!["user_id".into()]),
            filters: vec![Filter {
                field: "stripe_subscription_id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(stripe_subscription_id),
            }],
            limit: Some(1),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    Ok(rows
        .records
        .first()
        .and_then(|record| record.data.get("user_id"))
        .and_then(|value| value.as_str())
        .map(String::from))
}

/// Whether the user has an `active` subscription whose `plan` equals `plan`.
///
/// `Ok(false)` is "no such subscription". A failed read is `Err`, because the
/// answer gates a purchase: it used to end in `matches!(rows, Ok(..))`, so an
/// outage told the buyer they did not own the product their checkout required.
pub(crate) async fn active_plan_exists(
    ctx: &dyn Context,
    user_id: &str,
    plan: &str,
) -> Result<bool, WaferError> {
    let rows = db::list(
        ctx,
        SUBSCRIPTIONS_TABLE,
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
                    value: serde_json::json!(SubscriptionStatus::Active),
                },
                Filter {
                    field: "plan".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(plan),
                },
            ],
            limit: Some(1),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    Ok(!rows.records.is_empty())
}

/// Fetch a user's subscription row with addon columns coalesced to 0 for the
/// admin subscription-status endpoint. `Ok(None)` means "no subscription
/// row" (the legitimate, explicit not-found case). A real repository
/// failure returns `Err` instead of being folded into the same `None` the
/// caller uses for "user has no subscription" — those are different facts
/// and previously reported identically as `{"subscription": null}`.
///
/// This is not a grouped aggregate — it's a single-row lookup by `user_id`
/// with 4 addon columns defaulted from NULL/absent to 0, so it's built on
/// `db::get_by_field` rather than `db::aggregate` (which can't express an
/// empty-group `COALESCE`). The response is returned directly to the
/// authenticated user via `handle_subscription`, so the row is projected
/// through [`SubscriptionView`], whose closed field list is what keeps
/// `user_id` / `stripe_customer_id` out of it.
pub(crate) async fn subscription_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Option<SubscriptionView>, WaferError> {
    match db::get_by_field(
        ctx,
        SUBSCRIPTIONS_TABLE,
        "user_id",
        serde_json::json!(user_id),
    )
    .await
    {
        Ok(record) => SubscriptionView::from_record(&record).map(Some),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
