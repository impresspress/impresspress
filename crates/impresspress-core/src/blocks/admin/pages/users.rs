use maud::{html, Markup};
use wafer_block::db::{ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, InputStream, Message, OutputStream, WaferError};

use super::{admin_page, crumb};
use crate::{
    blocks::{
        admin::{logs::audit_log, ops, ROLES_TABLE},
        auth::repo::{
            api_keys,
            users::{self, ActiveUserQuery, UserRow},
        },
        crud,
    },
    http::{err_not_found, ResponseBuilder},
    ui::{
        self,
        components::{self, badge, pagination, Badge, BadgeVariant},
        icons,
        shell::Topbar,
        templates::list_page,
        UserInfo,
    },
    util::{parse_form_body, RecordExt},
};

pub async fn users_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    // Still needed by the page body (the current admin's own row is
    // rendered differently); the shell loads its own copy.
    let user = UserInfo::from_message(msg);
    let tab = msg.query("tab");
    let active_tab = match tab {
        "roles" => "roles",
        "api-keys" => "api-keys",
        _ => "users",
    };

    let tabs_markup = components::tab_navigation(vec![
        components::Tab {
            active: active_tab == "users",
            href: "/b/admin/users",
            label: "Users",
            icon: Some(icons::users()),
        },
        components::Tab {
            active: active_tab == "roles",
            href: "/b/admin/users?tab=roles",
            label: "Roles",
            icon: Some(icons::shield()),
        },
        components::Tab {
            active: active_tab == "api-keys",
            href: "/b/admin/users?tab=api-keys",
            label: "API Keys",
            icon: Some(icons::key()),
        },
    ]);

    let current_uid = user
        .as_ref()
        .map(|u| u.id.as_str())
        .unwrap_or("")
        .to_string();
    // An unreadable table is an error page, never the empty state ("No
    // roles", "No API keys") and never the failure's own text in the page.
    let tab = match active_tab {
        "users" => users_tab(ctx, msg, &current_uid).await,
        "roles" => roles_tab(ctx)
            .await
            .map(|roles| html! { div #iam-content { (roles) } }),
        _ => api_keys_tab(ctx).await,
    };
    let tab = match tab {
        Ok(tab) => tab,
        Err(e) => return crud::db_error_page(msg, e, "admin users page: tab read failed"),
    };
    let tab_content = html! {
        div #users-tab-content { (tab) }
    };

    let body = list_page(Some(tabs_markup), tab_content, None);

    admin_page(
        ctx,
        msg,
        "Users",
        Topbar {
            crumbs: crumb("Users"),
            primary_action: None,
            subtitle: Some("Manage accounts, roles, and API keys"),
            show_palette: true,
        },
        body,
    )
    .await
}

/// Users tab content (table + search + pagination).
/// `Err` when a read failed; the caller answers it, never an empty table.
async fn users_tab(
    ctx: &dyn Context,
    msg: &Message,
    current_user_id: &str,
) -> Result<Markup, WaferError> {
    let (page, page_size, _) = msg.pagination_params(20);
    let search = msg.query("search").to_string();

    // Both the filter (`deleted_at IS NULL`), the sort and the search shape
    // (email OR id) live in `users::list_active_page`, shared with the JSON
    // list endpoint. `total_count` is now the full matched count across all
    // pages, so the footer below paginates a search correctly instead of
    // reporting the in-page count as the total.
    let list = users::list_active_page(
        ctx,
        &ActiveUserQuery {
            page: page as i64,
            page_size: page_size as u32,
            search: (!search.is_empty()).then(|| search.clone()),
        },
    )
    .await?;
    let table = users_table(&list.rows, ctx, current_user_id).await?;

    Ok(html! {
        div .filter-bar {
            (components::search_input_with_value("search", "Search by email or user ID...", "/b/admin/users", "#content", &search))
        }

        (table)

        @if let Some(per_page) = std::num::NonZeroU32::new(page_size as u32) {
            (pagination(list.page as u32, per_page, list.total_count as u32, "/b/admin/users"))
        }
    })
}

/// Render the users table body. Async because it enriches each user with roles.
///
/// A failed roles read is an error, not a table of users with no roles.
async fn users_table(
    records: &[UserRow],
    ctx: &dyn Context,
    current_user_id: &str,
) -> Result<Markup, WaferError> {
    // Bulk-fetch all roles for the visible users in a single query (was N+1:
    // one `list_all` per row), via the shared `ops::fetch_roles` helper.
    let user_ids: Vec<&str> = records.iter().map(|r| r.id.as_str()).collect();
    let user_roles = ops::fetch_roles(ctx, &user_ids).await?;

    let rows: Vec<components::TableRow> = records
        .iter()
        .map(|record| {
            let roles: &[String] = user_roles.get(&record.id).map(Vec::as_slice).unwrap_or(&[]);
            single_user_row(record, roles, current_user_id)
        })
        .collect();

    Ok(components::DataTable::new(&USER_COLUMNS)
        .rows(rows)
        .empty(html! { p .text-center .text-muted { "No users found" } })
        .render())
}

/// The users table's columns. Declared once so the `<td data-label>` the
/// component stamps on every cell names the same column the header does — and
/// so the single-row htmx swap in [`user_row_fragment`] renders against the
/// same list the table did.
const USER_COLUMNS: [components::TableCol<'static>; 5] = [
    components::TableCol {
        label: "Email",
        width: None,
    },
    components::TableCol {
        label: "Roles",
        width: None,
    },
    components::TableCol {
        label: "Status",
        width: None,
    },
    components::TableCol {
        label: "Created",
        width: None,
    },
    components::TableCol {
        label: "Actions",
        width: None,
    },
];

/// Render one row of the users table. Shared between the multi-row table
/// renderer and `user_row_fragment` (htmx outerHTML swap target for the
/// enable/disable mutations).
///
/// `current_uid` is `""` when the caller is rendering a single-row update
/// fragment (no "(you)" affordance) — the mutation endpoints reject
/// self-disable before reaching this path.
fn single_user_row(record: &UserRow, roles: &[String], current_uid: &str) -> components::TableRow {
    let email = record.email.as_str();
    let disabled = record.disabled;
    let created = record.created_at.as_str();
    let is_self = !current_uid.is_empty() && record.id == current_uid;
    components::TableRow::new(vec![
        html! { (email) },
        html! {
            @for role in roles {
                (Badge::new(BadgeVariant::Primary).classes("mr-1").render(html! { (role) }))
            }
            @if roles.is_empty() {
                span .text-muted { "\u{2014}" }
            }
        },
        html! {
            @if disabled {
                (components::status_badge("disabled"))
            } @else {
                (components::status_badge("active"))
            }
        },
        html! { time .text-muted datetime=(created) { (created.get(..10).unwrap_or(created)) } },
        html! {
                @if is_self {
                    span .text-muted { "(you)" }
                } @else {
                    @if disabled {
                        button .btn .btn--sm .btn--success
                            hx-post={"/b/admin/users/" (record.id) "/enable"}
                            hx-target={"#user-row-" (record.id)}
                            hx-swap="outerHTML"
                            title="Enable user"
                        { "Enable" }
                    } @else {
                        button .btn .btn--sm .btn--secondary
                            hx-post={"/b/admin/users/" (record.id) "/disable"}
                            hx-target={"#user-row-" (record.id)}
                            hx-swap="outerHTML"
                            hx-confirm={"Disable " (email) "?"}
                            title="Disable user"
                        { "Disable" }
                    }
                    " "
                    button .btn .btn--sm .btn--danger
                        hx-delete={"/b/admin/users/" (record.id)}
                        hx-target={"#user-row-" (record.id)}
                        hx-swap="outerHTML"
                        hx-confirm={"Delete " (email) "? This cannot be undone."}
                        title="Delete user"
                    { (icons::trash()) }
                }
        },
    ])
    .id(format!("user-row-{}", record.id))
}

/// Render a single user table row (used by enable/disable mutations).
///
/// It goes back through the shared component against the same
/// [`USER_COLUMNS`], so the row htmx swaps in carries the same classes and the
/// same `data-label` cells as the row it replaces.
///
/// `Err` with the reason when the row cannot be re-read: the user read or the
/// roles read failed (classified by [`crud::db_error_notice`]), or the user
/// is gone. The caller swaps in an error row instead —
/// an empty body would delete the row from the table under a success toast,
/// and a row built from a failed roles read would show the user holding none.
async fn user_row_fragment(ctx: &dyn Context, user_id: &str) -> Result<Markup, &'static str> {
    let record = match users::find_by_id(ctx, user_id).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            tracing::error!(
                user_id,
                "admin users: row re-read found no user after a mutation"
            );
            return Err("something went wrong");
        }
        Err(e) => return Err(crud::db_error_notice(e, "admin users: row re-read failed")),
    };

    // Single-user lookup via the shared roles helper (the `[one]` case).
    let roles = match ops::fetch_roles(ctx, &[user_id]).await {
        Ok(mut roles) => roles.remove(user_id).unwrap_or_default(),
        Err(e) => {
            return Err(crud::db_error_notice(
                e,
                "admin users: roles re-read failed",
            ))
        }
    };

    Ok(single_user_row(&record, &roles, "").render(&USER_COLUMNS, None))
}

/// The answer to an Enable/Disable that landed: the re-rendered row under a
/// success toast, or — when the row cannot be re-read — an error row in its
/// place saying the change was made.
async fn user_row_response(ctx: &dyn Context, user_id: &str, done: &str) -> OutputStream {
    match user_row_fragment(ctx, user_id).await {
        Ok(row) => ui::html_response_with_toast(row, done, "success"),
        Err(reason) => ui::swap_error_row_response(
            &format!("user-row-{user_id}"),
            USER_COLUMNS.len(),
            &format!(
                "{done}, but the row could not be reloaded: {reason}. Reload the page to see it."
            ),
        ),
    }
}

/// `POST /b/admin/users/{id}/disable`. `{id}` is read only as the route
/// table bound it.
pub async fn handle_user_disable(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.var("id");
    // Self-disable guard, update, and audit-log write live in the shared ops
    // layer (single source of truth shared with the JSON surface).
    if let Err(out) = ops::set_user_disabled(ctx, msg, user_id, true).await {
        return out;
    }
    user_row_response(ctx, user_id, "User disabled").await
}

/// `POST /b/admin/users/{id}/enable`. `{id}` is read only as the route
/// table bound it.
pub async fn handle_user_enable(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.var("id");
    if let Err(out) = ops::set_user_disabled(ctx, msg, user_id, false).await {
        return out;
    }
    user_row_response(ctx, user_id, "User enabled").await
}

/// `DELETE /b/admin/users/{id}`. `{id}` is read only as the route table
/// bound it.
pub async fn handle_user_delete(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.var("id");
    // Self-delete guard, soft-delete, and audit-log write live in the shared
    // ops layer.
    if let Err(out) = ops::delete_user(ctx, msg, user_id).await {
        return out;
    }
    ui::html_response_with_toast(html! {}, "User deleted", "success")
}

/// POST /b/admin/iam/roles (create role from modal form)
pub async fn handle_create_role(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let bytes = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body = parse_form_body(&bytes);

    let name = body.get("name").map(|s| s.as_str()).unwrap_or("");
    let description = body.get("description").map(|s| s.as_str());

    // Name-required guard, create, and audit-log write live in the shared ops
    // layer (single source of truth shared with the JSON surface).
    if let Err(out) = ops::create_role(ctx, msg, name, description, None).await {
        return out;
    }

    // Return the updated roles tab + close modal + toast
    let content = match roles_tab(ctx).await {
        Ok(content) => content,
        Err(e) => return reread_failed("Role created", "the role list", e),
    };
    let trigger = r#"{"showToast":{"message":"Role created","type":"success"},"closeModal":{"id":"create-role"}}"#;
    ResponseBuilder::new()
        .set_header("HX-Trigger", trigger)
        .body(
            content.into_string().into_bytes(),
            "text/html; charset=utf-8",
        )
}

/// `DELETE /b/admin/iam/roles/{id}` (from the roles tab). `{id}` is read only
/// as the route table bound it.
pub async fn handle_delete_role(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let role_id = msg.var("id");
    // System-role guard, grant revocation, delete, and audit-log write live
    // in the shared ops layer.
    let deleted = match ops::delete_role(ctx, msg, role_id).await {
        Ok(deleted) => deleted,
        Err(out) => return out,
    };
    let content = match roles_tab(ctx).await {
        Ok(content) => content,
        Err(e) => return reread_failed("Role deleted", "the role list", e),
    };
    match late_pass_warning(&deleted) {
        None => ui::html_response_with_toast(content, "Role deleted", "success"),
        Some(warning) => ui::html_response_with_toast(content, &warning, "warning"),
    }
}

/// The warning toast for a role delete whose revocation pass after the row
/// delete failed — what may be left, who holds it, and why it matters — or
/// `None` for a clean delete.
fn late_pass_warning(deleted: &ops::RoleDeleted) -> Option<String> {
    let revives = "Creating a role with this name again would give it back to them.";
    let warning = match deleted {
        ops::RoleDeleted::Clean => return None,
        ops::RoleDeleted::LateGrantsNotRevoked { holders: None } => format!(
            "Role deleted, but checking for a grant assigned during the delete failed, so one \
             may remain. Remove the role from anyone the Users tab still lists with it. \
             {revives}"
        ),
        ops::RoleDeleted::LateGrantsNotRevoked {
            holders: Some(holders),
        } if holders.is_empty() => format!(
            "Role deleted, but revoking any grant assigned during the delete failed. None was \
             found, but one assigned in that moment may remain: remove the role from anyone \
             the Users tab still lists with it. {revives}"
        ),
        ops::RoleDeleted::LateGrantsNotRevoked {
            holders: Some(holders),
        } => format!(
            "Role deleted, but a grant of it assigned during the delete may remain for user(s) \
             {}. Remove the role from them. {revives}",
            holders.join(", ")
        ),
        ops::RoleDeleted::LateSessionsNotInvalidated { holders } => format!(
            "Role deleted and every grant of it revoked, but the sessions of user(s) {} were \
             not invalidated: a token they already hold may carry the role until it expires.",
            holders.join(", ")
        ),
    };
    Some(warning)
}

/// `POST /b/admin/api-keys/{id}/revoke` (from the API-keys tab). `{id}` is
/// read only as the route table bound it.
///
/// The Revoke button swaps the answer into `#users-tab-content`, so the answer
/// is this tab, re-rendered with the key shown revoked. auth-ui's
/// `PATCH /b/auth/api/api-keys/{id}` revokes the same row through the same
/// `api_keys::revoke`, but it answers with JSON, and a JSON body swapped into
/// the tab replaces the key table with its own source text. The tab's markup
/// is this block's to render, so the route that answers with it is too.
pub async fn handle_revoke_api_key(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let key_id = msg.var("id");
    // A read that could not run is not a missing key: answering 404 would
    // tell the operator the key is already gone while it is still live.
    match api_keys::find_by_id(ctx, key_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return err_not_found("API key not found"),
        Err(e) => return crud::db_error_internal(e, "Could not load the API key"),
    }
    if let Err(e) = api_keys::revoke(ctx, key_id).await {
        return crud::db_error_internal(e, "Could not revoke the API key");
    }
    audit_log(
        ctx,
        msg.user_id(),
        "api_key.revoke",
        &format!("api_keys/{key_id}"),
        msg.remote_addr(),
    )
    .await;
    match api_keys_tab(ctx).await {
        Ok(tab) => ui::html_response_with_toast(tab, "API key revoked", "success"),
        Err(e) => reread_failed("API key revoked", "the key list", e),
    }
}

/// The answer to a write that landed when the tab its control swaps
/// (`innerHTML`) could not be re-read: an error notice in the tab under an
/// error toast, both saying the write was `done`. The reason is classified by
/// [`crud::db_error_notice`], so a WRAP denial reads as one and its own text
/// stays in the log.
fn reread_failed(done: &str, what: &str, error: WaferError) -> OutputStream {
    let reason = crud::db_error_notice(error, "admin users page: re-read after a write failed");
    ui::swap_notice_response(&format!(
        "{done}, but {what} could not be reloaded: {reason}. Reload the page to see it."
    ))
}

/// `Err` when the read failed; the caller answers it, never an empty table.
async fn roles_tab(ctx: &dyn Context) -> Result<Markup, WaferError> {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "name".into(),
            desc: false,
        }],
        limit: Some(100),
        ..Default::default()
    };
    let list = db::list(ctx, ROLES_TABLE, &opts).await?;

    Ok(html! {
        div .flex .items-center .justify-between .mb-4 {
            h3 .font-semibold { "Roles" }
            button .btn .btn--primary .btn--sm data-action="modal-open" data-modal-target="create-role" {
                (icons::plus()) " Create Role"
            }
        }

        @let rows: Vec<Vec<Markup>> = list.records.iter().map(|record| {
            let name = record.str_field("name");
            let is_system = record.bool_field("is_system");
            vec![
                html! { span .font-medium { (name) } },
                html! { span .text-muted { (record.str_field("description")) } },
                html! {
                    @if is_system {
                        (badge(BadgeVariant::Info, "System"))
                    } @else {
                        (badge(BadgeVariant::Primary, "Custom"))
                    }
                },
                html! {
                    @if !is_system {
                        button .btn .btn--sm .btn--danger
                            hx-delete={"/b/admin/iam/roles/" (record.id)}
                            hx-target="#iam-content"
                            hx-confirm={"Delete role \"" (name) "\"? Everyone it is assigned to loses it."}
                        { (icons::trash()) }
                    }
                },
            ]
        }).collect();

        (components::data_table::<fn(usize) -> Option<String>>(
            &ROLE_COLUMNS,
            rows,
            None,
            html! { p .text-center .text-muted { "No roles" } },
        ))

        // Create role modal
        (components::modal("create-role", "Create Role", html! {
            form hx-post="/b/admin/iam/roles" hx-target="#iam-content" {
                div .form-group {
                    label .form-label .required for="role-name" { "Name" }
                    input .form-input type="text" #role-name name="name" placeholder="e.g. editor" required;
                }
                div .form-group {
                    label .form-label for="role-desc" { "Description" }
                    input .form-input type="text" #role-desc name="description" placeholder="Optional description";
                }
                div .form-actions {
                    button .btn .btn--secondary type="button" data-action="modal-close" data-modal-target="create-role" { "Cancel" }
                    button .btn .btn--primary type="submit" { "Create" }
                }
            }
        }))
    })
}

/// `Err` when the read failed; the caller answers it, never an empty table.
async fn api_keys_tab(ctx: &dyn Context) -> Result<Markup, WaferError> {
    // Every key in the deployment, newest first — this tab is the operator's
    // view, not one account's (`api_keys::list_for_user` is what the
    // userportal and the auth-ui CRUD endpoints use).
    let list = api_keys::list_recent(ctx, 100).await?;
    let now = chrono::Utc::now();

    Ok(html! {
        div .flex .items-center .justify-between .mb-4 {
            h3 .font-semibold { "API Keys" }
            button .btn .btn--primary .btn--sm data-action="modal-open" data-modal-target="create-api-key" {
                (icons::plus()) " Create API Key"
            }
        }

        @let rows: Vec<Vec<Markup>> = list.iter().map(|record| {
            let user_id = record.user_id.as_str();
            let created = record.created_at.as_str();
            // A key authenticates only while it is neither revoked nor past
            // its expiry — the two checks `auth` makes before it accepts one
            // — so the badge names whichever of them retired it.
            let retired = if record.is_revoked() {
                Some("revoked")
            } else if record.is_expired(now) {
                Some("expired")
            } else {
                None
            };
            vec![
                html! { code { (record.key_prefix) "..." } },
                html! { (record.name) },
                html! { span .text-muted { (user_id.get(..8).unwrap_or(user_id)) } },
                html! { span .text-muted { (created.get(..10).unwrap_or(created)) } },
                html! {
                    @if let Some(reason) = retired {
                        (components::badge(components::BadgeVariant::Danger, reason))
                    } @else {
                        (components::status_badge("active"))
                    }
                },
                html! {
                    // Only a live key has anything left to revoke: an expiry
                    // is set once, at creation, and never moves.
                    @if retired.is_none() {
                        // This block's own route, answered with
                        // this tab re-rendered: see
                        // `handle_revoke_api_key`.
                        button .btn .btn--sm .btn--secondary
                            hx-post={"/b/admin/api-keys/" (record.id) "/revoke"}
                            hx-target="#users-tab-content"
                            hx-confirm="Revoke this API key?"
                        { "Revoke" }
                    }
                },
            ]
        }).collect();

        (components::data_table::<fn(usize) -> Option<String>>(
            &API_KEY_COLUMNS,
            rows,
            None,
            html! { p .text-center .text-muted { "No API keys" } },
        ))

        // Create API key modal
        (components::modal("create-api-key", "Create API Key", html! {
            form hx-post="/b/auth/api/api-keys" hx-target="#users-tab-content" {
                div .form-group {
                    label .form-label for="key-name" { "Name" }
                    input .form-input type="text" #key-name name="name" placeholder="e.g. CI/CD key" required;
                }
                div .form-actions {
                    button .btn .btn--secondary type="button" data-action="modal-close" data-modal-target="create-api-key" { "Cancel" }
                    button .btn .btn--primary type="submit" { "Create" }
                }
            }
        }))
    })
}

/// The roles and API-key tables' columns. Declared once each so the
/// `<td data-label>` the component stamps on every cell names the same column
/// its header does.
const ROLE_COLUMNS: [components::TableCol<'static>; 4] = [
    components::TableCol {
        label: "Name",
        width: None,
    },
    components::TableCol {
        label: "Description",
        width: None,
    },
    components::TableCol {
        label: "Type",
        width: None,
    },
    components::TableCol {
        label: "Actions",
        width: None,
    },
];

const API_KEY_COLUMNS: [components::TableCol<'static>; 6] = [
    components::TableCol {
        label: "Prefix",
        width: None,
    },
    components::TableCol {
        label: "Name",
        width: None,
    },
    components::TableCol {
        label: "User",
        width: None,
    },
    components::TableCol {
        label: "Created",
        width: None,
    },
    components::TableCol {
        label: "Status",
        width: None,
    },
    components::TableCol {
        label: "Actions",
        width: None,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{admin_msg, TestContext};

    /// A user's creation date is a per-run value wherever the page is
    /// screenshotted, and the visual-baseline suite masks dates by the
    /// `<time>` element alone (`crates/impresspress-web/tests/e2e/
    /// visual-baseline.spec.ts`), so the cell has to render one.
    #[tokio::test]
    async fn the_users_table_renders_the_created_date_as_a_time_element() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.seed_auth_user("u-1").await;

        let parts = crate::blocks::admin::test_support::browser_request(
            &ctx,
            admin_msg("retrieve", "/b/admin/users"),
        )
        .await;
        let html = String::from_utf8(parts.body).expect("UTF-8 body");

        assert_eq!(parts.status, 200, "{html}");
        assert!(
            html.contains(r#"datetime="2026-01-01T00:00:00Z">2026-01-01</time>"#),
            "the Created cell must be a <time>: {html}"
        );
    }

    /// A key the auth block would refuse is not shown as active. Revoked and
    /// expired are both dead keys, each badged with its own reason, and
    /// neither offers a Revoke button — only the live key does.
    #[tokio::test]
    async fn the_api_keys_tab_badges_each_dead_key_with_why() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.seed_auth_user("u-1").await;
        let seed = ctx.fixture();
        let hour = chrono::Duration::hours(1);
        for (prefix, expires_at) in [
            ("ipk_live", Some(chrono::Utc::now() + hour)),
            ("ipk_expired", Some(chrono::Utc::now() - hour)),
            ("ipk_revoked", None),
        ] {
            let key = api_keys::insert(
                &seed,
                api_keys::NewApiKey {
                    user_id: "u-1",
                    name: prefix,
                    key_hash: prefix,
                    key_prefix: prefix,
                    expires_at,
                },
            )
            .await
            .expect("seed a key");
            if prefix == "ipk_revoked" {
                api_keys::revoke(&seed, &key.id).await.expect("revoke");
            }
        }

        let mut msg = admin_msg("retrieve", "/b/admin/users");
        msg.set_meta("req.query.tab", "api-keys");
        let parts = crate::blocks::admin::test_support::browser_request(&ctx, msg).await;
        let html = String::from_utf8(parts.body).expect("UTF-8 body");
        assert_eq!(parts.status, 200, "{html}");

        let row = |prefix: &str| -> String {
            html.split("<tr")
                .find(|row| row.contains(&format!("<code>{prefix}...</code>")))
                .unwrap_or_else(|| panic!("no row for {prefix}: {html}"))
                .to_owned()
        };
        for (prefix, badge, revocable) in [
            (
                "ipk_live",
                r#"<span class="badge badge-success">active</span>"#,
                true,
            ),
            (
                "ipk_expired",
                r#"<span class="badge badge-danger">expired</span>"#,
                false,
            ),
            (
                "ipk_revoked",
                r#"<span class="badge badge-danger">revoked</span>"#,
                false,
            ),
        ] {
            let row = row(prefix);
            assert!(row.contains(badge), "{prefix} must render {badge}: {row}");
            assert_eq!(
                row.contains("Revoke this API key?"),
                revocable,
                "{prefix}: a Revoke button only on a live key: {row}"
            );
        }
    }

    /// Disable answers one `<tr>`, swapped over the user's row. When the row
    /// cannot be re-read after the write landed, the answer is an error row
    /// under an error toast: not a row built from a failed roles read, which
    /// shows an admin holding no roles, and not an empty body, which deletes
    /// the row from the table under a "User disabled" success toast.
    ///
    /// `break_list_reads` keeps the user read working and fails the roles
    /// query, so this reaches the roles half of the re-read.
    #[tokio::test]
    async fn a_failed_row_reread_after_disable_swaps_an_error_row() {
        use crate::platform_state::user_roles;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.seed_auth_user("u-1").await;
        user_roles::assign(&ctx, "u-1", "admin", "")
            .await
            .expect("grant");
        let failing = ctx.clone().break_list_reads();

        let parts = crate::blocks::admin::test_support::browser_request(
            &failing,
            admin_msg("create", "/b/admin/users/u-1/disable"),
        )
        .await;
        let html = String::from_utf8(parts.body).expect("UTF-8 body");

        assert_eq!(parts.status, 200, "htmx swaps only a 2xx");
        assert!(
            html.starts_with(r#"<tr id="user-row-u-1">"#),
            "the swap target is a row, and the answer must be one: {html}"
        );
        assert!(html.contains("alert--error"), "{html}");
        assert!(html.contains("User disabled, but"), "{html}");
        assert!(
            !html.contains("\u{2014}"),
            "a row claiming the user holds no roles: {html}"
        );
        let trigger = parts
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("HX-Trigger"))
            .map(|(_, value)| value.clone())
            .expect("an error toast must be triggered");
        let toast: serde_json::Value = serde_json::from_str(&trigger).expect("trigger JSON");
        assert_eq!(toast["showToast"]["type"], "error", "{toast}");

        // The write did land — the notice is right to say so.
        let row = users::find_by_id(&ctx, "u-1")
            .await
            .expect("read")
            .expect("row");
        assert!(row.disabled);
    }

    /// The roles tab's delete, when the revocation pass after the row delete
    /// fails: the tab re-renders without the role and the toast warns about
    /// a possibly surviving grant, rather than an error that leaves the
    /// deleted role on screen.
    ///
    /// Names `user_roles::TABLE` only to aim the fault injector;
    /// `tests/repo_door.rs` allowlists it as one.
    #[tokio::test]
    async fn deleting_a_role_whose_late_revocation_pass_fails_warns_and_rerenders() {
        use crate::{
            platform_state::user_roles,
            test_support::{output_header, FailingDbOpContext},
        };

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let data = crate::util::json_map(serde_json::json!({
            "name": "editor",
            "description": "",
            "permissions": [],
            "is_system": false,
        }));
        let role_id = db::create(&ctx, ROLES_TABLE, data).await.expect("role").id;
        user_roles::assign(&ctx, "u-1", "editor", "")
            .await
            .expect("grant");

        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.list", user_roles::TABLE)])
                .after_passing(1);
        let msg = crate::blocks::admin::test_support::routed(admin_msg(
            "delete",
            &format!("/b/admin/iam/roles/{role_id}"),
        ));
        let out = handle_delete_role(&failing, &msg).await;

        let trigger = output_header(out, "HX-Trigger")
            .await
            .expect("a re-rendered tab carries its toast");
        let toast: serde_json::Value = serde_json::from_str(&trigger).expect("trigger JSON");
        assert_eq!(toast["showToast"]["type"], "warning", "{toast}");
        assert_eq!(
            toast["showToast"]["message"],
            "Role deleted, but checking for a grant assigned during the delete failed, so one \
             may remain. Remove the role from anyone the Users tab still lists with it. \
             Creating a role with this name again would give it back to them.",
            "the pass could not read the grants, so it cannot name a holder — the toast says \
             where to look, and why it matters"
        );
    }

    /// When the late pass did read the grants, the toast names who holds the
    /// one that may remain, and says what recreating the role would do.
    ///
    /// Names `user_roles::TABLE` only to aim the fault injector;
    /// `tests/repo_door.rs` allowlists it as one.
    #[tokio::test]
    async fn the_late_pass_warning_names_the_holder_of_the_grant_that_may_remain() {
        use crate::{
            blocks::admin::test_support::AssignBeforeGrantRead,
            platform_state::user_roles,
            test_support::{output_header, FailingDbOpContext},
        };

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let data = crate::util::json_map(serde_json::json!({
            "name": "editor",
            "description": "",
            "permissions": [],
            "is_system": false,
        }));
        let role_id = db::create(&ctx, ROLES_TABLE, data).await.expect("role").id;
        for user in ["u-1", "u-late"] {
            ctx.seed_auth_user(user).await;
        }
        user_roles::assign(&ctx, "u-1", "editor", "")
            .await
            .expect("grant");

        let racing = AssignBeforeGrantRead::new(
            ctx.clone(),
            FailingDbOpContext::new(
                ctx.clone(),
                vec![("database.delete_where_count", user_roles::TABLE)],
            )
            .after_passing(1),
            2,
            &["u-late"],
            "editor",
        );
        let msg = crate::blocks::admin::test_support::routed(admin_msg(
            "delete",
            &format!("/b/admin/iam/roles/{role_id}"),
        ));
        let trigger = output_header(handle_delete_role(&racing, &msg).await, "HX-Trigger")
            .await
            .expect("a re-rendered tab carries its toast");
        let toast: serde_json::Value = serde_json::from_str(&trigger).expect("trigger JSON");
        assert_eq!(toast["showToast"]["type"], "warning", "{toast}");
        assert_eq!(
            toast["showToast"]["message"],
            "Role deleted, but a grant of it assigned during the delete may remain for user(s) \
             u-late. Remove the role from them. Creating a role with this name again would give \
             it back to them."
        );
    }
}
