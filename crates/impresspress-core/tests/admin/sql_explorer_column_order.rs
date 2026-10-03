//! The admin SQL explorer shows a result's columns in the order the query
//! returned them.
//!
//! `SELECT b, a, c` is `b`, `a`, `c` — not name order, not a hash order —
//! in both explorer surfaces: the SSR SQL editor's result grid
//! (`POST /b/admin/database/query`) and the JSON API
//! (`POST /b/admin/api/database/query`), which names the columns in a
//! `columns` list and writes each row's `data` keys in the same order. Driven
//! through the production request path, as `sql_explorer_secrets.rs` is.

use impresspress_core::{
    blocks::admin::{migrations, AdminBlock},
    test_support::{admin_msg, TestContext},
};
use serde_json::json;
use wafer_core::clients::database as db;

/// Neither name order (`a`, `b`, `c`, `id`) nor its reverse.
const QUERY: &str = "SELECT name AS b, id, description AS a, is_system AS c \
                     FROM impresspress__admin__roles WHERE id = 'role-order'";
const ORDER: [&str; 4] = ["b", "id", "a", "c"];

async fn explorer_ctx() -> TestContext {
    let mut ctx = TestContext::new().await;
    ctx.register_block("impresspress/admin", std::sync::Arc::new(AdminBlock::new()));
    migrations::apply(&ctx.fixture())
        .await
        .expect("apply admin migrations");
    let data = [
        ("id", json!("role-order")),
        ("name", json!("ordered")),
        ("description", json!("cols")),
        ("permissions", json!("[]")),
        ("is_system", json!(0)),
        ("created_at", json!("2026-01-01T00:00:00Z")),
        ("updated_at", json!("2026-01-01T00:00:00Z")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    db::create(&ctx.fixture(), "impresspress__admin__roles", data)
        .await
        .expect("stage a roles row");
    ctx
}

/// Byte offset of each needle in `haystack`, asserting each is present.
fn positions(haystack: &str, needles: &[String]) -> Vec<usize> {
    needles
        .iter()
        .map(|n| {
            haystack
                .find(n.as_str())
                .unwrap_or_else(|| panic!("{n} missing from: {haystack}"))
        })
        .collect()
}

fn assert_increasing(at: &[usize], what: &str, body: &str) {
    assert!(
        at.windows(2).all(|w| w[0] < w[1]),
        "{what} out of SELECT order {ORDER:?} (offsets {at:?}): {body}"
    );
}

#[tokio::test]
async fn the_ssr_result_grid_lists_columns_in_select_order() {
    let ctx = explorer_ctx().await;
    let form = format!(
        "query={}",
        url::form_urlencoded::byte_serialize(QUERY.as_bytes()).collect::<String>()
    );
    let mut msg = admin_msg("create", "/b/admin/database/query");
    msg.set_meta("req.content_type", "application/x-www-form-urlencoded");
    let out = ctx
        .dispatch_resolved_with_input(msg, wafer_run::InputStream::from_bytes(form.into_bytes()))
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let html = String::from_utf8_lossy(&parts.body).into_owned();
    assert_eq!(parts.status, 200, "{html}");

    let headers: Vec<String> = ORDER.iter().map(|c| format!(">{c}</th>")).collect();
    assert_increasing(&positions(&html, &headers), "header cells", &html);
    // The row's cells follow the same order: `ordered`, `role-order`, `cols`, `0`.
    let cells: Vec<String> = ["ordered", "role-order", "cols"]
        .iter()
        .map(|v| format!(">{v}</td>"))
        .collect();
    assert_increasing(&positions(&html, &cells), "row cells", &html);
}

#[tokio::test]
async fn the_json_api_returns_columns_in_select_order() {
    let ctx = explorer_ctx().await;
    let out = ctx
        .dispatch_resolved_json(
            admin_msg("create", "/b/admin/api/database/query"),
            &json!({ "query": QUERY }),
        )
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let body = String::from_utf8_lossy(&parts.body).into_owned();
    assert_eq!(parts.status, 200, "{body}");

    let value: serde_json::Value = serde_json::from_str(&body).expect("JSON body");
    assert_eq!(value["columns"], json!(ORDER), "{body}");
    assert_eq!(value["row_count"], json!(1), "{body}");
    assert_eq!(value["rows"][0]["id"], json!("role-order"), "{body}");

    // `serde_json::Value` sorts object keys, so read the row's key order off
    // the body text itself.
    let data_at = body.find(r#""data":{"#).expect("a row's data object");
    let keys: Vec<String> = ORDER.iter().map(|c| format!(r#""{c}":"#)).collect();
    assert_increasing(&positions(&body[data_at..], &keys), "row data keys", &body);
}

#[tokio::test]
async fn an_empty_result_has_no_columns() {
    let ctx = explorer_ctx().await;
    let out = ctx
        .dispatch_resolved_json(
            admin_msg("create", "/b/admin/api/database/query"),
            &json!({ "query": "SELECT id FROM impresspress__admin__roles WHERE id = 'none'" }),
        )
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let value: serde_json::Value = serde_json::from_slice(&parts.body).expect("JSON body");
    assert_eq!(value, json!({ "columns": [], "rows": [], "row_count": 0 }),);
}
