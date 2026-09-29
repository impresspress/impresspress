//! Data-access layer for the products block's purchases and subscriptions
//! domains. Each submodule owns its table name(s) (the canonical
//! `repo`-module-owns-its-`TABLE` convention) and is the sole place that
//! issues `db::*` / `wafer_sql_utils` statements against those tables. Block
//! handlers call these functions and keep all HTTP, authz, logging, and
//! Stripe-retry policy at the call site.

pub(crate) mod checkout_presets;
pub(crate) mod disputes;
pub(crate) mod entitlements;
pub(crate) mod group_templates;
pub(crate) mod groups;
pub(crate) mod offer_components;
pub(crate) mod offers;
pub(crate) mod payment_links;
pub(crate) mod product_templates;
pub(crate) mod product_versions;
pub(crate) mod products;
pub(crate) mod provider_operations;
pub(crate) mod purchases;
pub(crate) mod refunds;
pub(crate) mod seller_accounts;
pub(crate) mod stripe_events;
pub(crate) mod subscription_items;
pub(crate) mod subscriptions;
pub(crate) mod types;
pub(crate) mod variables;

use super::contracts::SubscriptionStatus;

/// Attempts a leased unit of Stripe work gets before it dead-letters for
/// operator review. One budget for both leased queues: webhook events
/// (`stripe.rs`) and provider operations (`provider_operations`).
pub(crate) const MAX_ATTEMPTS: u64 = 8;

/// Local backoff after failed attempt number `attempts` (1-based):
/// `30 · 2^(attempts-1)` seconds, capped at one hour.
pub(crate) fn retry_delay_seconds(attempts: u64) -> i64 {
    let exponent = attempts.saturating_sub(1).min(7) as u32;
    (30_i64.saturating_mul(2_i64.pow(exponent))).min(3600)
}

/// Whether a subscription webhook write may apply over the stored projection:
/// strictly older events never apply, nothing leaves a terminal status, and
/// an equal-second delivery may only move toward a more-terminal status.
pub(crate) fn subscription_transition_allowed(
    current_status: SubscriptionStatus,
    current_event_created: i64,
    incoming_status: SubscriptionStatus,
    incoming_event_created: i64,
) -> bool {
    if current_event_created > incoming_event_created {
        return false;
    }
    if current_status.is_terminal() && !incoming_status.is_terminal() {
        return false;
    }
    if current_event_created == incoming_event_created
        && incoming_status.rank() < current_status.rank()
    {
        return false;
    }
    true
}
