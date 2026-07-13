use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::crud,
    http::{err_internal, ok_json},
};

/// Pricing-variable definitions (e.g. user-defined inputs available to
/// pricing-template formulas).
pub(crate) const TABLE: &str = "impresspress__products__variables";

/// Path prefix preceding a variable id in update/delete requests.
const PATH_PREFIX: &str = "/admin/b/products/variables/";

pub async fn handle_list(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let mut filters = Vec::new();
    let scope = msg.query("scope").to_string();
    if !scope.is_empty() {
        filters.push(Filter {
            field: "scope".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(scope),
        });
    }
    let product_id = msg.query("product_id").to_string();
    if !product_id.is_empty() {
        filters.push(Filter {
            field: "product_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(product_id),
        });
    }

    let opts = ListOptions {
        filters,
        sort: vec![SortField {
            field: "name".to_string(),
            desc: false,
        }],
        limit: 1000,
        ..Default::default()
    };

    match db::list(ctx, TABLE, &opts).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

pub async fn handle_create(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    // Defaults applied when the body omits these columns (`var_type` and
    // `scope` are NOT NULL with a DB default; supply them explicitly so the
    // write succeeds regardless of the backend's default handling).
    let defaults = HashMap::from([
        (
            "var_type".to_string(),
            serde_json::Value::String("number".to_string()),
        ),
        (
            "scope".to_string(),
            serde_json::Value::String("system".to_string()),
        ),
    ]);
    crud::crud_create(ctx, msg, input, TABLE, defaults).await
}

pub async fn handle_update(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    crud::crud_update(ctx, msg, input, TABLE, PATH_PREFIX, "Variable").await
}

pub async fn handle_delete(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete(ctx, msg, TABLE, PATH_PREFIX, "Variable").await
}
