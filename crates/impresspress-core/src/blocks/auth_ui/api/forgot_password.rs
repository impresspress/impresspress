//! POST /b/auth/api/forgot-password — relocated from auth/login.rs in Task 5.

use std::sync::Arc;

use wafer_core::clients::crypto;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::{auth::repo::users, auth_ui::contracts::MessageResponse, rate_limit::UserRateLimiter},
    http::{err_bad_request, ok_json},
    util::{hex_encode, sha256_hex},
};

pub async fn handle(
    limiter: &Arc<UserRateLimiter>,
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
    let safe_msg = "If that email is registered, a password reset link has been sent.";
    let constant = || {
        ok_json(&MessageResponse {
            message: safe_msg.to_string(),
        })
    };

    // DELIBERATE, do not "fix": like `verify::handle_resend`, this endpoint
    // is public and answers one constant body for every account state, so a
    // failed lookup must answer it too. Separating "no such account" from
    // "the lookup failed" would hand an anonymous caller a signal that
    // varies with the address they submitted — the account-enumeration
    // oracle `safe_msg` exists to close. The response stays constant; the
    // failure is logged so an outage on this endpoint is still findable.
    let user = match users::find_by_email(ctx, &email_lower).await {
        Ok(Some(user)) => user,
        Ok(None) => return constant(),
        Err(e) => {
            tracing::error!(code = ?e.code, error = %e, "forgot-password: user lookup failed");
            return constant();
        }
    };

    // Everything past the lookup runs after the response. Only a registered
    // address has a token to draw, a row to write and a mail to send; done
    // before answering, those steps made `constant()` arrive measurably later
    // for a registered address than for an unknown one — the same oracle the
    // constant body closes, read off a clock instead. Deferred, both paths
    // cost the handler one lookup.
    let mail = super::LaterMail::capture(limiter, ctx, msg);
    crate::deferred::defer(async move {
        send_reset_link(&mail, &user.id, &email_lower).await;
    });

    constant()
}

/// Mint a reset token for `user_id`, persist its digest and mail the raw
/// token to `email`. Runs after the response (see `handle`), so every
/// failure is logged: there is no reply left for it to change, and a reset
/// mail that never left is a user locked out.
async fn send_reset_link(mail: &super::LaterMail, user_id: &str, email: &str) {
    let ctx = mail.ctx();
    // The raw token goes in the email link; only its SHA-256 hex digest is
    // persisted, so a leak of the row (admin SQL explorer, backup, log dump,
    // any block with read grant on the users table) does not become a
    // password-reset oracle. It expires in an hour.
    let reset_token = match crypto::random_bytes(ctx, 32).await {
        Ok(bytes) => hex_encode(&bytes),
        Err(e) => {
            tracing::error!(
                code = ?e.code,
                error = %e,
                user_id = %user_id,
                "forgot-password: drawing the reset token failed"
            );
            return;
        }
    };
    let reset_token_hash = sha256_hex(reset_token.as_bytes());

    let expires = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    if let Err(e) = users::set_reset_token(ctx, user_id, &reset_token_hash, &expires).await {
        tracing::error!(
            code = ?e.code,
            error = %e,
            user_id = %user_id,
            "forgot-password: storing the reset token failed"
        );
        return;
    }

    // The email block's own log says which limit or provider refused it.
    if let Err(failure) = mail
        .send_template("password_reset", email, &reset_token)
        .await
    {
        super::log_email_not_sent("forgot-password", user_id, &failure);
    }
}

#[cfg(test)]
mod tests {
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

    /// The deliberate counterpart to the rest of this sweep: a failed lookup
    /// must NOT be distinguishable from "no such account". Anything that
    /// varied with the submitted address would be the account-enumeration
    /// oracle the constant body exists to close. Pinned so a later sweep
    /// cannot "fix" it back into one.
    #[tokio::test]
    async fn the_constant_body_survives_a_failed_lookup() {
        let ctx = TestContext::with_auth_and_crypto().await;
        users::insert(
            &ctx,
            NewUser {
                email: "known@example.com".into(),
                display_name: "Known".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: true,
                verification_token_hash: None,
            },
        )
        .await
        .expect("insert user");

        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        let unregistered =
            output_json(handle(&limiter, &ctx, &msg, body("nobody@example.com")).await).await;
        let failing = ctx.break_reads();
        let outage =
            output_json(handle(&limiter, &failing, &msg, body("known@example.com")).await).await;

        assert_eq!(
            outage, unregistered,
            "a failed lookup must answer the same constant body as an unregistered address"
        );
    }

    /// Everything the HTTP boundary sends for one forgot-password request.
    async fn on_the_wire(ctx: &dyn Context, email: &str) -> (u16, Vec<(String, String)>, String) {
        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        let parts = wafer_block::http_codec::collect_http_response(
            handle(&limiter, ctx, &msg, body(email)).await,
        )
        .await;
        (
            parts.status,
            parts.headers,
            String::from_utf8_lossy(&parts.body).into_owned(),
        )
    }

    async fn with_known_user(ctx: &TestContext) {
        users::insert(
            ctx,
            NewUser {
                email: "known@example.com".into(),
                display_name: "Known".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: true,
                verification_token_hash: None,
            },
        )
        .await
        .expect("insert user");
    }

    /// Same shape one step earlier: drawing the token needs the crypto
    /// block, and only a registered address asks it for anything. A
    /// deployment without it must still answer every address alike.
    #[tokio::test]
    async fn a_failed_reset_token_draw_answers_what_an_unregistered_address_does() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        with_known_user(&ctx).await;

        let unregistered = on_the_wire(&ctx, "nobody@example.com").await;
        let registered = on_the_wire(&ctx, "known@example.com").await;

        assert_eq!(
            unregistered.0, 200,
            "the unregistered answer is the constant 200"
        );
        assert_eq!(
            registered, unregistered,
            "a failed token draw must not be visible to the caller"
        );
    }

    /// Stands in for `impresspress/email`: answers `{"sent": true}` and
    /// keeps the token of every mail, so a test can redeem the link the way
    /// its recipient would.
    struct TokenInbox(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    #[wafer_block::wafer_async_trait]
    impl wafer_run::Block for TokenInbox {
        fn info(&self) -> wafer_run::BlockInfo {
            wafer_run::BlockInfo::new("impresspress/email", "0.0.1", "service@v1", "inbox stub")
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            input: InputStream,
        ) -> OutputStream {
            let body: serde_json::Value =
                serde_json::from_slice(&input.collect_to_bytes().await.expect("mail body read"))
                    .expect("mail body");
            self.0.lock().expect("inbox").push(
                body["token"]
                    .as_str()
                    .expect("the mail carries a token")
                    .to_string(),
            );
            ok_json(&serde_json::json!({ "sent": true }))
        }
    }

    /// The reply for a registered address was already the unregistered
    /// reply byte for byte; this pins that it also costs the handler the same
    /// work — one lookup, nothing else — so it does not arrive later either.
    /// The token draw, its write and the mail all run after the response.
    #[tokio::test]
    async fn a_registered_address_costs_the_handler_the_same_work_as_an_unknown_one() {
        use super::super::{run_deferred, CallLog};

        let mut ctx = TestContext::with_auth_and_crypto().await;
        let inbox = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        ctx.register_block(
            "impresspress/email",
            std::sync::Arc::new(TokenInbox(std::sync::Arc::clone(&inbox))),
        );
        with_known_user(&ctx).await;
        let ctx = CallLog::new(ctx);
        crate::deferred::queue_for_test();

        let unregistered = on_the_wire(&ctx, "nobody@example.com").await;
        let unregistered_calls = ctx.take();
        assert_eq!(run_deferred().await, 0, "an unknown address defers nothing");

        let registered = on_the_wire(&ctx, "known@example.com").await;
        let registered_calls = ctx.take();

        assert_eq!(registered, unregistered, "the reply is the same");
        assert_eq!(
            registered_calls, unregistered_calls,
            "and so are the operations performed before it"
        );
        assert!(
            inbox.lock().expect("inbox").is_empty(),
            "nothing is mailed before the response"
        );

        assert_eq!(run_deferred().await, 1);
        let deferred = ctx.take();
        for call in [
            "wafer-run/crypto crypto.random_bytes",
            "impresspress/email email.send_template",
        ] {
            assert!(
                deferred.iter().any(|c| c == call),
                "{call} runs after the response: {deferred:?}"
            );
        }
        assert_eq!(inbox.lock().expect("inbox").len(), 1, "the link went out");
    }

    /// An account left with no password — by the two-write signup that is
    /// now one write, or any other path — is recovered through the reset
    /// link: it proves control of the address, and redeeming it creates the
    /// missing credentials row. Signup cannot be the repair: it would hand
    /// the account to whoever typed the address, and the account may be a
    /// real one whose owner signs in some other way.
    #[tokio::test]
    async fn an_account_without_a_password_recovers_through_the_reset_link() {
        use crate::blocks::auth::repo::local_credentials;

        let mut ctx = TestContext::with_auth_and_crypto().await;
        let inbox = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        ctx.register_block(
            "impresspress/email",
            std::sync::Arc::new(TokenInbox(std::sync::Arc::clone(&inbox))),
        );
        with_known_user(&ctx).await;
        let user = users::find_by_email(&ctx, "known@example.com")
            .await
            .expect("lookup")
            .expect("seeded");
        assert!(
            local_credentials::find_by_user_id(&ctx, &user.id)
                .await
                .expect("credentials lookup")
                .is_none(),
            "precondition: the account has no password"
        );

        crate::deferred::queue_for_test();
        on_the_wire(&ctx, "known@example.com").await;
        super::super::run_deferred().await;
        let token = inbox
            .lock()
            .expect("inbox")
            .pop()
            .expect("the reset link was mailed");

        let reset = serde_json::json!({"token": token, "new_password": "a-new-horse-battery"});
        let status = crate::test_support::output_status(
            super::super::reset_password::handle(
                &ctx,
                InputStream::from_bytes(serde_json::to_vec(&reset).expect("body")),
            )
            .await,
        )
        .await;
        assert_eq!(status, 200, "the reset link is redeemed");

        let login =
            serde_json::json!({"email": "known@example.com", "password": "a-new-horse-battery"});
        let signed_in = output_json(
            super::super::login::handle(
                &ctx,
                InputStream::from_bytes(serde_json::to_vec(&login).expect("body")),
            )
            .await,
        )
        .await;
        assert!(
            signed_in["access_token"].is_string(),
            "the recovered account signs in with the password it chose: {signed_in}"
        );
    }
}
