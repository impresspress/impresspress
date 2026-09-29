//! Admin- and user-facing HTTP handlers for the impresspress/products block.
//!
//! Dispatches product, typed-offer, seller, order, storefront, and Stripe
//! operations for admin and user-facing routes.
//!
//! Split by domain responsibility:
//! - [`dispatch`] — `run`, the one fan-out from a matched
//!   `routes::Route` to the domain modules below, the order and Stripe
//!   modules, and the SSR pages.
//! - [`product`] — product CRUD, both admin (`/b/products/api/admin/products`)
//!   and user-owned (`/b/products/api/products`, gated on
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
pub(in crate::blocks::products) mod provider;
pub(in crate::blocks::products) mod seller_policy;
mod sellers;
mod stats;
mod subscription;
mod types;

pub(in crate::blocks::products) use dispatch::{run, user_products_enabled};
pub(in crate::blocks::products) use product::{
    is_owned_by, name_like_filter, product_json, write_error,
};

// The four table constants that used to live here (`GROUPS_TABLE`,
// `TYPES_TABLE`, `GROUP_TEMPLATES_TABLE`, `PRODUCT_TEMPLATES_TABLE`) moved to
// `repo::{groups, types, group_templates, product_templates}`, where the rest
// of the block's tables have always been declared. They were the last four
// products tables with no door, which is what let the SSR pages and the stats
// endpoint build their own queries against them; `tests/repo_door.rs` now
// covers all of them.
//
// `default_template_id(ctx, table)` went with them. It took the table as a
// parameter and so was a query no module owned; it is
// `repo::group_templates::default_id` and `repo::product_templates::default_id`
// now, one per table.
