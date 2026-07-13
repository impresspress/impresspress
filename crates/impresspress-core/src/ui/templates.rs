//! Page templates — six standard layouts used by every block, plus a tiny
//! status template. Each template returns the body markup that goes inside
//! the shell (or the standalone `auth_split` / `status_page`). Pages
//! declare their template inputs and call one function — no bespoke
//! page HTML outside this module.

use maud::{html, Markup, PreEscaped, DOCTYPE};

use super::{assets, SiteConfig};

/// Header line for list / detail / form pages.
pub struct PageHeader<'a> {
    pub title: &'a str,
    pub subtitle: Option<&'a str>,
    pub primary_action: Option<Markup>, // typically a `components::button(...)` invocation
}

fn render_header(h: &PageHeader<'_>) -> Markup {
    if h.title.is_empty() && h.subtitle.is_none() && h.primary_action.is_none() {
        return html! {};
    }
    html! {
        header .page-header {
            div .page-header__text {
                // h2, not h1: the shell topbar owns the page's single h1.
                @if !h.title.is_empty() { h2 .page-header__title { (h.title) } }
                @if let Some(s) = h.subtitle { p .page-header__subtitle { (s) } }
            }
            @if let Some(a) = &h.primary_action {
                div .page-header__action { (a.clone()) }
            }
        }
    }
}

/// `list_page` template.
///
/// Sections (each rendered when present):
///   - Page header: title + optional subtitle + optional primary action
///   - Filter row: free-form markup the page provides (search input, facets)
///   - Table: `components::data_table` already handled by caller
///   - Pagination: `components::pagination` already handled by caller
pub fn list_page(
    header: PageHeader<'_>,
    filters: Option<Markup>,
    table: Markup,
    pagination: Option<Markup>,
) -> Markup {
    html! {
        div .page .page--list {
            (render_header(&header))
            @if let Some(f) = filters { div .page-filters { (f) } }
            div .page-body { (table) }
            @if let Some(p) = pagination { div .page-pagination { (p) } }
        }
    }
}

/// Detail page hero — for a single resource.
pub struct DetailHero<'a> {
    pub icon: Option<Markup>, // typically `components::avatar(...)` or an icon
    pub title: &'a str,
    pub subtitle: Option<&'a str>,
    pub badges: Vec<Markup>, // typically `components::badge(...)` calls
    pub action_menu: Option<Markup>, // dropdown / button group
}

/// One key/value row in the right-rail metadata panel.
pub struct DetailMeta<'a> {
    pub key: &'a str,
    pub value: Markup,
}

/// `detail_page` template.
pub fn detail_page(
    hero: DetailHero<'_>,
    sections: Vec<Markup>, // typically `section .card { .. }` panels
    meta: Vec<DetailMeta<'_>>,
) -> Markup {
    html! {
        div .page .page--detail {
            header .detail-hero {
                @if let Some(icon) = hero.icon { div .detail-hero__icon { (icon) } }
                div .detail-hero__text {
                    // h2, not h1: the shell topbar owns the page's single h1
                    // (the vector index detail page renders this hero inside
                    // a shell whose last crumb is already the index name).
                    h2 .detail-hero__title { (hero.title) }
                    @if let Some(s) = hero.subtitle { p .detail-hero__subtitle { (s) } }
                    @if !hero.badges.is_empty() {
                        div .detail-hero__badges { @for b in &hero.badges { (b.clone()) } }
                    }
                }
                @if let Some(a) = hero.action_menu { div .detail-hero__action { (a) } }
            }
            div .detail-body {
                div .detail-body__main {
                    @for s in sections { (s) }
                }
                @if !meta.is_empty() {
                    aside .detail-meta {
                        dl {
                            @for row in &meta {
                                dt { (row.key) }
                                dd { (row.value.clone()) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One section of a form — a labeled group of fields.
pub struct FormSection<'a> {
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub body: Markup,
}

/// Shared tab-rail + sections chrome used by [`form_page`] and
/// [`tabbed_page`] — the `.form-grid` with an optional `.form-tabs` left
/// rail and the `.form-sections` column.
fn form_grid(tabs: Option<Vec<(String, String, bool)>>, sections: Vec<FormSection<'_>>) -> Markup {
    let has_tabs = tabs.is_some();
    html! {
        div .(if has_tabs { "form-grid form-grid--with-tabs" } else { "form-grid" }) {
            @if let Some(t) = tabs {
                nav .form-tabs aria-label="Form sections" {
                    ul {
                        @for (label, href, active) in t {
                            li .(if active { "is-active" } else { "" }) {
                                a href=(href) aria-current=[active.then_some("page")] { (label) }
                            }
                        }
                    }
                }
            }
            div .form-sections {
                @for sec in sections {
                    section .form-section {
                        header .form-section__head {
                            h2 .form-section__title { (sec.title) }
                            @if let Some(d) = sec.description {
                                p .form-section__desc { (d) }
                            }
                        }
                        div .form-section__body { (sec.body) }
                    }
                }
            }
        }
    }
}

/// `form_page` template.
///
/// The whole page is ONE `<form>` posting to `submit_url`, with a sticky
/// save bar. Section bodies therefore must not contain `<form>` elements of
/// their own (HTML forms cannot nest — the browser drops a nested form's
/// start tag). For a tabbed shell whose tab bodies own their forms, use
/// [`tabbed_page`] instead.
///
/// `tabs` is an optional left-rail of section anchors. Pass `None` for a
/// single-column form.
pub fn form_page(
    header: PageHeader<'_>,
    tabs: Option<Vec<(String, String, bool)>>, // (label, href, is_active)
    sections: Vec<FormSection<'_>>,
    submit_url: &str,
    method: &str,
    save_label: &str,
) -> Markup {
    html! {
        div .page .page--form {
            (render_header(&header))
            form .form-page action=(submit_url) method=(method) {
                (form_grid(tabs, sections))
                footer .form-bar {
                    button type="submit" .btn .btn--primary .btn--md { (save_label) }
                }
            }
        }
    }
}

/// `tabbed_page` template — the same tab-rail + section chrome as
/// [`form_page`], but form-LESS: a `div.form-page` instead of the outer
/// `<form>`, and no sticky save bar.
///
/// For tabbed shells whose tab bodies own their submission story. HTML forms
/// cannot nest, so a shell that wraps tab bodies in a `<form>` silently
/// breaks any `<form>` a tab renders — the browser drops the inner form's
/// start tag, its `action`/`hx-*` attributes vanish, and its inputs (and
/// submit button) join the outer form. With this shell each tab renders its
/// own complete `<form>` + submit control (or none, for read-only tabs).
/// Used by the admin Settings page.
pub fn tabbed_page(
    header: PageHeader<'_>,
    tabs: Vec<(String, String, bool)>, // (label, href, is_active)
    sections: Vec<FormSection<'_>>,
) -> Markup {
    html! {
        div .page .page--form {
            (render_header(&header))
            div .form-page {
                (form_grid(Some(tabs), sections))
            }
        }
    }
}

pub struct StatTile<'a> {
    pub label: &'a str,
    pub value: &'a str,         // pre-formatted (caller decides rounding/units)
    pub trend: Option<&'a str>, // e.g. "+12% 7d"
}

pub fn dashboard_page(
    header: PageHeader<'_>,
    stats: Vec<StatTile<'_>>,
    primary_card: Markup,
    secondary_card: Markup,
    full_width_card: Option<Markup>,
    top_card: Option<Markup>,
) -> Markup {
    html! {
        div .page .page--dashboard {
            (render_header(&header))
            @if let Some(tc) = top_card { div .dashboard-top { (tc) } }
            @if !stats.is_empty() {
                div .stats-grid {
                    @for s in &stats {
                        div .stat-tile {
                            div .stat-tile__label { (s.label) }
                            div .stat-tile__value { (s.value) }
                            @if let Some(t) = s.trend { div .stat-tile__trend { (t) } }
                        }
                    }
                }
            }
            div .dashboard-grid {
                div .dashboard-grid__primary { (primary_card) }
                div .dashboard-grid__secondary { (secondary_card) }
            }
            @if let Some(fw) = full_width_card { div .dashboard-wide { (fw) } }
        }
    }
}

pub fn chat_page(
    thread_list: Markup,
    messages: Markup,
    composer: Markup,
    right_rail: Option<Markup>,
) -> Markup {
    html! {
        div .page--chat {
            aside .chat-threads { (thread_list) }
            section .chat-main {
                div .chat-messages { (messages) }
                div .chat-composer { (composer) }
            }
            @if let Some(r) = right_rail {
                aside .chat-rail { (r) }
            }
        }
    }
}

/// Inputs for [`account_card_page`] — the single-card layout used by
/// `/b/userportal/` and its sub-pages (profile, sessions, security). No
/// shell, no sidebar; mobile-first centered card with logo + title header,
/// page-specific body, and a sign-out footer.
pub struct AccountCard<'a> {
    pub logo_url: &'a str,
    pub title: &'a str,
    /// When `Some(href)`, render a "‹ Back" link in the top-left of the
    /// header. Sub-pages use this to return to `/b/userportal/`; the
    /// dashboard itself passes `None`.
    pub back_href: Option<&'a str>,
}

pub fn account_card_page(opts: AccountCard<'_>, body: Markup) -> Markup {
    html! {
        div .account-page {
            main .account-card {
                header .account-card__head {
                    @if let Some(href) = opts.back_href {
                        a .account-card__back href=(href) aria-label="Back" {
                            (crate::ui::icons::chevron_left()) " Back"
                        }
                    }
                    @if !opts.logo_url.is_empty() {
                        img .account-card__logo src=(opts.logo_url) alt="";
                    }
                    h1 .account-card__title { (opts.title) }
                }
                div .account-card__body { (body) }
                footer .account-card__foot {
                    form action="/b/auth/api/logout" method="post" {
                        button .account-card__signout type="submit" {
                            (crate::ui::icons::log_out())
                            span { "Sign Out" }
                        }
                    }
                }
            }
        }
    }
}

pub struct BrandPanel<'a> {
    pub logo_html: Option<Markup>,
    pub headline: &'a str,
    pub tagline: Option<&'a str>,
}

pub fn auth_split(brand: BrandPanel<'_>, form_card: Markup) -> Markup {
    html! {
        div .auth-split {
            aside .auth-split__brand {
                @if let Some(l) = brand.logo_html { div .auth-split__logo { (l) } }
                h1 .auth-split__headline { (brand.headline) }
                @if let Some(t) = brand.tagline { p .auth-split__tagline { (t) } }
            }
            main .auth-split__form { (form_card) }
        }
    }
}

/// Tiny template for `/`, 404, 403, 500 — auth-split-shaped, just an
/// illustrated message + primary action. Replaces the inline 404/403
/// markup currently in `ui/mod.rs`.
pub fn status_page(
    code: &str, // "404", "403", "500", or "" for "/"
    title: &str,
    body: &str,
    primary_action: Option<(String, String)>, // (label, href)
) -> Markup {
    html! {
        div .status-page {
            div .status-page__inner {
                @if !code.is_empty() { div .status-page__code { (code) } }
                h1 .status-page__title { (title) }
                p .status-page__body { (body) }
                @if let Some((label, href)) = primary_action {
                    a .btn .btn--primary .btn--md href=(href) { (label) }
                }
            }
        }
    }
}

/// Inputs for [`public_page`] — anonymous full-page chrome shared by all
/// public-facing surfaces (legal pages, marketing, etc.). No sidebar, no
/// admin chrome, no auth-aware bits. Returns the *full* HTML document
/// including DOCTYPE — unlike the other templates in this module, which
/// return body fragments wrapped by `layout::page` later.
pub struct PublicPage<'a> {
    /// Window title. Combined with `config.app_name` if non-empty.
    pub title: &'a str,
    /// Site branding (favicon, app name) and any embedded scripts.
    pub config: &'a SiteConfig,
    /// Optional `<meta name="description">` for SEO / social cards.
    pub meta_description: Option<&'a str>,
    /// Optional href for the back-arrow shown in the header. None hides
    /// the header entirely (use for true root pages).
    pub back_url: Option<&'a str>,
    /// Optional CSS color override for the page background. Set via
    /// `--public-page-bg` custom property; falls back to `--surface-2`.
    pub bg_color: Option<&'a str>,
    /// Optional CSS color override for accent (links, focus). Set via
    /// `--public-page-accent`; falls back to `--primary-color`.
    pub accent_color: Option<&'a str>,
    /// Optional pre-sanitized footer HTML. Rendered verbatim (caller is
    /// responsible for sanitization — see `ammonia::clean` upstream).
    pub footer: Option<Markup>,
}

/// `public_page` template — full HTML document for anonymous public-facing
/// pages (legal documents, marketing, etc.). Standard impresspress CSS bundle,
/// minimal header (back-arrow only), optional footer.
///
/// The body is rendered inside `<main class="public-page">` with a centered
/// card (`.public-page__card`); pages put long-form prose inside
/// `.public-page__content` to inherit the prose typography.
pub fn public_page(opts: PublicPage<'_>, body: Markup) -> Markup {
    // Build a tiny inline `:root` override only when overrides are present;
    // otherwise the defaults from tokens.css apply.
    let inline_vars = match (opts.bg_color, opts.accent_color) {
        (None, None) => String::new(),
        (bg, accent) => {
            let mut s = String::from(":root{");
            if let Some(c) = bg {
                s.push_str(&format!("--public-page-bg:{c};"));
            }
            if let Some(c) = accent {
                s.push_str(&format!("--public-page-accent:{c};"));
            }
            s.push('}');
            s
        }
    };

    let full_title = if opts.config.app_name.is_empty() {
        opts.title.to_string()
    } else {
        format!("{} \u{2014} {}", opts.title, opts.config.app_name)
    };

    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                title { (full_title) }
                @if let Some(desc) = opts.meta_description {
                    meta name="description" content=(desc);
                }
                link rel="stylesheet" href=(assets::css_url());
                @if !opts.config.favicon_url.is_empty() {
                    link rel="icon" href=(opts.config.favicon_url);
                }
                @if !inline_vars.is_empty() {
                    style { (PreEscaped(inline_vars)) }
                }
            }
            body .public-page-body {
                @if let Some(href) = opts.back_url {
                    header .public-page__header {
                        div .public-page__header-inner {
                            a .public-page__back href=(href) title="Go back" aria-label="Go back" {
                                "\u{2190}"
                            }
                        }
                    }
                }
                main .public-page {
                    div .public-page__card {
                        (body)
                    }
                }
                @if let Some(f) = opts.footer {
                    footer .public-page__footer { (f) }
                }
                @for src in &opts.config.embedded_scripts {
                    script type="module" src=(src) {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use maud::PreEscaped;

    use super::*;
    use crate::ui::components::{button, BtnVariant, CtrlSize};

    #[test]
    fn list_page_renders_header_table_pagination() {
        let header = PageHeader {
            title: "Users",
            subtitle: Some("142 total"),
            primary_action: Some(button(
                BtnVariant::Primary,
                CtrlSize::Md,
                "+ Invite",
                PreEscaped(String::new()),
            )),
        };
        let table = html! { div .data-table { table {} } };
        let pagination = Some(html! { nav .pagination { "1/4" } });
        let s = list_page(header, None, table, pagination).into_string();
        assert!(s.contains("page--list"));
        assert!(s.contains(">Users<"));
        assert!(s.contains("142 total"));
        assert!(s.contains("+ Invite"));
        assert!(s.contains("data-table"));
        assert!(s.contains("page-pagination"));
    }

    #[test]
    fn list_page_omits_optional_sections_when_absent() {
        let header = PageHeader {
            title: "Empty",
            subtitle: None,
            primary_action: None,
        };
        let table = html! { div .empty { "none" } };
        let s = list_page(header, None, table, None).into_string();
        assert!(!s.contains("page-filters"));
        assert!(!s.contains("page-pagination"));
        assert!(!s.contains("page-header__action"));
        assert!(!s.contains("page-header__subtitle"));
    }

    #[test]
    fn detail_page_renders_hero_sections_and_meta() {
        let hero = DetailHero {
            icon: Some(html! { span .av {} }),
            title: "alice@example.com",
            subtitle: Some("Member since Jan 2026"),
            badges: vec![html! { span .badge { "Admin" } }],
            action_menu: None,
        };
        let sections = vec![
            html! { section .card { "Activity" } },
            html! { section .card { "Sessions" } },
        ];
        let meta = vec![
            DetailMeta {
                key: "ID",
                value: html! { code { "u_42" } },
            },
            DetailMeta {
                key: "Created",
                value: html! { "2026-01-12" },
            },
        ];
        let s = detail_page(hero, sections, meta).into_string();
        assert!(s.contains("detail-hero"));
        assert!(s.contains("alice@example.com"));
        assert!(s.contains("Admin"));
        assert!(s.contains("Activity"));
        assert!(s.contains("Sessions"));
        assert!(s.contains("u_42"));
        assert!(s.contains("Created"));
    }

    #[test]
    fn detail_page_omits_meta_aside_when_empty() {
        let hero = DetailHero {
            icon: None,
            title: "X",
            subtitle: None,
            badges: vec![],
            action_menu: None,
        };
        let s = detail_page(hero, vec![], vec![]).into_string();
        assert!(!s.contains("detail-meta"));
    }

    #[test]
    fn form_page_with_tabs_marks_active() {
        let header = PageHeader {
            title: "Settings",
            subtitle: None,
            primary_action: None,
        };
        let tabs = Some(vec![
            (
                "Email".to_string(),
                "/b/admin/settings/email".to_string(),
                false,
            ),
            (
                "Network".to_string(),
                "/b/admin/settings/network".to_string(),
                true,
            ),
        ]);
        let sections = vec![FormSection {
            title: "Network",
            description: None,
            body: html! { "..." },
        }];
        let s = form_page(
            header,
            tabs,
            sections,
            "/b/admin/settings/network",
            "post",
            "Save",
        )
        .into_string();
        assert!(s.contains("form-grid--with-tabs"));
        assert!(s.contains(r#"aria-current="page""#));
        assert!(s.contains("is-active"));
        assert!(s.contains(r#"action="/b/admin/settings/network""#));
        assert!(s.contains(">Save</button>"));
    }

    #[test]
    fn form_page_without_tabs_uses_single_column() {
        let header = PageHeader {
            title: "Profile",
            subtitle: None,
            primary_action: None,
        };
        let sections = vec![FormSection {
            title: "Account",
            description: Some("Public info"),
            body: html! { "..." },
        }];
        let s = form_page(header, None, sections, "/me", "post", "Update").into_string();
        assert!(!s.contains("form-grid--with-tabs"));
        assert!(s.contains("Account"));
        assert!(s.contains("Public info"));
    }

    #[test]
    fn dashboard_renders_stats_and_cards() {
        let header = PageHeader {
            title: "Dashboard",
            subtitle: None,
            primary_action: None,
        };
        let stats = vec![
            StatTile {
                label: "Users",
                value: "142",
                trend: Some("+5 7d"),
            },
            StatTile {
                label: "Storage",
                value: "1.2 GB",
                trend: None,
            },
        ];
        let primary = html! { section .card { "Quick actions" } };
        let secondary = html! { section .card { "Recent activity" } };
        let s = dashboard_page(header, stats, primary, secondary, None, None).into_string();
        assert!(s.contains("stats-grid"));
        assert!(s.contains(">Users<"));
        assert!(s.contains("142"));
        assert!(s.contains("+5 7d"));
        assert!(s.contains("Quick actions"));
        assert!(s.contains("Recent activity"));
        assert!(!s.contains("dashboard-wide"));
    }

    #[test]
    fn dashboard_page_renders_optional_top_card_above_stats() {
        let header = PageHeader {
            title: "Dash",
            subtitle: None,
            primary_action: None,
        };
        let m = dashboard_page(
            header,
            vec![StatTile {
                label: "Users",
                value: "1",
                trend: None,
            }],
            html! { div #primary {} },
            html! { div #secondary {} },
            None,
            Some(html! { div #top-card { "QA" } }),
        );
        let s = m.into_string();
        let top = s.find("dashboard-top").expect("dashboard-top div present");
        let stats = s.find("stats-grid").expect("stats-grid div present");
        assert!(top < stats, "top card must render above stats");
        assert!(s.contains(r#"id="top-card""#));
    }

    #[test]
    fn chat_page_with_rail() {
        let s = chat_page(
            html! { div { "threads" } },
            html! { div { "messages" } },
            html! { textarea {} },
            Some(html! { div { "rail" } }),
        )
        .into_string();
        assert!(s.contains("chat-threads"));
        assert!(s.contains("chat-main"));
        assert!(s.contains("chat-messages"));
        assert!(s.contains("chat-composer"));
        assert!(s.contains("chat-rail"));
        assert!(s.contains(">rail<"));
    }

    #[test]
    fn chat_page_no_rail_omits_aside() {
        let s = chat_page(
            html! { div { "threads" } },
            html! { div {} },
            html! { textarea {} },
            None,
        )
        .into_string();
        assert!(!s.contains("chat-rail"));
    }

    #[test]
    fn auth_split_renders_brand_and_form() {
        let brand = BrandPanel {
            logo_html: Some(html! { div .logo {} }),
            headline: "Welcome back",
            tagline: Some("Sign in to continue."),
        };
        let form = html! { section .card { "form" } };
        let s = auth_split(brand, form).into_string();
        assert!(s.contains("auth-split__brand"));
        assert!(s.contains("auth-split__form"));
        assert!(s.contains("Welcome back"));
        assert!(s.contains("Sign in to continue."));
    }

    #[test]
    fn status_page_404_renders_code_and_action() {
        let s = status_page(
            "404",
            "Page not found",
            "We couldn't find that page.",
            Some(("Go home".to_string(), "/".to_string())),
        )
        .into_string();
        assert!(s.contains(">404<"));
        assert!(s.contains("Page not found"));
        assert!(s.contains("Go home"));
        assert!(s.contains(r#"href="/""#));
    }

    #[test]
    fn status_page_no_code_no_action() {
        let s = status_page("", "Hello", "Welcome.", None).into_string();
        assert!(!s.contains("status-page__code"));
        assert!(!s.contains(r#"class="btn"#));
    }

    fn public_site_config() -> SiteConfig {
        SiteConfig {
            app_name: "Acme".to_string(),
            logo_url: String::new(),
            logo_icon_url: String::new(),
            favicon_url: "/favicon.ico".to_string(),
            primary_color: String::new(),
            embedded_scripts: Vec::new(),
        }
    }

    #[test]
    fn public_page_renders_full_document_with_doctype_head_and_body() {
        let cfg = public_site_config();
        let opts = PublicPage {
            title: "Terms of Service",
            config: &cfg,
            meta_description: Some("Our terms"),
            back_url: Some("/"),
            bg_color: None,
            accent_color: None,
            footer: None,
        };
        let body = html! { div .public-page__content { p { "Hello" } } };
        let s = public_page(opts, body).into_string();

        assert!(s.contains("<!DOCTYPE html>"));
        assert!(s.contains(r#"<html lang="en">"#));
        assert!(s.contains(r#"<meta charset="utf-8">"#));
        assert!(
            s.contains(r#"<meta name="viewport" content="width=device-width,initial-scale=1">"#)
        );
        // Title combines page title + app name.
        assert!(s.contains("Terms of Service \u{2014} Acme"));
        assert!(s.contains(r#"<meta name="description" content="Our terms">"#));
        assert!(s.contains(r#"href="/favicon.ico""#));
        // Standard CSS bundle linked (hash is content-derived).
        assert!(s.contains(r#"<link rel="stylesheet" href="/b/static/app-"#));
        // Header back link present.
        assert!(s.contains(r#"class="public-page__back" href="/""#));
        // Body wrapper present.
        assert!(s.contains(r#"<main class="public-page">"#));
        assert!(s.contains(r#"class="public-page__card""#));
        assert!(s.contains("Hello"));
    }

    #[test]
    fn public_page_omits_optional_chrome() {
        let cfg = public_site_config();
        let opts = PublicPage {
            title: "Hi",
            config: &cfg,
            meta_description: None,
            back_url: None,
            bg_color: None,
            accent_color: None,
            footer: None,
        };
        let s = public_page(opts, html! { p { "x" } }).into_string();
        assert!(!s.contains("public-page__header"));
        assert!(!s.contains("public-page__footer"));
        assert!(!s.contains(r#"name="description""#));
    }

    #[test]
    fn public_page_inlines_color_overrides() {
        let cfg = public_site_config();
        let opts = PublicPage {
            title: "x",
            config: &cfg,
            meta_description: None,
            back_url: None,
            bg_color: Some("#fafafa"),
            accent_color: Some("#6366f1"),
            footer: None,
        };
        let s = public_page(opts, html! {}).into_string();
        assert!(s.contains("--public-page-bg:#fafafa"));
        assert!(s.contains("--public-page-accent:#6366f1"));
    }

    #[test]
    fn public_page_renders_footer_markup() {
        let cfg = public_site_config();
        let opts = PublicPage {
            title: "x",
            config: &cfg,
            meta_description: None,
            back_url: None,
            bg_color: None,
            accent_color: None,
            footer: Some(html! { span { "© 2026 Acme" } }),
        };
        let s = public_page(opts, html! {}).into_string();
        assert!(s.contains("public-page__footer"));
        assert!(s.contains("© 2026 Acme"));
    }

    #[test]
    fn public_page_title_omits_separator_when_app_name_empty() {
        let cfg = SiteConfig {
            app_name: String::new(),
            logo_url: String::new(),
            logo_icon_url: String::new(),
            favicon_url: String::new(),
            primary_color: String::new(),
            embedded_scripts: Vec::new(),
        };
        let opts = PublicPage {
            title: "Just Title",
            config: &cfg,
            meta_description: None,
            back_url: None,
            bg_color: None,
            accent_color: None,
            footer: None,
        };
        let s = public_page(opts, html! {}).into_string();
        assert!(s.contains("<title>Just Title</title>"));
        assert!(!s.contains(" \u{2014} "));
    }
}
