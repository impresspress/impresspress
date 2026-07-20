//! Admin- and user-facing HTTP handlers for the impresspress/products block.
//!
//! Dispatches product, typed-offer, seller, order, storefront, and Stripe
//! operations for admin and user-facing routes.
//!
//! Split by domain responsibility:
//! - [`dispatch`] — the `AdminRoute`/`UserRoute` tables and the
//!   `handle_admin`/`handle_user` entry points that match a normalized
//!   sub-path and fan out to the domain modules below or the order and Stripe
//!   modules.
//! - [`product`] — product CRUD, both admin (`/admin/b/products/products`)
//!   and user-owned (`/b/products/products`, gated on
//!   `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS`).
//! - [`group`] — group CRUD (admin + user-owned), the "products in a
//!   group" listing, and the read-only group-templates listing.
//! - [`types`] — product-type taxonomy CRUD.
//! - [`catalog`] — the public product catalog (`/b/products/catalog`).
//! - [`subscription`] — the authenticated subscription-status endpoint.
//! - [`stats`] — the admin dashboard counts/revenue endpoint.
//!
//! Every item that was previously reachable at `handlers::*` is re-exported
//! here so `products/mod.rs`, `pages.rs`, and `tests/harness.rs` keep using
//! the same paths unchanged.

mod catalog;
mod commerce;
mod dispatch;
mod group;
mod offers;
mod payment_links;
mod product;
mod provider;
pub(in crate::blocks::products) mod seller_policy;
mod sellers;
mod stats;
mod subscription;
mod types;

pub(in crate::blocks::products) use dispatch::user_products_enabled;
pub use dispatch::{handle_admin, handle_user};
pub(in crate::blocks::products) use product::name_like_filter;
use wafer_core::clients::database as db;
use wafer_run::context::Context;

/// Products catalog table — one row per product offering.
pub(crate) const PRODUCTS_TABLE: &str = "impresspress__products__products";

/// Product groups (categories / bundles) table.
pub(crate) const GROUPS_TABLE: &str = "impresspress__products__groups";

/// Product types (taxonomy) table.
pub(crate) const TYPES_TABLE: &str = "impresspress__products__types";

/// Reusable group template definitions (admin-authored).
pub(crate) const GROUP_TEMPLATES_TABLE: &str = "impresspress__products__group_templates";

/// Reusable product template definitions (admin-authored).
pub(crate) const PRODUCT_TEMPLATES_TABLE: &str = "impresspress__products__product_templates";

/// Look up the id of the `name = "default"` template seeded by the Init
/// lifecycle. Used so client-omitted `*_template_id` fields default to a
/// real (UUIDv7) row instead of the literal integer `1` (which never
/// matches the seeded record and breaks any FK constraint).
///
/// Shared by [`product::handle_user_create_product`] (`product_template_id`)
/// and [`group::handle_user_create_group`] (`group_template_id`).
async fn default_template_id(ctx: &dyn Context, table: &str) -> Option<String> {
    db::get_by_field(ctx, table, "name", serde_json::json!("default"))
        .await
        .ok()
        .map(|r| r.id)
}
