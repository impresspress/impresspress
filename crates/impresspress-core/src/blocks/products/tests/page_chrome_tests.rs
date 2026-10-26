//! The products pages' chrome: the topbar owns the page's one title and its
//! actions, the block's sections are plain links between the header and the
//! body, one list's views are filter links, and each section of a page has at
//! most one primary action.

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

/// The topbar's status and actions slots, from the first of them to the
/// header's end, or "" when the page declares neither.
fn topbar_slots(html: &str) -> &str {
    let Some((_, rest)) = html.split_once(r#"<header class="topbar">"#) else {
        return "";
    };
    let header = rest.split_once("</header>").map_or("", |(h, _)| h);
    [
        r#"<div class="topbar__status">"#,
        r#"<div class="topbar__actions">"#,
    ]
    .iter()
    .filter_map(|slot| header.find(slot))
    .min()
    .map_or("", |at| &header[at..])
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
        html.contains(r#"<nav class="filter-toggles" aria-label="Product views"><a class="btn btn--secondary filter-toggle" href="/b/products/admin/manage"><span"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<a class="btn btn--secondary filter-toggle" href="/b/products/admin/manage?view=deleted" aria-current="true">"#),
        "{html}"
    );
    // The Deleted view has no create action: nothing to create there.
    assert_eq!(topbar_slots(&html), "", "{html}");
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
/// state is the topbar's status, its lifecycle actions are the topbar's, and
/// the topbar's primary is the page's ONLY visible one (layout rule 6): the
/// details card's Save, a draft offer's Publish and the revealed pricing
/// editor's Save are secondary, so neither revealing the editor nor adding a
/// draft puts a second primary beside it. Every section is one card level.
#[tokio::test]
async fn product_detail_has_one_visible_primary() {
    use crate::test_support::dom;

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
    let actions = topbar_slots(&html);
    assert!(
        actions.starts_with(
            r#"<div class="topbar__status"><span class="badge badge-warning">Draft</span></div>"#
        ),
        "the product's state is the header's status: {actions}"
    );
    // `products-manager.js` reads the status each lifecycle action sets off
    // the button that carries it.
    assert!(
        actions.contains(r#"<button data-product-status="archived" class="btn btn--secondary" type="button" data-action="pm-set-status">Archive product</button>"#),
        "{actions}"
    );
    assert!(
        actions.contains(r#"<button data-product-status="active" class="btn btn--primary" type="button" data-action="pm-set-status">Publish product</button>"#),
        "{actions}"
    );
    let (page, dialogs) = dom::visible_primaries(&html);
    assert_eq!(
        page,
        [r#"button.btn.btn--primary "Publish product""#],
        "{html}"
    );
    // Only the shared confirm dialog, which shows no primary until it asks.
    assert_eq!(dialogs, [Vec::<String>::new()], "{dialogs:?}");
    assert!(
        body(&html).contains(r#"<section class="card"><header class="card__head"><div><h2 class="card__title">Product details</h2>"#),
        "{html}"
    );
    assert!(
        body(&html).contains(r#"<div class="products-form-actions"><button class="btn btn--secondary" type="submit">Save product details</button></div>"#),
        "{html}"
    );
    // The pricing editor is a hidden card a script reveals: revealing it
    // shows its Save, which is secondary, so the page still has one primary.
    let editor = body(&html)
        .split_once(r#"<section class="card" id="product-manager-visual-editor" hidden>"#)
        .map(|(_, rest)| rest)
        .expect("the hidden editor card");
    let editor = editor
        .split_once(r#"<template id="wizard-variable-row">"#)
        .map_or("", |(e, _)| e);
    assert!(!editor.contains("btn--primary"), "{editor}");
    assert!(
        editor.contains(r#"<button class="btn btn--secondary" id="manager-visual-save" type="button" data-action="pm-save-visual-offer">"#),
        "{editor}"
    );
    let revealed = html.replace(
        r#"id="product-manager-visual-editor" hidden"#,
        r#"id="product-manager-visual-editor""#,
    );
    assert_eq!(dom::visible_primaries(&revealed).0.len(), 1, "{revealed}");
    assert!(
        dom::card_level_inside_card(&html).is_empty(),
        "{:?}",
        dom::card_level_inside_card(&html)
    );
    assert!(!html.contains("Create a product with pricing"), "{html}");
    assert!(
        !html.contains("Back to products"),
        "the crumb is the way back: {html}"
    );

    // A draft offer: a Card the manager script selects by its data
    // attributes, its controls in its first row, every one secondary.
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
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    let offer_id = created["offer"]["id"].as_str().expect("offer id");
    let html = admin_page(&ctx, &format!("/b/products/admin/products/{id}")).await;
    assert!(
        html.contains(&format!(
            concat!(
                r#"<section data-offer-card="" data-offer-id="{offer}" "#,
                r#"data-offer-url="/b/products/api/admin/products/{id}/offers/{offer}" "#,
                r#"data-preview-url="/b/products/api/admin/products/{id}/offers/{offer}/preview" "#,
                r#"data-presets-url="/b/products/api/admin/products/{id}/offers/{offer}/presets" "#,
                r#"data-links-url="/b/products/api/admin/products/{id}/offers/{offer}/payment-links" "#,
                r#"data-currency="NZD" class="card"><header class="card__head"><div><h2 class="card__title">Draft tier</h2>"#,
                r#"<p class="card__subtitle">One-time payment · fixed pricing · NZD</p>"#,
            ),
            offer = offer_id,
            id = id,
        )),
        "{html}"
    );
    let offer = html
        .split_once("data-offer-card")
        .map_or("", |(_, offer)| offer);
    assert!(
        offer.contains(r#"class="btn btn--secondary btn--sm" type="button" data-action="pm-offer-action" data-offer-op="publish""#),
        "{offer}"
    );
    assert_eq!(dom::visible_primaries(&html).0.len(), 1, "{html}");
    assert!(
        dom::card_level_inside_card(&html).is_empty(),
        "{:?}",
        dom::card_level_inside_card(&html)
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
        html.contains(r#"class="btn btn--quiet-danger btn--sm btn--icon" type="button" aria-label="Delete Consulting""#),
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
    assert!(!html.contains("stat__value\">Unavailable"), "{html}");
    assert_eq!(
        html.matches(r#"href="/b/products/admin/settings""#).count() - 1,
        1,
        "one settings action beyond the section link: {html}"
    );
    assert!(!html.contains("Edit Stripe settings"), "{html}");
    // Not configured, so configuring is the page's primary action.
    assert!(
        topbar_slots(&html).contains(
            r#"<a class="btn btn--primary" href="/b/products/admin/settings">Configure Stripe</a>"#
        ),
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

/// Products' sub-pages follow the trail rule: `Products ›`, linking the
/// overview, above the sub-page's own name as the `h1`. The overview is
/// "Products" with no trail.
#[tokio::test]
async fn the_sub_pages_are_titled_under_the_products_trail() {
    let ctx = ctx().await;
    let overview = admin_page(&ctx, "/b/products/admin/").await;
    assert!(
        overview.contains(r#"<h1 class="topbar__title">Products</h1>"#),
        "{overview}"
    );
    assert!(!overview.contains("topbar__crumbs"), "{overview}");
    for (path, h1) in [
        ("/b/products/admin/manage", "All products"),
        ("/b/products/admin/manage?view=deleted", "All products"),
        ("/b/products/admin/groups", "Groups"),
        ("/b/products/admin/purchases", "Orders"),
        ("/b/products/admin/sellers", "Sellers"),
        ("/b/products/admin/stripe", "Stripe setup"),
        ("/b/products/admin/settings", "Settings"),
    ] {
        let html = admin_page(&ctx, path).await;
        assert!(
            html.contains(&format!(
                concat!(
                    r#"<nav class="topbar__crumbs" aria-label="Breadcrumb"><ol>"#,
                    r#"<li><a href="/b/products/admin/">Products</a></li></ol></nav>"#,
                    r#"<h1 class="topbar__title">{h1}</h1>"#
                ),
                h1 = h1
            )),
            "{path}: expected the trail Products › {h1}: {html}"
        );
    }
}

/// The overview: the three guide tiles are cards, the stat row and the guide
/// sit on the page background, and the header's "New product" is the one
/// primary (the empty catalog's prompt repeats it as a secondary).
#[tokio::test]
async fn the_overview_guide_is_three_cards_under_one_primary() {
    use crate::test_support::dom;

    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    let html = admin_page(&ctx, "/b/products/admin/").await;
    let guide = html
        .split_once(r#"<div class="products-guide">"#)
        .and_then(|(_, rest)| rest.split_once("</section>"))
        .map_or("", |(guide, _)| guide);
    assert_eq!(
        guide
            .matches(
                r#"<div class="card"><div class="card__body"><span class="products-guide__number">"#
            )
            .count(),
        3,
        "{guide}"
    );
    assert!(!html.contains("products-guide__item"), "{html}");
    let (page, _) = dom::visible_primaries(&html);
    assert_eq!(page, [r#"a.btn.btn--primary "New product""#], "{html}");
    assert!(
        dom::card_level_inside_card(&html).is_empty(),
        "{:?}",
        dom::card_level_inside_card(&html)
    );
}

/// The wizard's steps are cards a script steps through: only the first is
/// shown, and Continue is the one visible primary. The publish button is the
/// other primary, hidden until the last step, when Continue hides.
#[tokio::test]
async fn the_wizard_steps_are_cards_with_one_visible_primary() {
    use crate::test_support::dom;

    let ctx = ctx().await;
    let html = admin_page(&ctx, "/b/products/admin/new").await;
    for step in 1..=5 {
        let opening = if step == 1 {
            format!(
                r#"<section data-wizard-step="{step}" class="card"><header class="card__head">"#
            )
        } else {
            format!(
                r#"<section data-wizard-step="{step}" class="card" hidden><header class="card__head">"#
            )
        };
        assert!(html.contains(&opening), "step {step}: {html}");
    }
    let (page, dialogs) = dom::visible_primaries(&html);
    assert_eq!(page, [r#"button.btn.btn--primary "Continue""#], "{html}");
    // Only the shared confirm dialog, which shows no primary until it asks.
    assert_eq!(dialogs, [Vec::<String>::new()], "{dialogs:?}");
    assert!(
        html.contains(r#"<button data-wizard-intent="publish" class="btn btn--primary" id="wizard-publish" hidden type="button" data-action="pw-submit">Create and publish</button>"#),
        "{html}"
    );
    // Shipping is a divided part of the checkout step, not a card in it.
    assert!(
        html.contains(
            r#"<div class="details-block--divider" id="wizard-shipping-settings" hidden>"#
        ),
        "{html}"
    );
    assert!(
        dom::card_level_inside_card(&html).is_empty(),
        "{:?}",
        dom::card_level_inside_card(&html)
    );
}

/// The offer editor's rows are rendered once, as `<template>`s, and are not
/// cards; the wizard's script clones them and writes no row markup of its
/// own.
#[tokio::test]
async fn the_offer_rows_are_server_rendered_templates_not_cards() {
    let ctx = ctx().await;
    let html = admin_page(&ctx, "/b/products/admin/new").await;
    for (template, row) in [
        ("wizard-variable-row", "data-variable-row"),
        ("wizard-component-row", "data-component-row"),
    ] {
        let body = html
            .split_once(&format!(r#"<template id="{template}">"#))
            .and_then(|(_, rest)| rest.split_once("</template>"))
            .map(|(body, _)| body)
            .unwrap_or_else(|| panic!("{template}: {html}"));
        assert!(
            body.starts_with(&format!(r#"<div class="offer-row" {row}>"#)),
            "{template}: {body}"
        );
        assert!(!body.contains(r#"class="card"#), "{template}: {body}");
        assert!(
            body.contains(r#"data-action="pw-remove-row""#),
            "{template}: {body}"
        );
    }
    let js = include_str!("../assets/products-wizard.js");
    assert!(!js.contains("innerHTML"), "the script writes no row markup");
    assert!(!js.contains("card"), "the script names no card class");
    assert!(js.contains("content.firstElementChild.cloneNode(true)"));
}

/// A seller whose policy allows no built-in template has nothing to step
/// through: the first step's card says so in a one-line note (not a framed
/// empty state inside the card), and there is no Continue and no wizard
/// script to drive steps that are not there.
#[tokio::test]
async fn a_seller_with_no_template_sees_a_note_in_the_first_step() {
    use crate::test_support::dom;

    let ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        (
            "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_TEMPLATES",
            "custom_template",
        ),
    ])
    .await;
    let (mut msg, input) = get_msg("/b/products/my-products/new", "seller_1");
    msg.set_meta("http.header.accept", "text/html");
    let html = output_to_html(dispatch(&ctx, msg, input).await).await;
    assert!(
        html.contains(
            r#"<div class="card__body"><p class="empty-note">No seller templates are available."#
        ),
        "{html}"
    );
    assert!(!html.contains(r#"class="empty""#), "{html}");
    assert!(dom::visible_primaries(&html).0.is_empty(), "{html}");
    assert!(!html.contains("data-wizard-step=\"2\""), "{html}");
    assert!(!html.contains("products-wizard"), "{html}");
}

/// Groups: the editor is a `<dialog>`, so its "Save group" is the dialog's
/// own primary and opening it never shows a second primary beside the
/// topbar's "New group". Its refusals show inside it; the empty list repeats
/// "New group" as a secondary.
#[tokio::test]
async fn the_group_editor_is_a_dialog_with_its_own_primary() {
    use crate::test_support::dom;

    let ctx = ctx().await;
    let html = admin_page(&ctx, "/b/products/admin/groups").await;
    let (page, dialogs) = dom::visible_primaries(&html);
    assert_eq!(page, [r#"button.btn.btn--primary "New group""#], "{html}");
    // The editor's own primary, then the shared confirm dialog's, hidden
    // until it asks.
    assert_eq!(
        dialogs,
        [
            vec![r#"button.btn.btn--primary "Save group""#.to_string()],
            vec![]
        ],
        "{html}"
    );
    assert!(
        html.contains(
            r#"<dialog class="modal" id="group-editor" aria-labelledby="group-editor-title">"#
        ),
        "{html}"
    );
    let dialog = html
        .split_once(r#"id="group-editor""#)
        .and_then(|(_, rest)| rest.split_once("</dialog>"))
        .map_or("", |(dialog, _)| dialog);
    assert!(
        dialog.contains(r#"<p class="login-error" id="group-editor-error" role="alert""#),
        "{dialog}"
    );
    assert!(
        !html.contains(r#"class="card" id="group-editor""#),
        "{html}"
    );
    assert!(
        html.contains(r#"<div class="empty__action"><button class="btn btn--secondary" type="button" data-action="pc-new"><svg"#),
        "{html}"
    );
    assert_eq!(html.matches("New group</button>").count(), 2, "{html}");
}

/// The mocked browser specs (`products-wizard.spec.ts`,
/// `products-manager.spec.ts`) clone the offer editor's rows from a fixture
/// file rather than a served page; this keeps that file what the page
/// renders. After a deliberate markup change, write the expected text to
/// the path.
#[test]
fn the_offer_row_templates_fixture_is_what_the_page_renders() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../impresspress-web/tests/e2e/fixtures/offer-row-templates.html"
    );
    let on_disk = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        on_disk,
        format!(
            "{}\n",
            super::super::pages::offer_row_templates().into_string()
        ),
        "{path} is out of date; write the expected text to it"
    );
}

// ---------------------------------------------------------------------------
// The commerce pages: orders, sellers, Stripe, the seller dashboard
// ---------------------------------------------------------------------------

/// A page as `user` (or the admin, for `None`) asks for it in a browser.
async fn page_as(ctx: &crate::test_support::TestContext, path: &str, user: Option<&str>) -> String {
    match user {
        None => admin_page(ctx, path).await,
        Some(user) => {
            let (resource, query) = path.split_once('?').unwrap_or((path, ""));
            let (mut msg, input) = get_msg(resource, user);
            for (k, v) in query.split('&').filter_map(|pair| pair.split_once('=')) {
                msg.set_meta(format!("req.query.{k}"), v);
            }
            msg.set_meta("http.header.accept", "text/html");
            output_to_html(dispatch(ctx, msg, input).await).await
        }
    }
}

fn record(
    pairs: &[(&str, serde_json::Value)],
) -> std::collections::HashMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// The seller `seller_1` (account `seller_account_1`, onboarding) and a
/// suspended `seller_3`, a pending listing, and six orders with line items:
/// one with a subscription, a refund and a dispute, one failed seller order,
/// and the long values the narrow table layouts exist for (an unbroken
/// email, a long listing name, provider ids). Every date is fixed, so the
/// pages render the same on every run.
async fn commerce_ctx() -> crate::test_support::TestContext {
    use serde_json::json;
    let ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS", "250"),
    ])
    .await;
    let when = json!("2026-10-01T10:00:00Z");
    for (id, user, status, ready, reason) in [
        ("seller_account_1", "seller_1", "onboarding", false, ""),
        (
            "seller_account_3",
            "seller_3",
            "suspended",
            true,
            "requirements.past_due",
        ),
    ] {
        seed(&ctx, super::super::repo::seller_accounts::TABLE, id, record(&[
            ("user_id", json!(user)), ("status", json!(status)),
            ("stripe_account_id", json!(format!("acct_{id}"))),
            ("details_submitted", json!(ready)), ("charges_enabled", json!(ready)), ("payouts_enabled", json!(false)),
            ("requirements_json", json!(r#"{"currently_due":["individual.verification.document","external_account"]}"#)),
            ("fee_basis_points", json!(250)), ("dashboard_type", json!("express")),
            ("disabled_reason", json!(reason)),
            (
                "sync_error",
                json!("No such account: 'acct_1NvQh2KZp7a9XyTqLongAccountIdentifier'"),
            ),
            ("created_at", when.clone()), ("updated_at", when.clone()),
        ])).await;
    }
    // Through the repo (the table is the repo's to name), with the dates
    // given, so they stay fixed.
    super::super::repo::products::create(
        &ctx,
        record(&[
            ("id", json!("prod_pending")),
            (
                "name",
                json!("Microcrystalline-glazed ceramic mug with a very long listing title"),
            ),
            ("owner_id", json!("seller_1")),
            ("created_by", json!("seller_1")),
            ("owner_type", json!("user")),
            ("status", json!("draft")),
            ("approval_status", json!("pending")),
            ("submitted_at", when.clone()),
            ("currency", json!("NZD")),
            ("slug", json!("prod-pending")),
            ("created_at", when.clone()),
            ("updated_at", when.clone()),
        ]),
    )
    .await
    .expect("seed the pending listing");
    for (i, status) in [
        "completed",
        "partially_refunded",
        "refunded",
        "failed",
        "pending",
        "completed",
    ]
    .iter()
    .enumerate()
    {
        let id = format!("ord_{i}_8f2a6c1e9b7d4e0f");
        super::super::repo::purchases::create(
            &ctx,
            record(&[
                ("id", json!(&id)),
                ("user_id", json!("buyer_1")),
                ("buyer_user_id", json!("buyer_1")),
                (
                    "buyer_email",
                    json!(if i == 1 {
                        "someone.with.a.really.long.address@acme.co.nz"
                    } else {
                        "ana@example.com"
                    }),
                ),
                ("status", json!(status)),
                ("currency", json!(if i == 5 { "JPY" } else { "NZD" })),
                // Large totals, the failed seller order's largest, so the order
                // and failure tables need their narrow layouts at 721px in
                // any font.
                (
                    "total_cents",
                    json!(match i {
                        3 => 12_345_678_901_i64,
                        // JPY has no minor unit: the same figure.
                        5 => 1_234_567,
                        _ => 123_456_789,
                    }),
                ),
                ("subtotal_cents", json!(1_200_000)),
                ("tax_cents", json!(34_567)),
                (
                    "refunded_total_cents",
                    json!(match i {
                        1 => 200_000,
                        2 => 123_456_789,
                        _ => 0,
                    }),
                ),
                ("platform_fee_cents", json!(3000)),
                (
                    "seller_account_id",
                    json!(if i % 2 == 0 || i == 3 {
                        "seller_account_1"
                    } else {
                        ""
                    }),
                ),
                ("provider", json!("stripe")),
                ("checkout_mode", json!("hosted")),
                (
                    "stripe_subscription_id",
                    json!(if i == 0 {
                        "sub_1NvQh2KZp7a9XyTqAbCdEfGh"
                    } else {
                        ""
                    }),
                ),
                (
                    "subscription_status",
                    json!(if i == 0 { "active" } else { "" }),
                ),
                ("stripe_customer_id", json!("cus_1NvQh2KZp7a9XyTq")),
                ("reconciliation_status", json!("reconciled")),
                // The failed order's error quotes an id, as Stripe's do.
                (
                    "reconciliation_error",
                    json!(if i == 3 {
                        "No such customer: 'cus_1NvQh2KZp7a9XyTqAbCdEfGh'"
                    } else {
                        ""
                    }),
                ),
                ("provider_payment_status", json!("succeeded")),
                (
                    "stripe_payment_intent_id",
                    json!("pi_3NvQh2KZp7a9XyTq1abcdef"),
                ),
                (
                    "provider_session_id",
                    json!("cs_test_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0"),
                ),
                ("created_at", when.clone()),
                ("updated_at", when.clone()),
                ("payment_at", when.clone()),
            ]),
        )
        .await
        .expect("seed an order");
        super::super::repo::purchases::add_line_item(
            &ctx,
            record(&[
                ("id", json!(format!("li_{i}"))),
                ("purchase_id", json!(id)),
                ("product_id", json!("prod_x")),
                (
                    "product_name",
                    json!("Annual team plan with priority support"),
                ),
                ("quantity", json!(3)),
                ("unit_amount_minor", json!(411_522)),
                ("total_minor", json!(1_234_567)),
                (
                    "input_snapshot",
                    json!(r#"{"seats":12,"region":"ap-southeast-2"}"#),
                ),
                ("created_at", when.clone()),
                ("updated_at", when.clone()),
            ]),
        )
        .await
        .expect("seed a line item");
    }
    seed(
        &ctx,
        super::super::repo::refunds::TABLE,
        "ref_1",
        record(&[
            ("purchase_id", json!(COMMERCE_ORDER)),
            ("status", json!("succeeded")),
            ("amount_minor", json!(200_000)),
            ("provider_refund_id", json!("re_3NvQh2KZp7a9XyTq0abcdef")),
            (
                "note",
                json!("Customer asked to downgrade two seats after the first invoice"),
            ),
            ("payment_intent_id", json!("pi_3NvQh2KZp7a9XyTq1abcdef")),
            ("idempotency_key", json!("ui_0_200000")),
            ("target_refunded_total_minor", json!(200_000)),
            ("currency", json!("NZD")),
            ("created_at", when.clone()),
            ("updated_at", when.clone()),
        ]),
    )
    .await;
    seed(
        &ctx,
        super::super::repo::disputes::TABLE,
        "dp_1",
        record(&[
            ("purchase_id", json!(COMMERCE_ORDER)),
            ("seller_account_id", json!("seller_account_1")),
            ("stripe_account_id", json!("acct_seller_account_1")),
            ("provider_dispute_id", json!("dp_1NvQh2KZp7a9XyTq")),
            ("payment_intent_id", json!("pi_3NvQh2KZp7a9XyTq1abcdef")),
            ("status", json!("needs_response")),
            ("amount_minor", json!(50_000)),
            ("currency", json!("NZD")),
            ("reason", json!("product_not_received")),
            ("evidence_due_by", json!("2026-11-18T03:33:20+00:00")),
            ("created_at", when.clone()),
            ("updated_at", when.clone()),
        ]),
    )
    .await;
    // A second dispute with no evidence deadline, a large amount and a
    // long dispute id.
    seed(
        &ctx,
        super::super::repo::disputes::TABLE,
        "dp_2",
        record(&[
            ("purchase_id", json!(COMMERCE_ORDER)),
            ("seller_account_id", json!("seller_account_1")),
            ("stripe_account_id", json!("acct_seller_account_1")),
            (
                "provider_dispute_id",
                json!("du_1NvQh2KZp7a9XyTqAbCdEfGhIjKl"),
            ),
            ("payment_intent_id", json!("pi_3NvQh2KZp7a9XyTq1abcdef")),
            ("status", json!("warning_needs_response")),
            ("amount_minor", json!(123_456_789)),
            ("currency", json!("NZD")),
            ("reason", json!("incorrect_account_details")),
            ("created_at", when.clone()),
            ("updated_at", when.clone()),
        ]),
    )
    .await;
    ctx
}

/// The order [`commerce_ctx`] gives a subscription, a refund and a dispute.
const COMMERCE_ORDER: &str = "ord_0_8f2a6c1e9b7d4e0f";

/// Each commerce page, and who views it (`None` is the admin).
const COMMERCE_PAGES: &[(&str, &str, Option<&str>)] = &[
    ("overview", "/b/products/admin/", None),
    ("orders", "/b/products/admin/purchases", None),
    ("sellers", "/b/products/admin/sellers", None),
    ("seller", "/b/products/admin/sellers/seller_account_1", None),
    (
        "order",
        "/b/products/admin/purchases/ord_0_8f2a6c1e9b7d4e0f",
        None,
    ),
    ("commerce", "/b/products/", Some("seller_1")),
    ("purchases", "/b/products/my-purchases", Some("buyer_1")),
    (
        "purchase",
        "/b/products/my-purchases/ord_0_8f2a6c1e9b7d4e0f",
        Some("buyer_1"),
    ),
    ("selling", "/b/products/selling", Some("seller_1")),
    (
        "seller-orders",
        "/b/products/selling/orders",
        Some("seller_1"),
    ),
];

/// Every commerce page shows at most one visible primary, holds no
/// card-level surface inside a card (the analytics stat rows and the seller
/// account's stats sit on the page background), and its headings descend
/// from the one h1 without a jump.
#[tokio::test]
async fn the_commerce_pages_have_one_primary_and_no_card_inside_a_card() {
    use crate::test_support::dom;

    let ctx = commerce_ctx().await;
    let mut pages: Vec<(&str, String)> = Vec::new();
    for (name, path, user) in COMMERCE_PAGES {
        pages.push((name, page_as(&ctx, path, *user).await));
    }
    pages.push(("stripe", admin_page(&ctx, "/b/products/admin/stripe").await));
    pages.push((
        "suspended",
        admin_page(&ctx, "/b/products/admin/sellers/seller_account_3").await,
    ));
    pages.push((
        "seller-order",
        page_as(
            &ctx,
            &format!("/b/products/selling/orders/{COMMERCE_ORDER}"),
            Some("seller_1"),
        )
        .await,
    ));
    pages.push((
        "no-account",
        page_as(&ctx, "/b/products/selling/orders", Some("buyer_1")).await,
    ));
    for (name, html) in &pages {
        let primaries = dom::visible_primaries(html).0;
        let expected: &[&str] = match *name {
            "commerce" => &[r#"a.btn.btn--primary "View purchases""#],
            "selling" => &[r#"button.btn.btn--primary "Continue Stripe setup""#],
            "purchase" => &[r#"button.btn.btn--primary "Manage subscription and billing""#],
            "suspended" => &[r#"button.btn.btn--primary "Reactivate seller""#],
            "stripe" => &[r#"a.btn.btn--primary "Configure Stripe""#],
            "no-account" => &[r#"a.btn.btn--primary "Open seller setup""#],
            "overview" => &[r#"a.btn.btn--primary "New product""#],
            _ => &[],
        };
        assert_eq!(primaries, expected, "{name}: {html}");
        assert!(
            dom::card_level_inside_card(html).is_empty(),
            "{name}: {:?}",
            dom::card_level_inside_card(html)
        );
        let outline = dom::heading_outline(html);
        assert_eq!(
            outline.iter().filter(|(level, _)| *level == 1).count(),
            1,
            "{name}: {outline:?}"
        );
        assert!(
            outline.windows(2).all(|pair| pair[1].0 <= pair[0].0 + 1),
            "{name}: {outline:?}"
        );
    }
}

/// The seller's admin page: the account's state is the header's status and
/// its one control the header's action, a secondary "Suspend seller" for an
/// active seller and the page's primary "Reactivate seller" for a suspended
/// one; the four stats sit on the page background, above the account card.
#[tokio::test]
async fn the_seller_page_puts_its_state_and_control_in_the_header() {
    let ctx = commerce_ctx().await;
    let active = admin_page(&ctx, "/b/products/admin/sellers/seller_account_1").await;
    assert!(
        topbar_slots(&active).contains(r#"<span class="badge badge-info">Onboarding</span>"#),
        "{active}"
    );
    assert!(
        topbar_slots(&active).contains(r#"<button data-seller-action="suspend" class="btn btn--secondary" type="button" data-action="psa-set-state">Suspend seller</button>"#),
        "{active}"
    );
    let body = body(&active);
    let stats = body.find(r#"<div class="stat-row">"#).expect("stat row");
    let card = body
        .find(r#"<h2 class="card__title">Seller account</h2>"#)
        .expect("account card");
    assert!(stats < card, "{body}");
    let suspended = admin_page(&ctx, "/b/products/admin/sellers/seller_account_3").await;
    assert!(
        topbar_slots(&suspended).contains(r#"<button data-seller-action="reactivate" class="btn btn--primary" type="button" data-action="psa-set-state">Reactivate seller</button>"#),
        "{suspended}"
    );
}

/// The body of each commerce page as `admin-core-widths.spec.ts` mounts it
/// in a real page to lay its tables out at nine widths: `main#content`'s
/// contents, scripts removed (their URLs carry a content hash, and a mounted
/// script would not run anyway), one `<!-- page: name -->` line before each.
pub(super) async fn commerce_fixture() -> String {
    let ctx = commerce_ctx().await;
    let mut out = String::new();
    for (name, path, user) in COMMERCE_PAGES {
        let html = page_as(&ctx, path, *user).await;
        let main = html
            .split_once(r#"<main class="shell__body" id="content" tabindex="0">"#)
            .and_then(|(_, rest)| rest.split_once("</main>"))
            .map(|(main, _)| main)
            .unwrap_or_else(|| panic!("{name}: no main#content\n{html}"));
        let mut body = String::new();
        let mut rest = main;
        while let Some(start) = rest.find("<script") {
            body.push_str(&rest[..start]);
            let end =
                rest[start..].find("</script>").expect("closed script") + start + "</script>".len();
            rest = &rest[end..];
        }
        body.push_str(rest);
        out.push_str(&format!("<!-- page: {name} -->\n{body}\n"));
    }
    out
}

/// `admin-core-widths.spec.ts` reads the commerce pages from a fixture file
/// (the visual server has no orders, sellers or disputes, and writes nothing);
/// this keeps that file what the pages render. To refresh it after a
/// deliberate markup change, write `commerce_fixture()` to the path below.
#[tokio::test]
async fn the_commerce_fixture_is_what_the_pages_render() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../impresspress-web/tests/e2e/fixtures/commerce-pages.html"
    );
    let expected = commerce_fixture().await;
    let on_disk = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        on_disk, expected,
        "{path} is out of date; write the expected text to it"
    );
}

/// The deleted product `gone`, with one published offer and one active
/// Payment Link that never reached Stripe: the close page's two actions,
/// Archive offer and Deactivate. Returns the context and
/// the offer and link ids, which are generated.
async fn close_ctx() -> (crate::test_support::TestContext, String, String) {
    use serde_json::json;

    use super::super::{contracts, offer_pricing, repo};

    let ctx = ctx().await;
    repo::products::create(
        &ctx,
        record(&[
            ("id", json!("gone")),
            ("name", json!("Field guide")),
            ("status", json!("active")),
        ]),
    )
    .await
    .expect("seed the product");
    let definition: contracts::OfferDefinitionRequest = serde_json::from_value(json!({
        "name": "Download",
        "mode": "payment",
        "currency": "nzd",
        "pricing_model": "fixed",
        "usage_type": "licensed",
        "billing_scheme": "per_unit",
        "tax_behavior": "exclusive",
        "components": [{
            "key": "price",
            "label": "Download",
            "required": true,
            "amount": {"type": "fixed", "unit_amount_minor": 4900}
        }]
    }))
    .expect("offer definition");
    let offer = repo::offers::create(&ctx, "gone", "admin_1", &definition)
        .await
        .expect("create offer");
    let offer_id = offer.offer.id;
    repo::offers::publish(&ctx, "gone", &offer_id)
        .await
        .expect("publish offer");
    let managed = repo::offers::get_managed(&ctx, &offer_id)
        .await
        .expect("the offer is readable");
    let preview = offer_pricing::evaluate_offer(
        &managed.offer,
        &contracts::PricingPreviewRequest {
            offer_id: offer_id.clone(),
            quantity: 1,
            inputs: Default::default(),
        },
        offer_pricing::InputScope::Management,
    )
    .expect("price the offer");
    let link_id = seed_pending_payment_link(&ctx, &offer_id, "close-me", &preview)
        .await
        .managed
        .id;
    // Through the door a delete takes. It stamps the time now, which the
    // fixture writes as `DELETED_AT`.
    repo::products::soft_delete(&ctx, "gone")
        .await
        .expect("soft delete");
    (ctx, offer_id, link_id)
}

/// `main#content`'s contents, with every `<script src>` dropped (their URLs
/// carry a content hash, and the spec loads the file itself) and every
/// inline script kept (the page's own config).
fn main_without_script_files(name: &str, html: &str) -> String {
    let main = html
        .split_once(r#"<main class="shell__body" id="content" tabindex="0">"#)
        .and_then(|(_, rest)| rest.split_once("</main>"))
        .map(|(main, _)| main)
        .unwrap_or_else(|| panic!("{name}: no main#content\n{html}"));
    let mut body = String::new();
    let mut rest = main;
    while let Some(start) = rest.find("<script src=") {
        body.push_str(&rest[..start]);
        let end =
            rest[start..].find("</script>").expect("closed script") + start + "</script>".len();
        rest = &rest[end..];
    }
    body.push_str(rest);
    body
}

/// How the fixture writes the close page's "Deleted …" time, which
/// `soft_delete` stamps with the time of the run.
const DELETED_AT: &str = r#"Deleted <time class="datetime" datetime="2026-09-01T00:00:00.000Z" title="2026-09-01T00:00:00.000Z">2026-09-01 00:00 UTC</time>"#;

/// `page` with its one `Deleted <time …>…</time>` written as [`DELETED_AT`].
fn with_fixed_deletion_time(page: &str) -> String {
    let start = page
        .find("Deleted <time")
        .unwrap_or_else(|| panic!("the close page states when it was deleted: {page}"));
    let end = page[start..].find("</time>").expect("a closed time") + start + "</time>".len();
    format!("{}{DELETED_AT}{}", &page[..start], &page[end..])
}

/// The three products actions that confirm in the shared dialog, as
/// `products-confirms.spec.ts` mounts them with their own scripts: the admin
/// order page of a partly refunded order (Create refund, whose question
/// states the amount, or the remaining balance for a blank one), and a
/// deleted product's close page (Archive offer and Deactivate). The close
/// page's generated ids are written `offer_1` and `link_1`, and its deletion
/// time [`DELETED_AT`].
pub(super) async fn product_confirms_fixture() -> String {
    let commerce = commerce_ctx().await;
    let order = page_as(
        &commerce,
        "/b/products/admin/purchases/ord_1_8f2a6c1e9b7d4e0f",
        None,
    )
    .await;
    let (close, offer_id, link_id) = close_ctx().await;
    let close_page = admin_page(&close, "/b/products/admin/products/gone/close").await;
    let close_page = with_fixed_deletion_time(
        &close_page
            .replace(&offer_id, "offer_1")
            .replace(&link_id, "link_1"),
    );
    let mut out = String::new();
    for (name, html) in [("refund", order), ("close", close_page)] {
        out.push_str(&format!(
            "<!-- page: {name} -->\n{}\n",
            main_without_script_files(name, &html)
        ));
    }
    out
}

/// `products-confirms.spec.ts` reads the two pages from a fixture file (the
/// e2e server has no orders and no Stripe to publish an offer through); this
/// keeps that file what the pages render. To refresh it after a deliberate
/// markup change, write `product_confirms_fixture()` to the path below.
#[tokio::test]
async fn the_product_confirms_fixture_is_what_the_pages_render() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../impresspress-web/tests/e2e/fixtures/product-confirms.html"
    );
    let expected = product_confirms_fixture().await;
    let on_disk = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        on_disk, expected,
        "{path} is out of date; write the expected text to it"
    );
}

/// An order page's header marks the payment mode as a neutral badge in
/// words ("Test mode"), not the raw `test` as a status badge; a dispute's
/// reason reads as a phrase; and Commerce's callout no longer repeats the
/// page's one way to the purchases.
#[tokio::test]
async fn the_order_header_names_the_mode_and_a_dispute_reason_reads_as_words() {
    let ctx = commerce_ctx().await;
    let order = admin_page(
        &ctx,
        &format!("/b/products/admin/purchases/{COMMERCE_ORDER}"),
    )
    .await;
    assert!(
        topbar_slots(&order).contains(r#"<span class="badge badge-secondary">Test mode</span>"#),
        "{order}"
    );
    let body = body(&order);
    assert!(
        body.contains(r#"<span class="text-sm">Product not received</span>"#),
        "{body}"
    );
    assert!(!body.contains("product_not_received"), "{body}");
    // No deadline: the line under the amount is the dispute id alone.
    assert!(
        body.contains(r#"<span class="commerce-table__meta"><code class="cell-wrap">du_1NvQh2KZp7a9XyTqAbCdEfGhIjKl</code></span>"#),
        "{body}"
    );
    let commerce = page_as(&ctx, "/b/products/", Some("seller_1")).await;
    assert!(!commerce.contains("View order history"), "{commerce}");
    assert_eq!(
        commerce
            .matches(r#"href="/b/products/my-purchases""#)
            .count(),
        2,
        "the Purchases section link and the one primary: {commerce}"
    );
}
