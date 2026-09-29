//! Group CRUD: admin (`/b/products/api/admin/groups`) and user-owned
//! (`/b/products/groups`, gated on `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS`),
//! plus the "products in a user's group" listing and the read-only
//! group-templates listing.
//!
//! Every group response is a `contracts::GroupView` (or a list of them)
//! built from the row; every write body is a typed request whose fields are
//! the only columns a client can reach.

use wafer_block::db::{Filter, FilterOp};
use wafer_run::{context::Context, InputStream, Message, OutputStream};

// `repo::groups::TABLE` is handed to the generic CRUD helpers below, each of
// which takes its table from the caller — which is why this file names the
// constant at all. Every query this file builds itself goes through
// `repo::groups` / `repo::group_templates`. See the `groups` entry in
// `tests/repo_door.rs`.
use crate::{
    blocks::{
        crud,
        products::{
            contracts::{
                CreateGroupRequest, CreateOwnGroupRequest, GroupListResponse,
                GroupTemplateListResponse, GroupView, PageQuery, ProductListResponse,
                UpdateGroupRequest, UpdateOwnGroupRequest,
            },
            repo::{self, groups::TABLE as GROUPS_TABLE},
        },
    },
    http::{err_unauthenticated, ok_json},
};

/// User-owned group rows (`/b/products/groups/{id}`), owned via `user_id`.
const USER_GROUP: crud::OwnedResource<'static> = crud::OwnedResource {
    collection: GROUPS_TABLE,
    owner_field: "user_id",
    label: "Group",
};

// --- Groups (admin) ---

pub(super) async fn handle_list_groups(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = PageQuery::from_message(msg);
    match crud::list_page(
        ctx,
        GROUPS_TABLE,
        i64::from(query.page),
        i64::from(query.page_size),
        vec![],
        None,
    )
    .await
    {
        Ok(list) => ok_json(&GroupListResponse::from_record_list(&list)),
        Err(response) => response,
    }
}

pub(super) async fn handle_create_group(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let request: CreateGroupRequest = match crud::read_json_body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let mut data = request.into_columns();
    data.entry("user_id".to_string())
        .or_insert_with(|| serde_json::Value::String(msg.user_id().to_string()));
    // The creator is the caller, as for products: the administrator here even
    // when the body assigns the group to someone else as its owner.
    data.insert(
        "created_by".to_string(),
        serde_json::Value::String(msg.user_id().to_string()),
    );
    match crud::create_record(ctx, GROUPS_TABLE, data).await {
        Ok(record) => ok_json(&GroupView::from_record(&record)),
        Err(response) => response,
    }
}

pub(super) async fn handle_update_group(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let id = match crud::path_id(msg, "Group") {
        Ok(id) => id,
        Err(response) => return response,
    };
    let request: UpdateGroupRequest = match crud::read_json_body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    match crud::update_record(ctx, GROUPS_TABLE, id, request.into_columns(), "Group").await {
        Ok(record) => ok_json(&GroupView::from_record(&record)),
        Err(response) => response,
    }
}

pub(super) async fn handle_delete_group(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Group") {
        Ok(id) => id,
        Err(response) => return response,
    };
    match crud::delete_record(ctx, GROUPS_TABLE, id, "Group").await {
        Ok(deleted) => ok_json(&deleted),
        Err(response) => response,
    }
}

// --- User's own groups ---

pub(super) async fn handle_user_list_groups(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthenticated("Not authenticated");
    }

    let owned = vec![Filter {
        field: "user_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id),
    }];
    match repo::groups::list_by_name(ctx, owned, 1000).await {
        Ok(result) => ok_json(&GroupListResponse::from_record_list(&result)),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

pub(super) async fn handle_user_get_group(ctx: &dyn Context, msg: &Message) -> OutputStream {
    match crud::get_owned(ctx, msg, &USER_GROUP).await {
        Ok(record) => ok_json(&GroupView::from_record(&record)),
        Err(response) => response,
    }
}

pub(super) async fn handle_user_create_group(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthenticated("Not authenticated");
    }

    let request: CreateOwnGroupRequest = match crud::read_json_body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let mut body = request.into_columns();
    body.insert(
        "user_id".to_string(),
        serde_json::Value::String(user_id.clone()),
    );
    body.insert("created_by".to_string(), serde_json::Value::String(user_id));
    // Default group_template_id to the seeded "default" template's real
    // (UUIDv7) id — same reasoning as for product_template_id in
    // `product::handle_user_create_product`.
    if body
        .get("group_template_id")
        .is_none_or(|v| v.as_str().is_some_and(str::is_empty))
    {
        match repo::group_templates::default_id(ctx).await {
            Ok(Some(default_id)) => {
                body.insert(
                    "group_template_id".to_string(),
                    serde_json::Value::String(default_id),
                );
            }
            // No default template seeded: proceed without one, as this create
            // always has.
            Ok(None) => {}
            // A read that failed is not "there is no default": writing the
            // row anyway persists exactly the template-less group the default
            // exists to prevent, and it outlives the outage looking
            // deliberate.
            Err(error) => return crud::db_error_internal(error, "Could not load group template"),
        }
    }

    match crud::create_record(ctx, GROUPS_TABLE, body).await {
        Ok(record) => ok_json(&GroupView::from_record(&record)),
        Err(response) => response,
    }
}

pub(super) async fn handle_user_update_group(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    // `user_id` is not a field of the request, so ownership cannot change.
    let request: UpdateOwnGroupRequest = match crud::read_json_body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    match crud::update_owned(ctx, msg, &USER_GROUP, request.into_columns()).await {
        Ok(record) => ok_json(&GroupView::from_record(&record)),
        Err(response) => response,
    }
}

pub(super) async fn handle_user_delete_group(ctx: &dyn Context, msg: &Message) -> OutputStream {
    match crud::delete_owned(ctx, msg, &USER_GROUP).await {
        Ok(deleted) => ok_json(&deleted),
        Err(response) => response,
    }
}

// Products in a user's group
pub(super) async fn handle_user_group_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    // `/b/products/groups/{id}/products`: `{id}` as the table bound it.
    let group_id = match crud::path_id(msg, "Group") {
        Ok(value) => value,
        Err(response) => return response,
    };

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

    let query = PageQuery::from_message(msg);
    let filters = vec![Filter {
        field: "group_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(group_id.to_string()),
    }];
    // The door, not a generic paginated read against the table: a group's
    // product list is a read like any other, so it takes the soft-delete
    // filter and then the typed projection.
    match repo::products::list_page(
        ctx,
        i64::from(query.page),
        i64::from(query.page_size),
        filters,
        None,
    )
    .await
    {
        Ok(list) => ok_json(&ProductListResponse::from_record_list(&list)),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

// User-accessible group templates (read-only). Answers with the same
// `{records, total_count, page, page_size}` envelope as every other list in
// the block — the shape its schema always documented — over the whole
// table, sorted by name, the way the owner group list does.
pub(super) async fn handle_user_list_group_templates(
    ctx: &dyn Context,
    _msg: &Message,
) -> OutputStream {
    match repo::group_templates::list_by_name(ctx, 1000).await {
        Ok(result) => ok_json(&GroupTemplateListResponse::from_record_list(&result)),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}
