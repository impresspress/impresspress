//! Pricing template CRUD (admin-only, `/admin/b/products/pricing`).
//!
//! Not to be confused with the sibling `pricing` module, which evaluates a
//! template's formula against a product/variables at request time
//! (`/b/products/calculate-price`).

use std::collections::HashMap;

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::blocks::{crud, products::PRICING_TABLE};

pub(super) async fn handle_list_pricing(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_list(ctx, msg, PRICING_TABLE, vec![], None).await
}

pub(super) async fn handle_create_pricing(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    crud::crud_create(ctx, msg, input, PRICING_TABLE, HashMap::new()).await
}

pub(super) async fn handle_update_pricing(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    crud::crud_update(
        ctx,
        msg,
        input,
        PRICING_TABLE,
        "/admin/b/products/pricing/",
        "Pricing template",
    )
    .await
}

pub(super) async fn handle_delete_pricing(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete(
        ctx,
        msg,
        PRICING_TABLE,
        "/admin/b/products/pricing/",
        "Pricing template",
    )
    .await
}
