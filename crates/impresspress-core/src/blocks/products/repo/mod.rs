//! Data-access layer for the products block's purchases and subscriptions
//! domains. Each submodule owns its table name(s) (the canonical
//! `repo`-module-owns-its-`TABLE` convention) and is the sole place that
//! issues `db::*` / `wafer_sql_utils` statements against those tables. Block
//! handlers call these functions and keep all HTTP, authz, logging, and
//! Stripe-retry policy at the call site.

pub(crate) mod purchases;
pub(crate) mod subscriptions;
