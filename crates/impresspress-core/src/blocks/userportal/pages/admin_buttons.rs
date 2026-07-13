//! `/b/userportal/admin/buttons` — admin management UI for the portal
//! navigation buttons: add form + table, htmx edit modal, and the
//! create/update/delete handlers that re-render the table fragment.

use std::collections::HashMap;

use maud::html;
use wafer_core::clients::database as db;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::super::load_buttons;
// Crate-rooted path (rather than `super::super::TABLE`) so the WRAP-grant
// audit (scripts/audit-wrap-grants.sh) can statically resolve the constant
// to this block's table.
use crate::blocks::userportal::TABLE;
use crate::{
    http::{err_bad_request, err_internal, err_not_found},
    ui::{self, components, icons, sidebar::nav_icon},
    util::{json_map, parse_form_body, stamp_created, stamp_updated, RecordExt},
};

/// Known icon names available for button configuration.
///
/// `pub(crate)`: `ui::sidebar`'s coverage test asserts every entry here
/// resolves to a real `nav_icon` arm (not the unknown-icon fallback), so
/// adding an option without a matching arm fails the build's tests instead
/// of rendering a `?` glyph at runtime.
pub(crate) const ICON_OPTIONS: &[(&str, &str)] = &[
    ("package", "Package"),
    ("shopping-cart", "Shopping Cart"),
    ("folder", "Folder"),
    ("key", "Key"),
    ("server", "Server"),
    ("globe", "Globe"),
    ("users", "Users"),
    ("user", "User"),
    ("settings", "Settings"),
    ("shield", "Shield"),
    ("file-text", "File"),
    ("bar-chart", "Chart"),
    ("dollar-sign", "Dollar"),
    ("dashboard", "Dashboard"),
];

pub async fn admin_buttons_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let buttons = load_buttons(ctx).await;

    let content = html! {
        (components::page_header(
            "Portal Buttons",
            Some("Configure navigation buttons shown on the user profile page"),
            None,
        ))

        // Add button form
        div .card style="margin-bottom:1.5rem;padding:1.25rem" {
            h3 style="margin:0 0 1rem;font-size:1rem" { "Add Button" }
            form
                hx-post="/b/userportal/admin/buttons"
                hx-target="#buttons-table"
                hx-swap="outerHTML"
                style="display:grid;grid-template-columns:1fr 1fr 1fr auto auto;gap:0.75rem;align-items:end"
            {
                div .form-group style="margin:0" {
                    label .form-label for="label" { "Label" }
                    input .form-input #label type="text" name="label"
                        placeholder="e.g. My Products" required;
                }
                div .form-group style="margin:0" {
                    label .form-label for="path" { "Path" }
                    input .form-input #path type="text" name="path"
                        placeholder="e.g. /b/products/mine" required;
                }
                div .form-group style="margin:0" {
                    label .form-label for="icon" { "Icon" }
                    select .form-input #icon name="icon" {
                        @for &(value, display) in ICON_OPTIONS {
                            option value=(value) { (display) }
                        }
                    }
                }
                div .form-group style="margin:0" {
                    label .form-label for="sort_order" { "Order" }
                    input .form-input #sort_order type="number" name="sort_order"
                        value="0" style="width:5rem";
                }
                button .btn .btn-primary type="submit" style="white-space:nowrap" {
                    (icons::plus()) " Add"
                }
            }
        }

        // Buttons table
        (render_buttons_table(&buttons))
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Portal Buttons", ui::NavKind::Portal, "Portal Buttons"),
        content,
    )
    .await
}

fn render_buttons_table(buttons: &[db::Record]) -> maud::Markup {
    html! {
        div #buttons-table {
            @if buttons.is_empty() {
                (components::empty_state(
                    icons::package(),
                    "No buttons configured",
                    "Add a button above to show navigation links on the user profile page.",
                    None,
                ))
            } @else {
                div .table-container {
                    table .table {
                        thead {
                            tr {
                                th { "Label" }
                                th { "Icon" }
                                th { "Path" }
                                th { "Order" }
                                th style="width:6rem" { "Actions" }
                            }
                        }
                        tbody {
                            @for btn in buttons {
                                tr {
                                    td .font-medium { (btn.str_field("label")) }
                                    td {
                                        span .nav-icon style="display:inline-flex" {
                                            (nav_icon(btn.str_field("icon")))
                                        }
                                        " "
                                        span .text-muted .text-sm { (btn.str_field("icon")) }
                                    }
                                    td { code { (btn.str_field("path")) } }
                                    td { (btn.i64_field("sort_order")) }
                                    td {
                                        div style="display:flex;gap:0.25rem" {
                                            button .btn .btn-ghost .btn-sm
                                                hx-get=(format!("/b/userportal/admin/buttons/{}/edit", btn.id))
                                                hx-target=(format!("#edit-modal-{}", btn.id))
                                                hx-swap="innerHTML"
                                                title="Edit"
                                            {
                                                (icons::edit())
                                            }
                                            button .btn .btn-ghost .btn-sm
                                                hx-delete=(format!("/b/userportal/admin/buttons/{}", btn.id))
                                                hx-target="#buttons-table"
                                                hx-swap="outerHTML"
                                                hx-confirm="Delete this button?"
                                                title="Delete"
                                                style="color:var(--danger)"
                                            {
                                                (icons::trash())
                                            }
                                        }
                                        div id=(format!("edit-modal-{}", btn.id)) {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Re-load the buttons and render the `#buttons-table` fragment — the htmx
/// swap target every mutating handler responds with.
async fn buttons_table_response(ctx: &dyn Context) -> OutputStream {
    let buttons = load_buttons(ctx).await;
    ui::html_response(render_buttons_table(&buttons))
}

/// Parse + validate the create/update button form (shared by both handlers):
/// extracts `label`/`path`/`icon`/`sort_order` with the same defaults and
/// trimming, rejects empty label/path, and returns the data map ready for
/// `db::create`/`db::update` (callers add their own timestamp stamp).
fn parse_button_form(raw: &[u8]) -> Result<HashMap<String, serde_json::Value>, OutputStream> {
    let body = parse_form_body(raw);

    let label = body.get("label").map(|s| s.as_str()).unwrap_or("").trim();
    let path = body.get("path").map(|s| s.as_str()).unwrap_or("").trim();
    let icon = body
        .get("icon")
        .map(|s| s.as_str())
        .unwrap_or("package")
        .trim();
    let sort_order: i64 = body
        .get("sort_order")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    if label.is_empty() || path.is_empty() {
        return Err(err_bad_request("Label and path are required"));
    }

    Ok(json_map(serde_json::json!({
        "label": label,
        "path": path,
        "icon": icon,
        "sort_order": sort_order,
    })))
}

pub async fn handle_create_button(ctx: &dyn Context, input: InputStream) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let mut data = match parse_button_form(&raw) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    stamp_created(&mut data);

    if let Err(e) = db::create(ctx, TABLE, data).await {
        return err_internal("Failed to create button", e.message);
    }

    buttons_table_response(ctx).await
}

/// Validate that `id` is safe to interpolate into inline HTML/JS strings
/// (DOM IDs, `getElementById` arguments, etc.). Rejects anything outside
/// `[A-Za-z0-9_-]` and any string longer than 64 chars or empty.
///
/// Used by SEC-058: the userportal edit-button form inlines the record ID
/// into a `script` block via `PreEscaped`, so it must not contain quotes,
/// angle brackets, backslashes, or any other JS-syntax-significant chars.
fn is_safe_dom_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub async fn handle_edit_button_form(ctx: &dyn Context, id: &str) -> OutputStream {
    // SEC-058: the modal auto-show script below uses `PreEscaped(format!(...))`
    // to inject the record ID into inline JS. A record returned from
    // `db::get` should always have a well-formed ID (UUIDv7), but defensively
    // reject any caller-supplied path segment that isn't strict
    // `[a-zA-Z0-9_-]{1,64}` so the JS string interpolation can never be
    // poisoned even if a future code path looks the record up by a
    // non-validated identifier.
    if !is_safe_dom_id(id) {
        return err_not_found("Button not found");
    }
    let Ok(record) = db::get(ctx, TABLE, id).await else {
        return err_not_found("Button not found");
    };

    let current_icon = record.str_field("icon");

    let markup = html! {
        (components::modal(&format!("edit-btn-{id}"), "Edit Button", html! {
            form
                hx-put=(format!("/b/userportal/admin/buttons/{id}"))
                hx-target="#buttons-table"
                hx-swap="outerHTML"
                style="display:flex;flex-direction:column;gap:0.75rem"
            {
                div .form-group style="margin:0" {
                    label .form-label { "Label" }
                    input .form-input type="text" name="label"
                        value=(record.str_field("label")) required;
                }
                div .form-group style="margin:0" {
                    label .form-label { "Path" }
                    input .form-input type="text" name="path"
                        value=(record.str_field("path")) required;
                }
                div .form-group style="margin:0" {
                    label .form-label { "Icon" }
                    select .form-input name="icon" {
                        @for &(value, display) in ICON_OPTIONS {
                            option value=(value) selected[value == current_icon] { (display) }
                        }
                    }
                }
                div .form-group style="margin:0" {
                    label .form-label { "Order" }
                    input .form-input type="number" name="sort_order"
                        value=(record.i64_field("sort_order"));
                }
                div style="display:flex;gap:0.5rem;justify-content:flex-end" {
                    button .btn .btn-secondary type="button"
                        onclick=(format!("document.getElementById('edit-btn-{id}').style.display='none'"))
                    { "Cancel" }
                    button .btn .btn-primary type="submit" { "Save" }
                }
            }
        }))
        // Auto-show the modal
        script { (maud::PreEscaped(format!(
            "document.getElementById('edit-btn-{id}').style.display='flex';"
        ))) }
    };

    ui::html_response(markup)
}

pub async fn handle_update_button(ctx: &dyn Context, input: InputStream, id: &str) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let mut data = match parse_button_form(&raw) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    stamp_updated(&mut data);

    if let Err(e) = db::update(ctx, TABLE, id, data).await {
        return err_internal("Failed to update button", e.message);
    }

    buttons_table_response(ctx).await
}

pub async fn handle_delete_button(ctx: &dyn Context, id: &str) -> OutputStream {
    if let Err(e) = db::delete(ctx, TABLE, id).await {
        return err_internal("Failed to delete button", e.message);
    }

    buttons_table_response(ctx).await
}
