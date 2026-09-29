//! Consolidated settings page — wraps each tab's body in the form-LESS
//! `tabbed_page` shell (tab rail + section chrome, no outer `<form>`).
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
//!   /b/admin/settings/network      → network::settings_body
//!   /b/admin/settings/variables    → variables::settings_body
//!   /b/admin/settings/permissions  → permissions::settings_body

use wafer_run::{context::Context, Message, OutputStream, WaferError};

use super::{admin_page, crumb, email, network, permissions, variables};
use crate::ui::{
    shell::Topbar,
    templates::{tabbed_page, FormSection},
};

/// Render the settings page for the given tab. `tab` is one of
/// "email" / "network" / "variables" / "permissions"; unknown values
/// fall back to "email".
///
/// Every tab body hands back a `Result`, and a failed read fails the whole
/// page through `crud::db_error_page`. `network` and `permissions` used to
/// swallow one into the empty table a healthy deployment with nothing
/// configured renders — "no inbound requests", "no custom grants". `email` is
/// a form, and a form filled from defaults because the stored values could
/// not be read would write those defaults back on Save.
pub async fn settings_page(ctx: &dyn Context, msg: &Message, tab: &str) -> OutputStream {
    match render(ctx, msg, tab).await {
        Ok(page) => page,
        Err(e) => {
            crate::blocks::crud::db_error_page(msg, e, "admin settings page: tab read failed")
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
        Err(e) => {
            let reason = crate::blocks::crud::db_error_notice(
                e,
                "admin settings page: re-read after a write failed",
            );
            crate::ui::swap_notice_response(&format!(
                "{done}, but the settings could not be reloaded: {reason}. Reload the page to \
                 see them."
            ))
        }
    }
}

async fn render(ctx: &dyn Context, msg: &Message, tab: &str) -> Result<OutputStream, WaferError> {
    let active = match tab {
        "email" | "network" | "variables" | "permissions" => tab,
        _ => "email",
    };

    let tabs = vec![
        (
            "Email".to_string(),
            "/b/admin/settings/email".to_string(),
            active == "email",
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
        "network" => network::settings_body(ctx, msg).await,
        "variables" => variables::settings_body(ctx, msg).await,
        "permissions" => permissions::settings_body(ctx, msg).await,
        // "email" and any unknown active (defensive — `active` is already
        // normalized above) render the email body.
        _ => email::settings_body(ctx, msg).await,
    };
    let body_markup = body_markup?;

    let form_body = tabbed_page(
        tabs,
        vec![FormSection {
            title: tab_title(active),
            description: tab_description(active),
            body: body_markup,
        }],
    );

    Ok(admin_page(
        ctx,
        msg,
        "Settings",
        Topbar {
            crumbs: crumb("Settings"),
            primary_action: None,
            subtitle: Some(tab_title(active)),
            show_palette: true,
        },
        form_body,
    )
    .await)
}

fn tab_title(active: &str) -> &'static str {
    match active {
        "email" => "Email",
        "network" => "Network",
        "variables" => "Variables",
        "permissions" => "Permissions",
        _ => "Settings",
    }
}

fn tab_description(active: &str) -> Option<&'static str> {
    match active {
        "email" => Some("Configure email delivery via Mailgun."),
        "network" => Some("Manage network access rules for blocks."),
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
        assert!(!html.contains("<form"), "{html}");
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
            assert!(
                !html.contains("<form class=\"form-page\""),
                "tab {tab}: the settings shell must not wrap tab bodies in an outer <form>"
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
            "Add Variable modal form must target the CreateVariable route: {html}"
        );
        assert!(
            html.contains("type=\"submit\""),
            "Add Variable modal must have a submit control"
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
            "Add Grant modal form must target the CreateWrapGrant route: {html}"
        );
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
