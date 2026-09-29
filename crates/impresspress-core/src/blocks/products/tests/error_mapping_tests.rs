//! What a failed database call answers, per file that classifies one.
//!
//! Every products handler used to pair an `ErrorCode::NotFound` arm with an
//! `err_internal` tail, so a WRAP `PermissionDenied` — a
//! [`wafer_run::ResourceGrant`] the block was deployed without, or a row
//! guard that refused — reached the client as
//! `500 Internal server error (ref: …)`. An operator could not tell it from
//! a corrupt row and a caller could not tell it from an outage.
//!
//! The tests that reach their site first drive a REAL `wrap::check_access`
//! denial: the fixture is a products deployment with the migrations applied
//! (so the tables exist and a refusal is a refusal, not a missing table)
//! whose caller identity holds no grants at all. The paired positive control
//! reads the same route with the grants in place and a row that is genuinely
//! absent, so the 404 the endpoint has always given is pinned alongside the
//! 403 that is new.
//!
//! A site behind earlier reads of other tables cannot be reached that way —
//! the first ungranted read refuses instead — so those tests refuse only the
//! one `(action, table)` the site calls, through `FailingDbOpContext`, and let
//! every earlier step of the route run for real.

use std::collections::HashMap;

use serde_json::json;
use wafer_run::{ErrorCode, OutputStream, ResourceGrant, ResourceType, WaferError};

use super::harness::{
    admin_create_msg, admin_get_msg, create_msg, ctx, ctx_with, dispatch, get_msg, output_to_json,
    seed,
};
use crate::{
    blocks::products::{handlers, repo, stripe},
    test_support::{output_http_status, FailingDbOpContext, TestContext},
};

/// A products fixture whose caller holds no WRAP grants, so every typed
/// database call the block makes is refused by the same `wrap::check_access`
/// the runtime applies.
///
/// The migrations run first (`with_products`), so the refusal is a denial
/// and not a missing table — which is the whole point: the two used to be
/// indistinguishable from outside.
async fn denied() -> TestContext {
    TestContext::with_products()
        .await
        .running_as("test/ungranted")
}

/// [`denied`] for a route that reads a setting through the config service
/// before it reaches a database call: the checkout path's Stripe settings,
/// and `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` for every seller route.
///
/// The one grant is `Config`-typed, so the settings resolve and the DATABASE
/// is still ungranted — otherwise the handler refuses at "Stripe is not
/// configured" (or "User product selling is disabled", a 403 of its own) and
/// never reaches the read under test.
async fn denied_with_config(config: &[(&str, &str)]) -> TestContext {
    let mut ctx = TestContext::with_products().await;
    for (key, value) in config {
        ctx.set_config(key, value);
    }
    ctx.add_deployment_grants(vec![
        ResourceGrant::read("*", "*").typed(ResourceType::Config)
    ]);
    ctx.running_as("test/ungranted")
}

// --- handlers/catalog.rs -------------------------------------------------

#[tokio::test]
async fn catalog_read_denial_is_403_not_500() {
    let ctx = denied().await;
    let (msg, input) = get_msg("/b/products/catalog/prod_absent", "");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        403
    );
}

#[tokio::test]
async fn catalog_read_of_a_missing_product_is_still_404() {
    let ctx = ctx().await;
    let (msg, input) = get_msg("/b/products/catalog/prod_absent", "");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        404
    );
}

// --- handlers/product.rs -------------------------------------------------

#[tokio::test]
async fn admin_product_read_denial_is_403_not_500() {
    let ctx = denied().await;
    let (msg, input) = admin_get_msg("/b/products/api/admin/products/prod_absent");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        403
    );
}

#[tokio::test]
async fn admin_product_read_of_a_missing_product_is_still_404() {
    let ctx = ctx().await;
    let (msg, input) = admin_get_msg("/b/products/api/admin/products/prod_absent");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        404
    );
}

// --- handlers/offers.rs --------------------------------------------------

#[tokio::test]
async fn offer_list_denial_is_403_not_500() {
    let ctx = denied().await;
    let (msg, input) = admin_get_msg("/b/products/api/admin/products/prod_absent/offers");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        403
    );
}

#[tokio::test]
async fn offer_list_for_a_missing_product_is_still_404() {
    let ctx = ctx().await;
    let (msg, input) = admin_get_msg("/b/products/api/admin/products/prod_absent/offers");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        404
    );
}

// --- handlers/commerce.rs ------------------------------------------------

#[tokio::test]
async fn storefront_product_denial_is_403_not_500() {
    let ctx = denied().await;
    let (msg, input) = get_msg("/b/products/storefront/prod_absent", "");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        403
    );
}

#[tokio::test]
async fn storefront_product_that_is_missing_is_still_404() {
    let ctx = ctx().await;
    let (msg, input) = get_msg("/b/products/storefront/prod_absent", "");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        404
    );
}

// --- purchase.rs ---------------------------------------------------------

#[tokio::test]
async fn purchase_read_denial_is_403_not_500() {
    let ctx = denied().await;
    let (msg, input) = get_msg("/b/products/purchases/pur_absent", "buyer_1");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        403
    );
}

#[tokio::test]
async fn purchase_read_of_a_missing_order_is_still_404() {
    let ctx = ctx().await;
    let (msg, input) = get_msg("/b/products/purchases/pur_absent", "buyer_1");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        404
    );
}

// --- pages.rs ------------------------------------------------------------

#[tokio::test]
async fn admin_product_page_denial_is_403_not_500() {
    let ctx = denied().await;
    let (msg, input) = admin_get_msg("/b/products/admin/products/prod_absent");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        403
    );
}

#[tokio::test]
async fn admin_product_page_for_a_missing_product_is_still_404() {
    let ctx = ctx().await;
    let (msg, input) = admin_get_msg("/b/products/admin/products/prod_absent");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        404
    );
}

// --- stripe.rs -----------------------------------------------------------

const STRIPE_CONFIG: &[(&str, &str)] = &[
    ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
    ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
];

#[tokio::test]
async fn checkout_offer_read_denial_is_403_not_500() {
    let ctx = denied_with_config(STRIPE_CONFIG).await;
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({"offer_id": "offer_absent"}),
    );
    assert_eq!(
        output_http_status(stripe::handle_checkout(&ctx, &msg, input).await).await,
        403
    );
}

#[tokio::test]
async fn checkout_for_a_missing_offer_is_still_404() {
    let ctx = ctx_with(STRIPE_CONFIG).await;
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({"offer_id": "offer_absent"}),
    );
    assert_eq!(
        output_http_status(stripe::handle_checkout(&ctx, &msg, input).await).await,
        404
    );
}

// --- handlers/provider.rs ------------------------------------------------

#[tokio::test]
async fn provider_error_gives_a_quota_refusal_its_429() {
    assert_eq!(
        output_http_status(handlers::provider::provider_error(
            "Could not list provider operations",
            WaferError::new(ErrorCode::ResourceExhausted, "daily read quota exhausted"),
        ))
        .await,
        429
    );
}

#[tokio::test]
async fn provider_error_keeps_its_other_classifications() {
    for (code, status) in [
        (ErrorCode::NotFound, 404),
        (ErrorCode::PermissionDenied, 403),
        (ErrorCode::InvalidArgument, 400),
        (ErrorCode::FailedPrecondition, 400),
        (ErrorCode::Internal, 500),
    ] {
        assert_eq!(
            output_http_status(handlers::provider::provider_error(
                "Could not list provider operations",
                WaferError::new(code, "cause"),
            ))
            .await,
            status,
            "{code:?}"
        );
    }
}

#[tokio::test]
async fn provider_operation_list_denial_is_403_not_500() {
    let ctx = denied().await;
    let (msg, input) = admin_get_msg("/b/products/api/admin/provider-operations");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        403
    );
}

// --- the sibling reads: a list and a create refuse the same way -----------

/// `repo::products::list_page` is told its table by the block, not by the
/// request, so its `NotFound` stays a 500 (it names no row of the caller's).
/// Its `PermissionDenied` is still a 403.
#[tokio::test]
async fn admin_product_list_denial_is_403_not_500() {
    let ctx = denied().await;
    let (msg, input) = admin_get_msg("/b/products/api/admin/products");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        403
    );
}

/// The positive control for the distinction above: an empty table is an
/// empty page, never a 404.
#[tokio::test]
async fn admin_product_list_of_an_empty_table_is_200() {
    let ctx = ctx().await;
    let (msg, input) = admin_get_msg("/b/products/api/admin/products");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        200
    );
}

/// A real row proves the denial tests above fail on the grant and not on the
/// fixture: the same read the denied context refuses answers 200 here, for a
/// product created through the block's own endpoint.
#[tokio::test]
async fn a_granted_read_of_a_present_product_is_200() {
    let ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({"name": "Present", "slug": "prod-present"}),
    );
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    let id = created["id"].as_str().expect("created product id");

    let (msg, input) = admin_get_msg(&format!("/b/products/api/admin/products/{id}"));
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        200
    );
}

// --- reads with no row of the caller's to miss ----------------------------
//
// Every read below is addressed by the block — a count, a listing, an insert
// — so a `NotFound` from it names no row of the caller's and stays a 500. A refusal is
// not: each of these sites goes through `crud::db_error_internal`, so a WRAP
// denial is the door's 403 and a quota keeps its 429. One real route per file,
// through `ProductsBlock::handle`, with the refusal injected on the one table
// the site reads so every earlier step of the route runs for real.

/// Seller routes are gated on this setting first, and the gate's refusal is a
/// 403 of its own — which is why [`assert_wrap_denial`] checks the message too.
const SELLING: &[(&str, &str)] = &[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")];

/// `inner`, with every `(action, table)` in `ops` refused with `code`.
fn refusing(
    inner: &TestContext,
    ops: Vec<(&'static str, &'static str)>,
    code: ErrorCode,
) -> FailingDbOpContext {
    FailingDbOpContext::failing_with(
        inner.clone(),
        ops,
        WaferError::new(code, "refused by the database client"),
    )
}

/// The request ended in the 403 `crud::db_error_internal` gives a WRAP
/// denial: `PermissionDenied` with the door's own "Access denied". The code
/// alone would also match a route gate's refusal, which carries its own
/// message.
async fn assert_wrap_denial(out: OutputStream) {
    use wafer_run::streams::output::TerminalNotResponse;

    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(error)) => assert_eq!(
            (error.code, error.message.as_str()),
            (ErrorCode::PermissionDenied, "Access denied"),
            "expected the database door's WRAP denial"
        ),
        Ok(_) => panic!("expected a WRAP denial, got a response"),
        Err(_) => panic!("expected a WRAP denial, got another terminal"),
    }
}

// --- handlers/stats.rs ----------------------------------------------------

#[tokio::test]
async fn admin_stats_count_refusal_keeps_its_code() {
    let ctx = ctx().await;
    for (code, status) in [
        (ErrorCode::PermissionDenied, 403),
        (ErrorCode::ResourceExhausted, 429),
        (ErrorCode::Internal, 500),
    ] {
        let failing = refusing(&ctx, vec![("database.count", repo::products::TABLE)], code);
        let (msg, input) = admin_get_msg("/b/products/api/admin/stats");
        assert_eq!(
            output_http_status(dispatch(&failing, msg, input).await).await,
            status,
            "{code:?}"
        );
    }

    // The control: the same route over the same fixture, nothing refused.
    let (msg, input) = admin_get_msg("/b/products/api/admin/stats");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        200
    );
}

#[tokio::test]
async fn seller_stats_denial_is_403_not_500() {
    let ctx = denied_with_config(SELLING).await;
    let (msg, input) = get_msg("/b/products/api/seller/stats", "maker_1");
    assert_wrap_denial(dispatch(&ctx, msg, input).await).await;
}

// --- handlers/subscription.rs ---------------------------------------------

#[tokio::test]
async fn subscription_read_denial_is_403_not_500() {
    let ctx = denied().await;
    let (msg, input) = get_msg("/b/products/subscription", "buyer_1");
    assert_wrap_denial(dispatch(&ctx, msg, input).await).await;
}

// --- handlers/group.rs ----------------------------------------------------

#[tokio::test]
async fn own_group_listings_denial_is_403_not_500() {
    let ctx = denied_with_config(SELLING).await;
    for path in ["/b/products/groups", "/b/products/group-templates"] {
        let (msg, input) = get_msg(path, "maker_1");
        assert_wrap_denial(dispatch(&ctx, msg, input).await).await;
    }
}

// --- handlers/sellers.rs --------------------------------------------------

#[tokio::test]
async fn admin_seller_detail_product_list_denial_is_403_not_500() {
    let ctx = ctx().await;
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_listed",
        HashMap::from([
            ("user_id".to_string(), json!("maker_listed")),
            ("status".to_string(), json!("active")),
            ("stripe_account_id".to_string(), json!("acct_listed")),
            ("requirements_json".to_string(), json!("{}")),
            ("fee_basis_points".to_string(), json!(250)),
        ]),
    )
    .await;
    let failing = refusing(
        &ctx,
        vec![("database.list", repo::products::TABLE)],
        ErrorCode::PermissionDenied,
    );
    let (msg, input) = admin_get_msg("/b/products/api/admin/sellers/seller_listed");
    assert_wrap_denial(dispatch(&failing, msg, input).await).await;
}

// --- handlers/commerce.rs (the storefront's offer listing) ---------------

#[tokio::test]
async fn storefront_offer_list_denial_is_403_not_500() {
    let ctx = ctx().await;
    seed(
        &ctx,
        repo::products::TABLE,
        "prod_public",
        HashMap::from([
            ("name".to_string(), json!("Public print")),
            ("status".to_string(), json!("active")),
            ("approval_status".to_string(), json!("approved")),
        ]),
    )
    .await;
    let failing = refusing(
        &ctx,
        vec![("database.list", repo::offers::TABLE)],
        ErrorCode::PermissionDenied,
    );
    let (msg, input) = get_msg("/b/products/storefront/prod_public", "");
    assert_wrap_denial(dispatch(&failing, msg, input).await).await;

    // The control: the product is visible, so the refusal above was the
    // offer read's and not the storefront's 404.
    let (msg, input) = get_msg("/b/products/storefront/prod_public", "");
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        200
    );
}

// --- handlers/product.rs (the duplicate's insert) -------------------------

#[tokio::test]
async fn product_duplicate_insert_denial_is_403_not_500() {
    let ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products",
        json!({"name": "Original", "slug": "original"}),
    );
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    let id = created["id"].as_str().expect("created product id");

    let failing = refusing(
        &ctx,
        vec![("database.create", repo::products::TABLE)],
        ErrorCode::PermissionDenied,
    );
    let (msg, input) = admin_create_msg(
        &format!("/b/products/api/admin/products/{id}/duplicate"),
        json!({}),
    );
    assert_wrap_denial(dispatch(&failing, msg, input).await).await;
}

// --- handlers/seller_policy.rs (the product cap's count) ------------------

#[tokio::test]
async fn seller_product_cap_count_denial_is_403_not_500() {
    let ctx = ctx_with(&[
        SELLING[0],
        ("IMPRESSPRESS__PRODUCTS__SELLER_MAX_PRODUCTS", "1"),
    ])
    .await;
    let failing = refusing(
        &ctx,
        vec![("database.count", repo::products::TABLE)],
        ErrorCode::PermissionDenied,
    );
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "maker_1",
        json!({
            "name": "Capped",
            "product_template_id": "simple_product",
            "currency": "USD"
        }),
    );
    assert_wrap_denial(dispatch(&failing, msg, input).await).await;
}

// --- mod.rs (the seller-suspension gate) -----------------------------------

#[tokio::test]
async fn seller_suspension_check_denial_is_403_not_500() {
    let ctx = ctx_with(SELLING).await;
    let failing = refusing(
        &ctx,
        vec![
            ("database.list", repo::seller_accounts::TABLE),
            ("database.get", repo::seller_accounts::TABLE),
        ],
        ErrorCode::PermissionDenied,
    );
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "maker_1",
        json!({"name": "Gated", "slug": "gated"}),
    );
    assert_wrap_denial(dispatch(&failing, msg, input).await).await;
}

// --- pages.rs -------------------------------------------------------------

#[tokio::test]
async fn admin_overview_count_denial_is_403_not_500() {
    let ctx = ctx().await;
    let failing = refusing(
        &ctx,
        vec![("database.count", repo::products::TABLE)],
        ErrorCode::PermissionDenied,
    );
    let (msg, input) = admin_get_msg("/b/products/admin");
    assert_wrap_denial(dispatch(&failing, msg, input).await).await;
}

#[tokio::test]
async fn portal_home_count_denial_is_403_not_500() {
    let ctx = denied().await;
    let (msg, input) = get_msg("/b/products", "buyer_1");
    assert_wrap_denial(dispatch(&ctx, msg, input).await).await;
}

/// The settings page renders every value through the config service, which
/// WRAP guards like the database. A deployment that never granted the block
/// its own settings gets the 403 page — not a 500, and not a form of
/// defaults whose Save would overwrite the stored values.
#[tokio::test]
async fn settings_page_config_denial_is_the_403_page_not_a_500() {
    let ctx = denied().await;
    let (mut msg, input) = admin_get_msg("/b/products/admin/settings");
    msg.set_meta("http.header.accept", "text/html");
    let parts =
        wafer_block::http_codec::collect_http_response(dispatch(&ctx, msg, input).await).await;
    let html = String::from_utf8_lossy(&parts.body);
    assert_eq!(parts.status, 403, "{html}");
    assert!(!html.contains("settings-form"), "{html}");
}

/// Records a miss unless `msg` answers the styled 403 page a refused read
/// gets — not a 200 page printing the refusal's own text where the table
/// would be.
async fn expect_refused_page(
    misses: &mut Vec<String>,
    ctx: &dyn wafer_run::context::Context,
    (mut msg, input): (wafer_run::Message, wafer_run::InputStream),
) {
    let path = msg.path().to_string();
    msg.set_meta("http.header.accept", "text/html");
    let parts =
        wafer_block::http_codec::collect_http_response(dispatch(ctx, msg, input).await).await;
    let html = String::from_utf8_lossy(&parts.body);
    if parts.status != 403 || !html.contains("Go home") || html.contains("refused by") {
        misses.push(format!("{path}: {} {html}", parts.status));
    }
}

#[tokio::test]
async fn refused_list_page_reads_are_the_403_page() {
    let ctx = ctx_with(SELLING).await;
    // The seller orders page lists orders only for a seller with an account.
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_acct_1",
        HashMap::from([
            ("user_id".to_string(), json!("seller_1")),
            ("status".to_string(), json!("active")),
            ("stripe_account_id".to_string(), json!("acct_seller_1")),
            ("details_submitted".to_string(), json!(true)),
            ("charges_enabled".to_string(), json!(true)),
            ("payouts_enabled".to_string(), json!(true)),
            ("requirements_json".to_string(), json!("{}")),
        ]),
    )
    .await;
    let products = || {
        refusing(
            &ctx,
            vec![("database.list", repo::products::TABLE)],
            ErrorCode::PermissionDenied,
        )
    };
    let purchases = || {
        refusing(
            &ctx,
            vec![("database.list", repo::purchases::PURCHASES_TABLE)],
            ErrorCode::PermissionDenied,
        )
    };
    let mut misses = Vec::new();

    expect_refused_page(
        &mut misses,
        &products(),
        admin_get_msg("/b/products/admin/manage"),
    )
    .await;
    expect_refused_page(
        &mut misses,
        &refusing(
            &ctx,
            vec![("database.list", repo::groups::TABLE)],
            ErrorCode::PermissionDenied,
        ),
        admin_get_msg("/b/products/admin/groups"),
    )
    .await;
    expect_refused_page(
        &mut misses,
        &purchases(),
        admin_get_msg("/b/products/admin/purchases"),
    )
    .await;
    expect_refused_page(
        &mut misses,
        &purchases(),
        get_msg("/b/products/selling/orders", "seller_1"),
    )
    .await;
    expect_refused_page(
        &mut misses,
        &products(),
        get_msg("/b/products/my-products", "seller_1"),
    )
    .await;
    expect_refused_page(
        &mut misses,
        &purchases(),
        get_msg("/b/products/my-purchases", "buyer_1"),
    )
    .await;

    assert!(
        misses.is_empty(),
        "expected the 403 page at every list page:\n{}",
        misses.join("\n")
    );
}
