//! Sidebar component — grouped navigation with brand, groups, and user profile.

use maud::Markup;

use super::icons;

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
    pub items: Vec<super::NavItem>,
}

/// Grouped sidebar — same layout as `sidebar(...)`, but items are
/// partitioned into labeled groups. The brand at top, the user pinned
/// at bottom (when `user` is `Some`).
pub fn sidebar_grouped(
    groups: &[NavGroup],
    user: Option<&crate::ui::UserInfo>,
    current_path: &str,
    logo_url: &str,
    logo_icon_url: &str,
) -> maud::Markup {
    use maud::html;

    html! {
        nav .sidebar aria-label="Primary" {
            div .sidebar__brand {
                @if !logo_icon_url.is_empty() {
                    img src=(logo_icon_url) alt="" .sidebar__brand-icon;
                }
                @if !logo_url.is_empty() {
                    img src=(logo_url) alt="Impresspress" .sidebar__brand-wordmark;
                } @else {
                    span .sidebar__brand-name { "Impresspress" }
                }
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
                                    @let active = current_path == item.href
                                        || current_path
                                            .strip_prefix(item.href.as_str())
                                            .is_some_and(|rest| rest.starts_with('/'));
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
                button .sidebar__collapse-toggle id="sidebar-collapse-btn" type="button" onclick="toggleSidebar()" aria-label="Toggle sidebar" {
                    span .sidebar__collapse-icon-expanded { (icons::chevron_left()) }
                    span .sidebar__collapse-icon-collapsed { (icons::chevron_right()) }
                }
            }
            @if let Some(u) = user {
                div .sidebar__user-container {
                    button .sidebar__user id="user-menu-btn" type="button" onclick="toggleProfileMenu()" {
                        (crate::ui::components::avatar(&u.email, crate::ui::components::CtrlSize::Sm))
                        div .sidebar__user-text {
                            div .sidebar__user-email { (u.email) }
                            div .sidebar__user-role {
                                @if u.is_admin() { "Admin" } @else { "User" }
                            }
                        }
                    }
                    div .profile-menu #profile-menu style="display:none" {
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
        script { (maud::PreEscaped(r#"
function toggleProfileMenu() {
    var m = document.getElementById('profile-menu');
    if (m) m.style.display = m.style.display === 'none' ? 'block' : 'none';
}
document.addEventListener('click', function(e) {
    var m = document.getElementById('profile-menu');
    var b = document.getElementById('user-menu-btn');
    if (m && b && !b.contains(e.target) && !m.contains(e.target)) {
        m.style.display = 'none';
    }
});
function toggleSidebar() {
    var s = document.querySelector('.sidebar');
    if (!s) return;
    s.classList.toggle('collapsed');
    try { localStorage.setItem('sidebar.collapsed', s.classList.contains('collapsed') ? '1' : '0'); } catch (e) {}
}
(function() {
    try {
        if (localStorage.getItem('sidebar.collapsed') === '1') {
            var s = document.querySelector('.sidebar');
            if (s) s.classList.add('collapsed');
        }
    } catch (e) {}
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
        let s = sidebar_grouped(&groups, None, "/b/admin/users", "", "").into_string();
        assert!(s.contains(">Workspace<"));
        assert!(s.contains(">Data<"));
        assert!(s.contains("/b/admin/users"));
        assert!(s.contains(r#"aria-current="page""#));
    }

    #[test]
    fn grouped_sidebar_marks_active_via_subpath() {
        let groups = vec![NavGroup {
            label: None,
            items: vec![item("Storage", "/b/storage")],
        }];
        let s = sidebar_grouped(&groups, None, "/b/storage/files/foo.png", "", "").into_string();
        assert!(s.contains("is-active"));
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
        let s = sidebar_grouped(&groups, None, "/b/userportal/security", "", "").into_string();
        assert!(
            s.contains("M7 11V7a5 5 0 0 1 10 0v4"),
            "Security nav must render the lock icon (shackle path), got: {s}"
        );
    }
}
