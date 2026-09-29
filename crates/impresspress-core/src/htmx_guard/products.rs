//! The products block's entry in the htmx guard.
//!
//! The pages' per-record controls are the close-only surface for a deleted
//! product — Restore, Archive offer, Deactivate Payment Link — on both the
//! admin and the seller side, so the fixture seeds a deleted product with a
//! published offer and an active link for each owner, plus a live one of
//! each, a seller account, an order and a group so every page with an `{id}`
//! renders.
//!
//! The block's repository is private to it, so the offers are created and
//! published, and the products deleted, through the block's own JSON API as
//! their owners would. The rows no API writes without Stripe — the products
//! themselves (so their ids are known), the seller account, the order, the
//! group and the Payment Link — are test-fixture rows written straight to the
//! database.

use std::{collections::HashMap, sync::Arc};

use wafer_core::clients::database as db;
use wafer_run::{Block, InputStream, Message};

use super::{Entry, Exempt};
use crate::{
    blocks::products::ProductsBlock,
    test_support::{
        admin_msg, auth_msg,
        htmx::{Fixture, Page, Site},
        TestContext,
    },
};

/// The seller who owns `mine` and `mine_gone`.
const SELLER: &str = "seller_a";
/// The buyer of `pur_1`.
const BUYER: &str = "user_1";
/// The raw token of `pur_1`'s guest receipt link.
const RECEIPT_TOKEN: &str = "htmx-guard-receipt";

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/products",
        fixture: Some(fixture),
        exempt: EXEMPT,
        must_reach: &[
            "/b/products/my-products?view=deleted",
            "/b/products/admin/manage?view=deleted",
        ],
        cannot_succeed: &[],
        must_fire: &[
            "create /b/products/api/admin/products/{id}/restore",
            "delete /b/products/api/admin/products/{product_id}/offers/{offer_id}",
            "delete /b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links/{link_id}",
            "create /b/products/api/products/{id}/restore",
            "delete /b/products/api/products/{product_id}/offers/{offer_id}",
            "delete /b/products/api/products/{product_id}/offers/{offer_id}/payment-links/{link_id}",
        ],
    }
}

/// Who asks: the admin under an `/admin` path, the buyer on their purchase
/// pages and rows, the seller everywhere else.
fn caller(action: &str, path: &str) -> Message {
    if path.contains("/admin") {
        admin_msg(action, path)
    } else if path.starts_with("/b/products/my-purchases")
        || path.starts_with("/b/products/purchases/")
        || path.starts_with("/b/products/orders/")
    {
        auth_msg(action, path, BUYER)
    } else {
        auth_msg(action, path, SELLER)
    }
}

fn fixture() -> std::pin::Pin<Box<dyn std::future::Future<Output = Fixture>>> {
    Box::pin(async {
        let mut ctx = TestContext::with_products().await;
        ctx.set_config(crate::config_vars::ALLOW_USER_PRODUCTS_KEY, "true");
        let ctx = ctx;

        seed(
            &ctx,
            "impresspress__products__seller_accounts",
            "seller_1",
            serde_json::json!({
                "user_id": SELLER,
                "status": "active",
                "stripe_account_id": "acct_seller_1",
                "details_submitted": true,
                "charges_enabled": true,
                "payouts_enabled": true,
                "requirements_json": "{}",
                "fee_basis_points": 250,
            }),
        )
        .await;

        seed_product(&ctx, "live", None).await;
        let live_offer = publish_offer(&ctx, "live", None).await;
        let live_preset = create_preset(&ctx, "live", &live_offer, None).await;
        seed_product(&ctx, "gone", None).await;
        let gone_offer = publish_offer(&ctx, "gone", None).await;
        seed_link(&ctx, &gone_offer).await;
        delete_product(&ctx, "gone", None).await;

        seed_product(&ctx, "mine", Some(SELLER)).await;
        let mine_offer = publish_offer(&ctx, "mine", Some(SELLER)).await;
        let mine_preset = create_preset(&ctx, "mine", &mine_offer, Some(SELLER)).await;
        seed_product(&ctx, "mine_gone", Some(SELLER)).await;
        let mine_gone_offer = publish_offer(&ctx, "mine_gone", Some(SELLER)).await;
        seed_link(&ctx, &mine_gone_offer).await;
        delete_product(&ctx, "mine_gone", Some(SELLER)).await;

        seed(
            &ctx,
            "impresspress__products__purchases",
            "pur_1",
            serde_json::json!({
                "user_id": BUYER,
                "buyer_user_id": BUYER,
                "seller_account_id": "seller_1",
                "status": "completed",
                "provider": "stripe",
                "currency": "USD",
                "total_cents": 1000,
                "subtotal_cents": 1000,
                // The guest receipt link's token, so the order-status row
                // answers the order.
                "receipt_token_hash": crate::util::sha256_hex(RECEIPT_TOKEN.as_bytes()),
                "receipt_token_expires_at":
                    (chrono::Utc::now() + chrono::Duration::days(365)).to_rfc3339(),
            }),
        )
        .await;
        seed(
            &ctx,
            "impresspress__products__groups",
            "grp_1",
            serde_json::json!({"name": "Group one", "user_id": SELLER}),
        )
        .await;

        Fixture {
            ctx,
            site: Site(vec![Arc::new(ProductsBlock::new()) as Arc<dyn Block>]),
            caller,
            pages: pages(),
            probes: probes(&live_offer, &live_preset, &mine_offer, &mine_preset),
            operator_input: &[],
        }
    })
}

/// Every products page, with the purchases filter the admin list links to.
/// The Deleted views are the crawl's to find, from the Live lists' links.
fn pages() -> Vec<Page> {
    vec![
        Page::at("/b/products"),
        Page::at("/b/products/"),
        Page::at("/b/products/my-products"),
        Page::at("/b/products/my-products/new"),
        Page::at("/b/products/my-products/mine"),
        Page::at("/b/products/my-products/mine_gone/close"),
        Page::at("/b/products/my-purchases"),
        Page::at("/b/products/my-purchases/pur_1"),
        Page::at("/b/products/selling"),
        Page::at("/b/products/selling/orders"),
        Page::at("/b/products/selling/orders/pur_1"),
        Page::at("/b/products/admin"),
        Page::at("/b/products/admin/"),
        Page::at("/b/products/admin/manage"),
        Page::at("/b/products/admin/new"),
        Page::at("/b/products/admin/products/live"),
        Page::at("/b/products/admin/products/gone/close"),
        Page::at("/b/products/admin/groups"),
        Page::at("/b/products/admin/purchases"),
        Page::at("/b/products/admin/purchases").with("status", "completed"),
        Page::at("/b/products/admin/purchases/pur_1"),
        Page::at("/b/products/admin/sellers"),
        Page::at("/b/products/admin/sellers/seller_1"),
        Page::at("/b/products/admin/stripe"),
        Page::at("/b/products/admin/settings"),
    ]
}

/// Where each parameterised JSON row is read: the platform's live product,
/// offer and preset under the admin rows, the seller's under their own, and
/// the seeded purchase, seller account and group.
fn probes(
    live_offer: &Offer,
    live_preset: &str,
    mine_offer: &Offer,
    mine_preset: &str,
) -> Vec<(&'static str, String)> {
    let admin = "/b/products/api/admin/products/live";
    let admin_offer = format!("{admin}/offers/{}", live_offer.id);
    let own = "/b/products/api/products/mine";
    let own_offer = format!("{own}/offers/{}", mine_offer.id);
    vec![
        ("/b/products/api/admin/products/{id}", admin.to_string()),
        (
            "/b/products/api/admin/products/{product_id}/offers",
            format!("{admin}/offers"),
        ),
        (
            "/b/products/api/admin/products/{product_id}/offers/{offer_id}",
            admin_offer.clone(),
        ),
        (
            "/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets",
            format!("{admin_offer}/presets"),
        ),
        (
            "/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
            format!("{admin_offer}/presets/{live_preset}"),
        ),
        (
            "/b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links",
            format!("{admin_offer}/payment-links"),
        ),
        (
            "/b/products/api/admin/purchases/{id}",
            "/b/products/api/admin/purchases/pur_1".to_string(),
        ),
        (
            "/b/products/api/admin/sellers/{id}",
            "/b/products/api/admin/sellers/seller_1".to_string(),
        ),
        ("/b/products/api/products/{id}", own.to_string()),
        (
            "/b/products/api/products/{product_id}/offers",
            format!("{own}/offers"),
        ),
        (
            "/b/products/api/products/{product_id}/offers/{offer_id}",
            own_offer.clone(),
        ),
        (
            "/b/products/api/products/{product_id}/offers/{offer_id}/presets",
            format!("{own_offer}/presets"),
        ),
        (
            "/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
            format!("{own_offer}/presets/{mine_preset}"),
        ),
        (
            "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links",
            format!("{own_offer}/payment-links"),
        ),
        (
            "/b/products/groups/{id}",
            "/b/products/groups/grp_1".to_string(),
        ),
        (
            "/b/products/groups/{id}/products",
            "/b/products/groups/grp_1/products".to_string(),
        ),
        (
            "/b/products/api/seller/orders/{id}",
            "/b/products/api/seller/orders/pur_1".to_string(),
        ),
        (
            "/b/products/catalog/{id}",
            "/b/products/catalog/live".to_string(),
        ),
        (
            "/b/products/storefront/{product_id}",
            "/b/products/storefront/live".to_string(),
        ),
        (
            "/b/products/orders/{id}/status",
            format!("/b/products/orders/pur_1/status?receipt_token={RECEIPT_TOKEN}"),
        ),
        (
            "/b/products/purchases/{id}",
            "/b/products/purchases/pur_1".to_string(),
        ),
    ]
}

/// A checkout preset on `offer` of `product`, created as its owner; its id.
async fn create_preset(
    ctx: &TestContext,
    product: &str,
    offer: &Offer,
    owner: Option<&str>,
) -> String {
    let created = call(
        ctx,
        owner,
        "create",
        &format!(
            "{}/offers/{}/presets",
            product_api(product, owner),
            offer.id
        ),
        serde_json::json!({"name": "Probe preset"}),
    )
    .await;
    created["id"]
        .as_str()
        .or_else(|| created["preset"]["id"].as_str())
        .unwrap_or_else(|| panic!("the created preset carries its id: {created}"))
        .to_string()
}

/// `GET` rows that are not pages and publish no schema. Every other `GET` row
/// of the block publishes a response schema.
const EXEMPT: &[(&str, Exempt)] = &[("/b/products/storefront.js", Exempt::Asset)];

/// Write one fixture row under `id`.
async fn seed(ctx: &TestContext, table: &str, id: &str, data: serde_json::Value) {
    let mut data: HashMap<String, serde_json::Value> =
        serde_json::from_value(data).expect("fixture row is an object");
    data.insert("id".to_string(), serde_json::json!(id));
    db::create(ctx, table, data)
        .await
        .unwrap_or_else(|e| panic!("seed into {table} failed: {}", e.message));
}

/// A live product: the platform's when `owner` is `None`, else that seller's.
async fn seed_product(ctx: &TestContext, id: &str, owner: Option<&str>) {
    let mut data = serde_json::json!({"name": format!("Product {id}"), "status": "active"});
    if let Some(owner) = owner {
        data["owner_kind"] = serde_json::json!("user");
        data["owner_id"] = serde_json::json!(owner);
        data["created_by"] = serde_json::json!(owner);
        data["seller_account_id"] = serde_json::json!("seller_1");
    }
    seed(ctx, "impresspress__products__products", id, data).await;
}

/// The offer API base for `product` as its owner reaches it.
fn product_api(product: &str, owner: Option<&str>) -> String {
    match owner {
        None => format!("/b/products/api/admin/products/{product}"),
        Some(_) => format!("/b/products/api/products/{product}"),
    }
}

/// Send a JSON request through the block as `owner` (the admin when `None`),
/// requiring a 2xx, and return the JSON answer.
async fn call(
    ctx: &TestContext,
    owner: Option<&str>,
    action: &str,
    path: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let mut msg = match owner {
        None => admin_msg(action, path),
        Some(user) => auth_msg(action, path, user),
    };
    msg.set_meta("http.header.content-type", "application/json");
    let out = ProductsBlock::new()
        .handle(
            ctx,
            msg,
            InputStream::from_bytes(serde_json::to_vec(&body).expect("encode body")),
        )
        .await;
    let answer = crate::test_support::htmx::answer(out).await;
    assert!(
        (200..300).contains(&answer.status),
        "fixture request {action} {path} was answered {}: {}",
        answer.status,
        answer.body
    );
    serde_json::from_str(&answer.body).unwrap_or(serde_json::Value::Null)
}

/// Create and publish a fixed-price offer on `product` as its owner, and
/// return its id.
async fn publish_offer(ctx: &TestContext, product: &str, owner: Option<&str>) -> Offer {
    let base = product_api(product, owner);
    let created = call(
        ctx,
        owner,
        "create",
        &format!("{base}/offers"),
        serde_json::json!({
            "name": "Plan",
            "mode": "payment",
            "currency": "usd",
            "pricing_model": "fixed",
            "usage_type": "licensed",
            "billing_scheme": "per_unit",
            "tax_behavior": "exclusive",
            "components": [{
                "key": "price",
                "label": "Plan",
                "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": 1000}
            }]
        }),
    )
    .await;
    let id = created["offer"]["id"]
        .as_str()
        .or_else(|| created["id"].as_str())
        .unwrap_or_else(|| panic!("the created offer carries its id: {created}"))
        .to_string();
    call(
        ctx,
        owner,
        "create",
        &format!("{base}/offers/{id}/publish"),
        serde_json::json!({}),
    )
    .await;
    Offer { id }
}

/// A published offer's id.
struct Offer {
    id: String,
}

/// An active, never-synced Payment Link on `offer`: what the close manager
/// renders a Deactivate control for.
async fn seed_link(ctx: &TestContext, offer: &Offer) {
    seed(
        ctx,
        "impresspress__products__payment_links",
        &format!("link_{}", offer.id),
        serde_json::json!({
            "offer_id": offer.id,
            "active": true,
            "configuration_hash": "close-me",
        }),
    )
    .await;
}

/// Soft-delete `product` through the block, as its owner would.
async fn delete_product(ctx: &TestContext, product: &str, owner: Option<&str>) {
    call(
        ctx,
        owner,
        "delete",
        &product_api(product, owner),
        serde_json::json!({}),
    )
    .await;
}
