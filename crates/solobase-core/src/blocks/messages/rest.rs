//! REST endpoint handlers for the messages block.
//!
//! Thin layer: parse HTTP request → call service → format JSON response.
//! Pure-CRUD shells (get context/entry, delete entry) go through the shared
//! `blocks::crud` helpers instead.

use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::service::{self, ListContextsParams, ListEntriesParams};
use crate::blocks::{
    crud,
    helpers::{err_bad_request, err_internal, err_not_found, ok_json},
};

/// Path prefix preceding the context id in the REST routes.
const CONTEXTS_PREFIX: &str = "/b/messages/api/contexts/";

/// Path prefix preceding the entry id in the REST routes.
const ENTRIES_PREFIX: &str = "/b/messages/api/entries/";

/// Convert empty string to None (msg.query() returns "" for missing params).
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

// ---------------------------------------------------------------------------
// Path extraction helpers
// ---------------------------------------------------------------------------

/// Extract context ID from paths like `/b/messages/api/contexts/{id}`
/// or `/b/messages/api/contexts/{id}/entries`.
fn extract_context_id(msg: &Message) -> &str {
    let var = msg.var("id");
    if !var.is_empty() {
        return var;
    }
    let suffix = msg.path().strip_prefix(CONTEXTS_PREFIX).unwrap_or("");
    suffix.split('/').next().unwrap_or("")
}

// ---------------------------------------------------------------------------
// Context endpoints
// ---------------------------------------------------------------------------

pub async fn list_contexts(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (_, page_size, offset) = msg.pagination_params(20);
    let params = ListContextsParams {
        context_type: non_empty(msg.query("type")),
        status: non_empty(msg.query("status")),
        sender_id: non_empty(msg.query("sender_id")),
        parent_id: non_empty(msg.query("parent_id")),
        page_size: page_size as i64,
        offset: offset as i64,
    };
    match service::list_contexts(ctx, &params).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("list_contexts failed", e),
    }
}

pub async fn create_context(ctx: &dyn Context, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(rename = "type")]
        context_type: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        sender_id: String,
        #[serde(default)]
        recipient_id: String,
        parent_id: Option<String>,
        metadata: Option<serde_json::Value>,
    }
    let raw = input.collect_to_bytes().await;
    let body: Body = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    match service::create_context(
        ctx,
        &body.context_type,
        &body.title,
        &body.sender_id,
        &body.recipient_id,
        body.parent_id.as_deref(),
        body.metadata,
    )
    .await
    {
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("create_context failed", e),
    }
}

pub async fn get_context(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_get(
        ctx,
        msg,
        service::CONTEXTS_TABLE,
        CONTEXTS_PREFIX,
        "Context",
    )
    .await
}

pub async fn update_context(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let id = extract_context_id(msg);
    if id.is_empty() {
        return err_bad_request("Missing context ID");
    }
    let raw = input.collect_to_bytes().await;
    let body: std::collections::HashMap<String, serde_json::Value> =
        match serde_json::from_slice(&raw) {
            Ok(b) => b,
            Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
        };
    match service::update_context(ctx, id, body).await {
        Ok(record) => ok_json(&record),
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Context not found"),
        Err(e) => err_internal("Database error", e),
    }
}

pub async fn delete_context(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = extract_context_id(msg);
    if id.is_empty() {
        return err_bad_request("Missing context ID");
    }
    match service::delete_context(ctx, id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Context not found"),
        Err(e) => err_internal("delete_context failed", e),
    }
}

// ---------------------------------------------------------------------------
// Entry endpoints
// ---------------------------------------------------------------------------

pub async fn list_entries(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let context_id = extract_context_id(msg);
    if context_id.is_empty() {
        return err_bad_request("Missing context ID");
    }
    let (_, page_size, offset) = msg.pagination_params(100);
    let params = ListEntriesParams {
        kind: non_empty(msg.query("kind")),
        role: non_empty(msg.query("role")),
        page_size: page_size as i64,
        offset: offset as i64,
    };
    match service::list_entries(ctx, context_id, &params).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("list_entries failed", e),
    }
}

pub async fn add_entry(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let context_id = extract_context_id(msg);
    if context_id.is_empty() {
        return err_bad_request("Missing context ID");
    }
    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(default = "default_kind")]
        kind: String,
        #[serde(default)]
        role: String,
        #[serde(default)]
        sender_id: String,
        #[serde(default)]
        content: String,
        content_type: Option<String>,
        metadata: Option<serde_json::Value>,
    }
    fn default_kind() -> String {
        "message".to_string()
    }
    let raw = input.collect_to_bytes().await;
    let body: Body = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    match service::add_entry(
        ctx,
        context_id,
        &body.kind,
        &body.role,
        &body.sender_id,
        &body.content,
        body.content_type.as_deref(),
        body.metadata,
    )
    .await
    {
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("add_entry failed", e),
    }
}

pub async fn get_entry(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_get(ctx, msg, service::ENTRIES_TABLE, ENTRIES_PREFIX, "Entry").await
}

pub async fn delete_entry(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete(ctx, msg, service::ENTRIES_TABLE, ENTRIES_PREFIX, "Entry").await
}
