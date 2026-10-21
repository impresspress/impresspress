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
        html.contains(r#"<nav class="filter-toggles" aria-label="Product views"><a class="btn btn--secondary filter-toggle" href="/b/products/admin/manage"><span"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<a class="btn btn--secondary filter-toggle" href="/b/products/admin/manage?view=deleted" aria-current="true">"#),
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
    let actions = topbar_actions(&html);
    assert!(
        actions.starts_with(r#"<span class="badge badge-warning">draft</span>"#),
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
    assert!(dialogs.is_empty(), "{dialogs:?}");
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
    assert!(!html.contains("stat__value\">Unavailable"), "{html}");
    assert_eq!(
        html.matches(r#"href="/b/products/admin/settings""#).count() - 1,
        1,
        "one settings action beyond the section link: {html}"
    );
    assert!(!html.contains("Edit Stripe settings"), "{html}");
    // Not configured, so configuring is the page's primary action.
    assert!(
        topbar_actions(&html).contains(
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
    assert!(dialogs.is_empty());
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
    assert_eq!(
        dialogs,
        [vec![r#"button.btn.btn--primary "Save group""#.to_string()]],
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
