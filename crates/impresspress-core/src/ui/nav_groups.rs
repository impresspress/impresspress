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
    }
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
                item("Vector indexes", "/b/vector/", icons::network),
            ],
        },
        NavGroup {
            label: Some("Communication".to_string()),
            items: vec![
                item("Messages", "/b/messages/", icons::file_text),
                item("LLM", "/b/llm/", icons::robot),
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
                item("Products", "/b/products/", icons::package),
                item("Files", "/b/storage/", icons::folder),
                item("Shares", "/b/cloudstorage/", icons::link),
                item("Legal", "/b/legalpages/admin/privacy", icons::file_text),
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
