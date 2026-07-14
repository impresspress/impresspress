//! Canonical sidebar groups per audience. Single source of truth for
//! both `sidebar_grouped()` callers and the ⌘K palette entries.

use maud::Markup;

use super::{icons, sidebar::NavGroup, NavItem};

fn item(label: &str, href: &str, icon: fn() -> Markup) -> NavItem {
    NavItem {
        label: label.to_string(),
        href: href.to_string(),
        icon,
        external: false,
        block: None,
    }
}

/// A nav item backed by an optional (feature-gated) block. The item only
/// renders when its block is registered — see [`retain_registered`].
fn block_item(label: &str, href: &str, icon: fn() -> Markup, block: &'static str) -> NavItem {
    NavItem {
        block: Some(block),
        ..item(label, href, icon)
    }
}

/// Drop nav items whose backing block isn't registered in this deployment,
/// then drop groups left empty. Which blocks exist varies per target
/// (`impresspress/vector` isn't compiled into every wasm build; Cloudflare
/// ships without `impresspress/llm`) — a static nav that ignores that links
/// straight into "block not found". Called by `ui::shell_page` with
/// `ctx.registered_blocks()`.
pub fn retain_registered(groups: &mut Vec<NavGroup>, registered: &std::collections::HashSet<&str>) {
    for g in groups.iter_mut() {
        g.items
            .retain(|i| i.block.is_none_or(|b| registered.contains(b)));
    }
    groups.retain(|g| !g.items.is_empty());
}

/// Admin sidebar groups.
pub fn admin() -> Vec<NavGroup> {
    vec![
        NavGroup {
            label: Some("Workspace".to_string()),
            items: vec![
                item("Dashboard", "/b/admin/", icons::layout_dashboard),
                item("Users", "/b/admin/users", icons::users),
            ],
        },
        NavGroup {
            label: Some("Data".to_string()),
            items: vec![
                item("Storage", "/b/storage/admin/", icons::hard_drive),
                item("Database", "/b/admin/database", icons::server),
                block_item(
                    "Vector indexes",
                    "/b/vector/",
                    icons::network,
                    "impresspress/vector",
                ),
            ],
        },
        NavGroup {
            label: Some("Communication".to_string()),
            items: vec![
                block_item(
                    "Messages",
                    "/b/messages/",
                    icons::file_text,
                    "impresspress/messages",
                ),
                block_item("LLM", "/b/llm/", icons::robot, "impresspress/llm"),
            ],
        },
        NavGroup {
            label: Some("System".to_string()),
            items: vec![
                item("Blocks", "/b/admin/blocks", icons::package),
                item("Logs", "/b/admin/logs", icons::file_text),
                item("Settings", "/b/admin/settings/email", icons::settings),
            ],
        },
    ]
}

/// Portal sidebar groups (end-user account + apps).
pub fn portal() -> Vec<NavGroup> {
    vec![
        NavGroup {
            label: Some("Account".to_string()),
            items: vec![
                item("Profile", "/b/userportal/profile", icons::user),
                item("Organizations", "/b/auth/orgs", icons::users),
                item("Sessions", "/b/userportal/sessions", icons::shield),
                item("Security", "/b/userportal/security", icons::lock),
            ],
        },
        NavGroup {
            label: Some("Apps".to_string()),
            items: vec![
                block_item(
                    "Products",
                    "/b/products/",
                    icons::package,
                    "impresspress/products",
                ),
                item("Files", "/b/storage/", icons::folder),
                // `/b/cloudstorage/` routes to the files block (see routing.rs).
                block_item(
                    "Shares",
                    "/b/cloudstorage/",
                    icons::link,
                    "impresspress/files",
                ),
                block_item(
                    "Legal",
                    "/b/legalpages/admin/privacy",
                    icons::file_text,
                    "impresspress/legalpages",
                ),
            ],
        },
    ]
}

/// Flatten a slice of `NavGroup`s into palette entries. Same items the
/// sidebar shows; ⌘K uses the same source of truth so the two can never
/// drift out of sync.
pub fn palette_entries_from_groups(groups: &[NavGroup]) -> Vec<crate::ui::palette::PaletteEntry> {
    use crate::ui::palette::PaletteEntry;
    groups
        .iter()
        .flat_map(|g| g.items.iter())
        .map(|item| PaletteEntry {
            keywords: format!("{} {}", item.label.to_lowercase(), item.href),
            label: item.label.clone(),
            kind_label: "Page".to_string(),
            href: item.href.clone(),
            external: item.external,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_has_four_labeled_groups_in_spec_order() {
        let groups = admin();
        let labels: Vec<&str> = groups
            .iter()
            .map(|g| g.label.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(labels, vec!["Workspace", "Data", "Communication", "System"]);
    }

    #[test]
    fn admin_workspace_has_dashboard_and_users() {
        let groups = admin();
        let workspace = &groups[0];
        let labels: Vec<&str> = workspace.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["Dashboard", "Users"]);
    }

    #[test]
    fn admin_data_group_has_database_not_sql() {
        let groups = admin();
        let data = groups
            .iter()
            .find(|g| g.label.as_deref() == Some("Data"))
            .unwrap();
        let database = data.items.iter().find(|i| i.label == "Database").unwrap();
        assert_eq!(database.href, "/b/admin/database");
        assert!(
            data.items.iter().all(|i| i.label != "SQL"),
            "SQL should be renamed to Database"
        );
    }

    #[test]
    fn admin_storage_entry_points_at_actual_route() {
        let groups = admin();
        let storage = groups
            .iter()
            .flat_map(|g| g.items.iter())
            .find(|i| i.label == "Storage")
            .expect("Storage entry exists in admin nav");
        assert_eq!(storage.href, "/b/storage/admin/");
    }

    #[test]
    fn admin_settings_points_at_email_tab_for_phase_3_route() {
        let groups = admin();
        let system = groups
            .iter()
            .find(|g| g.label.as_deref() == Some("System"))
            .unwrap();
        let settings = system.items.iter().find(|i| i.label == "Settings").unwrap();
        assert_eq!(settings.href, "/b/admin/settings/email");
    }

    #[test]
    fn portal_has_account_and_apps() {
        let groups = portal();
        let labels: Vec<&str> = groups
            .iter()
            .map(|g| g.label.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(labels, vec!["Account", "Apps"]);
    }

    #[test]
    fn portal_account_includes_profile_orgs_sessions_security() {
        let groups = portal();
        let account = &groups[0];
        let hrefs: Vec<&str> = account.items.iter().map(|i| i.href.as_str()).collect();
        assert_eq!(
            hrefs,
            vec![
                "/b/userportal/profile",
                "/b/auth/orgs",
                "/b/userportal/sessions",
                "/b/userportal/security"
            ]
        );
    }

    #[test]
    fn portal_apps_includes_products_files_legal() {
        let groups = portal();
        let apps = &groups[1];
        let labels: Vec<&str> = apps.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["Products", "Files", "Shares", "Legal"]);
    }

    #[test]
    fn portal_apps_includes_shares() {
        let groups = portal();
        let apps = groups
            .iter()
            .find(|g| g.label.as_deref() == Some("Apps"))
            .expect("Apps group exists");
        let shares = apps
            .items
            .iter()
            .find(|i| i.label == "Shares")
            .expect("Shares entry exists");
        assert_eq!(shares.href, "/b/cloudstorage/");
    }

    #[test]
    fn retain_registered_drops_unregistered_blocks_and_empty_groups() {
        // A deployment without vector/llm/messages (e.g. Cloudflare) must not
        // show nav links into "block not found" — and the Communication group,
        // left empty, must disappear entirely.
        let mut groups = admin();
        let registered: std::collections::HashSet<&str> =
            ["impresspress/admin", "impresspress/files"].into();
        retain_registered(&mut groups, &registered);

        let labels: Vec<&str> = groups
            .iter()
            .flat_map(|g| g.items.iter())
            .map(|i| i.label.as_str())
            .collect();
        assert!(!labels.contains(&"Vector indexes"), "vector not registered");
        assert!(!labels.contains(&"LLM"), "llm not registered");
        assert!(!labels.contains(&"Messages"), "messages not registered");
        // Ungated items survive regardless.
        assert!(labels.contains(&"Dashboard"));
        assert!(labels.contains(&"Storage"));
        assert!(
            !groups
                .iter()
                .any(|g| g.label.as_deref() == Some("Communication")),
            "emptied group must be dropped"
        );
    }

    #[test]
    fn retain_registered_keeps_everything_when_all_blocks_present() {
        let mut groups = admin();
        let registered: std::collections::HashSet<&str> = [
            "impresspress/vector",
            "impresspress/messages",
            "impresspress/llm",
        ]
        .into();
        let before: usize = groups.iter().map(|g| g.items.len()).sum();
        retain_registered(&mut groups, &registered);
        let after: usize = groups.iter().map(|g| g.items.len()).sum();
        assert_eq!(before, after, "fully-featured deployment keeps every item");
    }

    #[test]
    fn palette_entries_for_admin_groups_includes_admin_pages() {
        let entries = palette_entries_from_groups(&admin());
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert!(labels.contains(&"Dashboard"));
        assert!(labels.contains(&"Users"));
        assert!(labels.contains(&"Settings"));
    }

    #[test]
    fn palette_entries_for_portal_groups_includes_portal_pages() {
        let entries = palette_entries_from_groups(&portal());
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert!(labels.contains(&"Profile"));
        assert!(labels.contains(&"Products"));
    }

    #[test]
    fn palette_entry_keywords_lowercase_label_plus_href() {
        let entries = palette_entries_from_groups(&admin());
        let users = entries.iter().find(|e| e.label == "Users").unwrap();
        assert!(users.keywords.contains("users"));
        assert!(users.keywords.contains("/b/admin/users"));
        assert_eq!(users.kind_label, "Page");
    }
}
