//! The products pages' chrome: the topbar owns the page's one title and its
//! actions, the block's sections are plain links above the content card, one
//! list's views are filter links, and each section of a page has at most one
//! primary action.

use super::harness::*;

/// Every admin products page, with the section link that must be current.
const ADMIN_PAGES: &[(&str, &str)] = &[
    ("/b/products/admin/", "Overview"),
    ("/b/products/admin/manage", "Products"),
    ("/b/products/admin/manage?view=deleted", "Products"),
    ("/b/products/admin/new", "Products"),
    ("/b/products/admin/groups", "Groups"),
    ("/b/products/admin/purchases", "Orders"),
    ("/b/products/admin/sellers", "Sellers"),
    ("/b/products/admin/stripe", "Stripe"),
    ("/b/products/admin/settings", "Settings"),
];

/// An admin page as a browser asks for it: the full document.
async fn admin_page(ctx: &crate::test_support::TestContext, path: &str) -> String {
    let (resource, query) = path.split_once('?').unwrap_or((path, ""));
    let (mut msg, input) = admin_get_msg(resource);
    for (k, v) in query.split('&').filter_map(|pair| pair.split_once('=')) {
        msg.set_meta(format!("req.query.{k}"), v);
    }
    msg.set_meta("http.header.accept", "text/html");
    output_to_html(dispatch(ctx, msg, input).await).await
}

/// The topbar's actions slot, or "" when the page declares none.
fn topbar_actions(html: &str) -> &str {
    html.split_once(r#"<div class="topbar__actions">"#)
        .and_then(|(_, rest)| rest.split_once("</header>"))
        .map_or("", |(actions, _)| actions)
}

/// The page body (`main#content`).
fn body(html: &str) -> &str {
    html.split_once(r#"id="content""#)
        .map_or("", |(_, rest)| rest)
}

/// The section links, as plain links, the current one marked — never the
/// htmx tab swap that left the previous page's title and actions in the
/// topbar when it replaced only the body — and the body repeats no title.
#[tokio::test]
async fn admin_pages_carry_section_links_and_one_title() {
    let ctx = ctx().await;
    for (path, section) in ADMIN_PAGES {
        let html = admin_page(&ctx, path).await;
        assert_eq!(html.matches("<h1").count(), 1, "{path}: one h1\n{html}");
        let subnav = html
            .split_once(r#"<nav class="subnav" aria-label="Products sections">"#)
            .and_then(|(_, rest)| rest.split_once("</nav>"))
            .map(|(links, _)| links);
        let Some(subnav) = subnav else {
            panic!("{path}: section links\n{html}");
        };
        assert!(
            html.contains(&format!(r#"aria-current="page">{section}</a>"#)),
            "{path}: {section} is the current section\n{html}"
        );
        assert!(
            !subnav.contains("hx-get"),
            "{path}: no section link swaps only the body\n{subnav}"
        );
        assert!(
            !body(&html).contains("page-title"),
            "{path}: no in-body page header repeating the title\n{html}"
        );
        assert!(!html.contains("products-tabs"), "{path}\n{html}");
        assert!(!html.contains("\"+ "), "{path}: no typed plus\n{html}");
    }
}

/// Active / Deleted are views of one list — filter links in the body, not a
/// second tab strip that looks like the section links.
#[tokio::test]
async fn manage_views_are_filter_links() {
    let ctx = ctx().await;
    let html = admin_page(&ctx, "/b/products/admin/manage?view=deleted").await;
    assert!(
        html.contains(r#"<nav class="filter-toggles" aria-label="Product views"><a class="btn btn--secondary btn--sm filter-toggle" href="/b/products/admin/manage"><span"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<a class="btn btn--secondary btn--sm filter-toggle" href="/b/products/admin/manage?view=deleted" aria-current="true">"#),
        "{html}"
    );
    // The Deleted view has no create action: nothing to create there.
    assert_eq!(topbar_actions(&html), "", "{html}");
}

/// A product row is one link, named after the product; the row carries no
/// repeated "open to edit" note; and its date is the shared timestamp.
#[tokio::test]
async fn manage_rows_link_by_name_with_a_timestamp() {
    let ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Widget" }),
    );
    let product = output_to_json(dispatch(&ctx, msg, input).await).await;
    let id = product["id"].as_str().expect("product id");

    let html = admin_page(&ctx, "/b/products/admin/manage").await;
    assert!(!html.contains("Open to edit"), "{html}");
    assert!(
        html.contains(r#"<td id="row-link-"#)
            && html.contains(r#"class="data-table__cell--primary""#),
        "the name is the row's title, which names its link: {html}"
    );
    assert!(
        html.contains(&format!(
            r#"<a href="/b/products/admin/products/{id}" aria-labelledby="#
        )),
        "{html}"
    );
    assert!(html.contains(r#"<time class="datetime""#), "{html}");
}

/// "Try a different search" only when a search was made; an empty catalog
/// points at the wizard instead.
#[tokio::test]
async fn manage_empty_state_depends_on_the_search() {
    let ctx = ctx().await;
    let empty = admin_page(&ctx, "/b/products/admin/manage").await;
    assert!(empty.contains("No products yet"), "{empty}");
    assert!(!empty.contains("different"), "{empty}");
    assert!(
        !empty.contains("pagination"),
        "no pagination on an empty list: {empty}"
    );

    let searched = admin_page(&ctx, "/b/products/admin/manage?search=zzz").await;
    assert!(
        searched.contains("No products match your search"),
        "{searched}"
    );
    assert!(searched.contains("Try a different name"), "{searched}");
    // The search swaps the whole body (which re-renders the box with its
    // value), never a nested copy of the page into a list container.
    assert!(searched.contains(r##"hx-target="#content""##), "{searched}");
    assert!(searched.contains(r#"value="zzz""#), "{searched}");
}

/// The wizard's current step is announced, not only coloured.
#[tokio::test]
async fn wizard_marks_the_current_step() {
    let ctx = ctx().await;
    let html = admin_page(&ctx, "/b/products/admin/new").await;
    assert_eq!(html.matches(r#"aria-current="step""#).count(), 1, "{html}");
    assert!(
        html.contains(r#"<li class="badge badge-primary badge--center" data-wizard-indicator="1" aria-current="step">"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<span class="wizard-step__label">Type</span>"#),
        "{html}"
    );
    // A detail of the products list: the trail leads back to it.
    assert!(
        html.contains(r#"<a href="/b/products/admin/manage">All products</a>"#),
        "{html}"
    );
}

/// A product page: its name is the title under an "All products" crumb, its
/// lifecycle actions are the topbar's with one primary, the details form has
/// its own one primary, a draft offer has one, and a product without pricing
/// offers no link to create a different product.
#[tokio::test]
async fn product_detail_has_one_primary_per_section() {
    let ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Widget" }),
    );
    let product = output_to_json(dispatch(&ctx, msg, input).await).await;
    let id = product["id"].as_str().expect("product id").to_string();

    let html = admin_page(&ctx, &format!("/b/products/admin/products/{id}")).await;
    assert!(
        html.contains(r#"<h1 class="topbar__title">Widget</h1>"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<a href="/b/products/admin/manage">All products</a>"#),
        "{html}"
    );
    let actions = topbar_actions(&html);
    assert_eq!(actions.matches("btn--primary").count(), 1, "{actions}");
    assert!(actions.contains("Publish product"), "{actions}");
    // The details card: everything before the (hidden) pricing editor, a
    // section with its own Save.
    let card = body(&html)
        .split_once(r#"id="product-manager-visual-editor""#)
        .map_or("", |(card, _)| card);
    assert_eq!(card.matches("btn--primary").count(), 1, "{card}");
    assert!(
        card.contains(r#"<div class="products-form-actions">"#),
        "{card}"
    );
    assert!(!html.contains("Create a product with pricing"), "{html}");
    assert!(
        !html.contains("Back to products"),
        "the crumb is the way back: {html}"
    );

    // A draft offer: Publish is its one primary; Edit visually and Save draft
    // definition are secondary.
    let (msg, input) = admin_create_msg(
        &format!("/b/products/api/admin/products/{id}/offers"),
        serde_json::json!({
            "name": "Draft tier",
            "mode": "payment",
            "currency": "NZD",
            "pricing_model": "fixed",
            "interval_count": 1,
            "usage_type": "licensed",
            "billing_scheme": "per_unit",
            "tax_behavior": "exclusive",
            "variables": [],
            "components": [{
                "key": "price", "label": "Price", "sort_order": 0, "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": 2599},
                "quantity": {"type": "fixed", "value": 1},
                "condition": {"op": "always"}
            }],
            "checkout": {}
        }),
    );
    output_to_json(dispatch(&ctx, msg, input).await).await;
    let html = admin_page(&ctx, &format!("/b/products/admin/products/{id}")).await;
    let offer = html
        .split_once("data-offer-card")
        .map_or("", |(_, offer)| offer);
    assert_eq!(offer.matches("btn--primary").count(), 1, "{offer}");
    assert!(
        offer.contains(r#"class="btn btn--primary btn--sm" type="button" data-action="pm-offer-action" data-offer-op="publish""#),
        "{offer}"
    );
}

/// Groups: an empty description is the no-value dash, and Delete is a
/// labelled danger icon set apart from Edit.
#[tokio::test]
async fn groups_rows_dash_an_empty_description_and_mark_delete() {
    let ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/groups",
        serde_json::json!({ "name": "Consulting", "description": "", "status": "active" }),
    );
    output_to_json(dispatch(&ctx, msg, input).await).await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/groups",
        serde_json::json!({ "name": "Prints", "description": "Printed goods", "status": "active" }),
    );
    output_to_json(dispatch(&ctx, msg, input).await).await;

    let html = admin_page(&ctx, "/b/products/admin/groups").await;
    assert!(
        html.contains(r#"<span class="text-muted text-sm">—</span>"#),
        "{html}"
    );
    assert!(
        html.contains(r#"class="btn btn--ghost-danger btn--sm btn--icon" type="button" aria-label="Delete Consulting""#),
        "{html}"
    );
    assert!(html.contains(r#"aria-label="Edit Consulting""#), "{html}");
}

/// A product that does not exist is the 404 page for a browser — not a 500,
/// and not the error page inside the shell, which is for a read that failed.
#[tokio::test]
async fn a_missing_product_is_the_404_page() {
    let ctx = ctx().await;
    for path in [
        "/b/products/admin/products/prod_absent",
        "/b/products/admin/products/prod_absent/close",
        "/b/products/admin/purchases/pur_absent",
        "/b/products/admin/sellers/seller_absent",
    ] {
        let (mut msg, input) = admin_get_msg(path);
        msg.set_meta("http.header.accept", "text/html");
        let parts =
            wafer_block::http_codec::collect_http_response(dispatch(&ctx, msg, input).await).await;
        let html = String::from_utf8_lossy(&parts.body);
        assert_eq!(parts.status, 404, "{path}: {html}");
        assert!(
            parts.headers.iter().any(|(k, v)| {
                k.eq_ignore_ascii_case("content-type") && v.starts_with("text/html")
            }) && html.contains("404"),
            "{path}: the styled 404 page, not JSON: {html}"
        );
    }
}

/// Sellers: the queue's count is a sentence, not "0 listing(s)".
#[tokio::test]
async fn sellers_queue_count_reads_as_a_sentence() {
    let ctx = ctx().await;
    let html = admin_page(&ctx, "/b/products/admin/sellers").await;
    assert!(
        html.contains("No listings are waiting for a decision."),
        "{html}"
    );
    assert!(!html.contains("listing(s)"), "{html}");
}

/// Stripe: capability states are badges (not a 2xl stat that overflowed on
/// a phone), the checklist's state follows its title, and the settings are
/// reached by one action.
#[tokio::test]
async fn stripe_page_states_are_badges_and_settings_have_one_way_in() {
    let ctx = ctx().await;
    let html = admin_page(&ctx, "/b/products/admin/stripe").await;
    assert!(
        html.contains(
            r#"<dt>Payments</dt><dd><span class="badge badge-warning">Unavailable</span></dd>"#
        ),
        "{html}"
    );
    assert!(!html.contains("stat-value\">Unavailable"), "{html}");
    assert_eq!(
        html.matches(r#"href="/b/products/admin/settings""#).count() - 1,
        1,
        "one settings action beyond the section link: {html}"
    );
    assert!(!html.contains("Edit Stripe settings"), "{html}");
    // Not configured, so configuring is the page's primary action.
    assert!(
        topbar_actions(&html).contains(r#"<a class="btn btn--sm btn--primary" href="/b/products/admin/settings">Configure Stripe</a>"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<li class="products-checklist__item"><div><strong>Secret key</strong>"#),
        "the title starts the row; the state follows it: {html}"
    );
}

/// A failed read is drawn in the shell, under the section links, with a link
/// back — not a bare page whose only way out is "Go home".
#[tokio::test]
async fn a_failed_admin_read_is_the_error_page_inside_the_shell() {
    let ctx = ctx().await.break_list_reads();
    let (mut msg, input) = admin_get_msg("/b/products/admin/groups");
    msg.set_meta("http.header.accept", "text/html");
    let parts =
        wafer_block::http_codec::collect_http_response(dispatch(&ctx, msg, input).await).await;
    let html = String::from_utf8_lossy(&parts.body);
    assert_eq!(parts.status, 500, "{html}");
    assert!(html.contains(r#"class="shell""#), "{html}");
    assert!(
        html.contains(r#"<nav class="subnav" aria-label="Products sections">"#),
        "{html}"
    );
    assert!(html.contains(r#"href="/b/products/admin/""#), "{html}");
    assert!(html.contains("Back to the products overview"), "{html}");
    assert!(!html.contains("Go home"), "{html}");
}

/// The signed-in user's commerce pages: one row of section links (no second
/// strip of seller buttons), the title in the topbar.
#[tokio::test]
async fn portal_pages_carry_one_row_of_section_links() {
    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    for (path, section) in [
        ("/b/products/", "Commerce"),
        ("/b/products/my-purchases", "Purchases"),
        ("/b/products/my-products", "My products"),
        ("/b/products/selling/orders", "Seller orders"),
    ] {
        let (mut msg, input) = get_msg(path, "seller_1");
        msg.set_meta("http.header.accept", "text/html");
        let html = output_to_html(dispatch(&ctx, msg, input).await).await;
        assert_eq!(html.matches("<h1").count(), 1, "{path}\n{html}");
        assert_eq!(
            html.matches("<nav class=\"subnav\"").count(),
            1,
            "{path}\n{html}"
        );
        assert!(
            html.contains(&format!(r#"aria-current="page">{section}</a>"#)),
            "{path}\n{html}"
        );
        assert!(!html.contains("Seller workspace"), "{path}\n{html}");
        assert!(!body(&html).contains("page-title"), "{path}\n{html}");
    }
}
