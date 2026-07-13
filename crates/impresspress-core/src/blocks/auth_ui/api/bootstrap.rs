//! POST /b/auth/api/bootstrap — bootstrap admin token redemption.
//!
//! The first-boot bootstrap flow can install a sha256(token) row into
//! `wafer_run__auth__bootstrap_tokens` (24h expiry) instead of creating an
//! admin user directly — see [`crate::blocks::auth::bootstrap`]. This
//! handler is the redemption side: holder posts the raw token + chosen
//! email/password, we verify the token row, create the admin user via the
//! same code path the env-var bootstrap uses, consume the token row, and
//! mint a session cookie identical to the one [`super::login`] would issue.
//!
//! Request body is `application/x-www-form-urlencoded` (the GET page
//! submits a plain HTML form — no JS).

use wafer_core::clients::config;
use wafer_run::{context::Context, InputStream, OutputStream};

use crate::{
    blocks::{
        auth::{
            bootstrap,
            helpers::issue_tokens_and_cookie,
            repo::{bootstrap_tokens, users},
            service::hash_token,
        },
        auth_ui::redirect::{default_post_login_redirect, is_safe_local_redirect},
        errors::error_response,
    },
    http::{
        err_bad_request, err_internal, err_internal_no_cause, err_unauthorized, ResponseBuilder,
    },
    util::parse_form_body,
};

pub async fn handle(ctx: &dyn Context, input: InputStream) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let form = parse_form_body(&raw);
    let token = match form.get("token") {
        Some(t) if !t.is_empty() => t.clone(),
        _ => return err_bad_request("missing token"),
    };
    let email = match form.get("email") {
        Some(e) if !e.is_empty() => e.trim().to_lowercase(),
        _ => return err_bad_request("missing email"),
    };
    let password = match form.get("password") {
        Some(p) if !p.is_empty() => p.clone(),
        _ => return err_bad_request("missing password"),
    };
    if let Err((code, msg)) = super::password_policy::validate_new_password(ctx, &password).await {
        return error_response(code, &msg);
    }

    let token_hash = hash_token(&token);

    // 1. Verify the token row exists and hasn't expired.
    match bootstrap_tokens::is_valid(ctx, &token_hash).await {
        Ok(true) => {}
        Ok(false) => return err_unauthorized("invalid or expired bootstrap token"),
        Err(e) => return err_internal("bootstrap_tokens lookup", e),
    }

    // 2. Create the admin user via the same code path bootstrap-on-init uses.
    //    Reusing this keeps the legacy companion columns (`name`, `disabled`,
    //    `deleted_at`) and the local_credentials row consistent with the
    //    env-var path.
    if let Err(e) = bootstrap::bootstrap_with_email_password(ctx, &email, &password).await {
        return err_internal("create admin", e);
    }

    // 3. Consume the token row. Best-effort: the admin user already exists,
    //    so a delete failure here just leaves a stale row that will expire on
    //    its own.
    if let Err(e) = bootstrap_tokens::delete_by_hash(ctx, &token_hash).await {
        tracing::warn!(
            "bootstrap_tokens::delete_by_hash after redemption failed (admin already created): {e}"
        );
    }

    // 4. Look up the just-created user so we have its id for session minting.
    let user = match users::find_by_email(ctx, &email).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return err_internal_no_cause(
                "bootstrap created admin but find_by_email returned no row",
            )
        }
        Err(e) => return err_internal("users::find_by_email after bootstrap", e),
    };

    // 5. Mint a session — same shared token-issuance tail as login/signup.
    let roles = vec!["admin".to_string()];
    let issued =
        match issue_tokens_and_cookie(ctx, &user.id, &email, &roles, "password", None, 0).await {
            Ok(i) => i,
            Err(r) => return r,
        };

    // 6. Set the auth cookie + redirect to a real post-login destination. The
    //    form is a plain HTML POST (no JS), so a 302 with Set-Cookie is the
    //    right completion signal. Honor WAFER_RUN_SHARED__POST_LOGIN_REDIRECT
    //    (validated) like login/oauth, defaulting to the admin home — the old
    //    `/b/auth/dashboard` target is not a registered route (404).
    let post_login_raw =
        config::get_default(ctx, "WAFER_RUN_SHARED__POST_LOGIN_REDIRECT", "/b/admin/").await;
    let admin_default = if is_safe_local_redirect(&post_login_raw) {
        post_login_raw
    } else {
        "/b/admin/".to_string()
    };
    // Bootstrap redemption always creates the admin account (step 2 above),
    // so `is_admin` is always `true` here — routed through the same
    // single-sourced rule as login/OAuth (`redirect::default_post_login_redirect`)
    // for consistency, not because the outcome differs.
    let dest = default_post_login_redirect(true, &admin_default);
    ResponseBuilder::new()
        .status(302)
        .set_cookie(&issued.cookie)
        .set_header("Location", &dest)
        .empty()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::test_support::TestContext;

    /// Register a real crypto block on the test context — bootstrap admin
    /// creation goes through `crypto::hash` for the password, and session
    /// minting goes through `crypto::sign`/`random_bytes`. Without this the
    /// handler trips on `block 'wafer-run/crypto' not registered`.
    async fn ctx_with_crypto() -> TestContext {
        let mut ctx = TestContext::with_auth().await;
        let svc = Arc::new(
            wafer_block_crypto::service::Argon2JwtCryptoService::new(
                // ≥ 32 bytes for HMAC-SHA256 minimum-length check.
                "test-jwt-secret-padded-to-min-32-bytes-aaaa".to_string(),
            )
            .expect("test secret is long enough"),
        );
        let crypto_block: Arc<dyn wafer_run::Block> =
            Arc::new(wafer_core::service_blocks::crypto::CryptoBlock::new(svc));
        ctx.register_block("wafer-run/crypto", crypto_block);
        ctx
    }

    #[tokio::test]
    async fn redeems_valid_token_creates_admin_and_consumes_row() {
        let ctx = ctx_with_crypto().await;

        // Seed a bootstrap token row (sha256 of "test-token-xyz").
        let raw = "test-token-xyz";
        let hash = hash_token(raw);
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        bootstrap_tokens::insert(&ctx, hash.clone(), &expires)
            .await
            .unwrap();

        // POST the form.
        let form = format!("token={raw}&email=admin@example.com&password=test1234");
        let input = InputStream::from_bytes(form.into_bytes());
        let _ = handle(&ctx, input).await;

        // Admin user got created.
        let user = users::find_by_email(&ctx, "admin@example.com")
            .await
            .unwrap()
            .expect("admin user created");
        assert_eq!(user.role, "admin");

        // Bootstrap-token row consumed.
        assert!(!bootstrap_tokens::is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn rejects_invalid_token() {
        let ctx = ctx_with_crypto().await;
        let form = "token=wrong&email=admin@example.com&password=test1234";
        let input = InputStream::from_bytes(form.as_bytes().to_vec());
        let _ = handle(&ctx, input).await;
        // No admin user was created.
        assert!(users::find_by_email(&ctx, "admin@example.com")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn rejects_short_password() {
        let ctx = ctx_with_crypto().await;
        // Even with a valid token row, the handler must reject password <8 chars.
        let raw = "another-token";
        let hash = hash_token(raw);
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        bootstrap_tokens::insert(&ctx, hash.clone(), &expires)
            .await
            .unwrap();

        let form = format!("token={raw}&email=admin@example.com&password=short");
        let input = InputStream::from_bytes(form.into_bytes());
        let _ = handle(&ctx, input).await;

        // No admin user, token row still valid (not consumed on rejection).
        assert!(users::find_by_email(&ctx, "admin@example.com")
            .await
            .unwrap()
            .is_none());
        assert!(bootstrap_tokens::is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn rejects_common_password() {
        let ctx = ctx_with_crypto().await;
        // "admin123" is 8 chars — the old `password.len() < 8` check let it
        // through. It must now be rejected via the shared blocklist
        // (`validate_new_password`) routed through in Task 5.
        let raw = "common-pw-token";
        let hash = hash_token(raw);
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        bootstrap_tokens::insert(&ctx, hash.clone(), &expires)
            .await
            .unwrap();

        let form = format!("token={raw}&email=admin@example.com&password=admin123");
        let input = InputStream::from_bytes(form.into_bytes());
        let _ = handle(&ctx, input).await;

        // No admin user, token row still valid (not consumed on rejection).
        assert!(users::find_by_email(&ctx, "admin@example.com")
            .await
            .unwrap()
            .is_none());
        assert!(bootstrap_tokens::is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn redeems_valid_token_redirects_to_a_real_route() {
        use wafer_run::{MetaGet, META_RESP_STATUS};
        let ctx = ctx_with_crypto().await;
        let raw = "redirect-token";
        let hash = hash_token(raw);
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        bootstrap_tokens::insert(&ctx, hash, &expires)
            .await
            .unwrap();

        let form = format!("token={raw}&email=admin@example.com&password=test1234");
        let buf = handle(&ctx, InputStream::from_bytes(form.into_bytes()))
            .await
            .collect_buffered()
            .await
            .expect("redirect response");
        assert_eq!(MetaGet::get(&buf.meta, META_RESP_STATUS), Some("302"));
        // Defaults to the admin home (no POST_LOGIN_REDIRECT configured); the
        // old `/b/auth/dashboard` target was an unregistered route (404).
        assert_eq!(
            MetaGet::get(&buf.meta, "resp.header.Location"),
            Some("/b/admin/")
        );
    }
}
