//! Admin- and user-facing HTTP handlers for the suppers-ai/products block.
//!
//! Dispatches under `/admin/b/products/...` (admin CRUD on products, groups,
//! types, pricing templates, purchases, stats) and `/b/products/...` (catalog,
//! user-owned products/groups when `SOLOBASE_SHARED__ALLOW_USER_PRODUCTS` is
//! enabled, calculate-price, purchases, checkout, subscription status).
//! Stripe webhook + checkout-session flows live in the sibling `stripe` module.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::{config, database as db};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::PRICING_TABLE;
use crate::blocks::{
    crud,
    helpers::{
        err_bad_request, err_forbidden, err_internal, err_not_found, err_unauthorized,
        field_as_string, ok_json, RecordExt,
    },
};

/// Products catalog table — one row per product offering.
pub(crate) const PRODUCTS_TABLE: &str = "suppers_ai__products__products";

/// Product groups (categories / bundles) table.
pub(crate) const GROUPS_TABLE: &str = "suppers_ai__products__groups";

/// Product types (taxonomy) table.
pub(crate) const TYPES_TABLE: &str = "suppers_ai__products__types";

/// Reusable group template definitions (admin-authored).
pub(crate) const GROUP_TEMPLATES_TABLE: &str = "suppers_ai__products__group_templates";

/// Reusable product template definitions (admin-authored).
pub(crate) const PRODUCT_TEMPLATES_TABLE: &str = "suppers_ai__products__product_templates";

async fn user_products_enabled(ctx: &dyn Context) -> bool {
    config::get_default(ctx, "SOLOBASE_SHARED__ALLOW_USER_PRODUCTS", "false").await == "true"
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

pub async fn handle_admin(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let action = msg.action();
    let path = msg.path();

    match (action, path) {
        // Products
        ("retrieve", "/admin/b/products/products") => handle_list_products(ctx, msg).await,
        ("retrieve", _) if path.starts_with("/admin/b/products/products/") => {
            handle_get_product(ctx, msg).await
        }
        ("create", "/admin/b/products/products") => handle_create_product(ctx, msg, input).await,
        ("update", _) if path.starts_with("/admin/b/products/products/") => {
            handle_update_product(ctx, msg, input).await
        }
        ("delete", _) if path.starts_with("/admin/b/products/products/") => {
            handle_delete_product(ctx, msg).await
        }
        // Groups
        ("retrieve", "/admin/b/products/groups") => handle_list_groups(ctx, msg).await,
        ("create", "/admin/b/products/groups") => handle_create_group(ctx, msg, input).await,
        ("update", _) if path.starts_with("/admin/b/products/groups/") => {
            handle_update_group(ctx, msg, input).await
        }
        ("delete", _) if path.starts_with("/admin/b/products/groups/") => {
            handle_delete_group(ctx, msg).await
        }
        // Types
        ("retrieve", "/admin/b/products/types") => handle_list_types(ctx, msg).await,
        ("create", "/admin/b/products/types") => handle_create_type(ctx, msg, input).await,
        ("delete", _) if path.starts_with("/admin/b/products/types/") => {
            handle_delete_type(ctx, msg).await
        }
        // Pricing templates
        ("retrieve", "/admin/b/products/pricing") => handle_list_pricing(ctx, msg).await,
        ("create", "/admin/b/products/pricing") => handle_create_pricing(ctx, msg, input).await,
        ("update", _) if path.starts_with("/admin/b/products/pricing/") => {
            handle_update_pricing(ctx, msg, input).await
        }
        ("delete", _) if path.starts_with("/admin/b/products/pricing/") => {
            handle_delete_pricing(ctx, msg).await
        }
        // Variables
        ("retrieve", "/admin/b/products/variables") => {
            super::variables::handle_list(ctx, msg).await
        }
        ("create", "/admin/b/products/variables") => {
            super::variables::handle_create(ctx, input).await
        }
        ("update", _) if path.starts_with("/admin/b/products/variables/") => {
            super::variables::handle_update(ctx, msg, input).await
        }
        ("delete", _) if path.starts_with("/admin/b/products/variables/") => {
            super::variables::handle_delete(ctx, msg).await
        }
        // Purchases (admin view)
        ("retrieve", "/admin/b/products/purchases") => {
            super::purchase::handle_list_admin(ctx, msg).await
        }
        ("retrieve", _) if path.starts_with("/admin/b/products/purchases/") => {
            super::purchase::handle_get(ctx, msg).await
        }
        ("update", _)
            if path.starts_with("/admin/b/products/purchases/") && path.ends_with("/refund") =>
        {
            super::purchase::handle_refund(ctx, msg, input).await
        }
        // Stats
        ("retrieve", "/admin/b/products/stats") => handle_stats(ctx, msg).await,
        _ => err_not_found("not found"),
    }
}

pub async fn handle_user(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let action = msg.action();
    let path = msg.path();
    let user_products = user_products_enabled(ctx).await;

    match (action, path) {
        // User's own products (requires ALLOW_USER_PRODUCTS)
        ("retrieve", "/b/products/products") if user_products => {
            handle_user_list_products(ctx, msg).await
        }
        ("retrieve", _) if user_products && path.starts_with("/b/products/products/") => {
            handle_user_get_product(ctx, msg).await
        }
        ("create", "/b/products/products") if user_products => {
            handle_user_create_product(ctx, msg, input).await
        }
        ("update", _) if user_products && path.starts_with("/b/products/products/") => {
            handle_user_update_product(ctx, msg, input).await
        }
        ("delete", _) if user_products && path.starts_with("/b/products/products/") => {
            handle_user_delete_product(ctx, msg).await
        }
        // User's own groups (requires ALLOW_USER_PRODUCTS)
        ("retrieve", "/b/products/groups") if user_products => {
            handle_user_list_groups(ctx, msg).await
        }
        ("retrieve", _)
            if user_products
                && path.starts_with("/b/products/groups/")
                && !path.ends_with("/products") =>
        {
            handle_user_get_group(ctx, msg).await
        }
        ("create", "/b/products/groups") if user_products => {
            handle_user_create_group(ctx, msg, input).await
        }
        ("update", _)
            if user_products
                && path.starts_with("/b/products/groups/")
                && !path.ends_with("/products") =>
        {
            handle_user_update_group(ctx, msg, input).await
        }
        ("delete", _)
            if user_products
                && path.starts_with("/b/products/groups/")
                && !path.ends_with("/products") =>
        {
            handle_user_delete_group(ctx, msg).await
        }
        // Products in a group (requires ALLOW_USER_PRODUCTS)
        ("retrieve", _)
            if user_products
                && path.starts_with("/b/products/groups/")
                && path.ends_with("/products") =>
        {
            handle_user_group_products(ctx, msg).await
        }
        // Read-only: types and group templates
        ("retrieve", "/b/products/types") => handle_list_types(ctx, msg).await,
        ("retrieve", "/b/products/group-templates") => {
            handle_user_list_group_templates(ctx, msg).await
        }
        // Catalog (public)
        ("retrieve", "/b/products/catalog") => handle_catalog(ctx, msg).await,
        ("retrieve", _) if path.starts_with("/b/products/catalog/") => {
            handle_get_product_public(ctx, msg).await
        }
        // Pricing, purchases, checkout
        ("create", "/b/products/calculate-price") => {
            super::pricing::handle_calculate(ctx, input).await
        }
        ("create", "/b/products/purchases") => {
            super::purchase::handle_create(ctx, msg, input).await
        }
        ("retrieve", "/b/products/purchases") => super::purchase::handle_list_user(ctx, msg).await,
        ("retrieve", _) if path.starts_with("/b/products/purchases/") => {
            super::purchase::handle_get(ctx, msg).await
        }
        ("create", "/b/products/checkout") => super::stripe::handle_checkout(ctx, msg, input).await,
        // Subscription status
        ("retrieve", "/b/products/subscription") => handle_subscription(ctx, msg).await,
        // User products/groups disabled
        (_, _)
            if path.starts_with("/b/products/products")
                || path.starts_with("/b/products/groups") =>
        {
            err_forbidden("user products are not enabled")
        }
        _ => err_not_found("not found"),
    }
}

// --- Product CRUD ---

async fn handle_list_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
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
    let search = msg.query("search").to_string();
    if !search.is_empty() {
        filters.push(Filter {
            field: "name".to_string(),
            operator: FilterOp::Like,
            value: serde_json::Value::String(format!("%{}%", escape_like(&search))),
        });
    }

    crud::crud_list(ctx, msg, PRODUCTS_TABLE, filters, None).await
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
    let (page, page_size, _) = msg.pagination_params(20);
    let filters = vec![Filter {
        field: "status".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String("active".to_string()),
    }];
    let sort = vec![SortField {
        field: "name".to_string(),
        desc: false,
    }];
    match db::paginated_list(
        ctx,
        PRODUCTS_TABLE,
        page as i64,
        page_size as i64,
        filters,
        sort,
    )
    .await
    {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_get_product_public(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let path = msg.path();
    let id = path.strip_prefix("/b/products/catalog/").unwrap_or("");
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

    let (page, page_size, _) = msg.pagination_params(20);
    let mut filters = vec![Filter {
        field: "created_by".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id),
    }];
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
    let search = msg.query("search").to_string();
    if !search.is_empty() {
        filters.push(Filter {
            field: "name".to_string(),
            operator: FilterOp::Like,
            value: serde_json::Value::String(format!("%{}%", escape_like(&search))),
        });
    }

    let sort = vec![SortField {
        field: "created_at".to_string(),
        desc: true,
    }];
    match db::paginated_list(
        ctx,
        PRODUCTS_TABLE,
        page as i64,
        page_size as i64,
        filters,
        sort,
    )
    .await
    {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_user_get_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let path = msg.path();
    let id = path.strip_prefix("/b/products/products/").unwrap_or("");
    if id.is_empty() {
        return err_bad_request("Missing product ID");
    }

    match db::get(ctx, PRODUCTS_TABLE, id).await {
        Ok(record) => {
            if field_as_string(&record, "created_by") != user_id {
                return err_not_found("Product not found");
            }
            ok_json(&record)
        }
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Product not found"),
        Err(e) => err_internal("Database error", e),
    }
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

    let now = chrono::Utc::now().to_rfc3339();
    data.entry("status".to_string())
        .or_insert(serde_json::Value::String("draft".to_string()));
    data.insert(
        "created_at".to_string(),
        serde_json::Value::String(now.clone()),
    );
    data.insert("updated_at".to_string(), serde_json::Value::String(now));
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
    let user_id = msg.user_id().to_string();
    let path = msg.path();
    let id = path
        .strip_prefix("/b/products/products/")
        .unwrap_or("")
        .to_string();
    if id.is_empty() {
        return err_bad_request("Missing product ID");
    }

    // Verify ownership
    match db::get(ctx, PRODUCTS_TABLE, &id).await {
        Ok(record) => {
            if field_as_string(&record, "created_by") != user_id {
                return err_not_found("Product not found");
            }
        }
        Err(e) if e.code == ErrorCode::NotFound => return err_not_found("Product not found"),
        Err(e) => return err_internal("Database error", e),
    }

    let raw = input.collect_to_bytes().await;
    let mut body: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    body.remove("created_by"); // prevent ownership change
    body.insert(
        "updated_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );

    match db::update(ctx, PRODUCTS_TABLE, &id, body).await {
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_user_delete_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let path = msg.path();
    let id = path
        .strip_prefix("/b/products/products/")
        .unwrap_or("")
        .to_string();
    if id.is_empty() {
        return err_bad_request("Missing product ID");
    }

    // Verify ownership
    match db::get(ctx, PRODUCTS_TABLE, &id).await {
        Ok(record) => {
            if field_as_string(&record, "created_by") != user_id {
                return err_not_found("Product not found");
            }
        }
        Err(e) if e.code == ErrorCode::NotFound => return err_not_found("Product not found"),
        Err(e) => return err_internal("Database error", e),
    }

    match db::delete(ctx, PRODUCTS_TABLE, &id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) => err_internal("Database error", e),
    }
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
    let user_id = msg.user_id().to_string();
    let path = msg.path();
    let id = path.strip_prefix("/b/products/groups/").unwrap_or("");
    if id.is_empty() {
        return err_bad_request("Missing group ID");
    }

    match db::get(ctx, GROUPS_TABLE, id).await {
        Ok(record) => {
            if field_as_string(&record, "user_id") != user_id {
                return err_not_found("Group not found");
            }
            ok_json(&record)
        }
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Group not found"),
        Err(e) => err_internal("Database error", e),
    }
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
    body.insert(
        "created_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
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
    let user_id = msg.user_id().to_string();
    let path = msg.path();
    let id = path
        .strip_prefix("/b/products/groups/")
        .unwrap_or("")
        .to_string();
    if id.is_empty() {
        return err_bad_request("Missing group ID");
    }

    // Verify ownership
    match db::get(ctx, GROUPS_TABLE, &id).await {
        Ok(record) => {
            if field_as_string(&record, "user_id") != user_id {
                return err_not_found("Group not found");
            }
        }
        Err(e) if e.code == ErrorCode::NotFound => return err_not_found("Group not found"),
        Err(e) => return err_internal("Database error", e),
    }

    let raw = input.collect_to_bytes().await;
    let mut body: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    body.remove("user_id"); // prevent ownership change

    match db::update(ctx, GROUPS_TABLE, &id, body).await {
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_user_delete_group(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let path = msg.path();
    let id = path
        .strip_prefix("/b/products/groups/")
        .unwrap_or("")
        .to_string();
    if id.is_empty() {
        return err_bad_request("Missing group ID");
    }

    // Verify ownership
    match db::get(ctx, GROUPS_TABLE, &id).await {
        Ok(record) => {
            if field_as_string(&record, "user_id") != user_id {
                return err_not_found("Group not found");
            }
        }
        Err(e) if e.code == ErrorCode::NotFound => return err_not_found("Group not found"),
        Err(e) => return err_internal("Database error", e),
    }

    match db::delete(ctx, GROUPS_TABLE, &id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) => err_internal("Database error", e),
    }
}

// Products in a user's group
async fn handle_user_group_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let path = msg.path();
    // Path: /b/products/groups/{id}/products
    let rest = path.strip_prefix("/b/products/groups/").unwrap_or("");
    let group_id = rest.strip_suffix("/products").unwrap_or("");
    if group_id.is_empty() {
        return err_bad_request("Missing group ID");
    }

    // Verify group ownership
    match db::get(ctx, GROUPS_TABLE, group_id).await {
        Ok(record) => {
            if field_as_string(&record, "user_id") != user_id {
                return err_not_found("Group not found");
            }
        }
        Err(_) => return err_not_found("Group not found"),
    }

    let (page, page_size, _) = msg.pagination_params(20);
    let filters = vec![Filter {
        field: "group_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(group_id.to_string()),
    }];
    let sort = vec![SortField {
        field: "created_at".to_string(),
        desc: true,
    }];
    match db::paginated_list(
        ctx,
        PRODUCTS_TABLE,
        page as i64,
        page_size as i64,
        filters,
        sort,
    )
    .await
    {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
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
    let sub = super::repo::subscriptions::subscription_for_user(ctx, &user_id).await;
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
    // solobase-core's Cargo.toml — futures 0.3 is unconditional.
    let (total_products, active_products, total_purchases, total_revenue, total_groups) = futures::join!(
        db::count(ctx, PRODUCTS_TABLE, &[]),
        db::count(ctx, PRODUCTS_TABLE, &active_filter),
        super::repo::purchases::count_all(ctx),
        super::repo::purchases::sum_completed_cents(ctx),
        db::count(ctx, GROUPS_TABLE, &[]),
    );

    ok_json(&serde_json::json!({
        "total_products": total_products.unwrap_or(0),
        "active_products": active_products.unwrap_or(0),
        "total_purchases": total_purchases.unwrap_or(0),
        "total_revenue": total_revenue.unwrap_or(0.0),
        "total_groups": total_groups.unwrap_or(0)
    }))
}
