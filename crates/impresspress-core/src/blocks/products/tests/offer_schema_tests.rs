//! The offer schemas an agent reads are derived from the types the handler
//! deserializes, so a value built by following the schema is one the
//! handler accepts.

use serde_json::Value;
use wafer_run::HttpMethod;

use super::harness::{admin_create_msg, ctx, dispatch, output_to_json};
use crate::blocks::products::routes::ROUTES;

const CREATE_OFFER: &str = "/b/products/api/admin/products/{product_id}/offers";

/// The input schema the route table declares for `method template`.
fn route_input(method: HttpMethod, template: &str) -> Value {
    let row = ROUTES
        .iter()
        .find(|r| r.method == method && r.template == template)
        .unwrap_or_else(|| panic!("{method} {template} is not in ROUTES"));
    (row.input.expect("the route declares an input"))()
}

#[test]
fn create_offer_schema_describes_components() {
    let schema = route_input(HttpMethod::Post, CREATE_OFFER);
    let item = &schema["properties"]["components"]["items"];
    for field in ["key", "label", "amount"] {
        assert!(
            item["properties"][field].is_object(),
            "component.{field} missing: {item}"
        );
    }
    let amount = serde_json::to_string(&item["properties"]["amount"]).unwrap();
    assert!(
        amount.contains("unit_amount_minor"),
        "AmountRule variants not described: {amount}"
    );
    assert!(
        amount.contains("\"fixed\""),
        "AmountRule's `type` tag not described: {amount}"
    );
}

#[tokio::test]
async fn a_component_shaped_by_the_schema_is_accepted_by_create_offer() {
    let ctx = ctx().await;
    let (create, input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({"name": "Bag"}),
    );
    let product = output_to_json(dispatch(&ctx, create, input).await).await;
    let product_id = product["id"].as_str().expect("product id").to_string();

    // `key` and `label` strings, and `amount` as the `fixed` variant of the
    // `type`-tagged `AmountRule`, as the derived schema describes them.
    let schema = route_input(HttpMethod::Post, CREATE_OFFER);
    let amount = &schema["properties"]["components"]["items"]["properties"]["amount"];
    assert!(
        serde_json::to_string(amount).unwrap().contains("\"fixed\""),
        "{amount}"
    );
    let body = serde_json::json!({
        "name": "250 g bag", "mode": "payment", "currency": "nzd",
        "pricing_model": "fixed", "usage_type": "licensed",
        "billing_scheme": "per_unit", "tax_behavior": "unspecified",
        "components": [{"key": "bag", "label": "250 g bag",
                        "amount": {"type": "fixed", "unit_amount_minor": 1450}}]
    });
    let (create_offer, input) = admin_create_msg(
        &format!("/b/products/api/admin/products/{product_id}/offers"),
        body,
    );
    let offer = output_to_json(dispatch(&ctx, create_offer, input).await).await;
    assert_eq!(
        offer["offer"]["components"][0]["amount"]["unit_amount_minor"], 1450,
        "{offer}"
    );
}
