//! SSR pages for the products block (admin + user views).

use std::collections::HashMap;

use maud::{html, Markup};
use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{
    contracts::{
        AmountRule, CommerceAnalytics, ManagedOffer, OfferStatus, SellerAccount,
        SellerFailureSummary, StripeConnectionState, StripeConnectionStatus, VariableDefinition,
        VariableKind,
    },
    money, repo, stripe_provider, GROUPS_TABLE, PRODUCTS_TABLE, PURCHASES_TABLE,
};

fn display_money(amount_minor: i64, currency: &str) -> String {
    let currency = money::normalize_currency(currency).unwrap_or_else(|_| currency.to_uppercase());
    match money::format_amount_minor(amount_minor, &currency) {
        Ok(amount) => format!("{amount} {currency}"),
        Err(_) => format!("{amount_minor} minor units ({currency})"),
    }
}

fn analytics_section(analytics: &[CommerceAnalytics], title: &str, seller_view: bool) -> Markup {
    html! {
        section style="margin-top:1.5rem" {
            h2 style="margin-bottom:.25rem" { (title) }
            p .text-muted .text-sm style="margin-top:0" {
                "Money is reported separately for each currency. Gross values include orders that were later refunded; after-refund sales subtract customer refunds."
                @if seller_view { " Proceeds shown here subtract recorded platform fees but are before Stripe fees, disputes, reserves, and payout adjustments; Stripe remains authoritative for available balance and payouts." }
            }
            @if analytics.is_empty() {
                (components::empty_state(icons::bar_chart(), "No sales data yet", "Completed orders and subscription activity will appear here.", None))
            } @else {
                @for currency in analytics {
                    article .card style="margin-top:1rem" {
                        header .card__head {
                            div {
                                h3 .card__title { (currency.currency) }
                                p .text-muted .text-sm style="margin:.25rem 0 0" { (currency.paid_order_count) " paid of " (currency.order_count) " checkout records" }
                            }
                            (components::status_badge(&currency.currency))
                        }
                        div .card__body {
                            div .stats-grid {
                                (components::stat_card("Gross sales", &display_money(currency.gross_volume_minor, &currency.currency), icons::dollar_sign()))
                                (components::stat_card("Customer refunds", &display_money(currency.refunded_volume_minor, &currency.currency), icons::arrow_down_left()))
                                (components::stat_card(if seller_view { "After refunds" } else { "Net sales" }, &display_money(currency.net_volume_minor, &currency.currency), icons::arrow_up_right()))
                                (components::stat_card("Platform fees", &display_money(currency.platform_fees_minor, &currency.currency), icons::dollar_sign()))
                                @if seller_view {
                                    (components::stat_card("Before Stripe fees", &display_money(currency.net_volume_minor.saturating_sub(currency.platform_fees_minor), &currency.currency), icons::arrow_up_right()))
                                }
                                (components::stat_card("Failed orders", &currency.failed_order_count.to_string(), icons::info()))
                                (components::stat_card("Past due", &currency.past_due_subscription_count.to_string(), icons::help_circle()))
                                (components::stat_card("Open disputes", &format!("{} · {}", currency.open_dispute_count, display_money(currency.open_disputed_volume_minor, &currency.currency)), icons::help_circle()))
                                (components::stat_card("Lost disputes", &format!("{} · {}", currency.lost_dispute_count, display_money(currency.lost_disputed_volume_minor, &currency.currency)), icons::arrow_down_left()))
                            }
                            p .text-muted .text-sm {
                                "Subscriptions: " (currency.active_subscription_count) " active, "
                                (currency.trialing_subscription_count) " trialing, "
                                (currency.past_due_subscription_count) " past due, "
                                (currency.canceled_subscription_count) " canceled. Refunded orders: "
                                (currency.refunded_order_count) "."
                                @if currency.open_dispute_count > 0 { " Open disputes require attention in Stripe." }
                            }
                            @if !currency.top_products.is_empty() {
                                h4 { "Top products by gross sales" }
                                @let cols = [
                                    components::TableCol { label: "Product", width: None },
                                    components::TableCol { label: "Quantity", width: None },
                                    components::TableCol { label: "Gross", width: None },
                                ];
                                @let rows: Vec<Vec<Markup>> = currency.top_products.iter().map(|product| vec![
                                    html! { span .font-medium { (product.name) } },
                                    html! { (product.quantity) },
                                    html! { (display_money(product.revenue_minor, &currency.currency)) },
                                ]).collect();
                                (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! {}))
                            }
                        }
                    }
                }
            }
        }
    }
}

fn seller_failures_section(failures: &[SellerFailureSummary]) -> Markup {
    html! {
        section style="margin-top:1.5rem" {
            h2 { "Recent payment failures" }
            p .text-muted .text-sm { "Failed seller orders that may need customer follow-up. Stripe Dashboard provides provider-level payment details." }
            @if failures.is_empty() {
                (components::empty_state(icons::info(), "No recent failures", "No failed seller orders need attention.", None))
            } @else {
                @let row_hrefs: Vec<String> = failures.iter().map(|failure| format!("/b/products/selling/orders/{}", failure.order_id)).collect();
                @let cols = [
                    components::TableCol { label: "Order", width: None },
                    components::TableCol { label: "Amount", width: None },
                    components::TableCol { label: "Last result", width: None },
                    components::TableCol { label: "Date", width: None },
                ];
                @let rows: Vec<Vec<Markup>> = failures.iter().map(|failure| vec![
                    html! { code { (&failure.order_id) } },
                    html! { (display_money(failure.total_minor, &failure.currency)) },
                    html! { span .text-sm { (if failure.error.is_empty() { "Payment did not complete" } else { &failure.error }) } },
                    html! { span .text-muted .text-sm { (failure.created_at.get(..10).unwrap_or("—")) } },
                ]).collect();
                (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! {}))
            }
        }
    }
}
use crate::{
    config_vars,
    ui::{self, components, icons, settings_form, settings_form::SettingsSection},
    util::RecordExt,
};

const PRODUCTS_UI_CSS: &str = r#"
.products-tabs{min-width:0;max-width:100%;margin:0 0 1.25rem;overflow-x:auto;overflow-y:hidden;scrollbar-width:none;-ms-overflow-style:none}
.products-tabs::-webkit-scrollbar{display:none}
.products-tabs>nav{min-width:max-content}
/* The strip above is the single horizontal scroller: the inner .tabs
   component must not become a second, nested one (its own mobile rule sets
   overflow-x:auto), and fractional-zoom rounding must never turn the strip
   into a vertical scroller that silently clips icon tops behind the hidden
   scrollbars. */
.products-tabs .tabs{overflow-x:visible}
.products-tabs+header,.products-tabs+.page-header{margin-top:0}
.products-guide{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:1rem;margin:1.25rem 0}
.products-guide__item{position:relative;padding:1.2rem 1.25rem;border:1px solid color-mix(in srgb,var(--border-color) 82%,transparent);border-radius:14px;background:linear-gradient(145deg,color-mix(in srgb,var(--surface-1) 96%,var(--primary-color) 4%),var(--surface-1));box-shadow:0 8px 26px rgba(15,23,42,.05)}
.products-guide__number{display:grid;place-items:center;width:2rem;height:2rem;margin-bottom:.8rem;border-radius:999px;background:color-mix(in srgb,var(--primary-color) 14%,var(--surface-1));color:var(--primary-color);font-weight:750}
.products-guide__item h3{font-size:1rem;margin:0 0 .35rem}
.products-guide__item p{margin:0;line-height:1.55}
.products-callout{display:flex;align-items:flex-start;justify-content:space-between;gap:1rem;padding:1rem 1.1rem;margin:0 0 1.25rem;border:1px solid color-mix(in srgb,var(--primary-color) 28%,var(--border-color));border-radius:14px;background:color-mix(in srgb,var(--primary-color) 6%,var(--surface-1))}
.products-callout__copy{max-width:68ch}
.products-callout__copy strong{display:block;margin-bottom:.2rem}
.products-callout__copy p{margin:0}
.products-callout__actions{display:flex;gap:.5rem;align-items:center;flex-wrap:wrap;flex:none}
.products-section{margin-top:1.75rem}
.products-section__head{display:flex;align-items:end;justify-content:space-between;gap:1rem;flex-wrap:wrap;margin-bottom:.75rem}
.products-section__head h2,.products-section__head h3,.products-section__head p{margin:0}
.products-section__head p{margin-top:.25rem}
.products-form-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:0 1rem}
.products-form-grid--compact{grid-template-columns:repeat(auto-fit,minmax(170px,1fr))}
.products-choice-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(230px,1fr));gap:.75rem}
.products-choice{display:flex;gap:.7rem;align-items:flex-start;padding:.8rem .9rem;border:1px solid var(--border-color);border-radius:12px;background:var(--surface-1);cursor:pointer}
.products-choice:has(input:checked){border-color:var(--primary-color);background:color-mix(in srgb,var(--primary-color) 5%,var(--surface-1));box-shadow:0 0 0 2px color-mix(in srgb,var(--primary-color) 10%,transparent)}
.products-choice input{margin-top:.2rem;flex:none}
.products-choice strong{display:block;font-size:.92rem}
.products-choice small{display:block;margin-top:.15rem;line-height:1.45}
.products-actions{display:flex;align-items:center;gap:.5rem;flex-wrap:wrap}
.products-status-stack{display:flex;align-items:center;gap:.35rem;flex-wrap:wrap}
.products-advanced{margin-top:1rem;border:1px solid var(--border-color);border-radius:12px;background:color-mix(in srgb,var(--surface-1) 97%,var(--border-color) 3%)}
.products-advanced>summary{cursor:pointer;padding:.85rem 1rem;font-weight:650;list-style-position:inside}
.products-advanced[open]>summary{border-bottom:1px solid var(--border-color)}
.products-advanced__body{padding:1rem}
.products-plain-details{margin-top:.75rem}
.products-plain-details>summary{cursor:pointer;color:var(--text-muted);font-size:.875rem;font-weight:600}
.products-filter-label{font-size:.78rem;font-weight:700;text-transform:uppercase;letter-spacing:.04em;color:var(--text-muted);margin-right:.15rem}
.products-checklist{list-style:none;padding:0;margin:0}
.products-checklist li:last-child{margin-bottom:0!important}
.products-code-block{display:block;width:100%;padding:.75rem .85rem;border:1px solid var(--border-color);border-radius:10px;background:var(--surface-2);overflow-wrap:anywhere}
.products-settings-note{margin-bottom:1rem}
body:has(.products-tabs) .shell__main,body:has(.products-tabs) .shell__body{min-width:0}
body:has(.products-tabs) .shell__body{padding:1.1rem 1.4rem}
@media(max-width:760px){body:has(.products-tabs) .shell__body{padding:.85rem .9rem}}
body:has(.products-tabs) .page-header{padding-bottom:.35rem}
body:has(.products-tabs) .stats-grid{gap:1rem;margin-top:1rem}
body:has(.products-tabs) .stat-card{border-radius:14px;box-shadow:0 8px 24px rgba(15,23,42,.045)}
body:has(.products-tabs) .card{border-radius:14px;box-shadow:0 8px 26px rgba(15,23,42,.045)}
body:has(.products-tabs) .filter-bar{margin:1rem 0;padding:.75rem;border:1px solid var(--border-color);border-radius:14px;background:var(--surface-1)}
.product-wizard-progress{margin:0 0 1.25rem}
.product-wizard-progress ol{display:grid;grid-template-columns:repeat(5,minmax(0,1fr));gap:.55rem;list-style:none;padding:0;margin:0}
.product-wizard-progress li{min-height:2.25rem;border-radius:999px}
.wizard-step-check{display:inline-flex;margin-right:.3rem}
.wizard-step-check svg{width:.75rem;height:.75rem}
.product-wizard-actions{position:sticky;bottom:0;z-index:2;display:flex;justify-content:space-between;gap:1rem;flex-wrap:wrap;margin-top:1rem;padding:.75rem 0;border-top:1px solid var(--border-color);background:var(--surface-1)}
.product-wizard-actions__buttons{display:flex;gap:.75rem;margin-left:auto}
.product-template-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:1rem}
.product-template-card{display:grid;grid-template-columns:auto 1fr;gap:.35rem .75rem;align-items:start;padding:1rem;border:1px solid var(--border-color);border-radius:14px;cursor:pointer;transition:border-color .15s ease,box-shadow .15s ease,transform .15s ease}
.product-template-card:hover{border-color:var(--primary-color);box-shadow:0 8px 22px rgba(15,23,42,.08);transform:translateY(-1px)}
.product-template-card:has(input:checked){border-color:var(--primary-color);box-shadow:0 0 0 3px color-mix(in srgb,var(--primary-color) 14%,transparent)}
.product-template-card input{margin-top:.2rem}
.product-template-card strong,.product-template-card span{grid-column:2}
.product-template-card strong{margin:0!important}
[data-wizard-step]>.card__head{background:color-mix(in srgb,var(--surface-1) 94%,var(--primary-color) 6%)}
[data-offer-card]>.card__head{align-items:flex-start;background:linear-gradient(120deg,color-mix(in srgb,var(--surface-1) 94%,var(--primary-color) 6%),var(--surface-1))}
@media(max-width:760px){
 .products-guide{grid-template-columns:1fr}
 .products-callout{flex-direction:column}
 .products-callout__actions,.products-actions{width:100%}
 .products-callout__actions .btn{flex:1}
 .product-template-grid{grid-template-columns:1fr}
 .product-wizard-progress{overflow:visible}
 .product-wizard-progress ol{min-width:0;gap:.35rem}
 .product-wizard-progress li{font-size:.72rem;white-space:nowrap}
 .product-wizard-actions{display:grid}
 .product-wizard-actions>.btn{width:100%}
 .product-wizard-actions__buttons{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));width:100%;margin-left:0}
 .product-wizard-actions__buttons:has(#wizard-next:not([hidden])){grid-template-columns:1fr}
 .product-wizard-actions__buttons .btn{width:100%}
 body:has(.products-tabs) .page-header{align-items:flex-start}
}
"#;

fn products_styles() -> Markup {
    html! { style { (maud::PreEscaped(PRODUCTS_UI_CSS)) } }
}

fn admin_tabs(active: &str) -> Markup {
    html! {
        (products_styles())
        div .products-tabs {
            (components::tab_navigation(vec![
        components::Tab {
            active: active == "overview",
            href: "/b/products/admin/",
            label: "Overview",
            icon: Some(icons::layout_dashboard()),
        },
        components::Tab {
            active: active == "products",
            href: "/b/products/admin/manage",
            label: "Products",
            icon: Some(icons::package()),
        },
        components::Tab {
            active: active == "groups",
            href: "/b/products/admin/groups",
            label: "Groups",
            icon: Some(icons::folder()),
        },
        components::Tab {
            active: active == "orders",
            href: "/b/products/admin/purchases",
            label: "Orders",
            icon: Some(icons::shopping_cart()),
        },
        components::Tab {
            active: active == "sellers",
            href: "/b/products/admin/sellers",
            label: "Sellers",
            icon: Some(icons::package()),
        },
        components::Tab {
            active: active == "stripe",
            href: "/b/products/admin/stripe",
            label: "Stripe",
            icon: Some(icons::credit_card()),
        },
        components::Tab {
            active: active == "settings",
            href: "/b/products/admin/settings",
            label: "Settings",
            icon: Some(icons::settings()),
        },
            ]))
        }
    }
}

fn portal_tabs(active: &str, seller_enabled: bool) -> Markup {
    let mut tabs = vec![
        components::Tab {
            active: active == "home",
            href: "/b/products/",
            label: "Commerce",
            icon: Some(icons::layout_dashboard()),
        },
        components::Tab {
            active: active == "purchases",
            href: "/b/products/my-purchases",
            label: "Purchases",
            icon: Some(icons::shopping_cart()),
        },
    ];
    if seller_enabled {
        tabs.push(components::Tab {
            active: active == "selling",
            href: "/b/products/selling",
            label: "Selling",
            icon: Some(icons::package()),
        });
    }
    html! {
        (products_styles())
        div .products-tabs { (components::tab_navigation(tabs)) }
    }
}

// ---------------------------------------------------------------------------
// Admin: Overview (stats)
// ---------------------------------------------------------------------------

pub async fn overview(ctx: &dyn Context, msg: &Message) -> OutputStream {
    // A repository failure on any of these must surface as an error, not be
    // fabricated into a "0" stat: besides misreporting the catalog size, a
    // false `products_count == 0` also trips `render_overview_empty_state`'s
    // "Add your first product" CTA during a real outage — actively
    // misleading, not just cosmetically wrong.
    let products_count = match db::count(ctx, PRODUCTS_TABLE, &[]).await {
        Ok(n) => n,
        Err(e) => return crate::http::err_internal("Database error", e),
    };
    let groups_count = match db::count(ctx, GROUPS_TABLE, &[]).await {
        Ok(n) => n,
        Err(e) => return crate::http::err_internal("Database error", e),
    };
    let purchases_count = match repo::purchases::count_all(ctx).await {
        Ok(n) => n,
        Err(e) => return crate::http::err_internal("Database error", e),
    };
    let offers_count = match db::count(ctx, repo::offers::TABLE, &[]).await {
        Ok(n) => n,
        Err(e) => return crate::http::err_internal("Database error", e),
    };
    let analytics = match repo::purchases::commerce_analytics(ctx, None).await {
        Ok(analytics) => analytics,
        Err(error) => return crate::http::err_internal("Database error", error),
    };
    let user_products_enabled = super::handlers::user_products_enabled(ctx).await;

    let content = html! {
        (admin_tabs("overview"))
        (components::page_header("Products", Some("Everything you need to set up your catalog and start taking payments"), Some(html! {
            a .btn .btn--primary .btn--sm href="/b/products/admin/new" { "+ Create product" }
        })))
        div .stats-grid {
            (components::stat_card("Products", &products_count.to_string(), icons::package()))
            (components::stat_card("Groups", &groups_count.to_string(), icons::folder()))
            (components::stat_card("Offers", &offers_count.to_string(), icons::dollar_sign()))
            (components::stat_card("Orders", &purchases_count.to_string(), icons::shopping_cart()))
        }
        div .products-section__head style="margin-top:1.5rem" {
            div {
                h2 { "Get selling in three steps" }
                p .text-muted .text-sm { "Start with the essentials. You can refine every setting later." }
            }
        }
        section .products-guide aria-label="Commerce setup" {
            article .products-guide__item {
                span .products-guide__number { "1" }
                h3 { "Connect Stripe" }
                p .text-muted .text-sm { "Add your Stripe keys and confirm that payments are ready." }
                a .text-sm href="/b/products/admin/stripe" { "Check Stripe setup →" }
            }
            article .products-guide__item {
                span .products-guide__number { "2" }
                h3 { "Create a product" }
                p .text-muted .text-sm { "Choose a one-time product, subscription, or configurable checkout." }
                a .text-sm href="/b/products/admin/new" { "Create a product →" }
            }
            article .products-guide__item {
                span .products-guide__number { "3" }
                h3 { "Publish and share" }
                p .text-muted .text-sm { "Publish the price, then copy a payment link or add checkout to your site." }
                a .text-sm href="/b/products/admin/manage" { "Manage products →" }
            }
        }
        (render_overview_empty_state(products_count, user_products_enabled))
        (analytics_section(&analytics, "Sales and subscriptions", false))
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Products", ui::NavKind::Admin, "Products"),
        content,
    )
    .await
}

/// Render the Products Overview empty-state guidance in place of a bare,
/// actionless stat grid. Renders empty markup once the catalog has at least
/// one product. Two mutually exclusive states while it's empty:
///
///   - `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` off: a live 403 on
///     `/b/products/api/products` (the user-owned-product route,
///     `handlers::dispatch::UserRoute::requires_user_products`) was previously the
///     only signal a new admin got that self-serve selling is disabled —
///     name the var and link to Settings, where it's the first toggle in
///     the Features section. The admin JSON create route
///     (`/b/products/api/admin/products`) is NOT gated by this flag, so no
///     CTA is withheld here — this state is purely informational.
///   - Otherwise: an "Add your first product" CTA straight to the Manage
///     Products page, which owns the "+ New Product" create modal (the
///     real create path, wired to that same admin route).
fn render_overview_empty_state(products_count: i64, user_products_enabled: bool) -> maud::Markup {
    if products_count > 0 {
        return html! {};
    }
    if user_products_enabled {
        components::empty_state(
            icons::package(),
            "Add your first product",
            "Your catalog is empty. Add a product to start selling.",
            Some(html! {
                a .btn .btn--primary .btn--md href="/b/products/admin/manage" { "+ Add product" }
            }),
        )
    } else {
        components::empty_state(
            icons::info(),
            "User products are turned off",
            "Customer accounts cannot create their own listings yet. You can still create platform products now, or enable seller products in Settings when you are ready to run a marketplace.",
            Some(html! {
                a .btn .btn--secondary .btn--md href="/b/products/admin/settings" { "Go to Settings" }
            }),
        )
    }
}

// ---------------------------------------------------------------------------
// Admin: Manage Products
// ---------------------------------------------------------------------------

pub async fn manage_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (page, page_size, _) = msg.pagination_params(20);
    let search = msg.query("search").to_string();

    let mut filters = vec![Filter {
        field: "deleted_at".into(),
        operator: FilterOp::IsNull,
        value: serde_json::Value::Null,
    }];
    if let Some(search) = super::handlers::name_like_filter(&search) {
        filters.push(search);
    }

    let sort = vec![SortField {
        field: "created_at".into(),
        desc: true,
    }];
    let result = db::paginated_list(
        ctx,
        PRODUCTS_TABLE,
        page as i64,
        page_size as i64,
        filters,
        sort,
    )
    .await;

    let new_product_button = html! {
        a .btn .btn--primary .btn--sm href="/b/products/admin/new" { "+ New Product" }
    };

    let content = html! {
        (admin_tabs("products"))
        (components::page_header("Products", Some("Create, publish, and share the things you sell"), Some(new_product_button)))

        div .filter-bar {
            (components::search_input("search", "Search by product name", "/b/products/admin/manage", "#products-content"))
        }

        div #products-content {
            @match &result {
                Ok(list) => {
                    @let row_hrefs: Vec<String> = list.records.iter().map(|record| format!("/b/products/admin/products/{}", record.id)).collect();
                    @let cols = [
                        components::TableCol { label: "Name", width: None },
                        components::TableCol { label: "Availability", width: None },
                        components::TableCol { label: "Owner", width: None },
                        components::TableCol { label: "Currency", width: None },
                        components::TableCol { label: "Updated", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|record| {
                        let updated = record.str_field("updated_at");
                        let seller_owned = record.str_field("owner_kind") == "user";
                        vec![
                            html! { div { span .font-medium { (record.str_field("name")) } br; span .text-muted .text-sm { "Open to edit pricing and checkout" } } },
                            html! { div .products-status-stack { (components::status_badge(record.str_field("status"))) @if seller_owned { (components::status_badge(record.str_field("approval_status"))) } } },
                            html! { span .text-muted .text-sm { @if seller_owned { "Seller" } @else { "Your store" } } },
                            html! { span .font-medium { (record.str_field("currency")) } },
                            html! { span .text-muted .text-sm { (updated.get(..10).unwrap_or("—")) } },
                        ]
                    }).collect();
                    (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! {
                        (components::empty_state(icons::package(), "No products found", "Try a different search, or create your first product.", Some(html! {
                            a .btn .btn--primary .btn--sm href="/b/products/admin/new" { "+ Create product" }
                        })))
                    }))
                    (components::pagination(list.page as u32, list.page_size as u32, list.total_count as u32, "/b/products/admin/manage"))
                }
                Err(e) => { div .login-error { "Error: " (e.message) } }
            }
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Products", ui::NavKind::Admin, "Products"),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Admin: seller governance and moderation
// ---------------------------------------------------------------------------

pub async fn admin_sellers(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let seller_records = match db::list_all(ctx, repo::seller_accounts::TABLE, vec![]).await {
        Ok(records) => records,
        Err(error) => return crate::http::err_internal("Could not list sellers", error),
    };
    let sellers = match seller_records
        .iter()
        .map(repo::seller_accounts::to_contract)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(sellers) => sellers,
        Err(error) => return crate::http::err_internal("Could not read seller status", error),
    };
    let seller_products = match db::list_all(
        ctx,
        PRODUCTS_TABLE,
        vec![Filter {
            field: "owner_kind".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!("user"),
        }],
    )
    .await
    {
        Ok(products) => products,
        Err(error) => return crate::http::err_internal("Could not list seller products", error),
    };
    let mut product_counts = HashMap::<String, usize>::new();
    for product in &seller_products {
        *product_counts
            .entry(product.str_field("owner_id").to_string())
            .or_default() += 1;
    }
    let pending: Vec<_> = seller_products
        .iter()
        .filter(|product| {
            product.str_field("status") == "pending_review"
                && product.str_field("approval_status") == "pending"
        })
        .collect();
    let selling_enabled = super::handlers::user_products_enabled(ctx).await;

    let content = html! {
        (admin_tabs("sellers"))
        (components::page_header("Sellers", Some("Approve listings and help sellers get ready to take payments"), None))
        @if !selling_enabled {
            section .products-callout {
                div .products-callout__copy {
                    strong { "Seller products are turned off" }
                    p .text-muted .text-sm { "Existing sellers remain visible, but new seller listings cannot be created." }
                }
                div .products-callout__actions {
                    a .btn .btn--secondary .btn--sm href="/b/products/admin/settings" { "Open settings" }
                }
            }
        }
        section .products-section {
            div .products-section__head {
                div { h2 { "Moderation queue" } p .text-muted .text-sm { (pending.len()) " listing(s) waiting for a decision." } }
            }
            @if pending.is_empty() {
                (components::empty_state(icons::info(), "Queue clear", "No seller listings are waiting for review.", None))
            } @else {
                @let row_hrefs: Vec<String> = pending.iter().map(|product| format!("/b/products/admin/products/{}", product.id)).collect();
                @let cols = [
                    components::TableCol { label: "Product", width: None },
                    components::TableCol { label: "Seller", width: None },
                    components::TableCol { label: "Submitted", width: None },
                    components::TableCol { label: "Status", width: None },
                ];
                @let rows: Vec<Vec<Markup>> = pending.iter().map(|product| vec![
                    html! { span .font-medium { (product.str_field("name")) } },
                    html! { span .text-muted .text-sm { (product.str_field("owner_id")) } },
                    html! { span .text-muted .text-sm { (product.str_field("submitted_at").get(..10).unwrap_or("—")) } },
                    components::status_badge("pending review"),
                ]).collect();
                (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! {}))
            }
        }
        section .products-section {
            div .products-section__head {
                div { h2 { "Seller accounts" } p .text-muted .text-sm { "Open a seller to review payment readiness and their products." } }
            }
            @if sellers.is_empty() {
                (components::empty_state(icons::link(), "No sellers yet", "Seller accounts appear here after a user starts Stripe onboarding.", None))
            } @else {
                @let row_hrefs: Vec<String> = sellers.iter().map(|seller| format!("/b/products/admin/sellers/{}", seller.id)).collect();
                @let cols = [
                    components::TableCol { label: "Seller", width: None },
                    components::TableCol { label: "Selling", width: None },
                    components::TableCol { label: "Payments", width: None },
                    components::TableCol { label: "Payouts", width: None },
                    components::TableCol { label: "Listings", width: None },
                    components::TableCol { label: "Needs action", width: None },
                ];
                @let rows: Vec<Vec<Markup>> = sellers.iter().map(|seller| vec![
                    html! { span .font-medium { (&seller.user_id) } },
                    components::status_badge(&seller.status),
                    components::status_badge(if seller.capabilities.charges_enabled { "enabled" } else { "disabled" }),
                    components::status_badge(if seller.capabilities.payouts_enabled { "enabled" } else { "disabled" }),
                    html! { (product_counts.get(&seller.user_id).copied().unwrap_or_default()) },
                    html! { @if seller.capabilities.requirements_due.is_empty() { span .text-muted { "None" } } @else { strong { (seller.capabilities.requirements_due.len()) } } },
                ]).collect();
                (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! {}))
            }
        }
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Sellers", ui::NavKind::Admin, "Products"),
        content,
    )
    .await
}

pub async fn admin_seller_detail(
    ctx: &dyn Context,
    msg: &Message,
    seller_id: &str,
) -> OutputStream {
    let record = match db::get(ctx, repo::seller_accounts::TABLE, seller_id).await {
        Ok(record) => record,
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => {
            return crate::http::err_not_found("Seller not found");
        }
        Err(error) => return crate::http::err_internal("Could not load seller", error),
    };
    let seller = match repo::seller_accounts::to_contract(&record) {
        Ok(seller) => seller,
        Err(error) => return crate::http::err_internal("Could not read seller status", error),
    };
    let products = match db::list_all(
        ctx,
        PRODUCTS_TABLE,
        vec![Filter {
            field: "owner_id".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!(&seller.user_id),
        }],
    )
    .await
    {
        Ok(products) => products,
        Err(error) => return crate::http::err_internal("Could not list seller products", error),
    };
    let action = if seller.status == "suspended" {
        "reactivate"
    } else {
        "suspend"
    };
    let action_label = if seller.status == "suspended" {
        "Reactivate seller"
    } else {
        "Suspend seller"
    };
    let action_class = if seller.status == "suspended" {
        "btn--primary"
    } else {
        "btn--secondary"
    };
    let config = serde_json::json!({
        "action_url": format!("/b/products/api/admin/sellers/{seller_id}/{action}"),
        "action": action,
    });
    let content = html! {
        (admin_tabs("sellers"))
        (components::page_header(
            &seller.user_id,
            Some("Review payment readiness, outstanding steps, and seller listings"),
            Some(html! { a .btn .btn--secondary .btn--sm href="/b/products/admin/sellers" { "Back to sellers" } }),
        ))
        p #seller-admin-error .login-error role="alert" aria-live="assertive" hidden {}
        section .card {
            header .card__head {
                div {
                    h3 .card__title { "Seller account" }
                    p .text-muted .text-sm style="margin:.25rem 0 0" { "Stripe verification and selling access" }
                }
                div style="display:flex;gap:.5rem;align-items:center;flex-wrap:wrap" {
                    (components::status_badge(&seller.status))
                    button .btn .(action_class) .btn--sm type="button" data-seller-action=(action) onclick="adminSellerSetState(this)" { (action_label) }
                }
            }
            div .card__body {
                div .stats-grid {
                    (components::stat_card("Payments", if seller.capabilities.charges_enabled { "Enabled" } else { "Disabled" }, icons::dollar_sign()))
                    (components::stat_card("Payouts", if seller.capabilities.payouts_enabled { "Enabled" } else { "Disabled" }, icons::arrow_up_right()))
                    (components::stat_card("Verification", if seller.capabilities.details_submitted { "Complete" } else { "Incomplete" }, icons::info()))
                    (components::stat_card("Platform fee", &format!("{:.2}%", seller.fee_basis_points as f64 / 100.0), icons::dollar_sign()))
                }
                details .products-plain-details {
                    summary { "Technical account details" }
                    div .text-sm {
                        p { strong { "Local account ID: " } code { (&seller.id) } }
                        p { strong { "Stripe account: " } @if seller.stripe_account_id.is_empty() { "Not connected" } @else { code { (&seller.stripe_account_id) } } }
                        @if !seller.disabled_reason.is_empty() { p { strong { "Disabled reason: " } (friendly_requirement(&seller.disabled_reason)) } }
                    }
                }
                @if !seller.sync_error.is_empty() { p .login-error { "Stripe connection: " (&seller.sync_error) } }
                h4 { "What this seller still needs to do" }
                @if seller.capabilities.requirements_due.is_empty() {
                    p .text-muted .text-sm { "Nothing — Stripe has no outstanding requirements." }
                } @else {
                    ul { @for requirement in &seller.capabilities.requirements_due { li { (friendly_requirement(requirement)) } } }
                }
            }
        }
        section style="margin-top:1.5rem" {
            h2 { "Owned products" }
            @if products.is_empty() {
                (components::empty_state(icons::package(), "No products", "This seller has not created any products.", None))
            } @else {
                @let row_hrefs: Vec<String> = products.iter().map(|product| format!("/b/products/admin/products/{}", product.id)).collect();
                @let cols = [
                    components::TableCol { label: "Product", width: None },
                    components::TableCol { label: "Status", width: None },
                    components::TableCol { label: "Approval", width: None },
                    components::TableCol { label: "Updated", width: None },
                ];
                @let rows: Vec<Vec<Markup>> = products.iter().map(|product| vec![
                    html! { span .font-medium { (product.str_field("name")) } },
                    components::status_badge(product.str_field("status")),
                    components::status_badge(product.str_field("approval_status")),
                    html! { span .text-muted .text-sm { (product.str_field("updated_at").get(..10).unwrap_or("—")) } },
                ]).collect();
                (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! {}))
            }
        }
        script { (maud::PreEscaped(format!("window.__sellerAdminConfig={config};\n{SELLER_ADMIN_JS}"))) }
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Seller", ui::NavKind::Admin, "Products"),
        content,
    )
    .await
}

const SELLER_ADMIN_JS: &str = r#"
async function adminSellerSetState(button){if(window.__sellerAdminConfig.action==='suspend'&&!window.confirm('Suspend this seller? Active offers and Payment Links will be archived in Stripe before local access is revoked.'))return;button.disabled=true;var original=button.textContent;button.textContent='Working…';var target=document.getElementById('seller-admin-error');target.hidden=true;try{var response=await fetch(window.__sellerAdminConfig.action_url,{method:'POST',credentials:'same-origin',headers:{Accept:'application/json','Content-Type':'application/json'},body:'{}'}),text=await response.text(),payload={};if(text){try{payload=JSON.parse(text)}catch(_error){payload={message:text}}}if(!response.ok)throw new Error(payload.message||payload.error||('Request failed ('+response.status+')'));window.location.reload()}catch(error){target.textContent=error.message;target.hidden=false;button.disabled=false;button.textContent=original}}
"#;

// ---------------------------------------------------------------------------
// Shared admin/seller product wizard
// ---------------------------------------------------------------------------

pub async fn product_wizard(ctx: &dyn Context, msg: &Message, admin: bool) -> OutputStream {
    let configured_currency = wafer_core::clients::config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__DEFAULT_CURRENCY",
        "USD",
    )
    .await;
    let mut default_currency = super::money::normalize_currency(&configured_currency)
        .unwrap_or_else(|_| "USD".to_string());
    let template_definitions = [
        (
            "simple_product",
            "One-time product",
            "A single price paid once. Best for most physical and digital products.",
        ),
        (
            "simple_subscription",
            "Subscription",
            "A recurring price billed weekly, monthly, or yearly, with an optional trial.",
        ),
        (
            "configurable_product",
            "Configurable one-time product",
            "Use for bookings, quantities, customer choices, and optional add-ons.",
        ),
        (
            "configurable_subscription",
            "Configurable subscription",
            "A recurring plan whose price can change with quantities, dates, or choices.",
        ),
    ];
    let seller_templates = if admin {
        std::collections::HashSet::new()
    } else {
        super::handlers::seller_policy::allowed_templates(ctx).await
    };
    let template_definitions: Vec<_> = template_definitions
        .into_iter()
        .filter(|(id, _, _)| admin || seller_templates.is_empty() || seller_templates.contains(*id))
        .collect();
    let initial_template = template_definitions
        .first()
        .map(|(id, _, _)| *id)
        .unwrap_or("");
    let mut seller_currencies = if admin {
        Vec::new()
    } else {
        super::handlers::seller_policy::allowed_currencies(ctx)
            .await
            .into_iter()
            .collect::<Vec<_>>()
    };
    seller_currencies.sort();
    if !seller_currencies.is_empty() && !seller_currencies.contains(&default_currency) {
        default_currency = seller_currencies[0].clone();
    }
    let automatic_tax = wafer_core::clients::config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX",
        "false",
    )
    .await
        == "true";
    let configured_country = wafer_core::clients::config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY",
        "US",
    )
    .await;
    let configured_country = configured_country.trim();
    let platform_country = if configured_country.len() == 2
        && configured_country
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic())
    {
        configured_country.to_ascii_uppercase()
    } else {
        "US".to_string()
    };
    let back_href = if admin {
        "/b/products/admin/manage"
    } else {
        "/b/products/my-products"
    };
    let content = html! {
        @if admin { (admin_tabs("products")) } @else { (portal_tabs("products", true)) }
        (components::page_header(
            "Create product",
            Some("Choose a starting point, add the essentials, and publish when you are ready"),
            Some(html! { a .btn .btn--secondary .btn--sm href=(back_href) { "Cancel" } }),
        ))
        nav .product-wizard-progress aria-label="Product setup progress" {
            ol {
                @for (number, label) in [(1, "Type"), (2, "Basics"), (3, "Price"), (4, "Checkout"), (5, "Publish")] {
                    li data-wizard-indicator=(number) .badge .(if number == 1 { "badge-primary" } else { "badge-secondary" }) style="justify-content:center" {
                        // Completed steps get a check icon (revealed by the
                        // wizard JS) so state is not conveyed by color alone.
                        span .wizard-step-check aria-hidden="true" hidden { (icons::check()) }
                        (number) ". " (label)
                    }
                }
            }
        }
        form #product-wizard-form novalidate onsubmit="return false" {
            p #product-wizard-error .text-sm role="alert" aria-live="assertive" hidden style="color:var(--accent-danger);margin-top:0" {}

            section .card data-wizard-step="1" {
                header .card__head {
                    div {
                        h3 .card__title { "What are you selling?" }
                        p .text-muted .text-sm style="margin:.25rem 0 0" { "Choose the closest match. You can change every detail before saving." }
                    }
                }
                div .card__body {
                    fieldset style="border:0;padding:0;margin:0" {
                        legend .sr-only { "Product template" }
                        div .product-template-grid {
                            @for (value, title, description) in &template_definitions {
                                label .product-template-card {
                                    input type="radio" name="product_template" value=(value) checked[*value == initial_template] onchange="productWizardTemplateChanged()";
                                    strong { (title) }
                                    span .text-muted .text-sm { (description) }
                                }
                            }
                            @if template_definitions.is_empty() {
                                (components::empty_state(icons::info(), "No seller templates are available", "Ask an administrator to allow at least one built-in product template.", None))
                            }
                        }
                    }
                }
            }

            section .card data-wizard-step="2" hidden {
                header .card__head { h3 .card__title { "Product details" } }
                div .card__body {
                    div .form-group {
                        label .form-label .required for="wizard-name" { "Product name" }
                        input #wizard-name .form-input type="text" maxlength="160" required placeholder="e.g. Team plan";
                    }
                    div .form-group {
                        label .form-label for="wizard-description" { "Customer-facing description" }
                        textarea #wizard-description .form-textarea maxlength="4000" placeholder="What the customer receives" {}
                    }
                    details .products-advanced {
                        summary { "More product details (optional)" }
                        div .products-advanced__body {
                            div .products-form-grid {
                                div .form-group {
                                    label .form-label for="wizard-slug" { "Web address" }
                                    input #wizard-slug .form-input type="text" maxlength="160" pattern="[a-z0-9]+(?:-[a-z0-9]+)*" placeholder="Generated from the product name";
                                    p .text-muted .text-sm { "Leave blank to create this automatically." }
                                }
                                div .form-group {
                                    label .form-label for="wizard-image" { "Image URL" }
                                    input #wizard-image .form-input type="url" placeholder="https://…";
                                }
                                div .form-group {
                                    label .form-label for="wizard-fulfillment" { "How it is delivered" }
                                    select #wizard-fulfillment .form-select {
                                        option value="none" { "No automatic delivery" }
                                        option value="manual" { "Handled manually" }
                                        option value="download" { "Digital download" }
                                        option value="entitlement" { "Grant access" }
                                        option value="webhook" { "Notify another system" }
                                    }
                                }
                            }
                            div .form-group {
                                label .form-label for="wizard-tags" { "Tags" }
                                input #wizard-tags .form-input type="text" placeholder="team, premium";
                                p .text-muted .text-sm { "Optional, comma-separated labels for storefronts and integrations." }
                            }
                        }
                    }
                }
            }

            section .card data-wizard-step="3" hidden {
                header .card__head {
                    div {
                        h3 .card__title { "Pricing" }
                        p .text-muted .text-sm style="margin:.25rem 0 0" { "Set the amount customers will see at checkout." }
                    }
                }
                div .card__body {
                    div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:1rem" {
                        div .form-group {
                            label .form-label .required for="wizard-currency" { "Currency" }
                            input #wizard-currency .form-input type="text" value=(default_currency) maxlength="3" list="wizard-currency-options" required;
                            @if !seller_currencies.is_empty() {
                                datalist #wizard-currency-options { @for currency in &seller_currencies { option value=(currency) {} } }
                                p .text-muted .text-sm { "Allowed seller currencies: " (seller_currencies.join(", ")) }
                            }
                        }
                        div .form-group data-simple-pricing {
                            label .form-label .required for="wizard-price" { "Price" }
                            input #wizard-price .form-input type="text" inputmode="decimal" value="0.00" required;
                        }
                        details .products-advanced style="grid-column:1/-1;margin-top:0" {
                            summary { "Advanced price settings (optional)" }
                            div .products-advanced__body {
                                div .products-form-grid .products-form-grid--compact {
                                    div .form-group {
                                        label .form-label for="wizard-tax-behavior" { "How tax is shown" }
                                        select #wizard-tax-behavior .form-select {
                                            option value="unspecified" { "Use the Stripe default" }
                                            option value="exclusive" { "Add tax at checkout" }
                                            option value="inclusive" { "Price already includes tax" }
                                        }
                                    }
                                    div .form-group {
                                        label .form-label for="wizard-minimum-total" { "Minimum item total" }
                                        input #wizard-minimum-total .form-input type="text" inputmode="decimal" placeholder="No minimum";
                                    }
                                    div .form-group {
                                        label .form-label for="wizard-maximum-total" { "Maximum item total" }
                                        input #wizard-maximum-total .form-input type="text" inputmode="decimal" placeholder="No maximum";
                                    }
                                }
                            }
                        }
                        div .form-group data-subscription-field hidden {
                            label .form-label for="wizard-interval" { "Billing interval" }
                            select #wizard-interval .form-select {
                                option value="month" { "Monthly" }
                                option value="year" { "Yearly" }
                                option value="week" { "Weekly" }
                                option value="day" { "Daily" }
                            }
                        }
                        div .form-group data-subscription-field hidden {
                            label .form-label for="wizard-interval-count" { "Every" }
                            input #wizard-interval-count .form-input type="number" min="1" max="36" value="1";
                        }
                    }
                    div #wizard-advanced-pricing hidden {
                        section style="margin-top:1rem" {
                            div style="display:flex;align-items:center;justify-content:space-between;gap:1rem" {
                                div {
                                    h4 style="margin:0" { "Customer fields" }
                                    p .text-muted .text-sm { "Collect dates, quantities, choices, toggles, and notes from the customer." }
                                }
                                button .btn .btn--secondary .btn--sm type="button" onclick="addWizardVariable()" { "+ Add input" }
                            }
                            div #wizard-variables {}
                        }
                        section style="margin-top:1.5rem" {
                            div style="display:flex;align-items:center;justify-content:space-between;gap:1rem" {
                                div {
                                    h4 style="margin:0" { "Itemized price rows" }
                                    p .text-muted .text-sm { "Build the total from clear rows such as base booking, nights, guests, and add-ons." }
                                }
                                button .btn .btn--secondary .btn--sm type="button" onclick="addWizardComponent()" { "+ Add row" }
                            }
                            div #wizard-components {}
                        }
                    }
                    p .text-muted .text-sm {
                        "Customers will see an itemized total calculated from the price and choices above."
                    }
                }
            }

            section .card data-wizard-step="4" hidden {
                header .card__head { h3 .card__title { "Checkout options" } }
                div .card__body {
                    div .products-choice-grid {
                        @for (id, label, help, checked) in [
                            ("wizard-promotions", "Promotion codes", "Let customers enter a Stripe coupon code.", false),
                            ("wizard-automatic-tax", "Automatic tax", "Ask Stripe to calculate tax for this checkout.", automatic_tax),
                            ("wizard-billing-address", "Billing address", "Collect the customer's billing address.", false),
                            ("wizard-shipping-address", "Shipping address", "Collect delivery details and show shipping rates.", false),
                            ("wizard-create-customer", "Create a Stripe Customer for one-time payments", "Useful when customers may buy again or need billing support.", false),
                            ("wizard-terms", "Terms consent", "Require customers to accept your terms before paying.", false),
                        ] {
                            label .products-choice {
                                input id=(id) type="checkbox" checked[checked] onchange="productWizardShippingChanged()";
                                span { strong { (label) } small .text-muted { (help) } }
                            }
                        }
                        div .form-group data-subscription-field hidden {
                            label .form-label for="wizard-trial-days" { "Free trial days" }
                            input #wizard-trial-days .form-input type="number" min="0" max="730" value="0";
                        }
                    }
                    div #wizard-shipping-settings .card hidden style="margin-top:1rem" {
                        div .card__body {
                            h4 style="margin-top:0" { "Shipping destinations and rates" }
                            div style="display:grid;grid-template-columns:minmax(180px,1fr) minmax(280px,2fr);gap:1rem" {
                                div .form-group {
                                    label .form-label for="wizard-shipping-countries" { "Allowed countries" }
                                    input #wizard-shipping-countries .form-input type="text" value=(platform_country) placeholder="NZ, AU, US";
                                    p .text-muted .text-sm { "Comma-separated two-letter country codes." }
                                }
                                div .form-group {
                                    label .form-label for="wizard-shipping-options" { "Shipping options" }
                                    textarea #wizard-shipping-options .form-textarea rows="4" placeholder="Standard | 5.00 | 3 | 5 | business_day | shr_optional" {}
                                    p .text-muted .text-sm {
                                        "One per line: name | amount | minimum | maximum | hour/day/business_day/week/month | optional Stripe rate ID. "
                                        "Leave estimates or the rate ID blank when unused. Inline rates work in hosted and embedded Checkout; Payment Links require a saved shr_ rate ID."
                                    }
                                }
                            }
                        }
                    }
                    p .text-muted .text-sm {
                        "Hosted Checkout, embedded Checkout, and shareable Payment Links can be created from the saved offer. Customer-entered totals are never trusted."
                    }
                }
            }

            section .card data-wizard-step="5" hidden {
                header .card__head { h3 .card__title { "Review" } }
                div .card__body {
                    div #wizard-review aria-live="polite" {}
                    @if !admin {
                        p .text-muted .text-sm {
                            "Publishing may submit the product for administrator review. It will not appear in the public storefront until moderation and Stripe capability checks pass."
                        }
                    }
                }
            }

            div .product-wizard-actions {
                button #wizard-previous .btn .btn--secondary .btn--md type="button" onclick="productWizardPrevious()" hidden { "Back" }
                div .product-wizard-actions__buttons {
                    button #wizard-next .btn .btn--primary .btn--md type="button" onclick="productWizardNext()" disabled[template_definitions.is_empty()] { "Continue" }
                    button #wizard-save-draft .btn .btn--secondary .btn--md type="button" onclick="submitProductWizard('draft')" hidden { "Save draft" }
                    button #wizard-publish .btn .btn--primary .btn--md type="button" onclick="submitProductWizard('publish')" hidden {
                        @if admin { "Create and publish" } @else { "Submit for publication" }
                    }
                }
            }
        }
        script { (maud::PreEscaped(product_wizard_bootstrap(admin))) }
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple(
            "Create product",
            if admin {
                ui::NavKind::Admin
            } else {
                ui::NavKind::Portal
            },
            "Products",
        ),
        content,
    )
    .await
}

fn product_wizard_bootstrap(admin: bool) -> String {
    let config = serde_json::json!({
        "admin": admin,
        "product_collection": if admin { "/b/products/api/admin/products" } else { "/b/products/api/products" },
        "return_url": if admin { "/b/products/admin/manage" } else { "/b/products/my-products" },
    });
    format!("window.__productWizardConfig={config};\n{PRODUCT_WIZARD_JS}\ninitProductWizard();")
}

const PRODUCT_WIZARD_JS: &str = r#"
var productWizardStep=1;
var productWizardVariableIndex=0;
var productWizardComponentIndex=0;

function wizardById(id){return document.getElementById(id)}
function productWizardTemplate(){
  var selected=document.querySelector('input[name="product_template"]:checked');
  return selected?selected.value:'simple_product';
}
function productWizardIsSubscription(){return productWizardTemplate().indexOf('subscription')!==-1}
function productWizardIsConfigurable(){return productWizardTemplate().indexOf('configurable')===0}
function productWizardShowError(message,focus){
  var error=wizardById('product-wizard-error');
  error.textContent=message;error.hidden=false;
  if(focus&&typeof focus.focus==='function')focus.focus();
  error.scrollIntoView({block:'center'});
}
function productWizardClearError(){var error=wizardById('product-wizard-error');error.textContent='';error.hidden=true}
function productWizardSlug(value){
  return value.toLowerCase().normalize('NFKD').replace(/[\u0300-\u036f]/g,'').replace(/[^a-z0-9]+/g,'-').replace(/^-+|-+$/g,'').slice(0,160);
}
function productWizardTemplateChanged(){
  var subscription=productWizardIsSubscription();
  var configurable=productWizardIsConfigurable();
  document.querySelectorAll('[data-subscription-field]').forEach(function(el){el.hidden=!subscription});
  document.querySelectorAll('[data-simple-pricing]').forEach(function(el){el.hidden=configurable});
  wizardById('wizard-advanced-pricing').hidden=!configurable;
  if(configurable&&wizardById('wizard-variables').children.length===0){
    addWizardVariable({key:'quantity',label:'Quantity',kind:'integer',required:true,minimum:'1',maximum:'100',step:'1'});
    addWizardComponent({key:'base',label:'Base price',amount_type:'fixed',amount:'0.00',required:true});
    addWizardComponent({key:'quantity',label:'Quantity',amount_type:'per_unit',amount:'0.00',input:'quantity',required:true});
  }
}
function productWizardShowStep(step,scrollToStep){
  productWizardStep=Math.max(1,Math.min(5,step));
  document.querySelectorAll('[data-wizard-step]').forEach(function(el){el.hidden=Number(el.dataset.wizardStep)!==productWizardStep});
  document.querySelectorAll('[data-wizard-indicator]').forEach(function(el){
    var current=Number(el.dataset.wizardIndicator);
    el.className='badge '+(current===productWizardStep?'badge-primary':current<productWizardStep?'badge-success':'badge-secondary');
    var check=el.querySelector('.wizard-step-check');
    if(check)check.hidden=current>=productWizardStep;
  });
  wizardById('wizard-previous').hidden=productWizardStep===1;
  wizardById('wizard-next').hidden=productWizardStep===5;
  wizardById('wizard-save-draft').hidden=productWizardStep!==5;
  wizardById('wizard-publish').hidden=productWizardStep!==5;
  if(productWizardStep===5)renderProductWizardReview();
  productWizardClearError();
  var current=document.querySelector('[data-wizard-step="'+productWizardStep+'"]');
  if(current&&scrollToStep!==false)current.scrollIntoView({block:'start'});
}
function productWizardValidateStep(step){
  if(step===2){
    var name=wizardById('wizard-name');
    if(!name.value.trim()){productWizardShowError('Product name is required.',name);return false}
    var slug=wizardById('wizard-slug');
    if(slug.value.trim()&&!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(slug.value.trim())){
      productWizardShowError('URL slug may contain lowercase letters, numbers, and single hyphens.',slug);return false
    }
    var image=wizardById('wizard-image');
    if(image.value&&!image.checkValidity()){productWizardShowError('Image URL must be a valid absolute URL.',image);return false}
  }
  if(step===3){
    try{buildProductWizardOffer()}catch(error){productWizardShowError(error.message);return false}
  }
  return true;
}
function productWizardNext(){if(productWizardValidateStep(productWizardStep))productWizardShowStep(productWizardStep+1)}
function productWizardPrevious(){productWizardShowStep(productWizardStep-1)}

function addWizardVariable(seed){
  seed=seed||{};var index=productWizardVariableIndex++;
  var row=document.createElement('section');row.className='card';row.dataset.variableRow='';row.style.marginTop='.75rem';
  row.innerHTML=`<div class="card__body">
    <div style="display:flex;justify-content:space-between;gap:.75rem;align-items:center"><strong>Customer input</strong><button class="btn btn--secondary btn--sm" type="button" data-remove-row>Remove</button></div>
    <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:.75rem;margin-top:.75rem">
      <div class="form-group"><label class="form-label required" for="wizard-variable-key-${index}">Key</label><input class="form-input" id="wizard-variable-key-${index}" data-variable-key required placeholder="quantity"></div>
      <div class="form-group"><label class="form-label required" for="wizard-variable-label-${index}">Label</label><input class="form-input" id="wizard-variable-label-${index}" data-variable-label required placeholder="Quantity"></div>
      <div class="form-group"><label class="form-label" for="wizard-variable-kind-${index}">Type</label><select class="form-select" id="wizard-variable-kind-${index}" data-variable-kind><option value="integer">Whole number</option><option value="number">Decimal number</option><option value="date">Date</option><option value="date_time">Date and time</option><option value="boolean">Yes / no</option><option value="select">Choice</option><option value="multi_select">Multiple choices</option><option value="text">Text</option></select></div>
      <div class="form-group" data-variable-min-wrap><label class="form-label" for="wizard-variable-min-${index}">Minimum</label><input class="form-input" id="wizard-variable-min-${index}" data-variable-min inputmode="decimal"></div>
      <div class="form-group" data-variable-max-wrap><label class="form-label" for="wizard-variable-max-${index}">Maximum</label><input class="form-input" id="wizard-variable-max-${index}" data-variable-max inputmode="decimal"></div>
      <div class="form-group" data-variable-step-wrap><label class="form-label" for="wizard-variable-step-${index}">Step</label><input class="form-input" id="wizard-variable-step-${index}" data-variable-step inputmode="decimal"></div>
      <div class="form-group" data-variable-options-wrap><label class="form-label" for="wizard-variable-options-${index}">Choices</label><input class="form-input" id="wizard-variable-options-${index}" data-variable-options placeholder="small, medium, large"></div>
      <div class="form-group"><label class="form-label" for="wizard-variable-visibility-${index}">Visibility</label><select class="form-select" id="wizard-variable-visibility-${index}" data-variable-visibility><option value="public">Customer</option><option value="hidden">Hidden</option><option value="admin_only">Admin only</option></select></div>
      <div class="form-group"><label class="form-label" for="wizard-variable-default-${index}">Default value</label><input class="form-input" id="wizard-variable-default-${index}" data-variable-default placeholder="Optional"></div>
      <div class="form-group" data-variable-length-wrap><label class="form-label" for="wizard-variable-length-${index}">Maximum text length</label><input class="form-input" id="wizard-variable-length-${index}" data-variable-length type="number" min="1" max="10000"></div>
    </div><label style="display:flex;gap:.5rem"><input type="checkbox" data-variable-required> Required</label>
    <div class="form-group"><label class="form-label" for="wizard-variable-help-${index}">Help text</label><input class="form-input" id="wizard-variable-help-${index}" data-variable-help maxlength="500" placeholder="Shown beside this input"></div>
  </div>`;
  row.querySelector('[data-remove-row]').onclick=function(){row.remove()};
  row.querySelector('[data-variable-key]').value=seed.key||'';
  row.querySelector('[data-variable-label]').value=seed.label||'';
  row.querySelector('[data-variable-kind]').value=seed.kind||'integer';
  row.querySelector('[data-variable-min]').value=seed.minimum||'';
  row.querySelector('[data-variable-max]').value=seed.maximum||'';
  row.querySelector('[data-variable-step]').value=seed.step||'';
  row.querySelector('[data-variable-options]').value=(seed.allowed_values||[]).join(', ');
  row.querySelector('[data-variable-visibility]').value=seed.visibility||'public';
  row.querySelector('[data-variable-default]').value=seed.default_value===undefined||seed.default_value===null?'':Array.isArray(seed.default_value)?seed.default_value.join(', '):String(seed.default_value);
  row.querySelector('[data-variable-length]').value=seed.maximum_length||'';
  row.querySelector('[data-variable-help]').value=seed.help_text||'';
  row.querySelector('[data-variable-required]').checked=seed.required!==false;
  row.querySelector('[data-variable-kind]').onchange=function(){wizardVariableKindChanged(row)};
  wizardVariableKindChanged(row);
  wizardById('wizard-variables').appendChild(row);
}

function wizardVariableKindChanged(row){
  var kind=row.querySelector('[data-variable-kind]').value,numeric=kind==='integer'||kind==='number',dated=kind==='date'||kind==='date_time',choices=kind==='select'||kind==='multi_select';
  var minimum=row.querySelector('[data-variable-min]'),maximum=row.querySelector('[data-variable-max]'),defaultInput=row.querySelector('[data-variable-default]');
  row.querySelector('[data-variable-min-wrap]').hidden=!(numeric||dated);row.querySelector('[data-variable-max-wrap]').hidden=!(numeric||dated);row.querySelector('[data-variable-step-wrap]').hidden=!numeric;row.querySelector('[data-variable-options-wrap]').hidden=!choices;row.querySelector('[data-variable-length-wrap]').hidden=kind!=='text';
  minimum.type=kind==='date'?'date':kind==='date_time'?'datetime-local':'text';maximum.type=minimum.type;defaultInput.type=minimum.type;
  minimum.inputMode=numeric?'decimal':'';maximum.inputMode=numeric?'decimal':'';
}

function addWizardComponent(seed){
  seed=seed||{};var index=productWizardComponentIndex++;
  var row=document.createElement('section');row.className='card';row.dataset.componentRow='';row.style.marginTop='.75rem';
  row.innerHTML=`<div class="card__body">
    <div style="display:flex;justify-content:space-between;gap:.75rem;align-items:center"><strong>Price row</strong><button class="btn btn--secondary btn--sm" type="button" data-remove-row>Remove</button></div>
    <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:.75rem;margin-top:.75rem">
      <div class="form-group"><label class="form-label required" for="wizard-component-key-${index}">Key</label><input class="form-input" id="wizard-component-key-${index}" data-component-key required placeholder="base"></div>
      <div class="form-group"><label class="form-label required" for="wizard-component-label-${index}">Label</label><input class="form-input" id="wizard-component-label-${index}" data-component-label required placeholder="Base price"></div>
      <div class="form-group"><label class="form-label" for="wizard-component-description-${index}">Description</label><input class="form-input" id="wizard-component-description-${index}" data-component-description maxlength="500"></div>
      <div class="form-group"><label class="form-label" for="wizard-component-type-${index}">Calculation</label><select class="form-select" id="wizard-component-type-${index}" data-component-type><option value="fixed">Fixed amount</option><option value="per_unit">Amount × input</option><option value="flat_plus_per_unit">Base + amount × input</option><option value="lookup">Price selected by input</option><option value="graduated">Graduated tiers</option><option value="volume">Volume tiers</option><option value="package">Packages / blocks</option></select></div>
      <div class="form-group"><label class="form-label required" for="wizard-component-amount-${index}">Amount / unit rate</label><input class="form-input" id="wizard-component-amount-${index}" data-component-amount inputmode="decimal" value="0.00" required></div>
      <div class="form-group"><label class="form-label" for="wizard-component-input-${index}">Pricing input key</label><input class="form-input" id="wizard-component-input-${index}" data-component-input placeholder="quantity"></div>
      <div class="form-group"><label class="form-label" for="wizard-condition-${index}">Condition</label><select class="form-select" id="wizard-condition-${index}" data-component-condition><option value="always">Always include</option><option value="equals">Input equals value</option><option value="not_equals">Input does not equal value</option><option value="greater_than">Input is greater than value</option><option value="greater_than_or_equal">Input is at least value</option><option value="less_than">Input is less than value</option><option value="less_than_or_equal">Input is at most value</option><option value="contains">Input contains value</option><option value="in">Input is one of these values</option><option value="present">Input is present</option><option value="advanced_preserved" hidden>Advanced condition (preserved)</option></select></div>
      <div class="form-group"><label class="form-label" for="wizard-condition-input-${index}">Condition input</label><input class="form-input" id="wizard-condition-input-${index}" data-condition-input></div>
      <div class="form-group"><label class="form-label" for="wizard-condition-value-${index}">Condition value</label><input class="form-input" id="wizard-condition-value-${index}" data-condition-value></div>
    </div>
    <details style="margin:.75rem 0"><summary>Advanced calculation details</summary>
      <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:.75rem;margin-top:.75rem">
        <div class="form-group"><label class="form-label" for="wizard-component-base-${index}">Base amount</label><input class="form-input" id="wizard-component-base-${index}" data-component-base inputmode="decimal" value="0.00"><p class="text-muted text-sm">Used by base + per-unit pricing.</p></div>
        <div class="form-group"><label class="form-label" for="wizard-component-package-size-${index}">Units per package</label><input class="form-input" id="wizard-component-package-size-${index}" data-component-package-size type="number" min="1" value="1"><p class="text-muted text-sm">Used by package pricing.</p></div>
        <div class="form-group"><label class="form-label" for="wizard-component-rounding-${index}">Partial packages</label><select class="form-select" id="wizard-component-rounding-${index}" data-component-rounding><option value="up">Round up and charge a package</option><option value="exact">Require an exact multiple</option></select></div>
      </div>
      <div class="form-group"><label class="form-label" for="wizard-component-details-${index}">Lookup prices or tiers</label><textarea class="form-textarea" id="wizard-component-details-${index}" data-component-details rows="4" placeholder="Lookup: small = 10.00&#10;Tier: 10 | 1.00 | 0.00&#10;Final tier: * | 0.80 | 0.00"></textarea><p class="text-muted text-sm">Lookup rows use <code>choice = amount</code>. Tier rows use <code>upper bound | unit amount | flat amount</code>; use <code>*</code> for the final open tier.</p></div>
    </details>
    <label style="display:flex;gap:.5rem"><input type="checkbox" data-component-required> Required row</label>
  </div>`;
  row.querySelector('[data-remove-row]').onclick=function(){row.remove()};
  row.querySelector('[data-component-key]').value=seed.key||'';
  row.querySelector('[data-component-label]').value=seed.label||'';
  row.querySelector('[data-component-description]').value=seed.description||'';
  row.querySelector('[data-component-type]').value=seed.amount_type||'fixed';
  row.querySelector('[data-component-amount]').value=seed.amount||'0.00';
  row.querySelector('[data-component-input]').value=seed.input||'';
  row.querySelector('[data-component-base]').value=seed.base_amount||'0.00';
  row.querySelector('[data-component-package-size]').value=seed.units_per_package||'1';
  row.querySelector('[data-component-rounding]').value=seed.rounding||'up';
  row.querySelector('[data-component-details]').value=seed.details||'';
  row.querySelector('[data-component-condition]').value=seed.condition||'always';
  if(seed.preserved_condition){row.dataset.preservedCondition=JSON.stringify(seed.preserved_condition);row.querySelector('[data-component-condition]').querySelector('[value="advanced_preserved"]').hidden=false;row.querySelector('[data-component-condition]').value='advanced_preserved'}
  if(seed.preserved_quantity)row.dataset.preservedQuantity=JSON.stringify(seed.preserved_quantity);
  if(seed.preserved_metadata)row.dataset.preservedMetadata=JSON.stringify(seed.preserved_metadata);
  row.querySelector('[data-condition-input]').value=seed.condition_input||'';
  row.querySelector('[data-condition-value]').value=seed.condition_value||'';
  row.querySelector('[data-component-required]').checked=seed.required!==false;
  wizardById('wizard-components').appendChild(row);
}

function wizardCurrencyExponent(currency){
  var zero=['BIF','CLP','DJF','GNF','JPY','KMF','KRW','MGA','PYG','RWF','UGX','VND','VUV','XAF','XOF','XPF'];
  var three=['BHD','JOD','KWD','OMR','TND'];
  return zero.indexOf(currency)!==-1?0:three.indexOf(currency)!==-1?3:2;
}
function wizardMoneyToMinor(raw,currency){
  raw=String(raw).trim();currency=String(currency).trim().toUpperCase();
  if(!/^[A-Z]{3}$/.test(currency))throw new Error('Currency must be a three-letter ISO code.');
  if(!/^\+?(?:\d+(?:\.\d*)?|\.\d+)$/.test(raw))throw new Error('Amounts must be non-negative plain decimal numbers.');
  raw=raw.replace(/^\+/,'');var parts=raw.split('.');var whole=parts[0]||'0';var fraction=parts[1]||'';var exponent=wizardCurrencyExponent(currency);
  if(fraction.length>exponent&&/[^0]/.test(fraction.slice(exponent)))throw new Error('Amount has more than '+exponent+' decimal places for '+currency+'.');
  fraction=fraction.slice(0,exponent).padEnd(exponent,'0');
  var multiplier=BigInt(10)**BigInt(exponent);var minor=BigInt(whole)*multiplier+BigInt(fraction||'0');
  if(minor>BigInt(Number.MAX_SAFE_INTEGER))throw new Error('Amount is too large.');
  return Number(minor);
}
function wizardMinorToDisplay(minor,currency){
  var exponent=wizardCurrencyExponent(currency),raw=String(minor).padStart(exponent+1,'0');
  return exponent===0?raw:raw.slice(0,-exponent)+'.'+raw.slice(-exponent);
}
function collectWizardVariables(){
  var variables=[],keys=new Set();
  document.querySelectorAll('[data-variable-row]').forEach(function(row,index){
    var key=row.querySelector('[data-variable-key]').value.trim();var label=row.querySelector('[data-variable-label]').value.trim();var kind=row.querySelector('[data-variable-kind]').value;
    if(!/^[A-Za-z][A-Za-z0-9_]*$/.test(key))throw new Error('Each customer input needs a unique key using letters, numbers, and underscores.');
    if(keys.has(key))throw new Error('Customer input keys must be unique: '+key);keys.add(key);
    if(!label)throw new Error('Each customer input needs a label.');
    var variable={key:key,kind:kind,label:label,required:row.querySelector('[data-variable-required]').checked,visibility:row.querySelector('[data-variable-visibility]').value,sort_order:index};
    var minimum=row.querySelector('[data-variable-min]').value.trim(),maximum=row.querySelector('[data-variable-max]').value.trim(),step=row.querySelector('[data-variable-step]').value.trim();
    if((kind==='integer'||kind==='number'||kind==='date'||kind==='date_time')&&minimum)variable.minimum=minimum;
    if((kind==='integer'||kind==='number'||kind==='date'||kind==='date_time')&&maximum)variable.maximum=maximum;
    if((kind==='integer'||kind==='number')&&step)variable.step=step;
    if(kind==='select'||kind==='multi_select'){
      variable.allowed_values=row.querySelector('[data-variable-options]').value.split(',').map(function(v){return v.trim()}).filter(Boolean);
      if(variable.allowed_values.length===0)throw new Error('Choice input '+key+' needs at least one allowed value.');
    }
    var help=row.querySelector('[data-variable-help]').value.trim(),defaultRaw=row.querySelector('[data-variable-default]').value.trim(),maximumLength=Number(row.querySelector('[data-variable-length]').value||0);
    if(help)variable.help_text=help;
    if(maximumLength){if(!Number.isSafeInteger(maximumLength)||maximumLength<1||maximumLength>10000)throw new Error('Maximum text length on '+key+' must be between 1 and 10000.');variable.maximum_length=maximumLength}
    if(defaultRaw!==''){
      if(kind==='multi_select')variable.default_value=defaultRaw.split(',').map(function(value){return value.trim()}).filter(Boolean);
      else variable.default_value=wizardConditionValue(defaultRaw,variable);
    }
    variables.push(variable);
  });
  return variables;
}
function wizardConditionValue(raw,variable){
  if(!variable)return raw;
  if(variable.kind==='boolean'){
    if(raw!=='true'&&raw!=='false')throw new Error('Boolean conditions must use true or false.');return raw==='true';
  }
  if(variable.kind==='integer'){
    if(!/^-?\d+$/.test(raw))throw new Error('Integer condition values must be whole numbers.');return Number(raw);
  }
  if(variable.kind==='number'){
    if(!/^-?(?:\d+(?:\.\d*)?|\.\d+)$/.test(raw))throw new Error('Number condition values must be decimal numbers.');return raw;
  }
  if(variable.kind==='date'&&!/^\d{4}-\d{2}-\d{2}$/.test(raw))throw new Error('Date values must use YYYY-MM-DD.');
  if(variable.kind==='date_time'&&!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(raw))throw new Error('Date and time values must use YYYY-MM-DDTHH:MM.');
  return raw;
}
function wizardPricingLines(raw){return String(raw).split(/\r?\n/).map(function(line){return line.trim()}).filter(Boolean)}
function wizardParseLookup(raw,currency,key){
  var prices={};wizardPricingLines(raw).forEach(function(line){
    var split=line.indexOf('=');if(split<1)throw new Error('Lookup row '+key+' must use choice = amount.');
    var choice=line.slice(0,split).trim(),value=line.slice(split+1).trim();
    if(!choice||Object.prototype.hasOwnProperty.call(prices,choice))throw new Error('Lookup choices on '+key+' must be non-empty and unique.');
    prices[choice]=wizardMoneyToMinor(value,currency);
  });
  if(Object.keys(prices).length===0)throw new Error('Lookup row '+key+' needs at least one choice and amount.');return prices;
}
function wizardParseTiers(raw,currency,key){
  var tiers=wizardPricingLines(raw).map(function(line,index,lines){
    var parts=line.split('|').map(function(part){return part.trim()});
    if(parts.length<2||parts.length>3)throw new Error('Tier row '+key+' must use upper bound | unit amount | optional flat amount.');
    var open=parts[0]==='*',upTo=open?undefined:Number(parts[0]);
    if(!open&&(!Number.isSafeInteger(upTo)||upTo<1))throw new Error('Tier bounds on '+key+' must be positive whole numbers.');
    if(open&&index!==lines.length-1)throw new Error('Only the final tier on '+key+' may use *.');
    if(!open&&index===lines.length-1)throw new Error('The final tier on '+key+' must use *.');
    var tier={unit_amount_minor:wizardMoneyToMinor(parts[1],currency),flat_amount_minor:wizardMoneyToMinor(parts[2]||'0',currency)};
    if(!open)tier.up_to=upTo;return tier;
  });
  if(tiers.length===0)throw new Error('Tiered row '+key+' needs at least one tier.');
  for(var i=1;i<tiers.length;i++){if(tiers[i-1].up_to!==undefined&&tiers[i].up_to!==undefined&&tiers[i].up_to<=tiers[i-1].up_to)throw new Error('Tier bounds on '+key+' must increase.')}
  return tiers;
}
function productWizardShippingChanged(){
  var settings=wizardById('wizard-shipping-settings'),shipping=wizardById('wizard-shipping-address');
  if(settings&&shipping)settings.hidden=!shipping.checked;
}
function wizardParseShippingCountries(raw){
  var seen=new Set(),countries=String(raw).split(',').map(function(value){return value.trim().toUpperCase()}).filter(Boolean);
  if(countries.length===0)throw new Error('Add at least one allowed shipping country.');
  if(countries.length>50)throw new Error('At most 50 shipping countries may be configured.');
  countries.forEach(function(country){if(!/^[A-Z]{2}$/.test(country))throw new Error('Shipping countries must use two-letter codes.');if(seen.has(country))throw new Error('Shipping countries must be unique.');seen.add(country)});
  return countries;
}
function wizardParseShippingOptions(raw,currency,taxBehavior){
  var units=new Set(['hour','day','business_day','week','month']);
  var options=wizardPricingLines(raw).map(function(line){
    var parts=line.split('|').map(function(part){return part.trim()});
    if(parts.length<2||parts.length>6)throw new Error('Shipping options must use name | amount | minimum | maximum | unit | optional Stripe rate ID.');
    while(parts.length<6)parts.push('');
    var name=parts[0],minimum=parts[2]===''?undefined:Number(parts[2]),maximum=parts[3]===''?undefined:Number(parts[3]),unit=parts[4],stripeId=parts[5];
    if(!name||name.length>100)throw new Error('Shipping option names must contain between 1 and 100 characters.');
    if(minimum!==undefined&&(!Number.isSafeInteger(minimum)||minimum<1))throw new Error('Shipping estimate minimums must be positive whole numbers.');
    if(maximum!==undefined&&(!Number.isSafeInteger(maximum)||maximum<1))throw new Error('Shipping estimate maximums must be positive whole numbers.');
    if(minimum!==undefined&&maximum!==undefined&&minimum>maximum)throw new Error('Shipping estimate minimums must not exceed maximums.');
    if((minimum!==undefined||maximum!==undefined)&&!units.has(unit))throw new Error('Shipping estimates need a valid time unit.');
    if(minimum===undefined&&maximum===undefined&&unit!=='')throw new Error('A shipping time unit needs a minimum or maximum estimate.');
    if(stripeId&&!/^shr_[A-Za-z0-9_]+$/.test(stripeId))throw new Error('Stripe shipping rate IDs must start with shr_.');
    var option={display_name:name,amount_minor:wizardMoneyToMinor(parts[1],currency),tax_behavior:taxBehavior,stripe_shipping_rate_id:stripeId};
    if(minimum!==undefined||maximum!==undefined){option.delivery_estimate={minimum:minimum,maximum:maximum,unit:unit}}
    return option;
  });
  if(options.length>5)throw new Error('Stripe Checkout supports at most five shipping options.');
  return options;
}
function collectWizardComponents(variables,currency,subscription,interval,intervalCount){
  var components=[],keys=new Set(),byKey={};variables.forEach(function(v){byKey[v.key]=v});
  document.querySelectorAll('[data-component-row]').forEach(function(row,index){
    var key=row.querySelector('[data-component-key]').value.trim(),label=row.querySelector('[data-component-label]').value.trim();
    if(!/^[A-Za-z][A-Za-z0-9_]*$/.test(key)||keys.has(key))throw new Error('Each price row needs a unique key using letters, numbers, and underscores.');keys.add(key);
    if(!label)throw new Error('Each price row needs a label.');
    var type=row.querySelector('[data-component-type]').value,input=row.querySelector('[data-component-input]').value.trim(),amount;
    var numeric=type==='per_unit'||type==='flat_plus_per_unit'||type==='graduated'||type==='volume'||type==='package';
    if(type!=='fixed'&&!byKey[input])throw new Error('Price row '+key+' must reference an existing input.');
    if(numeric&&byKey[input].kind!=='integer'&&byKey[input].kind!=='number')throw new Error('Price row '+key+' must reference a number input.');
    if(type==='fixed')amount={type:'fixed',unit_amount_minor:wizardMoneyToMinor(row.querySelector('[data-component-amount]').value,currency)};
    else if(type==='per_unit')amount={type:'per_unit',input:input,unit_amount_minor:wizardMoneyToMinor(row.querySelector('[data-component-amount]').value,currency)};
    else if(type==='flat_plus_per_unit')amount={type:'flat_plus_per_unit',base_amount_minor:wizardMoneyToMinor(row.querySelector('[data-component-base]').value,currency),input:input,unit_amount_minor:wizardMoneyToMinor(row.querySelector('[data-component-amount]').value,currency)};
    else if(type==='lookup'){
      if(byKey[input].kind!=='select'&&byKey[input].kind!=='text')throw new Error('Lookup row '+key+' must reference a choice or text input.');
      amount={type:'lookup',input:input,prices:wizardParseLookup(row.querySelector('[data-component-details]').value,currency,key)};
    }else if(type==='graduated'||type==='volume')amount={type:type,input:input,tiers:wizardParseTiers(row.querySelector('[data-component-details]').value,currency,key)};
    else if(type==='package'){
      var packageSize=Number(row.querySelector('[data-component-package-size]').value);
      if(!Number.isSafeInteger(packageSize)||packageSize<1)throw new Error('Package size on '+key+' must be a positive whole number.');
      amount={type:'package',input:input,units_per_package:packageSize,package_amount_minor:wizardMoneyToMinor(row.querySelector('[data-component-amount]').value,currency),rounding:row.querySelector('[data-component-rounding]').value};
    }else throw new Error('Price row '+key+' uses an unknown calculation.');
    var conditionType=row.querySelector('[data-component-condition]').value,conditionInput=row.querySelector('[data-condition-input]').value.trim(),rawCondition=row.querySelector('[data-condition-value]').value.trim();var condition={op:'always'};
    if(conditionType==='advanced_preserved'){
      try{condition=JSON.parse(row.dataset.preservedCondition)}catch(_error){throw new Error('Advanced condition on '+key+' could not be preserved.')}
    }else if(conditionType!=='always'){
      if(!byKey[conditionInput])throw new Error('Condition on '+key+' must reference an existing input.');
      if(conditionType==='present')condition={op:'present',input:conditionInput};
      else if(conditionType==='in'){
        var conditionValues=rawCondition.split(',').map(function(value){return value.trim()}).filter(Boolean);
        if(conditionValues.length===0)throw new Error('Condition on '+key+' needs at least one comparison value.');
        condition={op:'in',input:conditionInput,values:conditionValues.map(function(value){return wizardConditionValue(value,byKey[conditionInput])})};
      }else{
        if(rawCondition==='')throw new Error('Condition on '+key+' needs a comparison value.');
        condition={op:conditionType,input:conditionInput,value:wizardConditionValue(rawCondition,byKey[conditionInput])};
      }
    }
    var quantity={type:'fixed',value:1},metadata={};
    if(row.dataset.preservedQuantity){try{quantity=JSON.parse(row.dataset.preservedQuantity)}catch(_error){throw new Error('Advanced quantity rule on '+key+' could not be preserved.')}}
    if(row.dataset.preservedMetadata){try{metadata=JSON.parse(row.dataset.preservedMetadata)}catch(_error){throw new Error('Metadata on '+key+' could not be preserved.')}}
    var component={key:key,label:label,description:row.querySelector('[data-component-description]').value.trim(),sort_order:index,required:row.querySelector('[data-component-required]').checked,amount:amount,quantity:quantity,condition:condition,metadata:metadata};
    if(subscription)component.recurrence={interval:interval,interval_count:intervalCount};
    components.push(component);
  });
  if(components.length===0)throw new Error('Add at least one itemized price row.');return components;
}
function buildProductWizardOffer(){
  var template=productWizardTemplate(),subscription=productWizardIsSubscription(),configurable=productWizardIsConfigurable();
  var currency=wizardById('wizard-currency').value.trim().toUpperCase();
  if(!/^[A-Z]{3}$/.test(currency))throw new Error('Currency must be a three-letter ISO code.');
  var interval=wizardById('wizard-interval').value,intervalCount=Number(wizardById('wizard-interval-count').value||1);
  if(subscription&&(!Number.isInteger(intervalCount)||intervalCount<1||intervalCount>36))throw new Error('Billing interval count must be between 1 and 36.');
  var variables=[],components=[];
  if(configurable){variables=collectWizardVariables();components=collectWizardComponents(variables,currency,subscription,interval,intervalCount)}
  else{
    var amount=wizardMoneyToMinor(wizardById('wizard-price').value,currency);
    var component={key:'price',label:wizardById('wizard-name').value.trim()||'Price',sort_order:0,required:true,amount:{type:'fixed',unit_amount_minor:amount},quantity:{type:'fixed',value:1},condition:{op:'always'}};
    if(subscription)component.recurrence={interval:interval,interval_count:intervalCount};components=[component];
  }
  var tiered=components.some(function(component){return component.amount.type==='graduated'||component.amount.type==='volume'});
  var taxBehavior=wizardById('wizard-tax-behavior').value,collectShipping=wizardById('wizard-shipping-address').checked;
  var shippingCountries=collectShipping?wizardParseShippingCountries(wizardById('wizard-shipping-countries').value):[];
  var shippingOptions=collectShipping?wizardParseShippingOptions(wizardById('wizard-shipping-options').value,currency,taxBehavior):[];
  var minimumRaw=wizardById('wizard-minimum-total').value.trim(),maximumRaw=wizardById('wizard-maximum-total').value.trim();
  var minimumTotal=minimumRaw?wizardMoneyToMinor(minimumRaw,currency):null,maximumTotal=maximumRaw?wizardMoneyToMinor(maximumRaw,currency):null;
  if(maximumTotal!==null&&maximumTotal<=0)throw new Error('Maximum item total must be greater than zero.');
  if(minimumTotal!==null&&maximumTotal!==null&&minimumTotal>maximumTotal)throw new Error('Minimum item total must not exceed maximum item total.');
  var checkout={allow_promotion_codes:wizardById('wizard-promotions').checked,automatic_tax:wizardById('wizard-automatic-tax').checked,collect_billing_address:wizardById('wizard-billing-address').checked,collect_shipping_address:collectShipping,allowed_shipping_countries:shippingCountries,shipping_options:shippingOptions,create_customer:wizardById('wizard-create-customer').checked,require_terms_consent:wizardById('wizard-terms').checked,trial_days:subscription?Number(wizardById('wizard-trial-days').value||0):0};
  if(minimumTotal!==null)checkout.minimum_total_minor=minimumTotal;if(maximumTotal!==null)checkout.maximum_total_minor=maximumTotal;
  return {name:wizardById('wizard-name').value.trim()||'New offer',mode:subscription?'subscription':'payment',currency:currency,pricing_model:configurable?'components':'fixed',recurring_interval:subscription?interval:null,interval_count:subscription?intervalCount:1,usage_type:'licensed',billing_scheme:tiered?'tiered':'per_unit',tax_behavior:taxBehavior,variables:variables,components:components,checkout:checkout};
}
function buildProductWizardPayload(){
  var name=wizardById('wizard-name').value.trim();if(!name)throw new Error('Product name is required.');
  var slug=wizardById('wizard-slug').value.trim()||productWizardSlug(name);if(!slug)throw new Error('Product name must contain at least one letter or number.');
  var offer=buildProductWizardOffer();var tags=wizardById('wizard-tags').value.split(',').map(function(v){return v.trim()}).filter(Boolean);
  var product={name:name,slug:slug,description:wizardById('wizard-description').value.trim(),image_url:wizardById('wizard-image').value.trim(),tags:tags,currency:offer.currency,fulfillment_kind:wizardById('wizard-fulfillment').value,product_template_id:productWizardTemplate(),metadata:{impresspress_template:productWizardTemplate()}};
  return {product:product,offer:offer};
}
function renderProductWizardReview(){
  var target=wizardById('wizard-review');target.replaceChildren();
  try{
    var built=buildProductWizardPayload(),offer=built.offer;
    var title=document.createElement('h4');title.textContent=built.product.name;target.appendChild(title);
    var summary=document.createElement('p');summary.className='text-muted text-sm';summary.textContent=(offer.mode==='subscription'?'Subscription':'One-time payment')+' · '+offer.currency+' · '+(offer.pricing_model==='fixed'?'Fixed price':'Configurable rows');target.appendChild(summary);
    var list=document.createElement('ul');
    offer.components.forEach(function(component){var item=document.createElement('li'),rule=component.amount,description='';
      if(rule.type==='fixed')description=wizardMinorToDisplay(rule.unit_amount_minor,offer.currency)+' '+offer.currency;
      else if(rule.type==='per_unit')description=wizardMinorToDisplay(rule.unit_amount_minor,offer.currency)+' '+offer.currency+' per '+rule.input;
      else if(rule.type==='flat_plus_per_unit')description=wizardMinorToDisplay(rule.base_amount_minor,offer.currency)+' + '+wizardMinorToDisplay(rule.unit_amount_minor,offer.currency)+' '+offer.currency+' per '+rule.input;
      else if(rule.type==='lookup')description=Object.keys(rule.prices).length+' lookup price(s) selected by '+rule.input;
      else if(rule.type==='graduated'||rule.type==='volume')description=rule.tiers.length+' '+rule.type+' tier(s) based on '+rule.input;
      else if(rule.type==='package')description=wizardMinorToDisplay(rule.package_amount_minor,offer.currency)+' '+offer.currency+' per '+rule.units_per_package+' '+rule.input;
      item.textContent=component.label+': '+description+(component.condition.op!=='always'?' when '+component.condition.input+' '+component.condition.op.replace(/_/g,' ')+' '+String(component.condition.value||component.condition.values||''):'');list.appendChild(item)});
    target.appendChild(list);
    var options=document.createElement('p');options.className='text-muted text-sm';options.textContent=offer.variables.length+' customer input(s), '+offer.components.length+' price row(s)'+(offer.checkout.minimum_total_minor!==undefined?', minimum '+wizardMinorToDisplay(offer.checkout.minimum_total_minor,offer.currency)+' '+offer.currency:'')+(offer.checkout.maximum_total_minor!==undefined?', maximum '+wizardMinorToDisplay(offer.checkout.maximum_total_minor,offer.currency)+' '+offer.currency:'')+(offer.checkout.automatic_tax?', automatic tax':'')+(offer.checkout.allow_promotion_codes?', promotion codes':'');target.appendChild(options);
  }catch(error){productWizardShowError(error.message)}
}
async function productWizardRequest(path,method,body){
  var response=await fetch(path,{method:method,credentials:'same-origin',headers:{'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});var data={};try{data=await response.json()}catch(_){}
  if(!response.ok)throw new Error(data.message||data.error||'The server rejected the product configuration.');return data;
}
async function submitProductWizard(intent){
  productWizardClearError();var buttons=[wizardById('wizard-save-draft'),wizardById('wizard-publish')];var productId='';
  try{
    var built=buildProductWizardPayload();buttons.forEach(function(button){button.disabled=true});
    var config=window.__productWizardConfig;var created=await productWizardRequest(config.product_collection,'POST',built.product);productId=created.id;
    if(!productId)throw new Error('Product creation returned no product ID.');
    var offerCollection=config.product_collection+'/'+encodeURIComponent(productId)+'/offers';var managed=await productWizardRequest(offerCollection,'POST',built.offer);var offerId=managed.offer&&managed.offer.id;
    if(!offerId)throw new Error('Pricing creation returned no offer ID.');
    if(intent==='publish'){
      await productWizardRequest(offerCollection+'/'+encodeURIComponent(offerId)+'/publish','POST',{});
      await productWizardRequest(config.product_collection+'/'+encodeURIComponent(productId),'PATCH',{status:'active'});
    }
    window.location.assign(config.return_url+'?created='+encodeURIComponent(productId)+(intent==='publish'?'&published=1':''));
  }catch(error){
    productWizardShowError((productId?'Product draft '+productId+' was created, but setup did not finish. ':'')+(error.message||'Product setup failed.'));
    buttons.forEach(function(button){button.disabled=false});
  }
}
function initProductWizard(){productWizardTemplateChanged();productWizardShippingChanged();productWizardShowStep(1,false)}
"#;

// ---------------------------------------------------------------------------
// Shared admin/seller product lifecycle manager
// ---------------------------------------------------------------------------

fn commerce_wire<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn amount_rule_summary(amount: &AmountRule, currency: &str) -> String {
    match amount {
        AmountRule::Fixed { unit_amount_minor } => display_money(*unit_amount_minor, currency),
        AmountRule::PerUnit {
            input,
            unit_amount_minor,
        } => format!(
            "{} per {input}",
            display_money(*unit_amount_minor, currency)
        ),
        AmountRule::FlatPlusPerUnit {
            base_amount_minor,
            input,
            unit_amount_minor,
        } => format!(
            "{} + {} per {input}",
            display_money(*base_amount_minor, currency),
            display_money(*unit_amount_minor, currency)
        ),
        AmountRule::Lookup { input, prices } => {
            format!("{} configured prices selected by {input}", prices.len())
        }
        AmountRule::Graduated { input, tiers } => {
            format!("{} graduated tiers based on {input}", tiers.len())
        }
        AmountRule::Volume { input, tiers } => {
            format!("{} volume tiers based on {input}", tiers.len())
        }
        AmountRule::Package {
            input,
            units_per_package,
            package_amount_minor,
            rounding,
        } => format!(
            "{} per {units_per_package} {input} ({})",
            display_money(*package_amount_minor, currency),
            commerce_wire(rounding)
        ),
    }
}

fn offer_definition_json(managed: &ManagedOffer) -> String {
    let Ok(mut value) = serde_json::to_value(&managed.offer) else {
        return "{}".to_string();
    };
    if let Some(object) = value.as_object_mut() {
        for field in [
            "id",
            "product_id",
            "version",
            "stripe_product_id",
            "stripe_price_id",
        ] {
            object.remove(field);
        }
        if let Some(components) = object
            .get_mut("components")
            .and_then(serde_json::Value::as_array_mut)
        {
            for component in components {
                if let Some(component) = component.as_object_mut() {
                    component.remove("id");
                    component.remove("stripe_price_id");
                }
            }
        }
    }
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
}

fn preset_defaults_json(managed: &ManagedOffer) -> String {
    let defaults = managed
        .offer
        .variables
        .iter()
        .filter_map(|variable| {
            variable
                .default_value
                .clone()
                .map(|value| (variable.key.clone(), value))
        })
        .collect::<serde_json::Map<String, serde_json::Value>>();
    serde_json::to_string_pretty(&defaults).unwrap_or_else(|_| "{}".to_string())
}

fn variable_default_text(variable: &VariableDefinition) -> String {
    match variable.default_value.as_ref() {
        Some(serde_json::Value::String(value)) => value.clone(),
        Some(serde_json::Value::Number(value)) => value.to_string(),
        Some(serde_json::Value::Bool(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn variable_default_selected(variable: &VariableDefinition, option: &str) -> bool {
    match variable.default_value.as_ref() {
        Some(serde_json::Value::String(value)) => value == option,
        Some(serde_json::Value::Array(values)) => {
            values.iter().any(|value| value.as_str() == Some(option))
        }
        _ => false,
    }
}

fn render_offer_variable_input(
    variable: &VariableDefinition,
    offer_id: &str,
    purpose: &str,
) -> Markup {
    let id = format!("{purpose}-{offer_id}-{}", variable.key);
    let data_attribute = if purpose == "preset" {
        "preset"
    } else {
        "preview"
    };
    let kind = commerce_wire(&variable.kind);
    let value = variable_default_text(variable);
    html! {
        div .form-group {
            label .form-label for=(id) {
                (variable.label)
                @if variable.required { " *" }
            }
            @match variable.kind {
                VariableKind::Boolean => {
                    label style="display:flex;align-items:center;gap:.5rem;min-height:2.5rem" {
                        input id=(id) type="checkbox" data-offer-variable=(data_attribute) data-variable-key=(variable.key) data-variable-kind=(kind) checked[variable.default_value == Some(serde_json::Value::Bool(true))];
                        "Yes"
                    }
                }
                VariableKind::Select => {
                    select id=(id) .form-select data-offer-variable=(data_attribute) data-variable-key=(variable.key) data-variable-kind=(kind) required[variable.required] {
                        @if !variable.required { option value="" { "Choose…" } }
                        @for option in &variable.allowed_values {
                            option value=(option) selected[variable_default_selected(variable, option)] { (option) }
                        }
                    }
                }
                VariableKind::MultiSelect => {
                    select id=(id) .form-select multiple data-offer-variable=(data_attribute) data-variable-key=(variable.key) data-variable-kind=(kind) required[variable.required] {
                        @for option in &variable.allowed_values {
                            option value=(option) selected[variable_default_selected(variable, option)] { (option) }
                        }
                    }
                }
                VariableKind::Number | VariableKind::Integer => {
                    input id=(id) .form-input type="number" inputmode=(if variable.kind == VariableKind::Integer { "numeric" } else { "decimal" })
                        step=(variable.step.as_deref().unwrap_or(if variable.kind == VariableKind::Integer { "1" } else { "any" }))
                        min=[variable.minimum.as_deref()] max=[variable.maximum.as_deref()]
                        value=(value) required[variable.required]
                        data-offer-variable=(data_attribute) data-variable-key=(variable.key) data-variable-kind=(kind);
                }
                VariableKind::Date | VariableKind::DateTime => {
                    input id=(id) .form-input type=(if variable.kind == VariableKind::Date { "date" } else { "datetime-local" })
                        min=[variable.minimum.as_deref()] max=[variable.maximum.as_deref()]
                        value=(value) required[variable.required]
                        data-offer-variable=(data_attribute) data-variable-key=(variable.key) data-variable-kind=(kind);
                }
                VariableKind::Text => {
                    input id=(id) .form-input type="text" value=(value) required[variable.required]
                        maxlength=[variable.maximum_length]
                        data-offer-variable=(data_attribute) data-variable-key=(variable.key) data-variable-kind=(kind);
                }
            }
            @if !variable.help_text.is_empty() { p .text-muted .text-sm { (variable.help_text) } }
        }
    }
}

fn render_managed_offer(managed: &ManagedOffer, product_api_url: &str) -> Markup {
    let offer = &managed.offer;
    let offer_url = format!("{product_api_url}/offers/{}", offer.id);
    let preview_url = format!("{offer_url}/preview");
    let presets_url = format!("{offer_url}/presets");
    let links_url = format!("{offer_url}/payment-links");
    let status = commerce_wire(&managed.status);
    let definition = offer_definition_json(managed);
    let preset_defaults = preset_defaults_json(managed);
    let product_id = &offer.product_id;
    let hosted_snippet = format!(
        "<script src=\"https://YOUR-IMPRESSPRESS-DOMAIN/b/products/storefront.js\" defer></script>\n<impresspress-product api-base=\"https://YOUR-IMPRESSPRESS-DOMAIN\" product-id=\"{product_id}\" presentation=\"hosted\" credentials=\"omit\"></impresspress-product>"
    );
    let embedded_snippet =
        hosted_snippet.replace("presentation=\"hosted\"", "presentation=\"embedded\"");
    let charge_label = if commerce_wire(&offer.mode) == "subscription" {
        "Subscription"
    } else {
        "One-time payment"
    };
    let pricing_label = commerce_wire(&offer.pricing_model).replace('_', " ");
    html! {
        section .card data-offer-card data-offer-id=(offer.id) data-offer-url=(offer_url) data-preview-url=(preview_url) data-presets-url=(presets_url) data-links-url=(links_url) data-currency=(offer.currency) style="margin-top:1rem" {
            header .card__head {
                div {
                    div style="display:flex;align-items:center;gap:.5rem;flex-wrap:wrap" {
                        h3 .card__title style="margin:0" { (offer.name) }
                        (components::status_badge(&status))
                        span .badge .badge-secondary { "v" (offer.version) }
                    }
                    p .text-muted .text-sm style="margin:.25rem 0 0" {
                        (charge_label) " · " (pricing_label) " pricing · " (offer.currency)
                        @if let Some(interval) = offer.recurring_interval {
                            " · every " (offer.interval_count) " " (commerce_wire(&interval))
                        }
                        @if managed.sync_status == "failed" { " · Stripe needs attention" }
                        @else if managed.sync_status == "synced" { " · Synced with Stripe" }
                    }
                }
                div .products-actions {
                    @if managed.status == OfferStatus::Draft {
                        button .btn .btn--primary .btn--sm type="button" onclick="productManagerOpenVisualEditor(this)" { "Edit visually" }
                        button .btn .btn--primary .btn--sm type="button" onclick="productManagerOfferAction(this,'publish')" { "Publish" }
                    }
                    @if managed.status == OfferStatus::Active {
                        button .btn .btn--secondary .btn--sm type="button" onclick="productManagerOfferAction(this,'sync')" {
                            @if managed.sync_status == "failed" {
                                "Retry Stripe sync"
                            } @else if managed.sync_status == "synced" {
                                "Reconcile Stripe"
                            } @else {
                                "Sync to Stripe"
                            }
                        }
                    }
                    button .btn .btn--secondary .btn--sm type="button" onclick="productManagerOfferAction(this,'duplicate')" { "Duplicate to draft" }
                    @if managed.status != OfferStatus::Archived {
                        button .btn .btn--secondary .btn--sm type="button" onclick="productManagerOfferAction(this,'archive')" { "Archive" }
                    }
                }
            }
            div .card__body {
                p data-offer-error .login-error role="alert" aria-live="assertive" hidden {}
                @if !managed.sync_error.is_empty() {
                    div .login-error role="alert" { "Stripe sync error: " (managed.sync_error) }
                }
                @if managed.status != OfferStatus::Archived {
                    details .products-advanced {
                        summary { "Preview a customer price" }
                        div .products-advanced__body {
                            div .products-section__head {
                                div {
                                    h4 { "Test checkout price" }
                                    p .text-muted .text-sm { "Enter a typical order to confirm the amount customers will see." }
                                }
                                button .btn .btn--secondary .btn--sm type="button" onclick="productManagerPreview(this)" { "Calculate preview" }
                            }
                            div data-preview-inputs .products-form-grid .products-form-grid--compact {
                                div .form-group {
                                    label .form-label for=(format!("preview-{}-quantity", offer.id)) { "Quantity" }
                                    input .form-input id=(format!("preview-{}-quantity", offer.id)) data-preview-quantity type="number" min="1" step="1" value="1" required;
                                }
                                @for variable in &offer.variables { (render_offer_variable_input(variable, &offer.id, "preview")) }
                            }
                            div data-pricing-preview aria-live="polite" {}
                        }
                    }
                }
                div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:1rem" {
                    div {
                        h4 style="margin:.25rem 0" { "Customer fields" }
                        @if offer.variables.is_empty() {
                            p .text-muted .text-sm { "No customer fields" }
                        } @else {
                            ul style="margin:.5rem 0;padding-left:1.25rem" {
                                @for variable in &offer.variables {
                                    li { (variable.label) " (" (commerce_wire(&variable.kind)) ")" @if variable.required { " — required" } }
                                }
                            }
                        }
                    }
                    div {
                        h4 style="margin:.25rem 0" { "Itemized price rows" }
                        ul style="margin:.5rem 0;padding-left:1.25rem" {
                            @for component in &offer.components {
                                li { strong { (component.label) } ": " (amount_rule_summary(&component.amount, &offer.currency)) }
                            }
                        }
                    }
                    div {
                        h4 style="margin:.25rem 0" { "Checkout" }
                        p .text-muted .text-sm {
                            (offer.components.len()) " row(s), " (offer.variables.len()) " input(s)"
                            @if let Some(minimum) = offer.checkout.minimum_total_minor { ", minimum " (display_money(minimum, &offer.currency)) }
                            @if let Some(maximum) = offer.checkout.maximum_total_minor { ", maximum " (display_money(maximum, &offer.currency)) }
                            @if offer.checkout.automatic_tax { ", automatic tax" }
                            @if offer.checkout.allow_promotion_codes { ", promotion codes" }
                            @if offer.checkout.trial_days > 0 { ", " (offer.checkout.trial_days) " trial days" }
                        }
                    }
                }
                @if managed.status == OfferStatus::Draft {
                    details style="margin-top:1rem" {
                        summary style="cursor:pointer;font-weight:600" { "Advanced draft definition" }
                        p .text-muted .text-sm { "Edit the complete typed offer JSON. Published offers are immutable; duplicate one to create an editable draft." }
                        textarea .form-textarea data-offer-definition rows="18" spellcheck="false" { (definition) }
                        button .btn .btn--primary .btn--sm type="button" style="margin-top:.75rem" onclick="productManagerSaveOffer(this)" { "Save draft definition" }
                    }
                }
                @if managed.status == OfferStatus::Active {
                    section style="border-top:1px solid var(--border-color);margin-top:1.25rem;padding-top:1.25rem" {
                        h4 style="margin:0" { "Shareable Stripe Payment Links" }
                        p .text-muted .text-sm { "Create a hosted checkout link you can paste into an email, button, or social post. Products with choices save those choices as a reusable preset." }
                        @if !offer.variables.is_empty() {
                            div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:1rem" {
                                div .form-group {
                                    label .form-label { "Preset name" }
                                    input .form-input data-preset-name type="text" value=(format!("{} share link", offer.name));
                                }
                                div .form-group {
                                    label .form-label { "Preset slug (optional)" }
                                    input .form-input data-preset-slug type="text" pattern="[a-z0-9]+(?:-[a-z0-9]+)*" placeholder="team-five";
                                }
                                @for variable in &offer.variables { (render_offer_variable_input(variable, &offer.id, "preset")) }
                            }
                            details style="margin:.5rem 0 1rem" {
                                summary style="cursor:pointer" { "Advanced preset JSON" }
                                textarea .form-textarea data-preset-values rows="5" spellcheck="false" { (preset_defaults) }
                            }
                        }
                        div .form-group style="max-width:620px" {
                            label .form-label { "After-completion URL (optional)" }
                            input .form-input data-link-completion-url type="url" placeholder="https://example.com/thank-you";
                        }
                        div style="display:flex;gap:.5rem;flex-wrap:wrap" {
                            button .btn .btn--primary .btn--sm type="button" data-create-link onclick="productManagerCreateLink(this)" { "+ Create or reuse Payment Link" }
                            @if !offer.variables.is_empty() {
                                button .btn .btn--secondary .btn--sm type="button" onclick="productManagerNewPreset(this)" { "New preset" }
                            }
                        }
                        @if !offer.variables.is_empty() {
                            h5 style="margin-bottom:.25rem" { "Saved presets" }
                            div data-checkout-presets aria-live="polite" { p .text-muted .text-sm { "Loading presets…" } }
                        }
                        div data-payment-links style="margin-top:1rem" { p .text-muted .text-sm { "Loading Payment Links…" } }
                    }
                    details style="border-top:1px solid var(--border-color);margin-top:1.25rem;padding-top:1.25rem" {
                        summary style="cursor:pointer;font-weight:600" { "Hosted, embedded, and static-site integration" }
                        p .text-muted .text-sm { "The browser sends inputs to Impresspress for authoritative pricing. Replace the placeholder domain with this Impresspress deployment; secret Stripe keys never belong in static HTML." }
                        div .form-group {
                            label .form-label { "Hosted Checkout widget" }
                            textarea .form-textarea data-integration-snippet readonly rows="4" spellcheck="false" { (hosted_snippet) }
                            button .btn .btn--secondary .btn--sm type="button" style="margin-top:.5rem" onclick="productManagerCopyField(this)" { "Copy hosted snippet" }
                        }
                        div .form-group {
                            label .form-label { "Embedded Checkout widget" }
                            textarea .form-textarea data-integration-snippet readonly rows="4" spellcheck="false" { (embedded_snippet) }
                            button .btn .btn--secondary .btn--sm type="button" style="margin-top:.5rem" onclick="productManagerCopyField(this)" { "Copy embedded snippet" }
                        }
                    }
                }
            }
        }
    }
}

pub async fn product_manager(
    ctx: &dyn Context,
    msg: &Message,
    product_id: &str,
    admin: bool,
) -> OutputStream {
    let product = match db::get(ctx, PRODUCTS_TABLE, product_id).await {
        Ok(product) => product,
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => {
            return crate::http::err_not_found("Product not found");
        }
        Err(error) => return crate::http::err_internal("Could not load product", error),
    };
    let deleted = product
        .data
        .get("deleted_at")
        .is_some_and(|value| !value.is_null() && value.as_str() != Some(""));
    if deleted {
        return crate::http::err_not_found("Product not found");
    }
    if !admin {
        let user_id = msg.user_id();
        if user_id.is_empty()
            || (product.str_field("owner_id") != user_id
                && product.str_field("created_by") != user_id)
        {
            return crate::http::err_not_found("Product not found");
        }
    }
    let offers = match repo::offers::list_for_product(ctx, product_id).await {
        Ok(offers) => offers,
        Err(error) => return crate::http::err_internal("Could not load product pricing", error),
    };
    let seller_enabled = !admin && super::handlers::user_products_enabled(ctx).await;
    let product_api_url = if admin {
        format!("/b/products/api/admin/products/{product_id}")
    } else {
        format!("/b/products/api/products/{product_id}")
    };
    let back_href = if admin {
        "/b/products/admin/manage"
    } else {
        "/b/products/my-products"
    };
    let detail_base_url = if admin {
        "/b/products/admin/products/"
    } else {
        "/b/products/my-products/"
    };
    let page_config = serde_json::json!({
        "product_url": product_api_url,
        "detail_base_url": detail_base_url,
    });
    let status = product.str_field("status");
    let approval = product.str_field("approval_status");
    let content = html! {
        @if admin { (admin_tabs("products")) } @else { (portal_tabs("products", seller_enabled)) }
        (components::page_header(
            product.str_field("name"),
            Some("Update what customers see, manage pricing, and share checkout"),
            Some(html! { a .btn .btn--secondary .btn--sm href=(back_href) { "Back to products" } }),
        ))
        p #product-manager-error .login-error role="alert" aria-live="assertive" hidden {}
        section .card {
            header .card__head {
                div {
                    div .products-status-stack {
                        h3 .card__title style="margin:0" { "Product details" }
                        (components::status_badge(status))
                        @if product.str_field("owner_kind") == "user" { span .badge .badge-secondary { "Review: " (approval) } }
                    }
                    @if !admin && status == "pending_review" {
                        p .text-muted .text-sm style="margin:.25rem 0 0" { "This product is awaiting administrator review and is not public yet." }
                    }
                }
                div .products-actions {
                    @if admin && product.str_field("owner_kind") == "user" && status == "pending_review" && approval == "pending" {
                        button .btn .btn--primary .btn--sm type="button" data-moderation-action="approve" onclick="productManagerModerate(this,'approve')" { "Approve listing" }
                        button .btn .btn--secondary .btn--sm type="button" data-moderation-action="reject" onclick="productManagerModerate(this,'reject')" { "Return to seller" }
                    }
                    button .btn .btn--secondary .btn--sm type="button" onclick="productManagerDuplicate(this)" { "Duplicate product" }
                    @if status != "active" && status != "pending_review" {
                        button .btn .btn--primary .btn--sm type="button" onclick="productManagerSetStatus('active',this)" { @if admin { "Publish product" } @else { "Submit for publication" } }
                    }
                    @if status != "archived" {
                        button .btn .btn--secondary .btn--sm type="button" onclick="productManagerSetStatus('archived',this)" { "Archive product" }
                    }
                    @if status == "active" && approval == "approved" {
                        a .btn .btn--secondary .btn--sm href=(format!("/b/products/catalog/{product_id}")) target="_blank" rel="noopener" { "View storefront" }
                    }
                }
            }
            div .card__body {
                form #product-manager-form onsubmit="productManagerSaveProduct(event)" {
                    div .form-group { label .form-label .required for="manager-product-name" { "Product name" } input #manager-product-name .form-input type="text" maxlength="160" required value=(product.str_field("name")); }
                    div .form-group { label .form-label for="manager-product-description" { "Customer-facing description" } textarea #manager-product-description .form-textarea maxlength="4000" { (product.str_field("description")) } }
                    details .products-advanced {
                        summary { "More product details (optional)" }
                        div .products-advanced__body {
                            div .products-form-grid {
                                div .form-group { label .form-label for="manager-product-slug" { "Web address" } input #manager-product-slug .form-input type="text" maxlength="160" value=(product.str_field("slug")); }
                                div .form-group { label .form-label for="manager-product-image" { "Image URL" } input #manager-product-image .form-input type="url" value=(product.str_field("image_url")); }
                                div .form-group {
                                    label .form-label for="manager-product-fulfillment" { "How it is delivered" }
                                    select #manager-product-fulfillment .form-select {
                                        @for (value, label) in [("none", "No automatic delivery"), ("manual", "Handled manually"), ("download", "Digital download"), ("entitlement", "Grant access"), ("webhook", "Notify another system")] {
                                            option value=(value) selected[product.str_field("fulfillment_kind") == value] { (label) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    button .btn .btn--primary .btn--sm type="submit" { "Save product details" }
                }
            }
        }
        section #product-manager-visual-editor .card hidden style="margin-top:1.5rem" {
            header .card__head {
                div {
                    h3 #manager-visual-title .card__title { "Edit pricing draft" }
                    p .text-muted .text-sm style="margin:.25rem 0 0" { "Manage customer inputs, itemized price rows, conditions, and recurring terms without editing JSON." }
                }
                button .btn .btn--secondary .btn--sm type="button" onclick="productManagerCloseVisualEditor()" { "Close editor" }
            }
            div .card__body {
                div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:1rem" {
                    div .form-group {
                        label .form-label .required for="manager-visual-offer-name" { "Offer name" }
                        input #manager-visual-offer-name .form-input type="text" maxlength="160" required;
                    }
                    div .form-group {
                        label .form-label for="manager-visual-mode" { "Charge type" }
                        select #manager-visual-mode .form-select onchange="productManagerVisualModeChanged()" { option value="payment" { "One-time payment" } option value="subscription" { "Subscription" } }
                    }
                    div .form-group {
                        label .form-label .required for="manager-visual-currency" { "Currency" }
                        input #manager-visual-currency .form-input type="text" maxlength="3" required;
                    }
                    div .form-group data-manager-recurring hidden {
                        label .form-label for="manager-visual-interval" { "Billing interval" }
                        select #manager-visual-interval .form-select { option value="day" { "Day" } option value="week" { "Week" } option value="month" { "Month" } option value="year" { "Year" } }
                    }
                    div .form-group data-manager-recurring hidden {
                        label .form-label for="manager-visual-interval-count" { "Every" }
                        input #manager-visual-interval-count .form-input type="number" min="1" max="36" step="1" value="1";
                    }
                }
                section style="margin-top:1rem" {
                    div style="display:flex;align-items:center;justify-content:space-between;gap:1rem;flex-wrap:wrap" {
                        div { h4 style="margin:0" { "Customer fields" } p .text-muted .text-sm { "Typed quantities, choices, flags, and text used by price rows." } }
                        button .btn .btn--secondary .btn--sm type="button" onclick="addWizardVariable()" { "+ Add input" }
                    }
                    div #wizard-variables {}
                }
                section style="margin-top:1.5rem" {
                    div style="display:flex;align-items:center;justify-content:space-between;gap:1rem;flex-wrap:wrap" {
                        div { h4 style="margin:0" { "Itemized price rows" } p .text-muted .text-sm { "Fixed, per-unit, lookup, tiered, package, and conditional rows are supported." } }
                        button .btn .btn--secondary .btn--sm type="button" onclick="addWizardComponent()" { "+ Add row" }
                    }
                    div #wizard-components {}
                }
                p .text-muted .text-sm { "Checkout collection, shipping, tax, and fulfillment settings remain unchanged. Advanced nested conditions and quantity rules are preserved when saved." }
                div style="display:flex;gap:.5rem;margin-top:1rem;flex-wrap:wrap" {
                    button .btn .btn--primary .btn--sm type="button" onclick="productManagerSaveVisualOffer(this)" { "Save visual changes" }
                    button .btn .btn--secondary .btn--sm type="button" onclick="productManagerCloseVisualEditor()" { "Cancel" }
                }
            }
        }
        section .products-section {
            div .products-section__head {
                div { h2 { "Prices and checkout" } p .text-muted .text-sm { "Published offers are immutable so existing orders and links retain their exact terms." } }
            }
            @if offers.is_empty() {
                (components::empty_state(icons::dollar_sign(), "No pricing offers", "This product does not have a checkout price yet.", Some(html! {
                    a .btn .btn--primary .btn--sm href="/b/products/admin/new" { "Create a product with pricing" }
                })))
            } @else {
                @for offer in &offers { (render_managed_offer(offer, &product_api_url)) }
            }
        }
        script { (maud::PreEscaped(format!("window.__productManagerConfig={page_config};\n{PRODUCT_WIZARD_JS}\n{PRODUCT_MANAGER_JS}\ninitProductManager();"))) }
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple(
            product.str_field("name"),
            if admin {
                ui::NavKind::Admin
            } else {
                ui::NavKind::Portal
            },
            "Products",
        ),
        content,
    )
    .await
}

const PRODUCT_MANAGER_JS: &str = r#"
function productManagerError(message){var target=document.getElementById('product-manager-error');target.textContent=message||'Something went wrong.';target.hidden=false;target.scrollIntoView({block:'nearest'});}
function productManagerClearError(){var target=document.getElementById('product-manager-error');target.hidden=true;target.textContent='';}
async function productManagerRequest(url,method,body){var options={method:method,credentials:'same-origin',headers:{Accept:'application/json'}};if(body!==undefined){options.headers['Content-Type']='application/json';options.body=JSON.stringify(body)}var response=await fetch(url,options),text=await response.text(),payload={};if(text){try{payload=JSON.parse(text)}catch(_error){payload={message:text}}}if(!response.ok)throw new Error(payload.message||payload.error||('Request failed ('+response.status+')'));return payload;}
function productManagerButton(button,busy){if(!button)return;button.disabled=busy;if(busy){button.dataset.originalText=button.textContent;button.textContent='Working…'}else if(button.dataset.originalText){button.textContent=button.dataset.originalText;delete button.dataset.originalText}}
var productManagerVisualCard=null;
var productManagerVisualDefinition=null;
function productManagerVisualModeChanged(){var recurring=document.getElementById('manager-visual-mode').value==='subscription';document.querySelectorAll('[data-manager-recurring]').forEach(function(field){field.hidden=!recurring})}
function productManagerComponentSeed(component,currency){var amount=component.amount||{},seed={key:component.key,label:component.label,description:component.description||'',amount_type:amount.type||'fixed',required:component.required!==false,amount:'0.00',input:amount.input||'',base_amount:'0.00',units_per_package:amount.units_per_package||1,rounding:amount.rounding||'up',details:'',preserved_quantity:component.quantity||{type:'fixed',value:1},preserved_metadata:component.metadata||{}};if(amount.type==='fixed')seed.amount=wizardMinorToDisplay(amount.unit_amount_minor||0,currency);else if(amount.type==='per_unit'||amount.type==='flat_plus_per_unit')seed.amount=wizardMinorToDisplay(amount.unit_amount_minor||0,currency);else if(amount.type==='package')seed.amount=wizardMinorToDisplay(amount.package_amount_minor||0,currency);if(amount.type==='flat_plus_per_unit')seed.base_amount=wizardMinorToDisplay(amount.base_amount_minor||0,currency);if(amount.type==='lookup')seed.details=Object.keys(amount.prices||{}).map(function(key){return key+' = '+wizardMinorToDisplay(amount.prices[key],currency)}).join('\n');if(amount.type==='graduated'||amount.type==='volume')seed.details=(amount.tiers||[]).map(function(tier){return (tier.up_to===undefined||tier.up_to===null?'*':tier.up_to)+' | '+wizardMinorToDisplay(tier.unit_amount_minor||0,currency)+' | '+wizardMinorToDisplay(tier.flat_amount_minor||0,currency)}).join('\n');var condition=component.condition||{op:'always'},simple=['always','present','equals','not_equals','greater_than','greater_than_or_equal','less_than','less_than_or_equal','contains','in'];if(simple.indexOf(condition.op)!==-1){seed.condition=condition.op;seed.condition_input=condition.input||'';seed.condition_value=condition.op==='in'?(condition.values||[]).join(', '):condition.value===undefined?'':String(condition.value)}else seed.preserved_condition=condition;return seed}
function productManagerOpenVisualEditor(button){var card=productManagerCard(button),source=card.querySelector('[data-offer-definition]'),definition;productManagerClearError();try{definition=JSON.parse(source.value)}catch(error){productManagerError('Draft definition is not valid JSON: '+error.message);return}productManagerVisualCard=card;productManagerVisualDefinition=definition;var currency=String(definition.currency||'USD').toUpperCase();document.getElementById('manager-visual-title').textContent='Edit '+(definition.name||'pricing draft');document.getElementById('manager-visual-offer-name').value=definition.name||'';document.getElementById('manager-visual-mode').value=definition.mode||'payment';document.getElementById('manager-visual-currency').value=currency;document.getElementById('manager-visual-interval').value=definition.recurring_interval||'month';document.getElementById('manager-visual-interval-count').value=definition.interval_count||1;document.getElementById('wizard-variables').replaceChildren();document.getElementById('wizard-components').replaceChildren();(definition.variables||[]).forEach(addWizardVariable);(definition.components||[]).forEach(function(component){addWizardComponent(productManagerComponentSeed(component,currency))});productManagerVisualModeChanged();var editor=document.getElementById('product-manager-visual-editor');editor.hidden=false;editor.scrollIntoView({block:'start'});document.getElementById('manager-visual-offer-name').focus()}
function productManagerCloseVisualEditor(){var editor=document.getElementById('product-manager-visual-editor');if(editor)editor.hidden=true;productManagerVisualCard=null;productManagerVisualDefinition=null;productManagerClearError()}
async function productManagerSaveVisualOffer(button){if(!productManagerVisualCard||!productManagerVisualDefinition){productManagerError('Choose a draft offer to edit first.');return}productManagerClearError();var nameField=document.getElementById('manager-visual-offer-name'),currencyField=document.getElementById('manager-visual-currency'),mode=document.getElementById('manager-visual-mode').value,currency=currencyField.value.trim().toUpperCase(),interval=document.getElementById('manager-visual-interval').value,intervalCount=Number(document.getElementById('manager-visual-interval-count').value||1),definition=JSON.parse(JSON.stringify(productManagerVisualDefinition));try{if(!nameField.value.trim())throw Object.assign(new Error('Offer name is required.'),{focus:nameField});if(!/^[A-Z]{3}$/.test(currency))throw Object.assign(new Error('Currency must be a three-letter ISO code.'),{focus:currencyField});if(mode==='subscription'&&(!Number.isSafeInteger(intervalCount)||intervalCount<1||intervalCount>36))throw Object.assign(new Error('Billing interval count must be between 1 and 36.'),{focus:document.getElementById('manager-visual-interval-count')});var variables=collectWizardVariables(),components=collectWizardComponents(variables,currency,mode==='subscription',interval,intervalCount);definition.name=nameField.value.trim();definition.mode=mode;definition.currency=currency;definition.recurring_interval=mode==='subscription'?interval:null;definition.interval_count=mode==='subscription'?intervalCount:1;definition.variables=variables;definition.components=components;definition.pricing_model=variables.length||components.length!==1||components[0].amount.type!=='fixed'?'components':'fixed';definition.billing_scheme=components.some(function(component){return component.amount.type==='graduated'||component.amount.type==='volume'})?'tiered':'per_unit'}catch(error){productManagerError(error.message);if(error.focus)error.focus.focus();return}productManagerButton(button,true);try{await productManagerRequest(productManagerVisualCard.dataset.offerUrl,'PATCH',definition);var source=productManagerVisualCard.querySelector('[data-offer-definition]');if(source)source.value=JSON.stringify(definition,null,2);window.location.reload()}catch(error){productManagerError(error.message);productManagerButton(button,false)}}
async function productManagerSaveProduct(event){event.preventDefault();productManagerClearError();var button=event.currentTarget.querySelector('button[type="submit"]');productManagerButton(button,true);try{await productManagerRequest(window.__productManagerConfig.product_url,'PATCH',{name:document.getElementById('manager-product-name').value.trim(),slug:document.getElementById('manager-product-slug').value.trim(),description:document.getElementById('manager-product-description').value.trim(),image_url:document.getElementById('manager-product-image').value.trim(),fulfillment_kind:document.getElementById('manager-product-fulfillment').value});window.location.reload()}catch(error){productManagerError(error.message);productManagerButton(button,false)}}
async function productManagerSetStatus(status,button){if(status==='archived'&&!window.confirm('Archive this product? Public checkout will no longer be available.'))return;productManagerClearError();productManagerButton(button,true);try{await productManagerRequest(window.__productManagerConfig.product_url,'PATCH',{status:status});window.location.reload()}catch(error){productManagerError(error.message);productManagerButton(button,false)}}
async function productManagerDuplicate(button){productManagerClearError();productManagerButton(button,true);try{var result=await productManagerRequest(window.__productManagerConfig.product_url+'/duplicate','POST',{}),id=result.product&&result.product.id;if(!id)throw new Error('Product duplication returned no product ID');window.location.assign(window.__productManagerConfig.detail_base_url+encodeURIComponent(id))}catch(error){productManagerError(error.message);productManagerButton(button,false)}}
async function productManagerModerate(button,decision){if(decision==='reject'&&!window.confirm('Return this listing to the seller as a draft?'))return;productManagerClearError();productManagerButton(button,true);try{await productManagerRequest(window.__productManagerConfig.product_url+'/'+decision,'POST',{});window.location.reload()}catch(error){productManagerError(error.message);productManagerButton(button,false)}}
function productManagerCard(button){return button.closest('[data-offer-card]')}
function productManagerCardError(card,message,focus){var target=card&&card.querySelector('[data-offer-error]');if(!target){productManagerError(message);return}target.textContent=message||'Something went wrong.';target.hidden=false;if(focus&&typeof focus.focus==='function')focus.focus();target.scrollIntoView({block:'nearest'});}
function productManagerClearCardError(card){var target=card&&card.querySelector('[data-offer-error]');if(target){target.textContent='';target.hidden=true}card&&card.querySelectorAll('[aria-invalid="true"]').forEach(function(input){input.removeAttribute('aria-invalid')})}
function productManagerInputs(card,purpose){var inputs={};card.querySelectorAll('[data-offer-variable="'+purpose+'"]').forEach(function(input){var key=input.dataset.variableKey,kind=input.dataset.variableKind,value;if(!key)return;if(!input.checkValidity()){input.setAttribute('aria-invalid','true');throw Object.assign(new Error((input.labels&&input.labels[0]?input.labels[0].textContent:key)+' is invalid.'),{focus:input})}if(kind==='boolean')value=input.checked;else if(kind==='multi_select')value=Array.from(input.selectedOptions,function(option){return option.value});else if(kind==='integer'){if(input.value==='')return;value=Number(input.value);if(!Number.isSafeInteger(value))throw Object.assign(new Error(key+' must be a whole number.'),{focus:input})}else if(kind==='number'){if(input.value==='')return;value=Number(input.value);if(!Number.isFinite(value))throw Object.assign(new Error(key+' must be a number.'),{focus:input})}else{if(input.value==='')return;value=input.value}inputs[key]=value});return inputs}
function productManagerCurrencyExponent(currency){return ['BIF','CLP','DJF','GNF','JPY','KMF','KRW','MGA','PYG','RWF','UGX','VND','VUV','XAF','XOF','XPF'].indexOf(currency)!==-1?0:['BHD','JOD','KWD','OMR','TND'].indexOf(currency)!==-1?3:2}
function productManagerMoney(minor,currency){currency=String(currency||'USD').toUpperCase();var places=productManagerCurrencyExponent(currency),value;try{value=BigInt(String(minor))}catch(_error){return currency+' —'}var negative=value<0n;if(negative)value=-value;var divisor=10n**BigInt(places),whole=value/divisor,fraction=value%divisor;return (negative?'-':'')+(places?whole+'.'+fraction.toString().padStart(places,'0'):whole.toString())+' '+currency}
function productManagerRenderPreview(card,preview){var target=card.querySelector('[data-pricing-preview]');target.replaceChildren();var list=document.createElement('div');(preview.components||[]).forEach(function(component){var row=document.createElement('div');row.style.cssText='display:flex;justify-content:space-between;gap:1rem;padding:.35rem 0;border-bottom:1px solid var(--border-color)';var label=document.createElement('span');label.textContent=component.label+(component.included?'':' — not included');var amount=document.createElement('strong');amount.textContent=component.included?productManagerMoney(component.total_amount_minor,preview.amounts.currency):component.reason;row.append(label,amount);list.appendChild(row)});target.appendChild(list);var total=document.createElement('div');total.style.cssText='display:flex;justify-content:space-between;gap:1rem;padding:.75rem 0;font-size:1.08rem';var totalLabel=document.createElement('strong');totalLabel.textContent='Item total';var totalValue=document.createElement('strong');totalValue.textContent=productManagerMoney(preview.amounts.total_minor,preview.amounts.currency);total.append(totalLabel,totalValue);target.appendChild(total)}
async function productManagerPreview(button){var card=productManagerCard(button),quantityInput=card.querySelector('[data-preview-quantity]');productManagerClearCardError(card);productManagerButton(button,true);try{if(!quantityInput.checkValidity())throw Object.assign(new Error('Checkout quantity must be a positive whole number.'),{focus:quantityInput});var quantity=Number(quantityInput.value);if(!Number.isSafeInteger(quantity)||quantity<1)throw Object.assign(new Error('Checkout quantity must be a positive whole number.'),{focus:quantityInput});var preview=await productManagerRequest(card.dataset.previewUrl,'POST',{offer_id:card.dataset.offerId,quantity:quantity,inputs:productManagerInputs(card,'preview')});productManagerRenderPreview(card,preview)}catch(error){productManagerCardError(card,error.message,error.focus)}finally{productManagerButton(button,false)}}
async function productManagerOfferAction(button,action){var card=productManagerCard(button);if(action==='archive'&&!window.confirm('Archive this immutable offer? Existing order snapshots remain unchanged.'))return;productManagerClearError();productManagerButton(button,true);try{var method=action==='archive'?'DELETE':'POST',url=card.dataset.offerUrl+(action==='archive'?'':'/'+action);await productManagerRequest(url,method,action==='archive'?undefined:{});window.location.reload()}catch(error){productManagerError(error.message);productManagerButton(button,false)}}
async function productManagerSaveOffer(button){var card=productManagerCard(button),definition;productManagerClearError();try{definition=JSON.parse(card.querySelector('[data-offer-definition]').value)}catch(error){productManagerError('Offer definition is not valid JSON: '+error.message);return}productManagerButton(button,true);try{await productManagerRequest(card.dataset.offerUrl,'PATCH',definition);window.location.reload()}catch(error){productManagerError(error.message);productManagerButton(button,false)}}
function productManagerSetPresetInputs(card,inputs){inputs=inputs||{};card.querySelectorAll('[data-offer-variable="preset"]').forEach(function(input){var value=inputs[input.dataset.variableKey],kind=input.dataset.variableKind;if(kind==='boolean')input.checked=value===true;else if(kind==='multi_select')Array.from(input.options).forEach(function(option){option.selected=Array.isArray(value)&&value.indexOf(option.value)!==-1});else input.value=value===undefined||value===null?'':String(value)})}
function productManagerNewPreset(button){var card=productManagerCard(button);delete card.dataset.editPresetId;var name=card.querySelector('[data-preset-name]'),slug=card.querySelector('[data-preset-slug]'),action=card.querySelector('[data-create-link]');if(name)name.value=name.defaultValue;if(slug)slug.value='';card.querySelectorAll('[data-offer-variable="preset"]').forEach(function(input){if(input.type==='checkbox')input.checked=input.defaultChecked;else if(input.tagName==='SELECT')Array.from(input.options).forEach(function(option){option.selected=option.defaultSelected});else input.value=input.defaultValue});if(action)action.textContent='+ Create or reuse Payment Link';productManagerClearCardError(card)}
function productManagerEditPreset(card,preset){card.dataset.editPresetId=preset.id;var name=card.querySelector('[data-preset-name]'),slug=card.querySelector('[data-preset-slug]'),action=card.querySelector('[data-create-link]');if(name)name.value=preset.name||'';if(slug)slug.value=preset.slug||'';productManagerSetPresetInputs(card,preset.inputs);if(action)action.textContent='Update preset and create/reuse link';var first=name||card.querySelector('[data-offer-variable="preset"]');if(first)first.focus()}
async function productManagerArchivePreset(card,preset){if(!window.confirm('Archive preset '+preset.name+'? Existing Payment Links keep their immutable configuration.'))return;try{await productManagerRequest(card.dataset.presetsUrl+'/'+encodeURIComponent(preset.id),'DELETE');if(card.dataset.editPresetId===preset.id)productManagerNewPreset(card.querySelector('[data-create-link]'));await productManagerLoadPresets(card)}catch(error){productManagerCardError(card,error.message)}}
async function productManagerLoadPresets(card){var target=card.querySelector('[data-checkout-presets]');if(!target)return;target.textContent='Loading presets…';try{var payload=await productManagerRequest(card.dataset.presetsUrl,'GET'),presets=payload.presets||[];target.replaceChildren();if(!presets.length){target.textContent='No saved presets yet.';return}presets.forEach(function(preset){var row=document.createElement('div');row.style.cssText='display:flex;align-items:center;gap:.5rem;flex-wrap:wrap;padding:.4rem 0;border-bottom:1px solid var(--border-color)';var status=document.createElement('span');status.className='badge '+(preset.active?'badge-success':'badge-secondary');status.textContent=preset.active?'Active':'Archived';var name=document.createElement('strong');name.textContent=preset.name;var values=document.createElement('span');values.className='text-muted text-sm';values.textContent=JSON.stringify(preset.inputs||{});row.append(status,name,values);if(preset.active){var edit=document.createElement('button');edit.type='button';edit.className='btn btn--secondary btn--sm';edit.textContent='Edit preset';edit.onclick=function(){productManagerEditPreset(card,preset)};var archive=document.createElement('button');archive.type='button';archive.className='btn btn--secondary btn--sm';archive.textContent='Archive preset';archive.onclick=function(){productManagerArchivePreset(card,preset)};row.append(edit,archive)}target.appendChild(row)})}catch(error){target.textContent='Could not load presets: '+error.message}}
async function productManagerCreateLink(button){var card=productManagerCard(button),payload={};productManagerClearCardError(card);productManagerButton(button,true);try{var nameField=card.querySelector('[data-preset-name]');if(nameField){var name=nameField.value.trim();if(!name)throw Object.assign(new Error('Preset name is required.'),{focus:nameField});var slugField=card.querySelector('[data-preset-slug]'),slug=slugField?slugField.value.trim():'';if(slugField&&!slugField.checkValidity())throw Object.assign(new Error('Preset slug may contain lowercase letters, numbers, and single hyphens.'),{focus:slugField});var visual=card.querySelector('[data-offer-variable="preset"]'),inputs;if(visual)inputs=productManagerInputs(card,'preset');else{var values=card.querySelector('[data-preset-values]');try{inputs=JSON.parse(values.value)}catch(error){throw Object.assign(new Error('Preset values are not valid JSON: '+error.message),{focus:values})}}var editing=card.dataset.editPresetId,preset=await productManagerRequest(card.dataset.presetsUrl+(editing?'/'+encodeURIComponent(editing):''),editing?'PATCH':'POST',{name:name,slug:slug,inputs:inputs});if(!preset.id)throw new Error('Preset operation returned no ID');payload.preset_id=preset.id}var completion=card.querySelector('[data-link-completion-url]');if(completion&&completion.value.trim()){if(!completion.checkValidity())throw Object.assign(new Error('After-completion URL must be a valid absolute URL.'),{focus:completion});payload.after_completion_url=completion.value.trim()}await productManagerRequest(card.dataset.linksUrl,'POST',payload);await Promise.all([productManagerLoadPresets(card),productManagerLoadLinks(card)])}catch(error){productManagerCardError(card,error.message,error.focus)}finally{productManagerButton(button,false)}}
async function productManagerDeactivateLink(card,id){if(!window.confirm('Deactivate this Stripe Payment Link?'))return;try{await productManagerRequest(card.dataset.linksUrl+'/'+encodeURIComponent(id),'DELETE');await productManagerLoadLinks(card)}catch(error){productManagerError(error.message)}}
async function productManagerCopy(url,button){try{await navigator.clipboard.writeText(url);button.textContent='Copied';window.setTimeout(function(){button.textContent='Copy'},1200)}catch(_error){productManagerError('Copy failed. Open the link and copy it from the address bar.')}}
async function productManagerCopyField(button){var field=button.closest('.form-group').querySelector('[data-integration-snippet]');if(field)await productManagerCopy(field.value,button)}
async function productManagerRetryLink(card,link,button){productManagerClearCardError(card);productManagerButton(button,true);try{await productManagerRequest(card.dataset.linksUrl,'POST',link.preset_id?{preset_id:link.preset_id}:{});await productManagerLoadLinks(card)}catch(error){productManagerCardError(card,error.message)}finally{productManagerButton(button,false)}}
async function productManagerLoadLinks(card){var target=card.querySelector('[data-payment-links]');if(!target)return;target.textContent='Loading Payment Links…';try{var payload=await productManagerRequest(card.dataset.linksUrl,'GET'),links=payload.payment_links||[];target.replaceChildren();if(!links.length){target.textContent='No Payment Links yet.';return}links.forEach(function(link){var row=document.createElement('div');row.style.cssText='display:flex;align-items:center;gap:.5rem;flex-wrap:wrap;margin-top:.5rem';var failed=link.sync_status==='failed',status=document.createElement('span');status.className='badge '+(failed?'badge-danger':link.active?'badge-success':'badge-secondary');status.textContent=failed?'Sync failed':link.active?'Active':'Inactive';row.appendChild(status);if(link.url){var anchor=document.createElement('a');anchor.href=link.url;anchor.target='_blank';anchor.rel='noopener';anchor.textContent='Open hosted payment page';row.appendChild(anchor)}else{var pending=document.createElement('span');pending.className='text-muted text-sm';pending.textContent='Stripe link pending';row.appendChild(pending)}if(failed){var retry=document.createElement('button');retry.type='button';retry.className='btn btn--secondary btn--sm';retry.textContent='Retry link sync';retry.onclick=function(){productManagerRetryLink(card,link,retry)};row.appendChild(retry);if(link.sync_error){var error=document.createElement('span');error.className='text-muted text-sm';error.textContent=link.sync_error;row.appendChild(error)}}if(link.active&&link.url){var copy=document.createElement('button');copy.type='button';copy.className='btn btn--secondary btn--sm';copy.textContent='Copy';copy.onclick=function(){productManagerCopy(link.url,copy)};row.appendChild(copy);var deactivate=document.createElement('button');deactivate.type='button';deactivate.className='btn btn--secondary btn--sm';deactivate.textContent='Deactivate';deactivate.onclick=function(){productManagerDeactivateLink(card,link.id)};row.appendChild(deactivate)}target.appendChild(row)})}catch(error){target.textContent='Could not load Payment Links: '+error.message}}
function initProductManager(){document.querySelectorAll('[data-offer-card]').forEach(function(card){productManagerLoadLinks(card);productManagerLoadPresets(card)})}
"#;

const PRODUCT_CATALOG_ADMIN_JS: &str = r#"
function productCatalogById(id){return document.getElementById(id)}
function productCatalogError(message,focus){var target=productCatalogById('catalog-admin-error');target.textContent=message||'Something went wrong.';target.hidden=false;if(focus&&typeof focus.focus==='function')focus.focus();target.scrollIntoView({block:'nearest'})}
function productCatalogClearError(){var target=productCatalogById('catalog-admin-error');if(target){target.textContent='';target.hidden=true}}
function productCatalogBusy(button,busy){if(!button)return;button.disabled=busy;if(busy){button.dataset.originalText=button.textContent;button.textContent='Saving…'}else if(button.dataset.originalText){button.textContent=button.dataset.originalText;delete button.dataset.originalText}}
async function productCatalogRequest(url,method,body){var response=await fetch(url,{method:method,credentials:'same-origin',headers:{Accept:'application/json','Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)}),text=await response.text(),payload={};if(text){try{payload=JSON.parse(text)}catch(_error){payload={message:text}}}if(!response.ok)throw new Error(payload.message||payload.error||('Request failed ('+response.status+')'));return payload}
function productCatalogClose(){var editor=productCatalogById('group-editor');if(editor)editor.hidden=true;productCatalogClearError()}
function productCatalogNew(){productCatalogClearError();var editor=productCatalogById('group-editor');editor.hidden=false;productCatalogById('group-editor-id').value='';productCatalogById('group-editor-name').value='';productCatalogById('group-editor-description').value='';productCatalogById('group-editor-status').value='active';productCatalogById('group-editor-title').textContent='New group';editor.scrollIntoView({block:'start'});productCatalogById('group-editor-name').focus()}
function productCatalogEditGroup(button){productCatalogNew();productCatalogById('group-editor-title').textContent='Edit group';productCatalogById('group-editor-id').value=button.dataset.recordId;productCatalogById('group-editor-name').value=button.dataset.recordName||'';productCatalogById('group-editor-description').value=button.dataset.recordDescription||'';productCatalogById('group-editor-status').value=button.dataset.recordStatus||'active'}
async function productCatalogSaveGroup(event){event.preventDefault();productCatalogClearError();var form=event.currentTarget,name=productCatalogById('group-editor-name'),button=form.querySelector('button[type="submit"]');if(!form.checkValidity()){productCatalogError('Enter a group name before saving.',name);return}productCatalogBusy(button,true);try{var id=productCatalogById('group-editor-id').value,url='/b/products/api/admin/groups'+(id?'/'+encodeURIComponent(id):'');await productCatalogRequest(url,id?'PATCH':'POST',{name:name.value.trim(),description:productCatalogById('group-editor-description').value.trim(),status:productCatalogById('group-editor-status').value});window.location.reload()}catch(error){productCatalogError(error.message);productCatalogBusy(button,false)}}
async function productCatalogDelete(button){if(!window.confirm('Delete group '+(button.dataset.recordName||'')+'? Products already using it may prevent deletion.'))return;productCatalogClearError();button.disabled=true;try{await productCatalogRequest('/b/products/api/admin/groups/'+encodeURIComponent(button.dataset.recordId),'DELETE');window.location.reload()}catch(error){productCatalogError(error.message);button.disabled=false}}
"#;

// ---------------------------------------------------------------------------
// Admin: Groups
// ---------------------------------------------------------------------------

pub async fn groups(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "name".into(),
            desc: false,
        }],
        limit: 100,
        ..Default::default()
    };
    let result = db::list(ctx, GROUPS_TABLE, &opts).await;

    let content = html! {
        (admin_tabs("groups"))
        (components::page_header("Groups", Some("Keep related products together so your catalog is easier to browse"), Some(html! {
            button .btn .btn--primary .btn--sm type="button" onclick="productCatalogNew('group')" { "+ New group" }
        })))

        p #catalog-admin-error .login-error role="alert" aria-live="assertive" hidden {}
        section #group-editor .card hidden style="margin-bottom:1rem" {
            header .card__head {
                div { h3 #group-editor-title .card__title { "New group" } p .text-muted .text-sm style="margin:.25rem 0 0" { "Give the group a clear name customers will recognize." } }
            }
            div .card__body {
                form onsubmit="productCatalogSaveGroup(event)" {
                    input #group-editor-id type="hidden";
                    div .products-form-grid {
                        div .form-group {
                            label .form-label .required for="group-editor-name" { "Name" }
                            input #group-editor-name .form-input type="text" maxlength="160" required;
                        }
                        div .form-group {
                            label .form-label for="group-editor-status" { "Status" }
                            select #group-editor-status .form-select { option value="active" { "Active — available to use" } option value="archived" { "Archived — hidden" } }
                        }
                    }
                    div .form-group {
                        label .form-label for="group-editor-description" { "Description (optional)" }
                        textarea #group-editor-description .form-textarea maxlength="2000" {}
                    }
                    div .products-actions {
                        button .btn .btn--primary .btn--sm type="submit" { "Save group" }
                        button .btn .btn--secondary .btn--sm type="button" onclick="productCatalogClose('group')" { "Cancel" }
                    }
                }
            }
        }

        div #groups-content {
            @match &result {
                Ok(list) => {
                    @let cols = [
                        components::TableCol { label: "Name", width: None },
                        components::TableCol { label: "Description", width: None },
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Created", width: None },
                        components::TableCol { label: "Actions", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| vec![
                        html! { span .font-medium { (r.str_field("name")) } },
                        html! { span .text-muted .text-sm { (r.str_field("description")) } },
                        components::status_badge(r.str_field("status")),
                        html! { span .text-muted .text-sm { (r.str_field("created_at").get(..10).unwrap_or("")) } },
                        html! { div style="display:flex;gap:.4rem;flex-wrap:wrap" {
                            button .btn .btn--secondary .btn--sm type="button" data-record-id=(r.id) data-record-name=(r.str_field("name")) data-record-description=(r.str_field("description")) data-record-status=(r.str_field("status")) onclick="productCatalogEditGroup(this)" { "Edit" }
                            button .btn .btn--secondary .btn--sm type="button" data-record-id=(r.id) data-record-name=(r.str_field("name")) onclick="productCatalogDelete(this,'group')" { "Delete" }
                        } },
                    ]).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! {
                        (components::empty_state(icons::folder(), "No groups yet", "Groups are optional. Add one when you want to organize related products.", Some(html! { button .btn .btn--primary .btn--sm type="button" onclick="productCatalogNew('group')" { "+ Create group" } })))
                    }))
                }
                Err(e) => { div .login-error { "Error: " (e.message) } }
            }
        }
        script { (maud::PreEscaped(PRODUCT_CATALOG_ADMIN_JS)) }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Groups", ui::NavKind::Admin, "Products"),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Admin: Purchases
// ---------------------------------------------------------------------------

pub async fn purchases(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (page, page_size, _) = msg.pagination_params(20);
    let status_filter = msg.query("status").to_string();

    let mut filters = Vec::new();
    if !status_filter.is_empty() && status_filter != "all" {
        filters.push(Filter {
            field: "status".into(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(status_filter.clone()),
        });
    }

    let result = repo::purchases::list_paginated(ctx, filters, page as i64, page_size as i64).await;

    let content = html! {
        (admin_tabs("orders"))
        (components::page_header("Orders", Some("Track payments, refunds, and customer orders"), None))

        // Status filter
        div .filter-bar {
            span .products-filter-label { "Show" }
            @for (value, label) in [("all", "All"), ("pending", "Pending"), ("completed", "Completed"), ("partially_refunded", "Part-refunded"), ("refunded", "Refunded"), ("failed", "Failed")] {
                a .btn .(if (status_filter.is_empty() && value == "all") || status_filter == value { "btn--primary" } else { "btn--secondary" })
                    .btn--sm
                    href={"/b/products/admin/purchases?status=" (value)}
                    hx-get={"/b/products/admin/purchases?status=" (value)}
                    hx-target="#content"
                    hx-push-url="true"
                { (label) }
            }
        }

        div #purchases-content {
            @match &result {
                Ok(list) => {
                    @let row_hrefs: Vec<String> = list.records.iter().map(|record| format!("/b/products/admin/purchases/{}", record.id)).collect();
                    @let cols = [
                        components::TableCol { label: "Order", width: None },
                        components::TableCol { label: "Customer", width: None },
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Total", width: None },
                        components::TableCol { label: "Placed", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| {
                        let amount = display_money(r.i64_field("total_cents"), r.str_field("currency"));
                        let buyer = if !r.str_field("buyer_email").is_empty() { r.str_field("buyer_email") } else if !r.str_field("buyer_user_id").is_empty() { r.str_field("buyer_user_id") } else { r.str_field("user_id") };
                        vec![
                            html! { code .text-sm { (r.id.get(..8).unwrap_or(&r.id)) } },
                            html! { span .text-sm { (if buyer.is_empty() { "Guest" } else { buyer }) } },
                            components::status_badge(r.str_field("status")),
                            html! { span .font-medium { (amount) } },
                            html! { span .text-muted .text-sm { (r.str_field("created_at").get(..10).unwrap_or("—")) } },
                        ]
                    }).collect();
                    (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! { (components::empty_state(icons::shopping_cart(), "No orders yet", "Customer orders will appear here after checkout starts.", None)) }))
                    (components::pagination(list.page as u32, list.page_size as u32, list.total_count as u32, "/b/products/admin/purchases"))
                }
                Err(e) => { div .login-error { "Error: " (e.message) } }
            }
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Orders", ui::NavKind::Admin, "Products"),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Admin: Stripe setup and connection health
// ---------------------------------------------------------------------------

fn setup_check(label: &str, complete: bool, detail: &str) -> Markup {
    html! {
        li style="display:flex;align-items:flex-start;gap:.75rem;margin-bottom:.75rem" {
            span .badge .(if complete { "badge-success" } else { "badge-warning" }) {
                @if complete { "Ready" } @else { "Action needed" }
            }
            div {
                strong { (label) }
                p .text-muted .text-sm style="margin:.2rem 0 0" { (detail) }
            }
        }
    }
}

fn stripe_connection_card(status: &StripeConnectionStatus) -> Markup {
    let (state_label, badge_class, summary) = match status.state {
        StripeConnectionState::NotConfigured => (
            "Not configured",
            "badge-warning",
            "Add Stripe credentials before accepting payments.",
        ),
        StripeConnectionState::ConnectedTest => (
            "Connected — test mode",
            "badge-info",
            "Stripe is reachable. Payments use test data and do not move real money.",
        ),
        StripeConnectionState::ConnectedLive => (
            "Connected — live mode",
            "badge-success",
            "Stripe is connected and ready for live payments.",
        ),
        StripeConnectionState::Misconfigured => (
            "Connection problem",
            "badge-danger",
            "The configured Stripe credentials could not be validated.",
        ),
    };
    html! {
        section .card {
            header .card__head {
                div {
                    h3 .card__title { "Connection" }
                    p .text-muted .text-sm style="margin:.25rem 0 0" { (summary) }
                }
                span #stripe-state .badge .(badge_class) { (state_label) }
            }
            div .card__body {
                @if !status.error.is_empty() {
                    p #stripe-error .text-sm style="color:var(--accent-danger);margin-top:0" {
                        (status.error)
                    }
                } @else {
                    p #stripe-error .text-sm .text-muted style="margin-top:0" {}
                }
                div .stats-grid {
                    (components::stat_card("Payments", if status.charges_enabled { "Enabled" } else { "Unavailable" }, icons::credit_card()))
                    (components::stat_card("Payouts", if status.payouts_enabled { "Enabled" } else { "Unavailable" }, icons::arrow_up_right()))
                    (components::stat_card("Currency", if status.default_currency.is_empty() { "—" } else { &status.default_currency }, icons::dollar_sign()))
                }
                details .products-plain-details {
                    summary { "Technical connection details" }
                    div .text-sm {
                        p { strong { "Stripe account: " } @if status.account_id.is_empty() { "Not connected" } @else { code { (&status.account_id) } } }
                        p { strong { "Country: " } (if status.country.is_empty() { "—" } else { &status.country }) }
                        p { strong { "API version: " } code { (&status.api_version) } }
                    }
                }
                div style="display:flex;gap:.75rem;flex-wrap:wrap;margin-top:1.25rem" {
                    button #stripe-test-button .btn .btn--secondary .btn--md type="button" onclick="testStripeConnection()" {
                        "Test connection"
                    }
                    a .btn .btn--primary .btn--md href="/b/products/admin/settings" { "Configure Stripe" }
                }
            }
        }
    }
}

fn stripe_setup_js() -> &'static str {
    r#"
async function testStripeConnection(){
  var button=document.getElementById('stripe-test-button');
  var state=document.getElementById('stripe-state');
  var error=document.getElementById('stripe-error');
  button.disabled=true;button.textContent='Testing…';error.textContent='';
  try{
    var response=await fetch('/b/products/api/admin/stripe/status',{credentials:'same-origin'});
    var data=await response.json();
    if(!response.ok)throw new Error(data.message||'Stripe connection test failed.');
    var labels={not_configured:'Not configured',connected_test:'Connected — test mode',connected_live:'Connected — live mode',misconfigured:'Connection problem'};
    state.textContent=labels[data.state]||data.state||'Unknown';
    state.className='badge '+(data.state==='connected_live'?'badge-success':data.state==='connected_test'?'badge-info':data.state==='misconfigured'?'badge-danger':'badge-warning');
    error.textContent=data.error||'Connection test completed.';
  }catch(err){error.textContent=err.message||'Stripe connection test failed.'}
  finally{button.disabled=false;button.textContent='Test connection'}
}
function stripeWebhookElement(tag,text,className){
  var element=document.createElement(tag);
  if(text!==undefined)element.textContent=text;
  if(className)element.className=className;
  return element;
}
function stripeWebhookDate(value){
  if(!value)return '—';
  var date=new Date(value);
  return Number.isNaN(date.getTime())?value:date.toLocaleString();
}
function stripeWebhookStatus(status){
  return (status||'unknown').replace(/_/g,' ');
}
function renderStripeWebhookEvents(data){
  var target=document.getElementById('stripe-webhook-events');
  var summary=document.getElementById('stripe-webhook-summary');
  var records=Array.isArray(data.records)?data.records:[];
  target.replaceChildren();
  summary.textContent=(data.total_count||0)+' event'+(data.total_count===1?'':'s')+' match this filter.';
  if(!records.length){
    target.appendChild(stripeWebhookElement('p','No matching webhook events.','text-muted text-sm'));
    return;
  }
  var table=stripeWebhookElement('table',undefined,'data-table');
  var head=document.createElement('thead'),headRow=document.createElement('tr');
  ['Event','Status','Mode / attempts','Last result','Action'].forEach(function(label){headRow.appendChild(stripeWebhookElement('th',label))});
  head.appendChild(headRow);table.appendChild(head);
  var body=document.createElement('tbody');
  records.forEach(function(event){
    var row=document.createElement('tr');
    var eventCell=document.createElement('td');
    eventCell.dataset.label='Event';
    eventCell.appendChild(stripeWebhookElement('strong',event.event_type||'Unknown event'));
    eventCell.appendChild(document.createElement('br'));
    eventCell.appendChild(stripeWebhookElement('code',event.id));
    if(event.stripe_account_id){eventCell.appendChild(document.createElement('br'));eventCell.appendChild(stripeWebhookElement('span',event.stripe_account_id,'text-muted text-sm'))}
    row.appendChild(eventCell);
    var statusCell=document.createElement('td');statusCell.dataset.label='Status';
    var badgeClass=event.status==='processed'?'badge-success':event.status==='dead_letter'?'badge-danger':event.status==='failed'?'badge-warning':'badge-info';
    statusCell.appendChild(stripeWebhookElement('span',stripeWebhookStatus(event.status),'badge '+badgeClass));row.appendChild(statusCell);
    var attempts=document.createElement('td');attempts.dataset.label='Mode / attempts';attempts.textContent=(event.livemode?'Live':'Test')+' · '+event.attempts;row.appendChild(attempts);
    var result=document.createElement('td');result.dataset.label='Last result';
    result.appendChild(stripeWebhookElement('span',event.last_error||'No processing error recorded.',event.last_error?'':'text-muted'));
    result.appendChild(document.createElement('br'));
    result.appendChild(stripeWebhookElement('span',event.next_retry_at?'Retry '+stripeWebhookDate(event.next_retry_at):stripeWebhookDate(event.updated_at),'text-muted text-sm'));
    row.appendChild(result);
    var action=document.createElement('td');action.dataset.label='Action';
    if(event.status==='failed'||event.status==='dead_letter'){
      var replay=stripeWebhookElement('button','Replay','btn btn--secondary btn--sm');replay.type='button';
      replay.setAttribute('aria-label','Replay webhook '+event.id);
      replay.onclick=function(){replayStripeWebhookEvent(event.id,replay)};action.appendChild(replay);
    }else{action.appendChild(stripeWebhookElement('span','—','text-muted'))}
    row.appendChild(action);body.appendChild(row);
  });
  table.appendChild(body);target.appendChild(table);
}
async function loadStripeWebhookEvents(){
  var target=document.getElementById('stripe-webhook-events');
  var error=document.getElementById('stripe-webhook-error');
  var status=document.getElementById('stripe-webhook-filter').value;
  error.hidden=true;error.textContent='';target.textContent='Loading webhook events…';
  try{
    var query='?page=1&page_size=50'+(status?'&status='+encodeURIComponent(status):'');
    var response=await fetch('/b/products/api/admin/webhook-events'+query,{credentials:'same-origin'});
    var data={};try{data=await response.json()}catch(_){}
    if(!response.ok)throw new Error(data.message||'Could not load webhook events.');
    renderStripeWebhookEvents(data);
  }catch(err){target.replaceChildren();error.textContent=err.message||'Could not load webhook events.';error.hidden=false}
}
async function replayStripeWebhookEvent(id,button){
  if(!window.confirm('Replay this Stripe webhook through the normal validation pipeline?'))return;
  var error=document.getElementById('stripe-webhook-error');
  button.disabled=true;button.textContent='Replaying…';error.hidden=true;
  try{
    var response=await fetch('/b/products/api/admin/webhook-events/'+encodeURIComponent(id)+'/replay',{method:'POST',credentials:'same-origin'});
    var data={};try{data=await response.json()}catch(_){}
    if(!response.ok)throw new Error(data.message||'Could not replay the webhook event.');
    await loadStripeWebhookEvents();
  }catch(err){error.textContent=err.message||'Could not replay the webhook event.';error.hidden=false;button.disabled=false;button.textContent='Replay'}
}
function renderStripeProviderOperations(data){
  var target=document.getElementById('stripe-provider-operations-list');
  var summary=document.getElementById('stripe-provider-summary');
  var records=Array.isArray(data.records)?data.records:[];
  target.replaceChildren();
  summary.textContent=(data.total_count||0)+' operation'+(data.total_count===1?'':'s')+' match this filter.';
  if(!records.length){target.appendChild(stripeWebhookElement('p','No matching provider operations.','text-muted text-sm'));return}
  var table=stripeWebhookElement('table',undefined,'data-table');
  var head=document.createElement('thead'),headRow=document.createElement('tr');
  ['Operation','Status','Attempts','Last result'].forEach(function(label){headRow.appendChild(stripeWebhookElement('th',label))});
  head.appendChild(headRow);table.appendChild(head);var body=document.createElement('tbody');
  records.forEach(function(operation){
    var row=document.createElement('tr');
    var identity=document.createElement('td');identity.dataset.label='Operation';
    identity.appendChild(stripeWebhookElement('strong',operation.operation_type||'Provider operation'));
    identity.appendChild(document.createElement('br'));identity.appendChild(stripeWebhookElement('code',operation.aggregate_id||operation.id));
    if(operation.stripe_account_id){identity.appendChild(document.createElement('br'));identity.appendChild(stripeWebhookElement('span',operation.stripe_account_id,'text-muted text-sm'))}
    row.appendChild(identity);
    var state=document.createElement('td');state.dataset.label='Status';
    var badgeClass=operation.status==='succeeded'?'badge-success':operation.status==='dead_letter'?'badge-danger':operation.status==='failed'?'badge-warning':'badge-info';
    state.appendChild(stripeWebhookElement('span',stripeWebhookStatus(operation.status),'badge '+badgeClass));row.appendChild(state);
    var attempts=document.createElement('td');attempts.dataset.label='Attempts';attempts.textContent=String(operation.attempts||0);row.appendChild(attempts);
    var result=document.createElement('td');result.dataset.label='Last result';
    result.appendChild(stripeWebhookElement('span',operation.last_error||'No reconciliation error recorded.',operation.last_error?'':'text-muted'));
    result.appendChild(document.createElement('br'));
    result.appendChild(stripeWebhookElement('span',operation.next_attempt_at?'Retry '+stripeWebhookDate(operation.next_attempt_at):stripeWebhookDate(operation.updated_at),'text-muted text-sm'));
    row.appendChild(result);body.appendChild(row);
  });
  table.appendChild(body);target.appendChild(table);
}
async function loadStripeProviderOperations(){
  var target=document.getElementById('stripe-provider-operations-list');
  var error=document.getElementById('stripe-provider-error');
  var status=document.getElementById('stripe-provider-filter').value;
  error.hidden=true;error.textContent='';target.textContent='Loading provider operations…';
  try{
    var query='?page=1&page_size=50'+(status?'&status='+encodeURIComponent(status):'');
    var response=await fetch('/b/products/api/admin/provider-operations'+query,{credentials:'same-origin'});
    var data={};try{data=await response.json()}catch(_){}
    if(!response.ok)throw new Error(data.message||'Could not load provider operations.');
    renderStripeProviderOperations(data);
  }catch(err){target.replaceChildren();error.textContent=err.message||'Could not load provider operations.';error.hidden=false}
}
async function reconcileStripeProviderOperations(button){
  var error=document.getElementById('stripe-provider-error');
  var result=document.getElementById('stripe-provider-reconcile-result');
  button.disabled=true;button.textContent='Reconciling…';error.hidden=true;result.textContent='';
  try{
    var response=await fetch('/b/products/api/admin/provider-operations/reconcile?limit=50',{method:'POST',credentials:'same-origin'});
    var data={};try{data=await response.json()}catch(_){}
    if(!response.ok)throw new Error(data.message||'Could not reconcile provider operations.');
    result.textContent='Claimed '+data.claimed+'; completed '+data.succeeded+'; retry scheduled '+data.retry_scheduled+'; manual review '+data.dead_letter+'.';
    await loadStripeProviderOperations();
  }catch(err){error.textContent=err.message||'Could not reconcile provider operations.';error.hidden=false}
  finally{button.disabled=false;button.textContent='Reconcile due operations'}
}
loadStripeWebhookEvents();
loadStripeProviderOperations();
"#
}

pub async fn stripe_setup(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let status = stripe_provider::connection_status(ctx).await;
    let connected = matches!(
        status.state,
        StripeConnectionState::ConnectedTest | StripeConnectionState::ConnectedLive
    );
    let content = html! {
        (admin_tabs("stripe"))
        (components::page_header(
            "Stripe setup",
            Some("Connect Stripe, confirm payment readiness, and review anything that needs attention"),
            Some(html! { a .btn .btn--secondary .btn--sm href="/b/products/admin/settings" { "Edit Stripe settings" } }),
        ))
        @if status.state == StripeConnectionState::ConnectedTest {
            section .card style="border-color:var(--accent-warning);margin-bottom:1rem" {
                div .card__body {
                    strong { "Test mode is active" }
                    p .text-muted .text-sm style="margin:.35rem 0 0" {
                        "Checkout is safe to exercise, but no real funds will move. Replace both keys with matching live-mode keys only after the checklist below is complete."
                    }
                }
            }
        }
        (stripe_connection_card(&status))
        div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(320px,1fr));gap:1rem;margin-top:1rem" {
            section .card {
                header .card__head { h3 .card__title { "Go-live checklist" } }
                div .card__body {
                    ul .products-checklist {
                        (setup_check("Secret key", status.configured, "Stored server-side and never rendered back into this page."))
                        (setup_check("Publishable key", status.publishable_key_configured, "Required by embedded Checkout and browser storefronts."))
                        (setup_check("Webhook signing secret", status.webhook_secret_configured, "Required to verify Stripe event signatures and reject forged events."))
                        (setup_check("Stripe account verification", connected && status.details_submitted, "Stripe must confirm the account details before live processing."))
                        (setup_check("Charges enabled", connected && status.charges_enabled, "The platform account must be allowed to accept payments."))
                    }
                }
            }
            section .card {
                header .card__head { h3 .card__title { "Webhook destination" } }
                div .card__body {
                    p .text-muted .text-sm { "Register this HTTPS route as a Stripe webhook destination:" }
                    code .products-code-block { "/b/products/webhooks" }
                    details .products-plain-details {
                        summary { "Show required Stripe event types" }
                    ul .text-sm {
                        li { code { "account.updated" } }
                        li { code { "checkout.session.completed" } }
                        li { code { "checkout.session.async_payment_succeeded" } }
                        li { code { "checkout.session.async_payment_failed" } }
                        li { code { "payment_intent.succeeded" } ", " code { "payment_intent.payment_failed" } ", " code { "payment_intent.processing" } ", " code { "payment_intent.requires_action" } ", " code { "payment_intent.canceled" } }
                        li { code { "customer.subscription.updated" } }
                        li { code { "customer.subscription.deleted" } }
                        li { code { "invoice.paid" } ", " code { "invoice.payment_succeeded" } }
                        li { code { "invoice.payment_failed" } }
                        li { code { "charge.dispute.created" } ", " code { "charge.dispute.updated" } ", " code { "charge.dispute.closed" } }
                        li { code { "refund.created" } ", " code { "refund.updated" } ", " code { "refund.failed" } }
                        li { code { "charge.refunded" } }
                    }
                    }
                    p .text-muted .text-sm {
                        "Use the signing secret Stripe assigns to this destination in Products Settings. Keep test and live destinations separate."
                    }
                }
            }
        }
        details .products-advanced {
            summary { "Advanced: webhook delivery history" }
            section #stripe-webhook-operations .card style="border:0;box-shadow:none" {
            header .card__head style="align-items:flex-end;gap:1rem;flex-wrap:wrap" {
                div {
                    h3 .card__title { "Webhook delivery health" }
                    p .text-muted .text-sm style="margin:.25rem 0 0" {
                        "Review failed Stripe notifications and replay one after the underlying problem is fixed."
                    }
                }
                div style="display:flex;gap:.5rem;align-items:end;flex-wrap:wrap" {
                    label .text-sm for="stripe-webhook-filter" {
                        "Status"
                        select #stripe-webhook-filter onchange="loadStripeWebhookEvents()" style="display:block;margin-top:.25rem" {
                            option value="dead_letter" selected { "Needs manual review" }
                            option value="failed" { "Waiting to retry" }
                            option value="processing" { "Processing" }
                            option value="processed" { "Processed" }
                            option value="" { "All events" }
                        }
                    }
                    button .btn .btn--secondary .btn--sm type="button" onclick="loadStripeWebhookEvents()" { "Refresh" }
                }
            }
            div .card__body {
                p #stripe-webhook-summary .text-muted .text-sm aria-live="polite" style="margin-top:0" {}
                p #stripe-webhook-error .login-error role="alert" aria-live="assertive" hidden {}
                div #stripe-webhook-events aria-live="polite" { "Loading webhook events…" }
                noscript { p .text-muted .text-sm { "JavaScript is required to inspect and replay webhook deliveries." } }
            }
        }
        }
        details .products-advanced {
            summary { "Advanced: Stripe recovery tools" }
            section #stripe-provider-operations .card style="border:0;box-shadow:none" {
            header .card__head style="align-items:flex-end;gap:1rem;flex-wrap:wrap" {
                div {
                    h3 .card__title { "Provider reconciliation" }
                    p .text-muted .text-sm style="margin:.25rem 0 0" {
                        "Retry incomplete Stripe updates and review any operation that could not recover automatically."
                    }
                }
                div style="display:flex;gap:.5rem;align-items:end;flex-wrap:wrap" {
                    label .text-sm for="stripe-provider-filter" {
                        "Status"
                        select #stripe-provider-filter onchange="loadStripeProviderOperations()" style="display:block;margin-top:.25rem" {
                            option value="dead_letter" selected { "Needs manual review" }
                            option value="failed" { "Waiting to retry" }
                            option value="pending" { "Pending" }
                            option value="processing" { "Processing" }
                            option value="succeeded" { "Succeeded" }
                            option value="" { "All operations" }
                        }
                    }
                    button #stripe-provider-reconcile .btn .btn--primary .btn--sm type="button" onclick="reconcileStripeProviderOperations(this)" { "Reconcile due operations" }
                    button .btn .btn--secondary .btn--sm type="button" onclick="loadStripeProviderOperations()" { "Refresh" }
                }
            }
            div .card__body {
                p #stripe-provider-summary .text-muted .text-sm aria-live="polite" style="margin-top:0" {}
                p #stripe-provider-reconcile-result .text-sm role="status" aria-live="polite" {}
                p #stripe-provider-error .login-error role="alert" aria-live="assertive" hidden {}
                div #stripe-provider-operations-list aria-live="polite" { "Loading provider operations…" }
                noscript { p .text-muted .text-sm { "JavaScript is required to inspect and reconcile provider operations." } }
            }
        }
        }
        script { (maud::PreEscaped(stripe_setup_js())) }
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Stripe setup", ui::NavKind::Admin, "Products"),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// User: Commerce home (buyer + optional seller dashboard)
// ---------------------------------------------------------------------------

fn fee_percent(basis_points: u32) -> String {
    format!("{}.{:02}%", basis_points / 100, basis_points % 100)
}

fn friendly_requirement(requirement: &str) -> String {
    requirement
        .replace('.', " › ")
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn seller_status_card(account: Option<&SellerAccount>, fee_basis_points: u32) -> Markup {
    let ready = account.is_some_and(|account| {
        account.capabilities.details_submitted
            && account.capabilities.charges_enabled
            && account.capabilities.payouts_enabled
            && account.status != "suspended"
    });
    let suspended = account.is_some_and(|account| account.status == "suspended");
    let has_account = account.is_some_and(|account| !account.stripe_account_id.is_empty());
    html! {
        section .card {
            header .card__head {
                div {
                    h3 .card__title { "Stripe seller account" }
                    p .text-muted .text-sm style="margin:.25rem 0 0" {
                        "Stripe hosts identity verification, payouts, and the Express dashboard."
                    }
                }
                span .badge .(if suspended { "badge-danger" } else if ready { "badge-success" } else { "badge-warning" }) {
                    @if suspended { "Suspended" } @else if ready { "Ready to sell" } @else if has_account { "Setup incomplete" } @else { "Not connected" }
                }
            }
            div .card__body {
                div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:1rem" {
                    div {
                        p .text-muted .text-sm style="margin:0" { "Charges" }
                        strong { @if account.is_some_and(|a| a.capabilities.charges_enabled) { "Enabled" } @else { "Unavailable" } }
                    }
                    div {
                        p .text-muted .text-sm style="margin:0" { "Payouts" }
                        strong { @if account.is_some_and(|a| a.capabilities.payouts_enabled) { "Enabled" } @else { "Unavailable" } }
                    }
                    div {
                        p .text-muted .text-sm style="margin:0" { "Platform fee" }
                        strong { (fee_percent(account.map_or(fee_basis_points, |a| a.fee_basis_points))) }
                    }
                    div {
                        p .text-muted .text-sm style="margin:0" { "Mode" }
                        strong { @if account.is_some_and(|a| a.livemode) { "Live" } @else { "Test" } }
                    }
                }
                @if let Some(account) = account {
                    @if !account.capabilities.requirements_due.is_empty() {
                        div style="margin-top:1rem" {
                            strong { "Information Stripe still needs" }
                            ul .text-sm {
                                @for requirement in &account.capabilities.requirements_due {
                                    li { (friendly_requirement(requirement)) }
                                }
                            }
                        }
                    }
                    @if !account.disabled_reason.is_empty() {
                        p .text-sm style="color:var(--accent-danger)" { "Stripe restriction: " (friendly_requirement(&account.disabled_reason)) }
                    }
                    @if !account.sync_error.is_empty() {
                        p .text-muted .text-sm { "Last refresh: " (account.sync_error) }
                    }
                }
                div style="display:flex;gap:.75rem;flex-wrap:wrap;margin-top:1.25rem" {
                    @if !suspended && !ready {
                        button .btn .btn--primary .btn--md type="button" onclick="startSellerOnboarding()" {
                            @if has_account { "Continue Stripe setup" } @else { "Connect Stripe to sell" }
                        }
                    }
                    @if !suspended && has_account {
                        button .btn .btn--secondary .btn--md type="button" onclick="openSellerDashboard()" {
                            "Open Stripe dashboard"
                        }
                    }
                    a .btn .btn--secondary .btn--md href="/b/products/my-products" { "Manage products" }
                }
            }
        }
    }
}

fn commerce_portal_js() -> &'static str {
    r#"
function commercePortalError(message){
  var el=document.getElementById('commerce-portal-error');
  if(el){el.textContent=message||'Something went wrong. Please try again.';el.hidden=false}
}
async function commercePortalRedirect(path,body){
  var response=await fetch(path,{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json'},body:JSON.stringify(body||{})});
  var data={};try{data=await response.json()}catch(_){}
  if(!response.ok)throw new Error(data.message||'The request could not be completed.');
  if(!data.url||!/^https:\/\//.test(data.url))throw new Error('The payment provider returned an invalid redirect.');
  window.location.assign(data.url);
}
async function startSellerOnboarding(){
  try{
    var target=window.location.origin+'/b/products/';
    await commercePortalRedirect('/b/products/api/seller/onboarding',{return_url:target+'?stripe=returned',refresh_url:target+'?stripe=refresh'});
  }catch(error){commercePortalError(error.message)}
}
async function openSellerDashboard(){
  try{await commercePortalRedirect('/b/products/api/seller/dashboard',{})}
  catch(error){commercePortalError(error.message)}
}
async function manageBuyerBilling(){
  try{await commercePortalRedirect('/b/products/billing-portal',{return_url:window.location.origin+'/b/products/'})}
  catch(error){commercePortalError(error.message)}
}
"#
}

pub async fn portal_home(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let seller_enabled = super::handlers::user_products_enabled(ctx).await;
    let purchases_count = match db::count(
        ctx,
        PURCHASES_TABLE,
        &[Filter {
            field: "user_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(&user_id),
        }],
    )
    .await
    {
        Ok(count) => count,
        Err(error) => return crate::http::err_internal("Database error", error),
    };

    let (product_count, seller_account, fee_basis_points) = if seller_enabled {
        let count = match db::count(
            ctx,
            PRODUCTS_TABLE,
            &[
                Filter {
                    field: "created_by".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(&user_id),
                },
                Filter {
                    field: "deleted_at".to_string(),
                    operator: FilterOp::IsNull,
                    value: serde_json::Value::Null,
                },
            ],
        )
        .await
        {
            Ok(count) => count,
            Err(error) => return crate::http::err_internal("Database error", error),
        };
        let account = match repo::seller_accounts::get_for_user(ctx, &user_id).await {
            Ok(Some(record)) => match repo::seller_accounts::to_contract(&record) {
                Ok(account) => Some(account),
                Err(error) => return crate::http::err_internal("Seller account error", error),
            },
            Ok(None) => None,
            Err(error) => return crate::http::err_internal("Database error", error),
        };
        let fee = wafer_core::clients::config::get_default(
            ctx,
            "IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS",
            "0",
        )
        .await
        .parse::<u32>()
        .ok()
        .filter(|fee| *fee <= 10_000)
        .unwrap_or(0);
        (count, account, fee)
    } else {
        (0, None, 0)
    };

    let content = html! {
        (portal_tabs("home", seller_enabled))
        (components::page_header(
            "Commerce",
            Some("Review what you bought, manage billing, or start selling"),
            None,
        ))
        div #commerce-portal-error .text-sm hidden style="color:var(--accent-danger);margin-bottom:1rem" {}
        div .products-callout {
            div .products-callout__copy {
                strong { "One commerce workspace" }
                p .text-muted .text-sm { "Purchases stay separate from products you sell, so it is always clear whether you are buying or managing a storefront." }
            }
            a .btn .btn--secondary .btn--sm href="/b/products/my-purchases" { "View order history" }
        }
        div .stats-grid {
            (components::stat_card("Purchases", &purchases_count.to_string(), icons::shopping_cart()))
            @if seller_enabled {
                (components::stat_card("Products for sale", &product_count.to_string(), icons::package()))
            }
        }
        section .card style="margin-top:1rem" {
            header .card__head {
                div {
                    h3 .card__title { "Purchases and subscriptions" }
                    p .text-muted .text-sm style="margin:.25rem 0 0" {
                        "Review orders here. Stripe's secure Billing Portal handles saved payment methods, invoices, and subscription changes."
                    }
                }
            }
            div .card__body style="display:flex;gap:.75rem;flex-wrap:wrap" {
                a .btn .btn--primary .btn--md href="/b/products/my-purchases" { "View purchases" }
                button .btn .btn--secondary .btn--md type="button" onclick="manageBuyerBilling()" { "Manage billing" }
            }
        }
        @if seller_enabled {
            div style="margin-top:1rem" {
                (seller_status_card(seller_account.as_ref(), fee_basis_points))
            }
        }
        script { (maud::PreEscaped(commerce_portal_js())) }
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Commerce", ui::NavKind::Portal, "Products"),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Seller: dashboard and orders
// ---------------------------------------------------------------------------

fn seller_page_links(active: &str) -> Markup {
    html! {
        nav aria-label="Seller workspace" style="display:flex;gap:.75rem;flex-wrap:wrap;margin-bottom:1rem" {
            a .btn .(if active == "dashboard" { "btn--primary" } else { "btn--secondary" }) .btn--sm href="/b/products/selling" { "Dashboard" }
            a .btn .(if active == "products" { "btn--primary" } else { "btn--secondary" }) .btn--sm href="/b/products/my-products" { "Products and links" }
            a .btn .(if active == "orders" { "btn--primary" } else { "btn--secondary" }) .btn--sm href="/b/products/selling/orders" { "Orders and subscriptions" }
        }
    }
}

pub async fn seller_dashboard(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let account_record = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
        Ok(account) => account,
        Err(error) => return crate::http::err_internal("Database error", error),
    };
    let account = match account_record.as_ref() {
        Some(record) => match repo::seller_accounts::to_contract(record) {
            Ok(account) => Some(account),
            Err(error) => return crate::http::err_internal("Seller account error", error),
        },
        None => None,
    };
    let analytics = match account_record.as_ref() {
        Some(record) => match repo::purchases::commerce_analytics(ctx, Some(&record.id)).await {
            Ok(analytics) => analytics,
            Err(error) => return crate::http::err_internal("Database error", error),
        },
        None => Vec::new(),
    };
    let failures = match account_record.as_ref() {
        Some(record) => match repo::purchases::recent_seller_failures(ctx, &record.id, 5).await {
            Ok(failures) => failures,
            Err(error) => return crate::http::err_internal("Database error", error),
        },
        None => Vec::new(),
    };
    let fee_basis_points = wafer_core::clients::config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS",
        "0",
    )
    .await
    .parse::<u32>()
    .ok()
    .filter(|fee| *fee <= 10_000)
    .unwrap_or(0);
    let seller_enabled = super::handlers::user_products_enabled(ctx).await;
    let content = html! {
        (portal_tabs("selling", seller_enabled))
        (seller_page_links("dashboard"))
        (components::page_header("Seller dashboard", Some("Sales, subscriptions, Stripe readiness, and actions"), None))
        div #commerce-portal-error .text-sm hidden style="color:var(--accent-danger);margin-bottom:1rem" {}
        (seller_status_card(account.as_ref(), fee_basis_points))
        (analytics_section(&analytics, "Your sales by currency", true))
        (seller_failures_section(&failures))
        script { (maud::PreEscaped(commerce_portal_js())) }
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Seller dashboard", ui::NavKind::Portal, "Products"),
        content,
    )
    .await
}

pub async fn seller_orders(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let seller_enabled = super::handlers::user_products_enabled(ctx).await;
    let account = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            let content = html! {
                (portal_tabs("selling", seller_enabled))
                (seller_page_links("orders"))
                (components::page_header("Seller orders", Some("Orders and subscriptions sold through your Stripe account"), None))
                (components::empty_state(icons::link(), "Connect Stripe first", "Complete seller setup before accepting and reviewing seller orders.", Some(html! { a .btn .btn--primary .btn--md href="/b/products/selling" { "Open seller setup" } })))
            };
            return ui::shell_page(
                ctx,
                msg,
                ui::Shell::simple("Seller orders", ui::NavKind::Portal, "Products"),
                content,
            )
            .await;
        }
        Err(error) => return crate::http::err_internal("Database error", error),
    };
    let (page, page_size, _) = msg.pagination_params(20);
    let status_filter = msg.query("status").to_string();
    let mut filters = vec![Filter {
        field: "seller_account_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::json!(&account.id),
    }];
    if !status_filter.is_empty() && status_filter != "all" {
        filters.push(Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(&status_filter),
        });
    }
    let result = repo::purchases::list_paginated(ctx, filters, page as i64, page_size as i64).await;
    let content = html! {
        (portal_tabs("selling", seller_enabled))
        (seller_page_links("orders"))
        (components::page_header("Seller orders", Some("Orders, refunds, and subscription health for your products"), None))
        div .filter-bar {
            @for status in &["all", "pending", "completed", "partially_refunded", "refunded", "failed"] {
                a .btn .(if (status_filter.is_empty() && *status == "all") || status_filter == *status { "btn--primary" } else { "btn--secondary" }) .btn--sm
                    href={"/b/products/selling/orders?status=" (*status)} { (status.replace('_', " ")) }
            }
        }
        @match &result {
            Ok(list) => {
                @let row_hrefs: Vec<String> = list.records.iter().map(|record| format!("/b/products/selling/orders/{}", record.id)).collect();
                @let cols = [
                    components::TableCol { label: "Buyer", width: None },
                    components::TableCol { label: "Status", width: None },
                    components::TableCol { label: "Total", width: None },
                    components::TableCol { label: "Subscription", width: None },
                    components::TableCol { label: "Date", width: None },
                ];
                @let rows: Vec<Vec<Markup>> = list.records.iter().map(|order| vec![
                    html! { span .text-sm { (if order.str_field("buyer_email").is_empty() { order.str_field("buyer_user_id") } else { order.str_field("buyer_email") }) } },
                    components::status_badge(order.str_field("status")),
                    html! { span .font-medium { (display_money(order.i64_field("total_cents"), order.str_field("currency"))) } },
                    html! { @if order.str_field("stripe_subscription_id").is_empty() { span .text-muted { "—" } } @else { (components::status_badge(order.str_field("subscription_status"))) } },
                    html! { span .text-muted .text-sm { (order.str_field("created_at").get(..10).unwrap_or("")) } },
                ]).collect();
                (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! { p .text-muted { "No seller orders yet" } }))
                (components::pagination(list.page as u32, list.page_size as u32, list.total_count as u32, "/b/products/selling/orders"))
            }
            Err(error) => { div .login-error { "Error: " (error.message) } }
        }
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Seller orders", ui::NavKind::Portal, "Products"),
        content,
    )
    .await
}

#[derive(Clone, Copy)]
enum OrderPageAccess {
    Admin,
    Buyer,
    Seller,
}

pub async fn admin_purchase_detail(
    ctx: &dyn Context,
    msg: &Message,
    purchase_id: &str,
) -> OutputStream {
    order_detail(ctx, msg, purchase_id, OrderPageAccess::Admin).await
}

pub async fn my_purchase_detail(
    ctx: &dyn Context,
    msg: &Message,
    purchase_id: &str,
) -> OutputStream {
    order_detail(ctx, msg, purchase_id, OrderPageAccess::Buyer).await
}

pub async fn seller_order_detail(
    ctx: &dyn Context,
    msg: &Message,
    purchase_id: &str,
) -> OutputStream {
    order_detail(ctx, msg, purchase_id, OrderPageAccess::Seller).await
}

const ORDER_DETAIL_JS: &str = r#"
function orderDetailError(message){var target=document.getElementById('order-detail-error');if(target){target.textContent=message||'Something went wrong.';target.hidden=false;target.scrollIntoView({block:'nearest'})}}
function parseOrderRefundMinor(value,exponent){value=value.trim();if(!value)return null;if(!/^[+]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(value))throw new Error('Enter a plain positive amount.');value=value.replace(/^\+/,'');var parts=value.split('.'),whole=parts[0]||'0',fraction=parts[1]||'';if(fraction.length>exponent&&/[1-9]/.test(fraction.slice(exponent)))throw new Error('The amount has too many decimal places for this currency.');fraction=fraction.slice(0,exponent).padEnd(exponent,'0');var minor=BigInt(whole)*(10n**BigInt(exponent))+BigInt(fraction||'0');if(minor<=0n)throw new Error('Refund amount must be positive.');if(minor>BigInt(Number.MAX_SAFE_INTEGER))throw new Error('This amount is too large for the browser refund form.');return Number(minor)}
async function submitOrderRefund(button){var config=window.__orderDetailConfig,target=document.getElementById('order-detail-error');if(target)target.hidden=true;button.disabled=true;button.textContent='Refunding…';try{var amount=parseOrderRefundMinor(document.getElementById('order-refund-amount').value,config.currency_exponent),note=document.getElementById('order-refund-note').value.trim(),body={note:note,idempotency_key:'ui_'+config.refunded_total+'_'+(amount===null?'full':amount)};if(amount!==null)body.amount_minor=amount;var response=await fetch(config.refund_url,{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json','Accept':'application/json'},body:JSON.stringify(body)}),payload={};try{payload=await response.json()}catch(_error){}if(!response.ok)throw new Error(payload.message||payload.error||'Refund failed.');window.location.reload()}catch(error){orderDetailError(error.message);button.disabled=false;button.textContent='Create refund'}}
async function manageOrderBilling(){var config=window.__orderDetailConfig;try{await commercePortalRedirect('/b/products/billing-portal',{return_url:window.location.href,order_id:config.order_id})}catch(error){orderDetailError(error.message)}}
"#;

async fn order_detail(
    ctx: &dyn Context,
    msg: &Message,
    purchase_id: &str,
    access: OrderPageAccess,
) -> OutputStream {
    let purchase = match repo::purchases::get(ctx, purchase_id).await {
        Ok(purchase) => purchase,
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => {
            return crate::http::err_not_found("Purchase not found")
        }
        Err(error) => return crate::http::err_internal("Database error", error),
    };
    match access {
        OrderPageAccess::Admin => {}
        OrderPageAccess::Buyer => {
            let owner = if purchase.str_field("buyer_user_id").is_empty() {
                purchase.str_field("user_id")
            } else {
                purchase.str_field("buyer_user_id")
            };
            if owner != msg.user_id() {
                return crate::http::err_forbidden("Access denied");
            }
        }
        OrderPageAccess::Seller => {
            let account = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
                Ok(Some(account)) => account,
                Ok(None) => return crate::http::err_forbidden("Seller setup is required"),
                Err(error) => return crate::http::err_internal("Database error", error),
            };
            if purchase.str_field("seller_account_id") != account.id {
                return crate::http::err_forbidden("Access denied");
            }
        }
    }
    let line_items = match repo::purchases::list_line_items(ctx, purchase_id).await {
        Ok(items) => items,
        Err(error) => return crate::http::err_internal("Could not load order items", error),
    };
    let refunds = match repo::refunds::list_for_purchase(ctx, purchase_id).await {
        Ok(refunds) => refunds,
        Err(error) => return crate::http::err_internal("Could not load refunds", error),
    };
    let disputes = match repo::disputes::list_for_purchase(ctx, purchase_id).await {
        Ok(disputes) => disputes,
        Err(error) => return crate::http::err_internal("Could not load disputes", error),
    };
    let currency = purchase.str_field("currency");
    let refunded_total = purchase.i64_field("refunded_total_cents");
    let refundable = matches!(
        purchase.str_field("status"),
        "completed" | "partially_refunded"
    ) && refunded_total < purchase.i64_field("total_cents");
    let refund_url = match access {
        OrderPageAccess::Admin => Some(format!(
            "/b/products/api/admin/purchases/{purchase_id}/refund"
        )),
        OrderPageAccess::Seller => Some(format!(
            "/b/products/api/seller/orders/{purchase_id}/refund"
        )),
        OrderPageAccess::Buyer => None,
    };
    let currency_exponent = money::currency_exponent(currency).unwrap_or(2);
    let page_config = serde_json::json!({
        "order_id": purchase_id,
        "refund_url": refund_url.clone(),
        "refunded_total": refunded_total,
        "currency_exponent": currency_exponent,
    });
    let (back_url, back_label, tabs) = match access {
        OrderPageAccess::Admin => (
            "/b/products/admin/purchases",
            "Back to all orders",
            admin_tabs("orders"),
        ),
        OrderPageAccess::Buyer => (
            "/b/products/my-purchases",
            "Back to my purchases",
            portal_tabs(
                "purchases",
                super::handlers::user_products_enabled(ctx).await,
            ),
        ),
        OrderPageAccess::Seller => (
            "/b/products/selling/orders",
            "Back to seller orders",
            html! {
                (portal_tabs("selling", true))
                (seller_page_links("orders"))
            },
        ),
    };
    let buyer = if purchase.str_field("buyer_email").is_empty() {
        purchase.str_field("buyer_user_id")
    } else {
        purchase.str_field("buyer_email")
    };
    let content = html! {
        (tabs)
        a .text-sm href=(back_url) { "← " (back_label) }
        div style="display:flex;justify-content:space-between;gap:1rem;align-items:flex-start;flex-wrap:wrap;margin-top:1rem" {
            div {
                h1 style="margin-bottom:.35rem" { "Order #" (purchase.id.get(..8).unwrap_or(&purchase.id)) }
                p .text-muted style="margin-top:0" { "Placed " (purchase.str_field("created_at").get(..10).unwrap_or("—")) }
            }
            div style="display:flex;gap:.5rem;align-items:center" {
                (components::status_badge(purchase.str_field("status")))
                @if purchase.bool_field("livemode") {
                    (components::status_badge("live"))
                } @else {
                    (components::status_badge("test"))
                }
            }
        }
        div #order-detail-error .login-error hidden {}
        div .stats-grid {
            (components::stat_card("Total", &display_money(purchase.i64_field("total_cents"), currency), icons::dollar_sign()))
            (components::stat_card("Refunded", &display_money(refunded_total, currency), icons::arrow_down_left()))
            (components::stat_card("Customer", if buyer.is_empty() { "Guest" } else { buyer }, icons::users()))
            (components::stat_card("Items", &line_items.len().to_string(), icons::package()))
        }
        details .products-plain-details {
            summary { "View order total breakdown" }
            div .products-form-grid .products-form-grid--compact .text-sm {
                p { strong { "Subtotal: " } (display_money(purchase.i64_field("subtotal_cents"), currency)) }
                p { strong { "Discount: " } (display_money(purchase.i64_field("discount_cents"), currency)) }
                p { strong { "Tax: " } (display_money(purchase.i64_field("tax_cents"), currency)) }
                p { strong { "Shipping: " } (display_money(purchase.i64_field("shipping_cents"), currency)) }
                p { strong { "Platform fee: " } (display_money(purchase.i64_field("platform_fee_cents"), currency)) }
            }
        }
        details .products-advanced {
            summary { "Order timeline" }
            div .products-advanced__body .products-form-grid {
                @for (label, value) in [
                    ("Order created", purchase.str_field("created_at")),
                    ("Payment recorded", purchase.str_field("payment_at")),
                    ("Approved", purchase.str_field("approved_at")),
                    ("Refund updated", purchase.str_field("refunded_at")),
                    ("Subscription synced", purchase.str_field("subscription_last_synced_at")),
                    ("Subscription canceled", purchase.str_field("subscription_canceled_at")),
                ] {
                    @if !value.is_empty() {
                        div { p .text-muted .text-sm style="margin:0" { (label) } strong .text-sm { (value) } }
                    }
                }
            }
        }
        section .card style="margin-top:1rem" {
            header .card__head { h2 .card__title { "Items" } }
            div .card__body {
                @if line_items.is_empty() {
                    p .text-muted { "No line-item snapshot is available for this order." }
                } @else {
                    @let cols = [
                        components::TableCol { label: "Item", width: None },
                        components::TableCol { label: "Quantity", width: None },
                        components::TableCol { label: "Unit", width: None },
                        components::TableCol { label: "Total", width: None },
                        components::TableCol { label: "Configuration", width: None },
                    ];
                    @let rows: Vec<Vec<Markup>> = line_items.iter().map(|item| vec![
                        html! { strong { (item.str_field("product_name")) } },
                        html! { (item.i64_field("quantity")) },
                        html! { (display_money(item.i64_field("unit_amount_minor"), currency)) },
                        html! { strong { (display_money(item.i64_field("total_minor"), currency)) } },
                        html! { @if item.str_field("input_snapshot").is_empty() || item.str_field("input_snapshot") == "{}" { span .text-muted { "—" } } @else { details { summary { "View" } code .text-sm { (item.str_field("input_snapshot")) } } } },
                    ]).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! {}))
                }
            }
        }
        div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:1rem;margin-top:1rem" {
            section .card {
                header .card__head { h2 .card__title { "Buyer and checkout" } }
                div .card__body .text-sm {
                    p { strong { "Buyer: " } (if buyer.is_empty() { "Guest" } else { buyer }) }
                    p { strong { "Presentation: " } (purchase.str_field("checkout_mode")) }
                    p { strong { "Provider: " } (purchase.str_field("provider")) }
                    p { strong { "Seller account: " } (if purchase.str_field("seller_account_id").is_empty() { "Platform" } else { purchase.str_field("seller_account_id") }) }
                }
            }
            details .products-advanced style="margin-top:0" {
                summary { "Technical payment details" }
                div .products-advanced__body .text-sm {
                    h2 style="font-size:1rem;margin-top:0" { "Provider reconciliation" }
                    p { strong { "State: " } (components::status_badge(purchase.str_field("reconciliation_status"))) }
                    @if !purchase.str_field("provider_payment_status").is_empty() {
                        p { strong { "Payment state: " } (components::status_badge(purchase.str_field("provider_payment_status"))) }
                    }
                    @for (label, value) in [
                        ("Checkout Session", purchase.str_field("provider_session_id")),
                        ("PaymentIntent", purchase.str_field("stripe_payment_intent_id")),
                        ("Customer", purchase.str_field("stripe_customer_id")),
                        ("Stripe account", purchase.str_field("stripe_account_id")),
                    ] {
                        @if !value.is_empty() { p { strong { (label) ": " } code { (value) } } }
                    }
                    @if !purchase.str_field("reconciliation_error").is_empty() {
                        p style="color:var(--accent-danger)" { (purchase.str_field("reconciliation_error")) }
                    }
                    @if !purchase.str_field("provider_payment_error_code").is_empty() {
                        p { strong { "Provider code: " } code { (purchase.str_field("provider_payment_error_code")) } }
                    }
                    @if !purchase.str_field("provider_payment_error_message").is_empty()
                        && purchase.str_field("provider_payment_error_message") != purchase.str_field("reconciliation_error") {
                        p style="color:var(--accent-danger)" { (purchase.str_field("provider_payment_error_message")) }
                    }
                }
            }
        }
        @if !purchase.str_field("stripe_subscription_id").is_empty() {
            section .card style="margin-top:1rem" {
                header .card__head {
                    div {
                        h2 .card__title { "Subscription" }
                        p .text-muted .text-sm style="margin:.25rem 0 0" { code { (purchase.str_field("stripe_subscription_id")) } }
                    }
                    (components::status_badge(purchase.str_field("subscription_status")))
                }
                div .card__body {
                    p .text-sm { strong { "Current period ends: " } (if purchase.str_field("subscription_current_period_end").is_empty() { "Not reported yet" } else { purchase.str_field("subscription_current_period_end") }) }
                    p .text-sm { strong { "Cancels at period end: " } (if purchase.bool_field("subscription_cancel_at_period_end") { "Yes" } else { "No" }) }
                    @if !purchase.str_field("subscription_canceled_at").is_empty() { p .text-sm { strong { "Canceled: " } (purchase.str_field("subscription_canceled_at")) } }
                    @if matches!(access, OrderPageAccess::Buyer) && !purchase.str_field("stripe_customer_id").is_empty() {
                        button .btn .btn--primary .btn--md type="button" onclick="manageOrderBilling()" { "Manage subscription and billing" }
                    }
                }
            }
        }
        @if !refunds.is_empty() {
            section .card style="margin-top:1rem" {
                header .card__head { h2 .card__title { "Refund history" } }
                div .card__body {
                    @let cols = [
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Amount", width: None },
                        components::TableCol { label: "Provider refund", width: None },
                        components::TableCol { label: "Note", width: None },
                        components::TableCol { label: "Date", width: None },
                    ];
                    @let rows: Vec<Vec<Markup>> = refunds.iter().map(|refund| vec![
                        components::status_badge(refund.str_field("status")),
                        html! { (display_money(refund.i64_field("amount_minor"), currency)) },
                        html! { code .text-sm { (refund.str_field("provider_refund_id")) } },
                        html! { span .text-sm { (refund.str_field("note")) } },
                        html! { span .text-muted .text-sm { (refund.str_field("created_at")) } },
                    ]).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! {}))
                }
            }
        }
        @if !disputes.is_empty() {
            section .card style="margin-top:1rem" {
                header .card__head {
                    div {
                        h2 .card__title { "Payment disputes" }
                        @if matches!(access, OrderPageAccess::Admin | OrderPageAccess::Seller) {
                            p .text-muted .text-sm style="margin:.25rem 0 0" { "Evidence, balance impact, and payout actions are managed in Stripe. This ledger mirrors signed provider events." }
                        }
                    }
                }
                div .card__body {
                    @let cols = [
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Amount", width: None },
                        components::TableCol { label: "Reason", width: None },
                        components::TableCol { label: "Evidence due", width: None },
                        components::TableCol { label: "Provider dispute", width: None },
                    ];
                    @let rows: Vec<Vec<Markup>> = disputes.iter().map(|dispute| vec![
                        components::status_badge(dispute.str_field("status")),
                        html! { strong { (display_money(dispute.i64_field("amount_minor"), dispute.str_field("currency"))) } },
                        html! { span .text-sm { (if dispute.str_field("reason").is_empty() { "Not supplied" } else { dispute.str_field("reason") }) } },
                        html! { span .text-sm { (if dispute.str_field("evidence_due_by").is_empty() { "—" } else { dispute.str_field("evidence_due_by") }) } },
                        html! { code .text-sm { (dispute.str_field("provider_dispute_id")) } },
                    ]).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! {}))
                }
            }
        }
        @if refund_url.is_some() && refundable {
            section .card style="margin-top:1rem" {
                header .card__head { h2 .card__title { "Create refund" } }
                div .card__body {
                    p .text-muted .text-sm { "Leave the amount blank to refund the complete remaining balance. Stripe refunds and proportional Connect fee/transfer reversals are requested before local success is recorded." }
                    div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:1rem" {
                        div .form-group { label .form-label for="order-refund-amount" { "Amount (" (currency) ")" } input #order-refund-amount .form-input type="text" inputmode="decimal" placeholder="Full remaining amount" {} }
                        div .form-group { label .form-label for="order-refund-note" { "Private note" } textarea #order-refund-note .form-textarea maxlength="500" {} }
                    }
                    button .btn .btn--danger .btn--md type="button" onclick="submitOrderRefund(this)" { "Create refund" }
                }
            }
        }
        script { (maud::PreEscaped(format!("window.__orderDetailConfig={};\n{}\n{}", page_config, commerce_portal_js(), ORDER_DETAIL_JS))) }
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple(
            "Order detail",
            if matches!(access, OrderPageAccess::Admin) {
                ui::NavKind::Admin
            } else {
                ui::NavKind::Portal
            },
            "Products",
        ),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// User: My Products
// ---------------------------------------------------------------------------

pub async fn my_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let seller_enabled = super::handlers::user_products_enabled(ctx).await;
    let (page, page_size, _) = msg.pagination_params(20);

    let filters = vec![
        Filter {
            field: "created_by".into(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id),
        },
        Filter {
            field: "deleted_at".into(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        },
    ];
    let sort = vec![SortField {
        field: "created_at".into(),
        desc: true,
    }];
    let result = db::paginated_list(
        ctx,
        PRODUCTS_TABLE,
        page as i64,
        page_size as i64,
        filters,
        sort,
    )
    .await;

    let content = html! {
        (portal_tabs("selling", seller_enabled))
        (seller_page_links("products"))
        (components::page_header(
            "My Products",
            Some("Create products and manage their offers, checkout links, and publication status"),
            Some(html! {
                a .btn .btn--primary .btn--sm href="/b/products/my-products/new" { "+ New Product" }
            }),
        ))

        div #my-products-content {
            @match &result {
                Ok(list) => {
                    @let row_hrefs: Vec<String> = list.records.iter().map(|record| format!("/b/products/my-products/{}", record.id)).collect();
                    @let cols = [
                        components::TableCol { label: "Name", width: None },
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Currency", width: None },
                        components::TableCol { label: "Created", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| vec![
                        html! { span .font-medium { (r.str_field("name")) } },
                        components::status_badge(r.str_field("status")),
                        html! { span .font-medium { (r.str_field("currency")) } },
                        html! { span .text-muted .text-sm { (r.str_field("created_at").get(..10).unwrap_or("")) } },
                    ]).collect();
                    (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! { p .text-muted { "No products yet" } }))
                    (components::pagination(list.page as u32, list.page_size as u32, list.total_count as u32, "/b/products/my-products"))
                }
                Err(e) => { div .login-error { "Error: " (e.message) } }
            }
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("My Products", ui::NavKind::Portal, "My Products"),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// User: My Purchases
// ---------------------------------------------------------------------------

pub async fn my_purchases(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let seller_enabled = super::handlers::user_products_enabled(ctx).await;
    let (page, page_size, _) = msg.pagination_params(20);

    let filters = vec![Filter {
        field: "user_id".into(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id),
    }];
    let result = repo::purchases::list_paginated(ctx, filters, page as i64, page_size as i64).await;

    let content = html! {
        (portal_tabs("purchases", seller_enabled))
        (components::page_header("My Purchases", Some("Receipts, payment status, and subscription details"), None))

        div #my-purchases-content {
            @match &result {
                Ok(list) => {
                    @let row_hrefs: Vec<String> = list.records.iter().map(|record| format!("/b/products/my-purchases/{}", record.id)).collect();
                    @let cols = [
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Total", width: None },
                        components::TableCol { label: "Provider", width: None },
                        components::TableCol { label: "Date", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| {
                        let amount = display_money(r.i64_field("total_cents"), r.str_field("currency"));
                        vec![
                            components::status_badge(r.str_field("status")),
                            html! { span .font-medium { (amount) } },
                            html! { span .text-muted .text-sm { (r.str_field("provider")) } },
                            html! { span .text-muted .text-sm { (r.str_field("created_at").get(..10).unwrap_or("")) } },
                        ]
                    }).collect();
                    (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! { p .text-muted { "No purchases yet" } }))
                    (components::pagination(list.page as u32, list.page_size as u32, list.total_count as u32, "/b/products/my-purchases"))
                }
                Err(e) => { div .login-error { "Error: " (e.message) } }
            }
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("My Purchases", ui::NavKind::Portal, "My Purchases"),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Admin: Settings
// ---------------------------------------------------------------------------

/// The block + shared config vars rendered on the products settings page, in
/// their on-page order. Pulled from the declared [`ConfigVar`] metadata — the
/// block-owned ones from `super::config_vars()`, the shared ones from
/// `config_vars::shared_var()` — so nothing is re-declared in a parallel tuple.
async fn settings_vars(ctx: &dyn Context) -> SettingsVars {
    let own = super::config_vars();
    let trusted_server = super::stripe_secret_operations_allowed(ctx).await;
    let mut stripe = vec![config_vars::var_in(
        &own,
        "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY",
    )];
    let mut stripe_advanced = vec![config_vars::var_in(
        &own,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_VERSION",
    )];
    let mut webhooks = vec![config_vars::shared_var("WAFER_RUN_SHARED__FRONTEND_URL")];
    if trusted_server {
        stripe.splice(
            0..0,
            [
                config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY"),
                config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET"),
            ],
        );
        stripe_advanced.insert(
            0,
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL"),
        );
        webhooks.extend([
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__WEBHOOK_URL"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET"),
        ]);
    }
    SettingsVars {
        features: vec![config_vars::shared_var(
            "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS",
        )],
        stripe,
        stripe_advanced,
        checkout: vec![
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__DEFAULT_CURRENCY"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX"),
        ],
        checkout_advanced: vec![config_vars::var_in(
            &own,
            "IMPRESSPRESS__PRODUCTS__CHECKOUT_ALLOWED_ORIGINS",
        )],
        sellers: vec![
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__SELLER_MODERATION_REQUIRED"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_TEMPLATES"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CURRENCIES"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CATEGORIES"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__SELLER_MAX_PRODUCTS"),
        ],
        webhooks,
    }
}

struct SettingsVars {
    features: Vec<wafer_run::ConfigVar>,
    stripe: Vec<wafer_run::ConfigVar>,
    stripe_advanced: Vec<wafer_run::ConfigVar>,
    checkout: Vec<wafer_run::ConfigVar>,
    sellers: Vec<wafer_run::ConfigVar>,
    checkout_advanced: Vec<wafer_run::ConfigVar>,
    webhooks: Vec<wafer_run::ConfigVar>,
}

impl SettingsVars {
    /// Flatten to a single allowlist for the save handler.
    fn all(&self) -> Vec<wafer_run::ConfigVar> {
        let mut v = self.features.clone();
        v.extend(self.stripe.iter().cloned());
        v.extend(self.stripe_advanced.iter().cloned());
        v.extend(self.checkout.iter().cloned());
        v.extend(self.sellers.iter().cloned());
        v.extend(self.webhooks.iter().cloned());
        v.extend(self.checkout_advanced.iter().cloned());
        v
    }
}

pub async fn settings(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let trusted_server = super::stripe_secret_operations_allowed(ctx).await;
    let vars = settings_vars(ctx).await;
    let sections = [
        SettingsSection::new("Stripe credentials", icons::dollar_sign(), &vars.stripe)
            .description(
                "Add the keys from your Stripe Dashboard. Saved secret values stay masked.",
            ),
        SettingsSection::new("Store defaults", icons::shopping_cart(), &vars.checkout)
            .description("Preselected for new products. You can still change them on each product."),
        SettingsSection::new("Seller products (optional)", icons::users(), &vars.features)
            .description("Turn this on only if customers should be able to create and sell their own products.")
            .collapsible(),
        SettingsSection::new("Advanced checkout security", icons::settings(), &vars.checkout_advanced)
            .description("Restrict which website origins can be used as checkout return and cancel destinations.")
            .collapsible(),
        SettingsSection::new("Seller rules (optional)", icons::users(), &vars.sellers)
            .description("Set fees, approval rules, currencies, templates, and listing limits for sellers.")
            .collapsible(),
        SettingsSection::new("Advanced Stripe options", icons::settings(), &vars.stripe_advanced)
            .description("Provider endpoint and API version overrides. The defaults are right for most stores.")
            .collapsible(),
        SettingsSection::new("Developer webhooks (optional)", icons::globe(), &vars.webhooks)
            .description("Send signed billing events to another system you control.")
            .collapsible(),
    ];
    let content = html! {
        (admin_tabs("settings"))
        (components::page_header("Settings", Some("Set up payments and choose sensible defaults for new products"), None))
        section .products-callout .products-settings-note {
            div .products-callout__copy {
                strong { "Start with Stripe credentials and store defaults" }
                p .text-muted .text-sm { "Seller tools, provider overrides, and developer webhooks are optional and stay tucked away until you need them." }
            }
            div .products-callout__actions {
                a .btn .btn--secondary .btn--sm href="/b/products/admin/stripe" { "Check Stripe status" }
            }
        }
        @if !trusted_server {
            section .card style="border-color:var(--accent-warning);margin-bottom:1rem" {
                div .card__body {
                    strong { "Browser runtime safety" }
                    p .text-muted .text-sm style="margin:.35rem 0 0" {
                        "Stripe secret keys and signed webhooks are disabled here because browser storage is controlled by the visitor. Point the storefront widget at a trusted native or Cloudflare API, or use a pre-created Payment Link."
                    }
                }
            }
        }
        (settings_form::settings_form(ctx, "/b/products/admin/settings", &sections, html! {}).await)
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Settings", ui::NavKind::Admin, "Products"),
        content,
    )
    .await
}

pub async fn handle_save_settings(ctx: &dyn Context, input: InputStream) -> OutputStream {
    settings_form::save_settings(ctx, input, &settings_vars(ctx).await.all(), "products").await
}
