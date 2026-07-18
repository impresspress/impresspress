//! Group CRUD: admin (`/admin/b/products/groups`) and user-owned
//! (`/b/products/groups`, gated on `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS`),
//! plus the "products in a user's group" listing and the read-only
//! group-templates listing.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{default_template_id, GROUPS_TABLE, GROUP_TEMPLATES_TABLE, PRODUCTS_TABLE};
use crate::{
    blocks::crud,
    http::{err_bad_request, err_internal, err_unauthorized, ok_json},
    util::stamp_created,
};

/// User-owned group rows: `/b/products/groups/{id}`, owned via `user_id`.
const USER_GROUP: crud::OwnedResource<'static> = crud::OwnedResource {
    collection: GROUPS_TABLE,
    path_prefix: "/b/products/groups/",
    owner_field: "user_id",
    label: "Group",
};

// --- Groups (admin) ---

pub(super) async fn handle_list_groups(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_list(ctx, msg, GROUPS_TABLE, vec![], None).await
}

pub(super) async fn handle_create_group(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let mut defaults = HashMap::new();
    defaults.insert(
        "user_id".to_string(),
        serde_json::Value::String(msg.user_id().to_string()),
    );
    crud::crud_create(ctx, msg, input, GROUPS_TABLE, defaults).await
}

pub(super) async fn handle_update_group(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
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

pub(super) async fn handle_delete_group(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete(ctx, msg, GROUPS_TABLE, "/admin/b/products/groups/", "Group").await
}

// --- User's own groups ---

pub(super) async fn handle_user_list_groups(ctx: &dyn Context, msg: &Message) -> OutputStream {
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

pub(super) async fn handle_user_get_group(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_get_owned(ctx, msg, &USER_GROUP).await
}

pub(super) async fn handle_user_create_group(
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

pub(super) async fn handle_user_update_group(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    // Strip user_id to prevent ownership change.
    crud::crud_update_owned(ctx, msg, input, &USER_GROUP, &["user_id"]).await
}

pub(super) async fn handle_user_delete_group(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete_owned(ctx, msg, &USER_GROUP).await
}

// Products in a user's group
pub(super) async fn handle_user_group_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
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
pub(super) async fn handle_user_list_group_templates(
    ctx: &dyn Context,
    _msg: &Message,
) -> OutputStream {
    match db::list_all(ctx, GROUP_TEMPLATES_TABLE, vec![]).await {
        Ok(records) => ok_json(&records),
        Err(e) => err_internal("Database error", e),
    }
}
