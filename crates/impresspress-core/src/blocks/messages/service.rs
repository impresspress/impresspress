//! Service layer for the messages block.
//!
//! Plain async functions — no HTTP awareness. Shared by the REST and
//! SSR-page handlers. Pure-CRUD REST shells with no business logic
//! (get context/entry, delete entry) go through `blocks::crud` instead.

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

// Table-name constants live in `crate::messages_schema` so consumers
// (e.g. the LLM chat UI) can reference them without compiling this module.
// Re-exported here so existing `messages::service::{CONTEXTS_TABLE,
// ENTRIES_TABLE}` references inside the messages block continue to resolve.
pub use crate::messages_schema::{CONTEXTS_TABLE, ENTRIES_TABLE};
use crate::util::json_map;

/// Build an `Equal` filter for `field` when `value` is present. Mirrors the
/// per-field `if let Some(...) { filters.push(...) }` pattern used across
/// `list_contexts` / `list_entries`.
fn maybe_eq(field: &str, value: Option<&str>) -> Option<Filter> {
    let v = value?;
    Some(Filter {
        field: field.to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(v.to_string()),
    })
}

// ---------------------------------------------------------------------------
// Context operations
// ---------------------------------------------------------------------------

pub async fn create_context(
    ctx: &dyn Context,
    context_type: &str,
    title: &str,
    sender_id: &str,
    recipient_id: &str,
    parent_id: Option<&str>,
    metadata: Option<serde_json::Value>,
) -> Result<db::Record, WaferError> {
    let metadata = metadata.unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));

    let mut data = json_map(serde_json::json!({
        "type": context_type,
        "status": "active",
        "title": title,
        "sender_id": sender_id,
        "recipient_id": recipient_id,
        "metadata": metadata,
    }));

    if let Some(pid) = parent_id {
        data.insert("parent_id".to_string(), serde_json::json!(pid));
    }

    crate::util::stamp_created(&mut data);

    db::create(ctx, CONTEXTS_TABLE, data).await
}

pub async fn get_context(ctx: &dyn Context, id: &str) -> Result<db::Record, WaferError> {
    db::get(ctx, CONTEXTS_TABLE, id).await
}

pub struct ListContextsParams {
    pub context_type: Option<String>,
    pub status: Option<String>,
    pub sender_id: Option<String>,
    pub parent_id: Option<String>,
    pub page_size: i64,
    pub offset: i64,
}

pub async fn list_contexts(
    ctx: &dyn Context,
    params: &ListContextsParams,
) -> Result<db::RecordList, WaferError> {
    let filters = [
        ("type", params.context_type.as_deref()),
        ("status", params.status.as_deref()),
        ("sender_id", params.sender_id.as_deref()),
        ("parent_id", params.parent_id.as_deref()),
    ]
    .into_iter()
    .filter_map(|(field, value)| maybe_eq(field, value))
    .collect();

    let opts = ListOptions {
        filters,
        sort: vec![SortField {
            field: "updated_at".to_string(),
            desc: true,
        }],
        limit: params.page_size,
        offset: params.offset,
        skip_count: false,
        ..Default::default()
    };

    db::list(ctx, CONTEXTS_TABLE, &opts).await
}

pub async fn update_context(
    ctx: &dyn Context,
    id: &str,
    updates: std::collections::HashMap<String, serde_json::Value>,
) -> Result<db::Record, WaferError> {
    let allowed = ["status", "title", "metadata"];
    let mut data = std::collections::HashMap::new();
    for key in &allowed {
        if let Some(v) = updates.get(*key) {
            data.insert(key.to_string(), v.clone());
        }
    }
    crate::util::stamp_updated(&mut data);

    db::update(ctx, CONTEXTS_TABLE, id, data).await
}

pub async fn delete_context(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    // Cascade delete entries first
    let filters = vec![Filter {
        field: "context_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(id.to_string()),
    }];
    // Propagate a cascade failure instead of swallowing it: returning early
    // here leaves the context (and its entries) intact, whereas the old
    // `Ok`-on-warn deleted the parent and orphaned the entries.
    db::delete_by_filters(ctx, ENTRIES_TABLE, filters).await?;

    db::delete(ctx, CONTEXTS_TABLE, id).await
}

// ---------------------------------------------------------------------------
// Entry operations
// ---------------------------------------------------------------------------

// One argument per message field the caller must supply; a param-struct
// refactor is out of scope for a lint sweep (behavior-preserving cleanup only).
#[allow(clippy::too_many_arguments)]
pub async fn add_entry(
    ctx: &dyn Context,
    context_id: &str,
    kind: &str,
    role: &str,
    sender_id: &str,
    content: &str,
    content_type: Option<&str>,
    metadata: Option<serde_json::Value>,
) -> Result<db::Record, WaferError> {
    let metadata = metadata.unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let content_type = content_type.unwrap_or("text/plain");

    let now = crate::util::now_rfc3339();
    let data = json_map(serde_json::json!({
        "context_id": context_id,
        "kind": kind,
        "role": role,
        "status": "",
        "sender_id": sender_id,
        "content": content,
        "content_type": content_type,
        "metadata": metadata,
        "created_at": now,
    }));

    let record = db::create(ctx, ENTRIES_TABLE, data).await?;

    // Bump parent context's updated_at
    let mut context_update = std::collections::HashMap::new();
    crate::util::stamp_updated(&mut context_update);
    if let Err(e) = db::update(ctx, CONTEXTS_TABLE, context_id, context_update).await {
        tracing::warn!("Failed to update context updated_at after add_entry: {e}");
    }

    Ok(record)
}

pub struct ListEntriesParams {
    pub kind: Option<String>,
    pub role: Option<String>,
    pub page_size: i64,
    pub offset: i64,
}

pub async fn list_entries(
    ctx: &dyn Context,
    context_id: &str,
    params: &ListEntriesParams,
) -> Result<db::RecordList, WaferError> {
    let mut filters = vec![Filter {
        field: "context_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(context_id.to_string()),
    }];

    if let Some(ref k) = params.kind {
        filters.push(Filter {
            field: "kind".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(k.clone()),
        });
    }
    if let Some(ref r) = params.role {
        filters.push(Filter {
            field: "role".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(r.clone()),
        });
    }

    let opts = ListOptions {
        filters,
        sort: vec![SortField {
            field: "created_at".to_string(),
            desc: false,
        }],
        limit: params.page_size,
        offset: params.offset,
        skip_count: false,
        ..Default::default()
    };

    db::list(ctx, ENTRIES_TABLE, &opts).await
}
