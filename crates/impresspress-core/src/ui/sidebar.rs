//! Sidebar component — grouped navigation with brand, groups, and user profile.

use maud::Markup;

use super::{icons, NavItem};

/// Resolve a *user-supplied* icon-name string (stored in the DB by the
/// userportal admin-button editor, chosen from a fixed `ICON_OPTIONS`
/// dropdown) to its icon markup. The compile-time sidebar nav no longer
/// goes through here — `NavItem.icon` is a typed `fn() -> Markup`, so a
/// misspelled icon in `nav_groups.rs` is a build error rather than a silent
/// fallback. This resolver survives only for the genuinely-dynamic case
/// where the name comes from user input at runtime.
///
/// Unknown names render the visibly distinct [`icons::help_circle`] glyph
/// (plus a warn log) rather than silently masquerading as the package icon —
/// a stored name that no arm matches is a data/`ICON_OPTIONS` drift that
/// should be seen, not hidden. The `ICON_OPTIONS`-coverage test below keeps
/// every dropdown-selectable name resolving to a real arm.
pub fn nav_icon(name: &str) -> Markup {
    match name {
        "layout-dashboard" | "dashboard" => icons::layout_dashboard(),
        "users" => icons::users(),
        "shield" => icons::shield(),
        "key" => icons::key(),
        "settings" => icons::settings(),
        "file-text" | "logs" => icons::file_text(),
        "package" | "products" => icons::package(),
        "shopping-cart" => icons::shopping_cart(),
        "server" => icons::server(),
        "folder" | "files" => icons::folder(),
        "user" | "account" => icons::user(),
        "globe" => icons::globe(),
        "robot" | "bot" => icons::robot(),
        "network" => icons::network(),
        "hard-drive" | "storage" => icons::hard_drive(),
        "bar-chart" | "stats" => icons::bar_chart(),
        "dollar-sign" => icons::dollar_sign(),
        "link" => icons::link(),
        _ => {
            tracing::warn!("unknown nav icon name: {name}");
            icons::help_circle()
        }
    }
}

/// A group of nav items rendered with an optional uppercase label.
pub struct NavGroup {
    pub label: Option<String>,
    pub items: Vec<NavItem>,
}

/// The one item to highlight for `path`: the item whose claim on it is the
/// most specific, so at most one item is ever marked.
///
/// An item claims `path` when `path` is
///
/// - its `href`,
/// - below its `href`, for an `href` without a trailing slash (`/b/admin/users`
///   claims `/b/admin/users/{id}`; `/b/admin/` does not claim every admin
///   page), or
/// - inside its declared [`NavItem::section`] (`/b/admin/settings` claims
///   `/b/admin/settings/network`).
///
/// The claim's strength is the length of the prefix that matched, so a
/// section nested inside another item's (Storage's `/b/storage/admin` inside
/// Files' `/b/storage`) wins over it. On a tie the first item in nav order
/// wins.
pub fn active_item<'a>(groups: &'a [NavGroup], path: &str) -> Option<&'a NavItem> {
    let under = |prefix: &str| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    let claim = |item: &NavItem| -> Option<usize> {
        let by_href = (path == item.href || (!item.href.ends_with('/') && under(&item.href)))
            .then_some(item.href.len());
        let by_section = item.section.filter(|s| under(s)).map(str::len);
        by_href.max(by_section)
    };
    let mut best: Option<(usize, &NavItem)> = None;
    for item in groups.iter().flat_map(|g| &g.items) {
        if let Some(strength) = claim(item) {
            if best.is_none_or(|(b, _)| strength > b) {
                best = Some((strength, item));
            }
        }
    }
    best.map(|(_, item)| item)
}

/// Grouped sidebar — same layout as `sidebar(...)`, but items are
/// partitioned into labeled groups. The brand at top, the user pinned
/// at bottom (when `user` is `Some`).
/// `logo_url` (the header/email wordmark, `WAFER_RUN_SHARED__LOGO_URL`) is
/// accepted but intentionally unused here: the navy sidebar always renders
/// the icon + white text brand, never an `<img>` of that wordmark PNG (see
/// the `.sidebar__brand` block below). The parameter is kept so this
/// signature still matches `shell()`'s, which threads the same `SiteConfig`
/// fields through to every chrome surface.
///
/// `.sidebar__brand--text` is unconditional (not toggled on `logo_url`, as
/// origin/main's equivalent does for its own wordmark-image branch): this
/// branch never has a second, wordmark-image code path for the CSS modifier
/// to switch away from, so applying it unconditionally is the same outcome
/// with one fewer state. `logo_icon_2x_url`/`brand_icon` (the retina pixel-art
/// icon) are ported from origin/main during the main merge — 2026-09-02.
pub fn sidebar_grouped(
    groups: &[NavGroup],
    user: Option<&crate::ui::UserInfo>,
    current_path: &str,
    _logo_url: &str,
    logo_icon_url: &str,
    app_name: &str,
) -> maud::Markup {
    use maud::html;

    let active = active_item(groups, current_path);
    html! {
        nav .sidebar aria-label="Primary" {
            div .sidebar__brand .sidebar__brand--text {
                @if !logo_icon_url.is_empty() {
                    (crate::ui::templates::brand_icon(logo_icon_url, "sidebar__brand-icon", 32))
                }
                // The navy sidebar always shows the white text wordmark, never
                // an `<img>` of `logo_url` — that PNG is dark-ink artwork drawn
                // for the old white sidebar and is illegible on navy (task-11
                // review finding). `logo_url` (header/email wordmark) is a
                // different surface's concern; the sidebar only ever needs the
                // square icon (`logo_icon_url`) plus this text.
                span .sidebar__brand-name { (app_name) }
            }
            div .sidebar__panel {
                div .sidebar__groups {
                    @for g in groups {
                        div .sidebar__group {
                            @if let Some(l) = &g.label {
                                div .sidebar__group-label { (l) }
                            }
                            ul .sidebar__nav {
                                @for item in &g.items {
                                    @let active = active.is_some_and(|a| std::ptr::eq(a, item));
                                    li {
                                        a href=(item.href)
                                          class={ "sidebar__nav-item" @if active { " is-active" } }
                                          aria-current=[active.then_some("page")]
                                          target=[item.external.then_some("_blank")]
                                          rel=[item.external.then_some("noopener noreferrer")] {
                                            span .sidebar__nav-icon {
                                                ((item.icon)())
                                            }
                                            span .sidebar__nav-label { (item.label) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                button .sidebar__collapse-toggle id="sidebar-collapse-btn" type="button" data-action="sidebar-collapse" aria-label="Toggle sidebar" {
                    span .sidebar__collapse-icon-expanded { (icons::chevron_left()) }
                    span .sidebar__collapse-icon-collapsed { (icons::chevron_right()) }
                }
            }
            @if let Some(u) = user {
                div .sidebar__user-container {
                    button .sidebar__user id="user-menu-btn" type="button" data-action="profile-menu-toggle" {
                        (crate::ui::components::avatar(&u.email, crate::ui::components::CtrlSize::Sm))
                        div .sidebar__user-text {
                            div .sidebar__user-email { (u.email) }
                            div .sidebar__user-role {
                                @if u.is_admin() { "Admin" } @else { "User" }
                            }
                        }
                    }
                    div .profile-menu #profile-menu hidden {
                        div .profile-menu-header {
                            div .profile-menu-avatar { (u.avatar_initial()) }
                            div .profile-menu-info {
                                div .profile-menu-email { (u.email) }
                                div .profile-menu-role { (u.roles.join(", ")) }
                            }
                        }
                        div .profile-menu-divider {}
                        a .profile-menu-item href="/b/userportal/" {
                            (icons::user())
                            span { "My Account" }
                        }
                        a .profile-menu-item href="/b/auth/change-password" {
                            (icons::settings())
                            span { "Change Password" }
                        }
                        div .profile-menu-divider {}
                        form action="/b/auth/api/logout" method="post" {
                            button .profile-menu-item .profile-menu-item-danger type="submit" {
                                (icons::log_out())
                                span { "Sign Out" }
                            }
                        }
                    }
                }
            }
        }
        // The two controls above declare `data-action` and this one delegated
        // listener reads it — the rule is written out in `ui/assets/chrome.js`,
        // and the verbs `sidebar-collapse` and `profile-menu-toggle` belong to
        // this file. The outside-click branch that closes the profile menu was
        // already delegated; it now shares the listener rather than adding a
        // second one.
        script { (maud::PreEscaped(r#"
(function() {
    if (window.__sidebarInit) return;
    window.__sidebarInit = true;
    document.addEventListener('click', function(e) {
        var t = e.target;
        if (!(t instanceof Element)) return;
        var el = t.closest('[data-action]');
        var action = el ? el.getAttribute('data-action') : null;
        if (action === 'profile-menu-toggle') {
            var menu = document.getElementById('profile-menu');
            if (menu) menu.hidden = !menu.hidden;
            return;
        }
        if (action === 'sidebar-collapse') {
            var s = document.querySelector('.sidebar');
            if (s) {
                s.classList.toggle('collapsed');
                try { localStorage.setItem('sidebar.collapsed', s.classList.contains('collapsed') ? '1' : '0'); } catch (err) {}
            }
            // Deliberately no `return`: the collapse toggle is outside both the
            // profile button and the profile menu, so under the two separate
            // listeners this replaced it also dismissed an open profile menu.
            // Falling through to the outside-click branch keeps that.
        }
        var m = document.getElementById('profile-menu');
        var b = document.getElementById('user-menu-btn');
        if (m && b && !b.contains(t) && !m.contains(t)) {
            m.hidden = true;
        }
    });
    try {
        if (localStorage.getItem('sidebar.collapsed') === '1') {
            var s = document.querySelector('.sidebar');
            if (s) s.classList.add('collapsed');
        }
    } catch (err) {}
})();
"#)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::NavItem;

    fn item(label: &str, href: &str) -> NavItem {
        NavItem {
            label: label.to_string(),
            href: href.to_string(),
            icon: icons::package,
            external: false,
            block: None,
            section: None,
        }
    }

    #[test]
    fn grouped_sidebar_renders_labels_and_groups() {
        let groups = vec![
            NavGroup {
                label: Some("Workspace".to_string()),
                items: vec![item("Users", "/b/admin/users")],
            },
            NavGroup {
                label: Some("Data".to_string()),
                items: vec![item("Blocks", "/b/admin/blocks")],
            },
        ];
        let s =
            sidebar_grouped(&groups, None, "/b/admin/users", "", "", "Impresspress").into_string();
        assert!(s.contains(">Workspace<"));
        assert!(s.contains(">Data<"));
        assert!(s.contains("/b/admin/users"));
        assert!(s.contains(r#"aria-current="page""#));
    }

    /// Default branding: no wordmark image, so the brand row is the built-in
    /// pixel-art icon next to the app name. The icon is a `<picture>`: the
    /// 32-cell file by default and the 64-cell file from 1.5dppx up, chosen
    /// by a media query (deterministic — `srcset` width descriptors let the
    /// browser pick an already-cached larger candidate, which then gets
    /// nearest-neighbour *down*scaled). `.pixel-art` keeps art-pixels square.
    #[test]
    fn brand_without_wordmark_renders_pixel_art_icon_and_app_name() {
        let s = sidebar_grouped(
            &[],
            None,
            "/",
            "",
            &crate::ui::assets::logo_icon_url(),
            "Impresspress",
        )
        .into_string();
        assert!(s.contains("sidebar__brand--text"), "{s}");
        assert!(
            s.contains(&format!(
                r#"<picture><source media="(min-resolution: 1.5dppx)" srcset="{}"><img class="sidebar__brand-icon pixel-art" src="{}" width="32" height="32" alt=""></picture>"#,
                crate::ui::assets::logo_icon_2x_url(),
                crate::ui::assets::logo_icon_url()
            )),
            "{s}"
        );
        assert!(!s.contains("sizes="), "{s}");
        assert!(
            s.contains(r#"<span class="sidebar__brand-name">Impresspress</span>"#),
            "{s}"
        );
    }

    /// A white-labelled icon URL is a smooth logo we know nothing about:
    /// no nearest-neighbour class, no built-in srcset.
    #[test]
    fn custom_icon_url_gets_no_pixel_art_treatment() {
        let s =
            sidebar_grouped(&[], None, "/", "", "https://acme.test/mark.png", "Acme").into_string();
        assert!(s.contains(r#"src="https://acme.test/mark.png""#), "{s}");
        assert!(!s.contains("pixel-art"), "{s}");
        assert!(!s.contains("<picture>"), "{s}");
    }

    // Ported from origin/main's `wordmark_replaces_the_text_lockup` during the
    // main merge -- 2026-09-02 -- and dropped rather than adapted: main's
    // test asserted a configured `logo_url` replaces the icon+text lockup
    // with `<img class="sidebar__brand-wordmark">`. That's exactly the
    // navy-illegibility bug `grouped_sidebar_with_logo_url_never_renders_
    // the_wordmark_image` (below) regression-tests against -- the two
    // assertions are mutually exclusive, and this branch's fix wins (see
    // the coordinator's ruling in the merge report).

    #[test]
    fn grouped_sidebar_with_logo_url_never_renders_the_wordmark_image() {
        // Regression for the navy-sidebar illegibility bug: `logo_url` is
        // non-empty by default (`WAFER_RUN_SHARED__LOGO_URL` defaults to the
        // long dark-ink wordmark PNG drawn for the old white sidebar), so
        // this is the DEFAULT path, not an edge case. Before the fix, a
        // non-empty `logo_url` rendered `<img class="sidebar__brand-wordmark">`
        // instead of the white `.sidebar__brand-name` text, which is nearly
        // invisible against the navy panel.
        let groups = vec![NavGroup {
            label: None,
            items: vec![item("Users", "/b/admin/users")],
        }];
        let s = sidebar_grouped(
            &groups,
            None,
            "/b/admin/users",
            "https://example.com/impresspress-logo-long.png",
            "https://example.com/impresspress-logo.png",
            "Impresspress",
        )
        .into_string();
        assert!(
            !s.contains("sidebar__brand-wordmark"),
            "navy sidebar must never render the dark-ink wordmark image, even \
             with a logo_url configured: {s}"
        );
        assert!(
            s.contains("sidebar__brand-name"),
            "navy sidebar must always render the white text brand name: {s}"
        );
    }

    #[test]
    fn grouped_sidebar_marks_active_via_subpath() {
        let groups = vec![NavGroup {
            label: None,
            items: vec![item("Storage", "/b/storage")],
        }];
        let s = sidebar_grouped(
            &groups,
            None,
            "/b/storage/files/foo.png",
            "",
            "",
            "Impresspress",
        )
        .into_string();
        assert!(s.contains("is-active"));
    }

    fn active_label(groups: &[NavGroup], path: &str) -> Option<String> {
        active_item(groups, path).map(|item| item.label.clone())
    }

    /// The Settings item links to the Email page, and matching on the link
    /// alone left it unhighlighted on the other three settings pages. Every
    /// settings page highlights it, and the rendered sidebar marks exactly one
    /// item.
    #[test]
    fn settings_item_is_active_on_every_settings_page() {
        let groups = crate::ui::nav_groups::admin();
        for page in ["email", "network", "variables", "permissions"] {
            let path = format!("/b/admin/settings/{page}");
            assert_eq!(
                active_label(&groups, &path).as_deref(),
                Some("Settings"),
                "{path}"
            );
            let s = sidebar_grouped(&groups, None, &path, "", "", "Impresspress").into_string();
            assert_eq!(s.matches("is-active").count(), 1, "{path}: {s}");
            assert!(
                s.contains(r#"<a href="/b/admin/settings/email" class="sidebar__nav-item is-active" aria-current="page">"#),
                "{path}: {s}"
            );
        }
    }

    /// A section's pages highlight the item that links into it, the most
    /// specific claim wins where sections nest, and an item whose link ends in
    /// `/` (Dashboard's `/b/admin/`) does not claim the pages below it.
    #[test]
    fn active_item_picks_the_most_specific_claim() {
        let admin = crate::ui::nav_groups::admin();
        let portal = crate::ui::nav_groups::portal();
        let cases: [(&[NavGroup], &str, Option<&str>); 10] = [
            (&admin, "/b/admin/", Some("Dashboard")),
            (&admin, "/b/admin/users", Some("Users")),
            (&admin, "/b/admin/database", Some("Database")),
            (&admin, "/b/storage/admin/", Some("Storage")),
            (&admin, "/b/storage/admin/buckets", Some("Storage")),
            (&admin, "/b/products/admin/stripe", Some("Products")),
            (&admin, "/b/admin/settingsx", None),
            (&portal, "/b/storage/photos/", Some("Files")),
            (&portal, "/b/products/my-products", Some("Products")),
            (&portal, "/b/userportal/profile", Some("Profile")),
        ];
        for (groups, path, want) in cases {
            assert_eq!(active_label(groups, path).as_deref(), want, "{path}");
        }
    }

    /// Every user-selectable icon name (the userportal admin-button editor's
    /// `ICON_OPTIONS` dropdown) must resolve to a real `nav_icon` arm.
    /// Rendering the visibly-distinct unknown-icon glyph for a listed option
    /// means the dropdown and the resolver drifted.
    #[cfg(feature = "block-userportal")]
    #[test]
    fn every_icon_option_resolves_to_a_non_fallback_icon() {
        let fallback = icons::help_circle().into_string();
        for (name, display) in crate::blocks::userportal::pages::admin_buttons::ICON_OPTIONS {
            assert_ne!(
                nav_icon(name).into_string(),
                fallback,
                "ICON_OPTIONS entry '{name}' ({display}) hit the unknown-icon \
                 fallback — add a matching arm to nav_icon"
            );
        }
    }

    #[test]
    fn unknown_icon_name_renders_the_distinct_help_glyph() {
        let rendered = nav_icon("definitely-not-an-icon").into_string();
        assert_eq!(
            rendered,
            icons::help_circle().into_string(),
            "unknown names must render the visibly-distinct help glyph"
        );
        assert_ne!(
            rendered,
            icons::package().into_string(),
            "unknown names must NOT silently render the package glyph"
        );
    }

    #[test]
    fn portal_security_renders_the_lock_icon_not_the_fallback() {
        // Regression for the live mis-render: the Security nav entry used the
        // icon name "lock", which `nav_icon` had no arm for, so it silently
        // fell back to the package glyph. With typed `fn() -> Markup` icons the
        // entry references `icons::lock` directly. The lock SVG's shackle path
        // (`M7 11V7…`) is absent from the package SVG, so its presence proves
        // the lock — not the package fallback — now renders.
        let groups = crate::ui::nav_groups::portal();
        let s = sidebar_grouped(
            &groups,
            None,
            "/b/userportal/security",
            "",
            "",
            "Impresspress",
        )
        .into_string();
        assert!(
            s.contains("M7 11V7a5 5 0 0 1 10 0v4"),
            "Security nav must render the lock icon (shackle path), got: {s}"
        );
    }
}
