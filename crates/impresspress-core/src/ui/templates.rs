//! Page templates — the standard layouts used by every block, plus a tiny
//! status template. Each template returns the body markup that goes inside
//! the shell (or the standalone `auth_split` / `status_page`). Pages
//! declare their template inputs and call one function — no bespoke
//! page HTML outside this module.
//!
//! None of these templates renders a page header. The shell's topbar owns the
//! page's `h1` (`ui::shell::render_topbar`); a body-level heading is
//! `components::page_header`, rendered by the page into the template's body.

use maud::{html, Markup, PreEscaped, DOCTYPE};

use super::{assets, SiteConfig};

/// `list_page` template.
///
/// Sections (each rendered when present):
///   - Filter row: free-form markup the page provides (search input, facets)
///   - Table: `components::data_table` already handled by caller
///   - Pagination: `components::pagination` already handled by caller
pub fn list_page(filters: Option<Markup>, table: Markup, pagination: Option<Markup>) -> Markup {
    html! {
        div .page .page--list {
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

/// The tab-rail + sections chrome behind [`tabbed_page`] — the `.form-grid`,
/// its `.form-tabs` left rail and the `.form-sections` column.
///
/// The rail is not optional, and this signature is why: `form_page`, the
/// single-column template that passed no tabs, had no callers and is gone, so
/// `.form-grid` without `.form-grid--with-tabs` reached no page.
fn form_grid(tabs: Vec<(String, String, bool)>, sections: Vec<FormSection<'_>>) -> Markup {
    html! {
        div .form-grid .form-grid--with-tabs {
            nav .form-tabs aria-label="Form sections" {
                ul {
                    @for (label, href, active) in tabs {
                        li .(if active { "is-active" } else { "" }) {
                            a href=(href) aria-current=[active.then_some("page")] { (label) }
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

/// `tabbed_page` template — a tab rail over section bodies, form-LESS: the
/// `div.form-page` groups the sections but is not itself a `<form>`, and the
/// page carries no save bar of its own.
///
/// For tabbed shells whose tab bodies own their submission story. HTML forms
/// cannot nest, so a shell that wraps tab bodies in a `<form>` silently
/// breaks any `<form>` a tab renders — the browser drops the inner form's
/// start tag, its `action`/`hx-*` attributes vanish, and its inputs (and
/// submit button) join the outer form. With this shell each tab renders its
/// own complete `<form>` + submit control (or none, for read-only tabs).
/// Used by the admin Settings page.
pub fn tabbed_page(
    tabs: Vec<(String, String, bool)>, // (label, href, is_active)
    sections: Vec<FormSection<'_>>,
) -> Markup {
    html! {
        div .page .page--form {
            div .form-page {
                (form_grid(tabs, sections))
            }
        }
    }
}

/// `stats` are rendered tiles — `components::stat_card(..)` calls the page
/// makes itself. The template lays them out in `.stats-grid` and does not
/// describe them: a struct of the four `stat_card` arguments existed only so
/// this function could make that call, which made `stat_card`'s signature the
/// second place a tile's shape was written down.
///
/// `top_card` renders between the stat tiles and `dashboard-grid` — e.g. the
/// admin dashboard's three chart cards. The name is positional relative to
/// `dashboard-grid` (the two-column primary/secondary row below it), not
/// relative to the page as a whole: stats always render first, matching the
/// mockup's stats → charts → tables order.
pub fn dashboard_page(
    stats: Vec<Markup>,
    primary_card: Markup,
    secondary_card: Markup,
    full_width_card: Option<Markup>,
    top_card: Option<Markup>,
) -> Markup {
    html! {
        div .page .page--dashboard {
            @if !stats.is_empty() {
                div .stats-grid {
                    @for s in &stats { (s) }
                }
            }
            @if let Some(tc) = top_card { div .dashboard-top { (tc) } }
            div .dashboard-grid {
                div .dashboard-grid__primary { (primary_card) }
                div .dashboard-grid__secondary { (secondary_card) }
            }
            @if let Some(fw) = full_width_card { div .dashboard-wide { (fw) } }
        }
    }
}

/// The `id` of [`chat_page`]'s scrolling message pane, for a control that has
/// to address it — the messages composer scrolls it after a post.
pub const CHAT_MESSAGES_ID: &str = "chat-messages";

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
                div .chat-messages #(CHAT_MESSAGES_ID) { (messages) }
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
/// shell, no sidebar; mobile-first centered card with a brand + title
/// header, page-specific body, and a sign-out footer.
///
/// The header brand goes through [`brand_lockup`], the same helper every
/// auth card uses — a configured wordmark renders as before, and the blank
/// default renders the icon above the app name instead of nothing.
pub struct AccountCard<'a> {
    pub logo_url: &'a str,
    /// Feeds the same [`brand_lockup`] the auth cards use, so a blank
    /// `logo_url` (the default) falls back to icon + app name rather than
    /// leaving the card unbranded.
    pub logo_icon_url: &'a str,
    pub app_name: &'a str,
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
                    (brand_lockup(opts.logo_url, opts.logo_icon_url, opts.app_name))
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

/// The brand icon. The built-in mark is pixel art (32- and 64-art-pixel
/// renditions) and must only ever be drawn at a whole multiple of its size,
/// so which file is used is decided here, deterministically:
///
/// - `size_px` ≥ 64: the 64-cell file as a plain `<img>` (1:1 on 1x screens,
///   2:1 nearest-neighbour on 2x).
/// - smaller: a `<picture>` — the 32-cell file by default, the 64-cell file
///   from 1.5dppx up via a `min-resolution` media query. Not `srcset` width
///   descriptors: browsers may pick an already-cached larger candidate for
///   those, which then gets nearest-neighbour *down*scaled.
///
/// `.pixel-art` keeps the art-pixels square. A white-labelled icon URL is a
/// smooth logo we know nothing about and is left untouched.
pub fn brand_icon(logo_icon_url: &str, class: &str, size_px: u32) -> Markup {
    let builtin = logo_icon_url == crate::ui::assets::logo_icon_url();
    html! {
        @if builtin && size_px >= 64 {
            img class={ (class) " pixel-art" }
                src=(crate::ui::assets::logo_icon_2x_url())
                width=(size_px) height=(size_px) alt="";
        } @else if builtin {
            picture {
                source media="(min-resolution: 1.5dppx)" srcset=(crate::ui::assets::logo_icon_2x_url());
                img class={ (class) " pixel-art" }
                    src=(logo_icon_url)
                    width=(size_px) height=(size_px) alt="";
            }
        } @else {
            img class=(class) src=(logo_icon_url) width=(size_px) height=(size_px) alt="";
        }
    }
}

/// The brand lockup on auth cards (login, signup, reset, verify, …): a
/// configured wordmark image as before, otherwise the icon at 64px above the
/// app name as text. Blank `logo_url` is the default — there is no built-in
/// raster wordmark (brand text is text, only the art is pixel art).
pub fn brand_lockup(logo_url: &str, logo_icon_url: &str, app_name: &str) -> Markup {
    html! {
        @if !logo_url.is_empty() {
            img .logo-image src=(logo_url) alt=(app_name);
        } @else {
            div .login-brand {
                @if !logo_icon_url.is_empty() {
                    (brand_icon(logo_icon_url, "login-brand__icon", 64))
                }
                span .login-app-name { (app_name) }
            }
        }
    }
}

pub struct BrandPanel<'a> {
    pub logo_html: Option<Markup>,
    pub headline: &'a str,
    pub tagline: Option<&'a str>,
}

/// `form` is the bare form markup — no card wrapper. It sits directly on
/// `.auth-split__form`'s `#fdfdfd` surface; see `layouts/auth-split.css`.
pub fn auth_split(brand: BrandPanel<'_>, form: Markup) -> Markup {
    html! {
        div .auth-split {
            aside .auth-split__brand {
                @if let Some(l) = brand.logo_html { div .auth-split__logo { (l) } }
                // Headline + tagline are grouped in one wrapper so they can be
                // centered vertically as a single unit (see `.auth-split__text`
                // in auth-split.css) regardless of whether a tagline is
                // present -- centering the headline alone (no tagline) needs
                // the same treatment as centering the headline+tagline pair.
                div .auth-split__text {
                    h1 .auth-split__headline { (brand.headline) }
                    @if let Some(t) = brand.tagline { p .auth-split__tagline { (t) } }
                }
            }
            main .auth-split__form { (form) }
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
    // otherwise the defaults from styles/tokens.css apply.
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
                                (crate::ui::icons::arrow_left())
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
                script src=(assets::webmcp_js_url()) defer {}
                @for src in &opts.config.embedded_scripts {
                    script type="module" src=(src) {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::stat_card;

    #[test]
    fn list_page_renders_filters_table_pagination() {
        let filters = Some(html! { form .probe-filters { "search" } });
        let table = html! { div .data-table { table {} } };
        let pagination = Some(html! { nav .pagination { "1/4" } });
        let s = list_page(filters, table, pagination).into_string();
        assert!(s.contains("page--list"));
        assert!(s.contains("page-filters"));
        assert!(s.contains("probe-filters"));
        assert!(s.contains("data-table"));
        assert!(s.contains("page-pagination"));
    }

    #[test]
    fn list_page_omits_optional_sections_when_absent() {
        let table = html! { div .empty { "none" } };
        let s = list_page(None, table, None).into_string();
        assert!(!s.contains("page-filters"));
        assert!(!s.contains("page-pagination"));
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
    fn tabbed_page_marks_the_active_tab() {
        let tabs = vec![
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
        ];
        let sections = vec![FormSection {
            title: "Network",
            description: Some("Outbound requests"),
            body: html! { form action="/b/admin/settings/network" { "..." } },
        }];
        let s = tabbed_page(tabs, sections).into_string();
        assert!(s.contains("form-grid--with-tabs"));
        assert!(s.contains(r#"aria-current="page""#));
        assert!(s.contains("is-active"));
        assert!(s.contains("Network"));
        assert!(s.contains("Outbound requests"));
        // Form-LESS by construction: the tab body owns the only <form>.
        assert_eq!(s.matches("<form").count(), 1);
        assert!(!s.contains(r#"<form class="form-page""#));
    }

    #[test]
    fn dashboard_renders_stats_and_cards() {
        let stats = vec![
            stat_card(
                "Users",
                "142",
                html! { span .probe-icon-users {} },
                Some(html! { span .probe-spark {} }),
            ),
            stat_card(
                "Storage",
                "1.2 GB",
                html! { span .probe-icon-storage {} },
                None,
            ),
        ];
        let primary = html! { section .card { "Quick actions" } };
        let secondary = html! { section .card { "Recent activity" } };
        let s = dashboard_page(stats, primary, secondary, None, None).into_string();
        assert!(s.contains("stats-grid"));
        assert!(s.contains(">Users<"));
        assert!(s.contains("142"));
        assert!(s.contains("probe-icon-users"), "icon must render");
        assert!(s.contains("probe-spark"), "sparkline must render when Some");
        assert!(s.contains("Quick actions"));
        assert!(s.contains("Recent activity"));
        assert!(!s.contains("dashboard-wide"));
    }

    #[test]
    fn dashboard_page_renders_optional_top_card_between_stats_and_grid() {
        let m = dashboard_page(
            vec![stat_card("Users", "1", html! { span .probe-icon {} }, None)],
            html! { div #primary {} },
            html! { div #secondary {} },
            None,
            Some(html! { div #top-card { "QA" } }),
        );
        let s = m.into_string();
        let stats = s.find("stats-grid").expect("stats-grid div present");
        let top = s.find("dashboard-top").expect("dashboard-top div present");
        let grid = s
            .find("dashboard-grid")
            .expect("dashboard-grid div present");
        assert!(stats < top, "stats must render before the mid card");
        assert!(
            top < grid,
            "mid card must render before the primary/secondary grid"
        );
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

    fn account_card(logo_url: &str) -> String {
        account_card_page(
            AccountCard {
                logo_url,
                logo_icon_url: &crate::ui::assets::logo_icon_url(),
                app_name: "Acme",
                title: "Account",
                back_href: None,
            },
            html! { p { "body" } },
        )
        .into_string()
    }

    /// Blank `logo_url` is the default (there is no built-in raster
    /// wordmark), so the account card must fall back to the same icon +
    /// app-name lockup the auth cards use. Rendering an unbranded header
    /// instead is the bug this guards.
    #[test]
    fn account_card_falls_back_to_the_app_name_lockup_without_a_wordmark() {
        let s = account_card("");
        assert!(s.contains("login-brand"), "expected the brand lockup: {s}");
        assert!(s.contains("Acme"), "expected the app name: {s}");
        assert!(
            s.contains("pixel-art"),
            "the built-in mark must keep its nearest-neighbour class: {s}"
        );
    }

    /// A configured wordmark still wins, exactly as on the auth cards.
    #[test]
    fn account_card_prefers_a_configured_wordmark() {
        let s = account_card("https://acme.example/wordmark.png");
        assert!(s.contains("https://acme.example/wordmark.png"));
        assert!(
            !s.contains("login-brand"),
            "a configured wordmark must not also render the text lockup: {s}"
        );
    }

    fn public_site_config() -> SiteConfig {
        SiteConfig {
            app_name: "Acme".to_string(),
            logo_url: String::new(),
            logo_icon_url: String::new(),
            favicon_url: "/favicon.ico".to_string(),
            primary_color: String::new(),
            embedded_scripts: Vec::new(),
            auth_headline: String::new(),
            auth_tagline: String::new(),
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
    fn public_page_includes_the_webmcp_registration_script() {
        // `public_page` is the anonymous-visitor render path (legal pages,
        // marketing, the public storefront demo) — the one place a missing
        // WebMCP tag silently disables tools for exactly the audience that
        // surface targets. See `layout::tests::every_page_includes_the_webmcp_registration_script`
        // for the authenticated-chrome equivalent.
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
        let s = public_page(opts, html! { p { "body" } }).into_string();
        assert!(
            s.contains(&assets::webmcp_js_url()),
            "the WebMCP script must be on every public page: {s}"
        );
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
            auth_headline: String::new(),
            auth_tagline: String::new(),
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
