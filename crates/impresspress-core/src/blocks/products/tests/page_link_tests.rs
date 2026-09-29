//! Every link a products page emits must land on a declared row. The pages
//! spell their targets by hand (`format!("/b/products/api/...")`, JSON
//! config objects a page script reads, `fetch('...')` literals), and until
//! now no test compared them with the block's route table. This module
//! renders every SSR row of `routes::ROUTES` through `ProductsBlock::handle`
//! with one seed behind every per-record control, extracts each URL with the
//! method its carrier implies, and resolves it: under `/b/products/` through
//! `endpoint_match::dispatch` against `ROUTES`, under another block's prefix
//! through `endpoint_auth` against that block's declared endpoints.
//!
//! **The scan follows the page into its scripts.** Most of these `fetch(`
//! targets are written in JavaScript, and that JavaScript is no longer inside
//! the HTML: it is served from `/b/static/products-*.js`. So each page's
//! `<script src>` elements are resolved back through the asset manifest and
//! their source is scanned alongside the markup — otherwise this guard would
//! have gone quietly vacuous the day the scripts moved to files, which is the
//! failure mode the whole module exists to prevent.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use wafer_run::{Block, Message};

use super::{
    super::{
        contracts::OfferDefinitionRequest,
        offer_pricing, repo,
        routes::{Route, ROUTES},
        ProductsBlock,
    },
    harness::{ctx_with, seed, seed_pending_payment_link},
};
use crate::{
    endpoint_match::{self, endpoint_auth},
    test_support::{
        admin_msg, anon_msg, auth_msg,
        htmx::{self, crawl, Fixture, Site},
    },
};

/// `(needle in the rendered HTML, action the carrier implies)`. htmx maps
/// `hx-post` to `create`, `hx-get` to `retrieve`, `hx-patch`/`hx-put` to
/// `update`, `hx-delete` to `delete`. `href` is a navigation, so `retrieve`;
/// only its `/b/products/` targets are checked (the shared shell links every
/// block). The `data-*-url` attributes are read by the product manager
/// script: `data-preview-url` is only ever POSTed, the other three are
/// fetched (and then PATCHed/DELETEd/POSTed, each of which is also a row).
/// The JSON keys are the page config objects the scripts read:
/// `product_url` is fetched, `product_collection` POSTed (wizard),
/// `action_url` POSTed (seller suspend/reactivate), `refund_url` POSTed;
/// a `null` value does not match the needle. `commercePortalRedirect(` always
/// POSTs. `fetch(` is handled by [`fetch_targets`], which reads the method
/// from the call's options.
const LINK_NEEDLES: &[(&str, &str)] = &[
    ("hx-get=\"", "retrieve"),
    ("hx-post=\"", "create"),
    ("hx-patch=\"", "update"),
    ("hx-put=\"", "update"),
    ("hx-delete=\"", "delete"),
    ("href=\"", "retrieve"),
    ("data-offer-url=\"", "retrieve"),
    ("data-preview-url=\"", "create"),
    ("data-presets-url=\"", "retrieve"),
    ("data-links-url=\"", "retrieve"),
    ("\"product_url\":\"", "retrieve"),
    ("\"product_collection\":\"", "create"),
    ("\"action_url\":\"", "create"),
    ("\"refund_url\":\"", "create"),
    ("commercePortalRedirect('", "create"),
];

/// Query string stripped and `&amp;` unescaped.
fn path_of(url: &str) -> String {
    url.replace("&amp;", "&")
        .split('?')
        .next()
        .unwrap_or("")
        .to_string()
}

/// `(action, path)` for every attribute and config needle in `html`.
fn attribute_targets(html: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (needle, action) in LINK_NEEDLES {
        let quote = needle
            .chars()
            .last()
            .expect("every needle ends with its opening quote");
        let mut rest = html;
        while let Some(pos) = rest.find(needle) {
            let after = &rest[pos + needle.len()..];
            let end = after.find(quote).expect("attribute value is terminated");
            out.push((*action, path_of(&after[..end])));
            rest = &after[end..];
        }
    }
    out
}

/// `(action, path)` for every `fetch(` call in `html`.
///
/// The URL argument is either one literal (`fetch('/b/x')`, or the
/// JSON-encoded `fetch("/b/x", ...)` the settings form emits) or a
/// concatenation. Concatenations are read left to right: an expression
/// between two literals stands in for a path segment (`'/x/' +
/// encodeURIComponent(id) + '/replay'` becomes `/x/probe/replay`); a trailing
/// expression is a query string when the literal before it does not end in
/// `/` (`'/x' + query`) and a path segment otherwise. The action is the
/// `method:` the call's options name, `retrieve` when they name none.
fn fetch_targets(html: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("fetch(") {
        rest = &rest[pos + "fetch(".len()..];
        let Some(quote) = rest.chars().next().filter(|c| *c == '\'' || *c == '"') else {
            // `fetch(url, ...)`, `fetch(path, ...)`: the URL is a variable the
            // page filled from an attribute or config key already collected
            // by `attribute_targets`.
            continue;
        };
        rest = &rest[1..];
        let mut url = String::new();
        loop {
            let end = rest.find(quote).expect("string literal is terminated");
            url.push_str(&rest[..end]);
            rest = &rest[end + 1..];
            if !rest.starts_with('+') {
                break;
            }
            // Skip the concatenated expression, balanced over parentheses.
            rest = &rest[1..];
            let mut depth = 0i32;
            let mut consumed = rest.len();
            for (i, c) in rest.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' if depth > 0 => depth -= 1,
                    '+' | ',' | ')' if depth == 0 => {
                        consumed = i;
                        break;
                    }
                    _ => {}
                }
            }
            rest = &rest[consumed..];
            let followed_by_literal = rest.starts_with('+') && rest[1..].starts_with(quote);
            if followed_by_literal || url.ends_with('/') {
                url.push_str("probe");
            }
            if followed_by_literal {
                rest = &rest[2..];
                continue;
            }
            break;
        }
        let options: String = rest
            .chars()
            .take(160)
            .filter(|c| !c.is_whitespace())
            .collect();
        let action = if options.contains("method:'POST'") || options.contains("method:\"POST\"") {
            "create"
        } else if options.contains("method:'PATCH'") || options.contains("method:'PUT'") {
            "update"
        } else if options.contains("method:'DELETE'") {
            "delete"
        } else {
            "retrieve"
        };
        out.push((action, path_of(&url)));
    }
    out
}

const PRODUCTS_PREFIX: &str = "/b/products/";
const BLOCK_PREFIX: &str = "/b/";

/// The source of every products bundle the page loaded, in the order the page
/// loaded it.
///
/// The `src` carries a content hash (`products-wizard-a1b2c3d4.js`), so the
/// manifest is what maps it back to a logical key. A `/b/static/` `src` that
/// resolves to no manifest entry is an assertion failure, not a skip, because
/// that is what a renamed or unregistered asset looks like.
///
/// Scoped to this block's own bundles. The shared chrome (`htmx.min.js`,
/// `chrome.js`, `webmcp.js`) is on every shelled page in the tree and emits
/// URLs of its own; auditing those is the shared module's business, not this
/// guard's, and folding them in here would make every block's copy of this
/// test responsible for the chrome.
#[cfg(feature = "embed-assets")]
fn script_sources(html: &str) -> Vec<(&'static str, &'static str)> {
    let owned = crate::blocks::products::assets::LOGICAL_KEYS;
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("<script src=\"") {
        rest = &rest[pos + "<script src=\"".len()..];
        let end = rest.find('"').expect("script src is terminated");
        let src = &rest[..end];
        rest = &rest[end..];
        let Some(filename) = src.strip_prefix(crate::routing::STATIC_PREFIX) else {
            continue;
        };
        let entry = crate::ui::assets::ASSETS
            .iter()
            .find(|e| e.filename == filename)
            .unwrap_or_else(|| panic!("page loads {src}, which is in no asset manifest entry"));
        if !owned.contains(&entry.logical) {
            continue;
        }
        let bytes = crate::ui::assets::bytes(entry.logical)
            .unwrap_or_else(|| panic!("no bytes embedded for {}", entry.logical));
        out.push((
            entry.logical,
            std::str::from_utf8(bytes).expect("asset source is UTF-8"),
        ));
    }
    out
}

/// The SSR rows of `ROUTES` (section E of the plan's inventory): every one
/// must be rendered by this guard.
fn is_page(route: Route) -> bool {
    matches!(
        route,
        Route::PortalHome
            | Route::MyProductsPage
            | Route::NewProductPage
            | Route::MyProductPage
            | Route::MyProductClosePage
            | Route::MyPurchasesPage
            | Route::MyPurchasePage
            | Route::SellingPage
            | Route::SellingOrdersPage
            | Route::SellingOrderPage
            | Route::AdminOverview
            | Route::AdminManagePage
            | Route::AdminNewProductPage
            | Route::AdminProductPage
            | Route::AdminProductClosePage
            | Route::AdminGroupsPage
            | Route::AdminPurchasesPage
            | Route::AdminPurchasePage
            | Route::AdminSellersPage
            | Route::AdminSellerPage
            | Route::AdminStripePage
            | Route::AdminSettingsPage
            | Route::AdminSaveSettings
    )
}

struct Seeds {
    admin_offer_id: String,
    admin_gone_offer_id: String,
    admin_gone_link_id: String,
    seller_offer_id: String,
    seller_gone_offer_id: String,
    seller_gone_link_id: String,
}

async fn seed_product(ctx: &crate::test_support::TestContext, id: &str, owner: Option<&str>) {
    let mut data = HashMap::new();
    data.insert(
        "name".to_string(),
        serde_json::json!(format!("Product {id}")),
    );
    data.insert("status".to_string(), serde_json::json!("active"));
    if let Some(owner) = owner {
        data.insert("owner_kind".to_string(), serde_json::json!("user"));
        data.insert("owner_id".to_string(), serde_json::json!(owner));
        data.insert("created_by".to_string(), serde_json::json!(owner));
        data.insert(
            "seller_account_id".to_string(),
            serde_json::json!("seller_1"),
        );
    }
    seed(ctx, "impresspress__products__products", id, data).await;
}

/// A published fixed-price offer on `product_id`, plus a pending Payment
/// Link when `with_link` is set, so the manager renders an offer card and the
/// close-only manager renders both archive and deactivate controls.
async fn publish_offer(
    ctx: &crate::test_support::TestContext,
    product_id: &str,
    created_by: &str,
    with_link: bool,
) -> (String, String) {
    let definition: OfferDefinitionRequest = serde_json::from_value(serde_json::json!({
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
    }))
    .expect("offer definition");
    let offer = repo::offers::create(ctx, product_id, created_by, &definition)
        .await
        .expect("create offer");
    let offer_id = offer.offer.id;
    repo::offers::publish(ctx, product_id, &offer_id)
        .await
        .expect("publish offer");
    if !with_link {
        return (offer_id, String::new());
    }
    let managed = repo::offers::get_managed(ctx, &offer_id)
        .await
        .expect("the offer is readable");
    let preview = offer_pricing::evaluate_offer(
        &managed.offer,
        &super::super::contracts::PricingPreviewRequest {
            offer_id: offer_id.clone(),
            quantity: 1,
            inputs: Default::default(),
        },
        offer_pricing::InputScope::Management,
    )
    .expect("price the offer");
    let link_id = seed_pending_payment_link(ctx, &offer_id, "close-me", &preview)
        .await
        .managed
        .id;
    (offer_id, link_id)
}

async fn soft_delete(ctx: &crate::test_support::TestContext, id: &str) {
    repo::products::soft_delete(ctx, id)
        .await
        .expect("soft delete");
}

/// One row behind every per-record control the pages render: a live and a
/// deleted platform product (admin manager, admin close manager, admin
/// Deleted view), a live and a deleted product of `seller_a` (the seller
/// mirrors), an active seller account for `seller_a` (admin seller detail's
/// suspend action, the seller order page), a completed order sold by that
/// account to `user_1` (admin, seller and buyer order pages), and a group.
async fn seeded_ctx() -> (crate::test_support::TestContext, Seeds) {
    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;

    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_1",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_a")),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_seller_1"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
            ("requirements_json".to_string(), serde_json::json!("{}")),
            ("fee_basis_points".to_string(), serde_json::json!(250)),
        ]),
    )
    .await;

    seed_product(&ctx, "live", None).await;
    let (admin_offer_id, _) = publish_offer(&ctx, "live", "admin_1", false).await;
    seed_product(&ctx, "gone", None).await;
    let (admin_gone_offer_id, admin_gone_link_id) =
        publish_offer(&ctx, "gone", "admin_1", true).await;
    soft_delete(&ctx, "gone").await;

    seed_product(&ctx, "mine", Some("seller_a")).await;
    let (seller_offer_id, _) = publish_offer(&ctx, "mine", "seller_a", false).await;
    seed_product(&ctx, "mine_gone", Some("seller_a")).await;
    let (seller_gone_offer_id, seller_gone_link_id) =
        publish_offer(&ctx, "mine_gone", "seller_a", true).await;
    soft_delete(&ctx, "mine_gone").await;

    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "pur_1",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("user_1")),
            ("buyer_user_id".to_string(), serde_json::json!("user_1")),
            (
                "seller_account_id".to_string(),
                serde_json::json!("seller_1"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            ("provider".to_string(), serde_json::json!("stripe")),
            ("currency".to_string(), serde_json::json!("USD")),
            ("total_cents".to_string(), serde_json::json!(1000)),
            ("subtotal_cents".to_string(), serde_json::json!(1000)),
        ]),
    )
    .await;

    seed(
        &ctx,
        super::super::repo::groups::TABLE,
        "grp_1",
        HashMap::from([
            ("name".to_string(), serde_json::json!("Group one")),
            ("user_id".to_string(), serde_json::json!("seller_a")),
        ]),
    )
    .await;

    (
        ctx,
        Seeds {
            admin_offer_id,
            admin_gone_offer_id,
            admin_gone_link_id,
            seller_offer_id,
            seller_gone_offer_id,
            seller_gone_link_id,
        },
    )
}

/// Who renders a page.
/// Who renders `path`: the admin under `/admin`, the buyer (`user_1`) on
/// their purchase pages, the seller (`seller_a`) everywhere else.
fn viewer(action: &str, path: &str) -> Message {
    if path.starts_with("/b/products/admin") {
        admin_msg(action, path)
    } else if path.starts_with("/b/products/my-purchases") {
        auth_msg(action, path, "user_1")
    } else {
        auth_msg(action, path, "seller_a")
    }
}

/// `(path, query parameters)` of one page the crawl starts from.
type Page = (&'static str, &'static [(&'static str, &'static str)]);

/// Every `GET` row of section E of the plan's inventory, with the purchases
/// filter the admin list links to. The Deleted views are reached by
/// [`crawl`] from the Live lists' own links.
const PAGES: &[Page] = &[
    ("/b/products", &[]),
    ("/b/products/", &[]),
    ("/b/products/my-products", &[]),
    ("/b/products/my-products/new", &[]),
    ("/b/products/my-products/mine", &[]),
    ("/b/products/my-products/mine_gone/close", &[]),
    ("/b/products/my-purchases", &[]),
    ("/b/products/my-purchases/pur_1", &[]),
    ("/b/products/selling", &[]),
    ("/b/products/selling/orders", &[]),
    ("/b/products/selling/orders/pur_1", &[]),
    ("/b/products/admin", &[]),
    ("/b/products/admin/", &[]),
    ("/b/products/admin/manage", &[]),
    ("/b/products/admin/new", &[]),
    ("/b/products/admin/products/live", &[]),
    ("/b/products/admin/products/gone/close", &[]),
    ("/b/products/admin/groups", &[]),
    ("/b/products/admin/purchases", &[]),
    ("/b/products/admin/purchases", &[("status", "completed")]),
    ("/b/products/admin/purchases/pur_1", &[]),
    ("/b/products/admin/sellers", &[]),
    ("/b/products/admin/sellers/seller_1", &[]),
    ("/b/products/admin/stripe", &[]),
    ("/b/products/admin/settings", &[]),
];

#[tokio::test]
async fn every_link_a_products_page_emits_resolves_to_a_declared_row() {
    let (ctx, seeds) = seeded_ctx().await;
    let other_blocks = crate::blocks::all_block_infos();
    let fixture = Fixture {
        ctx,
        site: Site(vec![Arc::new(ProductsBlock::new()) as Arc<dyn Block>]),
        caller: viewer,
        pages: PAGES
            .iter()
            .map(|(path, query)| {
                query
                    .iter()
                    .fold(htmx::Page::at(*path), |page, (name, value)| {
                        page.with(*name, *value)
                    })
            })
            .collect(),
        probes: Vec::new(),
        operator_input: &[],
    };

    // Every GET page row is rendered at least once (the settings POST is
    // the form's save target, collected below rather than rendered).
    for row in ROUTES
        .iter()
        .filter(|row| is_page(row.handler) && row.method == wafer_run::HttpMethod::Get)
    {
        assert!(
            PAGES
                .iter()
                .any(|(path, _)| endpoint_match::match_template(row.template, path).is_some()),
            "page row {} is not rendered by this guard",
            row.template
        );
    }

    let mut collected: BTreeSet<(String, String)> = BTreeSet::new();
    let mut scanned_scripts: BTreeSet<&'static str> = BTreeSet::new();
    for view in crawl(&fixture).await {
        let page = view.page(&fixture);
        let path = &page.path;
        let html = htmx::render(&fixture, &page).await;
        let mut targets = attribute_targets(&html);
        targets.extend(fetch_targets(&html));
        #[cfg(feature = "embed-assets")]
        for (logical, source) in script_sources(&html) {
            scanned_scripts.insert(logical);
            targets.extend(attribute_targets(source));
            targets.extend(fetch_targets(source));
        }
        for (link_action, link_path) in targets {
            if link_path.starts_with(PRODUCTS_PREFIX) {
                assert!(
                    endpoint_match::dispatch(&mut anon_msg(link_action, &link_path), ROUTES)
                        .is_some(),
                    "{path} emits {link_action} {link_path}, which no products row serves"
                );
            } else if link_path.starts_with(BLOCK_PREFIX) {
                assert!(
                    other_blocks.iter().any(|info| endpoint_auth(
                        &info.endpoints,
                        link_action,
                        &link_path
                    )
                    .is_some()),
                    "{path} emits {link_action} {link_path}, which no block declares"
                );
            } else {
                // The shell's `href`s to `/`, anchors and external links are
                // not routes; anything else is a target this guard does not
                // understand.
                assert!(
                    link_path.is_empty()
                        || link_path == "/"
                        || link_path.starts_with('#')
                        || link_path.starts_with("http"),
                    "{path} emits {link_action} {link_path}: not a block path"
                );
                continue;
            }
            collected.insert((link_action.to_string(), link_path));
        }
    }

    // The scripts carry most of the `fetch(` targets, so a scan that reached
    // none of them would leave this guard passing on markup alone — the exact
    // way it would have rotted when the bundles moved out of the page. Every
    // bundle the block declares must be loaded by at least one page here; a
    // bundle no page loads is either dead or a page that forgot its script.
    #[cfg(feature = "embed-assets")]
    assert_eq!(
        scanned_scripts.iter().copied().collect::<Vec<_>>(),
        {
            let mut expected = crate::blocks::products::assets::LOGICAL_KEYS.to_vec();
            expected.sort_unstable();
            expected
        },
        "every products bundle must be loaded by a page this guard renders"
    );
    #[cfg(not(feature = "embed-assets"))]
    let _ = scanned_scripts;

    // The guard is only as good as what the pages rendered: each
    // per-record control must actually have been emitted for its seed.
    let expected = [
        // Admin Deleted view and close-only manager.
        (
            "create",
            "/b/products/api/admin/products/gone/restore".to_string(),
        ),
        (
            "retrieve",
            "/b/products/admin/products/gone/close".to_string(),
        ),
        (
            "delete",
            format!(
                "/b/products/api/admin/products/gone/offers/{}",
                seeds.admin_gone_offer_id
            ),
        ),
        (
            "delete",
            format!(
                "/b/products/api/admin/products/gone/offers/{}/payment-links/{}",
                seeds.admin_gone_offer_id, seeds.admin_gone_link_id
            ),
        ),
        // Admin product manager: config URL and the offer card's four.
        (
            "retrieve",
            "/b/products/api/admin/products/live".to_string(),
        ),
        (
            "retrieve",
            format!(
                "/b/products/api/admin/products/live/offers/{}",
                seeds.admin_offer_id
            ),
        ),
        (
            "create",
            format!(
                "/b/products/api/admin/products/live/offers/{}/preview",
                seeds.admin_offer_id
            ),
        ),
        (
            "retrieve",
            format!(
                "/b/products/api/admin/products/live/offers/{}/presets",
                seeds.admin_offer_id
            ),
        ),
        (
            "retrieve",
            format!(
                "/b/products/api/admin/products/live/offers/{}/payment-links",
                seeds.admin_offer_id
            ),
        ),
        // Seller mirrors.
        (
            "create",
            "/b/products/api/products/mine_gone/restore".to_string(),
        ),
        (
            "retrieve",
            "/b/products/my-products/mine_gone/close".to_string(),
        ),
        (
            "delete",
            format!(
                "/b/products/api/products/mine_gone/offers/{}",
                seeds.seller_gone_offer_id
            ),
        ),
        (
            "delete",
            format!(
                "/b/products/api/products/mine_gone/offers/{}/payment-links/{}",
                seeds.seller_gone_offer_id, seeds.seller_gone_link_id
            ),
        ),
        ("retrieve", "/b/products/api/products/mine".to_string()),
        (
            "create",
            format!(
                "/b/products/api/products/mine/offers/{}/preview",
                seeds.seller_offer_id
            ),
        ),
        // Wizards.
        ("create", "/b/products/api/admin/products".to_string()),
        ("create", "/b/products/api/products".to_string()),
        // Seller governance and refunds.
        (
            "create",
            "/b/products/api/admin/sellers/seller_1/suspend".to_string(),
        ),
        (
            "create",
            "/b/products/api/admin/purchases/pur_1/refund".to_string(),
        ),
        (
            "create",
            "/b/products/api/seller/orders/pur_1/refund".to_string(),
        ),
        // Stripe setup page scripts.
        (
            "retrieve",
            "/b/products/api/admin/stripe/status".to_string(),
        ),
        (
            "retrieve",
            "/b/products/api/admin/webhook-events".to_string(),
        ),
        (
            "create",
            "/b/products/api/admin/webhook-events/probe/replay".to_string(),
        ),
        (
            "retrieve",
            "/b/products/api/admin/provider-operations".to_string(),
        ),
        (
            "create",
            "/b/products/api/admin/provider-operations/reconcile".to_string(),
        ),
        // Portal scripts.
        ("create", "/b/products/api/seller/onboarding".to_string()),
        ("create", "/b/products/api/seller/dashboard".to_string()),
        ("create", "/b/products/billing-portal".to_string()),
        // Filters and the settings form.
        ("retrieve", "/b/products/admin/purchases".to_string()),
        ("create", "/b/products/admin/settings".to_string()),
    ];
    for (action, path) in expected {
        assert!(
            collected.contains(&(action.to_string(), path.clone())),
            "the pages must emit {action} {path}; collected: {collected:#?}"
        );
    }
    assert!(
        collected.len() >= 40,
        "expected at least 40 distinct links, collected {}",
        collected.len()
    );
}
