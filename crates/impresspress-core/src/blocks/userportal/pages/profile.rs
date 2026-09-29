//! `/b/userportal/profile` — profile info + display-name edit form, in
//! the shared single-card layout. Sign Out lives in the card footer;
//! Change Password lives on the security page.

use maud::html;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{auth::repo::users, crud},
    http::redirect,
    ui::{self, components, SiteConfig, UserInfo},
};

pub async fn profile_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return redirect(302, "/b/auth/login");
    }

    let site_config = match SiteConfig::load(ctx).await {
        Ok(site) => site,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "page: site config read failed")
        }
    };
    let user = UserInfo::from_message(msg);
    // `UserRow.display_name`, not the `name` alias this page used to read:
    // both are written together by `users::insert` and
    // `users::update_profile`, and `display_name` is the column migration
    // 001 declares NOT NULL, so it is the one that is always populated.
    //
    // The form below is pre-filled from this row and posts every field back,
    // so it is never rendered without it: a form built from a blank default
    // would write that blank over the real name on Save. A missing row for a
    // signed-in user is the same internal inconsistency
    // `handle_update_profile` reports, not an empty profile.
    let row = match users::find_by_id(ctx, &user_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            tracing::error!(user_id = %user_id, "userportal profile: signed-in user has no users row");
            return ui::server_error_response(msg);
        }
        Err(e) => return crud::db_error_page(msg, e, "userportal profile: user read failed"),
    };
    let display_name = row.display_name;
    let avatar_url = row.avatar_url.unwrap_or_default();
    let email = user.as_ref().map(|u| u.email.as_str()).unwrap_or("");

    let body = html! {
        section .account-section {
            div .profile-header {
                div .user-avatar .user-avatar--lg {
                    @if !avatar_url.is_empty() {
                        img src=(avatar_url) alt="Avatar";
                    } @else if let Some(u) = &user {
                        (u.avatar_initial())
                    }
                }
                div .profile-header__meta {
                    div .font-semibold .text-16 {
                        @if display_name.is_empty() { (email) } @else { (display_name) }
                    }
                    div .text-muted .text-sm { (email) }
                    @if let Some(u) = &user {
                        div .profile-header__roles {
                            @for role in &u.roles {
                                (components::status_badge(role))
                            }
                        }
                    }
                }
            }
            form action="/b/userportal/update-profile" method="post" {
                (crate::csrf::hidden_field(ctx, msg))

                div .form-group {
                    label .form-label for="display-name" { "Display name" }
                    // `required` + `pattern`: the update handler refuses an
                    // empty or whitespace-only name, so the browser does too.
                    input .form-input #display-name type="text" name="name"
                        value=(display_name) placeholder="Enter your name" required
                        pattern=".*\\S.*" title="Enter a name that is not just spaces";
                }
                button .btn .btn--primary type="submit" .w-full { "Save" }
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        let msg = anon_msg("retrieve", "/b/userportal/profile");
        let resp = profile_page(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 302);
    }

    #[tokio::test]
    async fn authenticated_renders_profile_form() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        ctx.seed_auth_user("user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/profile", "user-a");
        let resp = profile_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("Display name"), "missing edit-name form");
        assert!(
            html.contains(r#"name="name""#),
            "missing display-name field"
        );
        assert!(
            html.contains(r#"required pattern=".*\S.*""#),
            "the name field must refuse blank and whitespace-only input"
        );
    }

    #[tokio::test]
    async fn renders_back_link_to_dashboard() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        ctx.seed_auth_user("user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/profile", "user-a");
        let resp = profile_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(
            html.contains(r#"href="/b/userportal/""#) && html.contains("account-card__back"),
            "missing back link to dashboard"
        );
    }

    /// A failed user read is the 500 page, never the form. The form is
    /// pre-filled from the row and posts `name` back, so rendering it from a
    /// blank default (`value=""`) set the user up to wipe their own name on
    /// Save.
    #[tokio::test]
    async fn a_failed_user_read_is_a_500_without_the_form() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        ctx.seed_auth_user("user-a").await;
        let ctx = ctx.break_reads();

        let (status, html) = crate::blocks::userportal::test_support::browser_request(
            &ctx,
            auth_msg("retrieve", "/b/userportal/profile", "user-a"),
            "",
        )
        .await;

        assert_eq!(status, 500);
        assert!(
            !html.contains(r#"name="name""#),
            "the edit form must not render without the user's row:\n{html}"
        );
    }

    /// A signed-in user with no row is an inconsistency, not an empty
    /// profile: same answer as the failed read, for the same reason.
    #[tokio::test]
    async fn a_missing_user_row_is_a_500_without_the_form() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);

        let (status, html) = crate::blocks::userportal::test_support::browser_request(
            &ctx,
            auth_msg("retrieve", "/b/userportal/profile", "user-a"),
            "",
        )
        .await;

        assert_eq!(status, 500);
        assert!(!html.contains(r#"name="name""#), "{html}");
    }

    #[tokio::test]
    async fn shell_chrome_is_absent() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        ctx.seed_auth_user("user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/profile", "user-a");
        let resp = profile_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(
            !html.contains(r#"class="sidebar""#) && !html.contains(r#"class="topbar""#),
            "single-card layout must not render shell sidebar/topbar"
        );
    }
}
