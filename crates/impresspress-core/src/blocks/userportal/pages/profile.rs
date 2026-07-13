//! `/b/userportal/profile` — profile info + display-name edit form, in
//! the shared single-card layout. Sign Out lives in the card footer;
//! Change Password lives on the security page.

use maud::html;
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::auth::USERS_TABLE,
    http::redirect,
    ui::{components, SiteConfig, UserInfo},
    util::RecordExt,
};

pub async fn profile_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return redirect(302, "/b/auth/login");
    }

    let site_config = SiteConfig::load(ctx).await;
    let user = UserInfo::from_message(msg);
    let user_record = db::get(ctx, USERS_TABLE, &user_id).await.ok();
    let display_name = user_record
        .as_ref()
        .map(|r| r.str_field("name").to_string())
        .unwrap_or_default();
    let avatar_url = user_record
        .as_ref()
        .map(|r| r.str_field("avatar_url").to_string())
        .unwrap_or_default();
    let email = user.as_ref().map(|u| u.email.as_str()).unwrap_or("");

    let body = html! {
        section .account-section {
            div style="display:flex;align-items:center;gap:1rem;margin-bottom:1rem" {
                div .user-avatar style="width:56px;height:56px;font-size:1.25rem;flex-shrink:0" {
                    @if !avatar_url.is_empty() {
                        img src=(avatar_url) alt="Avatar"
                            style="width:100%;height:100%;border-radius:50%;object-fit:cover";
                    } @else if let Some(u) = &user {
                        (u.avatar_initial())
                    }
                }
                div style="flex:1;min-width:0" {
                    div style="font-weight:600;font-size:1rem" {
                        @if display_name.is_empty() { (email) } @else { (display_name) }
                    }
                    div .text-muted style="font-size:0.875rem" { (email) }
                    @if let Some(u) = &user {
                        div style="margin-top:0.375rem;display:flex;gap:0.25rem;flex-wrap:wrap" {
                            @for role in &u.roles {
                                (components::status_badge(role))
                            }
                        }
                    }
                }
            }
            form action="/b/userportal/update-profile" method="post" {
                div .form-group {
                    label .form-label for="display-name" { "Display name" }
                    input .form-input #display-name type="text" name="name"
                        value=(display_name) placeholder="Enter your name";
                }
                button .btn .btn-primary type="submit" style="width:100%" { "Save" }
            }
        }
    };

    super::account_page(&site_config, "Profile", Some("/b/userportal/"), body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{anon_msg, auth_msg, output_html, output_status, TestContext};

    #[tokio::test]
    async fn anonymous_redirects_to_login() {
        let ctx = TestContext::with_auth().await;
        let msg = anon_msg("retrieve", "/b/userportal/profile");
        let resp = profile_page(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 302);
    }

    #[tokio::test]
    async fn authenticated_renders_profile_form() {
        let ctx = TestContext::with_auth().await;
        let msg = auth_msg("retrieve", "/b/userportal/profile", "user-a");
        let resp = profile_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("Display name"), "missing edit-name form");
        assert!(
            html.contains(r#"name="name""#),
            "missing display-name field"
        );
    }

    #[tokio::test]
    async fn renders_back_link_to_dashboard() {
        let ctx = TestContext::with_auth().await;
        let msg = auth_msg("retrieve", "/b/userportal/profile", "user-a");
        let resp = profile_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(
            html.contains(r#"href="/b/userportal/""#) && html.contains("account-card__back"),
            "missing back link to dashboard"
        );
    }

    #[tokio::test]
    async fn shell_chrome_is_absent() {
        let ctx = TestContext::with_auth().await;
        let msg = auth_msg("retrieve", "/b/userportal/profile", "user-a");
        let resp = profile_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(
            !html.contains(r#"class="sidebar""#) && !html.contains(r#"class="topbar""#),
            "single-card layout must not render shell sidebar/topbar"
        );
    }
}
