//! Public product catalog: `/b/products/catalog` (list of active products)
//! and `/b/products/catalog/{id}` (single active product), both unauthenticated.

use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, Message, OutputStream};

use super::PRODUCTS_TABLE;
use crate::{
    blocks::crud,
    http::{err_bad_request, err_internal, err_not_found, ok_json},
    util::RecordExt,
};

pub(super) async fn handle_catalog(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let filters = vec![Filter {
        field: "status".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String("active".to_string()),
    }];
    let sort = vec![SortField {
        field: "name".to_string(),
        desc: false,
    }];
    crud::crud_list(ctx, msg, PRODUCTS_TABLE, filters, Some(sort)).await
}

pub(super) async fn handle_get_product_public(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = {
        let var = msg.var("id");
        if var.is_empty() {
            msg.path()
                .strip_prefix("/b/products/catalog/")
                .unwrap_or("")
        } else {
            var
        }
    };
    if id.is_empty() {
        return err_bad_request("Missing product ID");
    }

    match db::get(ctx, PRODUCTS_TABLE, id).await {
        Ok(record) => {
            let status = record.str_field("status");
            if status != "active" {
                return err_not_found("Product not found");
            }
            ok_json(&record)
        }
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Product not found"),
        Err(e) => err_internal("Database error", e),
    }
}
