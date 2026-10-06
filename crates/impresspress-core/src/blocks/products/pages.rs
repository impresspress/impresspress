//! SSR pages for the products block (admin + user views).

use maud::{html, Markup};
use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_run::{context::Context, Message, OutputStream};

use super::{
    assets,
    config::{
        AUTOMATIC_TAX, CHECKOUT_ALLOWED_ORIGINS, DEFAULT_CURRENCY, PLATFORM_COUNTRY,
        SELLER_ALLOWED_CATEGORIES, SELLER_ALLOWED_CURRENCIES, SELLER_ALLOWED_TEMPLATES,
        SELLER_APPLICATION_FEE_BPS, SELLER_MAX_PRODUCTS, SELLER_MODERATION_REQUIRED,
        STRIPE_API_URL, STRIPE_API_VERSION, STRIPE_PUBLISHABLE_KEY, STRIPE_SECRET_KEY,
        STRIPE_WEBHOOK_SECRET, WEBHOOK_SECRET, WEBHOOK_URL,
    },
    contracts::{
        AmountRule, ApprovalStatus, BillingScheme, CheckoutPolicy, CommerceAnalytics, ManagedOffer,
        OfferDefinitionRequest, OfferMode, OfferStatus, OfferSyncStatus, PricingModel,
        ProductStatus, SellerAccount, SellerFailureSummary, SellerStatus, StripeConnectionState,
        StripeConnectionStatus, StripeEventType, TaxBehavior, UsageType, VariableDefinition,
        VariableKind, PRODUCT_SLUG_MAX_LEN, PRODUCT_SLUG_PATTERN,
    },
    money, repo, stripe_provider,
};
use crate::config_vars::{ALLOW_USER_PRODUCTS_KEY, FRONTEND_URL_KEY};

fn display_money(amount_minor: i64, currency: &str) -> String {
    let currency = money::normalize_currency(currency).unwrap_or_else(|_| currency.to_uppercase());
    match money::format_amount_minor(amount_minor, &currency) {
        Ok(amount) => format!("{amount} {currency}"),
        Err(_) => format!("{amount_minor} minor units ({currency})"),
    }
}

fn analytics_section(analytics: &[CommerceAnalytics], title: &str, seller_view: bool) -> Markup {
    html! {
        section .products-section {
            (components::section_header(title, None))
            p .section-desc {
                "Money is reported separately for each currency. Gross values include orders that were later refunded; after-refund sales subtract customer refunds."
                @if seller_view { " Proceeds shown here subtract recorded platform fees but are before Stripe fees, disputes, reserves, and payout adjustments; Stripe remains authoritative for available balance and payouts." }
            }
            @if analytics.is_empty() {
                (components::empty_state(icons::bar_chart(), "No sales data yet", "Completed orders and subscription activity will appear here.", None))
            } @else {
                @for currency in analytics {
                    article .card .mt-4 {
                        header .card__head {
                            div {
                                h2 .card__title { (currency.currency) }
                                p .text-muted .text-sm .text-subtitle { (currency.paid_order_count) " paid of " (currency.order_count) " checkout records" }
                            }
                            (components::status_badge(&currency.currency))
                        }
                        div .card__body {
                            div .stats-grid {
                                (components::stat_card("Gross sales", &display_money(currency.gross_volume_minor, &currency.currency), icons::dollar_sign(), None))
                                (components::stat_card("Customer refunds", &display_money(currency.refunded_volume_minor, &currency.currency), icons::arrow_down_left(), None))
                                (components::stat_card(if seller_view { "After refunds" } else { "Net sales" }, &display_money(currency.net_volume_minor, &currency.currency), icons::arrow_up_right(), None))
                                (components::stat_card("Platform fees", &display_money(currency.platform_fees_minor, &currency.currency), icons::dollar_sign(), None))
                                @if seller_view {
                                    (components::stat_card("Before Stripe fees", &display_money(currency.net_volume_minor.saturating_sub(currency.platform_fees_minor), &currency.currency), icons::arrow_up_right(), None))
                                }
                                (components::stat_card("Failed orders", &currency.failed_order_count.to_string(), icons::info(), None))
                                (components::stat_card("Past due", &currency.past_due_subscription_count.to_string(), icons::help_circle(), None))
                                (components::stat_card("Open disputes", &format!("{} · {}", currency.open_dispute_count, display_money(currency.open_disputed_volume_minor, &currency.currency)), icons::help_circle(), None))
                                (components::stat_card("Lost disputes", &format!("{} · {}", currency.lost_dispute_count, display_money(currency.lost_disputed_volume_minor, &currency.currency)), icons::arrow_down_left(), None))
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
                                h3 .products-subheading { "Top products by gross sales" }
                                @let cols = [
                                    components::TableCol::new("Product"),
                                    components::TableCol::new("Quantity"),
                                    components::TableCol::new("Gross"),
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
        section .products-section {
            (components::section_header("Recent payment failures", None))
            p .section-desc { "Failed seller orders that may need customer follow-up. Stripe Dashboard provides provider-level payment details." }
            @if failures.is_empty() {
                (components::empty_state(icons::info(), "No recent failures", "No failed seller orders need attention.", None))
            } @else {
                @let row_hrefs: Vec<String> = failures.iter().map(|failure| format!("/b/products/selling/orders/{}", failure.order_id)).collect();
                @let cols = [
                    components::TableCol::new("Order").primary(),
                    components::TableCol::new("Amount"),
                    components::TableCol::new("Last result"),
                    components::TableCol::new("Date"),
                ];
                @let rows: Vec<Vec<Markup>> = failures.iter().map(|failure| vec![
                    html! { code { (&failure.order_id) } },
                    html! { (display_money(failure.total_minor, &failure.currency)) },
                    html! { span .text-sm { (if failure.error.is_empty() { "Payment did not complete" } else { &failure.error }) } },
                    components::timestamp(&failure.created_at),
                ]).collect();
                (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! {}))
            }
        }
    }
}
use crate::{
    config_vars,
    ui::{self, components, icons, settings_form, settings_form::SettingsSection},
    util::{self, RecordExt},
};

/// The products admin sections, one link each above every products admin
/// page ([`components::subnav`]). Separate pages, so plain links: a tab that
/// swapped only the body would leave the previous page's title and actions in
/// the topbar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AdminSection {
    Overview,
    Products,
    Groups,
    Orders,
    Sellers,
    Stripe,
    Settings,
}

fn admin_sections(active: AdminSection) -> Markup {
    let tab = |section: AdminSection, href: &'static str, label: &'static str| components::Tab {
        active: section == active,
        href,
        label,
        icon: None,
    };
    components::subnav(
        "Products sections",
        vec![
            tab(AdminSection::Overview, "/b/products/admin/", "Overview"),
            tab(
                AdminSection::Products,
                "/b/products/admin/manage",
                "Products",
            ),
            tab(AdminSection::Groups, "/b/products/admin/groups", "Groups"),
            tab(
                AdminSection::Orders,
                "/b/products/admin/purchases",
                "Orders",
            ),
            tab(
                AdminSection::Sellers,
                "/b/products/admin/sellers",
                "Sellers",
            ),
            tab(AdminSection::Stripe, "/b/products/admin/stripe", "Stripe"),
            tab(
                AdminSection::Settings,
                "/b/products/admin/settings",
                "Settings",
            ),
        ],
    )
}

/// A signed-in user's commerce sections: what they bought, and — when seller
/// products are on — what they sell. One row of links rather than a second
/// strip of seller links under the first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PortalSection {
    Home,
    Purchases,
    SellerDashboard,
    SellerProducts,
    SellerOrders,
}

fn portal_sections(active: PortalSection, seller_enabled: bool) -> Markup {
    let tab = |section: PortalSection, href: &'static str, label: &'static str| components::Tab {
        active: section == active,
        href,
        label,
        icon: None,
    };
    let mut tabs = vec![
        tab(PortalSection::Home, "/b/products/", "Commerce"),
        tab(
            PortalSection::Purchases,
            "/b/products/my-purchases",
            "Purchases",
        ),
    ];
    if seller_enabled {
        tabs.extend([
            tab(
                PortalSection::SellerDashboard,
                "/b/products/selling",
                "Selling",
            ),
            tab(
                PortalSection::SellerProducts,
                "/b/products/my-products",
                "My products",
            ),
            tab(
                PortalSection::SellerOrders,
                "/b/products/selling/orders",
                "Seller orders",
            ),
        ]);
    }
    components::subnav("Commerce sections", tabs)
}

/// Which audience a products page serves, and so its sidebar and its
/// section links.
#[derive(Clone, Copy)]
enum Sections {
    Admin(AdminSection),
    Portal(PortalSection, bool),
}

impl Sections {
    fn markup(self) -> Markup {
        match self {
            Sections::Admin(section) => admin_sections(section),
            Sections::Portal(section, seller_enabled) => portal_sections(section, seller_enabled),
        }
    }

    /// The shell for this audience: `title` is the `<title>` and the page's
    /// `h1`.
    fn shell(self, title: &str) -> ui::Shell<'_> {
        match self {
            Sections::Admin(_) => ui::Shell::admin(title, title),
            Sections::Portal(..) => ui::Shell::portal(title, title),
        }
    }

    /// Where an error page in this section sends the reader back to.
    fn back(self) -> ui::BackLink<'static> {
        match self {
            Sections::Admin(AdminSection::Overview) => ui::BackLink::ADMIN_DASHBOARD,
            Sections::Admin(_) => ui::BackLink {
                label: "Back to the products overview",
                href: "/b/products/admin/",
            },
            Sections::Portal(PortalSection::Home, _) => ui::BackLink {
                label: "Back to your account",
                href: "/b/userportal/",
            },
            Sections::Portal(..) => ui::BackLink {
                label: "Back to Commerce",
                href: "/b/products/",
            },
        }
    }
}

/// A products page: `content` in the shell under the section links.
async fn products_page(
    ctx: &dyn Context,
    msg: &Message,
    shell: ui::Shell<'_>,
    sections: Sections,
    content: Markup,
) -> OutputStream {
    ui::shell_page(
        ctx,
        msg,
        shell,
        ui::PageBody::from(content).with_subnav(sections.markup()),
    )
    .await
}

/// A products page whose read failed: the error drawn in the shell under the
/// section links, titled `title`, with a link back
/// ([`ui::shell_error_page`]). Classified like every other failed read: a
/// denial is the 403, a quota the 429, anything else a logged 500.
async fn error_page(
    ctx: &dyn Context,
    msg: &Message,
    sections: Sections,
    title: &str,
    error: wafer_run::WaferError,
    context: &str,
) -> OutputStream {
    ui::shell_error_page(
        ctx,
        msg,
        sections.shell(title),
        Some(sections.markup()),
        sections.back(),
        error,
        context,
    )
    .await
}

/// [`error_page`] for the read of the one record a detail page is about: a
/// record that is not there is the 404 page, not a 500.
async fn record_error_page(
    ctx: &dyn Context,
    msg: &Message,
    sections: Sections,
    title: &str,
    error: wafer_run::WaferError,
    context: &str,
) -> OutputStream {
    if error.code == wafer_run::ErrorCode::NotFound {
        return ui::not_found_response(msg);
    }
    error_page(ctx, msg, sections, title, error, context).await
}

/// `count` followed by `noun`, pluralised with an `s` ("1 listing",
/// "3 listings").
fn count_noun(count: i64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// The "+" create action: a plus icon beside `label`, never a typed "+".
fn create_link(href: &str, label: &str, class: &str) -> Markup {
    html! {
        a class={ "btn btn--sm " (class) } href=(href) { (icons::plus()) " " (label) }
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
    let products_count = match repo::products::count(ctx, &[]).await {
        Ok(n) => n,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Overview),
                "Products",
                e,
                "Database error",
            )
            .await
        }
    };
    let groups_count = match repo::groups::count(ctx, &[]).await {
        Ok(n) => n,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Overview),
                "Products",
                e,
                "Database error",
            )
            .await
        }
    };
    let purchases_count = match repo::purchases::count_all(ctx).await {
        Ok(n) => n,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Overview),
                "Products",
                e,
                "Database error",
            )
            .await
        }
    };
    let offers_count = match repo::offers::count(ctx, &[]).await {
        Ok(n) => n,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Overview),
                "Products",
                e,
                "Database error",
            )
            .await
        }
    };
    let analytics = match repo::purchases::commerce_analytics(ctx, None).await {
        Ok(analytics) => analytics,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Overview),
                "Products",
                error,
                "Database error",
            )
            .await
        }
    };
    let user_products_enabled = match super::handlers::user_products_enabled(ctx).await {
        Ok(value) => value,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Overview),
                "Products",
                e,
                "products page: seller switch read failed",
            )
            .await
        }
    };

    let content = html! {
        div .stats-grid {
            (components::stat_card("Products", &products_count.to_string(), icons::package(), None))
            (components::stat_card("Groups", &groups_count.to_string(), icons::folder(), None))
            (components::stat_card("Offers", &offers_count.to_string(), icons::dollar_sign(), None))
            (components::stat_card("Orders", &purchases_count.to_string(), icons::shopping_cart(), None))
        }
        section .products-section {
            (components::section_header("Get selling in three steps", None))
            p .section-desc { "Start with the essentials. You can refine every setting later." }
            div .products-guide {
                article .products-guide__item {
                    span .products-guide__number { "1" }
                    h3 { "Connect Stripe" }
                    p .text-muted .text-sm { "Add your Stripe keys and confirm that payments are ready." }
                    a .products-guide__link .text-sm href="/b/products/admin/stripe" { "Check Stripe setup" (icons::arrow_right()) }
                }
                article .products-guide__item {
                    span .products-guide__number { "2" }
                    h3 { "Create a product" }
                    p .text-muted .text-sm { "Choose a one-time product, subscription, or configurable checkout." }
                    a .products-guide__link .text-sm href="/b/products/admin/new" { "Open the product wizard" (icons::arrow_right()) }
                }
                article .products-guide__item {
                    span .products-guide__number { "3" }
                    h3 { "Publish and share" }
                    p .text-muted .text-sm { "Publish the price, then copy a payment link or add checkout to your site." }
                    a .products-guide__link .text-sm href="/b/products/admin/manage" { "Manage products" (icons::arrow_right()) }
                }
            }
        }
        (render_overview_empty_state(products_count, user_products_enabled))
        (analytics_section(&analytics, "Sales and subscriptions", false))
    };

    products_page(
        ctx,
        msg,
        ui::Shell::admin("Products", "Products")
            .subtitle("Everything you need to set up your catalog and start taking payments")
            .actions(vec![create_link(
                "/b/products/admin/new",
                "Create product",
                "btn--primary",
            )]),
        Sections::Admin(AdminSection::Overview),
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
///     `routes::user_products_refusal`) was previously the
///     only signal a new admin got that self-serve selling is disabled —
///     name the var and link to Settings, where it's the first toggle in
///     the Features section. The admin JSON create route
///     (`/b/products/api/admin/products`) is NOT gated by this flag, so no
///     CTA is withheld here — this state is purely informational.
///   - Otherwise: an "Add your first product" prompt whose action opens the
///     product wizard (the real create path, wired to that same admin
///     route). A secondary button: the topbar's "Create product" is the
///     page's one primary action.
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
                (create_link("/b/products/admin/new", "Create your first product", "btn--secondary"))
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
    // The default list is live products. `?view=deleted` is the only way to
    // reach a soft-deleted row from this page — without it, soft delete
    // would be a one-way door: a deleted product would be unreachable by
    // any UI and there would be no way back to restore it.
    let deleted_view = msg.query("view") == "deleted";
    // Carried through the search box and pagination links below so acting
    // on either one doesn't silently bounce the admin back to the live view.
    let base_href = if deleted_view {
        "/b/products/admin/manage?view=deleted"
    } else {
        "/b/products/admin/manage"
    };

    let mut filters = Vec::new();
    if let Some(search) = super::handlers::name_like_filter(&search) {
        filters.push(search);
    }

    // Each view is ordered by the timestamp its own table shows: the live
    // list by `created_at`, the deleted list by the `Deleted` column it
    // renders. Sorting the deleted list by `created_at` would file the
    // product an admin just deleted under its creation date — for an old
    // product, the bottom of the list, on the one page whose whole purpose
    // is undoing a delete that was probably a moment ago.
    let sort = vec![SortField {
        field: if deleted_view {
            "deleted_at".into()
        } else {
            "created_at".into()
        },
        desc: true,
    }];
    // `list_page` appends the live-only filter; `list_deleted` appends the
    // opposite one. Both append onto this same caller-supplied `filters`,
    // so the search box narrows either view the same way.
    let result = if deleted_view {
        repo::products::list_deleted(ctx, page as i64, page_size as i64, filters, Some(sort)).await
    } else {
        repo::products::list_page(ctx, page as i64, page_size as i64, filters, Some(sort)).await
    };
    let list = match result {
        Ok(list) => list,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Products),
                "All products",
                e,
                "products admin list page: product read failed",
            )
            .await
        }
    };

    let content = html! {
        div .filter-bar {
            (product_views("/b/products/admin/manage", deleted_view))
            (components::search_input_with_value("search", "Search by product name", base_href, "#content", &search))
        }

        @if deleted_view {
            @let cols = [
                components::TableCol::new("Name").primary(),
                components::TableCol::new("Owner"),
                components::TableCol::new("Currency"),
                components::TableCol::new("Deleted"),
                components::TableCol::new("Actions").actions(),
            ];
            @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|record| {
                let seller_owned = record.str_field("owner_kind") == "user";
                // Percent-encoded, like every `encodeURIComponent`
                // call in this file's browser-side URLs. A
                // product id is not guaranteed URL-safe: the
                // database layer synthesizes a UUID only when the
                // body omits `id`, and the admin create endpoint
                // forwarded the body verbatim until this branch
                // began refusing it — so an id holding `/`, `?` or
                // `#` exists wherever a seeding client ever chose
                // its own keys. Unencoded, such an id splits the
                // path and the Restore button posts somewhere
                // that matches no route, on the only door out of
                // soft delete. maud escapes HTML, not URLs.
                let encoded_id = crate::util::url_path_encode(&record.id);
                let restore_url =
                    format!("/b/products/api/admin/products/{encoded_id}/restore");
                // Restore is the DANGEROUS half of what an admin
                // can do here: it puts an active, approved product
                // straight back into the public catalog. Soft
                // delete takes nothing down in Stripe, so the row
                // also needs the other half — a way to shut the
                // product's Prices and Payment Links down without
                // relisting it. That is what
                // `ProductState::LiveOrDeleted` exists for, and
                // until this link nothing reached it.
                let close_url =
                    format!("/b/products/admin/products/{encoded_id}/close");
                vec![
                    html! { (record.str_field("name")) },
                    html! { span .text-muted .text-sm { @if seller_owned { "Seller" } @else { "Your store" } } },
                    html! { (record.str_field("currency")) },
                    components::timestamp(record.str_field("deleted_at")),
                    deleted_row_actions(&close_url, &restore_url),
                ]
            }).collect();
            (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! {
                (product_list_empty(&search, true, "/b/products/admin/new"))
            }))
        } @else {
            @let row_hrefs: Vec<String> = list.records.iter().map(|record| format!("/b/products/admin/products/{}", crate::util::url_path_encode(&record.id))).collect();
            @let cols = [
                components::TableCol::new("Name").primary(),
                components::TableCol::new("Availability"),
                components::TableCol::new("Owner"),
                components::TableCol::new("Currency"),
                components::TableCol::new("Updated"),
            ];
            @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|record| {
                let seller_owned = record.str_field("owner_kind") == "user";
                vec![
                    html! { (record.str_field("name")) },
                    html! { div .products-status-stack { (components::status_badge(record.str_field("status"))) @if seller_owned { (components::status_badge(record.str_field("approval_status"))) } } },
                    html! { span .text-muted .text-sm { @if seller_owned { "Seller" } @else { "Your store" } } },
                    html! { (record.str_field("currency")) },
                    components::timestamp(record.str_field("updated_at")),
                ]
            }).collect();
            (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! {
                (product_list_empty(&search, false, "/b/products/admin/new"))
            }))
        }
        @if let Some(per_page) = std::num::NonZeroU32::new(page_size as u32) { (components::pagination(list.page as u32, per_page, list.total_count as u32, &with_search(base_href, &search))) }
    };

    let mut shell = ui::Shell::admin("All products", "All products").subtitle(if deleted_view {
        "Restore a product to bring it back into your catalog — a deleted product cannot be edited until it is restored"
    } else {
        "Create, publish, and share the things you sell"
    });
    if !deleted_view {
        shell = shell.actions(vec![create_link(
            "/b/products/admin/new",
            "New product",
            "btn--primary",
        )]);
    }
    products_page(
        ctx,
        msg,
        shell,
        Sections::Admin(AdminSection::Products),
        content,
    )
    .await
}

/// `href` with the list's `search` term carried along, so paging a search
/// result does not silently drop the search.
fn with_search(href: &str, search: &str) -> String {
    if search.is_empty() {
        return href.to_string();
    }
    let join = if href.contains('?') { '&' } else { '?' };
    format!("{href}{join}search={}", crate::util::urlencode(search))
}

/// The Active / Deleted views of a product list (`base` is the list's own
/// path), as filter links: views of one list, not sections of the block.
fn product_views(base: &str, deleted_view: bool) -> Markup {
    let deleted = format!("{base}?view=deleted");
    components::filter_links(
        "Product views",
        vec![
            components::Tab {
                active: !deleted_view,
                href: base,
                label: "Active",
                icon: None,
            },
            components::Tab {
                active: deleted_view,
                href: &deleted,
                label: "Deleted",
                icon: Some(icons::trash()),
            },
        ],
    )
}

/// A deleted product row's two ways out: close its Stripe surface, or
/// restore it.
fn deleted_row_actions(close_url: &str, restore_url: &str) -> Markup {
    html! {
        div .products-actions {
            a .btn .btn--secondary .btn--sm href=(close_url) { "Close Stripe surface" }
            button .btn .btn--secondary .btn--sm type="button"
                hx-post=(restore_url)
                hx-swap="none"
                data-error-label="Could not restore this product"
                data-reload-on-success
            { "Restore" }
        }
    }
}

/// What an empty product list says: a search that matched nothing says so
/// (and nothing else), an empty Deleted view says what it is for, and an
/// empty catalog points at the wizard — as a secondary action, since the
/// topbar already holds the page's primary one.
fn product_list_empty(search: &str, deleted_view: bool, new_href: &str) -> Markup {
    if !search.is_empty() {
        return components::empty_state(
            icons::search(),
            "No products match your search",
            "Try a different name, or clear the search.",
            None,
        );
    }
    if deleted_view {
        return components::empty_state(
            icons::trash(),
            "No deleted products",
            "Products stay here after deletion until you restore them.",
            None,
        );
    }
    components::empty_state(
        icons::package(),
        "No products yet",
        "Create your first product with the product wizard.",
        Some(create_link(new_href, "Create product", "btn--secondary")),
    )
}

/// Close-only manager for a soft-deleted product: archive its offers,
/// deactivate its Payment Links, and nothing else.
///
/// Soft delete touches nothing in Stripe. A deleted product's Prices and
/// Payment Links stay live in the connected account and keep taking money,
/// and the delete handler archives none of them — so
/// `offers::handle_list`/`handle_archive` and
/// `payment_links::list_links`/`deactivate_link` read through
/// `ProductState::LiveOrDeleted` precisely so that surface can be shut down.
/// This page is what reaches them: [`product_manager`] loads through the live-only
/// `repo::products::get` and 404s for a deleted product, and the Deleted view
/// offered only **Restore** — which returns an active, approved product to
/// the public catalog before anything has been closed. The one affordance on
/// offer was the dangerous one.
///
/// Deliberately NOT a mode of [`product_manager`]: that page's every other
/// control creates, edits, publishes, syncs, duplicates or opens a new way to
/// charge, and every one of those is refused for a deleted product by the
/// `ProductState::Live` gate below it. Rendering them disabled would be a
/// page that mostly does not work; rendering them live would be an invitation
/// to do the opposite of what this page is for.
///
/// Reads through `repo::products::get_deleted`, so it exists only for a
/// product that is actually deleted — a live one belongs in
/// [`product_manager`], which can do everything.
///
/// `admin` selects the tier this page is serving, exactly as
/// [`product_manager`]'s flag does: the admin form is reached from
/// `/b/products/admin/products/{id}/close` and drives the admin API; the
/// owner form is reached from `/b/products/my-products/{id}/close`, drives
/// the seller API, and additionally checks that the caller owns the product.
///
/// One page for both, not two, because the seller's need is the sharper one:
/// a seller who deletes their own product loses every view of it while its
/// Prices and Payment Links keep taking money in the connected account, and
/// until this page they had to ask an administrator. The two tiers differ
/// only in chrome, URLs, and whose products they may open — the closing
/// operations are identical, and the seller-tier API already permitted every
/// one of them (`ProductState::LiveOrDeleted` covers `OfferAccess::Owner`).
pub async fn deleted_product_close(
    ctx: &dyn Context,
    msg: &Message,
    product_id: &str,
    admin: bool,
) -> OutputStream {
    let product = match repo::products::get_deleted(ctx, product_id).await {
        Ok(product) => product,
        Err(error) => {
            return record_error_page(
                ctx,
                msg,
                if admin {
                    Sections::Admin(AdminSection::Products)
                } else {
                    Sections::Portal(PortalSection::SellerProducts, true)
                },
                "Product",
                error,
                "products page: product read failed",
            )
            .await
        }
    };
    if !admin && !super::handlers::is_owned_by(&product, msg.user_id()) {
        // The shared ownership rule, the same one `product_manager` and every
        // seller API route use — and the ENTIRE authorization boundary on the
        // owner form, since its declared tier only says "logged in". 404
        // rather than 403: a non-owner must not learn the product exists.
        return ui::not_found_response(msg);
    }
    let offers = match repo::offers::list_for_product(ctx, product_id).await {
        Ok(offers) => offers,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                if admin {
                    Sections::Admin(AdminSection::Products)
                } else {
                    Sections::Portal(PortalSection::SellerProducts, true)
                },
                "Product",
                error,
                "Could not load product pricing",
            )
            .await
        }
    };
    // One listing per offer rather than a join: `list_links` is the same read
    // the API exposes, and there are as many of them as the offer list the
    // admin is about to act on.
    let mut links = Vec::with_capacity(offers.len());
    for offer in &offers {
        match repo::payment_links::list_for_offer(ctx, &offer.offer.id).await {
            Ok(list) => links.push(list),
            Err(error) => {
                return error_page(
                    ctx,
                    msg,
                    if admin {
                        Sections::Admin(AdminSection::Products)
                    } else {
                        Sections::Portal(PortalSection::SellerProducts, true)
                    },
                    "Product",
                    error,
                    "Could not load payment links",
                )
                .await
            }
        }
    }

    let encoded_id = crate::util::url_path_encode(product_id);
    let api_base = if admin {
        format!("/b/products/api/admin/products/{encoded_id}")
    } else {
        format!("/b/products/api/products/{encoded_id}")
    };
    let deleted_href = if admin {
        "/b/products/admin/manage?view=deleted"
    } else {
        "/b/products/my-products?view=deleted"
    };
    let seller_enabled = !admin
        && match super::handlers::user_products_enabled(ctx).await {
            Ok(value) => value,
            Err(e) => {
                return error_page(
                    ctx,
                    msg,
                    if admin {
                        Sections::Admin(AdminSection::Products)
                    } else {
                        Sections::Portal(PortalSection::SellerProducts, true)
                    },
                    "Product",
                    e,
                    "products page: seller switch read failed",
                )
                .await
            }
        };
    let deleted_at = product.str_field("deleted_at");
    let content = html! {
        p .text-muted .text-sm {
            "Deleted " (components::timestamp(deleted_at))
            ". Restoring it is on the Deleted tab — a restored product returns to your catalog immediately, so close anything that should stop selling first."
        }
        p #close-manager-error .login-error role="alert" aria-live="assertive" hidden {}

        @if offers.is_empty() {
            (components::empty_state(
                icons::package(),
                "Nothing left to close",
                "This product has no offers, so it has no prices or payment links in Stripe.",
                None,
            ))
        }
        @for (offer, offer_links) in offers.iter().zip(links.iter()) {
            @let offer_url = format!("{api_base}/offers/{}", crate::util::url_path_encode(&offer.offer.id));
            article .card .mt-4 {
                header .card__head {
                    div .products-status-stack {
                        h2 .card__title .m-0 { (offer.offer.name) }
                        (components::status_badge(&commerce_wire(&offer.status)))
                    }
                    div .products-actions {
                        @if offer.status != OfferStatus::Archived {
                            button .btn .btn--secondary .btn--sm type="button"
                                hx-delete=(offer_url)
                                hx-swap="none"
                                data-error-label="Could not archive this offer"
                                data-reload-on-success
                            { "Archive offer" }
                        }
                    }
                }
                div .card__body {
                    @if offer_links.is_empty() {
                        p .text-muted .text-sm .m-0 { "No payment links." }
                    } @else {
                        ul .products-payment-link-list {
                            @for link in offer_links {
                                @let link_url = format!("{offer_url}/payment-links/{}", crate::util::url_path_encode(&link.id));
                                li .flex .gap-3 .items-center .justify-between .flex-wrap {
                                    span .text-sm .products-payment-link-url {
                                        (if link.url.is_empty() { link.id.as_str() } else { link.url.as_str() })
                                    }
                                    div .flex .gap-2 .items-center {
                                        (components::status_badge(if link.active { "active" } else { "inactive" }))
                                        @if link.active {
                                            button .btn .btn--secondary .btn--sm type="button"
                                                hx-delete=(link_url)
                                                hx-swap="none"
                                                data-error-label="Could not deactivate this payment link"
                                                data-reload-on-success
                                            { "Deactivate" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    let sections = if admin {
        Sections::Admin(AdminSection::Products)
    } else {
        Sections::Portal(PortalSection::SellerProducts, seller_enabled)
    };
    let name = product.str_field("name");
    let shell = sections
        .shell(name)
        .trail(vec![
            ui::shell::Crumb {
                label: "Deleted products",
                href: Some(deleted_href),
            },
            ui::shell::Crumb {
                label: name,
                href: None,
            },
        ])
        .subtitle("Archive its offers and deactivate its payment links so it stops taking money — deleting a product does none of that in Stripe");
    products_page(ctx, msg, shell, sections, content).await
}

// ---------------------------------------------------------------------------
// Admin: seller governance and moderation
// ---------------------------------------------------------------------------

pub async fn admin_sellers(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let sellers = match repo::seller_accounts::list_rows(ctx).await {
        Ok(sellers) => sellers,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Sellers),
                "Sellers",
                error,
                "Could not list sellers",
            )
            .await
        }
    };
    let seller_total = if sellers.truncated {
        match repo::seller_accounts::count_all(ctx).await {
            Ok(total) => total,
            Err(error) => {
                return error_page(
                    ctx,
                    msg,
                    Sections::Admin(AdminSection::Sellers),
                    "Sellers",
                    error,
                    "Could not count sellers",
                )
                .await
            }
        }
    } else {
        sellers.rows.len() as i64
    };
    // A GROUP BY, not a scan: the listing count beside each seller is derived
    // from every seller product on the platform, and that table grows without
    // bound.
    let product_counts = match repo::products::live_counts_by_owner(ctx, "user").await {
        Ok(counts) => counts,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Sellers),
                "Sellers",
                error,
                "Could not count seller products",
            )
            .await
        }
    };
    // The queue's predicate goes into the query for the same reason. The
    // total comes from a COUNT so the heading states how many listings are
    // waiting even when the table below shows only the first of them.
    let pending_total = match repo::products::count_pending_review(ctx).await {
        Ok(total) => total,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Sellers),
                "Sellers",
                error,
                "Could not count the moderation queue",
            )
            .await
        }
    };
    let pending = match repo::products::list_pending_review(ctx).await {
        Ok(pending) => pending,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Sellers),
                "Sellers",
                error,
                "Could not list seller products",
            )
            .await
        }
    };
    let selling_enabled = match super::handlers::user_products_enabled(ctx).await {
        Ok(value) => value,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Sellers),
                "Sellers",
                e,
                "products page: seller switch read failed",
            )
            .await
        }
    };

    let content = html! {
        @if !selling_enabled {
            (components::callout(
                components::CalloutTone::Info,
                "Seller products are turned off",
                html! { p { "Existing sellers remain visible, but new seller listings cannot be created." } },
                Some(html! { a .btn .btn--secondary .btn--sm href="/b/products/admin/settings" { "Open settings" } }),
            ))
        }
        section .products-section {
            (components::section_header("Moderation queue", None))
            p .section-desc {
                @if pending_total == 0 {
                    "No listings are waiting for a decision."
                } @else {
                    (count_noun(pending_total, "listing")) " waiting for a decision."
                }
                @if pending.truncated { " Showing the first " (pending.rows.len()) "." }
            }
            @if pending.rows.is_empty() {
                (components::empty_state(icons::info(), "Queue clear", "No seller listings are waiting for review.", None))
            } @else {
                @let row_hrefs: Vec<String> = pending.rows.iter().map(|product| format!("/b/products/admin/products/{}", crate::util::url_path_encode(&product.id))).collect();
                @let cols = [
                    components::TableCol::new("Product").primary(),
                    components::TableCol::new("Seller"),
                    components::TableCol::new("Submitted"),
                    components::TableCol::new("Status"),
                ];
                @let rows: Vec<Vec<Markup>> = pending.rows.iter().map(|product| vec![
                    html! { (product.str_field("name")) },
                    html! { span .text-muted .text-sm { (product.str_field("owner_id")) } },
                    components::timestamp(product.str_field("submitted_at")),
                    components::status_badge("pending review"),
                ]).collect();
                (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! {}))
            }
        }
        section .products-section {
            (components::section_header("Seller accounts", None))
            p .section-desc {
                "Open a seller to review payment readiness and their products."
                @if sellers.truncated { " Showing the first " (sellers.rows.len()) " of " (seller_total) "." }
            }
            @if sellers.rows.is_empty() {
                (components::empty_state(icons::link(), "No sellers yet", "Seller accounts appear here after a user starts Stripe onboarding.", None))
            } @else {
                @let row_hrefs: Vec<String> = sellers.rows.iter().map(|seller| format!("/b/products/admin/sellers/{}", seller.id)).collect();
                @let cols = [
                    components::TableCol::new("Seller").primary(),
                    components::TableCol::new("Selling"),
                    components::TableCol::new("Payments"),
                    components::TableCol::new("Payouts"),
                    components::TableCol::new("Listings"),
                    components::TableCol::new("Needs action"),
                ];
                @let rows: Vec<Vec<Markup>> = sellers.rows.iter().map(|seller| vec![
                    html! { (&seller.user_id) },
                    components::status_badge(&commerce_wire(&seller.status)),
                    components::status_badge(if seller.capabilities.charges_enabled { "enabled" } else { "disabled" }),
                    components::status_badge(if seller.capabilities.payouts_enabled { "enabled" } else { "disabled" }),
                    html! { (product_counts.get(&seller.user_id).copied().unwrap_or_default()) },
                    html! { @if seller.capabilities.requirements_due.is_empty() { span .text-muted { "None" } } @else { strong { (seller.capabilities.requirements_due.len()) } } },
                ]).collect();
                (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! {}))
            }
        }
    };
    products_page(
        ctx,
        msg,
        ui::Shell::admin("Sellers", "Sellers")
            .subtitle("Approve listings and help sellers get ready to take payments"),
        Sections::Admin(AdminSection::Sellers),
        content,
    )
    .await
}

pub async fn admin_seller_detail(
    ctx: &dyn Context,
    msg: &Message,
    seller_id: &str,
) -> OutputStream {
    // Not `platform_fee`: this page carries the suspend control, and a typo
    // in the fee setting must not take the fraud control down with it. An
    // unreadable fee is shown as exactly that, never as a number.
    let fee = super::config::seller_fee_bps(ctx).await.map_err(|error| {
        tracing::warn!(error = %error, "platform application fee setting cannot be read");
    });
    let seller = match repo::seller_accounts::get_row(ctx, seller_id).await {
        Ok(Some(seller)) => seller,
        Ok(None) => return ui::not_found_response(msg),
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Sellers),
                "Seller",
                error,
                "Could not load seller",
            )
            .await
        }
    };
    let products = match repo::products::list_owned_by(ctx, &seller.user_id).await {
        Ok(products) => products,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Sellers),
                "Seller",
                error,
                "Could not list seller products",
            )
            .await
        }
    };
    let suspended = seller.status == SellerStatus::Suspended;
    let action = if suspended { "reactivate" } else { "suspend" };
    let action_label = if suspended {
        "Reactivate seller"
    } else {
        "Suspend seller"
    };
    let action_class = if suspended {
        "btn--primary"
    } else {
        "btn--secondary"
    };
    let config = ui::script_json(&serde_json::json!({
        "action_url": format!("/b/products/api/admin/sellers/{seller_id}/{action}"),
        "action": action,
    }));
    let content = html! {
        p #seller-admin-error .login-error role="alert" aria-live="assertive" hidden {}
        section .card {
            header .card__head {
                div {
                    h2 .card__title { "Seller account" }
                    p .text-muted .text-sm .text-subtitle { "Stripe verification and selling access" }
                }
                div .flex .gap-2 .items-center .flex-wrap {
                    (components::status_badge(&commerce_wire(&seller.status)))
                    button .btn .(action_class) .btn--sm type="button" data-seller-action=(action) data-action="psa-set-state" { (action_label) }
                }
            }
            div .card__body {
                div .stats-grid {
                    (components::stat_card("Payments", if seller.capabilities.charges_enabled { "Enabled" } else { "Disabled" }, icons::dollar_sign(), None))
                    (components::stat_card("Payouts", if seller.capabilities.payouts_enabled { "Enabled" } else { "Disabled" }, icons::arrow_up_right(), None))
                    (components::stat_card("Verification", if seller.capabilities.details_submitted { "Complete" } else { "Incomplete" }, icons::info(), None))
                    (components::stat_card("Platform fee", &fee.map_or_else(|()| "Misconfigured".to_string(), |fee| fee_percent(fee.into())), icons::dollar_sign(), None))
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
                h3 .products-subheading { "What this seller still needs to do" }
                @if seller.capabilities.requirements_due.is_empty() {
                    p .text-muted .text-sm { "Nothing — Stripe has no outstanding requirements." }
                } @else {
                    ul { @for requirement in &seller.capabilities.requirements_due { li { (friendly_requirement(requirement)) } } }
                }
            }
        }
        section .products-section {
            (components::section_header("Owned products", None))
            @if products.truncated {
                p .section-desc { "Showing the first " (products.rows.len()) " of this seller's live products." }
            }
            @if products.rows.is_empty() {
                (components::empty_state(icons::package(), "No products", "This seller has not created any products.", None))
            } @else {
                @let row_hrefs: Vec<String> = products.rows.iter().map(|product| format!("/b/products/admin/products/{}", crate::util::url_path_encode(&product.id))).collect();
                @let cols = [
                    components::TableCol::new("Product").primary(),
                    components::TableCol::new("Status"),
                    components::TableCol::new("Approval"),
                    components::TableCol::new("Updated"),
                ];
                @let rows: Vec<Vec<Markup>> = products.rows.iter().map(|product| vec![
                    html! { (product.str_field("name")) },
                    components::status_badge(product.str_field("status")),
                    components::status_badge(product.str_field("approval_status")),
                    components::timestamp(product.str_field("updated_at")),
                ]).collect();
                (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), html! {}))
            }
        }
        script { (maud::PreEscaped(format!("window.__sellerAdminConfig={config};"))) }
        script src=(assets::seller_admin_js_url()) {}
    };
    products_page(
        ctx,
        msg,
        ui::Shell::admin(&seller.user_id, &seller.user_id)
            .trail(vec![
                ui::shell::Crumb {
                    label: "Sellers",
                    href: Some("/b/products/admin/sellers"),
                },
                ui::shell::Crumb {
                    label: &seller.user_id,
                    href: None,
                },
            ])
            .subtitle("Review payment readiness, outstanding steps, and seller listings"),
        Sections::Admin(AdminSection::Sellers),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Shared admin/seller product wizard
// ---------------------------------------------------------------------------

pub async fn product_wizard(ctx: &dyn Context, msg: &Message, admin: bool) -> OutputStream {
    let configured_currency =
        match wafer_core::clients::config::get_default(ctx, DEFAULT_CURRENCY, "USD").await {
            Ok(currency) => currency,
            Err(e) => {
                return error_page(
                    ctx,
                    msg,
                    if admin {
                        Sections::Admin(AdminSection::Products)
                    } else {
                        Sections::Portal(PortalSection::SellerProducts, true)
                    },
                    "Create product",
                    e,
                    "product wizard: default currency read failed",
                )
                .await
            }
        };
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
        match super::handlers::seller_policy::allowed_templates(ctx).await {
            Ok(templates) => templates,
            Err(e) => {
                return error_page(
                    ctx,
                    msg,
                    if admin {
                        Sections::Admin(AdminSection::Products)
                    } else {
                        Sections::Portal(PortalSection::SellerProducts, true)
                    },
                    "Create product",
                    e,
                    "product wizard: seller policy read failed",
                )
                .await
            }
        }
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
        match super::handlers::seller_policy::allowed_currencies(ctx).await {
            Ok(currencies) => currencies.into_iter().collect::<Vec<_>>(),
            Err(e) => {
                return error_page(
                    ctx,
                    msg,
                    if admin {
                        Sections::Admin(AdminSection::Products)
                    } else {
                        Sections::Portal(PortalSection::SellerProducts, true)
                    },
                    "Create product",
                    e,
                    "product wizard: seller policy read failed",
                )
                .await
            }
        }
    };
    seller_currencies.sort();
    if !seller_currencies.is_empty() && !seller_currencies.contains(&default_currency) {
        default_currency = seller_currencies[0].clone();
    }
    let automatic_tax = match super::stripe::automatic_tax_enabled(ctx).await {
        Ok(enabled) => enabled,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                if admin {
                    Sections::Admin(AdminSection::Products)
                } else {
                    Sections::Portal(PortalSection::SellerProducts, true)
                },
                "Create product",
                e,
                "product wizard: automatic tax read failed",
            )
            .await
        }
    };
    // Blank when no platform country is configured: the field's placeholder
    // then asks for the list, which is the honest prompt. It used to prefill
    // `US` on a deployment that had never said it was in the US.
    let platform_country = match super::config::platform_country(ctx).await {
        Ok(country) => country
            .map(|code| code.as_str().to_string())
            .unwrap_or_default(),
        Err(error) => return crate::http::err_internal("Platform country is misconfigured", error),
    };
    let back_href = if admin {
        "/b/products/admin/manage"
    } else {
        "/b/products/my-products"
    };
    let content = html! {
        (wizard_progress())
        form #product-wizard-form novalidate {
            p #product-wizard-error .text-sm role="alert" aria-live="assertive" hidden .text-danger .mt-0 {}

            section .card data-wizard-step="1" {
                header .card__head {
                    div {
                        h2 .card__title { "What are you selling?" }
                        p .text-muted .text-sm .text-subtitle { "Choose the closest match. You can change every detail before saving." }
                    }
                }
                div .card__body {
                    fieldset .fieldset-reset {
                        legend .sr-only { "Product template" }
                        div .product-template-grid {
                            @for (value, title, description) in &template_definitions {
                                label .product-template-card {
                                    input type="radio" name="product_template" value=(value) checked[*value == initial_template] data-action="pw-template-changed";
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
                header .card__head { h2 .card__title { "Product details" } }
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
                                    input #wizard-slug .form-input type="text" maxlength=(PRODUCT_SLUG_MAX_LEN) pattern=(PRODUCT_SLUG_PATTERN) placeholder="Generated from the product name";
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
                        h2 .card__title { "Pricing" }
                        p .text-muted .text-sm .text-subtitle { "Set the amount customers will see at checkout." }
                    }
                }
                div .card__body {
                    div .grid .grid-auto-180 .gap-4 {
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
                        details .products-advanced .col-span-full .mt-0 {
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
                        section .mt-4 {
                            div .flex .items-center .justify-between .gap-4 {
                                div {
                                    h3 .products-subheading .m-0 { "Customer fields" }
                                    p .text-muted .text-sm { "Collect dates, quantities, choices, toggles, and notes from the customer." }
                                }
                                button .btn .btn--secondary .btn--sm type="button" data-action="pw-add-variable" { (icons::plus()) " Add input" }
                            }
                            div #wizard-variables {}
                        }
                        section .products-section {
                            div .flex .items-center .justify-between .gap-4 {
                                div {
                                    h3 .products-subheading .m-0 { "Itemized price rows" }
                                    p .text-muted .text-sm { "Build the total from clear rows such as base booking, nights, guests, and add-ons." }
                                }
                                button .btn .btn--secondary .btn--sm type="button" data-action="pw-add-component" { (icons::plus()) " Add row" }
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
                header .card__head { h2 .card__title { "Checkout options" } }
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
                                input id=(id) type="checkbox" checked[checked] data-action="pw-shipping-changed";
                                span { strong { (label) } small .text-muted { (help) } }
                            }
                        }
                        div .form-group data-subscription-field hidden {
                            label .form-label for="wizard-trial-days" { "Free trial days" }
                            input #wizard-trial-days .form-input type="number" min="0" max="730" value="0";
                        }
                    }
                    div #wizard-shipping-settings .card hidden .mt-4 {
                        div .card__body {
                            h3 .products-subheading .mt-0 { "Shipping destinations and rates" }
                            div .grid .grid-shipping .gap-4 {
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
                header .card__head { h2 .card__title { "Review" } }
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
                button #wizard-previous .btn .btn--secondary .btn--md type="button" data-action="pw-previous" hidden { "Back" }
                div .product-wizard-actions__buttons {
                    button #wizard-next .btn .btn--primary .btn--md type="button" data-action="pw-next" disabled[template_definitions.is_empty()] { "Continue" }
                    button #wizard-save-draft .btn .btn--secondary .btn--md type="button" data-action="pw-submit" data-wizard-intent="draft" hidden { "Save draft" }
                    button #wizard-publish .btn .btn--primary .btn--md type="button" data-action="pw-submit" data-wizard-intent="publish" hidden {
                        @if admin { "Create and publish" } @else { "Submit for publication" }
                    }
                }
            }
        }
        script { (maud::PreEscaped(product_wizard_bootstrap(admin))) }
        script src=(assets::wizard_js_url()) {}
    };
    let sections = if admin {
        Sections::Admin(AdminSection::Products)
    } else {
        Sections::Portal(PortalSection::SellerProducts, true)
    };
    let shell = sections
        .shell("Create product")
        .trail(vec![
            ui::shell::Crumb {
                label: if admin { "All products" } else { "My products" },
                href: Some(back_href),
            },
            ui::shell::Crumb {
                label: "Create product",
                href: None,
            },
        ])
        .subtitle("Choose a starting point, add the essentials, and publish when you are ready")
        .actions(vec![
            html! { a .btn .btn--secondary .btn--sm href=(back_href) { "Cancel" } },
        ]);
    products_page(ctx, msg, shell, sections, content).await
}

/// The wizard's five steps, as an ordered list. The current step carries
/// `aria-current="step"` and the brand pill; a finished one a check mark and
/// the success pill; so the state is never colour alone. `products-wizard.js`
/// moves all three as the user steps through. Below 560px only the current
/// step shows its name — the others keep theirs for screen readers — so the
/// five pills fit one row on a phone.
fn wizard_progress() -> Markup {
    html! {
        nav .product-wizard-progress aria-label="Product setup progress" {
            ol {
                @for (number, label) in [(1, "Type"), (2, "Basics"), (3, "Price"), (4, "Checkout"), (5, "Publish")] {
                    li data-wizard-indicator=(number)
                        .badge .(if number == 1 { "badge-primary" } else { "badge-secondary" }) .badge--center
                        aria-current=[(number == 1).then_some("step")]
                    {
                        span .wizard-step-check aria-hidden="true" hidden { (icons::check()) }
                        span .wizard-step__number { (number) }
                        span .wizard-step__label { (label) }
                    }
                }
            }
        }
    }
}

fn product_wizard_bootstrap(admin: bool) -> String {
    let config = ui::script_json(&serde_json::json!({
        "admin": admin,
        "product_collection": if admin { "/b/products/api/admin/products" } else { "/b/products/api/products" },
        "return_url": if admin { "/b/products/admin/manage" } else { "/b/products/my-products" },
    }));
    format!("window.__productWizardConfig={config};")
}

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
                    label .flex .items-center .gap-2 .min-h-10 {
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
        section .card data-offer-card data-offer-id=(offer.id) data-offer-url=(offer_url) data-preview-url=(preview_url) data-presets-url=(presets_url) data-links-url=(links_url) data-currency=(offer.currency) .mt-4 {
            header .card__head {
                div {
                    div .flex .items-center .gap-2 .flex-wrap {
                        h2 .card__title { (offer.name) }
                        (components::status_badge(&status))
                        (components::badge(components::BadgeVariant::Secondary, &format!("v{}", offer.version)))
                    }
                    p .text-muted .text-sm .text-subtitle {
                        (charge_label) " · " (pricing_label) " pricing · " (offer.currency)
                        @if let Some(interval) = offer.recurring_interval {
                            " · every " (offer.interval_count) " " (commerce_wire(&interval))
                        }
                        @if managed.sync_status == OfferSyncStatus::Failed { " · Stripe needs attention" }
                        @else if managed.sync_status == OfferSyncStatus::Synced { " · Synced with Stripe" }
                    }
                }
                div .products-actions {
                    @if managed.status == OfferStatus::Draft {
                        button .btn .btn--secondary .btn--sm type="button" data-action="pm-open-visual-editor" { "Edit visually" }
                    }
                    @if managed.status == OfferStatus::Active {
                        button .btn .btn--secondary .btn--sm type="button" data-action="pm-offer-action" data-offer-op="sync" {
                            @if managed.sync_status == OfferSyncStatus::Failed {
                                "Retry Stripe sync"
                            } @else if managed.sync_status == OfferSyncStatus::Synced {
                                "Reconcile Stripe"
                            } @else {
                                "Sync to Stripe"
                            }
                        }
                    }
                    button .btn .btn--secondary .btn--sm type="button" data-action="pm-offer-action" data-offer-op="duplicate" { "Duplicate to draft" }
                    @if managed.status != OfferStatus::Archived {
                        button .btn .btn--secondary .btn--sm type="button" data-action="pm-offer-action" data-offer-op="archive" { "Archive" }
                    }
                    // The offer's one primary action, last.
                    @if managed.status == OfferStatus::Draft {
                        button .btn .btn--primary .btn--sm type="button" data-action="pm-offer-action" data-offer-op="publish" { "Publish" }
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
                            div .flex .items-center .justify-between .gap-4 .flex-wrap {
                                div {
                                    h3 .products-subheading { "Test checkout price" }
                                    p .text-muted .text-sm { "Enter a typical order to confirm the amount customers will see." }
                                }
                                button .btn .btn--secondary .btn--sm type="button" data-action="pm-preview" { "Calculate preview" }
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
                div .grid .grid-auto-220 .gap-4 {
                    div {
                        h3 .products-subheading .my-1 { "Customer fields" }
                        @if offer.variables.is_empty() {
                            p .text-muted .text-sm { "No customer fields" }
                        } @else {
                            ul .list-compact {
                                @for variable in &offer.variables {
                                    li { (variable.label) " (" (commerce_wire(&variable.kind)) ")" @if variable.required { " — required" } }
                                }
                            }
                        }
                    }
                    div {
                        h3 .products-subheading .my-1 { "Itemized price rows" }
                        ul .list-compact {
                            @for component in &offer.components {
                                li { strong { (component.label) } ": " (amount_rule_summary(&component.amount, &offer.currency)) }
                            }
                        }
                    }
                    div {
                        h3 .products-subheading .my-1 { "Checkout" }
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
                    details .mt-4 {
                        summary .summary-strong { "Advanced draft definition" }
                        p .text-muted .text-sm { "Edit the complete typed offer JSON. Published offers are immutable; duplicate one to create an editable draft." }
                        textarea .form-textarea aria-label="Offer definition (JSON)" data-offer-definition rows="18" spellcheck="false" { (definition) }
                        button .btn .btn--secondary .btn--sm type="button" .mt-3 data-action="pm-save-offer" { "Save draft definition" }
                    }
                }
                @if managed.status == OfferStatus::Active {
                    section .details-block--divider {
                        h3 .products-subheading .m-0 { "Shareable Stripe Payment Links" }
                        p .text-muted .text-sm { "Create a hosted checkout link you can paste into an email, button, or social post. Products with choices save those choices as a reusable preset." }
                        @if !offer.variables.is_empty() {
                            div .grid .grid-auto-260 .gap-4 {
                                div .form-group {
                                    label .form-label for=(format!("preset-{}-name", offer.id)) { "Preset name" }
                                    input .form-input id=(format!("preset-{}-name", offer.id)) data-preset-name type="text" value=(format!("{} share link", offer.name));
                                }
                                div .form-group {
                                    label .form-label for=(format!("preset-{}-slug", offer.id)) { "Preset slug (optional)" }
                                    input .form-input id=(format!("preset-{}-slug", offer.id)) data-preset-slug type="text" pattern="[a-z0-9]+(?:-[a-z0-9]+)*" placeholder="team-five";
                                }
                                @for variable in &offer.variables { (render_offer_variable_input(variable, &offer.id, "preset")) }
                            }
                            details .details-block--spaced {
                                summary { "Advanced preset JSON" }
                                textarea .form-textarea aria-label="Preset values (JSON)" data-preset-values rows="5" spellcheck="false" { (preset_defaults) }
                            }
                        }
                        div .form-group .form-group--narrow {
                            label .form-label for=(format!("link-{}-completion-url", offer.id)) { "After-completion URL (optional)" }
                            input .form-input id=(format!("link-{}-completion-url", offer.id)) data-link-completion-url type="url" placeholder="https://example.com/thank-you";
                        }
                        div .flex .gap-2 .flex-wrap {
                            button .btn .btn--primary .btn--sm type="button" data-create-link data-action="pm-create-link" { "Create or reuse Payment Link" }
                            @if !offer.variables.is_empty() {
                                button .btn .btn--secondary .btn--sm type="button" data-action="pm-new-preset" { "New preset" }
                            }
                        }
                        @if !offer.variables.is_empty() {
                            h4 .products-subheading .mb-1 { "Saved presets" }
                            div data-checkout-presets aria-live="polite" { p .text-muted .text-sm { "Loading presets…" } }
                        }
                        div data-payment-links .mt-4 { p .text-muted .text-sm { "Loading Payment Links…" } }
                    }
                    details .details-block--divider {
                        summary .summary-strong { "Hosted, embedded, and static-site integration" }
                        p .text-muted .text-sm { "The browser sends inputs to Impresspress for authoritative pricing. Replace the placeholder domain with this Impresspress deployment; secret Stripe keys never belong in static HTML." }
                        div .form-group {
                            label .form-label for=(format!("snippet-{}-hosted", offer.id)) { "Hosted Checkout widget" }
                            textarea .form-textarea id=(format!("snippet-{}-hosted", offer.id)) data-integration-snippet readonly rows="4" spellcheck="false" { (hosted_snippet) }
                            button .btn .btn--secondary .btn--sm type="button" .mt-2 data-action="pm-copy-field" { "Copy hosted snippet" }
                        }
                        div .form-group {
                            label .form-label for=(format!("snippet-{}-embedded", offer.id)) { "Embedded Checkout widget" }
                            textarea .form-textarea id=(format!("snippet-{}-embedded", offer.id)) data-integration-snippet readonly rows="4" spellcheck="false" { (embedded_snippet) }
                            button .btn .btn--secondary .btn--sm type="button" .mt-2 data-action="pm-copy-field" { "Copy embedded snippet" }
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
    // `repo::products::get` already answers `NotFound` for a soft-deleted row,
    // so the hand-written `deleted_at` check this used to need is gone too.
    let product = match repo::products::get(ctx, product_id).await {
        Ok(product) => product,
        Err(error) => {
            return record_error_page(
                ctx,
                msg,
                if admin {
                    Sections::Admin(AdminSection::Products)
                } else {
                    Sections::Portal(PortalSection::SellerProducts, true)
                },
                "Product",
                error,
                "products page: product read failed",
            )
            .await
        }
    };
    if !admin && !super::handlers::is_owned_by(&product, msg.user_id()) {
        // The shared rule again — this page and the API that backs its
        // buttons must not disagree about who owns the product.
        return ui::not_found_response(msg);
    }
    let offers = match repo::offers::list_for_product(ctx, product_id).await {
        Ok(offers) => offers,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                if admin {
                    Sections::Admin(AdminSection::Products)
                } else {
                    Sections::Portal(PortalSection::SellerProducts, true)
                },
                "Product",
                error,
                "Could not load product pricing",
            )
            .await
        }
    };
    let seller_enabled = !admin
        && match super::handlers::user_products_enabled(ctx).await {
            Ok(value) => value,
            Err(e) => {
                return error_page(
                    ctx,
                    msg,
                    if admin {
                        Sections::Admin(AdminSection::Products)
                    } else {
                        Sections::Portal(PortalSection::SellerProducts, true)
                    },
                    "Product",
                    e,
                    "products page: seller switch read failed",
                )
                .await
            }
        };
    let automatic_tax = match super::stripe::automatic_tax_enabled(ctx).await {
        Ok(enabled) => enabled,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                if admin {
                    Sections::Admin(AdminSection::Products)
                } else {
                    Sections::Portal(PortalSection::SellerProducts, true)
                },
                "Product",
                e,
                "products page: automatic tax read failed",
            )
            .await
        }
    };
    // The wizard's currency policy, applied to the offer editor too: a
    // seller picks from the allowed list, an administrator from any.
    let mut seller_currencies = if admin {
        Vec::new()
    } else {
        match super::handlers::seller_policy::allowed_currencies(ctx).await {
            Ok(currencies) => currencies.into_iter().collect::<Vec<_>>(),
            Err(e) => {
                return error_page(
                    ctx,
                    msg,
                    Sections::Portal(PortalSection::SellerProducts, true),
                    "Product",
                    e,
                    "products page: seller policy read failed",
                )
                .await
            }
        }
    };
    seller_currencies.sort();
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
    let page_config = ui::script_json(&serde_json::json!({
        "product_url": product_api_url,
        "detail_base_url": detail_base_url,
        "new_offer": new_offer_definition(
            product.str_field("name"),
            product.str_field("currency"),
            &seller_currencies,
            automatic_tax,
        ),
    }));
    let status = product.str_field("status");
    let approval = product.str_field("approval_status");
    // As above: the stored spelling compared against the variant's own. The
    // badge below still renders the raw column, so a value outside the
    // contract shows up on the page instead of replacing it with a 500.
    let pending_review = status == commerce_wire(&ProductStatus::PendingReview);
    let publishable = status != commerce_wire(&ProductStatus::Active) && !pending_review;
    let archived = status == commerce_wire(&ProductStatus::Archived);
    let live = status == commerce_wire(&ProductStatus::Active)
        && approval == commerce_wire(&ApprovalStatus::Approved);
    let awaiting_moderation = pending_review && approval == commerce_wire(&ApprovalStatus::Pending);
    // The product's lifecycle actions are the page's: they go in the
    // topbar, the primary one (publish, or approve a listing under review)
    // last. The details card below keeps one primary of its own, Save.
    let owned_by_seller = product.str_field("owner_kind") == "user";
    let mut actions = vec![html! {
        button .btn .btn--secondary .btn--sm type="button" data-action="pm-duplicate" { "Duplicate product" }
    }];
    if !archived {
        actions.push(html! {
            button .btn .btn--secondary .btn--sm type="button" data-action="pm-set-status" data-product-status="archived" { "Archive product" }
        });
    }
    if live {
        actions.push(html! {
            a .btn .btn--secondary .btn--sm href=(format!("/b/products/catalog/{product_id}")) target="_blank" rel="noopener" { "View storefront" }
        });
    }
    let moderating = admin && owned_by_seller && awaiting_moderation;
    if moderating {
        actions.push(html! {
            button .btn .btn--secondary .btn--sm type="button" data-moderation-action="reject" data-action="pm-moderate" { "Return to seller" }
        });
        actions.push(html! {
            button .btn .btn--primary .btn--sm type="button" data-moderation-action="approve" data-action="pm-moderate" { "Approve listing" }
        });
    } else if publishable {
        actions.push(html! {
            button .btn .btn--primary .btn--sm type="button" data-action="pm-set-status" data-product-status="active" { @if admin { "Publish product" } @else { "Submit for publication" } }
        });
    }
    let content = html! {
        p #product-manager-error .login-error role="alert" aria-live="assertive" hidden {}
        section .card {
            header .card__head {
                div {
                    div .products-status-stack {
                        h2 .card__title { "Product details" }
                        (components::status_badge(status))
                        @if owned_by_seller { (components::badge(components::BadgeVariant::Secondary, &format!("Review: {approval}"))) }
                    }
                    @if !admin && pending_review {
                        p .text-muted .text-sm .text-subtitle { "This product is awaiting administrator review and is not public yet." }
                    }
                }
            }
            div .card__body {
        form #product-manager-form {
                    div .form-group { label .form-label .required for="manager-product-name" { "Product name" } input #manager-product-name .form-input type="text" maxlength="160" required value=(product.str_field("name")); }
                    div .form-group { label .form-label for="manager-product-description" { "Customer-facing description" } textarea #manager-product-description .form-textarea maxlength="4000" { (product.str_field("description")) } }
                    details .products-advanced {
                        summary { "More product details (optional)" }
                        div .products-advanced__body {
                            div .products-form-grid {
                                // No `pattern`: a stored slug may break the grammar, and a
                                // `pattern` would block every save of such a product, even one
                                // that leaves the slug alone. The server refuses a changed slug
                                // outside the grammar; `maxlength` binds only what is typed.
                                div .form-group { label .form-label for="manager-product-slug" { "Web address" } input #manager-product-slug .form-input type="text" maxlength=(PRODUCT_SLUG_MAX_LEN) value=(product.str_field("slug")); }
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
                    div .products-form-actions {
                        button .btn .btn--primary .btn--sm type="submit" { "Save product details" }
                    }
                }
            }
        }
        section #product-manager-visual-editor .card hidden .mt-6 {
            header .card__head {
                div {
                    h2 #manager-visual-title .card__title { "Edit pricing draft" }
                    p .text-muted .text-sm .text-subtitle { "Manage customer inputs, itemized price rows, conditions, and recurring terms without editing JSON." }
                }
                button .btn .btn--secondary .btn--sm type="button" data-action="pm-close-visual-editor" { "Close editor" }
            }
            div .card__body {
                div .grid .grid-auto-180 .gap-4 {
                    div .form-group {
                        label .form-label .required for="manager-visual-offer-name" { "Offer name" }
                        input #manager-visual-offer-name .form-input type="text" maxlength="160" required;
                    }
                    div .form-group {
                        label .form-label for="manager-visual-mode" { "Charge type" }
                        select #manager-visual-mode .form-select data-action="pm-visual-mode-changed" { option value="payment" { "One-time payment" } option value="subscription" { "Subscription" } }
                    }
                    div .form-group {
                        label .form-label .required for="manager-visual-currency" { "Currency" }
                        input #manager-visual-currency .form-input type="text" maxlength="3" list="manager-visual-currency-options" required;
                        @if !seller_currencies.is_empty() {
                            datalist #manager-visual-currency-options { @for currency in &seller_currencies { option value=(currency) {} } }
                            p .text-muted .text-sm { "Allowed seller currencies: " (seller_currencies.join(", ")) }
                        }
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
                section .mt-4 {
                    div .flex .items-center .justify-between .gap-4 .flex-wrap {
                        div { h3 .products-subheading .m-0 { "Customer fields" } p .text-muted .text-sm { "Typed quantities, choices, flags, and text used by price rows." } }
                        button .btn .btn--secondary .btn--sm type="button" data-action="pw-add-variable" { (icons::plus()) " Add input" }
                    }
                    div #wizard-variables {}
                }
                section .products-section {
                    div .flex .items-center .justify-between .gap-4 .flex-wrap {
                        div { h3 .products-subheading .m-0 { "Itemized price rows" } p .text-muted .text-sm { "Fixed, per-unit, lookup, tiered, package, and conditional rows are supported." } }
                        button .btn .btn--secondary .btn--sm type="button" data-action="pw-add-component" { (icons::plus()) " Add row" }
                    }
                    div #wizard-components {}
                }
                p .text-muted .text-sm { "Checkout collection, shipping, tax, and fulfillment settings remain unchanged. Advanced nested conditions and quantity rules are preserved when saved." }
                // The editor's own outcome, next to the button that caused
                // it, in both modes: what to fix, or why the server refused.
                p #manager-visual-error .login-error .mt-4 role="alert" hidden {}
                div .flex .gap-2 .mt-4 .flex-wrap {
                    button #manager-visual-save .btn .btn--primary .btn--sm .btn--block type="button" data-action="pm-save-visual-offer" { "Save visual changes" }
                    button .btn .btn--secondary .btn--sm .btn--block type="button" data-action="pm-close-visual-editor" { "Cancel" }
                }
            }
        }
        section #product-pricing .products-section {
            // The empty state's button is the section's one primary; once
            // the product has a price, adding another is secondary to each
            // draft's Publish.
            (components::section_header("Prices and checkout", (!offers.is_empty()).then(|| add_price_button("btn--secondary"))))
            p .section-desc { "Published offers are immutable so existing orders and links retain their exact terms." }
            @if offers.is_empty() {
                (components::empty_state(icons::dollar_sign(), "No prices yet", "Customers cannot buy this product until it has a published price.", Some(add_price_button("btn--primary"))))
            } @else {
                @for offer in &offers { (render_managed_offer(offer, &product_api_url)) }
            }
        }
        script { (maud::PreEscaped(format!("window.__productManagerConfig={page_config};"))) }
        script src=(assets::wizard_js_url()) {}
        script src=(assets::manager_js_url()) {}
    };
    let sections = if admin {
        Sections::Admin(AdminSection::Products)
    } else {
        Sections::Portal(PortalSection::SellerProducts, seller_enabled)
    };
    let name = product.str_field("name");
    let shell = sections
        .shell(name)
        .trail(vec![
            ui::shell::Crumb {
                label: if admin { "All products" } else { "My products" },
                href: Some(back_href),
            },
            ui::shell::Crumb {
                label: name,
                href: None,
            },
        ])
        .subtitle("Update what customers see, manage pricing, and share checkout")
        .actions(actions);
    products_page(ctx, msg, shell, sections, content).await
}

/// The control that opens the offer editor in create mode. `variant` is the
/// button's emphasis: primary in the empty state, where adding a price is
/// the only thing the section can do, secondary beside existing offers.
fn add_price_button(variant: &str) -> Markup {
    html! {
        button class=(format!("btn {variant} btn--sm")) type="button" data-action="pm-add-price" { (icons::plus()) " Add a price" }
    }
}

/// The definition "Add a price" opens the offer editor with: a one-time,
/// fixed-price offer named after the product, in the product's currency,
/// with the deployment's automatic-tax setting — what the product wizard
/// sends for its simplest template. It has no price row; the editor adds the
/// one row it asks the seller to price. A stored currency outside the ISO
/// grammar seeds an empty field rather than a guess, so the editor asks for
/// one. `allowed_currencies` is the seller policy's sorted list (empty for an
/// administrator, or when the policy allows any): a product currency the
/// policy no longer allows seeds its first entry instead, as the wizard does
/// with the default currency, so the seed is one the create endpoint takes.
fn new_offer_definition(
    product_name: &str,
    product_currency: &str,
    allowed_currencies: &[String],
    automatic_tax: bool,
) -> OfferDefinitionRequest {
    let mut currency = money::normalize_currency(product_currency).unwrap_or_default();
    if let Some(first) = allowed_currencies.first() {
        if !allowed_currencies.contains(&currency) {
            currency = first.clone();
        }
    }
    OfferDefinitionRequest {
        name: product_name.to_string(),
        mode: OfferMode::Payment,
        currency,
        pricing_model: PricingModel::Fixed,
        recurring_interval: None,
        interval_count: 1,
        usage_type: UsageType::Licensed,
        billing_scheme: BillingScheme::PerUnit,
        tax_behavior: TaxBehavior::Unspecified,
        variables: Vec::new(),
        components: Vec::new(),
        checkout: CheckoutPolicy {
            automatic_tax,
            ..CheckoutPolicy::default()
        },
    }
}

// ---------------------------------------------------------------------------
// Admin: Groups
// ---------------------------------------------------------------------------

pub async fn groups(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let result = repo::groups::list_by_name(ctx, vec![], 100).await;
    let list = match result {
        Ok(list) => list,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Groups),
                "Groups",
                e,
                "products groups page: group read failed",
            )
            .await
        }
    };

    let content = html! {
        p #catalog-admin-error .login-error role="alert" aria-live="assertive" hidden {}
        section #group-editor .card hidden .mb-4 {
            header .card__head {
                div { h2 #group-editor-title .card__title { "New group" } p .text-muted .text-sm .text-subtitle { "Give the group a clear name customers will recognize." } }
            }
            div .card__body {
                form data-action="pc-save-group" {
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
                        button .btn .btn--secondary .btn--sm type="button" data-action="pc-close" { "Cancel" }
                    }
                }
            }
        }

        div #groups-content {
            @let cols = [
                components::TableCol::new("Name").primary(),
                components::TableCol::new("Description").optional(),
                components::TableCol::new("Status"),
                components::TableCol::new("Created"),
                components::TableCol::new("Actions").actions(),
            ];
            @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| {
                let name = r.str_field("name");
                let description = r.str_field("description");
                vec![
                    html! { (name) },
                    html! { span .text-muted .text-sm { (if description.is_empty() { components::NO_VALUE } else { description }) } },
                    components::status_badge(r.str_field("status")),
                    components::timestamp(r.str_field("created_at")),
                    html! { div .products-actions {
                        button .btn .btn--ghost .btn--sm .btn--icon type="button"
                            aria-label={ "Edit " (name) } title="Edit"
                            data-record-id=(r.id) data-record-name=(name) data-record-description=(description) data-record-status=(r.str_field("status"))
                            data-action="pc-edit-group"
                        { (icons::edit()) }
                        // Destructive, so set apart: the danger colour, and
                        // the script asks before it deletes.
                        button .btn .btn--ghost-danger .btn--sm .btn--icon type="button"
                            aria-label={ "Delete " (name) } title="Delete"
                            data-record-id=(r.id) data-record-name=(name)
                            data-action="pc-delete"
                        { (icons::trash()) }
                    } },
                ]
            }).collect();
            (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! {
                (components::empty_state(icons::folder(), "No groups yet", "Groups are optional. Add one when you want to organize related products.", Some(html! {
                    button .btn .btn--secondary .btn--sm type="button" data-action="pc-new" { (icons::plus()) " Create group" }
                })))
            }))
        }
        script src=(assets::catalog_admin_js_url()) {}
    };

    products_page(
ctx,
        msg,
        ui::Shell::admin("Groups", "Groups")
            .subtitle("Keep related products together so your catalog is easier to browse")
            .actions(vec![html! {
                button .btn .btn--primary .btn--sm type="button" data-action="pc-new" { (icons::plus()) " New group" }
            }]),
        Sections::Admin(AdminSection::Groups),
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
    let list = match result {
        Ok(list) => list,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Orders),
                "Orders",
                e,
                "products purchases page: order read failed",
            )
            .await
        }
    };

    let base_href = order_list_href("/b/products/admin/purchases", &status_filter);
    let content = html! {
        div .filter-bar {
            (order_status_chips("/b/products/admin/purchases", &status_filter))
        }
        @let row_hrefs: Vec<String> = list.records.iter().map(|record| format!("/b/products/admin/purchases/{}", crate::util::url_path_encode(&record.id))).collect();
        @let cols = [
            components::TableCol::new("Order").primary(),
            components::TableCol::new("Customer"),
            components::TableCol::new("Status"),
            components::TableCol::new("Total"),
            components::TableCol::new("Placed"),
        ];
        @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| {
            let amount = display_money(r.i64_field("total_cents"), r.str_field("currency"));
            let buyer = if !r.str_field("buyer_email").is_empty() { r.str_field("buyer_email") } else if !r.str_field("buyer_user_id").is_empty() { r.str_field("buyer_user_id") } else { r.str_field("user_id") };
            vec![
                html! { code .text-sm { (r.id.get(..8).unwrap_or(&r.id)) } },
                html! { span .text-sm { @if buyer.is_empty() { "Guest" } @else { (components::breakable_id(buyer)) } } },
                components::status_badge(r.str_field("status")),
                html! { span .font-medium { (amount) } },
                components::timestamp(r.str_field("created_at")),
            ]
        }).collect();
        (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), order_list_empty(&status_filter, "Customer orders will appear here after checkout starts.")))
        @if let Some(per_page) = std::num::NonZeroU32::new(page_size as u32) { (components::pagination(list.page as u32, per_page, list.total_count as u32, &base_href)) }
    };

    products_page(
        ctx,
        msg,
        ui::Shell::admin("Orders", "Orders")
            .subtitle("Track payments, refunds, and customer orders"),
        Sections::Admin(AdminSection::Orders),
        content,
    )
    .await
}

/// The order statuses an order list can be narrowed to, with their labels.
const ORDER_STATUSES: [(&str, &str); 6] = [
    ("all", "All"),
    ("pending", "Pending"),
    ("completed", "Completed"),
    ("partially_refunded", "Part-refunded"),
    ("refunded", "Refunded"),
    ("failed", "Failed"),
];

/// An order list at `base` narrowed to `status` ("" or "all" is every
/// order).
fn order_list_href(base: &str, status: &str) -> String {
    if status.is_empty() || status == "all" {
        base.to_string()
    } else {
        format!("{base}?status={}", crate::util::urlencode(status))
    }
}

/// The status filter of an order list at `base`, as filter links.
fn order_status_chips(base: &str, current: &str) -> Markup {
    let hrefs: Vec<String> = ORDER_STATUSES
        .iter()
        .map(|(value, _)| order_list_href(base, value))
        .collect();
    components::filter_links(
        "Order status",
        ORDER_STATUSES
            .iter()
            .zip(&hrefs)
            .map(|((value, label), href)| components::Tab {
                active: (current.is_empty() && *value == "all") || current == *value,
                href,
                label,
                icon: None,
            })
            .collect(),
    )
}

/// An empty order list: under a status filter it says the filter matched
/// nothing; unfiltered, `none_yet` says where orders come from.
fn order_list_empty(status: &str, none_yet: &str) -> Markup {
    if status.is_empty() || status == "all" {
        components::empty_state(icons::shopping_cart(), "No orders yet", none_yet, None)
    } else {
        components::empty_state(
            icons::shopping_cart(),
            "No orders with this status",
            "Choose another status, or All.",
            None,
        )
    }
}

// ---------------------------------------------------------------------------
// Admin: Stripe setup and connection health
// ---------------------------------------------------------------------------

/// One go-live checklist row: the item and what it is for, its state at the
/// end of the row — so every title starts at the same edge whatever the
/// state's width, and the state is a word as well as a colour.
fn setup_check(label: &str, complete: bool, detail: &str) -> Markup {
    html! {
        li .products-checklist__item {
            div {
                strong { (label) }
                p .text-muted .text-sm .text-subtitle { (detail) }
            }
            @if complete {
                (components::badge(components::BadgeVariant::Success, "Ready"))
            } @else {
                (components::badge(components::BadgeVariant::Warning, "Action needed"))
            }
        }
    }
}

/// A capability's state as a badge: a word in a pill sized to its text, not
/// a headline stat ("Unavailable" in a stat tile overflowed it on a phone).
fn capability_badge(enabled: bool) -> Markup {
    if enabled {
        components::badge(components::BadgeVariant::Success, "Enabled")
    } else {
        components::badge(components::BadgeVariant::Warning, "Unavailable")
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
                    h2 .card__title { "Connection" }
                    p .text-muted .text-sm .text-subtitle { (summary) }
                }
                span #stripe-state .badge .(badge_class) { (state_label) }
            }
            div .card__body {
                @if !status.error.is_empty() {
                    p #stripe-error .text-sm .text-danger .mt-0 {
                        (status.error)
                    }
                } @else {
                    p #stripe-error .text-sm .text-muted .mt-0 {}
                }
                dl .products-status-list {
                    div { dt { "Payments" } dd { (capability_badge(status.charges_enabled)) } }
                    div { dt { "Payouts" } dd { (capability_badge(status.payouts_enabled)) } }
                    div { dt { "Default currency" } dd { (if status.default_currency.is_empty() { components::NO_VALUE } else { &status.default_currency }) } }
                }
                details .products-plain-details {
                    summary { "Technical connection details" }
                    div .text-sm {
                        p { strong { "Stripe account: " } @if status.account_id.is_empty() { "Not connected" } @else { code { (&status.account_id) } } }
                        p { strong { "Country: " } (if status.country.is_empty() { "—" } else { &status.country }) }
                        p { strong { "API version: " } code { (&status.api_version) } }
                    }
                }
                div .products-form-actions {
                    button #stripe-test-button .btn .btn--secondary .btn--sm type="button" data-action="ps-test-connection" {
                        "Test connection"
                    }
                }
            }
        }
    }
}

pub async fn stripe_setup(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let status = match stripe_provider::connection_status(ctx).await {
        Ok(status) => status,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Stripe),
                "Stripe setup",
                e,
                "stripe setup page: settings read failed",
            )
            .await
        }
    };
    let connected = matches!(
        status.state,
        StripeConnectionState::ConnectedTest | StripeConnectionState::ConnectedLive
    );
    let content = html! {
        @if status.state == StripeConnectionState::ConnectedTest {
            (components::callout(
                components::CalloutTone::Warning,
                "Test mode is active",
                html! { p { "Checkout is safe to exercise, but no real funds will move. Replace both keys with matching live-mode keys only after the checklist below is complete." } },
                None,
            ))
        }
        (stripe_connection_card(&status))
        div .grid .grid-auto-320 .gap-4 .mt-4 {
            section .card {
                header .card__head { h2 .card__title { "Go-live checklist" } }
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
                header .card__head { h2 .card__title { "Webhook destination" } }
                div .card__body {
                    p .text-muted .text-sm { "Register this HTTPS route as a Stripe webhook destination:" }
                    code .products-code-block { "/b/products/webhooks" }
                    details .products-plain-details {
                        summary { "Show required Stripe event types" }
                        // The set `handle_webhook` dispatches on, not a
                        // second copy of it: a type the block starts
                        // handling is advertised here without anyone
                        // remembering to add it, and one it stops handling
                        // stops being advertised.
                        ul .text-sm {
                            @for event_type in StripeEventType::ALL {
                                li { code { (util::wire_str(event_type)) } }
                            }
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
            section #stripe-webhook-operations .card .card--flat {
            header .card__head .card__head--end {
                div {
                    h2 .card__title { "Webhook delivery health" }
                    p .text-muted .text-sm .text-subtitle {
                        "Review failed Stripe notifications and replay one after the underlying problem is fixed."
                    }
                }
                div .flex .gap-2 .items-end .flex-wrap {
                    label .text-sm for="stripe-webhook-filter" {
                        "Status"
                        select #stripe-webhook-filter data-action="ps-load-webhooks" .d-block .mt-1 {
                            option value="dead_letter" selected { "Needs manual review" }
                            option value="failed" { "Waiting to retry" }
                            option value="processing" { "Processing" }
                            option value="processed" { "Processed" }
                            option value="" { "All events" }
                        }
                    }
                    button .btn .btn--secondary .btn--sm type="button" data-action="ps-load-webhooks" { "Refresh" }
                }
            }
            div .card__body {
                p #stripe-webhook-summary .text-muted .text-sm aria-live="polite" .mt-0 {}
                p #stripe-webhook-error .login-error role="alert" aria-live="assertive" hidden {}
                div #stripe-webhook-events aria-live="polite" { "Loading webhook events…" }
                noscript { p .text-muted .text-sm { "JavaScript is required to inspect and replay webhook deliveries." } }
            }
        }
        }
        details .products-advanced {
            summary { "Advanced: Stripe recovery tools" }
            section #stripe-provider-operations .card .card--flat {
            header .card__head .card__head--end {
                div {
                    h2 .card__title { "Provider reconciliation" }
                    p .text-muted .text-sm .text-subtitle {
                        "Retry incomplete Stripe updates and review any operation that could not recover automatically."
                    }
                }
                div .flex .gap-2 .items-end .flex-wrap {
                    label .text-sm for="stripe-provider-filter" {
                        "Status"
                        select #stripe-provider-filter data-action="ps-load-provider-ops" .d-block .mt-1 {
                            option value="dead_letter" selected { "Needs manual review" }
                            option value="failed" { "Waiting to retry" }
                            option value="pending" { "Pending" }
                            option value="processing" { "Processing" }
                            option value="succeeded" { "Succeeded" }
                            option value="" { "All operations" }
                        }
                    }
                    button #stripe-provider-reconcile .btn .btn--primary .btn--sm type="button" data-action="ps-reconcile" { "Reconcile due operations" }
                    button .btn .btn--secondary .btn--sm type="button" data-action="ps-load-provider-ops" { "Refresh" }
                }
            }
            div .card__body {
                p #stripe-provider-summary .text-muted .text-sm aria-live="polite" .mt-0 {}
                p #stripe-provider-reconcile-result .text-sm role="status" aria-live="polite" {}
                p #stripe-provider-error .login-error role="alert" aria-live="assertive" hidden {}
                div #stripe-provider-operations-list aria-live="polite" { "Loading provider operations…" }
                noscript { p .text-muted .text-sm { "JavaScript is required to inspect and reconcile provider operations." } }
            }
        }
        }
        script src=(assets::stripe_setup_js_url()) {}
    };
    // One way to the Stripe settings: the primary action while Stripe is
    // not configured, a secondary one once it is.
    let configure_class = if status.state == StripeConnectionState::NotConfigured {
        "btn--primary"
    } else {
        "btn--secondary"
    };
    products_page(
ctx,
        msg,
        ui::Shell::admin("Stripe setup", "Stripe setup")
            .subtitle("Connect Stripe, confirm payment readiness, and review anything that needs attention")
            .actions(vec![html! {
                a class={ "btn btn--sm " (configure_class) } href="/b/products/admin/settings" { "Configure Stripe" }
            }]),
        Sections::Admin(AdminSection::Stripe),
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

/// The platform application fee the seller's own pages show: the fee new
/// Checkout Sessions and Payment Links carry, read from the one place they
/// read it.
///
/// Rendered, so it must not be invented: a fee the platform cannot read
/// would otherwise print as "0.00%" on the page an operator opens to check
/// exactly that number. The admin seller detail page reads the setting
/// itself, because it must render without it.
async fn platform_fee(ctx: &dyn Context) -> Result<u16, OutputStream> {
    super::config::seller_fee_bps(ctx).await.map_err(|error| {
        crate::http::err_internal("Platform application fee is misconfigured", error)
    })
}

fn friendly_requirement(requirement: &str) -> String {
    requirement
        .replace('.', " › ")
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn seller_status_card(account: Option<&SellerAccount>, fee_basis_points: u16) -> Markup {
    let ready = account.is_some_and(|account| {
        account.capabilities.details_submitted
            && account.capabilities.charges_enabled
            && account.capabilities.payouts_enabled
            && account.status != SellerStatus::Suspended
    });
    let suspended = account.is_some_and(|account| account.status == SellerStatus::Suspended);
    let has_account = account.is_some_and(|account| !account.stripe_account_id.is_empty());
    html! {
        section .card {
            header .card__head {
                div {
                    h2 .card__title { "Stripe seller account" }
                    p .text-muted .text-sm .text-subtitle {
                        "Stripe hosts identity verification, payouts, and the Express dashboard."
                    }
                }
                @if suspended {
                    (components::badge(components::BadgeVariant::Danger, "Suspended"))
                } @else if ready {
                    (components::badge(components::BadgeVariant::Success, "Ready to sell"))
                } @else if has_account {
                    (components::badge(components::BadgeVariant::Warning, "Setup incomplete"))
                } @else {
                    (components::badge(components::BadgeVariant::Warning, "Not connected"))
                }
            }
            div .card__body {
                dl .products-status-list {
                    div { dt { "Charges" } dd { (capability_badge(account.is_some_and(|a| a.capabilities.charges_enabled))) } }
                    div { dt { "Payouts" } dd { (capability_badge(account.is_some_and(|a| a.capabilities.payouts_enabled))) } }
                    div { dt { "Platform fee" } dd { (fee_percent(fee_basis_points.into())) } }
                    div { dt { "Mode" } dd { @if account.is_some_and(|a| a.livemode) { "Live" } @else { "Test" } } }
                }
                @if let Some(account) = account {
                    @if !account.capabilities.requirements_due.is_empty() {
                        div .mt-4 {
                            strong { "Information Stripe still needs" }
                            ul .text-sm {
                                @for requirement in &account.capabilities.requirements_due {
                                    li { (friendly_requirement(requirement)) }
                                }
                            }
                        }
                    }
                    @if !account.disabled_reason.is_empty() {
                        p .text-sm .text-danger { "Stripe restriction: " (friendly_requirement(&account.disabled_reason)) }
                    }
                    @if !account.sync_error.is_empty() {
                        p .text-muted .text-sm { "Last refresh: " (account.sync_error) }
                    }
                }
                div .flex .gap-3 .flex-wrap .mt-5 {
                    @if !suspended && !ready {
                        button .btn .btn--primary .btn--md type="button" data-action="pp-seller-onboarding" {
                            @if has_account { "Continue Stripe setup" } @else { "Connect Stripe to sell" }
                        }
                    }
                    @if !suspended && has_account {
                        button .btn .btn--secondary .btn--md type="button" data-action="pp-seller-dashboard" {
                            "Open Stripe dashboard"
                        }
                    }
                    a .btn .btn--secondary .btn--md href="/b/products/my-products" { "Manage products" }
                }
            }
        }
    }
}

pub async fn portal_home(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let seller_enabled = match super::handlers::user_products_enabled(ctx).await {
        Ok(value) => value,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::Home, false),
                "Commerce",
                e,
                "products page: seller switch read failed",
            )
            .await
        }
    };
    let purchases_count = match repo::purchases::count_for_user(ctx, &user_id).await {
        Ok(count) => count,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::Home, false),
                "Commerce",
                error,
                "Database error",
            )
            .await
        }
    };

    let (product_count, seller_account, fee_basis_points) = if seller_enabled {
        let fee = match platform_fee(ctx).await {
            Ok(fee) => fee,
            Err(response) => return response,
        };
        // The soft-delete filter used to be hand-written above;
        // `repo::products::count` now appends it.
        let count = match repo::products::count(
            ctx,
            &[Filter {
                field: "created_by".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(&user_id),
            }],
        )
        .await
        {
            Ok(count) => count,
            Err(error) => {
                return error_page(
                    ctx,
                    msg,
                    Sections::Portal(PortalSection::Home, false),
                    "Commerce",
                    error,
                    "Database error",
                )
                .await
            }
        };
        let account = match repo::seller_accounts::get_for_user(ctx, &user_id).await {
            Ok(Some(record)) => match repo::seller_accounts::to_contract(&record, fee) {
                Ok(account) => Some(account),
                Err(error) => return crate::http::err_internal("Seller account error", error),
            },
            Ok(None) => None,
            Err(error) => {
                return error_page(
                    ctx,
                    msg,
                    Sections::Portal(PortalSection::Home, false),
                    "Commerce",
                    error,
                    "Database error",
                )
                .await
            }
        };
        (count, account, fee)
    } else {
        (0, None, 0)
    };

    let content = html! {
        div #commerce-portal-error .text-sm hidden .text-danger .mb-4 {}
        (components::callout(
            components::CalloutTone::Info,
            "One commerce workspace",
            html! { p { "Purchases stay separate from products you sell, so it is always clear whether you are buying or managing a storefront." } },
            Some(html! { a .btn .btn--secondary .btn--sm href="/b/products/my-purchases" { "View order history" } }),
        ))
        div .stats-grid {
            (components::stat_card("Purchases", &purchases_count.to_string(), icons::shopping_cart(), None))
            @if seller_enabled {
                (components::stat_card("Products for sale", &product_count.to_string(), icons::package(), None))
            }
        }
        section .card .mt-4 {
            header .card__head {
                div {
                    h2 .card__title { "Purchases and subscriptions" }
                    p .text-muted .text-sm .text-subtitle {
                        "Review orders here. Stripe's secure Billing Portal handles saved payment methods, invoices, and subscription changes."
                    }
                }
            }
            div .card__body .flex .gap-3 .flex-wrap {
                a .btn .btn--primary .btn--md href="/b/products/my-purchases" { "View purchases" }
                button .btn .btn--secondary .btn--md type="button" data-action="pp-buyer-billing" { "Manage billing" }
            }
        }
        @if seller_enabled {
            div .mt-4 {
                (seller_status_card(seller_account.as_ref(), fee_basis_points))
            }
        }
        script src=(assets::commerce_portal_js_url()) {}
    };
    products_page(
        ctx,
        msg,
        ui::Shell::portal("Commerce", "Commerce")
            .subtitle("Review what you bought, manage billing, or start selling"),
        Sections::Portal(PortalSection::Home, seller_enabled),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Seller: dashboard and orders
// ---------------------------------------------------------------------------

pub async fn seller_dashboard(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let fee_basis_points = match platform_fee(ctx).await {
        Ok(fee) => fee,
        Err(response) => return response,
    };
    let account_record = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
        Ok(account) => account,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::SellerDashboard, true),
                "Seller dashboard",
                error,
                "Database error",
            )
            .await
        }
    };
    let account = match account_record.as_ref() {
        Some(record) => match repo::seller_accounts::to_contract(record, fee_basis_points) {
            Ok(account) => Some(account),
            Err(error) => return crate::http::err_internal("Seller account error", error),
        },
        None => None,
    };
    let analytics = match account_record.as_ref() {
        Some(record) => match repo::purchases::commerce_analytics(ctx, Some(&record.id)).await {
            Ok(analytics) => analytics,
            Err(error) => {
                return error_page(
                    ctx,
                    msg,
                    Sections::Portal(PortalSection::SellerDashboard, true),
                    "Seller dashboard",
                    error,
                    "Database error",
                )
                .await
            }
        },
        None => Vec::new(),
    };
    let failures = match account_record.as_ref() {
        Some(record) => match repo::purchases::recent_seller_failures(ctx, &record.id, 5).await {
            Ok(failures) => failures,
            Err(error) => {
                return error_page(
                    ctx,
                    msg,
                    Sections::Portal(PortalSection::SellerDashboard, true),
                    "Seller dashboard",
                    error,
                    "Database error",
                )
                .await
            }
        },
        None => Vec::new(),
    };
    let seller_enabled = match super::handlers::user_products_enabled(ctx).await {
        Ok(value) => value,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::SellerDashboard, true),
                "Seller dashboard",
                e,
                "products page: seller switch read failed",
            )
            .await
        }
    };
    let content = html! {
        div #commerce-portal-error .text-sm hidden .text-danger .mb-4 {}
        (seller_status_card(account.as_ref(), fee_basis_points))
        (analytics_section(&analytics, "Your sales by currency", true))
        (seller_failures_section(&failures))
        script src=(assets::commerce_portal_js_url()) {}
    };
    products_page(
        ctx,
        msg,
        ui::Shell::portal("Seller dashboard", "Seller dashboard")
            .subtitle("Sales, subscriptions, Stripe readiness, and actions"),
        Sections::Portal(PortalSection::SellerDashboard, seller_enabled),
        content,
    )
    .await
}

pub async fn seller_orders(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let seller_enabled = match super::handlers::user_products_enabled(ctx).await {
        Ok(value) => value,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::SellerOrders, true),
                "Seller orders",
                e,
                "products page: seller switch read failed",
            )
            .await
        }
    };
    let account = match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            let content = html! {
                (components::empty_state(icons::link(), "Connect Stripe first", "Complete seller setup before accepting and reviewing seller orders.", Some(html! { a .btn .btn--primary .btn--md href="/b/products/selling" { "Open seller setup" } })))
            };
            return products_page(
                ctx,
                msg,
                ui::Shell::portal("Seller orders", "Seller orders")
                    .subtitle("Orders and subscriptions sold through your Stripe account"),
                Sections::Portal(PortalSection::SellerOrders, seller_enabled),
                content,
            )
            .await;
        }
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::SellerOrders, true),
                "Seller orders",
                error,
                "Database error",
            )
            .await
        }
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
    let list = match result {
        Ok(list) => list,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::SellerOrders, true),
                "Seller orders",
                e,
                "products seller orders page: order read failed",
            )
            .await
        }
    };
    let base_href = order_list_href("/b/products/selling/orders", &status_filter);
    let content = html! {
        div .filter-bar {
            (order_status_chips("/b/products/selling/orders", &status_filter))
        }
        @let row_hrefs: Vec<String> = list.records.iter().map(|record| format!("/b/products/selling/orders/{}", crate::util::url_path_encode(&record.id))).collect();
        @let cols = [
            components::TableCol::new("Buyer").primary(),
            components::TableCol::new("Status"),
            components::TableCol::new("Total"),
            components::TableCol::new("Subscription"),
            components::TableCol::new("Date"),
        ];
        @let rows: Vec<Vec<Markup>> = list.records.iter().map(|order| vec![
            html! { (components::breakable_id(if order.str_field("buyer_email").is_empty() { order.str_field("buyer_user_id") } else { order.str_field("buyer_email") })) },
            components::status_badge(order.str_field("status")),
            html! { span .font-medium { (display_money(order.i64_field("total_cents"), order.str_field("currency"))) } },
            html! { @if order.str_field("stripe_subscription_id").is_empty() { span .text-muted { (components::NO_VALUE) } } @else { (components::status_badge(order.str_field("subscription_status"))) } },
            components::timestamp(order.str_field("created_at")),
        ]).collect();
        (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), order_list_empty(&status_filter, "Orders for your products will appear here after a customer checks out.")))
        @if let Some(per_page) = std::num::NonZeroU32::new(page_size as u32) { (components::pagination(list.page as u32, per_page, list.total_count as u32, &base_href)) }
    };
    products_page(
        ctx,
        msg,
        ui::Shell::portal("Seller orders", "Seller orders")
            .subtitle("Orders, refunds, and subscription health for your products"),
        Sections::Portal(PortalSection::SellerOrders, seller_enabled),
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

async fn order_detail(
    ctx: &dyn Context,
    msg: &Message,
    purchase_id: &str,
    access: OrderPageAccess,
) -> OutputStream {
    // Where an error on this page is drawn, before the seller switch is
    // read: the buyer's sections then show without the seller links.
    let error_sections = match access {
        OrderPageAccess::Admin => Sections::Admin(AdminSection::Orders),
        OrderPageAccess::Buyer => Sections::Portal(PortalSection::Purchases, false),
        OrderPageAccess::Seller => Sections::Portal(PortalSection::SellerOrders, true),
    };
    // A seller without an account has no seller orders at all: that refusal
    // is about the viewer, so it is answered before any order is looked up
    // and says nothing about whether this id exists.
    let seller_account_id = match access {
        OrderPageAccess::Seller => {
            match repo::seller_accounts::get_for_user(ctx, msg.user_id()).await {
                Ok(Some(account)) => Some(account.id),
                Ok(None) => {
                    return ui::forbidden_response(
                        ctx,
                        msg,
                        "Complete seller setup before viewing seller orders",
                        ui::SignedOutAction::SignIn,
                    )
                    .await
                }
                Err(error) => {
                    return error_page(ctx, msg, error_sections, "Order", error, "Database error")
                        .await
                }
            }
        }
        OrderPageAccess::Admin | OrderPageAccess::Buyer => None,
    };
    let purchase = match repo::purchases::get(ctx, purchase_id).await {
        Ok(purchase) => purchase,
        Err(error) => {
            return record_error_page(
                ctx,
                msg,
                error_sections,
                "Order",
                error,
                "order page: purchase read failed",
            )
            .await
        }
    };
    // Someone else's order is answered exactly as a missing one is: a
    // refusal naming it would confirm that the id exists.
    let visible = match access {
        OrderPageAccess::Admin => true,
        OrderPageAccess::Buyer => {
            let owner = if purchase.str_field("buyer_user_id").is_empty() {
                purchase.str_field("user_id")
            } else {
                purchase.str_field("buyer_user_id")
            };
            owner == msg.user_id()
        }
        OrderPageAccess::Seller => {
            seller_account_id.as_deref() == Some(purchase.str_field("seller_account_id"))
        }
    };
    if !visible {
        return ui::not_found_response(msg);
    }
    let line_items = match repo::purchases::list_line_items(ctx, purchase_id).await {
        Ok(items) => items,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                error_sections,
                "Order",
                error,
                "Could not load order items",
            )
            .await
        }
    };
    let refunds = match repo::refunds::list_for_purchase(ctx, purchase_id).await {
        Ok(refunds) => refunds,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                error_sections,
                "Order",
                error,
                "Could not load refunds",
            )
            .await
        }
    };
    let disputes = match repo::disputes::list_for_purchase(ctx, purchase_id).await {
        Ok(disputes) => disputes,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                error_sections,
                "Order",
                error,
                "Could not load disputes",
            )
            .await
        }
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
    let page_config = ui::script_json(&serde_json::json!({
        "order_id": purchase_id,
        "refund_url": refund_url.clone(),
        "refunded_total": refunded_total,
        "currency_exponent": currency_exponent,
    }));
    let (back_url, back_label, sections) = match access {
        OrderPageAccess::Admin => (
            "/b/products/admin/purchases",
            "Orders",
            Sections::Admin(AdminSection::Orders),
        ),
        OrderPageAccess::Buyer => (
            "/b/products/my-purchases",
            "My purchases",
            Sections::Portal(
                PortalSection::Purchases,
                match super::handlers::user_products_enabled(ctx).await {
                    Ok(enabled) => enabled,
                    Err(e) => {
                        return error_page(
                            ctx,
                            msg,
                            error_sections,
                            "Order",
                            e,
                            "order page: seller switch read failed",
                        )
                        .await
                    }
                },
            ),
        ),
        OrderPageAccess::Seller => (
            "/b/products/selling/orders",
            "Seller orders",
            Sections::Portal(PortalSection::SellerOrders, true),
        ),
    };
    let buyer = if purchase.str_field("buyer_email").is_empty() {
        purchase.str_field("buyer_user_id")
    } else {
        purchase.str_field("buyer_email")
    };
    let title = format!("Order #{}", purchase.id.get(..8).unwrap_or(&purchase.id));
    let content = html! {
        p .text-muted .mt-0 { "Placed " (components::timestamp(purchase.str_field("created_at"))) }
        div #order-detail-error .login-error hidden {}
        div .stats-grid {
            (components::stat_card("Total", &display_money(purchase.i64_field("total_cents"), currency), icons::dollar_sign(), None))
            (components::stat_card("Refunded", &display_money(refunded_total, currency), icons::arrow_down_left(), None))
            (components::stat_card("Customer", if buyer.is_empty() { "Guest" } else { buyer }, icons::users(), None))
            (components::stat_card("Items", &line_items.len().to_string(), icons::package(), None))
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
                        div { p .text-muted .text-sm .m-0 { (label) } strong .text-sm { (components::timestamp(value)) } }
                    }
                }
            }
        }
        section .card .mt-4 {
            header .card__head { h2 .card__title { "Items" } }
            div .card__body {
                @if line_items.is_empty() {
                    p .text-muted { "No line-item snapshot is available for this order." }
                } @else {
                    @let cols = [
                        components::TableCol::new("Item"),
                        components::TableCol::new("Quantity"),
                        components::TableCol::new("Unit"),
                        components::TableCol::new("Total"),
                        components::TableCol::new("Configuration"),
                    ];
                    @let rows: Vec<Vec<Markup>> = line_items.iter().map(|item| {
                        // `input_snapshot` is a JSON object encoded into a
                        // `TEXT` column, so it arrives as the encoded string
                        // on every backend. `json_text_field` renders that,
                        // and a decoded value too.
                        let snapshot = item.json_text_field("input_snapshot");
                        vec![
                            html! { strong { (item.str_field("product_name")) } },
                            html! { (item.i64_field("quantity")) },
                            html! { (display_money(item.i64_field("unit_amount_minor"), currency)) },
                            html! { strong { (display_money(item.i64_field("total_minor"), currency)) } },
                            html! { @if snapshot.is_empty() || snapshot == "{}" { span .text-muted { "—" } } @else { details { summary { "View" } code .text-sm { (snapshot) } } } },
                        ]
                    }).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! {}))
                }
            }
        }
        div .grid .grid-auto-280 .gap-4 .mt-4 {
            section .card {
                header .card__head { h2 .card__title { "Buyer and checkout" } }
                div .card__body .text-sm {
                    p { strong { "Buyer: " } (if buyer.is_empty() { "Guest" } else { buyer }) }
                    p { strong { "Presentation: " } (purchase.str_field("checkout_mode")) }
                    p { strong { "Provider: " } (purchase.str_field("provider")) }
                    p { strong { "Seller account: " } (if purchase.str_field("seller_account_id").is_empty() { "Platform" } else { purchase.str_field("seller_account_id") }) }
                }
            }
            details .products-advanced .mt-0 {
                summary { "Technical payment details" }
                div .products-advanced__body .text-sm {
                    h2 .details-heading { "Provider reconciliation" }
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
                        p .text-danger { (purchase.str_field("reconciliation_error")) }
                    }
                    @if !purchase.str_field("provider_payment_error_code").is_empty() {
                        p { strong { "Provider code: " } code { (purchase.str_field("provider_payment_error_code")) } }
                    }
                    @if !purchase.str_field("provider_payment_error_message").is_empty()
                        && purchase.str_field("provider_payment_error_message") != purchase.str_field("reconciliation_error") {
                        p .text-danger { (purchase.str_field("provider_payment_error_message")) }
                    }
                }
            }
        }
        @if !purchase.str_field("stripe_subscription_id").is_empty() {
            section .card .mt-4 {
                header .card__head {
                    div {
                        h2 .card__title { "Subscription" }
                        p .text-muted .text-sm .text-subtitle { code { (purchase.str_field("stripe_subscription_id")) } }
                    }
                    (components::status_badge(purchase.str_field("subscription_status")))
                }
                div .card__body {
                    p .text-sm { strong { "Current period ends: " } @if purchase.str_field("subscription_current_period_end").is_empty() { "Not reported yet" } @else { (components::timestamp(purchase.str_field("subscription_current_period_end"))) } }
                    p .text-sm { strong { "Cancels at period end: " } (if purchase.bool_field("subscription_cancel_at_period_end") { "Yes" } else { "No" }) }
                    @if !purchase.str_field("subscription_canceled_at").is_empty() { p .text-sm { strong { "Canceled: " } (components::timestamp(purchase.str_field("subscription_canceled_at"))) } }
                    @if matches!(access, OrderPageAccess::Buyer) && !purchase.str_field("stripe_customer_id").is_empty() {
                        button .btn .btn--primary .btn--md type="button" data-action="pp-order-billing" { "Manage subscription and billing" }
                    }
                }
            }
        }
        @if !refunds.is_empty() {
            section .card .mt-4 {
                header .card__head { h2 .card__title { "Refund history" } }
                div .card__body {
                    @let cols = [
                        components::TableCol::new("Status"),
                        components::TableCol::new("Amount"),
                        components::TableCol::new("Provider refund"),
                        components::TableCol::new("Note"),
                        components::TableCol::new("Date"),
                    ];
                    @let rows: Vec<Vec<Markup>> = refunds.iter().map(|refund| vec![
                        components::status_badge(refund.str_field("status")),
                        html! { (display_money(refund.i64_field("amount_minor"), currency)) },
                        html! { code .text-sm { (refund.str_field("provider_refund_id")) } },
                        html! { span .text-sm { (refund.str_field("note")) } },
                        components::timestamp(refund.str_field("created_at")),
                    ]).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! {}))
                }
            }
        }
        @if !disputes.is_empty() {
            section .card .mt-4 {
                header .card__head {
                    div {
                        h2 .card__title { "Payment disputes" }
                        @if matches!(access, OrderPageAccess::Admin | OrderPageAccess::Seller) {
                            p .text-muted .text-sm .text-subtitle { "Evidence, balance impact, and payout actions are managed in Stripe. This ledger mirrors signed provider events." }
                        }
                    }
                }
                div .card__body {
                    @let cols = [
                        components::TableCol::new("Status"),
                        components::TableCol::new("Amount"),
                        components::TableCol::new("Reason"),
                        components::TableCol::new("Evidence due"),
                        components::TableCol::new("Provider dispute"),
                    ];
                    @let rows: Vec<Vec<Markup>> = disputes.iter().map(|dispute| vec![
                        components::status_badge(dispute.str_field("status")),
                        html! { strong { (display_money(dispute.i64_field("amount_minor"), dispute.str_field("currency"))) } },
                        html! { span .text-sm { (if dispute.str_field("reason").is_empty() { "Not supplied" } else { dispute.str_field("reason") }) } },
                        html! { @if dispute.str_field("evidence_due_by").is_empty() { (components::NO_VALUE) } @else { (components::timestamp(dispute.str_field("evidence_due_by"))) } },
                        html! { code .text-sm { (dispute.str_field("provider_dispute_id")) } },
                    ]).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! {}))
                }
            }
        }
        @if refund_url.is_some() && refundable {
            section .card .mt-4 {
                header .card__head { h2 .card__title { "Create refund" } }
                div .card__body {
                    p .text-muted .text-sm { "Leave the amount blank to refund the complete remaining balance. Stripe refunds and proportional Connect fee/transfer reversals are requested before local success is recorded." }
                    div .grid .grid-auto-240 .gap-4 {
                        div .form-group { label .form-label for="order-refund-amount" { "Amount (" (currency) ")" } input #order-refund-amount .form-input type="text" inputmode="decimal" placeholder="Full remaining amount" {} }
                        div .form-group { label .form-label for="order-refund-note" { "Private note" } textarea #order-refund-note .form-textarea maxlength="500" {} }
                    }
                    button .btn .btn--danger .btn--md type="button" data-action="po-submit-refund" { "Create refund" }
                }
            }
        }
        script { (maud::PreEscaped(format!("window.__orderDetailConfig={page_config};"))) }
        script src=(assets::commerce_portal_js_url()) {}
        script src=(assets::order_detail_js_url()) {}
    };
    let livemode = if purchase.bool_field("livemode") {
        "live"
    } else {
        "test"
    };
    let shell = sections
        .shell(&title)
        .trail(vec![
            ui::shell::Crumb {
                label: back_label,
                href: Some(back_url),
            },
            ui::shell::Crumb {
                label: &title,
                href: None,
            },
        ])
        .actions(vec![
            components::status_badge(purchase.str_field("status")),
            components::status_badge(livemode),
        ]);
    products_page(ctx, msg, shell, sections, content).await
}

// ---------------------------------------------------------------------------
// User: My Products
// ---------------------------------------------------------------------------

pub async fn my_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let seller_enabled = match super::handlers::user_products_enabled(ctx).await {
        Ok(value) => value,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::SellerProducts, true),
                "My products",
                e,
                "products page: seller switch read failed",
            )
            .await
        }
    };
    let (page, page_size, _) = msg.pagination_params(20);

    // `?view=deleted` is the seller's mirror of the admin Deleted tab, and
    // for the same reason: this page reads live-only, so a product the seller
    // deleted was invisible to them everywhere — while its Prices and
    // Payment Links stayed live in the connected account and went on taking
    // money. Without this view a seller could not see, restore, close or even
    // recover the id of their own deleted product.
    let deleted_view = msg.query("view") == "deleted";
    // Carried through pagination so paging the deleted list does not silently
    // bounce back to the live one.
    let base_href = if deleted_view {
        "/b/products/my-products?view=deleted"
    } else {
        "/b/products/my-products"
    };

    // The owner filter is the SAME on both views. `list_deleted` narrows the
    // deleted set exactly as `list_page` narrows the live one — it cannot
    // widen either — so handing it this filter is what keeps the Deleted tab
    // the caller's own products rather than every seller's.
    let filters = vec![Filter {
        field: "created_by".into(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id),
    }];
    // Each view is ordered by the timestamp its own table shows, matching
    // `manage_products`: sorting the deleted list by `created_at` files the
    // product the seller just deleted under its creation date, which for an
    // old product is the bottom of the one page that exists to undo it.
    let sort = vec![SortField {
        field: if deleted_view {
            "deleted_at".into()
        } else {
            "created_at".into()
        },
        desc: true,
    }];
    let result = if deleted_view {
        repo::products::list_deleted(ctx, page as i64, page_size as i64, filters, Some(sort)).await
    } else {
        repo::products::list_page(ctx, page as i64, page_size as i64, filters, Some(sort)).await
    };
    let list = match result {
        Ok(list) => list,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::SellerProducts, true),
                "My products",
                e,
                "products my-products page: product read failed",
            )
            .await
        }
    };

    let content = html! {
        div .filter-bar {
            (product_views("/b/products/my-products", deleted_view))
        }
        @if deleted_view {
            @let cols = [
                components::TableCol::new("Name").primary(),
                components::TableCol::new("Currency"),
                components::TableCol::new("Deleted"),
                components::TableCol::new("Actions").actions(),
            ];
            @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|record| {
                // Percent-encoded for the same reason the admin
                // Deleted view encodes: a product id is not
                // guaranteed URL-safe, and maud escapes HTML, not
                // URLs. Unencoded, an id holding `/`, `?` or `#`
                // splits the path and both buttons below aim at
                // nothing.
                let encoded_id = crate::util::url_path_encode(&record.id);
                let restore_url = format!("/b/products/api/products/{encoded_id}/restore");
                // Restore is the DANGEROUS half: it returns an
                // active, approved product to the public catalog
                // at once. Soft delete takes nothing down in
                // Stripe, so the row needs the other half too — a
                // way to shut this product's Prices and Payment
                // Links off WITHOUT relisting it.
                let close_url = format!("/b/products/my-products/{encoded_id}/close");
                vec![
                    html! { (record.str_field("name")) },
                    html! { (record.str_field("currency")) },
                    components::timestamp(record.str_field("deleted_at")),
                    deleted_row_actions(&close_url, &restore_url),
                ]
            }).collect();
            (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, product_list_empty("", true, "/b/products/my-products/new")))
        } @else {
            @let row_hrefs: Vec<String> = list.records.iter().map(|record| format!("/b/products/my-products/{}", crate::util::url_path_encode(&record.id))).collect();
            @let cols = [
                components::TableCol::new("Name").primary(),
                components::TableCol::new("Status"),
                components::TableCol::new("Currency"),
                components::TableCol::new("Created"),
            ];
            @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| vec![
                html! { (r.str_field("name")) },
                components::status_badge(r.str_field("status")),
                html! { (r.str_field("currency")) },
                components::timestamp(r.str_field("created_at")),
            ]).collect();
            (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), product_list_empty("", false, "/b/products/my-products/new")))
        }
        @if let Some(per_page) = std::num::NonZeroU32::new(page_size as u32) { (components::pagination(list.page as u32, per_page, list.total_count as u32, base_href)) }
    };

    let mut shell = ui::Shell::portal("My products", "My products").subtitle(if deleted_view {
        "Restore a product to bring it back into your catalog — a deleted product cannot be edited until it is restored"
    } else {
        "Create products and manage their offers, checkout links, and publication status"
    });
    if !deleted_view {
        shell = shell.actions(vec![create_link(
            "/b/products/my-products/new",
            "New product",
            "btn--primary",
        )]);
    }
    products_page(
        ctx,
        msg,
        shell,
        Sections::Portal(PortalSection::SellerProducts, seller_enabled),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// User: My Purchases
// ---------------------------------------------------------------------------

pub async fn my_purchases(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    let seller_enabled = match super::handlers::user_products_enabled(ctx).await {
        Ok(value) => value,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::Purchases, false),
                "My purchases",
                e,
                "products page: seller switch read failed",
            )
            .await
        }
    };
    let (page, page_size, _) = msg.pagination_params(20);

    let filters = vec![Filter {
        field: "user_id".into(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id),
    }];
    let result = repo::purchases::list_paginated(ctx, filters, page as i64, page_size as i64).await;
    let list = match result {
        Ok(list) => list,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Portal(PortalSection::Purchases, false),
                "My purchases",
                e,
                "products my-purchases page: order read failed",
            )
            .await
        }
    };

    let content = html! {
        @let row_hrefs: Vec<String> = list.records.iter().map(|record| format!("/b/products/my-purchases/{}", crate::util::url_path_encode(&record.id))).collect();
        @let cols = [
            components::TableCol::new("Status"),
            components::TableCol::new("Total"),
            components::TableCol::new("Provider"),
            components::TableCol::new("Date"),
        ];
        @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| {
            let amount = display_money(r.i64_field("total_cents"), r.str_field("currency"));
            vec![
                components::status_badge(r.str_field("status")),
                html! { span .font-medium { (amount) } },
                html! { span .text-muted .text-sm { (r.str_field("provider")) } },
                components::timestamp(r.str_field("created_at")),
            ]
        }).collect();
        (components::data_table(&cols, rows, Some(move |index| row_hrefs.get(index).cloned()), components::empty_state(icons::shopping_cart(), "No purchases yet", "Orders you place will appear here with their receipts.", None)))
        @if let Some(per_page) = std::num::NonZeroU32::new(page_size as u32) { (components::pagination(list.page as u32, per_page, list.total_count as u32, "/b/products/my-purchases")) }
    };

    products_page(
        ctx,
        msg,
        ui::Shell::portal("My purchases", "My purchases")
            .subtitle("Receipts, payment status, and subscription details"),
        Sections::Portal(PortalSection::Purchases, seller_enabled),
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
/// A failed runtime read is returned: the list decides whether secret keys
/// may be edited here.
async fn settings_vars(ctx: &dyn Context) -> Result<SettingsVars, wafer_run::WaferError> {
    let own = super::config_vars();
    let trusted_server = super::stripe_secret_operations_allowed(ctx);
    let mut stripe = vec![config_vars::var_in(&own, STRIPE_PUBLISHABLE_KEY)];
    let mut stripe_advanced = vec![config_vars::var_in(&own, STRIPE_API_VERSION)];
    let mut webhooks = vec![config_vars::shared_var(FRONTEND_URL_KEY)];
    if trusted_server {
        stripe.splice(
            0..0,
            [
                config_vars::var_in(&own, STRIPE_SECRET_KEY),
                config_vars::var_in(&own, STRIPE_WEBHOOK_SECRET),
            ],
        );
        stripe_advanced.insert(0, config_vars::var_in(&own, STRIPE_API_URL));
        webhooks.extend([
            config_vars::var_in(&own, WEBHOOK_URL),
            config_vars::var_in(&own, WEBHOOK_SECRET),
        ]);
    }
    Ok(SettingsVars {
        features: vec![config_vars::shared_var(ALLOW_USER_PRODUCTS_KEY)],
        stripe,
        stripe_advanced,
        checkout: vec![
            config_vars::var_in(&own, DEFAULT_CURRENCY),
            config_vars::var_in(&own, PLATFORM_COUNTRY),
            config_vars::var_in(&own, AUTOMATIC_TAX),
        ],
        checkout_advanced: vec![config_vars::var_in(&own, CHECKOUT_ALLOWED_ORIGINS)],
        sellers: vec![
            config_vars::var_in(&own, SELLER_APPLICATION_FEE_BPS),
            config_vars::var_in(&own, SELLER_MODERATION_REQUIRED),
            config_vars::var_in(&own, SELLER_ALLOWED_TEMPLATES),
            config_vars::var_in(&own, SELLER_ALLOWED_CURRENCIES),
            config_vars::var_in(&own, SELLER_ALLOWED_CATEGORIES),
            config_vars::var_in(&own, SELLER_MAX_PRODUCTS),
        ],
        webhooks,
    })
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
    let settings = async {
        Ok::<_, wafer_run::WaferError>((
            super::stripe_secret_operations_allowed(ctx),
            settings_vars(ctx).await?,
        ))
    };
    let (trusted_server, vars) = match settings.await {
        Ok(settings) => settings,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Sections::Admin(AdminSection::Settings),
                "Products settings",
                e,
                "products settings page: runtime read failed",
            )
            .await
        }
    };
    let sections = [
        SettingsSection::new("Stripe credentials", &vars.stripe)
            .description(
                "Add the keys from your Stripe Dashboard. Saved secret values stay masked.",
            ),
        SettingsSection::new("Store defaults", &vars.checkout)
            .description("Preselected for new products. You can still change them on each product."),
        SettingsSection::new("Seller products (optional)", &vars.features)
            .description("Turn this on only if customers should be able to create and sell their own products.")
            .collapsible(),
        SettingsSection::new("Advanced checkout security", &vars.checkout_advanced)
            .description("Restrict which website origins can be used as checkout return and cancel destinations.")
            .collapsible(),
        SettingsSection::new("Seller rules (optional)", &vars.sellers)
            .description("Set fees, approval rules, currencies, templates, and listing limits for sellers.")
            .collapsible(),
        SettingsSection::new("Advanced Stripe options", &vars.stripe_advanced)
            .description("Provider endpoint and API version overrides. The defaults are right for most stores.")
            .collapsible(),
        SettingsSection::new("Developer webhooks (optional)", &vars.webhooks)
            .description("Send signed billing events to another system you control.")
            .collapsible(),
    ];
    let form =
        match settings_form::settings_form(ctx, SETTINGS_SAVE_PATH, &sections, html! {}).await {
            Ok(form) => form,
            Err(e) => {
                return error_page(
                    ctx,
                    msg,
                    Sections::Admin(AdminSection::Settings),
                    "Products settings",
                    e,
                    "products settings: current values read failed",
                )
                .await
            }
        };
    let content = html! {
        (components::callout(
            components::CalloutTone::Info,
            "Start with Stripe credentials and store defaults",
            html! { p { "Seller tools, provider overrides, and developer webhooks are optional and stay tucked away until you need them." } },
            Some(html! { a .btn .btn--secondary .btn--sm href="/b/products/admin/stripe" { "Check Stripe status" } }),
        ))
        @if !trusted_server {
            (components::callout(
                components::CalloutTone::Warning,
                "Browser runtime safety",
                html! { p { "Stripe secret keys and signed webhooks are disabled here because browser storage is controlled by the visitor. Point the storefront widget at a trusted native or Cloudflare API, or use a pre-created Payment Link." } },
                None,
            ))
        }
        (form)
    };
    products_page(
        ctx,
        msg,
        ui::Shell::admin("Products settings", "Settings")
            .subtitle("Set up payments and choose sensible defaults for new products"),
        Sections::Admin(AdminSection::Settings),
        content,
    )
    .await
}

/// Where the settings form posts: the admin block, because the page shows
/// `WAFER_RUN_SHARED__*` keys (`FRONTEND_URL`, `ALLOW_USER_PRODUCTS`) and
/// WRAP lets only the admin block write those. The products block's own
/// keys on the same form save in the same request — see
/// `admin::pages::products_settings`.
pub(crate) const SETTINGS_SAVE_PATH: &str = "/b/admin/settings/products";

/// Every var the settings page shows — the save's allowlist, for the admin
/// block's handler. The same list the page renders, so the save accepts
/// exactly the fields the page offers (the Stripe secrets only on a trusted
/// server).
pub(crate) async fn settings_allowlist(
    ctx: &dyn Context,
) -> Result<Vec<wafer_run::ConfigVar>, wafer_run::WaferError> {
    Ok(settings_vars(ctx).await?.all())
}
