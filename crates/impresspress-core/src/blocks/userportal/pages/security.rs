//! `/b/userportal/security` — change password + linked OAuth providers
//! + email verification status.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{
        auth::repo::{local_credentials, provider_links, users},
        crud,
    },
    http::{redirect, ResponseBuilder},
    ui::SiteConfig,
};

/// The resend-verification button's behaviour, delegated.
///
/// It was a single `onclick` attribute built with `format!` — an IIFE with the
/// signed-in address `serde_json`-encoded into a JavaScript string literal.
/// Nothing could break out of it, but it is the shape the rule at
/// `blocks/admin/pages/network.rs` warns about, and it was the only place in
/// the tree that put a user's email into executable source.
const RESEND_VERIFICATION_JS: &str = r#"
(function () {
  if (window.__resendVerificationInit) return;
  window.__resendVerificationInit = true;
  document.addEventListener('click', function (e) {
    if (!(e.target instanceof Element)) return;
    var btn = e.target.closest('[data-action="resend-verification"]');
    if (!btn) return;
    var result = document.getElementById('resend-verification-result');
    btn.disabled = true;
    btn.textContent = 'Sending…';
    fetch('/b/auth/api/resend-verification', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email: btn.getAttribute('data-verify-email') || '' })
    })
      .then(function (r) { return r.json(); })
      .then(function (d) { if (result) result.textContent = d.message || 'Sent'; })
      .catch(function (err) { if (result) result.textContent = 'Error: ' + err.message; })
      .finally(function () {
        btn.disabled = false;
        btn.textContent = 'Resend verification email';
      });
  });
})();
"#;

pub async fn security_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return redirect(302, "/b/auth/login");
    }

    // Both reads fail closed to an error page. An unreadable link list would
    // render "No external accounts linked" — hiding exactly the sign-in
    // routes this page exists to let a user audit — and an unreadable
    // verification flag would tell a verified user they are not, with a
    // Resend button beside it.
    let links = match provider_links::list_for_user(ctx, &user_id).await {
        Ok(links) => links,
        Err(e) => {
            return crud::db_error_page(msg, e, "userportal security: provider links read failed")
        }
    };
    let email_verified = match users::is_email_verified(ctx, &user_id).await {
        Ok(v) => v,
        Err(e) => {
            return crud::db_error_page(msg, e, "userportal security: email_verified read failed")
        }
    };
    let user_email = msg.get_meta("auth.user_email").to_string();

    let body = html! {
        section .account-section {
            h2 .account-section__title { "Password" }
            // A refusal comes back as markup for `#change-pw-result`
            // (`auth_ui::api::change_password::refused`), so the only
            // responses that reach the `htmx:responseError` listener are the
            // ones with no sentence of their own — a dead session, an outage.
            // `data-error-label` is what stops those from being toasted as
            // "Request failed (401)"; see the label rule in
            // `ui/assets/chrome.js`.
            form
                hx-post="/b/auth/api/change-password"
                hx-target="#change-pw-result"
                hx-swap="innerHTML"
                data-error-label="Could not change your password"
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
                button .btn .btn--primary type="submit" .w-full { "Change password" }
            }
        }
        section .account-section {
            h2 .account-section__title { "Email verification" }
            @if email_verified {
                p .text-muted .m-0 {
                    "Email verified"
                    @if !user_email.is_empty() { " — " (user_email) }
                }
            } @else {
                p .text-muted .m-0 .mb-3 {
                    "Email not verified"
                    @if !user_email.is_empty() { " — " (user_email) }
                }
                div #resend-verification-result {}
                // The address travels as an attribute the script reads back
                // with `getAttribute`, not as a `serde_json`-escaped literal
                // spliced into JavaScript source. See the delegated-action
                // rule in `ui/assets/chrome.js`.
                button .btn .btn--secondary
                    type="button"
                    .w-full
                    data-action="resend-verification"
                    data-verify-email=(user_email)
                { "Resend verification email" }
                script { (maud::PreEscaped(RESEND_VERIFICATION_JS)) }
            }
        }
        section .account-section {
            h2 .account-section__title { "Linked accounts" }
            @if links.is_empty() {
                p .text-muted .m-0 {
                    "No external accounts linked. Sign in with GitHub, Google, or Microsoft to link one."
                }
            } @else {
                p .text-muted .m-0 .mb-3 .text-sm {
                    "Anyone who can sign in to one of these can sign in to this \
                     account. Unlink any you do not recognize."
                }
                ul .linked-providers-list {
                    @for l in &links {
                        (linked_provider_row(l, None))
                    }
                }
            }
        }
    };

    let config = match SiteConfig::load(ctx).await {
        Ok(site) => site,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "page: site config read failed")
        }
    };
    super::account_page(&config, "Security", Some("/b/userportal/"), body)
}

/// One linked-provider row, optionally carrying the reason its last unlink was
/// refused.
///
/// Rendered by the page and again by [`handle_unlink`], which swaps this row
/// in place of itself: htmx does not swap a non-2xx response, so a refusal has
/// to come back as the row it declined to remove, carrying the reason.
fn linked_provider_row(link: &provider_links::ProviderLink, error: Option<&str>) -> Markup {
    html! {
        li .linked-provider {
            span .linked-provider__name { (link.provider) }
            span .linked-provider__login { (link.provider_login) }
            span .linked-provider__date { "linked " (link.linked_at) }
            button .btn .btn--ghost .btn--sm
                type="button"
                hx-delete=(format!("/b/userportal/security/providers/{}", link.provider))
                hx-target="closest li"
                hx-swap="outerHTML"
                hx-confirm=(format!(
                    "Unlink {}? You will no longer be able to sign in with it.",
                    link.provider
                ))
            { "Unlink" }
            @if let Some(message) = error {
                p .form-error .m-0 { (message) }
            }
        }
    }
}

/// DELETE `/b/userportal/security/providers/{provider}` — remove one of the
/// caller's OAuth links.
///
/// The eviction half of account recovery. A provider that asserts nothing
/// about the address it returns can bind itself to an account, and until this
/// existed there was no way to undo that from anywhere in the product: the
/// rightful owner could reset the password and prove the address, and the
/// other identity kept a working sign-in beside them. Sessions already had
/// this (`pages::sessions::handle_revoke`); links did not.
///
/// Scoped to the caller in the statement, so naming somebody else's provider
/// deletes nothing and is answered exactly like naming one you do not have.
///
/// Refuses to remove the last way in. An account with no password credential
/// and one link would be locked out of itself by a single click — and on a
/// deployment that requires verification it could not necessarily reset its
/// way back either. The refusal names the fix ("set a password first"), which
/// the form directly above it performs.
pub async fn handle_unlink(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return ResponseBuilder::new()
            .status(401)
            .set_header("WWW-Authenticate", crate::http::WWW_AUTHENTICATE)
            .body(b"unauthenticated".to_vec(), "text/plain");
    }
    let provider = msg.var("provider").to_string();
    if provider.is_empty() {
        return ResponseBuilder::new()
            .status(400)
            .body(b"bad provider".to_vec(), "text/plain");
    }

    let links = match provider_links::list_for_user(ctx, &user_id).await {
        Ok(l) => l,
        Err(e) => return crud::db_error_internal(e, "Could not read your linked accounts"),
    };
    let Some(target) = links.iter().find(|l| l.provider == provider) else {
        // Not linked to this caller — indistinguishable from someone else's
        // link, on purpose. htmx removes the row either way.
        return ResponseBuilder::new()
            .status(200)
            .body(Vec::new(), "text/html");
    };

    if links.len() == 1 {
        let has_password = match local_credentials::has_password(ctx, &user_id).await {
            Ok(has) => has,
            Err(e) => return crud::db_error_internal(e, "Could not check your sign-in methods"),
        };
        if !has_password {
            return ResponseBuilder::new().status(200).body(
                linked_provider_row(
                    target,
                    Some(
                        "This is the only way you can sign in. Set a password first, \
                         then unlink it.",
                    ),
                )
                .into_string()
                .into_bytes(),
                "text/html",
            );
        }
    }

    if let Err(e) = provider_links::delete_for_user(ctx, &user_id, &provider).await {
        return crud::db_error_internal(e, "Could not unlink that account");
    }
    ResponseBuilder::new()
        .status(200)
        .body(Vec::new(), "text/html")
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{
        blocks::auth::repo::provider_links::{upsert, NewLink},
        test_support::{anon_msg, auth_msg, output_html, output_json, output_status, TestContext},
    };

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        seed_user_with_verified(ctx, user_id, false).await;
    }

    async fn seed_user_with_verified(ctx: &TestContext, user_id: &str, verified: bool) {
        ctx.seed_auth_user_verified(user_id, verified).await;
    }

    #[tokio::test]
    async fn anonymous_redirects_to_login() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        let msg = anon_msg("retrieve", "/b/userportal/security");
        let resp = security_page(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 302);
    }

    #[tokio::test]
    async fn renders_three_sections() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/security", "user-a");
        let resp = security_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("/b/auth/api/change-password"));
        assert!(html.contains("name=\"current_password\""));
        assert!(html.contains("name=\"new_password\""));
        // The responses that reach the `htmx:responseError` listener are the
        // ones carrying no sentence for this form — a dead session, an
        // outage. Without a label they are toasted as the status alone.
        assert!(
            html.contains(r#"data-error-label="Could not change your password""#),
            "the form must name itself for the error toast"
        );
    }

    /// The form and the endpoint it posts to are one surface, and asserting
    /// on the rendered strings alone does not check that they still meet:
    /// this form is htmx, so it sends `application/x-www-form-urlencoded`
    /// under those names, and the handler read the body as JSON only — every
    /// post this page made was answered `400 Invalid body` and no password
    /// ever changed.
    ///
    /// So the bytes go through: the field names above, form-encoded, into the
    /// real handler, proved by signing in afterwards with the new password.
    #[tokio::test]
    async fn the_change_password_form_is_answered_by_the_endpoint_it_posts_to() {
        use wafer_run::InputStream;

        use crate::blocks::auth_ui::api::{change_password, login, signup};

        const OLD: &str = "original-horse-battery1";
        const NEW: &str = "new-horse-battery-2026";
        const EMAIL: &str = "portal-user@example.com";

        // A password change needs a credential row, so the account is made
        // the way a real one is.
        let ctx = TestContext::with_auth_and_crypto().await;
        let (limiter, mail_msg) = crate::blocks::auth_ui::api::test_mail_request();
        let signed_up = output_json(
            signup::handle(
                &limiter,
                &ctx,
                &mail_msg,
                InputStream::from_bytes(
                    serde_json::json!({ "email": EMAIL, "password": OLD })
                        .to_string()
                        .into_bytes(),
                ),
            )
            .await,
        )
        .await;
        let user_id = signed_up["user"]["id"]
            .as_str()
            .expect("signup response carries user.id")
            .to_string();

        let page = security_page(
            &ctx,
            &auth_msg("retrieve", "/b/userportal/security", &user_id),
        )
        .await;
        let html = output_html(page).await;
        assert!(html.contains(r#"hx-post="/b/auth/api/change-password""#));

        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("current_password", OLD)
            .append_pair("new_password", NEW)
            .finish();
        let mut post = auth_msg("update", "/b/auth/api/change-password", &user_id);
        post.set_meta("http.header.hx-request", "true");
        let answer =
            change_password::handle(&ctx, &post, InputStream::from_bytes(form.into_bytes())).await;
        assert_eq!(
            output_status(answer).await,
            200,
            "the page's own form post must be accepted"
        );

        let signed_in = output_json(
            login::handle(
                &ctx,
                InputStream::from_bytes(
                    serde_json::json!({ "email": EMAIL, "password": NEW })
                        .to_string()
                        .into_bytes(),
                ),
            )
            .await,
        )
        .await;
        assert!(
            signed_in["access_token"]
                .as_str()
                .is_some_and(|t| !t.is_empty()),
            "the new password must authenticate"
        );
    }

    #[tokio::test]
    async fn linked_accounts_empty_state() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/security", "user-a");
        let resp = security_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("No external accounts linked"));
    }

    #[tokio::test]
    async fn linked_accounts_render_when_present() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice",
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

    /// An unreadable provider-link list is the 500 page. Rendering it as
    /// "No external accounts linked" hides exactly the sign-in routes this
    /// page lets a user audit and unlink.
    #[tokio::test]
    async fn a_failed_link_read_is_a_500_not_no_linked_accounts() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        link(&ctx, "user-a", "github", "gh-1").await;
        let ctx = ctx.break_list_reads();

        let (status, html) = crate::blocks::userportal::test_support::browser_request(
            &ctx,
            auth_msg("retrieve", "/b/userportal/security", "user-a"),
            "",
        )
        .await;

        assert_eq!(status, 500);
        assert!(!html.contains("No external accounts linked"), "{html}");
    }

    /// An unreadable verification flag is the 500 page. Defaulting it to
    /// `false` told a verified user they were not, and offered to resend.
    /// Only the users-row `get` fails, so the link list above it still reads
    /// and the flag is the one thing that decides the response.
    ///
    /// Names `users::TABLE` only to aim the fault injector;
    /// `tests/repo_door.rs` allowlists it as one.
    #[tokio::test]
    async fn a_failed_verification_read_is_a_500_not_unverified() {
        use crate::test_support::FailingDbOpContext;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user_with_verified(&ctx, "user-a", true).await;
        let failing = FailingDbOpContext::new(ctx, vec![("database.get", users::TABLE)]);

        let (status, html) = crate::blocks::userportal::test_support::browser_request(
            &failing,
            auth_msg("retrieve", "/b/userportal/security", "user-a"),
            "",
        )
        .await;

        assert_eq!(status, 500);
        assert!(!html.contains("Email not verified"), "{html}");
        assert!(!html.contains("Resend verification email"), "{html}");
    }

    // --- WRAP regression: catches a future removal of the userportal
    // grant on `auth::repo::provider_links::TABLE`. Without it, the
    // /b/userportal/security page answers the 500 page for every
    // authenticated user. PR #77 added the grant.

    #[tokio::test]
    async fn wrap_denies_provider_links_list_to_a_block_without_a_grant() {
        // Seeded from the fixture's own frame — `seed_user` uses raw SQL.
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice",
            },
        )
        .await
        .unwrap();

        let ctx = ctx.running_as("test/ungranted");

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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice",
            },
        )
        .await
        .unwrap();

        let ctx = ctx.running_as("impresspress/userportal");

        use crate::blocks::auth::repo::provider_links;
        let links = provider_links::list_for_user(&ctx, "user-a")
            .await
            .expect("auth's production grants must cover userportal provider_links read");
        assert_eq!(links.len(), 1);
    }

    // -----------------------------------------------------------------
    // Unlink — the eviction half of account recovery
    // -----------------------------------------------------------------

    async fn link(ctx: &TestContext, user_id: &str, provider: &str, reference: &str) {
        upsert(
            ctx,
            NewLink {
                provider,
                provider_ref: reference,
                user_id,
                provider_login: "someone",
            },
        )
        .await
        .unwrap();
    }

    fn unlink_msg(user_id: &str, provider: &str) -> Message {
        let mut msg = auth_msg(
            "delete",
            &format!("/b/userportal/security/providers/{provider}"),
            user_id,
        );
        msg.set_meta("req.param.provider", provider);
        msg
    }

    /// The page has to offer the action at all — a link nobody can remove is
    /// the state that made a squatted account unrecoverable.
    #[tokio::test]
    async fn the_page_offers_an_unlink_for_each_link() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        link(&ctx, "user-a", "github", "gh-1").await;

        let html = output_html(
            security_page(
                &ctx,
                &auth_msg("retrieve", "/b/userportal/security", "user-a"),
            )
            .await,
        )
        .await;
        assert!(
            html.contains("/b/userportal/security/providers/github"),
            "the linked-accounts row must offer an unlink: {html}"
        );
        assert!(
            html.contains("Unlink"),
            "missing the unlink control: {html}"
        );
    }

    /// Unlinking removes the row. With a password on the account this is the
    /// owner evicting somebody else's provider identity.
    #[tokio::test]
    async fn unlink_removes_the_link_when_a_password_remains() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        link(&ctx, "user-a", "github", "gh-1").await;
        local_credentials::insert(&ctx.fixture(), "user-a", "hash", false)
            .await
            .expect("seed password");

        let status =
            output_status(handle_unlink(&ctx, &unlink_msg("user-a", "github")).await).await;
        assert_eq!(status, 200);
        assert!(
            provider_links::list_for_user(&ctx, "user-a")
                .await
                .unwrap()
                .is_empty(),
            "the link must be gone, or the other party keeps a way in"
        );
    }

    /// Scoped to the caller: naming another user's provider deletes nothing,
    /// and answers exactly like naming one you do not have.
    #[tokio::test]
    async fn unlink_cannot_reach_another_users_link() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        seed_user(&ctx, "user-b").await;
        link(&ctx, "user-b", "github", "gh-b").await;
        local_credentials::insert(&ctx.fixture(), "user-a", "hash", false)
            .await
            .expect("seed password");

        let status =
            output_status(handle_unlink(&ctx, &unlink_msg("user-a", "github")).await).await;
        assert_eq!(status, 200, "no answer that distinguishes the two cases");
        assert_eq!(
            provider_links::list_for_user(&ctx, "user-b")
                .await
                .unwrap()
                .len(),
            1,
            "another user's link must survive"
        );
    }

    /// An account whose only way in is the link cannot click itself out of
    /// existence. The refusal comes back as the row it declined to remove —
    /// htmx does not swap a non-2xx — carrying the fix.
    #[tokio::test]
    async fn unlink_refuses_to_remove_the_last_way_in() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        link(&ctx, "user-a", "github", "gh-1").await;
        // No local_credentials row: this account has no password.

        let html = output_html(handle_unlink(&ctx, &unlink_msg("user-a", "github")).await).await;
        assert!(
            html.contains("Set a password first"),
            "the refusal must name the fix: {html}"
        );
        assert_eq!(
            provider_links::list_for_user(&ctx, "user-a")
                .await
                .unwrap()
                .len(),
            1,
            "the last sign-in method must survive the refusal"
        );
    }

    #[tokio::test]
    async fn unlink_is_unauthenticated_without_a_session() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        let mut msg = anon_msg("delete", "/b/userportal/security/providers/github");
        msg.set_meta("req.param.provider", "github");
        let resp = handle_unlink(&ctx, &msg).await;
        let parts = wafer_block::http_codec::collect_http_response(resp).await;
        assert_eq!(parts.status, 401);
        assert!(
            parts.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("WWW-Authenticate")
                    && value == crate::http::WWW_AUTHENTICATE
            }),
            "{:?}",
            parts.headers
        );
    }
}
