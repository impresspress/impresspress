//! GET/POST /b/auth/api/verify and POST /b/auth/api/resend-verification —
//! relocated from auth/login.rs in Task 5.

use maud::html;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::{
        auth::repo::users, auth_ui::contracts::MessageResponse, crud, rate_limit::UserRateLimiter,
    },
    http::{err_bad_request, ok_json},
    ui,
    ui::{components::auth_panel, icons, templates::auth_split},
    util::sha256_hex,
};

pub async fn handle(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    // Through the async loader, not `ctx.config_get`: that snapshot is frozen
    // at boot, so an admin's saved branding never reached this page without a
    // restart, and on Cloudflare never reached it at all.
    let site = match ui::SiteConfig::load_for_auth(ctx).await {
        Ok(site) => site,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "page: site config read failed")
        }
    };
    let logo_url = site.logo_url.clone();
    let app_name = site.app_name.clone();
    let auth_headline = site.auth_headline.clone();
    let auth_tagline = site.auth_tagline.clone();

    // Token comes from query param or body
    let token = {
        let q = msg.get_meta("req.query.token").to_string();
        if !q.is_empty() {
            q
        } else {
            #[derive(serde::Deserialize)]
            struct Req {
                token: String,
            }
            let raw = match input.collect_to_bytes().await {
                Ok(bytes) => bytes,
                Err(e) => return OutputStream::error(e),
            };
            match serde_json::from_slice::<Req>(&raw) {
                Ok(r) => r.token,
                Err(_) => return err_bad_request("Missing verification token"),
            }
        }
    };

    if token.is_empty() {
        return err_bad_request("Missing verification token");
    }

    // Find user by verification token. The DB column stores
    // `sha256_hex(raw)`; hash the supplied token the same way before
    // comparing.
    let user = match users::find_by_verification_token(ctx, &sha256_hex(token.as_bytes())).await {
        Ok(Some(user)) => user,
        // No row carries this digest: the token was already used, rotated
        // away, or never minted. That is the real invalid-or-expired link.
        Ok(None) => {
            return html_respond(
                "Invalid Link",
                "This verification link is invalid or has expired. Please request a new one.",
                false,
                &logo_url,
                &app_name,
                &auth_headline,
                &auth_tagline,
            )
        }
        // A read that could not run is not a bad link. This endpoint is not
        // an enumeration surface — the caller already holds the token — so
        // there is nothing to protect by lying, and the page's own advice
        // ("request a new one") sends the holder of a good token to
        // `resend-verification`, which reads the same table and replaces the
        // token they were holding.
        Err(e) => return crud::db_error_internal(e, "Could not check the verification token"),
    };

    // `email_is_proven`, not `email_verified`. The flag is policy —
    // `api::signup` writes it `!REQUIRE_VERIFICATION` — so short-circuiting on
    // it would turn away the holder of a real link on every deployment that
    // does not require verification, and there would be no way left to record
    // the proof for an account that needs one to link a provider. The flag
    // being set while nothing proved it is exactly the state this redemption
    // exists to repair.
    if user.email_is_proven() {
        return html_respond(
            "Email Already Verified",
            "Your email has already been verified. You can sign in now.",
            true,
            &logo_url,
            &app_name,
            &auth_headline,
            &auth_tagline,
        );
    }

    // Record the proof + clear the token in one typed write. The holder of
    // this link received it at the address, which is the only evidence of
    // mailbox control this app ever collects itself.
    if let Err(e) = users::record_email_proof(ctx, &user.id, users::proof::EMAIL_TOKEN).await {
        return crud::db_error_internal(e, "Failed to verify email");
    }

    html_respond(
        "Email Verified",
        "Your email has been verified successfully. You can now sign in.",
        true,
        &logo_url,
        &app_name,
        &auth_headline,
        &auth_tagline,
    )
}

pub async fn handle_resend(
    limiter: &std::sync::Arc<UserRateLimiter>,
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        email: String,
    }
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    let email_lower = body.email.trim().to_lowercase();
    // The endpoint is public. Every branch below answers this same body so
    // an anonymous caller cannot tell a registered address from an
    // unregistered one, an already-verified account from an unverified one,
    // or an account inside its cooldown from one outside it.
    let safe_msg = "If that email is registered, a verification link has been sent.";
    let constant = || {
        ok_json(&MessageResponse {
            message: safe_msg.to_string(),
        })
    };

    // DELIBERATE, do not "fix": the `Err` arm is folded into the constant
    // response on purpose. It is the same collapse the T4 sweep removes
    // everywhere else, and here it is the feature — an answer that varied
    // with the submitted address, for any reason, is the enumeration oracle
    // the paragraph above closes. The response stays constant; the failure
    // is logged so an outage on this endpoint is still findable, which is
    // the part that was missing.
    let user = match users::find_by_email(ctx, &email_lower).await {
        Ok(Some(user)) => user,
        Ok(None) => return constant(),
        Err(e) => {
            tracing::error!(code = ?e.code, error = %e, "resend-verification: user lookup failed");
            return constant();
        }
    };

    // Same reading as the redemption above: an account whose address nobody
    // proved may still ask for a link, whatever the policy flag says. On a
    // deployment that does not require verification that is every account,
    // and it is the only route by which one of them can ever become
    // adoptable by an OAuth identity.
    if user.email_is_proven() {
        return constant();
    }

    // Mint, persist and mail a fresh link — the shared path, which owns both
    // the 60-second resend cooldown and the outbound-mail budget. Inside the
    // cooldown nothing is minted and nothing is said.
    //
    // After the response, like forgot-password: every step of it is work an
    // unregistered (or already proven) address never causes, so done inline
    // it made `constant()` arrive later for exactly the addresses it exists
    // to hide. Deferred, every path costs the handler one lookup.
    let mail = super::LaterMail::capture(limiter, ctx, msg);
    crate::deferred::defer(async move {
        match mail.send_verification(&user.id, &email_lower).await {
            Ok(super::VerificationMail::Sent | super::VerificationMail::WithinCooldown) => {}
            Ok(super::VerificationMail::NotSent(failure)) => {
                super::log_email_not_sent("resend-verification", &user.id, &failure);
            }
            // There is no reply left to carry it, and one could not anyway:
            // a mint that fails only for a registered, unproven address
            // would be the oracle `constant()` closes. Logged with its code.
            Err(e) => {
                tracing::error!(
                    code = ?e.code,
                    error = %e,
                    user_id = %user.id,
                    "resend-verification: minting the verification token failed"
                );
            }
        }
    });

    constant()
}

/// Return an HTML page response (for verify endpoints opened in browser).
fn html_respond(
    title: &str,
    message: &str,
    success: bool,
    logo_url: &str,
    app_name: &str,
    auth_headline: &str,
    auth_tagline: &str,
) -> OutputStream {
    // Static modifier rather than an inline `--icon-color`/`--icon-bg` pair:
    // the two states are fixed, so their colours belong in the stylesheet
    // where the contrast guard can see them (see auth-split.css).
    let icon_state = if success {
        "auth-status__icon--success"
    } else {
        "auth-status__icon--failure"
    };
    let config = ui::SiteConfig {
        app_name: app_name.to_string(),
        logo_url: logo_url.to_string(),
        logo_icon_url: String::new(),
        favicon_url: crate::ui::assets::favicon_url(),
        primary_color: String::new(),
        embedded_scripts: Vec::new(),
        auth_headline: auth_headline.to_string(),
        auth_tagline: auth_tagline.to_string(),
    };
    let markup = ui::layout::page(
        title,
        &config,
        auth_split(
            auth_panel(&config, Some("Verify your email.")),
            html! {
                div .login-container {
                    div .auth-status {
                        div class={"auth-status__icon " (icon_state)} aria-hidden="true" {
                            @if success { (icons::check()) } @else { (icons::x()) }
                        }
                        h2 .auth-status__title { (title) }
                        p .auth-status__message { (message) }
                        a .login-button .auth-status__action href="/b/auth/login" {
                            "Go to Sign In"
                        }
                    }
                }
            },
        ),
    );
    ui::html_response(markup)
}

#[cfg(test)]
mod verify_tests {
    use super::*;
    use crate::{
        blocks::auth::repo::users::{self, NewUser},
        test_support::{anon_msg, output_html, output_is_error, TestContext},
    };

    /// A user carrying `token`'s digest in `verification_token`.
    async fn seed_unverified(ctx: &TestContext, token: &str) -> String {
        let user = users::insert(
            ctx,
            NewUser {
                email: "pending@example.com".into(),
                display_name: "Pending".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: Some(sha256_hex(token.as_bytes())),
            },
        )
        .await
        .expect("insert user");
        user.id
    }

    fn verify_msg(token: &str) -> Message {
        let mut msg = anon_msg("retrieve", "/b/auth/api/verify");
        msg.set_meta("req.query.token", token);
        msg
    }

    /// Unlike `resend`, this endpoint is not an enumeration surface: the
    /// caller already holds the token. A failed lookup used to render the
    /// same "This verification link is invalid or has expired" page as a
    /// genuinely bad token — a 200 that tells the holder of a good link to
    /// throw it away, and whose advice sends them to `resend-verification`,
    /// which reads the same table and replaces the token they were holding.
    #[tokio::test]
    async fn an_unreadable_verification_token_is_an_outage_not_a_bad_link() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let token = "raw-verification-token-0123456789";
        let user_id = seed_unverified(&ctx, token).await;

        // The positive control first, on the same fixture: this token really
        // does verify, so the assertion below is about the outage and not
        // about a token the handler would have refused anyway.
        let verified = output_html(
            handle(
                &ctx,
                &verify_msg(token),
                InputStream::from_bytes(Vec::new()),
            )
            .await,
        )
        .await;
        assert!(
            verified.contains("Email Verified"),
            "the seeded token must verify on a healthy database: {verified}"
        );
        assert!(
            users::find_by_id(&ctx, &user_id)
                .await
                .expect("read back")
                .expect("the row is there")
                .email_verified
        );

        // The same fixture again, with a database whose reads all fail. The
        // token lookup is the handler's first read, so it is the one that
        // fails.
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        seed_unverified(&ctx, token).await;
        let failing = ctx.break_reads();

        let out = handle(
            &failing,
            &verify_msg(token),
            InputStream::from_bytes(Vec::new()),
        )
        .await;

        assert!(
            output_is_error(out, "Internal").await,
            "a failed token lookup must not render the invalid-link page"
        );
    }
}

#[cfg(test)]
mod resend_tests {
    use wafer_run::InputStream;

    use super::*;
    use crate::{
        blocks::auth::repo::users::{self, NewUser},
        test_support::{output_json, TestContext},
    };

    fn body(email: &str) -> InputStream {
        InputStream::from_bytes(
            serde_json::to_vec(&serde_json::json!({ "email": email })).expect("serialize body"),
        )
    }

    /// Register an account through the real signup handler, so the row
    /// carries what that handler writes for this deployment's configuration
    /// rather than what a fixture would like it to carry.
    async fn signup(ctx: &TestContext, email: &str) {
        let payload =
            serde_json::json!({ "email": email, "password": "correct-horse-battery" }).to_string();
        let (signup_limiter, signup_msg) = crate::blocks::auth_ui::api::test_mail_request();
        let out = crate::blocks::auth_ui::api::signup::handle(
            &signup_limiter,
            ctx,
            &signup_msg,
            InputStream::from_bytes(payload.into_bytes()),
        )
        .await;
        assert_eq!(
            crate::test_support::output_status(out).await,
            201,
            "the signup fixture must succeed"
        );
    }

    async fn seed(ctx: &TestContext, email: &str, verified: bool) -> String {
        let user = users::insert(
            ctx,
            NewUser {
                email: email.into(),
                display_name: "U".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .expect("insert user");
        users::set_email_verified(ctx, &user.id, verified)
            .await
            .expect("set email_verified");
        user.id
    }

    /// The endpoint is public. An anonymous caller must not be able to tell
    /// a registered address from an unregistered one by the response, so
    /// every branch answers the same constant body: no "already verified",
    /// no "please wait", no `retry_after`.
    #[tokio::test]
    async fn resend_answers_the_same_body_whatever_the_account_state() {
        let ctx = TestContext::with_auth_and_crypto().await;
        seed(&ctx, "verified@example.com", true).await;
        let cooling = seed(&ctx, "cooling@example.com", false).await;
        users::set_verification_token(&ctx, &cooling, "hash", &crate::util::now_rfc3339())
            .await
            .expect("set token");

        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        let unregistered =
            output_json(handle_resend(&limiter, &ctx, &msg, body("nobody@example.com")).await)
                .await;
        let already =
            output_json(handle_resend(&limiter, &ctx, &msg, body("verified@example.com")).await)
                .await;
        let cooldown =
            output_json(handle_resend(&limiter, &ctx, &msg, body("cooling@example.com")).await)
                .await;

        assert_eq!(
            already, unregistered,
            "a verified account must not be distinguishable from an unregistered one"
        );
        assert_eq!(
            cooldown, unregistered,
            "an account inside its cooldown must not be distinguishable from an unregistered one"
        );
        assert!(unregistered.get("retry_after").is_none());
    }

    /// Constant responses do not relax the cooldown: a request inside the
    /// window neither mints a new token nor moves the cooldown clock.
    #[tokio::test]
    async fn resend_inside_the_cooldown_does_not_rotate_the_token() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let id = seed(&ctx, "cooling@example.com", false).await;
        let sent_at = crate::util::now_rfc3339();
        users::set_verification_token(&ctx, &id, "hash-before", &sent_at)
            .await
            .expect("set token");

        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        crate::deferred::queue_for_test();
        let _ = handle_resend(&limiter, &ctx, &msg, body("cooling@example.com"))
            .await
            .collect_buffered()
            .await;
        // The cooldown is judged by the deferred send; run it.
        assert_eq!(crate::blocks::auth_ui::api::run_deferred().await, 1);

        assert_eq!(
            users::last_verification_sent(&ctx, &id)
                .await
                .expect("read cooldown"),
            sent_at
        );
    }

    /// The one place in this sweep where a failed read must NOT be
    /// distinguishable from a negative answer. An error here would vary the
    /// response by the submitted address, which is the enumeration oracle
    /// the constant body exists to close. Pinned so a later sweep cannot
    /// "fix" it back into an oracle.
    #[tokio::test]
    async fn resend_answers_the_constant_body_even_when_the_lookup_fails() {
        let ctx = TestContext::with_auth_and_crypto().await;
        seed(&ctx, "known@example.com", false).await;
        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        let expected =
            output_json(handle_resend(&limiter, &ctx, &msg, body("known@example.com")).await).await;
        let failing = ctx.break_reads();

        let outage =
            output_json(handle_resend(&limiter, &failing, &msg, body("known@example.com")).await)
                .await;

        assert_eq!(
            outage, expected,
            "a failed lookup must answer the same constant body as any other state"
        );
    }

    /// The recovery path the OAuth adoption rule depends on.
    ///
    /// On a deployment that does not require verification, `api::signup`
    /// writes `email_verified = true` and mails nothing, so every password
    /// account is flag-verified and nobody proved its address. Gating this
    /// endpoint on the flag made that permanent: the account could never be
    /// offered a link, so it could never record a proof, so an OAuth identity
    /// could never join it and only a manual database write would fix it.
    #[tokio::test]
    async fn a_flag_verified_account_that_nobody_proved_is_still_offered_a_link() {
        let ctx = TestContext::with_auth_and_crypto().await;
        signup(&ctx, "flagged@example.com").await;

        let user = users::find_by_email(&ctx, "flagged@example.com")
            .await
            .expect("user lookup ok")
            .expect("signup created the row");
        assert!(
            user.email_verified,
            "precondition: with verification off, signup marks the row verified"
        );
        assert!(
            !user.email_is_proven(),
            "precondition: no mail was sent, so nothing proved the address"
        );

        let (limiter, request) = crate::blocks::auth_ui::api::test_mail_request();
        crate::deferred::queue_for_test();
        let _ = handle_resend(&limiter, &ctx, &request, body("flagged@example.com"))
            .await
            .collect_buffered()
            .await;
        assert_eq!(
            crate::blocks::auth_ui::api::run_deferred().await,
            1,
            "the link is minted after the response"
        );

        assert!(
            !users::last_verification_sent(&ctx, &user.id)
                .await
                .expect("read cooldown")
                .is_empty(),
            "an unproven account must be able to ask for a link, whatever the flag says"
        );
    }

    /// And redeeming that link records the proof, rather than stopping at
    /// "Email Already Verified" — which reads the flag, and so would turn
    /// away the one caller who can actually prove the address.
    #[tokio::test]
    async fn redeeming_a_link_on_a_flag_verified_account_records_the_proof() {
        let ctx = TestContext::with_auth_and_crypto().await;
        signup(&ctx, "flagged2@example.com").await;
        let user = users::find_by_email(&ctx, "flagged2@example.com")
            .await
            .expect("user lookup ok")
            .expect("signup created the row");
        assert!(user.email_verified && !user.email_is_proven());

        let raw = "raw-verification-token-0123456789";
        users::set_verification_token(
            &ctx,
            &user.id,
            &crate::util::sha256_hex(raw.as_bytes()),
            &crate::util::now_rfc3339(),
        )
        .await
        .expect("set token");

        let mut msg = Message::new("auth.verify");
        msg.set_meta("req.query.token", raw);
        let page =
            crate::test_support::output_html(handle(&ctx, &msg, InputStream::empty()).await).await;
        assert!(
            page.contains("Email Verified"),
            "the link must be redeemable, not answered as already verified: {page}"
        );

        let proven = users::find_by_email(&ctx, "flagged2@example.com")
            .await
            .expect("user lookup ok")
            .expect("row present");
        assert_eq!(
            proven.email_verified_by.as_deref(),
            Some(users::proof::EMAIL_TOKEN),
            "redeeming the link is what records the proof"
        );
    }

    /// Everything the HTTP boundary sends for one resend request.
    async fn resend_on_the_wire(
        ctx: &dyn Context,
        email: &str,
    ) -> (u16, Vec<(String, String)>, String) {
        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        let parts = wafer_block::http_codec::collect_http_response(
            handle_resend(&limiter, ctx, &msg, body(email)).await,
        )
        .await;
        (
            parts.status,
            parts.headers,
            String::from_utf8_lossy(&parts.body).into_owned(),
        )
    }

    /// The token draw needs the crypto block; a deployment without it must
    /// still answer a registered, unproven address like any other.
    #[tokio::test]
    async fn a_failed_verification_token_draw_answers_what_an_unregistered_address_does() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        seed(&ctx, "unproven@example.com", false).await;

        let unregistered = resend_on_the_wire(&ctx, "nobody@example.com").await;
        let registered = resend_on_the_wire(&ctx, "unproven@example.com").await;

        assert_eq!(
            unregistered.0, 200,
            "the unregistered answer is the constant 200"
        );
        assert_eq!(
            registered, unregistered,
            "a failed token draw must not be visible to the caller"
        );
    }

    /// Every account state answers the same body (pinned above); this pins
    /// that every state also costs the handler the same work before
    /// answering — one lookup — so the body does not arrive later for an
    /// unproven account either. Minting, storing and mailing its link run
    /// after the response.
    #[tokio::test]
    async fn an_unproven_account_costs_the_handler_the_same_work_as_an_unknown_address() {
        use super::super::{run_deferred, CallLog};

        let ctx = TestContext::with_auth_and_crypto().await;
        let id = seed(&ctx, "unproven@example.com", false).await;
        let ctx = CallLog::new(ctx);
        crate::deferred::queue_for_test();
        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();

        let unknown =
            output_json(handle_resend(&limiter, &ctx, &msg, body("nobody@example.com")).await)
                .await;
        let unknown_calls = ctx.take();
        assert_eq!(run_deferred().await, 0);

        let unproven =
            output_json(handle_resend(&limiter, &ctx, &msg, body("unproven@example.com")).await)
                .await;
        let unproven_calls = ctx.take();

        assert_eq!(unproven, unknown);
        assert_eq!(
            unproven_calls, unknown_calls,
            "an unproven account must perform the same operations, in the same order, as an unknown address"
        );

        assert_eq!(
            run_deferred().await,
            1,
            "the link is minted after the response"
        );
        assert!(
            !users::last_verification_sent(&ctx, &id)
                .await
                .expect("read cooldown")
                .is_empty(),
            "the deferred task minted and stored the link"
        );
    }
}
