//! Admin/user route dispatch tables for the products block.
//!
//! `handle_admin`/`handle_user` are the two entry points called by
//! `ProductsBlock::handle` (via `handlers::handle_admin`/`handlers::handle_user`,
//! re-exported at the `handlers` module root) with the normalized sub-path
//! already resolved. Each matches against a declarative [`EndpointRoute`]
//! table and fans out to the domain handler modules.

use wafer_core::clients::config;
use wafer_run::{context::Context, HttpMethod, InputStream, Message, OutputStream};

use super::{catalog, group, pricing_template, product, stats, subscription, types};
use crate::{
    blocks::products::{pricing, purchase, stripe, variables},
    endpoint_match::{self, EndpointRoute},
    http::{err_forbidden, err_not_found},
};

/// Whether `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` is on — gates the
/// user-facing (non-admin) product/group CRUD routes below. Visible at
/// `crate::blocks::products` (re-exported as `handlers::user_products_enabled`)
/// so the admin Overview page (`pages::overview`) can render an accurate
/// notice instead of a silent empty catalog when it's off.
pub(in crate::blocks::products) async fn user_products_enabled(ctx: &dyn Context) -> bool {
    config::get_default(ctx, "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "false").await == "true"
}

/// Admin JSON-API dispatch targets (normalized `/admin/b/products/...`).
#[derive(Clone, Copy)]
pub(crate) enum AdminRoute {
    ListProducts,
    GetProduct,
    CreateProduct,
    UpdateProduct,
    DeleteProduct,
    ListGroups,
    CreateGroup,
    UpdateGroup,
    DeleteGroup,
    ListTypes,
    CreateType,
    DeleteType,
    ListPricing,
    CreatePricing,
    UpdatePricing,
    DeletePricing,
    ListVariables,
    CreateVariable,
    UpdateVariable,
    DeleteVariable,
    ListPurchases,
    RefundPurchase,
    GetPurchase,
    Stats,
}

/// Admin dispatch table over the normalized `/admin/b/products/...` paths.
/// The `purchases/{id}/refund` template precedes the generic
/// `purchases/{id}` so the refund route wins (the old `ends_with("/refund")`
/// guard).
const ADMIN_ROUTES: &[EndpointRoute<AdminRoute>] = &[
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/products",
        AdminRoute::ListProducts,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products",
        AdminRoute::CreateProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/products/{id}",
        AdminRoute::GetProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/admin/b/products/products/{id}",
        AdminRoute::UpdateProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/products/{id}",
        AdminRoute::DeleteProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/groups",
        AdminRoute::ListGroups,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/groups",
        AdminRoute::CreateGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/admin/b/products/groups/{id}",
        AdminRoute::UpdateGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/groups/{id}",
        AdminRoute::DeleteGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/types",
        AdminRoute::ListTypes,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/types",
        AdminRoute::CreateType,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/types/{id}",
        AdminRoute::DeleteType,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/pricing",
        AdminRoute::ListPricing,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/pricing",
        AdminRoute::CreatePricing,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/admin/b/products/pricing/{id}",
        AdminRoute::UpdatePricing,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/pricing/{id}",
        AdminRoute::DeletePricing,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/variables",
        AdminRoute::ListVariables,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/variables",
        AdminRoute::CreateVariable,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/admin/b/products/variables/{id}",
        AdminRoute::UpdateVariable,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/variables/{id}",
        AdminRoute::DeleteVariable,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/purchases",
        AdminRoute::ListPurchases,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/admin/b/products/purchases/{id}/refund",
        AdminRoute::RefundPurchase,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/purchases/{id}",
        AdminRoute::GetPurchase,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/stats",
        AdminRoute::Stats,
    ),
];

/// User-facing dispatch targets (normalized `/b/products/...`).
#[derive(Clone, Copy)]
pub(crate) enum UserRoute {
    ListProducts,
    GetProduct,
    CreateProduct,
    UpdateProduct,
    DeleteProduct,
    ListGroups,
    GetGroup,
    CreateGroup,
    UpdateGroup,
    DeleteGroup,
    GroupProducts,
    ListTypes,
    GroupTemplates,
    Catalog,
    CatalogItem,
    CalculatePrice,
    CreatePurchase,
    ListPurchases,
    GetPurchase,
    Checkout,
    Subscription,
}

impl UserRoute {
    /// Routes that operate on a user's OWN product/group rows and are gated on
    /// `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` (matching the old
    /// `starts_with("/b/products/products"|"groups")` 403 fallback).
    fn requires_user_products(self) -> bool {
        matches!(
            self,
            UserRoute::ListProducts
                | UserRoute::GetProduct
                | UserRoute::CreateProduct
                | UserRoute::UpdateProduct
                | UserRoute::DeleteProduct
                | UserRoute::ListGroups
                | UserRoute::GetGroup
                | UserRoute::CreateGroup
                | UserRoute::UpdateGroup
                | UserRoute::DeleteGroup
                | UserRoute::GroupProducts
        )
    }
}

/// User dispatch table over the normalized `/b/products/...` paths. The
/// `groups/{id}/products` template precedes the generic `groups/{id}` so the
/// "products in a group" route wins (the old `ends_with("/products")` guard);
/// `catalog` precedes `catalog/{id}`, and `purchases` precedes
/// `purchases/{id}`.
const USER_ROUTES: &[EndpointRoute<UserRoute>] = &[
    // Own products
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/products",
        UserRoute::ListProducts,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/products",
        UserRoute::CreateProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/products/{id}",
        UserRoute::GetProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/b/products/products/{id}",
        UserRoute::UpdateProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/products/products/{id}",
        UserRoute::DeleteProduct,
    ),
    // Own groups (group-products before the generic {id})
    EndpointRoute::new(HttpMethod::Get, "/b/products/groups", UserRoute::ListGroups),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/groups",
        UserRoute::CreateGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/groups/{id}/products",
        UserRoute::GroupProducts,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/groups/{id}",
        UserRoute::GetGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/b/products/groups/{id}",
        UserRoute::UpdateGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/products/groups/{id}",
        UserRoute::DeleteGroup,
    ),
    // Read-only taxonomy
    EndpointRoute::new(HttpMethod::Get, "/b/products/types", UserRoute::ListTypes),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/group-templates",
        UserRoute::GroupTemplates,
    ),
    // Catalog (public)
    EndpointRoute::new(HttpMethod::Get, "/b/products/catalog", UserRoute::Catalog),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/catalog/{id}",
        UserRoute::CatalogItem,
    ),
    // Pricing / purchases / checkout
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/calculate-price",
        UserRoute::CalculatePrice,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/purchases",
        UserRoute::CreatePurchase,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/purchases",
        UserRoute::ListPurchases,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/purchases/{id}",
        UserRoute::GetPurchase,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/checkout",
        UserRoute::Checkout,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/subscription",
        UserRoute::Subscription,
    ),
];

/// Admin JSON-API dispatch.
///
/// `norm` is the normalized admin sub-path (`/admin/b/products/...`), passed
/// as an explicit argument by `ProductsBlock::handle` rather than written
/// back onto `req.resource` (the old in-band routing mutation). The matcher
/// binds `{id}` into `req.param.id` so the `crud::*` helpers read it.
pub async fn handle_admin(
    ctx: &dyn Context,
    msg: &mut Message,
    norm: &str,
    input: InputStream,
) -> OutputStream {
    let action = msg.action().to_string();
    let Some(route) = endpoint_match::dispatch_path(msg, &action, norm, ADMIN_ROUTES) else {
        return err_not_found("not found");
    };
    match route {
        AdminRoute::ListProducts => product::handle_list_products(ctx, msg).await,
        AdminRoute::GetProduct => product::handle_get_product(ctx, msg).await,
        AdminRoute::CreateProduct => product::handle_create_product(ctx, msg, input).await,
        AdminRoute::UpdateProduct => product::handle_update_product(ctx, msg, input).await,
        AdminRoute::DeleteProduct => product::handle_delete_product(ctx, msg).await,
        AdminRoute::ListGroups => group::handle_list_groups(ctx, msg).await,
        AdminRoute::CreateGroup => group::handle_create_group(ctx, msg, input).await,
        AdminRoute::UpdateGroup => group::handle_update_group(ctx, msg, input).await,
        AdminRoute::DeleteGroup => group::handle_delete_group(ctx, msg).await,
        AdminRoute::ListTypes => types::handle_list_types(ctx, msg).await,
        AdminRoute::CreateType => types::handle_create_type(ctx, msg, input).await,
        AdminRoute::DeleteType => types::handle_delete_type(ctx, msg).await,
        AdminRoute::ListPricing => pricing_template::handle_list_pricing(ctx, msg).await,
        AdminRoute::CreatePricing => pricing_template::handle_create_pricing(ctx, msg, input).await,
        AdminRoute::UpdatePricing => pricing_template::handle_update_pricing(ctx, msg, input).await,
        AdminRoute::DeletePricing => pricing_template::handle_delete_pricing(ctx, msg).await,
        AdminRoute::ListVariables => variables::handle_list(ctx, msg).await,
        AdminRoute::CreateVariable => variables::handle_create(ctx, msg, input).await,
        AdminRoute::UpdateVariable => variables::handle_update(ctx, msg, input).await,
        AdminRoute::DeleteVariable => variables::handle_delete(ctx, msg).await,
        AdminRoute::ListPurchases => purchase::handle_list_admin(ctx, msg).await,
        AdminRoute::RefundPurchase => purchase::handle_refund(ctx, msg, input).await,
        AdminRoute::GetPurchase => purchase::handle_get(ctx, msg).await,
        AdminRoute::Stats => stats::handle_stats(ctx, msg).await,
    }
}

/// User-facing dispatch (own products/groups under `ALLOW_USER_PRODUCTS`, plus
/// the public catalog, purchases, checkout, subscription).
///
/// `norm` is the normalized user sub-path passed explicitly by
/// `ProductsBlock::handle`. The own-products/groups routes are gated on
/// `ALLOW_USER_PRODUCTS` *after* matching, preserving the prior "feature
/// disabled → 403" behaviour for those paths while leaving catalog/purchase
/// routes always available.
pub async fn handle_user(
    ctx: &dyn Context,
    msg: &mut Message,
    norm: &str,
    input: InputStream,
) -> OutputStream {
    let action = msg.action().to_string();
    let Some(route) = endpoint_match::dispatch_path(msg, &action, norm, USER_ROUTES) else {
        return err_not_found("not found");
    };

    // Own products/groups require ALLOW_USER_PRODUCTS; reject with the same
    // 403 the old `(_, _) if starts_with("/b/products/products"|"groups")` arm
    // produced when the feature is off.
    if route.requires_user_products() && !user_products_enabled(ctx).await {
        return err_forbidden("user products are not enabled");
    }

    match route {
        UserRoute::ListProducts => product::handle_user_list_products(ctx, msg).await,
        UserRoute::GetProduct => product::handle_user_get_product(ctx, msg).await,
        UserRoute::CreateProduct => product::handle_user_create_product(ctx, msg, input).await,
        UserRoute::UpdateProduct => product::handle_user_update_product(ctx, msg, input).await,
        UserRoute::DeleteProduct => product::handle_user_delete_product(ctx, msg).await,
        UserRoute::ListGroups => group::handle_user_list_groups(ctx, msg).await,
        UserRoute::GetGroup => group::handle_user_get_group(ctx, msg).await,
        UserRoute::CreateGroup => group::handle_user_create_group(ctx, msg, input).await,
        UserRoute::UpdateGroup => group::handle_user_update_group(ctx, msg, input).await,
        UserRoute::DeleteGroup => group::handle_user_delete_group(ctx, msg).await,
        UserRoute::GroupProducts => group::handle_user_group_products(ctx, msg).await,
        UserRoute::ListTypes => types::handle_list_types(ctx, msg).await,
        UserRoute::GroupTemplates => group::handle_user_list_group_templates(ctx, msg).await,
        UserRoute::Catalog => catalog::handle_catalog(ctx, msg).await,
        UserRoute::CatalogItem => catalog::handle_get_product_public(ctx, msg).await,
        UserRoute::CalculatePrice => pricing::handle_calculate(ctx, input).await,
        UserRoute::CreatePurchase => purchase::handle_create(ctx, msg, input).await,
        UserRoute::ListPurchases => purchase::handle_list_user(ctx, msg).await,
        UserRoute::GetPurchase => purchase::handle_get(ctx, msg).await,
        UserRoute::Checkout => stripe::handle_checkout(ctx, msg, input).await,
        UserRoute::Subscription => subscription::handle_subscription(ctx, msg).await,
    }
}
