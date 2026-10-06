//! Shell — renders sidebar (left) + topbar (top of content) + body (the rest).
//! Pages declare `Topbar` inputs; this module owns the chrome.

use maud::{html, Markup};

use super::{
    icons,
    sidebar::{sidebar_grouped, NavGroup, SignedIn},
    NavKind,
};

/// One breadcrumb segment.
pub struct Crumb<'a> {
    pub label: &'a str,
    pub href: Option<&'a str>,
}

/// The page header every shelled page declares. The topbar is the page's
/// header: blocks put their title, trail, description and page-level actions
/// here and render NO header of their own in the body (in-body headings are
/// `components::section_header`, an `h2`).
///
/// - **Title and trail** — `crumbs` is the full trail, and its LAST entry is
///   the current page: it renders as the page's one and only `h1`, styled as
///   a title. Earlier entries are the ancestors (a detail page passes
///   `[Products → /b/products/admin/manage, Widget]`, giving
///   "Products ›" above the "Widget" title); they render as a breadcrumb
///   `nav` above the title. A top-level page passes one crumb.
/// - **Subtitle** — one short sentence describing the page, on its own line
///   under the title. Hidden below 720px, so it must never carry anything
///   the page needs to be usable.
/// - **Actions** — the page's own buttons (create, upload, refresh, a status
///   badge), rendered right-aligned in order, so put the primary action LAST.
///   They wrap onto their own row below 720px, so any number fits.
/// - **Palette** — the ⌘K / Ctrl+K command-palette trigger.
pub struct Topbar<'a> {
    /// The trail; the last crumb is the page title (the `h1`).
    pub crumbs: Vec<Crumb<'a>>,
    /// One-line page description under the title (hidden below 720px).
    pub subtitle: Option<&'a str>,
    /// Page-level actions, left to right — primary action last.
    pub actions: Vec<Markup>,
    /// Whether to render the ⌘K palette trigger (every shelled page = true).
    pub show_palette: bool,
}

impl<'a> Default for Topbar<'a> {
    fn default() -> Self {
        Self {
            crumbs: Vec::new(),
            subtitle: None,
            actions: Vec::new(),
            show_palette: true,
        }
    }
}

/// How the shell's content region (`main.shell__body`) frames the page body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BodyLayout {
    /// The standard page: the body sits inside the content card's padding.
    #[default]
    Padded,
    /// A full-bleed layout that draws its own panes edge to edge (the chat
    /// template's thread list / messages / rail), so the card adds no padding.
    Flush,
}

fn render_topbar(t: &Topbar<'_>) -> Markup {
    // Skip rendering entirely when nothing was declared — avoids an empty
    // stripe on pages that don't need a topbar.
    if t.crumbs.is_empty() && t.subtitle.is_none() && t.actions.is_empty() && !t.show_palette {
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
            div .topbar__heading {
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
                    p .topbar__subtitle { (s) }
                }
            }
            @if !t.actions.is_empty() {
                div .topbar__actions {
                    @for a in &t.actions { (a.clone()) }
                }
            }
            @if t.show_palette {
                // The visible label is platform-neutral; the platform's
                // shortcut is in the `kbd` (swapped to Ctrl off-Mac by
                // chrome.js) and in `aria-keyshortcuts`.
                button .topbar__palette type="button"
                    data-action="palette-open"
                    aria-keyshortcuts="Meta+K Control+K"
                    aria-label="Search pages (command palette)" {
                    (icons::search())
                    span { "Search" }
                    kbd aria-hidden="true" {
                        span .topbar__palette-cmd { "⌘" }
                        span { "K" }
                    }
                }
            }
        }
    }
}

/// Renders sidebar + topbar + body in the standard 12-col grid.
///
/// `nav_groups` partitions the sidebar (Workspace / Data / System for admin,
/// Account / Apps for portal). `signed_in` (the viewer and their profile menu) is pinned at the sidebar bottom. The
/// body renders inside `main#content` — the page's one `main` landmark and
/// the skip link's target.
#[expect(
    clippy::too_many_arguments,
    reason = "every argument is an independent slot in the page chrome; a struct \
              would be the same list with a name on it"
)]
pub fn shell(
    nav_kind: NavKind,
    nav_groups: &[NavGroup],
    signed_in: Option<SignedIn<'_>>,
    current_path: &str,
    logo_url: &str,
    logo_icon_url: &str,
    app_name: &str,
    topbar: Topbar<'_>,
    body_layout: BodyLayout,
    subnav: Option<Markup>,
    body: Markup,
) -> Markup {
    let flush = body_layout == BodyLayout::Flush;
    html! {
        div .shell {
            a .skip-link href="#content" { "Skip to content" }
            // The phone's way into the navigation: the drawer toggle that
            // opens the Primary nav, and the page search. A `nav`, not a
            // `header` — the topbar is the page's one banner, and below
            // 720px both are on screen; and not a bare `div`, which would
            // leave the app name outside every landmark.
            nav .shell__mobile-header aria-label="Site" {
                button .shell__drawer-toggle type="button"
                    data-action="drawer-open"
                    aria-label="Open menu"
                {
                    (icons::menu())
                }
                span .shell__mobile-title { (app_name) }
                @if topbar.show_palette {
                    button .shell__palette-icon type="button"
                        data-action="palette-open"
                        aria-keyshortcuts="Meta+K Control+K"
                        aria-label="Search pages (command palette)"
                    {
                        (icons::search())
                    }
                }
            }
            div .shell__overlay data-action="drawer-close" {}
            (sidebar_grouped(nav_kind, nav_groups, signed_in, current_path, logo_url, logo_icon_url, app_name))
            div .shell__main {
                (render_topbar(&topbar))
                // A block's own sections (`PageBody::with_subnav`): a row of
                // links between the page header and the content card, so it
                // frames a full-bleed body as well as a padded one.
                @if let Some(subnav) = subnav { (subnav) }
                // The page scrolls inside this element, not the document, so
                // it is a Tab stop (`tabindex="0"`): a keyboard user can
                // focus it and scroll a page with nothing focusable in it.
                // It is also the skip link's target.
                main .shell__body .shell__body--flush[flush] #content tabindex="0" { (body) }
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
            block: None,
            section: None,
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
            actions: Vec::new(),
            subtitle: None,
            show_palette: true,
        };
        let body = html! { p { "page body" } };
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/b/admin/users",
            "",
            "",
            "Impresspress",
            topbar,
            BodyLayout::Padded,
            None,
            body,
        )
        .into_string();
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
            actions: Vec::new(),
            subtitle: None,
            show_palette: true,
        };
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/b/admin/users",
            "",
            "",
            "Impresspress",
            topbar,
            BodyLayout::Padded,
            None,
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
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/x",
            "",
            "",
            "Impresspress",
            tb,
            BodyLayout::Padded,
            None,
            html! {},
        )
        .into_string();
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
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/b/products/",
            "",
            "",
            "Impresspress",
            tb,
            BodyLayout::Padded,
            None,
            body,
        )
        .into_string();
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
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/x",
            "",
            "",
            "Impresspress",
            tb,
            BodyLayout::Padded,
            None,
            html! {},
        )
        .into_string();
        assert!(!s.contains("topbar__palette"));
        // The single crumb renders as the page h1 (no ancestor nav needed).
        assert!(s.contains(r#"<h1 class="topbar__title">X</h1>"#));
    }

    #[test]
    fn shell_renders_mobile_header_with_drawer_toggle() {
        let groups = one_group(vec![item("X", "/x")]);
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/x",
            "",
            "",
            "Impresspress",
            Topbar::default(),
            BodyLayout::Padded,
            None,
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

    /// The topbar is the page's only banner landmark. The mobile header is
    /// on screen with it below 720px, so it must not be a `header` (an
    /// unscoped `header` IS a banner: axe's `landmark-no-duplicate-banner`).
    /// It is a labelled `nav`, so its contents still sit in a landmark
    /// (axe's `region`), and its two controls stay named buttons.
    #[test]
    fn shell_has_exactly_one_banner_landmark() {
        let groups = one_group(vec![item("X", "/x")]);
        let tb = Topbar {
            crumbs: vec![Crumb {
                label: "X",
                href: None,
            }],
            ..Topbar::default()
        };
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/x",
            "",
            "",
            "Impresspress",
            tb,
            BodyLayout::Padded,
            None,
            html! { "body" },
        )
        .into_string();
        assert_eq!(s.matches("<header").count(), 1, "one banner: {s}");
        assert!(s.contains(r#"<header class="topbar">"#));
        assert!(!s.contains(r#"role="banner""#));
        assert!(s.contains(r#"<nav class="shell__mobile-header" aria-label="Site">"#));
        // Distinct from the sidebar's nav, so the two landmarks are told apart.
        assert!(s.contains(r#"aria-label="Primary""#));
        assert!(s.contains(r#"data-action="drawer-open" aria-label="Open menu""#));
        assert!(s.contains(
            r#"class="shell__palette-icon" type="button" data-action="palette-open" aria-keyshortcuts="Meta+K Control+K" aria-label="Search pages (command palette)""#
        ));
    }

    #[test]
    fn shell_mobile_header_omits_palette_icon_when_disabled() {
        let groups = one_group(vec![item("X", "/x")]);
        let tb = Topbar {
            show_palette: false,
            ..Default::default()
        };
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/x",
            "",
            "",
            "Impresspress",
            tb,
            BodyLayout::Padded,
            None,
            html! {},
        )
        .into_string();
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
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/x",
            "",
            "",
            "Impresspress",
            tb,
            BodyLayout::Padded,
            None,
            html! { "body" },
        )
        .into_string();
        // No topbar element at all when there's nothing to render in it.
        assert!(!s.contains(r#"class="topbar""#));
        assert!(s.contains(">body<") || s.contains(">body</"));
    }

    /// The body is the page's one `main` landmark and the skip link's
    /// target; a flush body (chat) drops only the padding modifier.
    #[test]
    fn body_renders_as_the_main_landmark() {
        let groups = one_group(vec![item("X", "/x")]);
        let render = |layout| {
            shell(
                NavKind::Admin,
                &groups,
                None,
                "/x",
                "",
                "",
                "Impresspress",
                Topbar::default(),
                layout,
                None,
                html! { "body" },
            )
            .into_string()
        };
        let padded = render(BodyLayout::Padded);
        assert_eq!(padded.matches("<main").count(), 1);
        assert!(padded.contains(r#"<main class="shell__body" id="content" tabindex="0">"#));
        let flush = render(BodyLayout::Flush);
        assert!(flush.contains(r#"<main class="shell__body shell__body--flush" id="content""#));
    }

    /// The subtitle sits on its own line under the title (no "|" separator)
    /// and every declared action renders, in order, in the actions slot.
    #[test]
    fn topbar_renders_subtitle_line_and_every_action_in_order() {
        let groups = one_group(vec![item("X", "/x")]);
        let tb = Topbar {
            crumbs: vec![
                Crumb {
                    label: "Products",
                    href: Some("/b/products/admin/manage"),
                },
                Crumb {
                    label: "Widget",
                    href: None,
                },
            ],
            subtitle: Some("Edit the product"),
            actions: vec![html! { a { "Preview" } }, html! { button { "Publish" } }],
            show_palette: true,
        };
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/x",
            "",
            "",
            "Impresspress",
            tb,
            BodyLayout::Padded,
            None,
            html! {},
        )
        .into_string();
        assert!(s.contains(r#"<h1 class="topbar__title">Widget</h1><p class="topbar__subtitle">Edit the product</p>"#));
        assert!(!s.contains("topbar__sep"));
        assert!(s.contains(
            r#"<div class="topbar__actions"><a>Preview</a><button>Publish</button></div>"#
        ));
        // The palette trigger is labelled by a platform-neutral name.
        assert!(s.contains(r#"aria-label="Search pages (command palette)""#));
        assert!(!s.contains("Ctrl K"));
    }
}
