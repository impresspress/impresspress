//! Shell — renders sidebar (left) + topbar (top of content) + body (the rest).
//! Pages declare `Topbar` inputs; this module owns the chrome.

use maud::{html, Markup};

use super::{
    sidebar::{sidebar_grouped, NavGroup},
    UserInfo,
};

/// One breadcrumb segment.
pub struct Crumb<'a> {
    pub label: &'a str,
    pub href: Option<&'a str>,
}

/// Topbar inputs declared by each page.
pub struct Topbar<'a> {
    pub crumbs: Vec<Crumb<'a>>,
    /// Subtitle shown after the crumbs separated by a vertical bar.
    pub subtitle: Option<&'a str>,
    pub primary_action: Option<Markup>,
    /// Whether to render the ⌘K palette trigger (every shelled page = true).
    pub show_palette: bool,
}

impl<'a> Default for Topbar<'a> {
    fn default() -> Self {
        Self {
            crumbs: Vec::new(),
            subtitle: None,
            primary_action: None,
            show_palette: true,
        }
    }
}

fn render_topbar(t: &Topbar<'_>) -> Markup {
    // Skip rendering entirely when nothing was declared — avoids an empty
    // stripe on pages that don't need a topbar.
    if t.crumbs.is_empty() && t.subtitle.is_none() && t.primary_action.is_none() && !t.show_palette
    {
        return html! {};
    }
    // The current page (last crumb) renders as the page's single `h1` AFTER
    // the breadcrumb nav — an h1 inside a breadcrumb nav is semantically
    // wrong, and the ancestors stay ordinary breadcrumb `li`s.
    let (current, ancestors) = match t.crumbs.split_last() {
        Some((last, rest)) => (Some(last), rest),
        None => (None, &[][..]),
    };
    html! {
        header .topbar {
            @if !ancestors.is_empty() {
                nav .topbar__crumbs aria-label="Breadcrumb" {
                    ol {
                        @for c in ancestors {
                            li {
                                @match c.href {
                                    Some(h) => a href=(h) { (c.label) },
                                    None => span { (c.label) },
                                }
                            }
                        }
                    }
                }
            }
            @if let Some(c) = current {
                h1 .topbar__title { (c.label) }
            }
            @if let Some(s) = t.subtitle {
                span .topbar__sep aria-hidden="true" { "|" }
                span .topbar__subtitle { (s) }
            }
            div .topbar__right {
                @if let Some(a) = &t.primary_action {
                    div .topbar__action { (a.clone()) }
                }
                @if t.show_palette {
                    button .topbar__palette type="button"
                        data-action="palette-open"
                        aria-keyshortcuts="Meta+K Control+K"
                        aria-label="Open command palette" {
                        span { "Quick jump" }
                        kbd {
                            span .topbar__palette-cmd { "⌘" }
                            span { "K" }
                        }
                    }
                }
            }
        }
    }
}

/// Renders sidebar + topbar + body in the standard 12-col grid.
///
/// `nav_groups` partitions the sidebar (Workspace / Data / System for admin,
/// Account / Apps for portal). `user` is pinned at the sidebar bottom.
pub fn shell(
    nav_groups: &[NavGroup],
    user: Option<&UserInfo>,
    current_path: &str,
    logo_url: &str,
    logo_icon_url: &str,
    app_name: &str,
    topbar: Topbar<'_>,
    body: Markup,
) -> Markup {
    html! {
        div .shell {
            a .skip-link href="#content" { "Skip to content" }
            header .shell__mobile-header {
                button .shell__drawer-toggle type="button"
                    data-action="drawer-open"
                    aria-label="Open menu"
                {
                    "☰"
                }
                span .shell__mobile-title { (app_name) }
                @if topbar.show_palette {
                    button .shell__palette-icon type="button"
                        data-action="palette-open"
                        aria-keyshortcuts="Meta+K Control+K"
                        aria-label="Open command palette"
                    {
                        "⌘K"
                    }
                }
            }
            div .shell__overlay data-action="drawer-close" {}
            (sidebar_grouped(nav_groups, user, current_path, logo_url, logo_icon_url, app_name))
            div .shell__main {
                (render_topbar(&topbar))
                div .shell__body #content { (body) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use maud::html;

    use super::*;
    use crate::ui::NavItem;

    /// Wraps a flat `Vec<NavItem>` into one unlabeled `NavGroup` for the shell
    /// render tests below. (Production shells always carry labeled groups via
    /// `nav_groups::{admin,portal}`, so this only exists for the tests.)
    fn one_group(items: Vec<NavItem>) -> Vec<NavGroup> {
        vec![NavGroup { label: None, items }]
    }

    fn item(label: &str, href: &str) -> NavItem {
        NavItem {
            label: label.to_string(),
            href: href.to_string(),
            icon: crate::ui::icons::package,
            external: false,
        }
    }

    #[test]
    fn shell_with_breadcrumb_and_palette_button() {
        let groups = one_group(vec![item("Users", "/b/admin/users")]);
        let topbar = Topbar {
            crumbs: vec![
                Crumb {
                    label: "Workspace",
                    href: Some("/b/admin"),
                },
                Crumb {
                    label: "Users",
                    href: None,
                },
            ],
            primary_action: None,
            subtitle: None,
            show_palette: true,
        };
        let body = html! { p { "page body" } };
        let s = shell(&groups, None, "/b/admin/users", "", "", "Impresspress", topbar, body).into_string();
        assert!(s.contains("topbar__crumbs"));
        assert!(s.contains(">Workspace<"));
        // The current page renders as the h1, after the breadcrumb nav.
        assert!(s.contains(r#"<h1 class="topbar__title">Users</h1>"#));
        assert!(s.contains(r#"data-action="palette-open""#));
        assert!(s.contains("page body"));
    }

    #[test]
    fn shell_renders_exactly_one_h1_and_skip_link() {
        let groups = one_group(vec![item("Users", "/b/admin/users")]);
        let topbar = Topbar {
            crumbs: vec![
                Crumb {
                    label: "Workspace",
                    href: Some("/b/admin"),
                },
                Crumb {
                    label: "Users",
                    href: None,
                },
            ],
            primary_action: None,
            subtitle: None,
            show_palette: true,
        };
        let s = shell(
            &groups,
            None,
            "/b/admin/users",
            "",
            "",
            "Impresspress",
            topbar,
            html! { p { "body" } },
        )
        .into_string();
        assert_eq!(s.matches("<h1").count(), 1, "expected exactly one h1");
        assert!(s.contains(r#"<h1 class="topbar__title">Users</h1>"#));
        // The ancestor crumb stays a breadcrumb link inside the nav…
        assert!(s.contains(r#"<a href="/b/admin">Workspace</a>"#));
        // …and the h1 sits AFTER the nav, not inside it.
        let nav_end = s.find("</nav>").expect("breadcrumb nav present");
        let h1_at = s.find("<h1").expect("h1 present");
        assert!(h1_at > nav_end, "h1 must render after the breadcrumb nav");
        // Skip link is the shell's first focusable element.
        assert!(s.contains(r##"<a class="skip-link" href="#content">Skip to content</a>"##));
        let skip_at = s.find("skip-link").unwrap();
        assert!(
            skip_at < s.find("shell__mobile-header").unwrap(),
            "skip link must come before the rest of the chrome"
        );
    }

    #[test]
    fn shell_single_crumb_renders_h1_without_breadcrumb_nav() {
        let groups = one_group(vec![item("X", "/x")]);
        let tb = Topbar {
            crumbs: vec![Crumb {
                label: "Dashboard",
                href: None,
            }],
            ..Topbar::default()
        };
        let s = shell(&groups, None, "/x", "", "", "Impresspress", tb, html! {}).into_string();
        assert!(s.contains(r#"<h1 class="topbar__title">Dashboard</h1>"#));
        // No ancestors -> no empty breadcrumb nav.
        assert!(!s.contains("topbar__crumbs"));
    }

    /// Regression guard for the double-h1 review finding: pages that render
    /// `components::page_header(...)` in their body (products, llm,
    /// legalpages/auth_ui settings, userportal branding, ...) must not add a
    /// second h1 next to the topbar's — the body header is an h2.
    #[test]
    fn shell_page_with_body_page_header_still_has_exactly_one_h1() {
        let groups = one_group(vec![item("Products", "/b/products/")]);
        let tb = Topbar {
            crumbs: vec![Crumb {
                label: "Products",
                href: None,
            }],
            ..Topbar::default()
        };
        let body = crate::ui::components::page_header(
            "Products Overview",
            Some("Product catalog statistics"),
            None,
        );
        let s = shell(&groups, None, "/b/products/", "", "", "Impresspress", tb, body).into_string();
        assert_eq!(
            s.matches("<h1").count(),
            1,
            "the shell topbar owns the only h1; body page_header must be h2: {s}"
        );
        assert!(s.contains(r#"<h1 class="topbar__title">Products</h1>"#));
        assert!(s.contains(r#"<h2 class="page-title">Products Overview</h2>"#));
    }

    #[test]
    fn shell_can_omit_palette() {
        let groups = one_group(vec![item("X", "/x")]);
        // Need at least one declared input to render the topbar at all.
        let tb = Topbar {
            show_palette: false,
            crumbs: vec![Crumb {
                label: "X",
                href: None,
            }],
            ..Default::default()
        };
        let s = shell(&groups, None, "/x", "", "", "Impresspress", tb, html! {}).into_string();
        assert!(!s.contains("topbar__palette"));
        // The single crumb renders as the page h1 (no ancestor nav needed).
        assert!(s.contains(r#"<h1 class="topbar__title">X</h1>"#));
    }

    #[test]
    fn shell_renders_mobile_header_with_drawer_toggle() {
        let groups = one_group(vec![item("X", "/x")]);
        let s = shell(
            &groups,
            None,
            "/x",
            "",
            "",
            "Impresspress",
            Topbar::default(),
            html! { "body" },
        )
        .into_string();
        assert!(s.contains("shell__mobile-header"), "missing mobile header");
        assert!(
            s.contains(r#"data-action="drawer-open""#),
            "missing drawer toggle"
        );
        assert!(
            s.contains(r#"data-action="drawer-close""#),
            "missing drawer overlay"
        );
        assert!(s.contains("shell__overlay"), "missing overlay element");
    }

    #[test]
    fn shell_mobile_header_omits_palette_icon_when_disabled() {
        let groups = one_group(vec![item("X", "/x")]);
        let tb = Topbar {
            show_palette: false,
            ..Default::default()
        };
        let s = shell(&groups, None, "/x", "", "", "Impresspress", tb, html! {}).into_string();
        // Mobile header itself is always rendered…
        assert!(s.contains("shell__mobile-header"));
        // …but the ⌘K icon-button inside it isn't, when the page disables the palette.
        assert!(!s.contains("shell__palette-icon"));
    }

    #[test]
    fn shell_renders_no_topbar_when_all_inputs_empty() {
        let groups = one_group(vec![item("X", "/x")]);
        let tb = Topbar {
            show_palette: false,
            ..Default::default()
        };
        let s = shell(&groups, None, "/x", "", "", "Impresspress", tb, html! { "body" }).into_string();
        // No topbar element at all when there's nothing to render in it.
        assert!(!s.contains(r#"class="topbar""#));
        assert!(s.contains(">body<") || s.contains(">body</"));
    }
}
