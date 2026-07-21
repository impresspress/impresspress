use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_core::clients::database as db;
use wafer_run::{AuthLevel, Block, ErrorCode};

use super::{
    super::{
        contracts::{OfferDefinitionRequest, OfferStatus},
        repo, ProductsBlock, PRODUCTS_TABLE,
    },
    harness::{
        admin_create_msg, admin_get_msg, create_msg, ctx, ctx_with, delete_msg, dispatch_admin,
        dispatch_user, output_is_error, output_to_json, request_msg, seed, update_msg,
    },
};
use crate::util::RecordExt;

fn offer_definition(unit_amount_minor: i64) -> Value {
    json!({
        "name": "Custom print",
        "mode": "payment",
        "currency": "nzd",
        "pricing_model": "components",
        "usage_type": "licensed",
        "billing_scheme": "per_unit",
        "tax_behavior": "exclusive",
        "variables": [
            {
                "key": "pages",
                "kind": "integer",
                "label": "Pages",
                "required": true,
                "minimum": "1",
                "maximum": "20",
                "step": "1",
                "sort_order": 0
            },
            {
                "key": "note",
                "kind": "text",
                "label": "Print note",
                "maximum_length": 120,
                "visibility": "admin_only",
                "sort_order": 1
            }
        ],
        "components": [
            {
                "key": "pages",
                "label": "Printed pages",
                "sort_order": 0,
                "required": true,
                "amount": {
                    "type": "per_unit",
                    "input": "pages",
                    "unit_amount_minor": unit_amount_minor
                }
            }
        ],
        "checkout": {
            "automatic_tax": true,
            "collect_billing_address": true
        }
    })
}

fn definition_request(unit_amount_minor: i64) -> OfferDefinitionRequest {
    serde_json::from_value(offer_definition(unit_amount_minor)).expect("offer definition decodes")
}

async fn seed_product(test_ctx: &crate::test_support::TestContext, id: &str, owner_id: &str) {
    seed(
        test_ctx,
        PRODUCTS_TABLE,
        id,
        HashMap::from([
            ("name".to_string(), json!("Print shop")),
            ("slug".to_string(), json!(id)),
            ("status".to_string(), json!("active")),
            ("approval_status".to_string(), json!("approved")),
            (
                "owner_kind".to_string(),
                json!(if owner_id.is_empty() {
                    "platform"
                } else {
                    "user"
                }),
            ),
            ("owner_id".to_string(), json!(owner_id)),
            ("created_by".to_string(), json!(owner_id)),
        ]),
    )
    .await;
}

#[tokio::test]
async fn admin_offer_lifecycle_preserves_versions_and_storefront_visibility() {
    let test_ctx = ctx().await;
    seed_product(&test_ctx, "product_print", "").await;

    let collection = "/admin/b/products/products/product_print/offers";
    let (msg, input) = admin_create_msg(collection, offer_definition(25));
    let created = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let offer_id = created["offer"]["id"].as_str().unwrap().to_string();
    let component_id = created["offer"]["components"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(created["status"], "draft");
    assert_eq!(created["offer"]["currency"], "NZD");
    assert_eq!(created["offer"]["version"], 1);
    assert_eq!(created["offer"]["variables"][1]["maximum_length"], 120);

    let detail = format!("{collection}/{offer_id}");
    let (msg, input) = admin_create_msg(
        &format!("{detail}/preview"),
        json!({"offer_id": offer_id, "quantity": 2, "inputs": {"pages": 4}}),
    );
    let draft_preview = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(draft_preview["amounts"]["total_minor"], 200);

    let (msg, input) = create_msg(
        "/b/products/pricing/preview",
        "",
        json!({"offer_id": offer_id, "inputs": {"pages": 4}}),
    );
    assert!(
        output_is_error(
            dispatch_user(&test_ctx, msg, input).await,
            ErrorCode::NotFound
        )
        .await
    );

    let (msg, input) = admin_create_msg(
        &format!("{detail}/preview"),
        json!({"offer_id": "different_offer", "inputs": {"pages": 4}}),
    );
    assert!(
        output_is_error(
            dispatch_admin(&test_ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );

    let (msg, input) = update_msg(&detail, "admin_1", offer_definition(30));
    let updated = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(updated["offer"]["version"], 2);
    assert_eq!(updated["offer"]["components"][0]["id"], component_id);
    assert_eq!(
        updated["offer"]["components"][0]["amount"]["unit_amount_minor"],
        30
    );

    let (msg, input) = admin_create_msg(&format!("{detail}/publish"), json!({}));
    let published = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(published["status"], "active");

    let (msg, input) = create_msg(
        "/b/products/pricing/preview",
        "",
        json!({"offer_id": offer_id, "quantity": 2, "inputs": {"pages": 4}}),
    );
    let preview = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(preview["amounts"]["total_minor"], 240);

    let (msg, input) = update_msg(&detail, "admin_1", offer_definition(40));
    assert!(
        output_is_error(
            dispatch_admin(&test_ctx, msg, input).await,
            ErrorCode::AlreadyExists
        )
        .await
    );

    let (msg, input) = admin_create_msg(&format!("{detail}/duplicate"), json!({}));
    let duplicate = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(duplicate["status"], "draft");
    assert_eq!(duplicate["offer"]["version"], 1);
    assert_ne!(duplicate["offer"]["id"], offer_id);

    let (msg, input) = delete_msg(&detail, "admin_1");
    let archived = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(archived["status"], "archived");

    let (msg, input) = create_msg(
        "/b/products/pricing/preview",
        "",
        json!({"offer_id": offer_id, "inputs": {"pages": 4}}),
    );
    assert!(
        output_is_error(
            dispatch_user(&test_ctx, msg, input).await,
            ErrorCode::NotFound
        )
        .await
    );

    let (msg, input) = admin_get_msg(collection);
    let list = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(list["offers"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn admin_preview_accepts_admin_only_inputs_that_public_preview_rejects() {
    let test_ctx = ctx().await;
    seed_product(&test_ctx, "product_scoped_preview", "").await;
    let collection = "/admin/b/products/products/product_scoped_preview/offers";
    let (msg, input) = admin_create_msg(collection, offer_definition(25));
    let created = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let offer_id = created["offer"]["id"].as_str().unwrap().to_string();
    let detail = format!("{collection}/{offer_id}");

    let body = json!({
        "offer_id": offer_id,
        "quantity": 1,
        "inputs": {"pages": 4, "note": "internal proof batch"}
    });
    let (msg, input) = admin_create_msg(&format!("{detail}/preview"), body.clone());
    let preview = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(preview["amounts"]["total_minor"], 100);
    assert_eq!(preview["inputs"]["note"], "internal proof batch");

    let (msg, input) = admin_create_msg(&format!("{detail}/publish"), json!({}));
    let published = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(published["status"], "active");

    // The identical request is rejected on the public preview route: the
    // admin-only note is not customer-editable.
    let (msg, input) = create_msg("/b/products/pricing/preview", "", body);
    assert!(
        output_is_error(
            dispatch_user(&test_ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
}

/// Repo-level: the draft-revision fence must not break the normal editing
/// flow — `update_draft` advances the revision and settles its in-progress
/// marker, and a publish that reads the settled draft afterwards pins the
/// updated variable/component set.
#[tokio::test]
async fn draft_update_then_publish_still_succeeds_with_a_settled_revision() {
    let test_ctx = ctx().await;
    seed_product(&test_ctx, "product_fence_flow", "").await;
    let created = repo::offers::create(
        &test_ctx,
        "product_fence_flow",
        "admin_1",
        &definition_request(25),
    )
    .await
    .unwrap();
    let offer_id = created.offer.id.clone();
    let record = db::get(&test_ctx, "impresspress__products__offers", &offer_id)
        .await
        .unwrap();
    assert_eq!(record.i64_field("draft_revision"), 0);
    assert!(
        !record.bool_field("draft_updating"),
        "create must settle its child-write fence"
    );

    let updated = repo::offers::update_draft(
        &test_ctx,
        "product_fence_flow",
        &offer_id,
        &definition_request(40),
    )
    .await
    .unwrap();
    assert_eq!(updated.offer.version, 2);
    let record = db::get(&test_ctx, "impresspress__products__offers", &offer_id)
        .await
        .unwrap();
    assert_eq!(
        record.i64_field("draft_revision"),
        1,
        "update_draft advances the draft revision"
    );
    assert!(
        !record.bool_field("draft_updating"),
        "a completed update settles its fence"
    );

    let published = repo::offers::publish(&test_ctx, "product_fence_flow", &offer_id)
        .await
        .unwrap();
    assert_eq!(published.status, OfferStatus::Active);
    assert_eq!(
        serde_json::to_value(&published.offer.components[0].amount).unwrap()["unit_amount_minor"],
        40,
        "publish pins the updated child set"
    );
}

/// Repo-level: published offers are immutable — `update_draft` must fail once
/// the offer left draft, leaving the published child set untouched.
#[tokio::test]
async fn update_draft_fails_once_the_offer_is_no_longer_draft() {
    let test_ctx = ctx().await;
    seed_product(&test_ctx, "product_fence_immutable", "").await;
    let created = repo::offers::create(
        &test_ctx,
        "product_fence_immutable",
        "admin_1",
        &definition_request(25),
    )
    .await
    .unwrap();
    let offer_id = created.offer.id.clone();
    repo::offers::publish(&test_ctx, "product_fence_immutable", &offer_id)
        .await
        .unwrap();

    let error = repo::offers::update_draft(
        &test_ctx,
        "product_fence_immutable",
        &offer_id,
        &definition_request(40),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::FailedPrecondition);
    let current = repo::offers::get_managed(&test_ctx, &offer_id)
        .await
        .unwrap();
    assert_eq!(current.status, OfferStatus::Active);
    assert_eq!(current.offer.version, 1);
    assert_eq!(
        serde_json::to_value(&current.offer.components[0].amount).unwrap()["unit_amount_minor"],
        25,
        "the published child set must be untouched"
    );
}

/// Repo-level: both halves of the update/publish race must fail cleanly. A
/// publish CAS fenced on a draft revision that a concurrent update has since
/// advanced must miss, and a publish that reads a draft while an update holds
/// the fence (its children may be half-replaced) must refuse — otherwise an
/// ACTIVE offer's variables/components could be swapped after publication,
/// desynchronizing the version presets and Payment Links pinned.
#[tokio::test]
async fn publish_fails_cleanly_when_the_draft_revision_moved_after_its_read() {
    let test_ctx = ctx().await;
    seed_product(&test_ctx, "product_fence_race", "").await;
    let created = repo::offers::create(
        &test_ctx,
        "product_fence_race",
        "admin_1",
        &definition_request(25),
    )
    .await
    .unwrap();
    let offer_id = created.offer.id.clone();

    // A publish read and validated the draft at revision 0; before its CAS
    // landed, a concurrent update_draft completed and advanced the revision.
    repo::offers::update_draft(
        &test_ctx,
        "product_fence_race",
        &offer_id,
        &definition_request(40),
    )
    .await
    .unwrap();
    let landed = repo::offers::update_if_current(
        &test_ctx,
        &offer_id,
        "draft",
        Some(0),
        HashMap::from([("status".to_string(), json!("active"))]),
    )
    .await
    .unwrap();
    assert!(
        !landed,
        "a publish CAS fenced on a stale draft revision must miss"
    );
    let record = db::get(&test_ctx, "impresspress__products__offers", &offer_id)
        .await
        .unwrap();
    assert_eq!(record.data["status"], "draft");

    // A publish that reads the draft while the update fence is raised (a
    // concurrent update between its child writes, or one that crashed there)
    // must refuse rather than pin a half-replaced child set.
    db::update(
        &test_ctx,
        "impresspress__products__offers",
        &offer_id,
        HashMap::from([("draft_updating".to_string(), json!(true))]),
    )
    .await
    .unwrap();
    let error = repo::offers::publish(&test_ctx, "product_fence_race", &offer_id)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::FailedPrecondition);
    let record = db::get(&test_ctx, "impresspress__products__offers", &offer_id)
        .await
        .unwrap();
    assert_eq!(
        record.data["status"], "draft",
        "a refused publish must leave the offer a draft"
    );

    // Once the update settles its fence, publish succeeds against the
    // consistent child set it re-reads.
    db::update(
        &test_ctx,
        "impresspress__products__offers",
        &offer_id,
        HashMap::from([("draft_updating".to_string(), json!(false))]),
    )
    .await
    .unwrap();
    let published = repo::offers::publish(&test_ctx, "product_fence_race", &offer_id)
        .await
        .unwrap();
    assert_eq!(published.status, OfferStatus::Active);
    assert_eq!(
        serde_json::to_value(&published.offer.components[0].amount).unwrap()["unit_amount_minor"],
        40
    );
}

#[tokio::test]
async fn offer_writes_are_strict_and_validate_the_complete_definition() {
    let test_ctx = ctx().await;
    seed_product(&test_ctx, "product_strict", "").await;
    let path = "/admin/b/products/products/product_strict/offers";

    let mut unknown = offer_definition(25);
    unknown["client_total"] = json!(1);
    let (msg, input) = admin_create_msg(path, unknown);
    assert!(
        output_is_error(
            dispatch_admin(&test_ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );

    let mut invalid = offer_definition(25);
    invalid["pricing_model"] = json!("fixed");
    let (msg, input) = admin_create_msg(path, invalid);
    assert!(
        output_is_error(
            dispatch_admin(&test_ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
}

#[tokio::test]
async fn storefront_detail_exposes_only_active_safe_configuration() {
    let test_ctx = ctx().await;
    seed(
        &test_ctx,
        PRODUCTS_TABLE,
        "product_storefront",
        HashMap::from([
            ("name".to_string(), json!("Public print shop")),
            ("slug".to_string(), json!("public-print")),
            ("description".to_string(), json!("Made to order")),
            (
                "image_url".to_string(),
                json!("https://example.test/print.jpg"),
            ),
            ("tags".to_string(), json!(["print", "custom"])),
            ("status".to_string(), json!("active")),
            ("approval_status".to_string(), json!("approved")),
            ("owner_id".to_string(), json!("internal_owner")),
            ("stripe_product_id".to_string(), json!("prod_internal")),
        ]),
    )
    .await;
    let collection = "/admin/b/products/products/product_storefront/offers";
    let (msg, input) = admin_create_msg(collection, offer_definition(25));
    let active = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let active_id = active["offer"]["id"].as_str().unwrap().to_string();
    let (msg, input) = admin_create_msg(&format!("{collection}/{active_id}/publish"), json!({}));
    output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;

    let (msg, input) = admin_create_msg(collection, offer_definition(99));
    let draft = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(draft["status"], "draft");

    let (msg, input) = request_msg(
        "retrieve",
        "/b/products/storefront/product_storefront",
        "",
        json!({}),
    );
    let body = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["name"], "Public print shop");
    assert_eq!(body["tags"], json!(["print", "custom"]));
    assert!(body.get("owner_id").is_none());
    assert!(body.get("stripe_product_id").is_none());
    let offers = body["offers"].as_array().unwrap();
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0]["id"], active_id);
    assert_eq!(offers[0]["variables"].as_array().unwrap().len(), 1);
    assert_eq!(offers[0]["variables"][0]["key"], "pages");
    assert!(offers[0].get("components").is_none());
    assert!(offers[0].get("stripe_price_id").is_none());
    assert!(offers[0].get("sync_status").is_none());

    seed(
        &test_ctx,
        PRODUCTS_TABLE,
        "pending_storefront",
        HashMap::from([
            ("name".to_string(), json!("Pending")),
            ("status".to_string(), json!("active")),
            ("approval_status".to_string(), json!("pending")),
        ]),
    )
    .await;
    let (msg, input) = request_msg(
        "retrieve",
        "/b/products/storefront/pending_storefront",
        "",
        json!({}),
    );
    assert!(
        output_is_error(
            dispatch_user(&test_ctx, msg, input).await,
            ErrorCode::NotFound
        )
        .await
    );
}

#[tokio::test]
async fn seller_offer_routes_enforce_feature_gate_and_product_ownership() {
    let test_ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    seed_product(&test_ctx, "seller_product", "seller_a").await;

    let collection = "/b/products/products/seller_product/offers";
    let (msg, input) = create_msg(collection, "seller_a", offer_definition(15));
    let created = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    let offer_id = created["offer"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["status"], "draft");

    let detail = format!("{collection}/{offer_id}");
    let (msg, input) = request_msg("retrieve", &detail, "seller_b", json!({}));
    assert!(
        output_is_error(
            dispatch_user(&test_ctx, msg, input).await,
            ErrorCode::NotFound
        )
        .await
    );

    let (msg, input) = request_msg("retrieve", collection, "seller_a", json!({}));
    let own = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(own["offers"].as_array().unwrap().len(), 1);

    let disabled = ctx().await;
    seed_product(&disabled, "disabled_product", "seller_a").await;
    let (msg, input) = create_msg(
        "/b/products/products/disabled_product/offers",
        "seller_a",
        offer_definition(15),
    );
    assert!(
        output_is_error(
            dispatch_user(&disabled, msg, input).await,
            ErrorCode::PermissionDenied
        )
        .await
    );
}

#[tokio::test]
async fn seller_product_publication_requires_moderation_and_protects_ownership() {
    let test_ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    let (msg, input) = create_msg(
        "/b/products/products",
        "seller_a",
        json!({
            "name": "Seller print",
            "status": "active",
            "owner_id": "attacker",
            "approval_status": "approved"
        }),
    );
    let created = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    let product_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["data"]["status"], "draft");
    assert_eq!(created["data"]["approval_status"], "draft");
    assert_eq!(created["data"]["owner_kind"], "user");
    assert_eq!(created["data"]["owner_id"], "seller_a");
    assert_eq!(created["data"]["created_by"], "seller_a");

    let (msg, input) = update_msg(
        &format!("/b/products/products/{product_id}"),
        "seller_a",
        json!({
            "status": "active",
            "owner_id": "attacker",
            "created_by": "attacker",
            "approval_status": "approved"
        }),
    );
    let submitted = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(submitted["data"]["status"], "pending_review");
    assert_eq!(submitted["data"]["approval_status"], "pending");
    assert_eq!(submitted["data"]["owner_id"], "seller_a");
    assert_eq!(submitted["data"]["created_by"], "seller_a");
    assert!(submitted["data"]["submitted_at"].as_str().is_some());
}

#[tokio::test]
async fn seller_product_can_publish_directly_when_moderation_is_disabled() {
    let test_ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        (
            "IMPRESSPRESS__PRODUCTS__SELLER_MODERATION_REQUIRED",
            "false",
        ),
    ])
    .await;
    let (msg, input) = create_msg(
        "/b/products/products",
        "seller_a",
        json!({"name": "Instant product"}),
    );
    let created = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    let product_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["data"]["approval_status"], "approved");

    let (msg, input) = update_msg(
        &format!("/b/products/products/{product_id}"),
        "seller_a",
        json!({"status": "active"}),
    );
    let published = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(published["data"]["status"], "active");
    assert_eq!(published["data"]["approval_status"], "approved");
    assert!(published["data"]["published_at"].as_str().is_some());
}

#[test]
fn offer_routes_declare_admin_and_seller_auth_tiers() {
    let info = ProductsBlock::new().info();
    assert_eq!(
        crate::endpoint_match::endpoint_auth(
            &info.endpoints,
            "create",
            "/b/products/api/admin/products/product_1/offers"
        ),
        Some(AuthLevel::Admin)
    );
    assert_eq!(
        crate::endpoint_match::endpoint_auth(
            &info.endpoints,
            "create",
            "/b/products/api/products/product_1/offers"
        ),
        Some(AuthLevel::Authenticated)
    );
    assert_eq!(
        crate::endpoint_match::endpoint_auth(
            &info.endpoints,
            "create",
            "/b/products/api/admin/products/product_1/offers/offer_1/sync"
        ),
        Some(AuthLevel::Admin)
    );
    assert_eq!(
        crate::endpoint_match::endpoint_auth(
            &info.endpoints,
            "create",
            "/b/products/api/products/product_1/offers/offer_1/sync"
        ),
        Some(AuthLevel::Authenticated)
    );
    assert_eq!(
        crate::endpoint_match::endpoint_auth(
            &info.endpoints,
            "retrieve",
            "/b/products/storefront/product_1"
        ),
        Some(AuthLevel::Public)
    );
    assert_eq!(
        crate::endpoint_match::endpoint_auth(&info.endpoints, "create", "/b/products/checkout"),
        Some(AuthLevel::Public),
        "typed offers support guest checkout from static storefronts"
    );
}
