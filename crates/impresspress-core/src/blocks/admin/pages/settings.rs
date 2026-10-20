//! Consolidated settings page — puts each tab's body beside the settings
//! rail in the form-LESS `tabbed_page` shell (no outer `<form>`).
//!
//! Each tab owns its submission story: email renders the self-contained
//! `settings_form` (posts JSON to `POST /b/admin/email`), variables and
//! permissions render htmx modal forms (`POST /b/admin/variables`,
//! `POST /b/admin/grants/rules`), and network is read-only. The shell must
//! never wrap these in an outer `<form>` — HTML forms cannot nest, and the
//! browser drops a nested form's start tag, silently breaking the tab's
//! Save/Add (see the `tabbed_page` docs and the tests below).
//!
//! Routes (rows of the admin block's `ROUTES`, one literal row per tab):
//!   /b/admin/settings/             → 308 redirect to /b/admin/settings/email
//!   /b/admin/settings/email        → email::settings_body
//!   /b/admin/settings/authentication → authentication::settings_body
//!   /b/admin/settings/branding     → branding::settings_body
//!   /b/admin/settings/network      → network::settings_body
//!   /b/admin/settings/variables    → variables::settings_body
//!   /b/admin/settings/permissions  → permissions::settings_body

use wafer_run::{context::Context, Message, OutputStream, WaferError};

use super::{
    admin_document, authentication, branding, crumb, email, network, permissions, variables,
};
use crate::ui::{shell::Topbar, templates::tabbed_page};

/// Render the settings page for the given tab. `tab` is one of
/// "email" / "network" / "variables" / "permissions"; unknown values
/// fall back to "email".
///
/// Every tab body hands back a `Result`, and a failed read fails the whole
/// page through `admin_error_page`. `network` and `permissions` used to
/// swallow one into the empty table a healthy deployment with nothing
/// configured renders — "no inbound requests", "no custom grants". `email` is
/// a form, and a form filled from defaults because the stored values could
/// not be read would write those defaults back on Save.
pub async fn settings_page(ctx: &dyn Context, msg: &Message, tab: &str) -> OutputStream {
    match render(ctx, msg, tab).await {
        Ok(page) => page,
        Err(e) => {
            super::admin_error_page(
                ctx,
                msg,
                "Settings",
                e,
                "admin settings page: tab read failed",
            )
            .await
        }
    }
}

/// [`settings_page`] re-rendered by a handler whose write has landed
/// (`done`), as the htmx swap its control makes. A failed read cannot be the
/// error page here: htmx 2 swaps only a 2xx, so the pre-write page would stay
/// on screen under no sign the write happened. It is a notice saying the
/// write landed and why the page could not be reloaded, classified by
/// `crud::db_error_notice`.
pub(super) async fn settings_page_after_write(
    ctx: &dyn Context,
    msg: &Message,
    tab: &str,
    done: &str,
) -> OutputStream {
    match render(ctx, msg, tab).await {
        Ok(page) => page,
        Err(e) => after_write_notice(e, done),
    }
}

/// [`settings_page_after_write`] for a write submitted from a modal on the
/// page (Add variable, Edit variable): the re-rendered page for the form's
/// `#content` target, plus `closeModal` for `modal_id` and a `done` toast.
///
/// The modal is CLOSED by the response rather than merely swapped away with
/// `#content`, so the write ends the way every other modal write does: the
/// dialog's `close` runs, focus goes back to the control that opened it, and
/// the toast says what landed (a bare swap said nothing). Whichever of
/// `closeModal` and `showToast` htmx acts on first, the toast is announced
/// once: chrome.js drops `role="alert"` from toasts already shown before it
/// moves the container out of the dialog. A request that is not htmx's gets
/// the full page, as [`settings_page_after_write`] answers it.
pub(super) async fn settings_page_closing_modal(
    ctx: &dyn Context,
    msg: &Message,
    tab: &str,
    done: &str,
    modal_id: &str,
) -> OutputStream {
    if !crate::ui::is_htmx(msg) {
        return settings_page_after_write(ctx, msg, tab, done).await;
    }
    match render_document(ctx, msg, tab).await {
        Ok(Rendered::Page(body)) => {
            crate::ui::html_response_closing_modal(body, modal_id, done, "success")
        }
        Ok(Rendered::Moved(location)) => moved(msg, &location),
        Err(e) => after_write_notice(e, done),
    }
}

/// [`settings_page_after_write`] for a row control on the page (a
/// variable's Delete): the re-rendered page for the control's `#content`
/// target under a `done` toast. A request that is not htmx's gets the full
/// page, as [`settings_page_after_write`] answers it.
pub(super) async fn settings_page_with_toast(
    ctx: &dyn Context,
    msg: &Message,
    tab: &str,
    done: &str,
) -> OutputStream {
    if !crate::ui::is_htmx(msg) {
        return settings_page_after_write(ctx, msg, tab, done).await;
    }
    match render_document(ctx, msg, tab).await {
        Ok(Rendered::Page(body)) => crate::ui::html_response_with_toast(body, done, "success"),
        Ok(Rendered::Moved(location)) => moved(msg, &location),
        Err(e) => after_write_notice(e, done),
    }
}

/// The notice a landed write answers when the page could not be re-read.
fn after_write_notice(e: WaferError, done: &str) -> OutputStream {
    let reason = crate::blocks::crud::db_error_notice(
        e,
        "admin settings page: re-read after a write failed",
    );
    crate::ui::swap_notice_response(&format!(
        "{done}, but the settings could not be reloaded: {reason}. Reload the page to see them."
    ))
}

async fn render(ctx: &dyn Context, msg: &Message, tab: &str) -> Result<OutputStream, WaferError> {
    Ok(match render_document(ctx, msg, tab).await? {
        Rendered::Page(page) => crate::ui::html_response(page),
        Rendered::Moved(location) => moved(msg, &location),
    })
}

/// What rendering a settings URL comes to: the page, or the URL the request
/// should have asked for (a network page number past the last page).
enum Rendered {
    Page(maud::Markup),
    Moved(String),
}

/// Send the browser to `location`: a `303` for a page load, and for an htmx
/// swap an `HX-Redirect`, because an XHR follows a redirect without telling
/// htmx, which would then push the URL that was asked for, not the one shown.
fn moved(msg: &Message, location: &str) -> OutputStream {
    if crate::ui::is_htmx(msg) {
        crate::http::ResponseBuilder::new()
            .set_header("HX-Redirect", location)
            .body(Vec::new(), "text/plain")
    } else {
        crate::http::redirect(303, location)
    }
}

/// The settings page's markup (htmx-aware, as every shelled page is), or
/// where the request belongs instead.
async fn render_document(
    ctx: &dyn Context,
    msg: &Message,
    tab: &str,
) -> Result<Rendered, WaferError> {
    let active = match tab {
        "email" | "authentication" | "branding" | "network" | "variables" | "permissions" => tab,
        _ => "email",
    };

    let tabs = vec![
        (
            "Email".to_string(),
            "/b/admin/settings/email".to_string(),
            active == "email",
        ),
        (
            "Authentication".to_string(),
            authentication::AUTHENTICATION_PATH.to_string(),
            active == "authentication",
        ),
        (
            "Branding".to_string(),
            branding::BRANDING_PATH.to_string(),
            active == "branding",
        ),
        (
            "Network".to_string(),
            "/b/admin/settings/network".to_string(),
            active == "network",
        ),
        (
            "Variables".to_string(),
            "/b/admin/settings/variables".to_string(),
            active == "variables",
        ),
        (
            "Permissions".to_string(),
            "/b/admin/settings/permissions".to_string(),
            active == "permissions",
        ),
    ];

    let body_markup = match active {
        "authentication" => authentication::settings_body(ctx, msg).await,
        "branding" => branding::settings_body(ctx, msg).await,
        "network" => match network::settings_body(ctx, msg).await? {
            network::NetworkBody::Page(body) => Ok(body),
            network::NetworkBody::Moved(location) => return Ok(Rendered::Moved(location)),
        },
        "variables" => variables::settings_body(ctx, msg).await,
        "permissions" => permissions::settings_body(ctx, msg).await,
        // "email" and any unknown active (defensive — `active` is already
        // normalized above) render the email body.
        _ => email::settings_body(ctx, msg).await,
    };
    let body_markup = body_markup?;

    // A read-only tab opens with the one line that says what it shows. A
    // form tab needs none: each of its section cards carries its own.
    let body_markup = match tab_description(active) {
        Some(description) => maud::html! {
            p .settings-lead { (description) }
            (body_markup)
        },
        None => body_markup,
    };
    let form_body = tabbed_page(tabs, body_markup);

    admin_document(
        ctx,
        msg,
        "Settings",
        Topbar {
            crumbs: crumb("Settings"),
            subtitle: Some(tab_title(active)),
            ..Topbar::default()
        },
        form_body,
    )
    .await
    .map(Rendered::Page)
}

fn tab_title(active: &str) -> &'static str {
    match active {
        "email" => "Email",
        "authentication" => "Authentication",
        "branding" => "Branding",
        "network" => "Network",
        "variables" => "Variables",
        "permissions" => "Permissions",
        _ => "Settings",
    }
}

/// The lead line of a tab that is not a settings form. A form tab
/// (email, authentication, branding) describes itself in its section cards'
/// subtitles instead.
fn tab_description(active: &str) -> Option<&'static str> {
    match active {
        "network" => Some("Inbound requests by path, grouped by the block that serves them."),
        "variables" => Some("Configure environment variables and shared config."),
        "permissions" => {
            Some("Control which blocks can access other blocks' data, files, and services.")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{admin_msg, output_html, TestContext};

    /// The email tab is a form whose Save posts every field. When the stored
    /// values cannot be read the page is a 500, never that form filled from
    /// the boot map and the declared defaults — saving it would write those
    /// over the stored settings.
    #[tokio::test]
    async fn a_failed_read_renders_no_email_form() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();

        let parts = crate::blocks::admin::test_support::browser_request(
            &ctx,
            admin_msg("retrieve", "/b/admin/settings/email"),
        )
        .await;

        assert_eq!(parts.status, 500);
        let html = String::from_utf8(parts.body).expect("UTF-8 body");
        assert!(!html.contains("settings-form"), "{html}");
        assert!(html.contains("status-page--in-shell"), "{html}");
    }

    /// Maximum `<form>` nesting depth in `html`. HTML forms cannot nest —
    /// a browser drops a nested `<form>` start tag entirely (its
    /// `action`/`hx-*` attributes vanish and its inputs join the outer
    /// form), so any depth > 1 means a tab's Save/Add is broken.
    fn max_form_nesting_depth(html: &str) -> usize {
        let b = html.as_bytes();
        let (mut depth, mut max, mut i) = (0usize, 0usize, 0usize);
        while i < b.len() {
            if b[i..].starts_with(b"</form>") {
                depth = depth.saturating_sub(1);
                i += "</form>".len();
            } else if b[i..].starts_with(b"<form")
                && matches!(b.get(i + 5), Some(b' ') | Some(b'>'))
            {
                depth += 1;
                max = max.max(depth);
                i += "<form".len();
            } else {
                i += 1;
            }
        }
        max
    }

    /// Number of `<form` start tags in `html`.
    fn count_forms(html: &str) -> usize {
        html.match_indices("<form").count()
    }

    /// Every settings page render for a signed-in admin carries exactly one
    /// page-chrome form — the sidebar profile menu's logout form. Each
    /// assertion below is relative to it.
    const CHROME_FORMS: usize = 1;

    /// The admin schema has to be applied: the network and permissions tab
    /// bodies read `impresspress__admin__{request_logs,wrap_grants}`, and a
    /// fixture without them is a fixture whose page never renders. These
    /// tests ran on a bare `TestContext::new()` and passed only because both
    /// reads swallowed "no such table" into an empty table — the same
    /// swallow this PR removes.
    async fn render_tab(tab: &str) -> String {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let msg = admin_msg("retrieve", &format!("/b/admin/settings/{tab}"));
        output_html(settings_page(&ctx, &msg, tab).await).await
    }

    #[tokio::test]
    async fn no_settings_tab_renders_nested_forms() {
        for tab in ["email", "network", "variables", "permissions"] {
            let html = render_tab(tab).await;
            assert_eq!(
                max_form_nesting_depth(&html),
                1,
                "tab {tab} must not nest <form> elements"
            );
        }
    }

    #[tokio::test]
    async fn email_tab_owns_its_form_and_posts_to_the_email_save_handler() {
        let html = render_tab("email").await;
        assert_eq!(
            count_forms(&html),
            CHROME_FORMS + 1,
            "email tab renders exactly one tab-owned form"
        );
        assert!(
            html.contains("id=\"settings-form\""),
            "email tab must render the self-contained settings form: {html}"
        );
        assert!(
            html.contains("fetch(\"/b/admin/email\""),
            "email form must post to the SaveEmailSettings route (/b/admin/email), \
             which parses the JSON body it sends: {html}"
        );
        assert!(
            html.contains("type=\"submit\""),
            "email form must have a submit control"
        );
    }

    #[tokio::test]
    async fn variables_tab_add_variable_modal_form_is_present_and_not_nested() {
        let html = render_tab("variables").await;
        assert_eq!(max_form_nesting_depth(&html), 1);
        assert!(
            html.contains("hx-post=\"/b/admin/variables\""),
            "Add variable modal form must target the CreateVariable route: {html}"
        );
        assert!(
            html.contains("type=\"submit\""),
            "Add variable modal must have a submit control"
        );
    }

    #[tokio::test]
    async fn network_tab_renders_no_form_of_its_own() {
        let html = render_tab("network").await;
        // The network tab is read-only monitoring — nothing to save, so no
        // tab-owned form (and therefore no dead "Save" button) at all.
        assert_eq!(
            count_forms(&html),
            CHROME_FORMS,
            "network tab must not render any form: {html}"
        );
    }

    #[tokio::test]
    async fn permissions_database_subtab_grant_modal_form_is_not_nested() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let mut msg = admin_msg("retrieve", "/b/admin/settings/permissions");
        msg.set_meta("req.query.subtab", "database");
        let html = output_html(settings_page(&ctx, &msg, "permissions").await).await;
        assert_eq!(max_form_nesting_depth(&html), 1);
        assert!(
            html.contains("hx-post=\"/b/admin/grants/rules\""),
            "Add grant modal form must target the CreateWrapGrant route: {html}"
        );
    }

    /// The topbar subtitle names the open tab; the body never repeats it as
    /// a heading. A form tab's text is its section cards' subtitles; a
    /// read-only tab opens with a lead line.
    #[tokio::test]
    async fn the_tab_is_named_once_and_described_where_it_is_read() {
        let html = render_tab("email").await;
        assert!(
            html.contains(r#"<nav class="form-tabs" aria-label="Settings sections">"#),
            "{html}"
        );
        assert!(!html.contains(">Email</h2>"), "{html}");
        assert!(
            html.contains(r#"<h2 class="card__title">Mailgun configuration</h2><p class="card__subtitle">Configure email delivery via Mailgun.</p>"#),
            "{html}"
        );
        assert!(!html.contains("settings-lead"), "{html}");

        let html = render_tab("permissions").await;
        assert!(!html.contains(">Permissions</h2>"), "{html}");
        assert!(
            html.contains(r#"<div class="form-grid__body"><p class="settings-lead">Control which blocks can access"#),
            "{html}"
        );
    }

    /// The level of every `<hN` start tag in `html`, in document order.
    fn heading_levels(html: &str) -> Vec<u8> {
        let b = html.as_bytes();
        (0..b.len().saturating_sub(3))
            .filter(|&i| {
                b[i] == b'<'
                    && b[i + 1] == b'h'
                    && (b'1'..=b'6').contains(&b[i + 2])
                    && matches!(b[i + 3], b' ' | b'>')
            })
            .map(|i| b[i + 2] - b'0')
            .collect()
    }

    /// The topbar's h1 names the page and the tab is its subtitle, so each
    /// tab's top-level sections are h2s. A tab that still opened with the h3s
    /// it had under the deleted tab-name h2 would skip a level (h1 → h3).
    /// Network renders its groups only with traffic, so one request is
    /// logged first.
    #[tokio::test]
    async fn no_settings_tab_skips_a_heading_level() {
        use crate::platform_state::request_logs::{self, NewRequestLog};

        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        request_logs::insert(
            &ctx,
            &NewRequestLog {
                method: "GET",
                path: "/b/admin/ok",
                status_code: 200,
                error_message: "",
                duration_ms: 1,
                client_ip: "203.0.113.7",
                user_id: "",
            },
        )
        .await
        .expect("log a request");
        for (tab, subtab, section) in [
            ("email", None, r#"<h2 class="card__title">Mailgun"#),
            (
                "authentication",
                None,
                r#"<h2 class="card__title">Registration"#,
            ),
            ("branding", None, r#"<h2 class="card__title">Branding"#),
            ("network", None, r#"<h2 class="group-fold__title">"#),
            ("variables", None, r#"<h2 class="group-fold__title">"#),
            (
                "permissions",
                Some("database"),
                r#"<h2 class="card__title">Custom "#,
            ),
        ] {
            let mut msg = admin_msg("retrieve", &format!("/b/admin/settings/{tab}"));
            if let Some(subtab) = subtab {
                msg.set_meta("req.query.subtab", subtab);
            }
            let html = output_html(settings_page(&ctx, &msg, tab).await).await;
            assert!(html.contains(section), "{tab}: no {section}: {html}");
            let levels = heading_levels(&html);
            assert_eq!(levels.first(), Some(&1), "{tab}: {levels:?}");
            for pair in levels.windows(2) {
                assert!(
                    pair[1] <= pair[0] + 1,
                    "{tab}: a heading skips from h{} to h{}: {levels:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    #[test]
    fn tab_title_known_tabs() {
        assert_eq!(tab_title("email"), "Email");
        assert_eq!(tab_title("network"), "Network");
        assert_eq!(tab_title("variables"), "Variables");
        assert_eq!(tab_title("permissions"), "Permissions");
    }

    #[test]
    fn tab_title_unknown_falls_back_to_settings() {
        assert_eq!(tab_title("unknown"), "Settings");
    }
}
