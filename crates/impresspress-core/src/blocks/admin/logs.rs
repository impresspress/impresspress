use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream};

use crate::http::{err_internal, err_not_found, ok_json};

/// Audit log entries (admin-initiated mutations).
pub(crate) const AUDIT_LOGS_TABLE: &str = "impresspress__admin__audit_logs";

// `REQUEST_LOGS_TABLE` lives in the leaf `crate::admin_schema` module (read by
// the request pipeline without depending on the admin block); re-exported here
// so existing `logs::REQUEST_LOGS_TABLE` / `admin::REQUEST_LOGS_TABLE`
// references keep resolving.
pub(crate) use crate::admin_schema::REQUEST_LOGS_TABLE;

/// Storage access log entries (one row per object read/write).
pub(crate) const STORAGE_ACCESS_LOGS_TABLE: &str = "impresspress__admin__storage_access_logs";

/// `path` is the normalized `/admin/logs` sub-path, passed explicitly (no
/// `req.resource` rewrite).
pub async fn handle(ctx: &dyn Context, msg: &Message, path: &str) -> OutputStream {
    let action = msg.action();

    match (action, path) {
        ("retrieve", "/admin/logs") => handle_list(ctx, msg).await,
        ("retrieve", "/admin/system-logs") => handle_system_logs(ctx, msg).await,
        _ => err_not_found("not found"),
    }
}

async fn handle_list(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (page, page_size, _) = msg.pagination_params(50);

    let mut filters = Vec::new();
    let user_id = msg.query("user_id").to_string();
    if !user_id.is_empty() {
        filters.push(Filter {
            field: "user_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id),
        });
    }
    let action_filter = msg.query("action").to_string();
    if !action_filter.is_empty() {
        filters.push(Filter {
            field: "action".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(action_filter),
        });
    }
    let resource = msg.query("resource").to_string();
    if !resource.is_empty() {
        filters.push(Filter {
            field: "resource".to_string(),
            operator: FilterOp::Like,
            value: serde_json::Value::String(format!("%{resource}%")),
        });
    }

    let sort = vec![SortField {
        field: "created_at".to_string(),
        desc: true,
    }];

    match db::paginated_list(
        ctx,
        AUDIT_LOGS_TABLE,
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

async fn handle_system_logs(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (page, page_size, _) = msg.pagination_params(50);

    let mut filters = Vec::new();
    let status = msg.query("status").to_string();
    if !status.is_empty() {
        filters.push(Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(status),
        });
    }
    let path_filter = msg.query("path").to_string();
    if !path_filter.is_empty() {
        filters.push(Filter {
            field: "path".to_string(),
            operator: FilterOp::Like,
            value: serde_json::Value::String(format!("%{path_filter}%")),
        });
    }

    let sort = vec![SortField {
        field: "created_at".to_string(),
        desc: true,
    }];

    match db::paginated_list(
        ctx,
        REQUEST_LOGS_TABLE,
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

// ---------------------------------------------------------------------------
// Audit log helper
// ---------------------------------------------------------------------------

/// Record an admin action in the audit_logs table.
/// Fire-and-forget: errors are logged but don't block the caller.
pub async fn audit_log(
    ctx: &dyn Context,
    user_id: &str,
    action: &str,
    resource: &str,
    ip_address: &str,
) {
    let mut data = std::collections::HashMap::new();
    data.insert("user_id".to_string(), serde_json::json!(user_id));
    data.insert("action".to_string(), serde_json::json!(action));
    data.insert("resource".to_string(), serde_json::json!(resource));
    data.insert("ip_address".to_string(), serde_json::json!(ip_address));
    crate::util::stamp_created(&mut data);

    if let Err(e) = db::create(ctx, AUDIT_LOGS_TABLE, data).await {
        tracing::warn!(action, resource, "audit_log write failed: {}", e.message);
    }
}
