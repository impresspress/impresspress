//! POST /b/auth/api/signup — relocated from auth/login.rs in Task 5.

use wafer_core::clients::{config, crypto, database as db};
use wafer_run::{context::Context, InputStream, OutputStream};

use crate::{
    blocks::{
        auth::{
            helpers::{
                email_domain_allowed, initial_role_for, issue_tokens_and_cookie, signup_allowed,
            },
            repo::{local_credentials, users},
            USERS_TABLE,
        },
        auth_ui::redirect::{default_post_login_redirect, is_safe_local_redirect},
        errors::{error_response, ErrorCode},
    },
    http::{err_bad_request, err_internal, ResponseBuilder},
    util::{hex_encode, json_map, sha256_hex},
};

/// Returns `Ok(true)` when a user with `email_lower` already exists, `Ok(false)`
/// when not. Any DB failure other than NOT_FOUND propagates — see [SEC-035]
/// note below; collapsing a WRAP denial or connection blip to "email is free"
/// would let a duplicate insert race in past the unique-email constraint.
async fn user_exists(ctx: &dyn Context, email_lower: &str) -> Result<bool, String> {
    match users::find_by_email(ctx, email_lower).await {
        Ok(opt) => Ok(opt.is_some()),
        Err(e) => Err(format!("{e}")),
    }
}

pub async fn handle(ctx: &dyn Context, input: InputStream) -> OutputStream {
    // Enforce ALLOW_SIGNUP on the API (not just the page)
    if !signup_allowed(ctx).await {
        return error_response(ErrorCode::Forbidden, "Signups are currently disabled");
    }

    #[derive(serde::Deserialize)]
    struct SignupReq {
        email: String,
        password: String,
        name: Option<String>,
    }
    let raw = input.collect_to_bytes().await;
    let body: SignupReq = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    let email_lower = body.email.trim().to_lowercase();
    let parts: Vec<&str> = email_lower.splitn(2, '@').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() || !parts[1].contains('.') {
        return error_response(ErrorCode::InvalidEmail, "Invalid email address");
    }

    // Check allowed email domains (if configured)
    if !email_domain_allowed(ctx, &email_lower).await {
        return error_response(
            ErrorCode::InvalidEmail,
            "Signups from this email domain are not allowed",
        );
    }

    if let Err((code, msg)) =
        super::password_policy::validate_new_password(ctx, &body.password).await
    {
        return error_response(code, &msg);
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

    // [SEC-035] If the email is already registered, do NOT confirm that to
    // the caller — return the same generic "check your email" response a
    // fresh signup would produce. The signup endpoint is otherwise a free
    // email-enumeration oracle for password-reset / phishing campaigns.
    //
    // Follow-up: send a "someone tried to sign up with your email" notice
    // to the existing account. Not included in this PR — needs the email
    // block's templating to grow a new template, which is out of scope.
    //
    // Use the typed `users::find_by_email` path (NOT_FOUND → Ok(None));
    // any other Err is a real backend failure (WRAP denial, DB outage)
    // that we must surface, not collapse to "email is free".
    let email_already_taken = match user_exists(ctx, &email_lower).await {
        Ok(t) => t,
        Err(e) => return err_internal("User lookup failed", e),
    };
    if email_already_taken {
        return ResponseBuilder::new().status(201).json(&serde_json::json!({
            "email_verified": false,
            "message": "Account created. Please verify your email before signing in.",
            "user": {
                "id": "",
                "email": email_lower,
            }
        }));
    }

    // Hash password
    let password_hash = match crypto::hash(ctx, &body.password).await {
        Ok(h) => h,
        Err(e) => return err_internal("Failed to hash password", e),
    };

    // Check if email verification is required
    let require_verification =
        config::get_default(ctx, "WAFER_RUN__AUTH__REQUIRE_VERIFICATION", "false").await;
    let require_verification = require_verification == "true" || require_verification == "1";

    // Generate verification token if needed
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
    let role = initial_role_for(ctx, &email_lower).await;

    // Insert via typed repo — no password_hash on the users row.
    let user = match users::insert(
        ctx,
        users::NewUser {
            email: email_lower.clone(),
            display_name: body.name.unwrap_or_default(),
            avatar_url: None,
            role: role.to_string(),
        },
    )
    .await
    {
        Ok(u) => u,
        Err(e) => return err_internal("Failed to create user", e),
    };

    if let Err(e) = local_credentials::insert(ctx, &user.id, &password_hash, false).await {
        return err_internal("Failed to store credentials", e);
    }

    // Set email_verified and verification_token on the legacy USERS_TABLE row
    // (Plan A2 users table stores email_verified too — keep them in sync).
    // Persist only `sha256_hex(raw)`; the raw token goes out only in the
    // verification email below.
    {
        let stored_verification = if verification_token.is_empty() {
            String::new()
        } else {
            sha256_hex(verification_token.as_bytes())
        };
        let mut upd = json_map(serde_json::json!({
            "email_verified": !require_verification,
            "verification_token": stored_verification,
        }));
        crate::util::stamp_updated(&mut upd);
        if let Err(e) = db::update(ctx, USERS_TABLE, &user.id, upd).await {
            tracing::warn!("Failed to set email_verified on signup: {e}");
        }
    }

    let roles = vec![role.to_string()];

    // Send verification email if required
    if require_verification {
        super::send_template_email(ctx, "verification", &email_lower, &verification_token).await;
        // Do NOT issue tokens before email is verified
        return ResponseBuilder::new().status(201).json(&serde_json::json!({
            "email_verified": false,
            "message": "Account created. Please verify your email before signing in.",
            "user": {
                "id": user.id,
                "email": email_lower,
            }
        }));
    }

    // Mint tokens, persist the refresh + session rows, build the cookie
    // (only when email verification is NOT required) — this is the
    // auto-login path: a brand-new user is fully signed in by the time this
    // response reaches the browser, no separate login step needed.
    let issued =
        match issue_tokens_and_cookie(ctx, &user.id, &email_lower, &roles, "password", None, 0)
            .await
        {
            Ok(i) => i,
            Err(r) => return r,
        };

    // Role-aware post-login default (Fix 2 / signup UX): a brand-new signup
    // is (almost) never an admin, so this sends them to `/b/userportal/`
    // instead of the silent bounce to `/b/auth/login` the page used to do —
    // same single-sourced rule Fix 1 applies to login/OAuth/bootstrap.
    let post_login_raw =
        config::get_default(ctx, "WAFER_RUN_SHARED__POST_LOGIN_REDIRECT", "/b/admin/").await;
    let admin_default = if is_safe_local_redirect(&post_login_raw) {
        post_login_raw
    } else {
        "/b/admin/".to_string()
    };
    let is_admin = roles.iter().any(|r| r == "admin");
    let default_redirect = default_post_login_redirect(is_admin, &admin_default);

    ResponseBuilder::new()
        .status(201)
        .set_cookie(&issued.cookie)
        .json(&serde_json::json!({
            "access_token": issued.access_token,
            "refresh_token": issued.refresh_token,
            "token_type": "Bearer",
            "expires_in": issued.access_lifetime,
            "email_verified": true,
            "default_redirect": default_redirect,
            "user": {
                "id": user.id,
                "email": email_lower,
                "roles": roles,
                "name": user.display_name
            }
        }))
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
    use std::sync::Arc;

    use super::*;
    use crate::test_support::{output_json, TestContext};

    async fn ctx_with_crypto() -> TestContext {
        let mut ctx = TestContext::with_auth().await;
        let svc = Arc::new(
            wafer_block_crypto::service::Argon2JwtCryptoService::new(
                "test-jwt-secret-padded-to-min-32-bytes-aaaa".to_string(),
            )
            .expect("test secret is long enough"),
        );
        let crypto_block: Arc<dyn wafer_run::Block> =
            Arc::new(wafer_core::service_blocks::crypto::CryptoBlock::new(svc));
        ctx.register_block("wafer-run/crypto", crypto_block);
        ctx
    }

    async fn signup(ctx: &TestContext, email: &str, password: &str) -> serde_json::Value {
        let body = serde_json::json!({"email": email, "password": password}).to_string();
        let out = handle(ctx, InputStream::from_bytes(body.into_bytes())).await;
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
        ctx.set_config(
            "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL",
            "admin@example.com",
        );

        let resp = signup(&ctx, "admin@example.com", "correct-horse-battery").await;

        assert_eq!(resp["user"]["roles"], serde_json::json!(["admin"]));
        assert_eq!(resp["default_redirect"], "/b/admin/");
    }

    #[tokio::test]
    async fn verification_required_does_not_auto_login() {
        let mut ctx = ctx_with_crypto().await;
        ctx.set_config("WAFER_RUN__AUTH__REQUIRE_VERIFICATION", "true");

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

        // The account was still created — a duplicate signup must return the
        // generic "already registered" response, not a fresh 201.
        let user = users::find_by_email(&ctx, "pending@example.com")
            .await
            .unwrap()
            .expect("user row created even though verification is pending");
        assert!(!user.email_verified);
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
}
