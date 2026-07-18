//! SSR pages for the products block (admin + user views).

use maud::html;
use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{repo, GROUPS_TABLE, PRICING_TABLE, PRODUCTS_TABLE};
use crate::{
    config_vars,
    ui::{self, components, icons, settings_form, settings_form::SettingsSection},
    util::RecordExt,
};

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
    let pricing_count = match db::count(ctx, PRICING_TABLE, &[]).await {
        Ok(n) => n,
        Err(e) => return crate::http::err_internal("Database error", e),
    };
    let user_products_enabled = super::handlers::user_products_enabled(ctx).await;

    let content = html! {
        (components::page_header("Products Overview", Some("Product catalog statistics"), None))
        div .stats-grid {
            (components::stat_card("Products", &products_count.to_string(), icons::package()))
            (components::stat_card("Groups", &groups_count.to_string(), icons::folder()))
            (components::stat_card("Pricing Templates", &pricing_count.to_string(), icons::dollar_sign()))
            (components::stat_card("Purchases", &purchases_count.to_string(), icons::shopping_cart()))
        }
        (render_overview_empty_state(products_count, user_products_enabled))
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Products", ui::NavKind::Portal, "Products"),
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
            "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS is disabled, so regular users can't create or manage their own product listings yet — you can still add products yourself from Manage Products. Enable this in Settings to let users sell their own products.",
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

    let new_product_button = components::button(
        components::BtnVariant::Primary,
        components::CtrlSize::Sm,
        "+ New Product",
        maud::PreEscaped(r#"type="button" onclick="openModal('create-product')""#.to_string()),
    );

    let content = html! {
        (components::page_header("Products", Some("Manage your product catalog"), Some(new_product_button)))

        div .filter-bar {
            (components::search_input("search", "Search products...", "/b/products/admin/manage", "#products-content"))
        }

        div #products-content {
            @match &result {
                Ok(list) => {
                    @let cols = [
                        components::TableCol { label: "Name", width: None },
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Price", width: None },
                        components::TableCol { label: "Group", width: None },
                        components::TableCol { label: "Created", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|record| {
                        let group_id = record.str_field("group_id");
                        let created = record.str_field("created_at");
                        vec![
                            html! { span .font-medium { (record.str_field("name")) } },
                            components::status_badge(record.str_field("status")),
                            html! { (record.str_field("base_price")) " " span .text-muted { (record.str_field("currency")) } },
                            html! { span .text-muted .text-sm { @if group_id.is_empty() { "—" } @else { (group_id.get(..8).unwrap_or(group_id)) } } },
                            html! { span .text-muted .text-sm { (created.get(..10).unwrap_or(created)) } },
                        ]
                    }).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! { p .text-muted { "No products found" } }))
                    (components::pagination(list.page as u32, list.page_size as u32, list.total_count as u32, "/b/products/admin/manage"))
                }
                Err(e) => { div .login-error { "Error: " (e.message) } }
            }
        }

        (render_create_product_modal())
        script { (maud::PreEscaped(create_product_js())) }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Products", ui::NavKind::Portal, "Products"),
        content,
    )
    .await
}

/// Render the "+ New Product" create modal for the admin Manage Products
/// page — the real, working create path the Overview empty-state CTA links
/// to. Submits via `create_product_js` (fetch + JSON) against the existing
/// admin JSON API (`POST /b/products/api/admin/products`, already
/// `AuthLevel::Admin`-gated in `mod.rs` and unaffected by
/// `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS`) rather than an htmx form post —
/// that endpoint's JSON contract (typed `base_price`, arbitrary optional
/// fields) is shared with programmatic API clients and shouldn't grow an
/// implicit urlencoded-vs-JSON coercion path just for this one modal.
/// Mirrors the bucket-create modal's fetch+JSON+reload pattern
/// (`pages_user::render_new_bucket_modal` / `files-browser.js`).
fn render_create_product_modal() -> maud::Markup {
    components::modal(
        "create-product",
        "Add product",
        html! {
            form id="create-product-form" onsubmit="return handleCreateProduct(event)" {
                p .form-error hidden {}
                div .form-group {
                    label .form-label .required for="product-name" { "Name" }
                    input .form-input type="text" id="product-name" name="name" placeholder="e.g. Cloud Hosting" required;
                }
                div .form-group {
                    label .form-label for="product-price" { "Base price" }
                    input .form-input type="number" id="product-price" name="base_price" step="0.01" min="0" placeholder="0.00";
                }
                div .form-group {
                    label .form-label for="product-currency" { "Currency" }
                    input .form-input type="text" id="product-currency" name="currency" value="USD" maxlength="3";
                }
                div .form-actions {
                    button .btn .btn-secondary type="button" onclick="closeModal('create-product')" { "Cancel" }
                    button .btn .btn-primary type="submit" id="create-product-submit" { "Create" }
                }
            }
        },
    )
}

/// JS backing [`render_create_product_modal`]. Validates the name
/// client-side, coerces an optional price to a number (never sends a
/// stringly-typed `base_price` to the JSON API), posts, and reloads on
/// success so the new row shows up in the (newest-first) table.
fn create_product_js() -> &'static str {
    r#"
async function handleCreateProduct(ev){
  ev.preventDefault();
  var form=document.getElementById('create-product-form');
  var errEl=form.querySelector('.form-error');
  var btn=document.getElementById('create-product-submit');
  var name=document.getElementById('product-name').value.trim();
  if(!name){errEl.textContent='Name is required.';errEl.hidden=false;return false}
  var priceRaw=document.getElementById('product-price').value.trim();
  var currency=document.getElementById('product-currency').value.trim()||'USD';
  var payload={name:name,currency:currency};
  if(priceRaw!==''){
    var price=parseFloat(priceRaw);
    if(isNaN(price)||price<0){errEl.textContent='Base price must be a positive number.';errEl.hidden=false;return false}
    payload.base_price=price;
  }
  errEl.hidden=true;
  btn.disabled=true;
  try{
    var r=await fetch('/b/products/api/admin/products',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(payload)});
    if(r.ok){window.location.reload();return false}
    var msg='Failed to create product.';
    try{var d=await r.json();if(d&&d.message)msg=d.message}catch(_){}
    errEl.textContent=msg;errEl.hidden=false;btn.disabled=false;
  }catch(e){errEl.textContent='Network error. Please try again.';errEl.hidden=false;btn.disabled=false}
  return false;
}
"#
}

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
        (components::page_header("Groups", Some("Organize products into groups"), None))

        div #groups-content {
            @match &result {
                Ok(list) => {
                    @let cols = [
                        components::TableCol { label: "Name", width: None },
                        components::TableCol { label: "Description", width: None },
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Created", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| vec![
                        html! { span .font-medium { (r.str_field("name")) } },
                        html! { span .text-muted .text-sm { (r.str_field("description")) } },
                        components::status_badge(r.str_field("status")),
                        html! { span .text-muted .text-sm { (r.str_field("created_at").get(..10).unwrap_or("")) } },
                    ]).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! { p .text-muted { "No groups" } }))
                }
                Err(e) => { div .login-error { "Error: " (e.message) } }
            }
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Groups", ui::NavKind::Portal, "Groups"),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Admin: Pricing Templates
// ---------------------------------------------------------------------------

pub async fn pricing(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "name".into(),
            desc: false,
        }],
        limit: 100,
        ..Default::default()
    };
    let result = db::list(ctx, PRICING_TABLE, &opts).await;

    let content = html! {
        (components::page_header("Pricing Templates", Some("Define pricing formulas for products"), None))

        div #pricing-content {
            @match &result {
                Ok(list) => {
                    @let cols = [
                        components::TableCol { label: "Name", width: None },
                        components::TableCol { label: "Formula", width: None },
                        components::TableCol { label: "Created", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| vec![
                        html! { span .font-medium { (r.str_field("name")) } },
                        html! { span .text-sm { code { (r.str_field("price_formula")) } } },
                        html! { span .text-muted .text-sm { (r.str_field("created_at").get(..10).unwrap_or("")) } },
                    ]).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! { p .text-muted { "No pricing templates" } }))
                }
                Err(e) => { div .login-error { "Error: " (e.message) } }
            }
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Pricing", ui::NavKind::Portal, "Pricing"),
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
        (components::page_header("Purchases", Some("Track customer orders and payments"), None))

        // Status filter
        div .filter-bar {
            @for s in &["all", "pending", "completed", "refunded", "cancelled"] {
                a .btn .(if (status_filter.is_empty() && *s == "all") || status_filter == *s { "btn-primary" } else { "btn-secondary" })
                    .btn-sm
                    href={"/b/products/purchases?status=" (*s)}
                    hx-get={"/b/products/purchases?status=" (*s)}
                    hx-target="#content"
                    hx-push-url="true"
                { (*s) }
            }
        }

        div #purchases-content {
            @match &result {
                Ok(list) => {
                    @let cols = [
                        components::TableCol { label: "User", width: None },
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Total", width: None },
                        components::TableCol { label: "Provider", width: None },
                        components::TableCol { label: "Date", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| {
                        let total_cents = r.i64_field("total_cents");
                        let amount = format!("{:.2}", total_cents as f64 / 100.0);
                        vec![
                            html! { span .text-sm { (r.str_field("user_id").get(..8).unwrap_or("—")) } },
                            components::status_badge(r.str_field("status")),
                            html! { span .font-medium { (amount) " " span .text-muted { (r.str_field("currency")) } } },
                            html! { span .text-muted .text-sm { (r.str_field("provider")) } },
                            html! { span .text-muted .text-sm { (r.str_field("created_at").get(..10).unwrap_or("")) } },
                        ]
                    }).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! { p .text-muted { "No purchases" } }))
                    (components::pagination(list.page as u32, list.page_size as u32, list.total_count as u32, "/b/products/admin/purchases"))
                }
                Err(e) => { div .login-error { "Error: " (e.message) } }
            }
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Purchases", ui::NavKind::Portal, "Purchases"),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// User: My Products
// ---------------------------------------------------------------------------

pub async fn my_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
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
        (components::page_header("My Products", None, None))

        div #my-products-content {
            @match &result {
                Ok(list) => {
                    @let cols = [
                        components::TableCol { label: "Name", width: None },
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Price", width: None },
                        components::TableCol { label: "Created", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| vec![
                        html! { span .font-medium { (r.str_field("name")) } },
                        components::status_badge(r.str_field("status")),
                        html! { (r.str_field("base_price")) " " span .text-muted { (r.str_field("currency")) } },
                        html! { span .text-muted .text-sm { (r.str_field("created_at").get(..10).unwrap_or("")) } },
                    ]).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! { p .text-muted { "No products yet" } }))
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
    let (page, page_size, _) = msg.pagination_params(20);

    let filters = vec![Filter {
        field: "user_id".into(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id),
    }];
    let result = repo::purchases::list_paginated(ctx, filters, page as i64, page_size as i64).await;

    let content = html! {
        (components::page_header("My Purchases", None, None))

        div #my-purchases-content {
            @match &result {
                Ok(list) => {
                    @let cols = [
                        components::TableCol { label: "Status", width: None },
                        components::TableCol { label: "Total", width: None },
                        components::TableCol { label: "Provider", width: None },
                        components::TableCol { label: "Date", width: None },
                    ];
                    @let rows: Vec<Vec<maud::Markup>> = list.records.iter().map(|r| {
                        let total_cents = r.i64_field("total_cents");
                        let amount = format!("{:.2}", total_cents as f64 / 100.0);
                        vec![
                            components::status_badge(r.str_field("status")),
                            html! { span .font-medium { (amount) " " span .text-muted { (r.str_field("currency")) } } },
                            html! { span .text-muted .text-sm { (r.str_field("provider")) } },
                            html! { span .text-muted .text-sm { (r.str_field("created_at").get(..10).unwrap_or("")) } },
                        ]
                    }).collect();
                    (components::data_table(&cols, rows, None::<fn(usize) -> Option<String>>, html! { p .text-muted { "No purchases yet" } }))
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
fn settings_vars() -> SettingsVars {
    let own = super::config_vars();
    SettingsVars {
        features: vec![config_vars::shared_var(
            "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS",
        )],
        stripe: vec![
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL"),
        ],
        webhooks: vec![
            config_vars::shared_var("WAFER_RUN_SHARED__FRONTEND_URL"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__WEBHOOK_URL"),
            config_vars::var_in(&own, "IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET"),
        ],
    }
}

struct SettingsVars {
    features: Vec<wafer_run::ConfigVar>,
    stripe: Vec<wafer_run::ConfigVar>,
    webhooks: Vec<wafer_run::ConfigVar>,
}

impl SettingsVars {
    /// Flatten to a single allowlist for the save handler.
    fn all(&self) -> Vec<wafer_run::ConfigVar> {
        let mut v = self.features.clone();
        v.extend(self.stripe.iter().cloned());
        v.extend(self.webhooks.iter().cloned());
        v
    }
}

pub async fn settings(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let vars = settings_vars();
    let sections = [
        SettingsSection::new("Features", icons::settings(), &vars.features),
        SettingsSection::new("Stripe", icons::dollar_sign(), &vars.stripe),
        SettingsSection::new("Webhooks", icons::globe(), &vars.webhooks),
    ];
    let content = html! {
        (components::page_header("Settings", Some("Configure payments and integrations"), None))
        (settings_form::settings_form(ctx, "/b/products/admin/settings", &sections, html! {}).await)
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Settings", ui::NavKind::Portal, "Settings"),
        content,
    )
    .await
}

pub async fn handle_save_settings(ctx: &dyn Context, input: InputStream) -> OutputStream {
    settings_form::save_settings(ctx, input, &settings_vars().all(), "products").await
}
