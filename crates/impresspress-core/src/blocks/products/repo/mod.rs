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
