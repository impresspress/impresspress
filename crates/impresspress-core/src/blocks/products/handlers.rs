//! Admin- and user-facing HTTP handlers for the impresspress/products block.
//!
//! Dispatches under `/admin/b/products/...` (admin CRUD on products, groups,
//! types, pricing templates, purchases, stats) and `/b/products/...` (catalog,
//! user-owned products/groups when `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` is
//! enabled, calculate-price, purchases, checkout, subscription status).
//! Stripe webhook + checkout-session flows live in the sibling `stripe` module.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::{config, database as db};
use wafer_run::{context::Context, ErrorCode, HttpMethod, InputStream, Message, OutputStream};

use super::PRICING_TABLE;
use crate::{
    blocks::crud,
    endpoint_match::{self, EndpointRoute},
    http::{
        err_bad_request, err_forbidden, err_internal, err_not_found, err_unauthorized, ok_json,
    },
    util::{field_as_string, stamp_created, RecordExt},
};

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

/// Whether `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` is on — gates the
/// user-facing (non-admin) product/group CRUD routes above. `pub(super)`
/// so the admin Overview page (`pages::overview`) can render an accurate
/// notice instead of a silent empty catalog when it's off.
pub(super) async fn user_products_enabled(ctx: &dyn Context) -> bool {
    config::get_default(ctx, "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "false").await == "true"
}

/// Look up the id of the `name = "default"` template seeded by the Init
/// lifecycle. Used so client-omitted `*_template_id` fields default to a
/// real (UUIDv7) row instead of the literal integer `1` (which never
/// matches the seeded record and breaks any FK constraint).
async fn default_template_id(ctx: &dyn Context, table: &str) -> Option<String> {
    db::get_by_field(ctx, table, "name", serde_json::json!("default"))
        .await
        .ok()
        .map(|r| r.id)
}

/// Escape SQL LIKE wildcards (`%`, `_`) and the escape char (`\`) in user
/// input so a user searching for `100% off` doesn't also match arbitrary
/// characters.
fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

/// Build a `name LIKE %search%` filter with LIKE wildcards escaped.
/// Returns `None` for an empty search term.
pub(super) fn name_like_filter(search: &str) -> Option<Filter> {
    if search.is_empty() {
        return None;
    }
    Some(Filter {
        field: "name".to_string(),
        operator: FilterOp::Like,
        value: serde_json::Value::String(format!("%{}%", escape_like(search))),
    })
}

/// Build the shared product list filters from query params: `group_id` /
/// `status` equality plus an escaped `search` LIKE on `name`.
fn product_filters(msg: &Message) -> Vec<Filter> {
    let mut filters = Vec::new();
    let group_id = msg.query("group_id").to_string();
    if !group_id.is_empty() {
        filters.push(Filter {
            field: "group_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(group_id),
        });
    }
    let status = msg.query("status").to_string();
    if !status.is_empty() {
        filters.push(Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(status),
        });
    }
    if let Some(search) = name_like_filter(msg.query("search")) {
        filters.push(search);
    }
    filters
}

/// User-owned product rows: `/b/products/products/{id}`, owned via `created_by`.
const USER_PRODUCT: crud::OwnedResource<'static> = crud::OwnedResource {
    collection: PRODUCTS_TABLE,
    path_prefix: "/b/products/products/",
    owner_field: "created_by",
    label: "Product",
};

/// User-owned group rows: `/b/products/groups/{id}`, owned via `user_id`.
const USER_GROUP: crud::OwnedResource<'static> = crud::OwnedResource {
    collection: GROUPS_TABLE,
    path_prefix: "/b/products/groups/",
    owner_field: "user_id",
    label: "Group",
};

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
        AdminRoute::ListProducts => handle_list_products(ctx, msg).await,
        AdminRoute::GetProduct => handle_get_product(ctx, msg).await,
        AdminRoute::CreateProduct => handle_create_product(ctx, msg, input).await,
        AdminRoute::UpdateProduct => handle_update_product(ctx, msg, input).await,
        AdminRoute::DeleteProduct => handle_delete_product(ctx, msg).await,
        AdminRoute::ListGroups => handle_list_groups(ctx, msg).await,
        AdminRoute::CreateGroup => handle_create_group(ctx, msg, input).await,
        AdminRoute::UpdateGroup => handle_update_group(ctx, msg, input).await,
        AdminRoute::DeleteGroup => handle_delete_group(ctx, msg).await,
        AdminRoute::ListTypes => handle_list_types(ctx, msg).await,
        AdminRoute::CreateType => handle_create_type(ctx, msg, input).await,
        AdminRoute::DeleteType => handle_delete_type(ctx, msg).await,
        AdminRoute::ListPricing => handle_list_pricing(ctx, msg).await,
        AdminRoute::CreatePricing => handle_create_pricing(ctx, msg, input).await,
        AdminRoute::UpdatePricing => handle_update_pricing(ctx, msg, input).await,
        AdminRoute::DeletePricing => handle_delete_pricing(ctx, msg).await,
        AdminRoute::ListVariables => super::variables::handle_list(ctx, msg).await,
        AdminRoute::CreateVariable => super::variables::handle_create(ctx, msg, input).await,
        AdminRoute::UpdateVariable => super::variables::handle_update(ctx, msg, input).await,
        AdminRoute::DeleteVariable => super::variables::handle_delete(ctx, msg).await,
        AdminRoute::ListPurchases => super::purchase::handle_list_admin(ctx, msg).await,
        AdminRoute::RefundPurchase => super::purchase::handle_refund(ctx, msg, input).await,
        AdminRoute::GetPurchase => super::purchase::handle_get(ctx, msg).await,
        AdminRoute::Stats => handle_stats(ctx, msg).await,
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
        UserRoute::ListProducts => handle_user_list_products(ctx, msg).await,
        UserRoute::GetProduct => handle_user_get_product(ctx, msg).await,
        UserRoute::CreateProduct => handle_user_create_product(ctx, msg, input).await,
        UserRoute::UpdateProduct => handle_user_update_product(ctx, msg, input).await,
        UserRoute::DeleteProduct => handle_user_delete_product(ctx, msg).await,
        UserRoute::ListGroups => handle_user_list_groups(ctx, msg).await,
        UserRoute::GetGroup => handle_user_get_group(ctx, msg).await,
        UserRoute::CreateGroup => handle_user_create_group(ctx, msg, input).await,
        UserRoute::UpdateGroup => handle_user_update_group(ctx, msg, input).await,
        UserRoute::DeleteGroup => handle_user_delete_group(ctx, msg).await,
        UserRoute::GroupProducts => handle_user_group_products(ctx, msg).await,
        UserRoute::ListTypes => handle_list_types(ctx, msg).await,
        UserRoute::GroupTemplates => handle_user_list_group_templates(ctx, msg).await,
        UserRoute::Catalog => handle_catalog(ctx, msg).await,
        UserRoute::CatalogItem => handle_get_product_public(ctx, msg).await,
        UserRoute::CalculatePrice => super::pricing::handle_calculate(ctx, input).await,
        UserRoute::CreatePurchase => super::purchase::handle_create(ctx, msg, input).await,
        UserRoute::ListPurchases => super::purchase::handle_list_user(ctx, msg).await,
        UserRoute::GetPurchase => super::purchase::handle_get(ctx, msg).await,
        UserRoute::Checkout => super::stripe::handle_checkout(ctx, msg, input).await,
        UserRoute::Subscription => handle_subscription(ctx, msg).await,
    }
}

// --- Product CRUD ---

async fn handle_list_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_list(ctx, msg, PRODUCTS_TABLE, product_filters(msg), None).await
}

async fn handle_get_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_get(
        ctx,
        msg,
        PRODUCTS_TABLE,
        "/admin/b/products/products/",
        "Product",
    )
    .await
}

async fn handle_create_product(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let mut defaults = HashMap::new();
    defaults.insert(
        "status".to_string(),
        serde_json::Value::String("draft".to_string()),
    );
    defaults.insert(
        "created_by".to_string(),
        serde_json::Value::String(msg.user_id().to_string()),
    );
    crud::crud_create(ctx, msg, input, PRODUCTS_TABLE, defaults).await
}

async fn handle_update_product(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    crud::crud_update(
        ctx,
        msg,
        input,
        PRODUCTS_TABLE,
        "/admin/b/products/products/",
        "Product",
    )
    .await
}

async fn handle_delete_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete(
        ctx,
        msg,
        PRODUCTS_TABLE,
        "/admin/b/products/products/",
        "Product",
    )
    .await
}

// --- Groups ---

async fn handle_list_groups(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_list(ctx, msg, GROUPS_TABLE, vec![], None).await
}

async fn handle_create_group(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let mut defaults = HashMap::new();
    defaults.insert(
        "user_id".to_string(),
        serde_json::Value::String(msg.user_id().to_string()),
    );
    crud::crud_create(ctx, msg, input, GROUPS_TABLE, defaults).await
}

async fn handle_update_group(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    crud::crud_update(
        ctx,
        msg,
        input,
        GROUPS_TABLE,
        "/admin/b/products/groups/",
        "Group",
    )
    .await
}

async fn handle_delete_group(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete(ctx, msg, GROUPS_TABLE, "/admin/b/products/groups/", "Group").await
}

// --- Types ---

async fn handle_list_types(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_list(ctx, msg, TYPES_TABLE, vec![], None).await
}

async fn handle_create_type(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    crud::crud_create(ctx, msg, input, TYPES_TABLE, HashMap::new()).await
}

async fn handle_delete_type(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete(ctx, msg, TYPES_TABLE, "/admin/b/products/types/", "Type").await
}

// --- Pricing Templates ---

async fn handle_list_pricing(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_list(ctx, msg, PRICING_TABLE, vec![], None).await
}

async fn handle_create_pricing(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    crud::crud_create(ctx, msg, input, PRICING_TABLE, HashMap::new()).await
}

async fn handle_update_pricing(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    crud::crud_update(
        ctx,
        msg,
        input,
        PRICING_TABLE,
        "/admin/b/products/pricing/",
        "Pricing template",
    )
    .await
}

async fn handle_delete_pricing(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete(
        ctx,
        msg,
        PRICING_TABLE,
        "/admin/b/products/pricing/",
        "Pricing template",
    )
    .await
}

// --- Public catalog ---

async fn handle_catalog(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let filters = vec![Filter {
        field: "status".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String("active".to_string()),
    }];
    let sort = vec![SortField {
        field: "name".to_string(),
        desc: false,
    }];
    crud::crud_list(ctx, msg, PRODUCTS_TABLE, filters, Some(sort)).await
}

async fn handle_get_product_public(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = {
        let var = msg.var("id");
        if var.is_empty() {
            msg.path()
                .strip_prefix("/b/products/catalog/")
                .unwrap_or("")
        } else {
            var
        }
    };
    if id.is_empty() {
        return err_bad_request("Missing product ID");
    }

    match db::get(ctx, PRODUCTS_TABLE, id).await {
        Ok(record) => {
            let status = record.str_field("status");
            if status != "active" {
                return err_not_found("Product not found");
            }
            ok_json(&record)
        }
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Product not found"),
        Err(e) => err_internal("Database error", e),
    }
}

// --- User's own products ---

async fn handle_user_list_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthorized("Not authenticated");
    }

    let mut filters = vec![Filter {
        field: "created_by".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id),
    }];
    filters.extend(product_filters(msg));

    crud::crud_list(ctx, msg, PRODUCTS_TABLE, filters, None).await
}

async fn handle_user_get_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_get_owned(ctx, msg, &USER_PRODUCT).await
}

async fn handle_user_create_product(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthorized("Not authenticated");
    }

    let raw = input.collect_to_bytes().await;
    let mut data: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Verify user owns the group (if provided)
    let group_id_str = data
        .get("group_id")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .or_else(|| {
            data.get("group_id")
                .and_then(|v| v.as_i64().map(|n| n.to_string()))
        })
        .unwrap_or_default();
    if !group_id_str.is_empty() {
        match db::get(ctx, GROUPS_TABLE, &group_id_str).await {
            Ok(group) => {
                if field_as_string(&group, "user_id") != user_id {
                    return err_bad_request("You don't own this group");
                }
            }
            Err(_) => return err_bad_request("Group not found"),
        }
    }

    data.entry("status".to_string())
        .or_insert(serde_json::Value::String("draft".to_string()));
    stamp_created(&mut data);
    data.insert("created_by".to_string(), serde_json::Value::String(user_id));
    // Default product_template_id to the seeded "default" template's real
    // (UUIDv7) id if the caller didn't specify one. The previous fallback
    // to the literal integer `1` would never match a seeded record (ids
    // are UUIDs, not integers).
    if !data.contains_key("product_template_id")
        || data
            .get("product_template_id")
            .is_some_and(|v| v.is_null() || v.as_str().is_some_and(|s| s.is_empty()))
    {
        if let Some(default_id) = default_template_id(ctx, PRODUCT_TEMPLATES_TABLE).await {
            data.insert(
                "product_template_id".to_string(),
                serde_json::Value::String(default_id),
            );
        }
    }

    match db::create(ctx, PRODUCTS_TABLE, data).await {
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_user_update_product(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    // Strip created_by to prevent ownership change.
    crud::crud_update_owned(ctx, msg, input, &USER_PRODUCT, &["created_by"]).await
}

async fn handle_user_delete_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete_owned(ctx, msg, &USER_PRODUCT).await
}

// --- User's own groups ---

async fn handle_user_list_groups(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthorized("Not authenticated");
    }

    let opts = ListOptions {
        filters: vec![Filter {
            field: "user_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id),
        }],
        sort: vec![SortField {
            field: "name".to_string(),
            desc: false,
        }],
        limit: 1000,
        ..Default::default()
    };
    match db::list(ctx, GROUPS_TABLE, &opts).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_user_get_group(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_get_owned(ctx, msg, &USER_GROUP).await
}

async fn handle_user_create_group(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthorized("Not authenticated");
    }

    let raw = input.collect_to_bytes().await;
    let mut body: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    stamp_created(&mut body);
    body.insert("user_id".to_string(), serde_json::Value::String(user_id));
    // Default group_template_id to the seeded "default" template's real
    // (UUIDv7) id — same reasoning as for product_template_id above.
    if !body.contains_key("group_template_id")
        || body
            .get("group_template_id")
            .is_some_and(|v| v.is_null() || v.as_str().is_some_and(|s| s.is_empty()))
    {
        if let Some(default_id) = default_template_id(ctx, GROUP_TEMPLATES_TABLE).await {
            body.insert(
                "group_template_id".to_string(),
                serde_json::Value::String(default_id),
            );
        }
    }

    match db::create(ctx, GROUPS_TABLE, body).await {
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_user_update_group(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    // Strip user_id to prevent ownership change.
    crud::crud_update_owned(ctx, msg, input, &USER_GROUP, &["user_id"]).await
}

async fn handle_user_delete_group(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete_owned(ctx, msg, &USER_GROUP).await
}

// Products in a user's group
async fn handle_user_group_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    // Path: /b/products/groups/{id}/products — prefer the matcher-bound `{id}`.
    let group_id = {
        let var = msg.var("id");
        if var.is_empty() {
            msg.path()
                .strip_prefix("/b/products/groups/")
                .unwrap_or("")
                .strip_suffix("/products")
                .unwrap_or("")
        } else {
            var
        }
    };
    if group_id.is_empty() {
        return err_bad_request("Missing group ID");
    }

    if let Err(resp) = crud::verify_owner(
        ctx,
        GROUPS_TABLE,
        group_id,
        "user_id",
        msg.user_id(),
        "Group",
    )
    .await
    {
        return resp;
    }

    let filters = vec![Filter {
        field: "group_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(group_id.to_string()),
    }];
    crud::crud_list(ctx, msg, PRODUCTS_TABLE, filters, None).await
}

// User-accessible group templates (read-only)
async fn handle_user_list_group_templates(ctx: &dyn Context, _msg: &Message) -> OutputStream {
    match db::list_all(ctx, GROUP_TEMPLATES_TABLE, vec![]).await {
        Ok(records) => ok_json(&records),
        Err(e) => err_internal("Database error", e),
    }
}

// --- Subscription status ---

async fn handle_subscription(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthorized("Not authenticated");
    }
    // A real repository failure must surface as an error, not be reported to
    // the caller as `{"subscription": null}` — indistinguishable from "you
    // have no subscription" and potentially misread as a cancellation.
    let sub = match super::repo::subscriptions::subscription_for_user(ctx, &user_id).await {
        Ok(sub) => sub,
        Err(e) => return err_internal("Database error", e),
    };
    ok_json(&serde_json::json!({"subscription": sub}))
}

// --- Stats ---

async fn handle_stats(ctx: &dyn Context, _msg: &Message) -> OutputStream {
    let active_filter = [Filter {
        field: "status".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String("active".to_string()),
    }];

    // Fan out the 5 independent counts/sums concurrently rather than
    // serializing 5 round-trips on the request path. `futures::join!`
    // (not `tokio::join!`) because tokio is an optional dep in
    // impresspress-core's Cargo.toml — futures 0.3 is unconditional.
    let (total_products, active_products, total_purchases, total_revenue, total_groups) = futures::join!(
        db::count(ctx, PRODUCTS_TABLE, &[]),
        db::count(ctx, PRODUCTS_TABLE, &active_filter),
        super::repo::purchases::count_all(ctx),
        super::repo::purchases::sum_completed_cents(ctx),
        db::count(ctx, GROUPS_TABLE, &[]),
    );

    // A repository failure on any of these must surface as an error, not be
    // fabricated into a "0" stat — an admin reading "0 products / $0 revenue"
    // during a genuine outage would read that as real business data rather
    // than a broken dashboard. `unwrap_or(0)` used to do exactly that for
    // every one of the 5 counts/sums independently.
    let total_products = match total_products {
        Ok(n) => n,
        Err(e) => return err_internal("Database error", e),
    };
    let active_products = match active_products {
        Ok(n) => n,
        Err(e) => return err_internal("Database error", e),
    };
    let total_purchases = match total_purchases {
        Ok(n) => n,
        Err(e) => return err_internal("Database error", e),
    };
    let total_revenue = match total_revenue {
        Ok(n) => n,
        Err(e) => return err_internal("Database error", e),
    };
    let total_groups = match total_groups {
        Ok(n) => n,
        Err(e) => return err_internal("Database error", e),
    };

    ok_json(&serde_json::json!({
        "total_products": total_products,
        "active_products": active_products,
        "total_purchases": total_purchases,
        "total_revenue": total_revenue,
        "total_groups": total_groups
    }))
}
