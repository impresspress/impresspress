//! Pure-sync routing for `AdminBlock::handle`.
//!
//! `route()` classifies a request by path + action into an `AdminRoute`
//! variant. It does no I/O, holds no `Context`, and allocates only when
//! a path-extracted identifier (e.g. a user id) needs to be carried
//! into the variant. The async `handle()` then dispatches the variant
//! to the appropriate leaf handler.
//!
//! This module is the single source of truth for the AdminBlock's
//! endpoint table. Every endpoint reachable in `handle()` must appear
//! as a variant here and be covered by the test table below.

/// Classification of an admin HTTP request.
///
/// Lifetime `'a` ties path-extracted slices (user_id, var_key, etc.) to
/// the caller-owned `path` string, avoiding allocations in the routing
/// step. Identifiers that need normalization (e.g. block names with
/// `--` → `/` decoding) own a `String` instead.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AdminRoute<'a> {
    // --- /b/admin/api/... (JSON API) ---
    /// `/b/admin/api/users*`
    UsersApi,
    /// `/b/admin/api/database*`
    DatabaseApi,
    /// `/b/admin/api/iam*`
    IamApi,
    /// `/b/admin/api/logs*`
    LogsApi,
    /// `/b/admin/api/settings*`
    SettingsApi,
    /// `/b/admin/api/extensions*`
    ExtensionsApi,
    /// `/b/admin/api/storage*` — delegated to `impresspress/files`
    StorageDelegate,
    /// `/b/admin/api/cloudstorage<rest>` — delegated to `impresspress/files`.
    /// Carries the suffix after `/cloudstorage` because the handler must
    /// build `/admin/b/cloudstorage<rest>` for the delegated meta.
    CloudStorageDelegate {
        rest: &'a str,
    },
    /// API path under /b/admin/api/ that didn't match any of the above.
    ApiNotFound,

    // --- /b/admin/settings/... (consolidated settings tabs) ---
    /// `/b/admin/settings` or `/b/admin/settings/` — redirect to email tab.
    SettingsRedirect,
    /// `/b/admin/settings/<tab>` where tab ∈ {email, network, variables, permissions}.
    SettingsPage {
        tab: &'a str,
    },

    // --- /b/admin/... (SSR pages + htmx mutations) ---
    /// action=create, sub=`/users/{user_id}/disable`
    UserDisable {
        user_id: &'a str,
    },
    /// action=create, sub=`/users/{user_id}/enable`
    UserEnable {
        user_id: &'a str,
    },
    /// action=delete, sub=`/users/{user_id}`
    UserDelete {
        user_id: &'a str,
    },
    /// action=create, sub=`/iam/roles`
    CreateRole,
    /// action=delete, sub=`/iam/roles/{role_id}`
    DeleteRole {
        role_id: &'a str,
    },
    /// action=retrieve, sub=`/blocks/{encoded}/detail` (block_name = encoded.replace("--", "/"))
    BlockDetail {
        block_name: String,
    },
    /// action=create, sub=`/blocks/{encoded}/toggle`
    BlockToggle {
        block_name: String,
    },
    /// action=create, sub=`/variables`
    CreateVariable,
    /// action=retrieve, sub=`/variables/{var_key}/edit`
    EditVariableForm {
        var_key: &'a str,
    },
    /// action=update, sub=`/variables/{var_key}`
    UpdateVariable {
        var_key: &'a str,
    },
    /// action=retrieve, sub=`/network/detail/inbound`
    NetworkInboundDetail,
    /// action=create, sub=`/grants/rules`
    CreateWrapGrant,
    /// action=delete, sub=`/grants/rules/{rule_id}`
    DeleteWrapGrant {
        rule_id: &'a str,
    },
    /// action=create, sub=`/email`
    SaveEmailSettings,
    /// action=create, sub=`/database/query`
    DatabaseQuery,

    // --- SSR fallthrough (GET) ---
    Dashboard,
    UsersPage,
    StoragePage,
    BlocksPage,
    DatabasePage,
    LogsPage,
    EmailRedirect,
    NetworkRedirect,
    VariablesRedirect,
    /// `/permissions` — preserves `?tab=` query as `?subtab=` in the new location.
    PermissionsRedirect,
    GrantsPage,

    /// Catch-all: path doesn't match any admin route.
    NotFound,
}

/// Classify a request by path + action. Pure sync, no allocations
/// except when an identifier must be normalized (block_name "--" → "/").
pub(super) fn route<'a>(path: &'a str, action: &str) -> AdminRoute<'a> {
    // 1) /b/admin/api/... — JSON API, order-sensitive
    if let Some(api_rest) = path.strip_prefix("/b/admin/api") {
        // Match by first path segment after /api
        let first = api_rest.split('/').nth(1).unwrap_or("");
        return match first {
            "users" => AdminRoute::UsersApi,
            "database" => AdminRoute::DatabaseApi,
            "iam" => AdminRoute::IamApi,
            "logs" => AdminRoute::LogsApi,
            "settings" => AdminRoute::SettingsApi,
            "extensions" => AdminRoute::ExtensionsApi,
            "storage" => AdminRoute::StorageDelegate,
            "cloudstorage" => AdminRoute::CloudStorageDelegate {
                rest: api_rest.strip_prefix("/cloudstorage").unwrap_or(""),
            },
            _ => AdminRoute::ApiNotFound,
        };
    }

    // 2) /b/admin/settings (consolidated) — must precede the /b/admin/ catch-all
    if path == "/b/admin/settings" || path == "/b/admin/settings/" {
        return AdminRoute::SettingsRedirect;
    }
    if let Some(rest) = path.strip_prefix("/b/admin/settings/") {
        let tab = rest.split('/').next().unwrap_or("");
        return match tab {
            "email" | "network" | "variables" | "permissions" => AdminRoute::SettingsPage { tab },
            _ => AdminRoute::NotFound,
        };
    }

    // 3) /b/admin/... (everything else)
    if let Some(sub) = path.strip_prefix("/b/admin") {
        // 3a) htmx mutations, action-gated
        if action == "create" {
            if let Some(id) = sub
                .strip_prefix("/users/")
                .and_then(|s| s.strip_suffix("/disable"))
            {
                if !id.is_empty() {
                    return AdminRoute::UserDisable { user_id: id };
                }
            }
            if let Some(id) = sub
                .strip_prefix("/users/")
                .and_then(|s| s.strip_suffix("/enable"))
            {
                if !id.is_empty() {
                    return AdminRoute::UserEnable { user_id: id };
                }
            }
            if sub == "/iam/roles" {
                return AdminRoute::CreateRole;
            }
            if let Some(encoded) = sub
                .strip_prefix("/blocks/")
                .and_then(|s| s.strip_suffix("/toggle"))
            {
                if !encoded.is_empty() {
                    return AdminRoute::BlockToggle {
                        block_name: encoded.replace("--", "/"),
                    };
                }
            }
            if sub == "/variables" {
                return AdminRoute::CreateVariable;
            }
            if sub == "/grants/rules" {
                return AdminRoute::CreateWrapGrant;
            }
            if sub == "/email" {
                return AdminRoute::SaveEmailSettings;
            }
            if sub == "/database/query" {
                return AdminRoute::DatabaseQuery;
            }
        }
        if action == "delete" {
            if let Some(id) = sub.strip_prefix("/users/") {
                if !id.is_empty() {
                    return AdminRoute::UserDelete { user_id: id };
                }
            }
            if let Some(id) = sub.strip_prefix("/iam/roles/") {
                if !id.is_empty() {
                    return AdminRoute::DeleteRole { role_id: id };
                }
            }
            if let Some(id) = sub.strip_prefix("/grants/rules/") {
                if !id.is_empty() {
                    return AdminRoute::DeleteWrapGrant { rule_id: id };
                }
            }
        }
        if action == "retrieve" {
            if let Some(encoded) = sub
                .strip_prefix("/blocks/")
                .and_then(|s| s.strip_suffix("/detail"))
            {
                if !encoded.is_empty() {
                    return AdminRoute::BlockDetail {
                        block_name: encoded.replace("--", "/"),
                    };
                }
            }
            if let Some(key) = sub
                .strip_prefix("/variables/")
                .and_then(|s| s.strip_suffix("/edit"))
            {
                if !key.is_empty() {
                    return AdminRoute::EditVariableForm { var_key: key };
                }
            }
            if sub == "/network/detail/inbound" {
                return AdminRoute::NetworkInboundDetail;
            }
        }
        if action == "update" {
            if let Some(key) = sub.strip_prefix("/variables/") {
                if !key.is_empty() {
                    return AdminRoute::UpdateVariable { var_key: key };
                }
            }
        }

        // 3b) SSR page fallthrough (GET-ish), action-agnostic at the dispatch layer.
        // The original code uses `match sub.as_str()` regardless of action; we mirror.
        return match sub {
            "" | "/" => AdminRoute::Dashboard,
            "/users" => AdminRoute::UsersPage,
            "/storage" => AdminRoute::StoragePage,
            "/blocks" => AdminRoute::BlocksPage,
            "/database" => AdminRoute::DatabasePage,
            "/logs" => AdminRoute::LogsPage,
            "/email" => AdminRoute::EmailRedirect,
            "/network" => AdminRoute::NetworkRedirect,
            "/variables" => AdminRoute::VariablesRedirect,
            "/permissions" => AdminRoute::PermissionsRedirect,
            "/grants" => AdminRoute::GrantsPage,
            _ => AdminRoute::NotFound,
        };
    }

    AdminRoute::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: this module routes purely on the input strings. The real handle() in
    // mod.rs must capture msg.path() and msg.action() into owned Strings
    // BEFORE any msg.set_meta("req.resource", ...) call — set_meta mutates
    // what msg.path() returns. See the API-meta-normalization comment in
    // handle() for details. Bug surfaced in code review 2026-05-28.

    // Helper: each case is (description, path, action, expected_variant).
    fn cases() -> Vec<(
        &'static str,
        &'static str,
        &'static str,
        AdminRoute<'static>,
    )> {
        vec![
            // /b/admin/api/... — order matters for the longer prefixes
            (
                "users api list",
                "/b/admin/api/users",
                "retrieve",
                AdminRoute::UsersApi,
            ),
            (
                "users api detail",
                "/b/admin/api/users/abc",
                "retrieve",
                AdminRoute::UsersApi,
            ),
            (
                "database api",
                "/b/admin/api/database/tables",
                "retrieve",
                AdminRoute::DatabaseApi,
            ),
            (
                "iam api",
                "/b/admin/api/iam/roles",
                "retrieve",
                AdminRoute::IamApi,
            ),
            (
                "logs api",
                "/b/admin/api/logs",
                "retrieve",
                AdminRoute::LogsApi,
            ),
            (
                "settings api",
                "/b/admin/api/settings/email",
                "retrieve",
                AdminRoute::SettingsApi,
            ),
            (
                "extensions api",
                "/b/admin/api/extensions",
                "retrieve",
                AdminRoute::ExtensionsApi,
            ),
            (
                "wafer api removed",
                "/b/admin/api/wafer",
                "retrieve",
                AdminRoute::ApiNotFound,
            ),
            (
                "custom-tables api removed",
                "/b/admin/api/custom-tables",
                "retrieve",
                AdminRoute::ApiNotFound,
            ),
            (
                "storage delegate",
                "/b/admin/api/storage/buckets",
                "retrieve",
                AdminRoute::StorageDelegate,
            ),
            (
                "cloudstorage delegate",
                "/b/admin/api/cloudstorage/foo",
                "retrieve",
                AdminRoute::CloudStorageDelegate { rest: "/foo" },
            ),
            (
                "cloudstorage empty",
                "/b/admin/api/cloudstorage",
                "retrieve",
                AdminRoute::CloudStorageDelegate { rest: "" },
            ),
            (
                "api unknown",
                "/b/admin/api/whatever",
                "retrieve",
                AdminRoute::ApiNotFound,
            ),
            // /b/admin/settings — special pre-check
            (
                "settings root no slash",
                "/b/admin/settings",
                "retrieve",
                AdminRoute::SettingsRedirect,
            ),
            (
                "settings root slash",
                "/b/admin/settings/",
                "retrieve",
                AdminRoute::SettingsRedirect,
            ),
            (
                "settings email",
                "/b/admin/settings/email",
                "retrieve",
                AdminRoute::SettingsPage { tab: "email" },
            ),
            (
                "settings network",
                "/b/admin/settings/network",
                "retrieve",
                AdminRoute::SettingsPage { tab: "network" },
            ),
            (
                "settings variables",
                "/b/admin/settings/variables",
                "retrieve",
                AdminRoute::SettingsPage { tab: "variables" },
            ),
            (
                "settings permissions",
                "/b/admin/settings/permissions",
                "retrieve",
                AdminRoute::SettingsPage { tab: "permissions" },
            ),
            (
                "settings unknown tab",
                "/b/admin/settings/foobar",
                "retrieve",
                AdminRoute::NotFound,
            ),
            // /b/admin/... htmx mutations
            (
                "user disable",
                "/b/admin/users/u1/disable",
                "create",
                AdminRoute::UserDisable { user_id: "u1" },
            ),
            (
                "user enable",
                "/b/admin/users/u1/enable",
                "create",
                AdminRoute::UserEnable { user_id: "u1" },
            ),
            (
                "user delete",
                "/b/admin/users/u1",
                "delete",
                AdminRoute::UserDelete { user_id: "u1" },
            ),
            (
                "create role",
                "/b/admin/iam/roles",
                "create",
                AdminRoute::CreateRole,
            ),
            (
                "delete role",
                "/b/admin/iam/roles/r1",
                "delete",
                AdminRoute::DeleteRole { role_id: "r1" },
            ),
            (
                "block detail",
                "/b/admin/blocks/wafer-run--auth/detail",
                "retrieve",
                AdminRoute::BlockDetail {
                    block_name: "wafer-run/auth".to_string(),
                },
            ),
            (
                "block toggle",
                "/b/admin/blocks/wafer-run--auth/toggle",
                "create",
                AdminRoute::BlockToggle {
                    block_name: "wafer-run/auth".to_string(),
                },
            ),
            (
                "create variable",
                "/b/admin/variables",
                "create",
                AdminRoute::CreateVariable,
            ),
            (
                "edit variable form",
                "/b/admin/variables/WAFER_RUN_SHARED__APP_NAME/edit",
                "retrieve",
                AdminRoute::EditVariableForm {
                    var_key: "WAFER_RUN_SHARED__APP_NAME",
                },
            ),
            (
                "update variable",
                "/b/admin/variables/WAFER_RUN_SHARED__APP_NAME",
                "update",
                AdminRoute::UpdateVariable {
                    var_key: "WAFER_RUN_SHARED__APP_NAME",
                },
            ),
            (
                "network inbound",
                "/b/admin/network/detail/inbound",
                "retrieve",
                AdminRoute::NetworkInboundDetail,
            ),
            (
                "create wrap grant",
                "/b/admin/grants/rules",
                "create",
                AdminRoute::CreateWrapGrant,
            ),
            (
                "delete wrap grant",
                "/b/admin/grants/rules/g1",
                "delete",
                AdminRoute::DeleteWrapGrant { rule_id: "g1" },
            ),
            (
                "save email settings",
                "/b/admin/email",
                "create",
                AdminRoute::SaveEmailSettings,
            ),
            (
                "database query",
                "/b/admin/database/query",
                "create",
                AdminRoute::DatabaseQuery,
            ),
            (
                "custom block install removed",
                "/b/admin/custom-blocks/install",
                "create",
                AdminRoute::NotFound,
            ),
            (
                "custom block delete removed",
                "/b/admin/custom-blocks/impresspress--foo",
                "delete",
                AdminRoute::NotFound,
            ),
            // /b/admin/... SSR pages (GET)
            (
                "dashboard empty",
                "/b/admin",
                "retrieve",
                AdminRoute::Dashboard,
            ),
            (
                "dashboard slash",
                "/b/admin/",
                "retrieve",
                AdminRoute::Dashboard,
            ),
            (
                "users page",
                "/b/admin/users",
                "retrieve",
                AdminRoute::UsersPage,
            ),
            (
                "storage page",
                "/b/admin/storage",
                "retrieve",
                AdminRoute::StoragePage,
            ),
            (
                "blocks page",
                "/b/admin/blocks",
                "retrieve",
                AdminRoute::BlocksPage,
            ),
            (
                "database page",
                "/b/admin/database",
                "retrieve",
                AdminRoute::DatabasePage,
            ),
            (
                "logs page",
                "/b/admin/logs",
                "retrieve",
                AdminRoute::LogsPage,
            ),
            (
                "email redirect",
                "/b/admin/email",
                "retrieve",
                AdminRoute::EmailRedirect,
            ),
            (
                "network redirect",
                "/b/admin/network",
                "retrieve",
                AdminRoute::NetworkRedirect,
            ),
            (
                "variables redirect",
                "/b/admin/variables",
                "retrieve",
                AdminRoute::VariablesRedirect,
            ),
            (
                "permissions redirect",
                "/b/admin/permissions",
                "retrieve",
                AdminRoute::PermissionsRedirect,
            ),
            (
                "grants page",
                "/b/admin/grants",
                "retrieve",
                AdminRoute::GrantsPage,
            ),
            (
                "unknown ssr",
                "/b/admin/whatever",
                "retrieve",
                AdminRoute::NotFound,
            ),
            // Outside the admin namespace entirely
            (
                "outside admin",
                "/b/other",
                "retrieve",
                AdminRoute::NotFound,
            ),
            ("root", "/", "retrieve", AdminRoute::NotFound),
            // Empty-id guards: paths that look like mutation patterns but
            // have empty extracted identifiers must NOT match the mutation
            // arm (the original handler falls through and returns the
            // SSR-or-NotFound branch).
            (
                "user disable empty id",
                "/b/admin/users//disable",
                "create",
                AdminRoute::NotFound,
            ),
            (
                "delete role empty id",
                "/b/admin/iam/roles/",
                "delete",
                AdminRoute::NotFound,
            ),
        ]
    }

    #[test]
    fn route_table_classifies_every_endpoint_correctly() {
        for (desc, path, action, expected) in cases() {
            let actual = route(path, action);
            assert_eq!(
                actual, expected,
                "case: {desc} (path={path:?}, action={action:?})"
            );
        }
    }
}
