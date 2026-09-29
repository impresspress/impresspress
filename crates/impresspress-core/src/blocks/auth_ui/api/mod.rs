//! JSON API handlers for the auth-ui block. One handler per leaf module;
//! routed from `auth_ui::AuthUiBlock::handle`.

use wafer_run::{context::Context, InputStream, Message};

use crate::blocks::rate_limit::{
    check_rate_limit, ip_identity, RateLimit, RateLimitOutcome, UserRateLimiter, UNKNOWN_IP,
};

pub mod api_keys;
pub mod bootstrap;
pub mod change_password;
pub mod forgot_password;
pub mod login;
pub mod logout;
pub mod me;
mod password_policy;
pub mod refresh;
pub mod reset_password;
pub mod signup;
pub mod verify;

/// Why a transactional email did not go out.
///
/// The email block answers a rate-limit refusal, an allow-list rejection, a
/// malformed request and a WRAP denial all as error terminals, and a failed
/// Mailgun call as a `200 {"sent": false}` body. Every one of them means "no
/// mail was sent", and none of them used to be distinguishable here from a
/// delivery: the old helper checked the stream for an error, logged a single
/// line, and never looked at `sent` at all, so a provider outage read as
/// success.
///
/// The variants are split by who has to act, not by where the failure
/// happened, because that is the only question the log level answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EmailNotSent {
    /// This requester has spent their [`RateLimit::AUTH_EMAIL`] budget for
    /// the window. Nothing left this block; the email block was never
    /// called, so no shared quota was touched.
    RequesterLimited,
    /// The email block refused the message under one of ITS rate limits —
    /// the per-recipient bucket or the deployment-wide per-caller ceiling.
    /// Identified by the `rate_limit_exceeded` detail code its 429 carries
    /// (`rate_limit::rate_limited_response`), never by the message text.
    RateLimited(String),
    /// The call to the email block failed for any other reason: the block is
    /// absent or disabled, a WRAP grant denies the call, the request is
    /// malformed, or `IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS` does
    /// not match the recipient. Every one of these is a deployment fault
    /// that silently drops auth mail, and none of them resolves itself.
    Undeliverable(String),
    /// The email block accepted the message and the provider call failed.
    ProviderFailed,
}

impl EmailNotSent {
    /// Whether this outcome needs an operator.
    ///
    /// A rate-limit decision does not: it is abuse handling working as
    /// designed, it is already reported by the limiter that made it (and
    /// `email.rs` logs the deployment ceiling at `error` precisely because
    /// that one does affect everybody), and it clears itself when the window
    /// rolls over.
    ///
    /// Everything else does. A provider failure means the deployment
    /// believes it can send mail and cannot; an [`Self::Undeliverable`] means
    /// the mail never reached the provider at all — a missing block, a
    /// missing grant, an allow-list that drops every address — and it will
    /// keep happening, unnoticed, until somebody changes the deployment.
    /// Filing those under "rate limiting, nobody needs to look" is exactly
    /// the silent-drop case this whole change exists to end.
    fn needs_an_operator(&self) -> bool {
        match self {
            Self::RequesterLimited | Self::RateLimited(_) => false,
            Self::Undeliverable(_) | Self::ProviderFailed => true,
        }
    }
}

impl std::fmt::Display for EmailNotSent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequesterLimited => {
                f.write_str("this requester has sent their limit of transactional mail")
            }
            Self::RateLimited(reason) => write!(f, "rate-limited by the email block: {reason}"),
            Self::Undeliverable(reason) => {
                write!(f, "the email block could not be asked to send: {reason}")
            }
            Self::ProviderFailed => f.write_str("accepted but the provider send failed"),
        }
    }
}

/// Classify an error terminal from the `impresspress/email` call.
///
/// Reads the `WaferError` rather than its rendered text: a rate-limit
/// refusal carries the `rate_limit_exceeded` detail code, and
/// `ResourceExhausted` is the wafer code that detail maps to, so either
/// spelling is recognised while nothing depends on wording. Anything else —
/// `NotFound` for an unregistered block, `PermissionDenied` for a WRAP
/// denial, `InvalidArgument` for an allow-list rejection — is a deployment
/// fault and is classified as such.
fn classify_email_error(terminal: &wafer_run::TerminalNotResponse) -> EmailNotSent {
    let wafer_run::TerminalNotResponse::Error(error) = terminal else {
        return EmailNotSent::Undeliverable(format!("{terminal:?}"));
    };
    let rate_limited = error.detail_code()
        == Some(crate::blocks::errors::ErrorCode::RateLimitExceeded.as_str())
        || error.code == wafer_run::ErrorCode::ResourceExhausted;
    let reason = format!("{:?}: {}", error.code, error.message);
    if rate_limited {
        EmailNotSent::RateLimited(reason)
    } else {
        EmailNotSent::Undeliverable(reason)
    }
}

/// A limiter and a request `Message` for a test that drives one of the
/// mail-sending handlers (`signup::handle`, `verify::handle_resend`,
/// `forgot_password::handle`) directly rather than through the block.
///
/// The limiter is fresh on every call, so its buckets start empty. That is
/// deliberate: most callers of these handlers in tests are only using them to
/// create a user, and one shared limiter would make the eleventh signup in a
/// file silently stop sending mail. A test that IS about the outbound budget
/// builds its own limiter and holds on to it across calls.
#[cfg(test)]
pub(crate) fn test_mail_request() -> (std::sync::Arc<UserRateLimiter>, Message) {
    let mut msg = Message::new("http.request");
    msg.set_meta(wafer_block::meta::META_REQ_CLIENT_IP, "203.0.113.7");
    (std::sync::Arc::new(UserRateLimiter::new()), msg)
}

/// A context that records every block call made through it — `"{block}
/// {message kind}"`, in order — and forwards it unchanged. Its `clone_arc`
/// shares the record, so calls a deferred task makes land in it too.
///
/// It is how a test states that two paths through a handler do the same
/// work: the same calls, in the same order, before the response.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct CallLog {
    inner: crate::test_support::TestContext,
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[cfg(test)]
impl CallLog {
    pub(crate) fn new(inner: crate::test_support::TestContext) -> Self {
        Self {
            inner,
            calls: std::sync::Arc::default(),
        }
    }

    /// The calls recorded since the last `take`, clearing the record.
    pub(crate) fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().expect("call log"))
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl Context for CallLog {
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), wafer_run::WaferError> {
        self.inner
            .check_resource_access(resource, resource_type, access)
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.inner
            .resource_access_admitted(resource, resource_type, access)
    }

    async fn call_block(
        &self,
        name: &str,
        msg: Message,
        input: InputStream,
    ) -> wafer_run::OutputStream {
        self.calls
            .lock()
            .expect("call log")
            .push(format!("{name} {}", msg.kind));
        self.inner.call_block(name, msg, input).await
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
        self.inner.registered_blocks()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        std::sync::Arc::new(self.clone())
    }
}

/// Run the work the handler under test deferred ([`crate::deferred`]), as
/// the platform would after the response. The test must have called
/// `crate::deferred::queue_for_test` before calling the handler; answers how
/// many tasks ran.
#[cfg(test)]
pub(crate) async fn run_deferred() -> usize {
    let tasks = crate::deferred::take_for_test();
    let n = tasks.len();
    for task in tasks {
        task.await;
    }
    n
}

/// Record an outcome from [`send_template_email`] against the flow that
/// produced it, at the level that outcome deserves.
///
/// Every caller answers a body that cannot vary with this (see each call
/// site), so this log line is where the failure lives.
pub(crate) fn log_email_not_sent(flow: &str, user_id: &str, failure: &EmailNotSent) {
    if failure.needs_an_operator() {
        tracing::error!(flow, user_id, %failure, "transactional email was not sent");
    } else {
        tracing::warn!(flow, user_id, %failure, "transactional email was not sent");
    }
}

/// Mail sent after the response, off the request path ([`crate::deferred`]).
///
/// The public auth endpoints answer a registered address and an unknown one
/// with the same body; if only the registered path also drew a token, wrote
/// it and called the email block before answering, the same body would come
/// back measurably later for it. Those steps run here instead, once the
/// handler has answered, so both paths perform the same operations before it answers.
///
/// Owns what the send needs from the request — the block's limiter, the
/// context and the request message (for the client address the outbound
/// budget is keyed on) — because the handler's borrows end with the
/// response. A failure here has no response left to change, so every task
/// records its own with [`log_email_not_sent`] or a log line of its own.
pub(crate) struct LaterMail {
    limiter: std::sync::Arc<UserRateLimiter>,
    ctx: std::sync::Arc<dyn Context>,
    msg: Message,
}

impl LaterMail {
    pub(crate) fn capture(
        limiter: &std::sync::Arc<UserRateLimiter>,
        ctx: &dyn Context,
        msg: &Message,
    ) -> Self {
        Self {
            limiter: std::sync::Arc::clone(limiter),
            ctx: ctx.clone_arc(),
            msg: msg.clone(),
        }
    }

    pub(crate) fn ctx(&self) -> &dyn Context {
        &*self.ctx
    }

    /// [`send_template_email`] with the captured request.
    pub(crate) async fn send_template(
        &self,
        template: &str,
        to: &str,
        token: &str,
    ) -> Result<(), EmailNotSent> {
        send_template_email(&self.limiter, &*self.ctx, &self.msg, template, to, token).await
    }

    /// [`send_verification_email`] with the captured request.
    pub(crate) async fn send_verification(
        &self,
        user_id: &str,
        email: &str,
    ) -> Result<VerificationMail, wafer_run::WaferError> {
        send_verification_email(&self.limiter, &*self.ctx, &self.msg, user_id, email).await
    }
}

/// Send a transactional email through the `impresspress/email` block.
///
/// Shared by the signup, email-verify and forgot-password handlers — every
/// caller builds the same `email.send_template` envelope `{template, to,
/// Seconds an account must wait between verification mails. One caller alone
/// can be driven by whoever holds the address (the resend endpoint) or by
/// whoever holds a provider account (an OAuth sign-in refused for want of a
/// verified address), so the cooldown belongs beside the send, not in either
/// caller.
const VERIFICATION_RESEND_COOLDOWN_SECS: i64 = 60;

/// What [`send_verification_email`] did, which is three outcomes and not two:
/// a link that went out, a link nothing was minted for because the account is
/// inside its cooldown, and a link that was minted and then not delivered.
/// Only the third needs recording, and the caller records it under its own
/// flow name.
#[derive(Debug)]
pub(crate) enum VerificationMail {
    /// A token was minted, persisted, and handed to the email block, which
    /// accepted it.
    Sent,
    /// The account is inside its resend cooldown: nothing minted, nothing
    /// sent, and deliberately nothing said about it either.
    WithinCooldown,
    /// A token was minted and persisted — the link in the user's hands, if
    /// they ever receive it, is valid — and the send did not happen.
    NotSent(EmailNotSent),
}

/// Mint a fresh email-verification token for `user_id`, persist its digest,
/// and mail the link — unless the account is still inside its resend
/// cooldown, in which case nothing is minted and nothing is sent.
///
/// Shared by `api::verify`'s resend endpoint and `oauth::callback`, which
/// needs it for the account a provider created without asserting anything
/// about the address: on a deployment that requires verification, that
/// account has no other way to ever become verified, and refusing it without
/// offering the link would lock it out permanently.
///
/// Both callers reach the same outbound-mail budget, because the send goes
/// through [`send_template_email`] like every other transactional mail: a
/// link mailed from an OAuth callback is as spendable a resource as one
/// mailed from the resend endpoint, and an OAuth flow that bypassed the
/// limiter would be the cheapest way to spend the deployment's mail.
///
/// `Err` is the mint or its persistence failing — a backend fault the caller
/// may surface. A mail that did not go out is not an error here: it comes
/// back as [`VerificationMail::NotSent`] for the caller to hand to
/// [`log_email_not_sent`], because none of these flows may vary their
/// response with it.
///
/// Only the SHA-256 digest is persisted; the raw token exists only in the
/// mail, so a row-read leak does not grant verification.
pub(crate) async fn send_verification_email(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
    msg: &Message,
    user_id: &str,
    email: &str,
) -> Result<VerificationMail, wafer_run::WaferError> {
    use crate::{
        blocks::auth::repo::users,
        util::{hex_encode, sha256_hex},
    };

    let last_sent = users::last_verification_sent(ctx, user_id).await?;
    if let Ok(last) = chrono::DateTime::parse_from_rfc3339(&last_sent) {
        let elapsed = chrono::Utc::now() - last.with_timezone(&chrono::Utc);
        if elapsed.num_seconds() < VERIFICATION_RESEND_COOLDOWN_SECS {
            return Ok(VerificationMail::WithinCooldown);
        }
    }

    let token = hex_encode(&wafer_core::clients::crypto::random_bytes(ctx, 32).await?);
    let now = crate::util::now_rfc3339();
    users::set_verification_token(ctx, user_id, &sha256_hex(token.as_bytes()), &now).await?;
    Ok(
        match send_template_email(limiter, ctx, msg, "verification", email, &token).await {
            Ok(()) => VerificationMail::Sent,
            Err(failure) => VerificationMail::NotSent(failure),
        },
    )
}

/// token}` and only the template name differs.
///
/// Spends this requester's [`RateLimit::AUTH_EMAIL`] budget first, keyed by
/// client IP, and gives up without calling the email block when it is empty.
/// This is the block that knows who asked: the email block sees only its
/// caller (always `impresspress/auth-ui`) and the recipient, so its two
/// buckets cap one address's share of the deployment ceiling but not one
/// requester's — a caller naming a fresh address every time spends the whole
/// ceiling under those two alone. Charged here, ten minutes of signups from
/// one IP costs ten messages instead of a hundred.
///
/// The category resolves `WAFER_RUN_SHARED__RATE_LIMIT_AUTH_EMAIL` (format
/// `requests/seconds`, `0` disables) like every other bucket, which is how a
/// deployment behind a shared egress IP raises or disables it. Like every
/// other `RATE_LIMIT_*` category it is set by key — process environment or
/// the `variables` table — and is declared by no `ConfigVar`, so it does not
/// appear as a field in the admin settings UI.
///
/// Returns why the mail did not go out, so a caller can say so in its own
/// terms instead of assuming delivery. Delivery stays best-effort — a
/// failure here must not turn a successful signup or reset request into an
/// error response — but "best-effort" is not "unrecorded": every caller hands
/// the [`EmailNotSent`] to [`log_email_not_sent`] with the flow that produced
/// it.
pub(crate) async fn send_template_email(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
    msg: &Message,
    template: &str,
    to: &str,
    token: &str,
) -> Result<(), EmailNotSent> {
    let requester = ip_identity(msg);
    if let RateLimitOutcome::Limited(_) = check_rate_limit(
        limiter,
        ctx,
        &requester,
        "auth_email",
        RateLimit::AUTH_EMAIL,
    )
    .await
    {
        // A refusal charged against `UNKNOWN_IP` is not one requester being
        // told to slow down: it is every request that arrived without a
        // client IP sharing one bucket, so the deployment as a whole just
        // stopped sending auth mail. That is an outage with a cause an
        // operator can fix (the platform is not populating `remote_addr`),
        // and it must not read as routine abuse handling.
        if requester == UNKNOWN_IP {
            tracing::error!(
                "outbound mail refused on the no-client-IP bucket: this deployment is not \
                 populating a client IP, so every requester shares one budget and auth mail \
                 is now failing for everyone"
            );
        }
        // The 429 `check_rate_limit` built is dropped on purpose: no caller
        // of this helper may answer one. All three flows answer a body that
        // is constant for every account state, and a 429 that appeared only
        // for addresses which reached the send step would be the
        // account-enumeration oracle those bodies exist to close.
        return Err(EmailNotSent::RequesterLimited);
    }

    let req = serde_json::json!({
        "template": template,
        "to": to,
        "token": token,
    });
    let email_msg = Message {
        kind: "email.send_template".to_string(),
        meta: Vec::new(),
    };
    let body_bytes = serde_json::to_vec(&req).unwrap_or_default();
    let out = ctx
        .call_block(
            "impresspress/email",
            email_msg,
            InputStream::from_bytes(body_bytes),
        )
        .await;
    let buffered = match out.collect_buffered().await {
        Ok(buffered) => buffered,
        Err(terminal) => return Err(classify_email_error(&terminal)),
    };
    // `{"sent": false}` is the email block's own report that the Mailgun
    // call failed; it rides a 200, so only the body tells the two apart. An
    // unparseable body is not evidence of delivery either.
    let sent = serde_json::from_slice::<serde_json::Value>(&buffered.body)
        .ok()
        .and_then(|body| body.get("sent").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    if sent {
        Ok(())
    } else {
        Err(EmailNotSent::ProviderFailed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        blocks::email::{RATE_LIMIT_PER_RECIPIENT_MAX, RATE_LIMIT_WINDOW_SECS},
        test_support::TestContext,
    };

    /// A context with the REAL email block registered, configured to admit
    /// `per_recipient` messages to any one address per window.
    async fn ctx_with_email(per_recipient: &str) -> TestContext {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.set_config(RATE_LIMIT_PER_RECIPIENT_MAX, per_recipient);
        ctx.set_config(RATE_LIMIT_WINDOW_SECS, "60");
        ctx.register_block(
            "impresspress/email",
            Arc::new(crate::blocks::email::EmailBlock::new()),
        );
        ctx
    }

    /// The email block answers a rate-limit refusal as an error stream. That
    /// refusal reaches the caller as [`EmailNotSent::RateLimited`], instead
    /// of being swallowed into a log line the flow cannot see.
    #[tokio::test]
    async fn a_rate_limit_refusal_reaches_the_caller() {
        let ctx = ctx_with_email("1").await;
        let (limiter, msg) = test_mail_request();

        // The first send is admitted by both limiters and then fails at the
        // provider (no `wafer-run/network` here) — already a distinguishable
        // outcome, and NOT a claim of delivery.
        assert_eq!(
            send_template_email(
                &limiter,
                &ctx,
                &msg,
                "verification",
                "alice@example.com",
                "t1"
            )
            .await,
            Err(EmailNotSent::ProviderFailed),
        );

        // The second is over the per-recipient limit, so the block never
        // reaches the provider at all.
        match send_template_email(
            &limiter,
            &ctx,
            &msg,
            "verification",
            "alice@example.com",
            "t2",
        )
        .await
        {
            Err(EmailNotSent::RateLimited(reason)) => assert!(
                reason.contains("ResourceExhausted"),
                "the refusal must carry the code the block refused with, got {reason:?}"
            ),
            other => panic!("expected a rate-limit refusal, got {other:?}"),
        }
    }

    /// The per-recipient bucket caps one ADDRESS's share of the deployment
    /// ceiling, not one REQUESTER's: a caller naming a fresh address every
    /// time never refills anyone's bucket and would spend the whole ceiling
    /// alone. `RateLimit::AUTH_EMAIL` is what stops that, and it stops it
    /// here — before the email block is called, so nothing is charged to the
    /// shared quota.
    #[tokio::test]
    async fn one_requester_cannot_spend_the_ceiling_by_naming_new_addresses() {
        let ctx = ctx_with_email("10").await;
        let (limiter, msg) = test_mail_request();

        let budget = RateLimit::AUTH_EMAIL.max_requests;
        for i in 0..budget {
            assert_eq!(
                send_template_email(
                    &limiter,
                    &ctx,
                    &msg,
                    "verification",
                    &format!("victim{i}@example.com"),
                    "t",
                )
                .await,
                Err(EmailNotSent::ProviderFailed),
                "send {i} is within this requester's budget and must reach the email block",
            );
        }

        assert_eq!(
            send_template_email(
                &limiter,
                &ctx,
                &msg,
                "verification",
                "victim-past-the-budget@example.com",
                "t",
            )
            .await,
            Err(EmailNotSent::RequesterLimited),
            "a fresh address does not buy a fresh budget",
        );
    }

    /// The budget is per requester, so a second client IP is unaffected by
    /// the first one's flood.
    #[tokio::test]
    async fn the_requester_budget_does_not_follow_the_recipient() {
        let ctx = ctx_with_email("10").await;
        let (limiter, flooder) = test_mail_request();
        let mut other = flooder.clone();
        other.set_meta(wafer_block::meta::META_REQ_CLIENT_IP, "198.51.100.4");

        for i in 0..RateLimit::AUTH_EMAIL.max_requests {
            let _ = send_template_email(
                &limiter,
                &ctx,
                &flooder,
                "verification",
                &format!("a{i}@example.com"),
                "t",
            )
            .await;
        }
        assert_eq!(
            send_template_email(
                &limiter,
                &ctx,
                &flooder,
                "verification",
                "b@example.com",
                "t"
            )
            .await,
            Err(EmailNotSent::RequesterLimited),
        );
        assert_eq!(
            send_template_email(&limiter, &ctx, &other, "verification", "b@example.com", "t").await,
            Err(EmailNotSent::ProviderFailed),
            "another requester still reaches the email block",
        );
    }

    /// A send nobody can perform — the email block is not registered at all
    /// — is `Undeliverable`, not a rate limit and not a silent success. It
    /// is the shape a disabled block, a missing WRAP grant and an allow-list
    /// that matches nothing all arrive in, and the one that must keep
    /// paging: nothing about it clears itself.
    #[tokio::test]
    async fn an_absent_email_block_is_undeliverable_not_rate_limited() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let (limiter, msg) = test_mail_request();
        match send_template_email(
            &limiter,
            &ctx,
            &msg,
            "verification",
            "alice@example.com",
            "t1",
        )
        .await
        {
            Err(failure @ EmailNotSent::Undeliverable(_)) => {
                assert!(
                    failure.needs_an_operator(),
                    "a deployment fault must not be logged as routine"
                );
            }
            other => panic!("expected an undeliverable outcome, got {other:?}"),
        }
    }

    /// The classification reads the error, not its text: only the detail
    /// code (or the wafer code that detail maps to) makes an error a rate
    /// limit. Everything else is a deployment fault that keeps paging.
    #[test]
    fn only_a_rate_limit_error_is_classified_as_one() {
        let limited = crate::blocks::rate_limit::rate_limited_response(30);
        let terminal = futures::executor::block_on(limited.collect_buffered())
            .expect_err("a 429 terminates as an error");
        assert!(matches!(
            classify_email_error(&terminal),
            EmailNotSent::RateLimited(_)
        ));

        // Same words, no code: an error that merely mentions rate limiting
        // must NOT be filed as one.
        let impostor = wafer_run::TerminalNotResponse::Error(wafer_run::WaferError::new(
            wafer_run::ErrorCode::Internal,
            "rate limit exceeded".to_string(),
        ));
        assert!(matches!(
            classify_email_error(&impostor),
            EmailNotSent::Undeliverable(_)
        ));

        // The shapes a deployment fault actually arrives in: an absent or
        // disabled block, a WRAP denial, an allow-list rejection.
        for code in [
            wafer_run::ErrorCode::NotFound,
            wafer_run::ErrorCode::PermissionDenied,
            wafer_run::ErrorCode::InvalidArgument,
        ] {
            let terminal = wafer_run::TerminalNotResponse::Error(wafer_run::WaferError::new(
                code,
                "nope".to_string(),
            ));
            assert!(
                matches!(
                    classify_email_error(&terminal),
                    EmailNotSent::Undeliverable(_)
                ),
                "{code:?} is a deployment fault"
            );
        }
    }

    /// A `200 {"sent": true}` — the only shape that means the message left
    /// the building — is the only one reported as a delivery.
    #[tokio::test]
    async fn only_a_sent_true_body_counts_as_delivered() {
        struct SentOk;

        #[wafer_block::wafer_async_trait]
        impl wafer_run::Block for SentOk {
            fn info(&self) -> wafer_run::BlockInfo {
                wafer_run::BlockInfo::new("impresspress/email", "0.0.1", "service@v1", "stub")
            }
            async fn handle(
                &self,
                _ctx: &dyn Context,
                _msg: Message,
                _input: InputStream,
            ) -> wafer_run::OutputStream {
                crate::http::ok_json(&serde_json::json!({ "sent": true }))
            }
        }

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.register_block("impresspress/email", Arc::new(SentOk));
        let (limiter, msg) = test_mail_request();
        assert_eq!(
            send_template_email(
                &limiter,
                &ctx,
                &msg,
                "verification",
                "alice@example.com",
                "t1"
            )
            .await,
            Ok(()),
        );
    }

    /// Rate limiting is routine; everything else pages. A rate-limit
    /// decision is already reported by the limiter that made it and clears
    /// itself when the window rolls over — a deployment fault does neither.
    #[test]
    fn only_rate_limiting_is_routine() {
        assert!(!EmailNotSent::RequesterLimited.needs_an_operator());
        assert!(!EmailNotSent::RateLimited("429".into()).needs_an_operator());
        assert!(EmailNotSent::Undeliverable("no such block".into()).needs_an_operator());
        assert!(EmailNotSent::ProviderFailed.needs_an_operator());
    }
}
