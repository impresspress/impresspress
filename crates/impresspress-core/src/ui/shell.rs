//! Shell — renders sidebar (left) + topbar (top of content) + body (the rest).
//! Pages declare `Topbar` inputs; this module owns the chrome.

use maud::{html, Markup};

use super::{
    components::{HeaderAction, HeaderStatus},
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
/// - **Status and actions** — typed slots, rendered right-aligned on the
///   title's row: the `status` badges (`.topbar__status`), then the actions
///   (`.topbar__actions`): the `secondary_actions`, then the
///   `primary_action` last. A [`HeaderAction`] has no emphasis or size of its
///   own (the slot gives it both), and a [`HeaderStatus`] is only ever a
///   badge, so a second header primary, a primary that is not last, a quiet,
///   destructive or small header button, and a badge posing as an action
///   cannot be written. Below 720px the actions wrap onto a row of their own,
///   so any number fits, while the status stays on the title's row: it
///   describes the page's subject, and among the buttons it would read as
///   one of them.
/// - **Palette** — the ⌘K / Ctrl+K command-palette trigger.
pub struct Topbar<'a> {
    /// The trail; the last crumb is the page title (the `h1`).
    pub crumbs: Vec<Crumb<'a>>,
    /// One-line page description under the title (hidden below 720px).
    pub subtitle: Option<&'a str>,
    /// The page subject's state, left to right, before the actions.
    pub status: Vec<HeaderStatus>,
    /// The page's other actions, left to right, rendered as secondaries.
    pub secondary_actions: Vec<HeaderAction<'a>>,
    /// The page's one primary action, rendered last.
    pub primary_action: Option<HeaderAction<'a>>,
    /// Whether to render the ⌘K palette trigger (every shelled page = true).
    pub show_palette: bool,
}

impl<'a> Default for Topbar<'a> {
    fn default() -> Self {
        Self {
            crumbs: Vec::new(),
            subtitle: None,
            status: Vec::new(),
            secondary_actions: Vec::new(),
            primary_action: None,
            show_palette: true,
        }
    }
}

impl Topbar<'_> {
    /// The `header.topbar`; empty when the page declared nothing for it.
    pub(crate) fn render(self) -> Markup {
        let t = self;
        // Skip rendering entirely when nothing was declared — avoids an empty
        // stripe on pages that don't need a topbar.
        let has_actions = !t.secondary_actions.is_empty() || t.primary_action.is_some();
        if t.crumbs.is_empty()
            && t.subtitle.is_none()
            && t.status.is_empty()
            && !has_actions
            && !t.show_palette
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
                @if !t.status.is_empty() {
                    div .topbar__status {
                        @for s in t.status { (s.render()) }
                    }
                }
                @if has_actions {
                    div .topbar__actions {
                        @for a in t.secondary_actions { (a.render_secondary()) }
                        @if let Some(a) = t.primary_action { (a.render_primary()) }
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
}

/// Renders sidebar + topbar + body in the standard 12-col grid.
///
/// `nav_groups` partitions the sidebar (Workspace / Data / System for admin,
/// Account / Apps for portal). `signed_in` (the viewer and their profile menu) is pinned at the sidebar bottom. The
/// body renders inside `main#content` — the page's one `main` landmark and
/// the skip link's target.
///
/// The shell draws no frame of its own: the header and the body sit on the
/// page background (`--surface-2`), so every card-level surface a page draws
/// (a `components::Card`, a stat, an empty state, a table, the chat panes)
/// is the first level of card, never a card inside the shell's.
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
    subnav: Option<Markup>,
    body: Markup,
) -> Markup {
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
                (topbar.render())
                // A block's own sections (`PageBody::with_subnav`): a row of
                // links between the page header and the body.
                @if let Some(subnav) = subnav { (subnav) }
                // The page scrolls inside this element, not the document, so
                // it is a Tab stop (`tabindex="0"`): a keyboard user can
                // focus it and scroll a page with nothing focusable in it.
                // It is also the skip link's target.
                main .shell__body #content tabindex="0" { (body) }
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
            ..Topbar::default()
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
            ..Topbar::default()
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
            None,
            html! {},
        )
        .into_string();
        assert!(s.contains(r#"<h1 class="topbar__title">Dashboard</h1>"#));
        // No ancestors -> no empty breadcrumb nav.
        assert!(!s.contains("topbar__crumbs"));
    }

    /// Regression guard for the double-h1 review finding: a page's body
    /// sections are `components::Card`s, and a card's title must not add a
    /// second h1 next to the topbar's. Read off the element tree: the outline
    /// is the topbar's h1, then each card's h2 under it.
    #[test]
    fn shell_page_with_body_cards_still_has_exactly_one_h1() {
        let groups = one_group(vec![item("Products", "/b/products/")]);
        let tb = Topbar {
            crumbs: vec![Crumb {
                label: "Products",
                href: None,
            }],
            ..Topbar::default()
        };
        let body = html! {
            (crate::ui::components::Card::titled("Overview", html! { p { "Totals" } }).render())
            (crate::ui::components::Card::plain(html! { p { "Notes" } }).render())
        };
        let s = shell(
            NavKind::Admin,
            &groups,
            None,
            "/b/products/",
            "",
            "",
            "Impresspress",
            tb,
            None,
            body,
        )
        .into_string();
        assert_eq!(
            crate::test_support::dom::heading_outline(&s),
            vec![(1, "Products".to_string()), (2, "Overview".to_string())],
            "the shell topbar owns the only h1; a card title is an h2: {s}"
        );
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
            None,
            html! { "body" },
        )
        .into_string();
        // No topbar element at all when there's nothing to render in it.
        assert!(!s.contains(r#"class="topbar""#));
        assert!(s.contains(">body<") || s.contains(">body</"));
    }

    /// The body is the page's one `main` landmark and the skip link's
    /// target, and it is one element whatever the page: the shell has no
    /// per-page frame to choose.
    #[test]
    fn body_renders_as_the_main_landmark() {
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
            None,
            html! { "body" },
        )
        .into_string();
        assert_eq!(s.matches("<main").count(), 1);
        assert!(
            s.contains(r#"<main class="shell__body" id="content" tabindex="0">body</main>"#),
            "{s}"
        );
    }

    /// The subtitle sits on its own line under the title (no "|" separator),
    /// the status badges come first, in their own slot, and the actions row
    /// renders the secondaries, then the one primary, last. Each action
    /// is a full-size `.btn`, whatever the slot: the header has no small
    /// button to write.
    #[test]
    fn topbar_renders_subtitle_line_then_status_secondaries_and_the_primary() {
        use crate::ui::components::{ActionTarget, Badge, BadgeVariant};
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
            status: vec![
                HeaderStatus::badge(Badge::new(BadgeVariant::Info), "Live"),
                HeaderStatus::status("pending"),
            ],
            secondary_actions: vec![
                HeaderAction::new("Preview", ActionTarget::Link("/preview")),
                HeaderAction::new("Duplicate", ActionTarget::Delegated("dup")).icon(icons::plus()),
            ],
            primary_action: Some(HeaderAction::new(
                "Publish",
                ActionTarget::Submit { form: None },
            )),
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
            None,
            html! {},
        )
        .into_string();
        assert!(s.contains(r#"<h1 class="topbar__title">Widget</h1><p class="topbar__subtitle">Edit the product</p>"#));
        assert!(!s.contains("topbar__sep"));
        let plus = icons::plus().into_string();
        assert!(
            s.contains(&format!(
                concat!(
                    r#"<div class="topbar__status">"#,
                    r#"<span class="badge badge-info">Live</span>"#,
                    r#"<span class="badge badge-warning">Pending</span>"#,
                    r#"</div><div class="topbar__actions">"#,
                    r#"<a class="btn btn--secondary" href="/preview">Preview</a>"#,
                    r#"<button class="btn btn--secondary" type="button" data-action="dup">{plus}Duplicate</button>"#,
                    r#"<button class="btn btn--primary" type="submit">Publish</button>"#,
                    r#"</div>"#,
                ),
                plus = plus
            )),
            "{s}"
        );
        assert!(!s.contains("btn--sm"), "no small header button: {s}");
        // The palette trigger is labelled by a platform-neutral name.
        assert!(s.contains(r#"aria-label="Search pages (command palette)""#));
        assert!(!s.contains("Ctrl K"));
    }

    /// A topbar that declares only a status renders the status slot and no
    /// actions row (the Database page's backend badge); one that declares
    /// nothing has neither.
    #[test]
    fn each_slot_renders_only_when_declared() {
        let only_status = Topbar {
            status: vec![HeaderStatus::status("active")],
            show_palette: false,
            ..Topbar::default()
        }
        .render()
        .into_string();
        assert_eq!(
            only_status,
            r#"<header class="topbar"><div class="topbar__heading"></div><div class="topbar__status"><span class="badge badge-success">Active</span></div></header>"#,
        );
        let nothing = Topbar {
            crumbs: vec![Crumb {
                label: "X",
                href: None,
            }],
            ..Topbar::default()
        }
        .render()
        .into_string();
        assert!(!nothing.contains("topbar__actions"), "{nothing}");
        assert!(!nothing.contains("topbar__status"), "{nothing}");
    }
}
