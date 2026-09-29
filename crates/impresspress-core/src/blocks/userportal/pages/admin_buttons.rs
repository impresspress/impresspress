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
    // The portal's buttons are admin-only configuration, so their mutations
    // belong in the one audit trail an operator reads — the admin block's
    // `audit_logs` table, reached under this block's own WRAP identity via
    // the grant `admin::AdminBlock` declares for it.
    blocks::{admin::logs::audit_log, crud},
    http::{err_bad_request, err_not_found},
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
    // "No buttons configured" is what an empty table renders; an unreadable
    // one is an error page, or the admin would add back buttons that exist.
    let buttons = match load_buttons(ctx).await {
        Ok(buttons) => buttons,
        Err(e) => {
            return crud::db_error_page(
                msg,
                e,
                "userportal admin buttons page: buttons read failed",
            )
        }
    };

    let content = html! {
        (components::page_header(
            "Portal Buttons",
            Some("Configure navigation buttons shown on the user profile page"),
            None,
        ))

        // Add button form
        div .card .mb-6 {
            h3 .text-16 .m-0 .mb-4 { "Add Button" }
            form
                hx-post="/b/userportal/admin/buttons"
                hx-target="#buttons-table"
                hx-swap="outerHTML"
                .grid .admin-buttons-form-grid .items-end .gap-3
            {
                div .form-group .m-0 {
                    label .form-label for="label" { "Label" }
                    input .form-input #label type="text" name="label"
                        placeholder="e.g. My Products" required;
                }
                div .form-group .m-0 {
                    label .form-label for="path" { "Path" }
                    input .form-input #path type="text" name="path"
                        placeholder="e.g. /b/products/mine" required;
                }
                div .form-group .m-0 {
                    label .form-label for="icon" { "Icon" }
                    select .form-input #icon name="icon" {
                        @for &(value, display) in ICON_OPTIONS {
                            option value=(value) { (display) }
                        }
                    }
                }
                div .form-group .m-0 {
                    label .form-label for="sort_order" { "Order" }
                    input .form-input #sort_order type="number" name="sort_order"
                        value="0" .w-80;
                }
                button .btn .btn--primary type="submit" .nowrap {
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
                                th .w-96 { "Actions" }
                            }
                        }
                        tbody {
                            @for btn in buttons {
                                tr {
                                    td .font-medium { (btn.str_field("label")) }
                                    td {
                                        span .nav-icon .d-inline-flex {
                                            (nav_icon(btn.str_field("icon")))
                                        }
                                        " "
                                        span .text-muted .text-sm { (btn.str_field("icon")) }
                                    }
                                    td { code { (btn.str_field("path")) } }
                                    td { (btn.i64_field("sort_order")) }
                                    td {
                                        div .flex .gap-1 {
                                            button .btn .btn--ghost .btn--sm
                                                hx-get=(format!("/b/userportal/admin/buttons/{}/edit", btn.id))
                                                hx-target=(format!("#edit-modal-{}", btn.id))
                                                hx-swap="innerHTML"
                                                title="Edit"
                                            {
                                                (icons::edit())
                                            }
                                            button .btn .btn--ghost .btn--sm .text-danger
                                                hx-delete=(format!("/b/userportal/admin/buttons/{}", btn.id))
                                                hx-target="#buttons-table"
                                                hx-swap="outerHTML"
                                                hx-confirm="Delete this button?"
                                                title="Delete"
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
///
/// Called only after the mutation has been written, so a failed re-read must
/// not render the empty table (which reads as "the change wiped every
/// button"): the target becomes an error notice saying the change was
/// `applied`, why the list could not be loaded (classified by
/// [`crud::db_error_notice`]), and that it needs a reload.
async fn buttons_table_response(ctx: &dyn Context, applied: &str) -> OutputStream {
    match load_buttons(ctx).await {
        Ok(buttons) => ui::html_response(render_buttons_table(&buttons)),
        Err(e) => {
            let reason = crud::db_error_notice(e, "userportal admin buttons: table re-read failed");
            ui::swap_error_response(
                "buttons-table",
                &format!(
                    "{applied}, but the button list could not be loaded: {reason}. Reload the \
                     page to see it."
                ),
            )
        }
    }
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

pub async fn handle_create_button(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let mut data = match parse_button_form(&raw) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    stamp_created(&mut data);

    let record = match db::create(ctx, TABLE, data).await {
        Ok(record) => record,
        Err(e) => return crud::db_error_internal(e, "Failed to create button"),
    };
    audit_log(
        ctx,
        msg.user_id(),
        "portal_button.create",
        &format!("portal_buttons/{}", record.id),
        msg.remote_addr(),
    )
    .await;

    buttons_table_response(ctx, "Button added").await
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
    // SEC-058 originally: the modal used to be revealed by a
    // `PreEscaped(format!(...))` script with the record ID injected into inline
    // JS. That script is gone — the id now travels as a `data-modal-target`
    // attribute (maud-escaped) and in a `serde_json`-built response header —
    // so the interpolation this guarded no longer exists. It stays because the
    // id also becomes an HTML element id that `getElementById` has to find
    // again: a record returned from `db::get` always has a well-formed ID
    // (UUIDv7), and any caller-supplied path segment that is not strict
    // `[a-zA-Z0-9_-]{1,64}` is still refused rather than rendered.
    if !is_safe_dom_id(id) {
        return err_not_found("Button not found");
    }
    // A read that could not run is not a deleted button. Answering 404 for a
    // row that is still there invites the obvious next move — recreate it —
    // which duplicates the button once the read works again. `crud::db_error`
    // is the one mapping: `NotFound` is the 404 above, a WRAP refusal is a
    // 403, everything else is a sealed 500 with the cause logged.
    let record = match db::get(ctx, TABLE, id).await {
        Ok(record) => record,
        Err(e) => return crud::db_error(e, "Button not found", "Could not load the button"),
    };

    let current_icon = record.str_field("icon");
    let modal_id = format!("edit-btn-{id}");

    let markup = html! {
        (components::modal(&modal_id, "Edit Button", html! {
            form
                hx-put=(format!("/b/userportal/admin/buttons/{id}"))
                hx-target="#buttons-table"
                hx-swap="outerHTML"
                .flex .flex-col .gap-3
            {
                div .form-group .m-0 {
                    label .form-label { "Label" }
                    input .form-input type="text" name="label"
                        value=(record.str_field("label")) required;
                }
                div .form-group .m-0 {
                    label .form-label { "Path" }
                    input .form-input type="text" name="path"
                        value=(record.str_field("path")) required;
                }
                div .form-group .m-0 {
                    label .form-label { "Icon" }
                    select .form-input name="icon" {
                        @for &(value, display) in ICON_OPTIONS {
                            option value=(value) selected[value == current_icon] { (display) }
                        }
                    }
                }
                div .form-group .m-0 {
                    label .form-label { "Order" }
                    input .form-input type="number" name="sort_order"
                        value=(record.i64_field("sort_order"));
                }
                div .flex .gap-2 .justify-end {
                    button .btn .btn--secondary type="button"
                        data-action="modal-close" data-modal-target=(&modal_id)
                    { "Cancel" }
                    button .btn .btn--primary type="submit" { "Save" }
                }
            }
        }))
    };

    // Reveal the modal once htmx has swapped it in. `components::modal()`
    // renders the boolean `hidden` attribute (base.css:
    // `[hidden] { display: none !important; }`), so revealing it means clearing
    // `hidden` — a `style.display` assignment loses to that `!important` and
    // the modal never opens. This used to be a `<script>` appended to the
    // fragment with the record id interpolated into JavaScript source; it is
    // now the `openModal` response-header channel.
    ui::html_response_opening_modal(markup, &modal_id)
}

pub async fn handle_update_button(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    id: &str,
) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let mut data = match parse_button_form(&raw) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    stamp_updated(&mut data);

    if let Err(e) = db::update(ctx, TABLE, id, data).await {
        return crud::db_error(e, "Button not found", "Failed to update button");
    }
    audit_log(
        ctx,
        msg.user_id(),
        "portal_button.update",
        &format!("portal_buttons/{id}"),
        msg.remote_addr(),
    )
    .await;

    buttons_table_response(ctx, "Button saved").await
}

pub async fn handle_delete_button(ctx: &dyn Context, msg: &Message, id: &str) -> OutputStream {
    if let Err(e) = db::delete(ctx, TABLE, id).await {
        // `DbExec::delete` answers `NotFound` when no row matched, so a stale
        // id in the caller's own page is their 404 — not this site reporting
        // an internal fault and inviting a retry.
        return crud::db_error(e, "Button not found", "Failed to delete button");
    }
    audit_log(
        ctx,
        msg.user_id(),
        "portal_button.delete",
        &format!("portal_buttons/{id}"),
        msg.remote_addr(),
    )
    .await;

    buttons_table_response(ctx, "Button deleted").await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;
    use wafer_core::clients::database as db;

    use super::*;
    use crate::{
        blocks::userportal::UserPortalBlock,
        test_support::{
            admin_msg, audit_rows, output_header, output_html, output_is_error, TestContext,
        },
    };

    async fn ctx_with_userportal() -> TestContext {
        let mut ctx = TestContext::with_userportal().await;
        ctx.register_block(
            "impresspress/userportal",
            std::sync::Arc::new(UserPortalBlock::new()),
        );
        ctx
    }

    /// The `Message` the route table hands the delete handler, with `{id}`
    /// bound the way it is on the wire.
    fn delete_msg(id: &str) -> Message {
        admin_msg("delete", &format!("/b/userportal/admin/buttons/{id}"))
    }

    fn button_data(label: &str, icon: &str, path: &str) -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert("label".to_string(), json!(label));
        m.insert("icon".to_string(), json!(icon));
        m.insert("path".to_string(), json!(path));
        m.insert("sort_order".to_string(), json!(0));
        m
    }

    /// Regression test for the six-week-silent bug (task 12e): the edit
    /// modal is `components::modal()`, which renders the boolean `hidden`
    /// attribute (`[hidden] { display: none !important; }` in base.css).
    /// A plain `el.style.display='flex'` inline-style toggle can never beat
    /// that `!important`, so the modal could never actually open even
    /// though the fragment rendered "successfully". This asserts the
    /// fragment routes through the shared modal machinery in
    /// `ui/assets/chrome.js` (which flips the `hidden` IDL property, not
    /// `style.display`) instead of reintroducing a hand-rolled inline-style
    /// toggle. The two controls used to be `onclick="openModal('…')"` /
    /// `onclick="closeModal('…')"` strings with the record id interpolated
    /// into JavaScript source; they are now a response header and a
    /// `data-action` attribute, and the assertions below moved with them.
    #[tokio::test]
    async fn edit_button_form_opens_via_shared_modal_helpers_not_inline_style() {
        let ctx = ctx_with_userportal().await;
        let record = db::create(&ctx, TABLE, button_data("Files", "folder", "/b/storage/"))
            .await
            .unwrap();

        // The handler takes `id` directly: `userportal/mod.rs` dispatches
        // `GET /b/userportal/admin/buttons/{id}/edit` through its route table
        // and passes the bound `{id}` in.
        let resp = handle_edit_button_form(&ctx, &record.id).await;
        let html = output_html(resp).await;

        // The modal container itself must still start hidden via the real
        // `hidden` attribute (not an inline style) -- `components::modal()`
        // renders `id=(id) hidden` in that order.
        assert!(
            html.contains(&format!(r#"id="edit-btn-{}" hidden"#, record.id)),
            "modal must render the boolean `hidden` attribute:\n{html}"
        );
        // Cancel closes it declaratively, with the id as inert attribute text.
        assert!(
            html.contains(&format!(
                r#"data-action="modal-close" data-modal-target="edit-btn-{}""#,
                record.id
            )),
            "Cancel button must declare the shared modal-close action:\n{html}"
        );
        // Nothing in the fragment is script at all any more.
        assert!(
            !html.contains("<script"),
            "the fragment must carry no script:\n{html}"
        );
        // The exact bug: a plain inline `style.display` toggle always loses
        // to `[hidden] { display: none !important; }`, so the modal could
        // never open. Any reintroduction of that pattern must fail this.
        assert!(
            !html.contains("style.display"),
            "modal must not be toggled via a plain inline style.display assignment:\n{html}"
        );

        // Auto-show is the response-header channel, fired after the swap so
        // the overlay is not revealed before its contents land.
        let resp = handle_edit_button_form(&ctx, &record.id).await;
        let trigger = output_header(resp, "HX-Trigger-After-Swap").await;
        assert_eq!(
            trigger,
            Some(format!(
                r#"{{"openModal":{{"id":"edit-btn-{}"}}}}"#,
                record.id
            )),
            "the fragment must ask chrome.js to open the modal after the swap"
        );
    }

    /// A read that could not run is not a deleted button. The `let Ok(record)
    /// = … else { err_not_found }` answered the admin `404 Button not found`
    /// for a row that is still there, so the obvious next move — recreate it
    /// — duplicates the button once the database comes back.
    #[tokio::test]
    async fn an_unreadable_button_row_is_an_outage_not_a_missing_button() {
        let ctx = ctx_with_userportal().await;
        let record = db::create(&ctx, TABLE, button_data("Files", "folder", "/b/storage/"))
            .await
            .unwrap();
        let failing = ctx.break_reads();

        let out = handle_edit_button_form(&failing, &record.id).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a failed row read must not answer 404"
        );
    }

    /// An unreadable buttons table is the 500 page, not "No buttons
    /// configured" — which invites the admin to add back buttons that exist.
    #[tokio::test]
    async fn a_failed_buttons_read_is_a_500_not_the_empty_state() {
        let ctx = ctx_with_userportal().await;
        db::create(&ctx, TABLE, button_data("Files", "folder", "/b/storage/"))
            .await
            .unwrap();
        let ctx = ctx.break_list_reads();

        let (status, html) = crate::blocks::userportal::test_support::browser_request(
            &ctx,
            admin_msg("retrieve", "/b/userportal/admin/buttons"),
            "",
        )
        .await;

        assert_eq!(status, 500);
        assert!(!html.contains("No buttons configured"), "{html}");
    }

    /// A mutation that was written but whose table re-read failed swaps in an
    /// error notice with an error toast, not the empty table. The empty table
    /// reads as "adding this button deleted all the others". The notice keeps
    /// the `buttons-table` id so the next `outerHTML` swap still has a target.
    #[tokio::test]
    async fn a_failed_reread_after_create_swaps_an_error_not_an_empty_table() {
        let ctx = ctx_with_userportal().await;
        db::create(&ctx, TABLE, button_data("Files", "folder", "/b/storage/"))
            .await
            .unwrap();
        let failing = ctx.clone().break_list_reads();

        let out = crate::blocks::userportal::test_support::browser_request(
            &failing,
            admin_msg("create", "/b/userportal/admin/buttons"),
            "label=Shop&path=%2Fb%2Fproducts%2F&icon=shopping-cart&sort_order=1",
        )
        .await;
        let (status, html) = out;

        assert_eq!(status, 200, "htmx swaps only a 2xx");
        assert!(!html.contains("No buttons configured"), "{html}");
        assert!(html.contains(r#"id="buttons-table""#), "{html}");
        assert!(html.contains("alert--error"), "{html}");
        assert!(html.contains("Button added"), "{html}");

        // The write did land — the notice is right to say so.
        let rows = db::list(&ctx, TABLE, &Default::default()).await.unwrap();
        assert_eq!(rows.records.len(), 2);
    }

    /// The toast half of the swap above rides the `HX-Trigger` header.
    #[tokio::test]
    async fn a_failed_reread_after_delete_fires_an_error_toast() {
        let ctx = ctx_with_userportal().await;
        let record = db::create(&ctx, TABLE, button_data("Files", "folder", "/b/storage/"))
            .await
            .unwrap();
        let failing = ctx.break_list_reads();

        let out = handle_delete_button(&failing, &delete_msg(&record.id), &record.id).await;
        let trigger = output_header(out, "HX-Trigger")
            .await
            .expect("an error toast must be triggered");
        let trigger: serde_json::Value = serde_json::from_str(&trigger).unwrap();
        assert_eq!(trigger["showToast"]["type"], "error", "{trigger}");
        assert!(
            trigger["showToast"]["message"]
                .as_str()
                .unwrap()
                .starts_with("Button deleted"),
            "{trigger}"
        );
    }

    /// Deleting a button that does not exist is a 404, not a 500.
    ///
    /// `db::delete` returns `DatabaseError::NotFound` when zero rows match
    /// (`wafer-core`'s `DbExec::delete`), and this handler turned every error
    /// — that one included — into `err_internal`. A caller asking to remove an
    /// already-removed button got "Internal server error", which reads as a
    /// fault in the site rather than a stale id in their own page. Found in
    /// the 2026-09-10 live endpoint run.
    #[tokio::test]
    async fn deleting_a_missing_button_is_not_found_not_internal() {
        let ctx = ctx_with_userportal().await;

        let out = handle_delete_button(
            &ctx,
            &delete_msg("btn_does_not_exist"),
            "btn_does_not_exist",
        )
        .await;

        assert!(
            output_is_error(out, "NotFound").await,
            "a missing button must answer NotFound, not an internal error"
        );
    }

    /// The form body the browser posts, in the field names the add/edit
    /// forms use.
    fn button_form(label: &str, path: &str) -> InputStream {
        InputStream::from_bytes(
            format!("label={label}&path={path}&icon=folder&sort_order=0").into_bytes(),
        )
    }

    /// Dispatch one admin request through the block's own `handle`, so the
    /// route table binds `{id}` and hands each handler the `Message`.
    async fn portal(
        ctx: &dyn Context,
        action: &str,
        path: &str,
        input: InputStream,
    ) -> OutputStream {
        wafer_run::Block::handle(&UserPortalBlock::new(), ctx, admin_msg(action, path), input).await
    }

    /// The portal buttons are admin-only configuration — they decide what
    /// every user sees in the portal nav — and they were the one admin
    /// surface whose create/update/delete wrote no audit row at all.
    #[tokio::test]
    async fn button_mutations_are_audited_in_the_admin_trail() {
        let ctx = ctx_with_userportal().await;

        portal(
            &ctx,
            "create",
            "/b/userportal/admin/buttons",
            button_form("Files", "/b/storage/"),
        )
        .await
        .collect_buffered()
        .await
        .expect("the create is answered with the table fragment");

        let created = audit_rows(&ctx, "portal_button.create").await;
        assert_eq!(created.len(), 1, "one row per created button");
        assert_eq!(created[0].str_field("user_id"), "admin_1");
        let id = created[0]
            .str_field("resource")
            .strip_prefix("portal_buttons/")
            .expect("the row names the button it created")
            .to_string();

        portal(
            &ctx,
            "update",
            &format!("/b/userportal/admin/buttons/{id}"),
            button_form("Storage", "/b/storage/"),
        )
        .await
        .collect_buffered()
        .await
        .expect("the update is answered with the table fragment");
        let updated = audit_rows(&ctx, "portal_button.update").await;
        assert_eq!(updated.len(), 1);
        assert_eq!(
            updated[0].str_field("resource"),
            format!("portal_buttons/{id}")
        );

        portal(
            &ctx,
            "delete",
            &format!("/b/userportal/admin/buttons/{id}"),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await
        .expect("the delete is answered with the table fragment");
        let deleted = audit_rows(&ctx, "portal_button.delete").await;
        assert_eq!(deleted.len(), 1);
        assert_eq!(
            deleted[0].str_field("resource"),
            format!("portal_buttons/{id}")
        );
    }

    /// A delete that matched no row changed nothing, so it writes no row.
    #[tokio::test]
    async fn a_delete_that_matched_nothing_is_not_audited() {
        let ctx = ctx_with_userportal().await;

        let out = portal(
            &ctx,
            "delete",
            "/b/userportal/admin/buttons/btn_does_not_exist",
            InputStream::empty(),
        )
        .await;
        assert!(output_is_error(out, "NotFound").await);
        assert_eq!(audit_rows(&ctx, "portal_button.delete").await.len(), 0);
    }

    /// `logs::audit_log` writes the ADMIN block's table, and it runs under
    /// this block's WRAP identity — so the row lands only because the admin
    /// block declares a grant for `impresspress/userportal` on it. The
    /// fixture carries the grants the deployment's blocks declare and no
    /// others, so a grant dropped from `AdminBlock::info()` fails this test
    /// rather than silently turning every portal-button mutation back into
    /// an unrecorded one (the write is fire-and-forget, so a denial is a
    /// `warn!` and a green 200).
    #[tokio::test]
    async fn the_admin_grant_is_what_carries_the_row_across_the_block_boundary() {
        let base = ctx_with_userportal().await;
        let granted = base.clone().running_as("impresspress/userportal");
        portal(
            &granted,
            "create",
            "/b/userportal/admin/buttons",
            button_form("Docs", "/b/legal/"),
        )
        .await
        .collect_buffered()
        .await
        .expect("the create is answered with the table fragment");
        assert_eq!(
            audit_rows(&base, "portal_button.create").await.len(),
            1,
            "admin's declared grants must cover the portal's audit write"
        );
    }
}
