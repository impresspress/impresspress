//! Generic CRUD helpers for block handlers.
//!
//! These encapsulate the repeated list/get/create/update/delete patterns
//! so each handler reduces to a one-liner for pure-CRUD operations.
//!
// audit-allow-file: pure pass-through helpers — every db::* call here takes
// the table name as a `collection: &str` parameter from the caller. WRAP
// coverage is the caller's responsibility; static analysis at this file
// would flag every line as unresolved without surfacing a real bug.

use std::collections::HashMap;

use wafer_block::db::{Filter, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use crate::{
    http::{err_bad_request, err_internal, err_not_found, err_unauthorized, ok_json},
    util::{field_as_string, stamp_created, stamp_updated},
};

/// Extract the record id that follows `path_prefix` in the request path.
/// Returns `""` when the prefix doesn't match or nothing follows it.
/// Extract the record id for a CRUD route. Prefers the router-populated
/// `req.param.id` (set by `endpoint_match::dispatch` when the block uses the
/// shared matcher) and falls back to stripping `path_prefix` off the resource
/// path for callers/tests that build the message by hand. Mirrors
/// [`crate::util::path_param`] but keeps the trailing-segment
/// behaviour of the old prefix-strip for the fallback.
fn id_from_path<'m>(msg: &'m Message, path_prefix: &str) -> &'m str {
    let var = msg.var("id");
    if !var.is_empty() {
        return var;
    }
    msg.path().strip_prefix(path_prefix).unwrap_or("")
}

// ---------------------------------------------------------------------------
// CRUD helpers
// ---------------------------------------------------------------------------

/// List records with pagination, optional extra filters, and optional sort
/// (`None` = newest first by `created_at`).
pub async fn crud_list(
    ctx: &dyn Context,
    msg: &Message,
    collection: &str,
    extra_filters: Vec<Filter>,
    sort: Option<Vec<SortField>>,
) -> OutputStream {
    let (page, page_size, _) = msg.pagination_params(20);
    let sort = sort.unwrap_or_else(|| {
        vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }]
    });
    match db::paginated_list(
        ctx,
        collection,
        page as i64,
        page_size as i64,
        extra_filters,
        sort,
    )
    .await
    {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

/// Get a single record by ID extracted from the path.
pub async fn crud_get(
    ctx: &dyn Context,
    msg: &Message,
    collection: &str,
    path_prefix: &str,
    not_found_label: &str,
) -> OutputStream {
    let id = id_from_path(msg, path_prefix);
    if id.is_empty() {
        return err_bad_request(&format!("Missing {} ID", not_found_label.to_lowercase()));
    }
    match db::get(ctx, collection, id).await {
        Ok(record) => ok_json(&record),
        Err(e) if e.code == ErrorCode::NotFound => {
            err_not_found(&format!("{not_found_label} not found"))
        }
        Err(e) => err_internal("Database error", e),
    }
}

/// Create a record from the request body, with auto-timestamps and optional defaults.
pub async fn crud_create(
    ctx: &dyn Context,
    _msg: &Message,
    input: InputStream,
    collection: &str,
    defaults: HashMap<String, serde_json::Value>,
) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let body: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    let mut data = body;
    stamp_created(&mut data);

    for (key, val) in defaults {
        data.entry(key).or_insert(val);
    }

    match db::create(ctx, collection, data).await {
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("Database error", e),
    }
}

/// Update a record by ID extracted from the path, with auto-updated_at.
pub async fn crud_update(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    collection: &str,
    path_prefix: &str,
    not_found_label: &str,
) -> OutputStream {
    let id = id_from_path(msg, path_prefix);
    if id.is_empty() {
        return err_bad_request(&format!("Missing {} ID", not_found_label.to_lowercase()));
    }

    let raw = input.collect_to_bytes().await;
    let mut body: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    stamp_updated(&mut body);

    match db::update(ctx, collection, id, body).await {
        Ok(record) => ok_json(&record),
        Err(e) if e.code == ErrorCode::NotFound => {
            err_not_found(&format!("{not_found_label} not found"))
        }
        Err(e) => err_internal("Database error", e),
    }
}

/// Delete a record by ID extracted from the path.
pub async fn crud_delete(
    ctx: &dyn Context,
    msg: &Message,
    collection: &str,
    path_prefix: &str,
    not_found_label: &str,
) -> OutputStream {
    let id = id_from_path(msg, path_prefix);
    if id.is_empty() {
        return err_bad_request(&format!("Missing {} ID", not_found_label.to_lowercase()));
    }
    match db::delete(ctx, collection, id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) if e.code == ErrorCode::NotFound => {
            err_not_found(&format!("{not_found_label} not found"))
        }
        Err(e) => err_internal("Database error", e),
    }
}

// ---------------------------------------------------------------------------
// Owner-scoped CRUD helpers
// ---------------------------------------------------------------------------

/// Identifies an owner-scoped resource for the `crud_*_owned` helpers.
///
/// Owner-scoped resources are user-facing rows where access requires the
/// requesting user to match the row's owner column (e.g. a user's own
/// products or groups).
pub struct OwnedResource<'a> {
    /// Table the records live in.
    pub collection: &'a str,
    /// Path prefix preceding the record id (e.g. `"/b/products/products/"`).
    pub path_prefix: &'a str,
    /// Column holding the owning user's id (e.g. `"created_by"`).
    pub owner_field: &'a str,
    /// Human-readable label for error messages (e.g. `"Product"`).
    pub label: &'a str,
}

/// Fetch `id` from `collection` and verify `record[owner_field] == user_id`.
///
/// Returns the record on success. On failure returns a ready-to-send error
/// response: 401 for unauthenticated callers, 404 for both "row missing" and
/// "row owned by someone else" (existence must not leak to non-owners), and
/// 500 for database errors.
pub async fn verify_owner(
    ctx: &dyn Context,
    collection: &str,
    id: &str,
    owner_field: &str,
    user_id: &str,
    not_found_label: &str,
) -> Result<Record, OutputStream> {
    if user_id.is_empty() {
        return Err(err_unauthorized("Not authenticated"));
    }
    match db::get(ctx, collection, id).await {
        Ok(record) => {
            if field_as_string(&record, owner_field) != user_id {
                return Err(err_not_found(&format!("{not_found_label} not found")));
            }
            Ok(record)
        }
        Err(e) if e.code == ErrorCode::NotFound => {
            Err(err_not_found(&format!("{not_found_label} not found")))
        }
        Err(e) => Err(err_internal("Database error", e)),
    }
}

/// Get a single owner-scoped record by ID extracted from the path.
pub async fn crud_get_owned(
    ctx: &dyn Context,
    msg: &Message,
    res: &OwnedResource<'_>,
) -> OutputStream {
    let id = id_from_path(msg, res.path_prefix);
    if id.is_empty() {
        return err_bad_request(&format!("Missing {} ID", res.label.to_lowercase()));
    }
    match verify_owner(
        ctx,
        res.collection,
        id,
        res.owner_field,
        msg.user_id(),
        res.label,
    )
    .await
    {
        Ok(record) => ok_json(&record),
        Err(resp) => resp,
    }
}

/// Update an owner-scoped record by ID extracted from the path, with
/// auto-`updated_at`. `strip_fields` are removed from the request body
/// before the write (e.g. the owner column, to prevent ownership changes).
pub async fn crud_update_owned(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    res: &OwnedResource<'_>,
    strip_fields: &[&str],
) -> OutputStream {
    let id = id_from_path(msg, res.path_prefix).to_string();
    if id.is_empty() {
        return err_bad_request(&format!("Missing {} ID", res.label.to_lowercase()));
    }
    if let Err(resp) = verify_owner(
        ctx,
        res.collection,
        &id,
        res.owner_field,
        msg.user_id(),
        res.label,
    )
    .await
    {
        return resp;
    }

    let raw = input.collect_to_bytes().await;
    let mut body: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    for field in strip_fields {
        body.remove(*field);
    }
    stamp_updated(&mut body);

    match db::update(ctx, res.collection, &id, body).await {
        Ok(record) => ok_json(&record),
        Err(e) if e.code == ErrorCode::NotFound => {
            err_not_found(&format!("{} not found", res.label))
        }
        Err(e) => err_internal("Database error", e),
    }
}

/// Delete an owner-scoped record by ID extracted from the path.
pub async fn crud_delete_owned(
    ctx: &dyn Context,
    msg: &Message,
    res: &OwnedResource<'_>,
) -> OutputStream {
    let id = id_from_path(msg, res.path_prefix).to_string();
    if id.is_empty() {
        return err_bad_request(&format!("Missing {} ID", res.label.to_lowercase()));
    }
    if let Err(resp) = verify_owner(
        ctx,
        res.collection,
        &id,
        res.owner_field,
        msg.user_id(),
        res.label,
    )
    .await
    {
        return resp;
    }
    match db::delete(ctx, res.collection, &id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) if e.code == ErrorCode::NotFound => {
            err_not_found(&format!("{} not found", res.label))
        }
        Err(e) => err_internal("Database error", e),
    }
}
