use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::logs::audit_log;
use crate::{
    http::{err_bad_request, err_conflict, err_forbidden, err_internal, err_not_found, ok_json},
    util::{json_map, RecordExt},
};

/// Role definitions table (one row per named role).
pub(crate) const ROLES_TABLE: &str = "impresspress__admin__roles";

/// Per-role permission rows (resource + actions tuples).
pub(crate) const PERMISSIONS_TABLE: &str = "impresspress__admin__permissions";

/// User → role assignment table (many-to-many via row per pair).
pub(crate) const USER_ROLES_TABLE: &str = "impresspress__admin__user_roles";

/// `path` is the normalized `/admin/iam/...` sub-path, passed explicitly (no
/// `req.resource` rewrite). Id-bearing leaves take their id from it.
pub async fn handle(
    ctx: &dyn Context,
    msg: &Message,
    path: &str,
    input: InputStream,
) -> OutputStream {
    let action = msg.action();

    match (action, path) {
        // Roles
        ("retrieve", "/admin/iam/roles") => handle_list_roles(ctx).await,
        ("create", "/admin/iam/roles") => handle_create_role(ctx, msg, input).await,
        ("update", _) if path.starts_with("/admin/iam/roles/") => {
            handle_update_role(ctx, path, input).await
        }
        ("delete", _) if path.starts_with("/admin/iam/roles/") => {
            handle_delete_role(ctx, msg, path).await
        }
        // Permissions
        ("retrieve", "/admin/iam/permissions") => handle_list_permissions(ctx).await,
        ("create", "/admin/iam/permissions") => handle_create_permission(ctx, input).await,
        ("delete", _) if path.starts_with("/admin/iam/permissions/") => {
            handle_delete_permission(ctx, path).await
        }
        // User-role assignments
        ("retrieve", "/admin/iam/user-roles") => handle_list_user_roles(ctx, msg).await,
        ("create", "/admin/iam/user-roles") => handle_assign_role(ctx, msg, input).await,
        ("delete", _) if path.starts_with("/admin/iam/user-roles/") => {
            handle_remove_role(ctx, msg, path).await
        }
        _ => err_not_found("not found"),
    }
}

async fn handle_list_roles(ctx: &dyn Context) -> OutputStream {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "name".to_string(),
            desc: false,
        }],
        limit: 1000,
        ..Default::default()
    };
    match db::list(ctx, ROLES_TABLE, &opts).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_create_role(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        name: String,
        description: Option<String>,
        permissions: Option<Vec<String>>,
    }
    let raw = input.collect_to_bytes().await;
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    // Validation, audit-log write, and the create live in the shared ops layer.
    match super::ops::create_role(
        ctx,
        msg,
        &body.name,
        body.description.as_deref(),
        body.permissions,
    )
    .await
    {
        Ok(record) => ok_json(&record),
        Err(out) => out,
    }
}

async fn handle_update_role(ctx: &dyn Context, path: &str, input: InputStream) -> OutputStream {
    let id = path.strip_prefix("/admin/iam/roles/").unwrap_or("");
    if id.is_empty() {
        return err_bad_request("Missing role ID");
    }

    let raw = input.collect_to_bytes().await;
    let body_peek: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Protect system roles from name changes (renaming "admin" would break auth)
    if let Ok(existing) = db::get(ctx, ROLES_TABLE, id).await {
        if existing.bool_field("is_system") {
            if body_peek.contains_key("name") {
                return err_forbidden("Cannot rename system roles");
            }
            let mut data = HashMap::new();
            for key in &["description", "permissions"] {
                if let Some(val) = body_peek.get(*key) {
                    data.insert(key.to_string(), val.clone());
                }
            }
            crate::util::stamp_updated(&mut data);
            return match db::update(ctx, ROLES_TABLE, id, data).await {
                Ok(record) => ok_json(&record),
                Err(e) => err_internal("Database error", e),
            };
        }
    }

    let mut data = HashMap::new();
    for key in &["name", "description", "permissions"] {
        if let Some(val) = body_peek.get(*key) {
            data.insert(key.to_string(), val.clone());
        }
    }
    crate::util::stamp_updated(&mut data);
    match db::update(ctx, ROLES_TABLE, id, data).await {
        Ok(record) => ok_json(&record),
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Role not found"),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_delete_role(ctx: &dyn Context, msg: &Message, path: &str) -> OutputStream {
    let id = path.strip_prefix("/admin/iam/roles/").unwrap_or("");
    // System-role guard, delete, and audit-log write live in the shared ops
    // layer (the JSON path previously logged nothing).
    match super::ops::delete_role(ctx, msg, id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(out) => out,
    }
}

async fn handle_list_permissions(ctx: &dyn Context) -> OutputStream {
    match db::list_all(ctx, PERMISSIONS_TABLE, vec![]).await {
        Ok(records) => {
            let total_count = records.len() as i64;
            ok_json(&db::RecordList {
                records,
                total_count,
                page: 1,
                page_size: total_count,
            })
        }
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_create_permission(ctx: &dyn Context, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        name: String,
        resource: String,
        actions: Vec<String>,
    }
    let raw = input.collect_to_bytes().await;
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    let mut data = json_map(serde_json::json!({
        "name": body.name,
        "resource": body.resource,
        "actions": body.actions
    }));
    crate::util::stamp_created(&mut data);
    match db::create(ctx, PERMISSIONS_TABLE, data).await {
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_delete_permission(ctx: &dyn Context, path: &str) -> OutputStream {
    let id = path.strip_prefix("/admin/iam/permissions/").unwrap_or("");
    if id.is_empty() {
        return err_bad_request("Missing permission ID");
    }
    match db::delete(ctx, PERMISSIONS_TABLE, id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Permission not found"),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_list_user_roles(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.query("user_id").to_string();
    let mut filters = Vec::new();
    if !user_id.is_empty() {
        filters.push(Filter {
            field: "user_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id),
        });
    }
    match db::list_all(ctx, USER_ROLES_TABLE, filters).await {
        Ok(records) => {
            let total_count = records.len() as i64;
            ok_json(&db::RecordList {
                records,
                total_count,
                page: 1,
                page_size: total_count,
            })
        }
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_assign_role(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        user_id: String,
        role: String,
    }
    let raw = input.collect_to_bytes().await;
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Check if already assigned
    let existing = db::list_all(
        ctx,
        USER_ROLES_TABLE,
        vec![
            Filter {
                field: "user_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(body.user_id.clone()),
            },
            Filter {
                field: "role".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(body.role.clone()),
            },
        ],
    )
    .await;
    match existing {
        Ok(records) => {
            if !records.is_empty() {
                return err_conflict("Role already assigned to user");
            }
        }
        Err(e) => return err_internal("Database error", e),
    }

    let assigned = format!("users/{}/roles/{}", body.user_id, body.role);
    let data = json_map(serde_json::json!({
        "user_id": body.user_id,
        "role": body.role,
        "assigned_at": crate::util::now_rfc3339(),
        "assigned_by": msg.user_id()
    }));
    match db::create(ctx, USER_ROLES_TABLE, data).await {
        Ok(record) => {
            // Audit-log like every other admin mutation (this JSON path used to
            // write zero audit rows).
            audit_log(
                ctx,
                msg.user_id(),
                "user_role.assign",
                &assigned,
                msg.remote_addr(),
            )
            .await;
            ok_json(&record)
        }
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_remove_role(ctx: &dyn Context, msg: &Message, path: &str) -> OutputStream {
    let id = path.strip_prefix("/admin/iam/user-roles/").unwrap_or("");
    if id.is_empty() {
        return err_bad_request("Missing user-role ID");
    }

    // Prevent admins from removing their own admin role (self-lockout)
    match db::get(ctx, USER_ROLES_TABLE, id).await {
        Ok(record) => {
            let role_user = record.str_field("user_id");
            let role_name = record.str_field("role");
            if role_user == msg.user_id() && role_name == "admin" {
                return err_bad_request("Cannot remove your own admin role");
            }
        }
        Err(e) if e.code == ErrorCode::NotFound => {
            return err_not_found("User-role assignment not found");
        }
        Err(e) => {
            return err_internal("Database error", e);
        }
    }

    match db::delete(ctx, USER_ROLES_TABLE, id).await {
        Ok(()) => {
            audit_log(
                ctx,
                msg.user_id(),
                "user_role.remove",
                &format!("user_roles/{id}"),
                msg.remote_addr(),
            )
            .await;
            ok_json(&serde_json::json!({"deleted": true}))
        }
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("User-role assignment not found"),
        Err(e) => err_internal("Database error", e),
    }
}

pub async fn seed_defaults(ctx: &dyn Context) {
    let count = db::count(ctx, ROLES_TABLE, &[]).await.unwrap_or(0);
    if count > 0 {
        return;
    }

    let now = crate::util::now_rfc3339();
    for (name, desc) in &[
        ("admin", "Full access to all resources"),
        ("user", "Standard user access"),
    ] {
        let data = json_map(serde_json::json!({
            "name": name,
            "description": desc,
            "is_system": true,
            "created_at": now,
            "permissions": []
        }));
        if let Err(e) = db::create(ctx, ROLES_TABLE, data).await {
            tracing::warn!("Failed to seed default role '{name}': {e}");
        }
    }
}
