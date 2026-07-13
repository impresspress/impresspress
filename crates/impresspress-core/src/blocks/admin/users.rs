use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::ops;
use crate::{
    blocks::auth::USERS_TABLE as COLLECTION,
    http::{err_bad_request, err_internal, err_not_found, ok_json},
};

/// `path` is the normalized `/admin/users[...]` sub-path passed explicitly by
/// the admin dispatcher (no `req.resource` rewrite). The leaf handlers read the
/// user id from `req.param.id`, which this dispatcher binds from `path`.
pub async fn handle(
    ctx: &dyn Context,
    msg: &Message,
    path: &str,
    input: InputStream,
) -> OutputStream {
    let action = msg.action();

    match (action, path) {
        ("retrieve", "/admin/users") => handle_list(ctx, msg).await,
        ("retrieve", _) if path.starts_with("/admin/users/") => {
            handle_get(ctx, msg, user_id_from(path)).await
        }
        ("update", _) if path.starts_with("/admin/users/") => {
            handle_update(ctx, msg, user_id_from(path), input).await
        }
        ("delete", _) if path.starts_with("/admin/users/") => {
            handle_delete(ctx, msg, user_id_from(path)).await
        }
        _ => err_not_found("not found"),
    }
}

/// Extract the first `/`-bounded user-id segment after `/admin/users/`.
fn user_id_from(path: &str) -> &str {
    let rest = path.strip_prefix("/admin/users/").unwrap_or("");
    match rest.find('/') {
        Some(idx) => &rest[..idx],
        None => rest,
    }
}

async fn handle_list(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (page, page_size, _) = msg.pagination_params(20);
    let search = msg.query("search").to_string();

    let mut filters = vec![Filter {
        field: "deleted_at".to_string(),
        operator: FilterOp::IsNull,
        value: serde_json::Value::Null,
    }];

    if !search.is_empty() {
        filters.push(Filter {
            field: "email".to_string(),
            operator: FilterOp::Like,
            value: serde_json::Value::String(format!("%{search}%")),
        });
    }

    let sort = vec![SortField {
        field: "created_at".to_string(),
        desc: true,
    }];

    match db::paginated_list(
        ctx,
        COLLECTION,
        page as i64,
        page_size as i64,
        filters,
        sort,
    )
    .await
    {
        Ok(mut result) => {
            // Strip password hashes and bulk-enrich with roles via a single
            // `In`-filter query (was N+1: one `list_all` per row).
            let user_ids: Vec<&str> = result.records.iter().map(|r| r.id.as_str()).collect();
            let roles_by_user = ops::fetch_roles(ctx, &user_ids).await;
            for record in &mut result.records {
                record.data.remove("password_hash");
                let roles = roles_by_user.get(&record.id).cloned().unwrap_or_default();
                record
                    .data
                    .insert("roles".to_string(), serde_json::json!(roles));
            }
            ok_json(&result)
        }
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_get(ctx: &dyn Context, _msg: &Message, id: &str) -> OutputStream {
    if id.is_empty() {
        return err_bad_request("Missing user ID");
    }
    get_user(ctx, id).await
}

async fn get_user(ctx: &dyn Context, id: &str) -> OutputStream {
    match db::get(ctx, COLLECTION, id).await {
        Ok(mut record) => {
            record.data.remove("password_hash");
            // Get roles via the shared single-query helper.
            let roles = ops::fetch_roles(ctx, &[id])
                .await
                .remove(id)
                .unwrap_or_default();
            let mut resp = match serde_json::to_value(&record) {
                Ok(v) => v,
                Err(e) => return err_internal("Failed to serialize user record", e),
            };
            if let Some(obj) = resp.as_object_mut() {
                obj.insert("roles".to_string(), serde_json::json!(roles));
            }
            ok_json(&resp)
        }
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("User not found"),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_update(
    ctx: &dyn Context,
    msg: &Message,
    id: &str,
    input: InputStream,
) -> OutputStream {
    if id.is_empty() {
        return err_bad_request("Missing user ID");
    }

    let raw = input.collect_to_bytes().await;
    let body: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // The self-disable guard, safe-field whitelist, and audit-log write all
    // live in the shared ops layer so the SSR surface can't diverge.
    match ops::update_user_fields(ctx, msg, id, &body).await {
        Ok(record) => ok_json(&record),
        Err(out) => out,
    }
}

async fn handle_delete(ctx: &dyn Context, msg: &Message, id: &str) -> OutputStream {
    if id.is_empty() {
        return err_bad_request("Missing user ID");
    }

    // Self-delete guard, soft-delete, and audit-log write live in the shared
    // ops layer (the JSON path previously logged nothing).
    match ops::delete_user(ctx, msg, id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(out) => out,
    }
}
