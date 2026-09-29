//! Public product catalog: `/b/products/catalog` (list of active products)
//! and `/b/products/catalog/{id}` (single active product), both unauthenticated.
//!
//! Both publish `contracts::CatalogProductView`, the public projection of a
//! product row. Its field list is what keeps the ownership, moderation and
//! provider columns off the anonymous surface; see the comment above the
//! type for what is withheld and why.

use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{
        crud,
        products::{
            contracts::{CatalogProductListResponse, CatalogProductView, PageQuery, ProductStatus},
            repo,
        },
    },
    http::{err_internal, err_not_found, ok_json},
    util::{wire_str, RecordExt},
};

pub(super) async fn handle_catalog(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = PageQuery::from_message(msg);
    let filters = vec![Filter {
        field: "status".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String("active".to_string()),
    }];
    let sort = vec![SortField {
        field: "name".to_string(),
        desc: false,
    }];
    // The door, not a generic paginated read against the table: this is the
    // anonymous catalog, so a read that skipped `deleted_at IS NULL` would
    // put soft-deleted products back on sale. The typed projection is
    // applied to what the door returns.
    match repo::products::list_page(
        ctx,
        i64::from(query.page),
        i64::from(query.page_size),
        filters,
        Some(sort),
    )
    .await
    {
        Ok(list) => ok_json(&CatalogProductListResponse::from_record_list(&list)),
        // The table is the block's, not the request's: a `NotFound` here names
        // no row of the caller's, so it stays a 500 rather than telling the
        // caller their query found nothing.
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

pub(super) async fn handle_get_product_public(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Product") {
        Ok(value) => value,
        Err(response) => return response,
    };

    match repo::products::get(ctx, id).await {
        Ok(record) => {
            // The stored spelling against the variant's own, not a decode: a
            // row whose `status` is outside the contract has to stay invisible
            // to the public catalog, and a 500 here would say it exists.
            if record.str_field("status") != wire_str(&ProductStatus::Active) {
                return err_not_found("Product not found");
            }
            match CatalogProductView::from_record(&record) {
                Ok(view) => ok_json(&view),
                Err(error) => err_internal("Product row is outside the contract", error),
            }
        }
        Err(e) => crud::db_error(e, "Product not found", "Database error"),
    }
}
