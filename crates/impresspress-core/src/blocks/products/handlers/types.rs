//! Product-type taxonomy CRUD (admin `/admin/b/products/types`; read-only
//! list also served to regular users at `/b/products/types`).

use std::collections::HashMap;

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::TYPES_TABLE;
use crate::blocks::crud;

pub(super) async fn handle_list_types(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_list(ctx, msg, TYPES_TABLE, vec![], None).await
}

pub(super) async fn handle_create_type(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    crud::crud_create(ctx, msg, input, TYPES_TABLE, HashMap::new()).await
}

pub(super) async fn handle_delete_type(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete(ctx, msg, TYPES_TABLE, "/admin/b/products/types/", "Type").await
}
