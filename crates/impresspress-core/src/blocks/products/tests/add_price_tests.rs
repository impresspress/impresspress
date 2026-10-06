//! "Add a price": a product without pricing offers the offer editor in create
//! mode from its empty pricing state, seeded from the product, and the
//! editor's submission is the offer API's own create request — so what the
//! API accepts and refuses is exactly what the editor shows.

use wafer_run::{streams::output::TerminalNotResponse, ErrorCode, OutputStream};

use super::harness::*;

/// The product manager page, as the admin route renders it.
async fn admin_manager(ctx: &crate::test_support::TestContext, product_id: &str) -> String {
    let (mut msg, input) = admin_get_msg(&format!("/b/products/admin/products/{product_id}"));
    msg.set_meta("http.header.accept", "text/html");
    output_to_html(dispatch(ctx, msg, input).await).await
}

/// The page's pricing section (`#product-pricing`), up to the scripts.
fn pricing_section(html: &str) -> &str {
    html.split_once(r#"id="product-pricing""#)
        .and_then(|(_, rest)| rest.split_once("<script"))
        .map_or("", |(section, _)| section)
}

/// The `window.__productManagerConfig` the page hands its bundle.
fn page_config(html: &str) -> serde_json::Value {
    let raw = html
        .split_once("window.__productManagerConfig=")
        .and_then(|(_, rest)| rest.split_once(";</script>"))
        .map(|(config, _)| config)
        .unwrap_or_else(|| panic!("no manager config in\n{html}"));
    serde_json::from_str(raw).expect("manager config is JSON")
}

/// The seed with the one price row the editor adds and the author prices,
/// in the shape `collectWizardComponents` builds it.
fn priced(mut seed: serde_json::Value, amount_minor: i64) -> serde_json::Value {
    let label = seed["name"].clone();
    seed["components"] = serde_json::json!([{
        "key": "price",
        "label": label,
        "description": "",
        "sort_order": 0,
        "required": true,
        "amount": {"type": "fixed", "unit_amount_minor": amount_minor},
        "quantity": {"type": "fixed", "value": 1},
        "condition": {"op": "always"},
        "metadata": {}
    }]);
    seed
}

/// The refusal an offer request ended in: its code and message.
async fn refusal(out: OutputStream) -> (ErrorCode, String) {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(error)) => (error.code, error.message),
        Ok(buffer) => panic!(
            "expected a refusal, got {}",
            String::from_utf8_lossy(&buffer.body)
        ),
        Err(_) => panic!("expected a refusal, got another terminal"),
    }
}

async fn admin_product(ctx: &crate::test_support::TestContext, name: &str) -> String {
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": name, "currency": "nzd" }),
    );
    let product = output_to_json(dispatch(ctx, msg, input).await).await;
    product["id"].as_str().expect("product id").to_string()
}

/// Without pricing, the section's empty state carries "Add a price" as the
/// section's one primary, and the page config carries the create-mode seed:
/// named after the product, in its currency (canonical uppercase), with no
/// price row for the editor to pre-fill.
#[tokio::test]
async fn a_product_without_pricing_offers_add_a_price() {
    let ctx = ctx().await;
    let id = admin_product(&ctx, "Poster").await;

    let html = admin_manager(&ctx, &id).await;
    let pricing = pricing_section(&html);
    assert!(pricing.contains("No prices yet"), "{pricing}");
    assert_eq!(pricing.matches("btn--primary").count(), 1, "{pricing}");
    assert!(
        pricing.contains(
            r#"class="btn btn--primary btn--sm" type="button" data-action="pm-add-price""#
        ),
        "{pricing}"
    );
    assert!(pricing.contains("Add a price"), "{pricing}");

    let config = page_config(&html);
    assert_eq!(
        config["product_url"],
        format!("/b/products/api/admin/products/{id}")
    );
    let seed = &config["new_offer"];
    assert_eq!(seed["name"], "Poster");
    assert_eq!(seed["currency"], "NZD");
    assert_eq!(seed["mode"], "payment");
    assert_eq!(seed["pricing_model"], "fixed");
    assert_eq!(seed["components"], serde_json::json!([]));
    assert_eq!(seed["checkout"]["automatic_tax"], false);

    // The editor shows its own outcome beside its Save, in both modes.
    let editor = html
        .split_once(r#"id="product-manager-visual-editor""#)
        .and_then(|(_, rest)| rest.split_once(r#"id="product-pricing""#))
        .map_or("", |(editor, _)| editor);
    assert!(
        editor.contains(r#"id="manager-visual-error" role="alert""#),
        "{editor}"
    );
    assert!(editor.contains(r#"id="manager-visual-save""#), "{editor}");
}

/// The seed takes the deployment's automatic-tax setting, as the wizard's
/// checkbox default does.
#[tokio::test]
async fn the_seed_follows_the_automatic_tax_setting() {
    let ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX", "true")]).await;
    let id = admin_product(&ctx, "Taxed").await;
    let config = page_config(&admin_manager(&ctx, &id).await);
    assert_eq!(config["new_offer"]["checkout"]["automatic_tax"], true);
}

/// Once the product has a price, adding another stays one click away but is
/// secondary: the draft's Publish is the section's one primary.
#[tokio::test]
async fn with_a_price_add_a_price_is_secondary() {
    let ctx = ctx().await;
    let id = admin_product(&ctx, "Mug").await;
    let seed = page_config(&admin_manager(&ctx, &id).await)["new_offer"].clone();
    let (msg, input) = admin_create_msg(
        &format!("/b/products/api/admin/products/{id}/offers"),
        priced(seed, 1200),
    );
    output_to_json(dispatch(&ctx, msg, input).await).await;

    let html = admin_manager(&ctx, &id).await;
    let pricing = pricing_section(&html);
    assert!(!pricing.contains("No prices yet"), "{pricing}");
    assert!(
        pricing.contains(
            r#"class="btn btn--secondary btn--sm" type="button" data-action="pm-add-price""#
        ),
        "{pricing}"
    );
    assert_eq!(pricing.matches("btn--primary").count(), 1, "{pricing}");
    assert!(pricing.contains(r#"data-offer-op="publish""#), "{pricing}");
}

/// The seed plus the row the author prices is a definition the create
/// endpoint takes as it is: a draft offer, listed on the page, that
/// publishes.
#[tokio::test]
async fn the_priced_seed_creates_a_publishable_draft() {
    let ctx = ctx().await;
    let id = admin_product(&ctx, "Print").await;
    let seed = page_config(&admin_manager(&ctx, &id).await)["new_offer"].clone();
    let collection = format!("/b/products/api/admin/products/{id}/offers");

    let (msg, input) = admin_create_msg(&collection, priced(seed, 2500));
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(created["status"], "draft", "{created}");
    assert_eq!(created["offer"]["name"], "Print");
    assert_eq!(created["offer"]["currency"], "NZD");
    let offer_id = created["offer"]["id"].as_str().expect("offer id");

    let html = admin_manager(&ctx, &id).await;
    assert!(
        pricing_section(&html).contains(&format!(r#"data-offer-id="{offer_id}""#)),
        "{html}"
    );

    let (msg, input) = admin_create_msg(
        &format!("{collection}/{offer_id}/publish"),
        serde_json::json!({}),
    );
    let published = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(published["status"], "active", "{published}");
}

/// What the editor shows when the server refuses: the create endpoint's own
/// message. A body that is not an offer definition, and a currency the
/// deployment does not let sellers use, are each a 400 naming the problem.
#[tokio::test]
async fn create_mode_refusals_carry_the_reason() {
    let ctx = ctx().await;
    let id = admin_product(&ctx, "Card").await;
    let collection = format!("/b/products/api/admin/products/{id}/offers");
    let (msg, input) = admin_create_msg(&collection, serde_json::json!({ "name": "Card" }));
    let (code, message) = refusal(dispatch(&ctx, msg, input).await).await;
    assert_eq!(code, ErrorCode::InvalidArgument);
    assert!(message.starts_with("Invalid body:"), "{message}");

    let ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CURRENCIES", "nzd"),
    ])
    .await;
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "seller_one",
        serde_json::json!({ "name": "Seller card", "currency": "NZD" }),
    );
    let product = output_to_json(dispatch(&ctx, msg, input).await).await;
    let id = product["id"].as_str().expect("product id").to_string();
    let (msg, _input) = get_msg(&format!("/b/products/my-products/{id}"), "seller_one");
    let html =
        output_to_html(super::super::pages::product_manager(&ctx, &msg, &id, false).await).await;
    let config = page_config(&html);
    assert_eq!(
        config["product_url"],
        format!("/b/products/api/products/{id}")
    );
    let mut definition = priced(config["new_offer"].clone(), 900);
    definition["currency"] = serde_json::json!("USD");
    let (msg, input) = create_msg(
        &format!("/b/products/api/products/{id}/offers"),
        "seller_one",
        definition,
    );
    let (code, message) = refusal(dispatch(&ctx, msg, input).await).await;
    assert_eq!(code, ErrorCode::InvalidArgument);
    assert_eq!(message, "This currency is not allowed for sellers");
}

/// A seller's editor offers the policy's currencies, as the wizard does, and
/// a product whose currency the policy no longer allows seeds the first
/// allowed one rather than a currency the create endpoint would refuse.
#[tokio::test]
async fn a_seller_seed_keeps_to_the_allowed_currencies() {
    // The product is created while the policy allows its currency; then the
    // policy moves on without it.
    let mut ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CURRENCIES", "usd"),
    ])
    .await;
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "seller_two",
        serde_json::json!({ "name": "Legacy print", "currency": "USD" }),
    );
    let product = output_to_json(dispatch(&ctx, msg, input).await).await;
    let id = product["id"].as_str().expect("product id").to_string();
    ctx.set_config(
        "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CURRENCIES",
        "nzd, eur",
    );

    let (msg, _input) = get_msg(&format!("/b/products/my-products/{id}"), "seller_two");
    let html =
        output_to_html(super::super::pages::product_manager(&ctx, &msg, &id, false).await).await;
    assert_eq!(page_config(&html)["new_offer"]["currency"], "EUR", "{html}");
    assert!(
        html.contains(r#"list="manager-visual-currency-options""#),
        "{html}"
    );
    assert!(
        html.contains(
            r#"<datalist id="manager-visual-currency-options"><option value="EUR"></option><option value="NZD"></option></datalist>"#
        ),
        "{html}"
    );
    assert!(
        html.contains("Allowed seller currencies: EUR, NZD"),
        "{html}"
    );

    // The seeded currency is one the seller's create endpoint takes.
    let definition = priced(page_config(&html)["new_offer"].clone(), 700);
    let (msg, input) = create_msg(
        &format!("/b/products/api/products/{id}/offers"),
        "seller_two",
        definition,
    );
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(created["offer"]["currency"], "EUR", "{created}");
}

/// An administrator is under no currency policy: no list, no hint, and the
/// product's own currency.
#[tokio::test]
async fn an_admin_seed_keeps_the_product_currency() {
    let ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CURRENCIES", "eur")]).await;
    let id = admin_product(&ctx, "Admin print").await;
    let html = admin_manager(&ctx, &id).await;
    assert_eq!(page_config(&html)["new_offer"]["currency"], "NZD");
    assert!(!html.contains("Allowed seller currencies"), "{html}");
}
