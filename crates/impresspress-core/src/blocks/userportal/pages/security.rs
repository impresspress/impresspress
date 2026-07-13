//! `/b/userportal/security` — change password + linked OAuth providers
//! + email verification status.

use maud::html;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::auth::repo::{provider_links, users},
    http::redirect,
    ui::SiteConfig,
};

pub async fn security_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return redirect(302, "/b/auth/login");
    }

    let links = provider_links::list_for_user(ctx, &user_id)
        .await
        .unwrap_or_default();

    // Read verification status. On error (missing user / db hiccup), default
    // to "unverified" + log — this matches the rest of the auth block's
    // defensive style (login.rs treats missing `email_verified` as false).
    let email_verified = match users::is_email_verified(ctx, &user_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, user_id = %user_id, "failed to read email_verified");
            false
        }
    };
    let user_email = msg.get_meta("auth.user_email").to_string();

    let body = html! {
        section .account-section {
            h2 .account-section__title { "Password" }
            form
                hx-post="/b/auth/api/change-password"
                hx-target="#change-pw-result"
                hx-swap="innerHTML"
            {
                div .form-group {
                    label .form-label for="current-password" { "Current password" }
                    input .form-input #current-password type="password"
                        name="current_password" required;
                }
                div .form-group {
                    label .form-label for="new-password" { "New password" }
                    input .form-input #new-password type="password"
                        name="new_password" required;
                }
                div #change-pw-result {}
                button .btn .btn-primary type="submit" style="width:100%" { "Change password" }
            }
        }
        section .account-section {
            h2 .account-section__title { "Email verification" }
            @if email_verified {
                p .text-muted style="margin:0" {
                    "Email verified"
                    @if !user_email.is_empty() { " — " (user_email) }
                }
            } @else {
                p .text-muted style="margin:0 0 0.75rem" {
                    "Email not verified"
                    @if !user_email.is_empty() { " — " (user_email) }
                }
                div #resend-verification-result {}
                button .btn .btn-secondary
                    type="button"
                    style="width:100%"
                    onclick=(format!(
                        "(function(b){{b.disabled=true;b.textContent='Sending…';\
                         fetch('/b/auth/api/resend-verification',{{method:'POST',\
                         headers:{{'Content-Type':'application/json'}},\
                         body:JSON.stringify({{email:{email_json}}})}})\
                         .then(function(r){{return r.json();}})\
                         .then(function(d){{document.getElementById('resend-verification-result').textContent=d.message||'Sent';}})\
                         .catch(function(e){{document.getElementById('resend-verification-result').textContent='Error: '+e.message;}})\
                         .finally(function(){{b.disabled=false;b.textContent='Resend verification email';}});}})(this)",
                        email_json = serde_json::Value::String(user_email.clone())
                    ))
                { "Resend verification email" }
            }
        }
        section .account-section {
            h2 .account-section__title { "Linked accounts" }
            @if links.is_empty() {
                p .text-muted style="margin:0" {
                    "No external accounts linked. Sign in with GitHub, Google, or Microsoft to link one."
                }
            } @else {
                ul .linked-providers-list {
                    @for l in &links {
                        li .linked-provider {
                            span .linked-provider__name { (l.provider) }
                            span .linked-provider__login { (l.provider_login) }
                            span .linked-provider__date { "linked " (l.linked_at) }
                        }
                    }
                }
            }
        }
    };

    let config = SiteConfig::load(ctx).await;
    super::account_page(&config, "Security", Some("/b/userportal/"), body)
}

#[cfg(test)]
mod tests {
    use wafer_core::clients::database as db;

    use super::*;
    use crate::{
        blocks::auth::repo::provider_links::{upsert, NewLink},
        test_support::{anon_msg, auth_msg, output_html, output_status, TestContext},
    };

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        seed_user_with_verified(ctx, user_id, false).await;
    }

    async fn seed_user_with_verified(ctx: &TestContext, user_id: &str, verified: bool) {
        db::exec_raw(
            ctx,
            "INSERT INTO wafer_run__auth__users \
             (id, email, display_name, role, email_verified, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[
                serde_json::json!(user_id),
                serde_json::json!(format!("{user_id}@example.com")),
                serde_json::json!(user_id),
                serde_json::json!("user"),
                serde_json::json!(if verified { 1 } else { 0 }),
                serde_json::json!("2026-01-01T00:00:00Z"),
                serde_json::json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn anonymous_redirects_to_login() {
        let ctx = TestContext::with_auth().await;
        let msg = anon_msg("retrieve", "/b/userportal/security");
        let resp = security_page(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 302);
    }

    #[tokio::test]
    async fn renders_three_sections() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/security", "user-a");
        let resp = security_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("Password"), "missing Password section title");
        assert!(
            html.contains("Email verification"),
            "missing Email verification section title"
        );
        assert!(
            html.contains("Linked accounts"),
            "missing Linked accounts section title"
        );
    }

    #[tokio::test]
    async fn unverified_state_shows_resend_cta() {
        let ctx = TestContext::with_auth().await;
        seed_user_with_verified(&ctx, "user-a", false).await;
        let msg = auth_msg("retrieve", "/b/userportal/security", "user-a");
        let resp = security_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("Email not verified"));
        assert!(
            html.contains("Resend verification email"),
            "unverified state should show the resend CTA"
        );
        assert!(
            html.contains("/b/auth/api/resend-verification"),
            "resend CTA should target the auth block's resend endpoint"
        );
    }

    #[tokio::test]
    async fn verified_state_hides_resend_cta() {
        let ctx = TestContext::with_auth().await;
        seed_user_with_verified(&ctx, "user-a", true).await;
        let msg = auth_msg("retrieve", "/b/userportal/security", "user-a");
        let resp = security_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("Email verified"));
        assert!(
            !html.contains("Resend verification email"),
            "verified state must not render the resend CTA"
        );
    }

    #[tokio::test]
    async fn change_password_form_posts_to_existing_api() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/security", "user-a");
        let resp = security_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("/b/auth/api/change-password"));
        assert!(html.contains("name=\"current_password\""));
        assert!(html.contains("name=\"new_password\""));
    }

    #[tokio::test]
    async fn linked_accounts_empty_state() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/security", "user-a");
        let resp = security_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("No external accounts linked"));
    }

    #[tokio::test]
    async fn linked_accounts_render_when_present() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice",
                access_token: "tok",
            },
        )
        .await
        .unwrap();
        let msg = auth_msg("retrieve", "/b/userportal/security", "user-a");
        let resp = security_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("github"));
        assert!(html.contains("alice"));
    }

    // --- WRAP regression: catches a future removal of the userportal
    // grant on `auth::repo::provider_links::TABLE`. Without it, the
    // /b/userportal/security page silently renders "No external accounts
    // linked" even after a real OAuth link. PR #77 added the grant.

    #[tokio::test]
    async fn wrap_denies_provider_links_list_without_grant() {
        // Seed before opting in to WRAP — `seed_user` uses raw SQL.
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice",
                access_token: "tok",
            },
        )
        .await
        .unwrap();

        let ctx = ctx.with_wrap("impresspress/userportal", Vec::new(), "impresspress/admin");

        use crate::blocks::auth::repo::provider_links;
        let err = provider_links::list_for_user(&ctx, "user-a")
            .await
            .expect_err("WRAP must deny provider_links list_for_user without grant");
        assert!(
            format!("{err:?}").contains("WRAP"),
            "error must mention WRAP, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn wrap_allows_provider_links_list_with_auth_block_grants() {
        use crate::blocks::auth::service::auth_grants;

        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice",
                access_token: "tok",
            },
        )
        .await
        .unwrap();

        let ctx = ctx.with_wrap("impresspress/userportal", auth_grants(), "impresspress/admin");

        use crate::blocks::auth::repo::provider_links;
        let links = provider_links::list_for_user(&ctx, "user-a")
            .await
            .expect("auth's production grants must cover userportal provider_links read");
        assert_eq!(links.len(), 1);
    }
}
