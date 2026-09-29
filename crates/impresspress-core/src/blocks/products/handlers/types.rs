//! Product-type taxonomy CRUD (admin `/b/products/api/admin/types`; read-only
//! list also served to regular users at `/b/products/types`).
//!
//! Every response is a `contracts::ProductTypeView` (or a list of them)
//! built from the row; the create body is a typed request.

use wafer_run::{context::Context, InputStream, Message, OutputStream};

// `repo::types::TABLE` is handed to the generic CRUD helpers below, each of
// which takes its table from the caller — which is why this file names the
// constant at all. See the `types` entry in `tests/repo_door.rs`.
use crate::{
    blocks::{
        crud,
        products::{
            contracts::{
                CreateProductTypeRequest, PageQuery, ProductTypeListResponse, ProductTypeView,
            },
            repo::types::TABLE as TYPES_TABLE,
        },
    },
    http::ok_json,
};

pub(super) async fn handle_list_types(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = PageQuery::from_message(msg);
    match crud::list_page(
        ctx,
        TYPES_TABLE,
        i64::from(query.page),
        i64::from(query.page_size),
        vec![],
        None,
    )
    .await
    {
        Ok(list) => ok_json(&ProductTypeListResponse::from_record_list(&list)),
        Err(response) => response,
    }
}

pub(super) async fn handle_create_type(
    ctx: &dyn Context,
    _msg: &Message,
    input: InputStream,
) -> OutputStream {
    let request: CreateProductTypeRequest = match crud::read_json_body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    match crud::create_record(ctx, TYPES_TABLE, request.into_columns()).await {
        Ok(record) => ok_json(&ProductTypeView::from_record(&record)),
        Err(response) => response,
    }
}

pub(super) async fn handle_delete_type(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Type") {
        Ok(id) => id,
        Err(response) => return response,
    };
    match crud::delete_record(ctx, TYPES_TABLE, id, "Type").await {
        Ok(deleted) => ok_json(&deleted),
        Err(response) => response,
    }
}
