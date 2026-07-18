//! Admin dashboard stats: `/admin/b/products/stats`.

use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream};

use super::{GROUPS_TABLE, PRODUCTS_TABLE};
use crate::{
    blocks::products::repo,
    http::{err_internal, ok_json},
};

pub(super) async fn handle_stats(ctx: &dyn Context, _msg: &Message) -> OutputStream {
    let active_filter = [Filter {
        field: "status".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String("active".to_string()),
    }];

    // Fan out the 5 independent counts/sums concurrently rather than
    // serializing 5 round-trips on the request path. `futures::join!`
    // (not `tokio::join!`) because tokio is an optional dep in
    // impresspress-core's Cargo.toml — futures 0.3 is unconditional.
    let (total_products, active_products, total_purchases, total_revenue, total_groups) = futures::join!(
        db::count(ctx, PRODUCTS_TABLE, &[]),
        db::count(ctx, PRODUCTS_TABLE, &active_filter),
        repo::purchases::count_all(ctx),
        repo::purchases::sum_completed_cents(ctx),
        db::count(ctx, GROUPS_TABLE, &[]),
    );

    // A repository failure on any of these must surface as an error, not be
    // fabricated into a "0" stat — an admin reading "0 products / $0 revenue"
    // during a genuine outage would read that as real business data rather
    // than a broken dashboard. `unwrap_or(0)` used to do exactly that for
    // every one of the 5 counts/sums independently.
    let total_products = match total_products {
        Ok(n) => n,
        Err(e) => return err_internal("Database error", e),
    };
    let active_products = match active_products {
        Ok(n) => n,
        Err(e) => return err_internal("Database error", e),
    };
    let total_purchases = match total_purchases {
        Ok(n) => n,
        Err(e) => return err_internal("Database error", e),
    };
    let total_revenue = match total_revenue {
        Ok(n) => n,
        Err(e) => return err_internal("Database error", e),
    };
    let total_groups = match total_groups {
        Ok(n) => n,
        Err(e) => return err_internal("Database error", e),
    };

    ok_json(&serde_json::json!({
        "total_products": total_products,
        "active_products": active_products,
        "total_purchases": total_purchases,
        "total_revenue": total_revenue,
        "total_groups": total_groups
    }))
}
