//! POST /b/auth/api/signup — relocated from auth/login.rs in Task 5.

use std::sync::Arc;

use wafer_core::clients::crypto;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::{
        auth::{
            hash_new_password,
            helpers::{
                email_domain_allowed, initial_role_for, issue_tokens_and_cookie, signup_allowed,
                RoleSource, Rotation, SessionLifetime, TokenGrant,
            },
            repo::users,
        },
        auth_ui::{
            contracts::{
                AuthenticatedUser, EmailVerified, PendingSignupUser, SignupRequest, SignupResponse,
                TokenType,
            },
            redirect::{configured_admin_default, default_post_login_redirect},
        },
        crud,
        errors::{error_response, ErrorCode},
        rate_limit::UserRateLimiter,
    },
    http::{err_bad_request, err_internal, ResponseBuilder},
    util::{hex_encode, sha256_hex},
};

/// The no-auto-login signup response. Under `REQUIRE_VERIFICATION` a fresh
/// signup and an already-registered address both answer exactly this, byte
/// for byte, so the reply cannot tell a caller whether the address has an
/// account ([SEC-035]). That is why it takes nothing but the address the
/// caller sent: no account id, which exists on only one of the two paths,
/// and nothing read back from a row.
///
/// Without `REQUIRE_VERIFICATION` a fresh signup is signed in on the spot and
/// answers tokens instead, so an address that answers this is one that is
/// already registered. That channel is the price of auto-login, not an
/// oversight; a deployment that needs signup not to reveal registered
/// addresses turns verification on.
fn pending_verification(email: String) -> SignupResponse {
    SignupResponse::PendingVerification {
        email_verified: EmailVerified,
        message: "Account created. Please verify your email before signing in.".to_string(),
        user: PendingSignupUser { email },
    }
}

pub async fn handle(
    limiter: &Arc<UserRateLimiter>,
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    // Enforce ALLOW_SIGNUP on the API (not just the page)
    let signup_allowed = match signup_allowed(ctx).await {
        Ok(allowed) => allowed,
        Err(e) => return crud::db_error_internal(e, "Could not read the signup switch"),
    };
    if !signup_allowed {
        return error_response(ErrorCode::Forbidden, "Signups are currently disabled");
    }

    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: SignupRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    let email_lower = users::normalize_email(&body.email);
    let parts: Vec<&str> = email_lower.splitn(2, '@').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() || !parts[1].contains('.') {
        return error_response(ErrorCode::InvalidEmail, "Invalid email address");
    }

    // Check allowed email domains (if configured)
    let domain_allowed = match email_domain_allowed(ctx, &email_lower).await {
        Ok(allowed) => allowed,
        Err(e) => {
            return crud::db_error_internal(e, "Could not read the allowed email domains");
        }
    };
    if !domain_allowed {
        return error_response(
            ErrorCode::InvalidEmail,
            "Signups from this email domain are not allowed",
        );
    }

    match super::password_policy::validate_new_password(ctx, &body.password).await {
        Ok(Ok(())) => {}
        Ok(Err((code, msg))) => return error_response(code, &msg),
        Err(response) => return response,
    }
    if email_lower.len() > 255 {
        return error_response(
            ErrorCode::InvalidEmail,
            "Email must not exceed 255 characters",
        );
    }
    if let Some(ref name) = body.name {
        if name.len() > 200 {
            return error_response(
                ErrorCode::InvalidInput,
                "Name must not exceed 200 characters",
            );
        }
    }

    // Everything up to and including the account write runs whether or not
    // the address is taken, because what a caller measures is not only the
    // reply ([SEC-035], see `pending_verification`) but how long it took.
    // A registered address answered before the ~100 ms password hash was an
    // enumeration oracle with the same body: fast meant "has an account".
    // So the hash, the token draw and the write happen on both paths, the
    // write is what finds out the address is taken, and the mail — the one
    // slow step only a new account has — goes out after the response.
    let password_hash = match hash_new_password(ctx, &body.password).await {
        Ok(hash) => hash,
        Err(response) => return response,
    };

    let require_verification = match crate::config_vars::get_bool(
        ctx,
        crate::blocks::auth::config::REQUIRE_VERIFICATION_KEY,
        false,
    )
    .await
    {
        Ok(required) => required,
        Err(e) => return crud::db_error_internal(e, "Could not read the verification policy"),
    };

    let verification_token = if require_verification {
        match crypto::random_bytes(ctx, 32).await {
            Ok(bytes) => hex_encode(&bytes),
            Err(e) => return err_internal("Failed to generate verification token", e),
        }
    } else {
        String::new()
    };

    // Determine the role: admin if the email matches the configured bootstrap
    // admin email (re-uses the same key as bootstrap for consistency).
    let role = match initial_role_for(ctx, &email_lower).await {
        Ok(role) => role,
        Err(e) => return crud::db_error_internal(e, "Could not read the bootstrap admin email"),
    };

    // The account row and its `local_credentials` row in one atomic write —
    // as two, a failure between them left an account with no password whose
    // address answered "already registered" to every retry. No `user_roles`
    // row: the initial role is the inline `users.role` column `NewUser.role`
    // writes, which is what `helpers::get_user_roles` reads first. The
    // verification state rides on the insert too.
    //
    // A taken address fails the write with `AlreadyExists` (`users.email` is
    // UNIQUE) and writes nothing; that is the registered answer. Any other
    // failure is a real backend fault on a new address and is reported as
    // one — it is not the enumeration oracle, because a registered address
    // never reaches it.
    // Resolved before the account exists when this signup will sign the user
    // in: a misconfigured session lifetime then refuses the request instead of
    // creating an account that answers "already registered" to the retry (see
    // `SessionLifetime`). A signup awaiting verification issues nothing.
    let lifetime = if require_verification {
        None
    } else {
        match SessionLifetime::resolve_or_error(ctx).await {
            Ok(lifetime) => Some(lifetime),
            Err(r) => return r,
        }
    };

    let user = match users::insert_with_password(
        ctx,
        users::NewUser {
            email: email_lower.clone(),
            display_name: body.name.unwrap_or_default(),
            avatar_url: None,
            role: role.to_string(),
            email_verified: !require_verification,
            // Persist only `sha256_hex(raw)`; the raw token goes out solely
            // in the verification email below.
            verification_token_hash: (!verification_token.is_empty())
                .then(|| sha256_hex(verification_token.as_bytes())),
        },
        &password_hash,
    )
    .await
    {
        Ok(u) => u,
        Err(e) if e.code == wafer_run::ErrorCode::AlreadyExists => {
            // Follow-up: send a "someone tried to sign up with your email"
            // notice to the existing account — after the response, like the
            // verification mail below. Needs the email block's templating to
            // grow a new template.
            return ResponseBuilder::new()
                .status(201)
                .json(&pending_verification(email_lower));
        }
        Err(e) => return crud::db_error_internal(e, "Failed to create account"),
    };

    // `None` exactly when verification is required (resolved above).
    let Some(lifetime) = lifetime else {
        // After the response: see above. The body cannot carry a send
        // failure anyway — it is the same body the registered branch
        // answers, and one that varied with whether mail went out would be
        // the oracle that branch closes. The account exists and the resend
        // endpoint can mint a fresh token, so the log line is what was
        // missing when mail fails.
        let mail = super::LaterMail::capture(limiter, ctx, msg);
        let (to, user_id) = (email_lower.clone(), user.id.clone());
        crate::deferred::defer(async move {
            if let Err(failure) = mail
                .send_template("verification", &to, &verification_token)
                .await
            {
                super::log_email_not_sent("signup", &user_id, &failure);
            }
        });
        // Do NOT issue tokens before email is verified
        return ResponseBuilder::new()
            .status(201)
            .json(&pending_verification(email_lower));
    };

    // The new account's roles, as stored, and the `auth_version` of the row
    // just inserted (see `TokenGrant`).
    let grant = match TokenGrant::resolve(ctx, &user, RoleSource::Stored).await {
        Ok(grant) => grant,
        Err(e) => return crud::db_error_internal(e, "Failed to resolve user roles"),
    };

    // Mint tokens, persist the refresh + session rows, build the cookie
    // (only when email verification is NOT required) — this is the
    // auto-login path: a brand-new user is fully signed in by the time this
    // response reaches the browser, no separate login step needed.
    let issued = match issue_tokens_and_cookie(
        ctx,
        &lifetime,
        &user.id,
        &email_lower,
        &grant,
        "password",
        Rotation::NewFamily,
    )
    .await
    {
        Ok(i) => i,
        Err(r) => return r,
    };

    // Role-aware post-login default (Fix 2 / signup UX): a brand-new signup
    // is (almost) never an admin, so this sends them to `/b/userportal/`
    // instead of the silent bounce to `/b/auth/login` the page used to do —
    // same single-sourced rule Fix 1 applies to login/OAuth/bootstrap.
    let admin_default = match configured_admin_default(ctx).await {
        Ok(admin_default) => admin_default,
        Err(e) => return crud::db_error_internal(e, "Could not read the post-login redirect"),
    };
    let roles = grant.into_roles();
    let is_admin = roles.iter().any(|r| r == "admin");
    let default_redirect = default_post_login_redirect(is_admin, &admin_default);

    ResponseBuilder::new()
        .status(201)
        .set_cookie(&issued.cookie)
        .json(&SignupResponse::SignedIn {
            email_verified: EmailVerified,
            access_token: issued.access_token,
            refresh_token: issued.refresh_token,
            token_type: TokenType::Bearer,
            expires_in: issued.access_lifetime,
            default_redirect,
            user: AuthenticatedUser {
                id: user.id,
                email: email_lower,
                roles,
                name: user.display_name,
            },
        })
}

/// Signup UX (Fix 2) regression tests. Before this fix, a successful signup
/// with verification NOT required already auto-logged the caller in
/// (tokens issued, cookie set) but the page's JS ignored that and
/// unconditionally navigated to `/b/auth/login` — a silent bounce with no
/// feedback. These tests drive the real [`handle`] end-to-end and assert on
/// the `default_redirect` the (now role-aware) auto-login response carries,
/// plus that the verification-required path still does NOT auto-login.
#[cfg(test)]
mod tests {

    use super::*;
    use crate::{
        blocks::auth::config::BOOTSTRAP_ADMIN_EMAIL_KEY,
        test_support::{output_json, TestContext},
    };

    async fn ctx_with_crypto() -> TestContext {
        TestContext::with_auth_and_crypto().await
    }

    async fn signup(ctx: &TestContext, email: &str, password: &str) -> serde_json::Value {
        let body = serde_json::json!({"email": email, "password": password}).to_string();
        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        let out = handle(
            &limiter,
            ctx,
            &msg,
            InputStream::from_bytes(body.into_bytes()),
        )
        .await;
        output_json(out).await
    }

    #[tokio::test]
    async fn regular_signup_auto_logs_in_and_defaults_to_userportal() {
        let ctx = ctx_with_crypto().await;

        let resp = signup(&ctx, "newuser@example.com", "correct-horse-battery").await;

        assert_eq!(resp["email_verified"], true);
        assert!(
            resp["access_token"].is_string() && !resp["access_token"].as_str().unwrap().is_empty(),
            "verification not required — signup must auto-login (issue a token): {resp}"
        );
        assert_eq!(
            resp["default_redirect"], "/b/userportal/",
            "brand-new non-admin signup must land on the user portal, not \
             bounce to /b/auth/login with no feedback: {resp}"
        );
    }

    #[tokio::test]
    async fn admin_email_signup_defaults_to_admin_home() {
        let mut ctx = ctx_with_crypto().await;
        ctx.set_config(BOOTSTRAP_ADMIN_EMAIL_KEY, "admin@example.com");

        let resp = signup(&ctx, "admin@example.com", "correct-horse-battery").await;

        assert_eq!(resp["user"]["roles"], serde_json::json!(["admin"]));
        assert_eq!(resp["default_redirect"], "/b/admin/");
    }

    #[tokio::test]
    async fn verification_required_does_not_auto_login() {
        let mut ctx = ctx_with_crypto().await;
        ctx.set_config(
            crate::blocks::auth::config::REQUIRE_VERIFICATION_KEY,
            "true",
        );

        let resp = signup(&ctx, "pending@example.com", "correct-horse-battery").await;

        assert_eq!(resp["email_verified"], false);
        assert!(
            resp.get("access_token").is_none(),
            "verification required — signup must NOT auto-login: {resp}"
        );
        assert!(
            resp.get("default_redirect").is_none(),
            "no redirect target is minted when the user isn't logged in yet: {resp}"
        );

        // The account was still created, unverified.
        let user = users::find_by_email(&ctx, "pending@example.com")
            .await
            .unwrap()
            .expect("user row created even though verification is pending");
        assert!(!user.email_verified);
    }

    /// Everything the HTTP boundary would send for one signup attempt:
    /// status, headers (cookies included) and body bytes.
    async fn signup_on_the_wire(
        ctx: &TestContext,
        email: &str,
        password: &str,
    ) -> wafer_block::http_codec::HttpResponseParts {
        let body = serde_json::json!({"email": email, "password": password}).to_string();
        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        let out = handle(
            &limiter,
            ctx,
            &msg,
            InputStream::from_bytes(body.into_bytes()),
        )
        .await;
        wafer_block::http_codec::collect_http_response(out).await
    }

    /// [SEC-035] Under REQUIRE_VERIFICATION, signing up with an address
    /// that is already registered answers exactly what signing it up fresh
    /// answered. Both attempts use the same address, so nothing but the
    /// account's existence differs between them, and the comparison is the
    /// whole response a caller receives: a difference anywhere — an account
    /// id on one side, a header, a status — is an oracle for which addresses
    /// have accounts.
    #[tokio::test]
    async fn verification_required_signup_is_byte_identical_for_new_and_registered_addresses() {
        let mut ctx = ctx_with_crypto().await;
        ctx.set_config(
            crate::blocks::auth::config::REQUIRE_VERIFICATION_KEY,
            "true",
        );

        let fresh = signup_on_the_wire(&ctx, "someone@example.com", "correct-horse-battery").await;
        assert!(
            users::find_by_email(&ctx, "someone@example.com")
                .await
                .unwrap()
                .is_some(),
            "the first attempt must create the account, or the second is not the registered case"
        );
        let registered =
            signup_on_the_wire(&ctx, "someone@example.com", "another-password-entirely").await;

        assert_eq!(fresh.status, registered.status, "status");
        assert_eq!(fresh.headers, registered.headers, "headers");
        assert_eq!(
            String::from_utf8_lossy(&fresh.body),
            String::from_utf8_lossy(&registered.body),
            "body"
        );
    }

    #[tokio::test]
    async fn duplicate_email_signup_response_has_no_default_redirect() {
        let ctx = ctx_with_crypto().await;
        signup(&ctx, "dupe@example.com", "correct-horse-battery").await;

        // Second attempt with the same email — [SEC-035] generic response,
        // no tokens, so no redirect target either.
        let resp = signup(&ctx, "dupe@example.com", "some-other-password").await;
        assert!(resp.get("access_token").is_none());
        assert!(resp.get("default_redirect").is_none());
    }

    /// `email_verified` and the tokens are one fact on every path the
    /// endpoint has: verification off or on, a fresh address or a
    /// registered one. Driven through the block's own route table, so the
    /// body checked is the one a browser receives, and each body must also
    /// decode as the published contract.
    #[tokio::test]
    async fn email_verified_is_true_exactly_when_the_reply_carries_tokens() {
        use wafer_run::Block;

        use crate::blocks::auth_ui::AuthUiBlock;

        async fn post(ctx: &TestContext, email: &str) -> (u16, serde_json::Value) {
            let mut msg = crate::test_support::anon_msg("create", "/b/auth/api/signup");
            msg.set_meta(wafer_block::meta::META_REQ_CLIENT_IP, "203.0.113.7");
            let body = serde_json::json!({"email": email, "password": "correct-horse-battery"});
            let parts = wafer_block::http_codec::collect_http_response(
                AuthUiBlock::default()
                    .handle(
                        ctx,
                        msg,
                        InputStream::from_bytes(serde_json::to_vec(&body).expect("body")),
                    )
                    .await,
            )
            .await;
            let json = serde_json::from_slice(&parts.body).expect("signup answers JSON");
            (parts.status, json)
        }

        for require_verification in [false, true] {
            let mut ctx = ctx_with_crypto().await;
            ctx.set_config(
                crate::blocks::auth::config::REQUIRE_VERIFICATION_KEY,
                if require_verification {
                    "true"
                } else {
                    "false"
                },
            );
            let fresh = post(&ctx, "someone@example.com").await;
            let registered = post(&ctx, "someone@example.com").await;

            for (path, (status, body)) in [("fresh", fresh), ("registered", registered)] {
                let case = format!("verification {require_verification}, {path} address");
                assert_eq!(status, 201, "{case}: {body}");
                assert_eq!(
                    body["email_verified"].as_bool(),
                    Some(body.get("access_token").is_some()),
                    "{case}: `email_verified` must say whether tokens were issued: {body}"
                );
                let decoded: SignupResponse = serde_json::from_value(body.clone())
                    .unwrap_or_else(|e| panic!("{case}: not the published contract ({e}): {body}"));
                let signed_in = !require_verification && path == "fresh";
                assert_eq!(
                    matches!(decoded, SignupResponse::SignedIn { .. }),
                    signed_in,
                    "{case}: wrong variant: {body}"
                );
            }
        }
    }

    /// The account row and its password are one write. As two, a failure
    /// between them kept the account — with no password — and the address
    /// then answered "already registered" to every retry, so its owner could
    /// neither sign up nor sign in.
    #[tokio::test]
    async fn a_failure_writing_the_password_leaves_no_account_and_the_retry_succeeds() {
        use crate::blocks::auth::repo::{
            local_credentials,
            test_faults::{drop_trigger, fail_inserts_into},
        };

        let ctx = ctx_with_crypto().await;
        let trigger = fail_inserts_into(&ctx, local_credentials::TABLE).await;

        let failed = signup_on_the_wire(&ctx, "halfway@example.com", "correct-horse-battery").await;
        assert_eq!(
            failed.status, 500,
            "a new address whose write failed is told so"
        );
        assert!(
            users::find_by_email(&ctx, "halfway@example.com")
                .await
                .expect("user lookup ok")
                .is_none(),
            "the account row must not outlive its failed password write"
        );

        drop_trigger(&ctx, &trigger).await;
        let retry = signup(&ctx, "halfway@example.com", "correct-horse-battery").await;
        assert!(
            retry["access_token"].is_string(),
            "the retry must create the account and sign it in, not be told the \
             address is already registered: {retry}"
        );
    }

    /// [SEC-035] Under REQUIRE_VERIFICATION the reply for a registered
    /// address is the fresh-signup reply byte for byte (pinned above). This
    /// pins the other half: the handler does the same work for both before
    /// answering — the same block calls, in the same order, the password
    /// hash included — so the reply does not arrive measurably sooner for a
    /// registered address either. The one step only a new account has, the
    /// verification mail, runs after the response.
    #[tokio::test]
    async fn a_registered_address_costs_signup_the_same_work_as_a_new_one() {
        use super::super::{run_deferred, CallLog};

        let mut ctx = ctx_with_crypto().await;
        ctx.set_config(
            crate::blocks::auth::config::REQUIRE_VERIFICATION_KEY,
            "true",
        );
        let ctx = CallLog::new(ctx);
        crate::deferred::queue_for_test();

        async fn attempt(ctx: &CallLog, email: &str) -> u16 {
            let body = serde_json::json!({"email": email, "password": "correct-horse-battery"})
                .to_string();
            let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
            crate::test_support::output_status(
                handle(
                    &limiter,
                    ctx,
                    &msg,
                    InputStream::from_bytes(body.into_bytes()),
                )
                .await,
            )
            .await
        }

        assert_eq!(attempt(&ctx, "someone@example.com").await, 201);
        let fresh = ctx.take();
        let fresh_deferred = run_deferred().await;
        let deferred = ctx.take();

        assert_eq!(attempt(&ctx, "someone@example.com").await, 201);
        let registered = ctx.take();
        let registered_deferred = run_deferred().await;

        assert_eq!(
            fresh, registered,
            "a registered address must perform the same operations, in the same order, as a new one (a failed write aborts where a new one commits; the calls are the same)"
        );
        assert_eq!(
            (fresh_deferred, registered_deferred),
            (1, 0),
            "a new account's verification mail goes out after the response; a \
             registered address has none to send"
        );
        assert_eq!(
            fresh
                .iter()
                .filter(|call| call.as_str() == "wafer-run/crypto crypto.hash")
                .count(),
            1,
            "both paths hash the password: {fresh:?}"
        );
        assert!(
            !fresh
                .iter()
                .any(|call| call.starts_with("impresspress/email")),
            "no mail is sent before the response: {fresh:?}"
        );
        assert!(
            deferred
                .iter()
                .any(|call| call == "impresspress/email email.send_template"),
            "the deferred task is the verification mail: {deferred:?}"
        );
    }

    async fn signup_status(ctx: &TestContext, email: &str, password: &str) -> u16 {
        let body = serde_json::json!({"email": email, "password": password}).to_string();
        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        let out = handle(
            &limiter,
            ctx,
            &msg,
            InputStream::from_bytes(body.into_bytes()),
        )
        .await;
        crate::test_support::output_http_status(out).await
    }

    /// A password hasher that cannot be reached is an outage the caller may
    /// retry: a 503, before any account is written, so the retry succeeds.
    #[tokio::test]
    async fn an_unreachable_password_hasher_is_a_503_and_writes_no_account() {
        use crate::test_support::HasherFault;

        let (ctx, hasher) = TestContext::with_auth_and_faulty_hasher().await;
        hasher.fail_hash(Some(HasherFault::Unreachable));

        assert_eq!(
            signup_status(&ctx, "ivy@example.com", "correct-horse-battery").await,
            503
        );
        assert!(
            users::find_by_email(&ctx, "ivy@example.com")
                .await
                .unwrap()
                .is_none(),
            "a sign-up the hasher could not serve writes no account"
        );

        hasher.fail_hash(None);
        let resp = signup(&ctx, "ivy@example.com", "correct-horse-battery").await;
        assert!(
            resp["access_token"].is_string(),
            "the retry signs up: {resp}"
        );
    }

    /// Guard: a hashing fault retrying does not fix stays a 500 (passes
    /// before and after the `Unavailable` classification by design).
    #[tokio::test]
    async fn a_broken_password_hasher_is_a_500() {
        use crate::test_support::HasherFault;

        let (ctx, hasher) = TestContext::with_auth_and_faulty_hasher().await;
        hasher.fail_hash(Some(HasherFault::Broken));

        assert_eq!(
            signup_status(&ctx, "jay@example.com", "correct-horse-battery").await,
            500
        );
        assert!(users::find_by_email(&ctx, "jay@example.com")
            .await
            .unwrap()
            .is_none());
    }
}
