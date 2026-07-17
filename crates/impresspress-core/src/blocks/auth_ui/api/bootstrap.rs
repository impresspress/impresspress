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
use wafer_run::{context::Context, InputStream, Message, OutputStream};

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
        err_bad_request, err_forbidden, err_internal, err_internal_no_cause, err_unauthorized,
        ResponseBuilder,
    },
    util::parse_form_body,
};

pub async fn handle(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let form = parse_form_body(&raw);

    // CSRF defense-in-depth: this is a plain (no-JS) `<form>` POST, so the
    // Fetch-Metadata/Origin layer (`crate::csrf::enforce_origin_policy`) is
    // the only thing that would otherwise gate it — that layer only applies
    // to COOKIE-authenticated requests, and this endpoint runs before any
    // session exists. Validate the token the GET page embedded via
    // `crate::csrf::hidden_field` instead.
    let submitted_csrf = form
        .get(crate::csrf::FIELD_NAME)
        .map(String::as_str)
        .unwrap_or("");
    if !crate::csrf::verify(ctx, msg, submitted_csrf) {
        return err_forbidden("invalid or missing csrf token");
    }

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

    // 1. Atomically validate AND consume the token in a single
    //    `DELETE ... RETURNING` round trip (`take_valid_by_hash`), *before*
    //    creating the admin account. This closes the redemption race the old
    //    validate → create-admin → best-effort-delete sequence had: two
    //    concurrent requests presenting the same raw token could both pass
    //    the (separate) `is_valid` read and both create an admin user before
    //    either got around to deleting the row. Because the read and the
    //    delete are now the same SQL statement, the database serializes the
    //    two attempts — only one caller can ever observe `true` here, so
    //    only one can proceed past this point.
    match bootstrap_tokens::take_valid_by_hash(ctx, &token_hash).await {
        Ok(true) => {}
        Ok(false) => return err_unauthorized("invalid or expired bootstrap token"),
        Err(e) => return err_internal("bootstrap_tokens lookup", e),
    }

    // 2. Create the admin user via the same code path bootstrap-on-init uses.
    //    Reusing this keeps the legacy companion columns (`name`, `disabled`,
    //    `deleted_at`) and the local_credentials row consistent with the
    //    env-var path. The token is already consumed (step 1), so a failure
    //    here just means the caller needs a fresh token from an operator —
    //    it can't reopen the single-use race.
    if let Err(e) = bootstrap::bootstrap_with_email_password(ctx, &email, &password).await {
        return err_internal("create admin", e);
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

    /// The `Message` the real GET page (`pages::bootstrap::handle_get`)
    /// would build for this POST's matching request — anonymous, no session
    /// yet (that's the whole point of bootstrap redemption). Used both to
    /// call `handle()` (which reads `msg.user_id()` via `crate::csrf::verify`)
    /// and to compute the matching token via [`csrf_field`].
    fn bootstrap_msg() -> Message {
        crate::test_support::anon_msg("create", "/b/auth/api/bootstrap")
    }

    /// The `csrf_token=...` form fragment the GET page would have embedded
    /// via `crate::csrf::hidden_field` for `msg`, computed the same way the
    /// handler validates it.
    fn csrf_field(ctx: &TestContext, msg: &Message) -> String {
        format!("csrf_token={}", crate::csrf::token(ctx, msg))
    }

    #[tokio::test]
    async fn redeems_valid_token_creates_admin_and_consumes_row() {
        let ctx = ctx_with_crypto().await;
        let msg = bootstrap_msg();

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
        let form = format!(
            "token={raw}&email=admin@example.com&password=test1234&{}",
            csrf_field(&ctx, &msg)
        );
        let input = InputStream::from_bytes(form.into_bytes());
        let _ = handle(&ctx, &msg, input).await;

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
    async fn missing_csrf_token_is_rejected() {
        let ctx = ctx_with_crypto().await;
        let msg = bootstrap_msg();

        let raw = "no-csrf-token";
        let hash = hash_token(raw);
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        bootstrap_tokens::insert(&ctx, hash.clone(), &expires)
            .await
            .unwrap();

        // No `csrf_token` field at all.
        let form = format!("token={raw}&email=admin@example.com&password=test1234");
        let out = handle(&ctx, &msg, InputStream::from_bytes(form.into_bytes())).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "a form POST with no csrf_token must be rejected"
        );
        // The token row must NOT have been consumed — rejection happens
        // before the bootstrap-token lookup.
        assert!(bootstrap_tokens::is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn wrong_csrf_token_is_rejected() {
        let ctx = ctx_with_crypto().await;
        let msg = bootstrap_msg();

        let raw = "wrong-csrf-token";
        let hash = hash_token(raw);
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        bootstrap_tokens::insert(&ctx, hash.clone(), &expires)
            .await
            .unwrap();

        let form =
            format!("token={raw}&email=admin@example.com&password=test1234&csrf_token=bogus");
        let out = handle(&ctx, &msg, InputStream::from_bytes(form.into_bytes())).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "a form POST with a wrong csrf_token must be rejected"
        );
        assert!(bootstrap_tokens::is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn second_redemption_of_same_token_fails_and_does_not_double_create() {
        // Reproduces the single-use race directly (not just the repo-level
        // primitive): the same raw token, redeemed a second time, must be
        // rejected rather than creating a second admin — proving
        // `take_valid_by_hash`'s atomic consume is actually wired into the
        // handler in the "consume before create" order.
        let ctx = ctx_with_crypto().await;
        let msg = bootstrap_msg();

        let raw = "single-use-token";
        let hash = hash_token(raw);
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        bootstrap_tokens::insert(&ctx, hash.clone(), &expires)
            .await
            .unwrap();

        let form = format!(
            "token={raw}&email=admin@example.com&password=test1234&{}",
            csrf_field(&ctx, &msg)
        );

        let first = handle(
            &ctx,
            &msg,
            InputStream::from_bytes(form.clone().into_bytes()),
        )
        .await;
        assert_eq!(
            crate::test_support::output_status(first).await,
            302,
            "first redemption of a fresh token must succeed"
        );

        // Same raw token again — the row is already gone.
        let second = handle(&ctx, &msg, InputStream::from_bytes(form.into_bytes())).await;
        assert!(
            crate::test_support::output_is_error(second, "Unauthenticated").await,
            "a second redemption of an already-consumed token must be rejected"
        );

        // Only one admin user exists — the second attempt never reached
        // `bootstrap_with_email_password`.
        let count =
            wafer_core::clients::database::count(&ctx, crate::blocks::auth::USERS_TABLE, &[])
                .await
                .unwrap();
        assert_eq!(
            count, 1,
            "the second redemption must not create a second user"
        );
    }

    #[tokio::test]
    async fn concurrent_redemption_of_same_token_creates_at_most_one_admin() {
        // Reproduces the actual race from the finding: two concurrent
        // requests presenting the SAME raw token but DIFFERENT emails. The
        // SQLite backend's write path is a genuine async round trip through
        // a dedicated worker thread (see `wafer-block-sqlite`'s
        // `ConnWorker`), so `tokio::join!`ing two `handle()` calls really
        // does interleave their DB round trips rather than running them
        // back-to-back. A non-atomic validate → create → delete sequence
        // can let both requests observe the token as valid before either
        // deletes it, minting two admin accounts from one single-use token.
        let ctx = ctx_with_crypto().await;
        let msg = bootstrap_msg();

        let raw = "concurrent-token";
        let hash = hash_token(raw);
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        bootstrap_tokens::insert(&ctx, hash.clone(), &expires)
            .await
            .unwrap();

        let csrf = csrf_field(&ctx, &msg);
        let form_a = format!("token={raw}&email=admin-a@example.com&password=test1234&{csrf}");
        let form_b = format!("token={raw}&email=admin-b@example.com&password=test1234&{csrf}");

        let (out_a, out_b) = tokio::join!(
            handle(&ctx, &msg, InputStream::from_bytes(form_a.into_bytes())),
            handle(&ctx, &msg, InputStream::from_bytes(form_b.into_bytes())),
        );
        // One of the two concurrent redemptions is expected to come back as
        // an error terminal (the loser observes the token already
        // consumed), so this can't use `output_status`/`collect_or_panic` —
        // those panic on an error-shaped `OutputStream`. Collect each
        // outcome as either a status code (success) or an error code
        // (rejection) without asserting per-branch which one it is; the
        // real assertion is the aggregate "exactly one success" below.
        async fn outcome(out: OutputStream) -> Result<u16, String> {
            match out.collect_buffered().await {
                Ok(buf) => Ok(buf
                    .meta
                    .iter()
                    .find(|m| m.key == "resp.status")
                    .and_then(|m| m.value.parse::<u16>().ok())
                    .unwrap_or(200)),
                Err(wafer_run::streams::output::TerminalNotResponse::Error(e)) => {
                    Err(format!("{:?}", e.code))
                }
                Err(other) => panic!("unexpected terminal: {other:?}"),
            }
        }
        let outcome_a = outcome(out_a).await;
        let outcome_b = outcome(out_b).await;

        let successes = [&outcome_a, &outcome_b]
            .iter()
            .filter(|o| matches!(o, Ok(302)))
            .count();
        assert_eq!(
            successes, 1,
            "exactly one concurrent redemption of the same token may succeed \
             (got {outcome_a:?} and {outcome_b:?})"
        );

        let count =
            wafer_core::clients::database::count(&ctx, crate::blocks::auth::USERS_TABLE, &[])
                .await
                .unwrap();
        assert_eq!(
            count, 1,
            "a single bootstrap token must create at most one admin account"
        );
    }

    #[tokio::test]
    async fn rejects_invalid_token() {
        let ctx = ctx_with_crypto().await;
        let msg = bootstrap_msg();
        let form = format!(
            "token=wrong&email=admin@example.com&password=test1234&{}",
            csrf_field(&ctx, &msg)
        );
        let input = InputStream::from_bytes(form.into_bytes());
        let _ = handle(&ctx, &msg, input).await;
        // No admin user was created.
        assert!(users::find_by_email(&ctx, "admin@example.com")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn rejects_short_password() {
        let ctx = ctx_with_crypto().await;
        let msg = bootstrap_msg();
        // Even with a valid token row, the handler must reject password <8 chars.
        let raw = "another-token";
        let hash = hash_token(raw);
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        bootstrap_tokens::insert(&ctx, hash.clone(), &expires)
            .await
            .unwrap();

        let form = format!(
            "token={raw}&email=admin@example.com&password=short&{}",
            csrf_field(&ctx, &msg)
        );
        let input = InputStream::from_bytes(form.into_bytes());
        let _ = handle(&ctx, &msg, input).await;

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
        let msg = bootstrap_msg();
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

        let form = format!(
            "token={raw}&email=admin@example.com&password=admin123&{}",
            csrf_field(&ctx, &msg)
        );
        let input = InputStream::from_bytes(form.into_bytes());
        let _ = handle(&ctx, &msg, input).await;

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
        let msg = bootstrap_msg();
        let raw = "redirect-token";
        let hash = hash_token(raw);
        let expires = (chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        bootstrap_tokens::insert(&ctx, hash, &expires)
            .await
            .unwrap();

        let form = format!(
            "token={raw}&email=admin@example.com&password=test1234&{}",
            csrf_field(&ctx, &msg)
        );
        let buf = handle(&ctx, &msg, InputStream::from_bytes(form.into_bytes()))
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
