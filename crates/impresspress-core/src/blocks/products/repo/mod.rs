//! Data-access layer for the products block's purchases and subscriptions
//! domains. Each submodule owns its table name(s) (the canonical
//! `repo`-module-owns-its-`TABLE` convention) and is the sole place that
//! issues `db::*` / `wafer_sql_utils` statements against those tables. Block
//! handlers call these functions and keep all HTTP, authz, logging, and
//! Stripe-retry policy at the call site.

pub(crate) mod checkout_presets;
pub(crate) mod disputes;
pub(crate) mod entitlements;
pub(crate) mod offer_components;
pub(crate) mod offers;
pub(crate) mod payment_links;
pub(crate) mod product_versions;
pub(crate) mod provider_operations;
pub(crate) mod purchases;
pub(crate) mod refunds;
pub(crate) mod seller_accounts;
pub(crate) mod subscription_items;
pub(crate) mod subscriptions;
pub(crate) mod variables;

/// How far along the Stripe subscription lifecycle a status is, for ordering
/// same-second webhook deliveries: terminal statuses (`canceled`/`cancelled`/
/// `incomplete_expired`) outrank delinquency (`past_due`/`unpaid`), which
/// outranks every live status (`active`, `trialing`, ...). Immediate
/// cancellation makes Stripe emit `customer.subscription.updated` and
/// `customer.subscription.deleted` with the same `created` second; ranking
/// keeps the deletion authoritative regardless of delivery order. Stripe
/// spells the terminal state "canceled" while the platform projection
/// historically stores "cancelled" — both are the same state.
pub(crate) fn subscription_status_rank(status: &str) -> u8 {
    match status {
        "canceled" | "cancelled" | "incomplete_expired" => 2,
        "past_due" | "unpaid" => 1,
        _ => 0,
    }
}

/// Whether a subscription status is terminal for its subscription id. A
/// canceled/expired Stripe subscription never becomes live again — a
/// resubscription creates a new subscription id — so no later event may move
/// a projection away from a terminal status.
pub(crate) fn subscription_status_is_terminal(status: &str) -> bool {
    subscription_status_rank(status) == 2
}

/// Whether a subscription webhook write may apply over the stored projection:
/// strictly older events never apply, nothing leaves a terminal status, and
/// an equal-second delivery may only move toward a more-terminal status.
pub(crate) fn subscription_transition_allowed(
    current_status: &str,
    current_event_created: i64,
    incoming_status: &str,
    incoming_event_created: i64,
) -> bool {
    if current_event_created > incoming_event_created {
        return false;
    }
    if subscription_status_is_terminal(current_status)
        && !subscription_status_is_terminal(incoming_status)
    {
        return false;
    }
    if current_event_created == incoming_event_created
        && subscription_status_rank(incoming_status) < subscription_status_rank(current_status)
    {
        return false;
    }
    true
}
