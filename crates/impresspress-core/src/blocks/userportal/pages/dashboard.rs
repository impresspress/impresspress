//! `/b/userportal/` — the portal's Overview page, in the portal shell.
//!
//! Anonymous → 302 to `/b/auth/login`. Authenticated → the account pages
//! (Profile / Security / Sessions / Organizations), each with one line on what
//! it holds, then the app links configured in this block's `buttons`
//! collection. An admin also gets "Open admin panel" in the topbar. Sign Out
//! is the shell's profile menu, as on every shelled page.

use maud::{html, Markup};
use wafer_core::clients::database::Record;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::crud,
    http::redirect,
    ui::{self, components::section_header, icons, sidebar::nav_icon, Shell, UserInfo},
    util::RecordExt,
};

struct DashboardButton {
    label: String,
    icon: String,
    path: String,
}

/// GET `/b/userportal/`. Anonymous users redirected to login.
pub async fn dashboard_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return redirect(302, "/b/auth/login");
    }

    // The app links are this page's one read. Rendering without them would
    // look like "no apps configured", so a failed read is an error page.
    let buttons = match load_buttons(ctx).await {
        Ok(buttons) => buttons,
        Err(e) => return crud::db_error_page(msg, e, "userportal dashboard: buttons read failed"),
    };
    let is_admin = UserInfo::from_message(msg).is_some_and(|u| u.is_admin());

    // Two lists, each under its own heading: an `hr` between the account
    // links and the app links inside one `ul` was not a list item (axe
    // `list`).
    let body = html! {
        div .account-sections {
            section .account-section {
                (section_header("Account", None))
                ul .account-nav {
                    (nav_link("/b/userportal/profile", icons::user(), "Profile", Some("Your name and avatar")))
                    (nav_link("/b/userportal/security", icons::lock(), "Security", Some("Password, email verification and linked accounts")))
                    (nav_link("/b/userportal/sessions", icons::shield(), "Sessions", Some("Devices signed in to your account")))
                    (nav_link("/b/auth/orgs", icons::users(), "Organizations", Some("Organizations you have claimed")))
                }
            }
            @if !buttons.is_empty() {
                section .account-section {
                    (section_header("Apps", None))
                    ul .account-nav {
                        @for b in &buttons {
                            (nav_link(&b.path, nav_icon(&b.icon), &b.label, None))
                        }
                    }
                }
            }
        }
    };

    let actions = if is_admin {
        vec![html! {
            a .btn .btn--secondary href="/b/admin/" {
                (icons::layout_dashboard())
                span { "Open admin panel" }
            }
        }]
    } else {
        Vec::new()
    };

    ui::shell_page(
        ctx,
        msg,
        Shell::portal("Overview", "Overview")
            .subtitle("Your account and apps.")
            .actions(actions),
        body,
    )
    .await
}

fn nav_link(href: &str, icon: Markup, label: &str, description: Option<&str>) -> Markup {
    html! {
        li {
            a .account-nav__item href=(href) {
                span .account-nav__icon aria-hidden="true" { (icon) }
                span .account-nav__text {
                    span .account-nav__label { (label) }
                    @if let Some(d) = description {
                        span .account-nav__desc { (d) }
                    }
                }
                span .account-nav__chev aria-hidden="true" { (icons::chevron_right()) }
            }
        }
    }
}

async fn load_buttons(ctx: &dyn Context) -> Result<Vec<DashboardButton>, wafer_run::WaferError> {
    Ok(super::super::load_buttons(ctx)
        .await?
        .into_iter()
        .map(|r: Record| DashboardButton {
            label: r.str_field("label").to_string(),
            icon: r.str_field("icon").to_string(),
            path: r.str_field("path").to_string(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use serde_json::json;
    use wafer_core::clients::database as db;

    use super::*;
    use crate::{
        blocks::userportal::UserPortalBlock,
        test_support::{
            anon_msg, auth_msg, output_header, output_html, output_status, TestContext,
        },
    };

    async fn ctx_with_userportal() -> TestContext {
        let mut ctx = TestContext::with_userportal().await;
        ctx.register_block("impresspress/userportal", Arc::new(UserPortalBlock::new()));
        ctx
    }

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        ctx.seed_auth_user(user_id).await;
    }

    fn button_data(
        label: &str,
        icon: &str,
        path: &str,
        sort_order: i64,
    ) -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert("label".to_string(), json!(label));
        m.insert("icon".to_string(), json!(icon));
        m.insert("path".to_string(), json!(path));
        m.insert("sort_order".to_string(), json!(sort_order));
        m
    }

    #[tokio::test]
    async fn anonymous_redirects_to_login() {
        let ctx = ctx_with_userportal().await;
        let msg = anon_msg("retrieve", "/b/userportal/");
        let resp = dashboard_page(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 302);
    }

    #[tokio::test]
    async fn anonymous_redirect_sets_location_header() {
        let ctx = ctx_with_userportal().await;
        let msg = anon_msg("retrieve", "/b/userportal/");
        let resp = dashboard_page(&ctx, &msg).await;
        assert_eq!(
            output_header(resp, "Location").await.as_deref(),
            Some("/b/auth/login")
        );
    }

    #[tokio::test]
    async fn authenticated_returns_200() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        let resp = dashboard_page(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 200);
    }

    #[tokio::test]
    async fn renders_account_links() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        let resp = dashboard_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        for (href, label) in [
            ("/b/userportal/profile", "Profile"),
            ("/b/userportal/security", "Security"),
            ("/b/userportal/sessions", "Sessions"),
            ("/b/auth/orgs", "Organizations"),
        ] {
            assert!(
                html.contains(href) && html.contains(label),
                "missing account link {label} -> {href}"
            );
        }
    }

    #[tokio::test]
    async fn renders_signout_form() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        let resp = dashboard_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(
            html.contains("/b/auth/api/logout") && html.contains("Sign Out"),
            "missing sign-out form"
        );
    }

    #[tokio::test]
    async fn renders_configured_app_tiles() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        db::create(
            &ctx,
            "impresspress__userportal__buttons",
            button_data("Files", "folder", "/b/storage/", 0),
        )
        .await
        .unwrap();
        let msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        let resp = dashboard_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("Files") && html.contains("/b/storage/"));
    }

    /// An unreadable buttons table is the 500 page, not an overview with
    /// the app links silently missing.
    #[tokio::test]
    async fn a_failed_buttons_read_is_a_500_not_a_card_without_tiles() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        db::create(
            &ctx,
            "impresspress__userportal__buttons",
            button_data("Files", "folder", "/b/storage/", 0),
        )
        .await
        .unwrap();
        let ctx = ctx.break_list_reads();

        let (status, html) = crate::blocks::userportal::test_support::browser_request(
            &ctx,
            auth_msg("retrieve", "/b/userportal/", "user-a"),
            "",
        )
        .await;

        assert_eq!(status, 500);
        assert!(!html.contains("account-nav"), "{html}");
    }

    /// The account links and the app links are two lists under their own
    /// headings; nothing but `li` sits inside either `ul` (axe `list`).
    #[tokio::test]
    async fn account_and_app_links_are_two_lists_with_no_hr() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        db::create(
            &ctx,
            "impresspress__userportal__buttons",
            button_data("Files", "folder", "/b/storage/", 0),
        )
        .await
        .unwrap();
        let msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        let html = output_html(dashboard_page(&ctx, &msg).await).await;
        assert!(!html.contains("<hr"), "{html}");
        assert_eq!(html.matches(r#"<ul class="account-nav">"#).count(), 2);
        assert!(html.contains(r#"<h2 class="section-header__title">Apps</h2>"#));
    }

    #[tokio::test]
    async fn no_apps_omits_the_apps_section() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        let html = output_html(dashboard_page(&ctx, &msg).await).await;
        assert!(!html.contains(">Apps</h2>"), "{html}");
    }

    /// The portal shell frames the page, as on every account page: the
    /// sidebar, the topbar with the page's one `h1`, and no account card.
    #[tokio::test]
    async fn renders_inside_the_portal_shell() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        let html = output_html(dashboard_page(&ctx, &msg).await).await;
        assert!(html.contains(r#"<nav class="sidebar""#), "{html}");
        assert!(
            html.contains(r#"<h1 class="topbar__title">Overview</h1>"#),
            "{html}"
        );
        assert_eq!(html.matches("<h1").count(), 1);
        assert!(!html.contains("account-card"), "{html}");
    }

    #[tokio::test]
    async fn an_admin_gets_the_admin_panel_action() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        let mut msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        msg.set_meta("auth.user_roles", "admin");
        let html = output_html(dashboard_page(&ctx, &msg).await).await;
        assert!(html.contains("Open admin panel"), "{html}");

        let msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        let html = output_html(dashboard_page(&ctx, &msg).await).await;
        assert!(!html.contains("Open admin panel"), "{html}");
    }
}
