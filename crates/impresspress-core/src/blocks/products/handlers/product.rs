//! Product CRUD: admin (`/admin/b/products/products`) and user-owned
//! (`/b/products/products`, gated on `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS`).

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{default_template_id, GROUPS_TABLE, PRODUCTS_TABLE, PRODUCT_TEMPLATES_TABLE};
use crate::{
    blocks::crud,
    http::{err_bad_request, err_internal, err_unauthorized, ok_json},
    util::{field_as_string, stamp_created},
};

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
///
/// Also called from `pages::manage_products` (admin search box), hence the
/// wider-than-`handlers` visibility.
pub(in crate::blocks::products) fn name_like_filter(search: &str) -> Option<Filter> {
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

// --- Product CRUD (admin) ---

pub(super) async fn handle_list_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_list(ctx, msg, PRODUCTS_TABLE, product_filters(msg), None).await
}

pub(super) async fn handle_get_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_get(
        ctx,
        msg,
        PRODUCTS_TABLE,
        "/admin/b/products/products/",
        "Product",
    )
    .await
}

pub(super) async fn handle_create_product(
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

pub(super) async fn handle_update_product(
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

pub(super) async fn handle_delete_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete(
        ctx,
        msg,
        PRODUCTS_TABLE,
        "/admin/b/products/products/",
        "Product",
    )
    .await
}

// --- User's own products ---

pub(super) async fn handle_user_list_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
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

pub(super) async fn handle_user_get_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_get_owned(ctx, msg, &USER_PRODUCT).await
}

pub(super) async fn handle_user_create_product(
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

pub(super) async fn handle_user_update_product(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    // Strip created_by to prevent ownership change.
    crud::crud_update_owned(ctx, msg, input, &USER_PRODUCT, &["created_by"]).await
}

pub(super) async fn handle_user_delete_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete_owned(ctx, msg, &USER_PRODUCT).await
}
