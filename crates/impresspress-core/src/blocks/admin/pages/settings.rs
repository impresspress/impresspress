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
//! Routes:
//!   /b/admin/settings              → 308 redirect to /b/admin/settings/email
//!   /b/admin/settings/email        → email::settings_body
//!   /b/admin/settings/network      → network::settings_body
//!   /b/admin/settings/variables    → variables::settings_body
//!   /b/admin/settings/permissions  → permissions::settings_body

use wafer_run::{context::Context, Message, OutputStream};

use super::{admin_page, crumb, email, network, permissions, variables};
use crate::ui::{
    shell::Topbar,
    templates::{tabbed_page, FormSection, PageHeader},
    SiteConfig, UserInfo,
};

/// Render the settings page for the given tab. `tab` is one of
/// "email" / "network" / "variables" / "permissions"; unknown values
/// fall back to "email".
pub async fn settings_page(ctx: &dyn Context, msg: &Message, tab: &str) -> OutputStream {
    let config = SiteConfig::load(ctx).await;
    let user = UserInfo::from_message(msg);
    let path = msg.path().to_string();

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

    let form_body = tabbed_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        tabs,
        vec![FormSection {
            title: tab_title(active),
            description: tab_description(active),
            body: body_markup,
        }],
    );

    admin_page(
        "Settings",
        &config,
        &path,
        user.as_ref(),
        Topbar {
            crumbs: crumb("Settings"),
            primary_action: None,
            subtitle: Some(tab_title(active)),
            show_palette: true,
        },
        form_body,
        msg,
    )
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

    async fn render_tab(tab: &str) -> String {
        let ctx = TestContext::new().await;
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
        let ctx = TestContext::new().await;
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
