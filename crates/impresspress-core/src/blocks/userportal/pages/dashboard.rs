//! `/b/userportal/` — portal home, single-card layout.
//!
//! Anonymous → 302 to `/b/auth/login`. Authenticated → renders a centered
//! account card with: logo + "Account" header, fixed account-management
//! links (Profile / Security / Sessions / Organizations), the configured
//! app tiles from this block's `buttons` collection, and a Sign Out
//! footer. No shell, no sidebar — mobile-first.

use maud::{html, Markup};
use wafer_core::clients::database::Record;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    http::redirect,
    ui::{icons, sidebar::nav_icon, SiteConfig, UserInfo},
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

    let buttons = load_buttons(ctx).await;
    let config = SiteConfig::load(ctx).await;
    let is_admin = UserInfo::from_message(msg).is_some_and(|u| u.is_admin());

    let body = html! {
        @if is_admin {
            a .account-admin-link href="/b/admin/" {
                (icons::layout_dashboard())
                span { "Open admin panel" }
            }
        }
        ul .account-nav {
            (nav_link("/b/userportal/profile", icons::user(), "Profile"))
            (nav_link("/b/userportal/security", icons::lock(), "Security"))
            (nav_link("/b/userportal/sessions", icons::shield(), "Sessions"))
            (nav_link("/b/auth/orgs", icons::users(), "Organizations"))
            @if !buttons.is_empty() {
                hr .account-nav__divider;
                @for b in &buttons {
                    (nav_link(&b.path, nav_icon(&b.icon), &b.label))
                }
            }
        }
    };

    super::account_page(&config, "Account", None, body)
}

fn nav_link(href: &str, icon: Markup, label: &str) -> Markup {
    html! {
        li {
            a .account-nav__item href=(href) {
                span .account-nav__icon { (icon) }
                span .account-nav__label { (label) }
                span .account-nav__chev aria-hidden="true" { "›" }
            }
        }
    }
}

async fn load_buttons(ctx: &dyn Context) -> Vec<DashboardButton> {
    super::super::load_buttons(ctx)
        .await
        .into_iter()
        .map(|r: Record| DashboardButton {
            label: r.str_field("label").to_string(),
            icon: r.str_field("icon").to_string(),
            path: r.str_field("path").to_string(),
        })
        .collect()
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
        db::exec_raw(
            ctx,
            "INSERT INTO wafer_run__auth__users (id, email, display_name, role, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
            &[
                json!(user_id),
                json!(format!("{user_id}@example.com")),
                json!(user_id),
                json!("user"),
                json!("2026-01-01T00:00:00Z"),
                json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .unwrap();
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

    #[tokio::test]
    async fn no_apps_omits_divider() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        let resp = dashboard_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(
            !html.contains("account-nav__divider"),
            "divider must only render when apps are configured"
        );
    }

    #[tokio::test]
    async fn shell_chrome_is_absent() {
        let ctx = ctx_with_userportal().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/", "user-a");
        let resp = dashboard_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(
            !html.contains(r#"class="sidebar""#) && !html.contains(r#"class="topbar""#),
            "single-card layout must not render shell sidebar/topbar"
        );
    }
}
