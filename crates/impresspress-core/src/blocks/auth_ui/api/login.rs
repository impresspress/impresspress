//! POST /b/auth/api/login — relocated from auth/login.rs in Task 5.

use wafer_run::{context::Context, InputStream, OutputStream};

use crate::{
    blocks::{
        auth::{
            burn_timing_equalization, check_password,
            helpers::{
                issue_tokens_and_cookie, touch_last_login_after_response, RoleSource, Rotation,
                SessionLifetime, TokenGrant,
            },
            repo::{local_credentials, users},
            PasswordCheck,
        },
        auth_ui::{
            contracts::{AuthenticatedUser, LoginRequest, LoginResponse, TokenType},
            redirect::{configured_admin_default, default_post_login_redirect},
        },
        crud,
        errors::{error_response, ErrorCode},
    },
    http::{err_bad_request, ResponseBuilder},
};

pub async fn handle(ctx: &dyn Context, input: InputStream) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: LoginRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    let email_lower = body.email.trim().to_lowercase();

    // Find user by email via typed repo. `users::find_by_email` already
    // collapses NOT_FOUND to `Ok(None)`; any `Err` here is a real failure
    // (WRAP denial, DB outage) that must not collapse to "invalid
    // credentials" — that would mask outages and silently log users out.
    let user_row = match users::find_by_email(ctx, &email_lower).await {
        Ok(opt) => opt,
        Err(e) => return crud::db_error_internal(e, "User lookup failed"),
    };

    // The real stored credential, if this login has one at all. A user with
    // no `local_credentials` row (an OAuth-only account) is `None` and takes
    // the equalization arm below.
    //
    // DELIBERATE, the same rule `forgot_password` and `verify::handle_resend`
    // follow: only a registered address reaches this read, so a failure here
    // answered with its own status (403, 500) would tell an anonymous caller
    // which emails have accounts whenever the credentials table alone is
    // refused or failing. It is logged with its code and the user id, and the
    // login continues down the equalization arm, answering — in the same
    // time — exactly what an unknown email gets. The users read above, which
    // every login makes, keeps its honest 403/500, so an outage of the
    // database as a whole is still visible to the caller.
    let stored_hash_owned: String;
    let stored_hash: Option<&str> = match &user_row {
        Some(u) => match local_credentials::find_by_user_id(ctx, &u.id).await {
            Ok(Some(cred)) => {
                stored_hash_owned = cred.password_hash;
                Some(&stored_hash_owned)
            }
            Ok(None) => None,
            Err(e) => {
                tracing::error!(
                    user_id = %u.id,
                    code = ?e.code,
                    error = %e,
                    "login: credential lookup failed; answered as invalid credentials"
                );
                None
            }
        },
        None => None,
    };

    // With no credential to verify, burn one verification against a hash in
    // the scheme this platform writes, so "no such user" and "wrong password"
    // cost the same and the response time is not a user-enumeration oracle.
    // See `auth::timing_equalization_hash` for why that hash is derived rather
    // than a constant, and `auth::burn_timing_equalization` for why its
    // outcome can never be a login.
    //
    // A stored hash the crypto service cannot check (`Unverifiable`, already
    // logged with the user id by `check_password`) is answered exactly like a
    // wrong password, and pays for the same verification: the crypto service
    // rejects such a hash without doing the work, and a faster or different
    // answer would tell the caller this email has an account. A comparison
    // that could not run at all (WRAP refusal, crypto service down) is the
    // classified 403/429/503 on both arms, known email or not.
    let password_ok = match (&user_row, stored_hash) {
        (Some(user), Some(hash)) => {
            match check_password(ctx, &user.id, &body.password, hash).await {
                Ok(PasswordCheck::Matches) => true,
                Ok(PasswordCheck::DoesNotMatch) => false,
                Ok(PasswordCheck::Unverifiable(_)) => {
                    if let Err(e) = burn_timing_equalization(ctx, &body.password).await {
                        return OutputStream::error(e);
                    }
                    false
                }
                Err(e) => return OutputStream::error(e),
            }
        }
        _ => {
            if let Err(e) = burn_timing_equalization(ctx, &body.password).await {
                return OutputStream::error(e);
            }
            false
        }
    };

    // Use the typed UserRow we already have. `disabled` and `email_verified`
    // ride on the row, so no second `db::get` is needed.
    let user = match user_row {
        Some(u) if password_ok => u,
        _ => return error_response(ErrorCode::InvalidCredentials, "Invalid email or password"),
    };

    // [SEC-034] Disabled accounts return the SAME generic invalid-credentials
    // response as a wrong-password attempt. Surfacing "account is disabled"
    // confirms to an attacker that the email exists, gives them a target for
    // a re-enable social-engineering attack, and signals when an admin has
    // taken action on a compromised account.
    if !user.is_active() {
        return error_response(ErrorCode::InvalidCredentials, "Invalid email or password");
    }

    // Check email verification if required
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
    if require_verification && !user.email_verified {
        return error_response(ErrorCode::EmailNotVerified, "Please verify your email before logging in. Check your inbox for the verification link.");
    }

    // The token's roles, granting admin idempotently when ADMIN_EMAIL
    // matches, and the `auth_version` of the row read above — the version
    // the checks above passed at. A WRAP denial or DB error here must not
    // silently resolve to "no roles" — that would 403 an admin or
    // double-grant on the next login (SB-3).
    let grant = match TokenGrant::resolve(
        ctx,
        &user,
        RoleSource::CheckBootstrapAdmin {
            email: &email_lower,
        },
    )
    .await
    {
        Ok(grant) => grant,
        Err(e) => return crud::db_error_internal(e, "Failed to resolve user roles"),
    };

    // Mint tokens, persist the refresh + session rows, build the cookie.
    let lifetime = match SessionLifetime::resolve_or_error(ctx).await {
        Ok(lifetime) => lifetime,
        Err(r) => return r,
    };
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

    // Update last login, after the response. Best-effort: the sign-in has
    // already succeeded and the tokens are already minted, so a failed
    // bookkeeping write is logged, not returned.
    touch_last_login_after_response(ctx, &user.id);

    // Role-aware post-login default (#1 onboarding bug fix). The login PAGE
    // is rendered before credentials are known, so it cannot pick between
    // the admin and user-portal destinations itself; this JSON response is
    // where the caller's role first becomes known, so it's where the
    // single-sourced default (`redirect::default_post_login_redirect`) gets
    // applied. The client only falls back to this when it has no explicit,
    // already-validated `next`/`redirect` param of its own.
    let admin_default = match configured_admin_default(ctx).await {
        Ok(admin_default) => admin_default,
        Err(e) => return crud::db_error_internal(e, "Could not read the post-login redirect"),
    };
    let roles = grant.into_roles();
    let is_admin = roles.iter().any(|r| r == "admin");
    let default_redirect = default_post_login_redirect(is_admin, &admin_default);

    ResponseBuilder::new()
        .set_cookie(&issued.cookie)
        .json(&LoginResponse {
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

/// Regression tests for the #1 onboarding bug: every successful login used
/// to compute `default_redirect` as a fixed `/b/admin/` regardless of the
/// caller's role, so a brand-new non-admin's login script sent them straight
/// into an admin-only route and hit a 403 dead-end. `default_redirect` is
/// now role-aware (`redirect::default_post_login_redirect`) — these tests
/// drive the real [`handle`] end-to-end through a seeded user (via the real
/// signup handler, so the password hash + role assignment are the same code
/// path production uses).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::{
            auth::{config::BOOTSTRAP_ADMIN_EMAIL_KEY, TIMING_EQUALIZATION_PASSWORD},
            auth_ui::api::signup,
        },
        config_vars::POST_LOGIN_REDIRECT_KEY,
        test_support::{collect_or_panic, output_http_status, output_json, TestContext},
    };

    /// A context with a real crypto block — login verifies passwords via
    /// `crypto::compare_hash` and mints tokens via `crypto::sign`/
    /// `random_bytes`. Without one the handler trips on
    /// `block 'wafer-run/crypto' not registered`.
    async fn ctx_with_crypto() -> TestContext {
        TestContext::with_auth_and_crypto().await
    }

    /// Sign a new user up through the real signup handler (REQUIRE_VERIFICATION
    /// is unset/false by default, so this also auto-logs them in — irrelevant
    /// here, we only need the user + local_credentials rows it creates).
    async fn signup_user(ctx: &TestContext, email: &str, password: &str) {
        let body = serde_json::json!({"email": email, "password": password}).to_string();
        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        let out = signup::handle(
            &limiter,
            ctx,
            &msg,
            InputStream::from_bytes(body.into_bytes()),
        )
        .await;
        collect_or_panic(out).await;
    }

    async fn login(ctx: &TestContext, email: &str, password: &str) -> serde_json::Value {
        let body = serde_json::json!({"email": email, "password": password}).to_string();
        let out = handle(ctx, InputStream::from_bytes(body.into_bytes())).await;
        output_json(out).await
    }

    #[tokio::test]
    async fn non_admin_login_defaults_to_userportal_not_admin() {
        let ctx = ctx_with_crypto().await;
        signup_user(&ctx, "regular@example.com", "correct-horse-battery").await;

        let resp = login(&ctx, "regular@example.com", "correct-horse-battery").await;

        assert_eq!(
            resp["user"]["roles"],
            serde_json::json!(["user"]),
            "fixture user must not be admin: {resp}"
        );
        assert_eq!(
            resp["default_redirect"], "/b/userportal/",
            "non-admin login must default to the user portal, not the admin-only \
             route (#1 onboarding bug): {resp}"
        );
    }

    #[tokio::test]
    async fn admin_login_still_defaults_to_configured_admin_default() {
        let mut ctx = ctx_with_crypto().await;
        // Matches the signup-time `initial_role_for` rule, so the seeded user
        // is created with role "admin" directly.
        ctx.set_config(BOOTSTRAP_ADMIN_EMAIL_KEY, "admin@example.com");
        signup_user(&ctx, "admin@example.com", "correct-horse-battery").await;

        let resp = login(&ctx, "admin@example.com", "correct-horse-battery").await;

        assert_eq!(
            resp["user"]["roles"],
            serde_json::json!(["admin"]),
            "fixture user must be admin: {resp}"
        );
        assert_eq!(
            resp["default_redirect"], "/b/admin/",
            "admin login must keep defaulting to the admin home: {resp}"
        );
    }

    #[tokio::test]
    async fn admin_login_honors_custom_configured_admin_default() {
        let mut ctx = ctx_with_crypto().await;
        ctx.set_config(BOOTSTRAP_ADMIN_EMAIL_KEY, "admin@example.com");
        ctx.set_config(POST_LOGIN_REDIRECT_KEY, "/b/admin/reports");
        signup_user(&ctx, "admin@example.com", "correct-horse-battery").await;

        let resp = login(&ctx, "admin@example.com", "correct-horse-battery").await;

        assert_eq!(resp["default_redirect"], "/b/admin/reports");
    }

    /// The equalization password is a public constant in this repository, and
    /// a user row whose `local_credentials` row is missing (an interrupted
    /// signup, a half-restored backup) takes the equalization arm. If that
    /// comparison's result were treated as a login, anyone could sign in as
    /// such an account by typing the constant.
    #[tokio::test]
    async fn the_timing_equalization_password_cannot_log_anyone_in() {
        use crate::blocks::auth::repo::users;

        let ctx = ctx_with_crypto().await;
        users::insert(
            &ctx,
            users::NewUser {
                email: "credentialless@example.com".into(),
                display_name: "No Credentials".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: true,
                verification_token_hash: None,
            },
        )
        .await
        .expect("seed a user with no local-credentials row");

        let body = serde_json::json!({
            "email": "credentialless@example.com",
            "password": TIMING_EQUALIZATION_PASSWORD,
        })
        .to_string();
        let out = handle(&ctx, InputStream::from_bytes(body.into_bytes())).await;

        match out.collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(err)) => assert_eq!(
                err.detail_code(),
                Some("invalid_credentials"),
                "expected the generic invalid-credentials answer, got {err:?}"
            ),
            other => panic!(
                "the timing-equalization password signed a credential-less user in: {other:?}"
            ),
        }
    }

    /// What a stored credential looks like when the crypto service cannot
    /// check it: no scheme it knows (`CryptoError::MalformedHash`).
    const MALFORMED_HASH: &str = "not-a-password-hash";

    /// Sign `email` up through the real signup handler, then overwrite its
    /// stored hash with [`MALFORMED_HASH`] through the real repo write.
    /// Returns the user id.
    async fn user_with_malformed_hash(ctx: &TestContext, email: &str) -> String {
        use crate::blocks::auth::repo::{local_credentials, users};

        signup_user(ctx, email, "correct-horse-battery").await;
        let user = users::find_by_email(ctx, email)
            .await
            .expect("user read")
            .expect("signup created the user");
        local_credentials::update_password(ctx, &user.id, MALFORMED_HASH)
            .await
            .expect("overwrite the stored hash");
        user.id
    }

    fn credentials(email: &str, password: &str) -> InputStream {
        InputStream::from_bytes(
            serde_json::json!({"email": email, "password": password})
                .to_string()
                .into_bytes(),
        )
    }

    async fn detail_code(out: OutputStream) -> Option<String> {
        match out.collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(err)) => {
                err.detail_code().map(str::to_string)
            }
            other => panic!("expected an error terminal, got {other:?}"),
        }
    }

    /// A stored hash the crypto service cannot check is answered exactly as a
    /// wrong password — a different answer would tell the caller the email
    /// has an account — and logged at error level with the user id, which is
    /// the only way an operator learns the account cannot sign in until its
    /// password is reset. The hash itself stays out of the log.
    #[tokio::test]
    async fn a_malformed_stored_hash_is_invalid_credentials_and_logged_with_the_user_id() {
        let ctx = ctx_with_crypto().await;
        let user_id = user_with_malformed_hash(&ctx, "broken@example.com").await;

        let captured = crate::test_support::CapturedEvents::install();
        let out = handle(
            &ctx,
            credentials("broken@example.com", "correct-horse-battery"),
        )
        .await;
        let code = detail_code(out).await;
        let events = captured.events();
        drop(captured);

        assert_eq!(code.as_deref(), Some("invalid_credentials"));
        let logged = events
            .iter()
            .find(|e| {
                e.level == tracing::Level::ERROR
                    && e.fields.get("user_id").map(String::as_str) == Some(user_id.as_str())
            })
            .unwrap_or_else(|| {
                panic!("no error-level event names user {user_id}; events: {events:?}")
            });
        assert!(
            logged.fields.values().all(|v| !v.contains(MALFORMED_HASH)),
            "the stored hash must not reach the log: {logged:?}"
        );
    }

    /// The crypto service rejects a hash it cannot check without doing the
    /// work a real verification does, so a login against one would answer
    /// measurably faster than a wrong password — the same enumeration oracle
    /// `auth::timing_equalization_hash` exists to close for unknown emails.
    /// The login burns one verification against the equalization hash to
    /// cost what a wrong password costs.
    #[tokio::test]
    async fn a_malformed_stored_hash_costs_a_full_verification() {
        use std::sync::{Arc, Mutex};

        use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

        /// The real crypto service, recording every hash `compare_hash` is
        /// asked to check.
        struct Recording {
            inner: wafer_block_crypto::service::Argon2JwtCryptoService,
            compared: Arc<Mutex<Vec<String>>>,
        }
        #[wafer_block::wafer_async_trait]
        impl CryptoService for Recording {
            async fn hash(&self, password: &str) -> Result<String, CryptoError> {
                self.inner.hash(password).await
            }
            async fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError> {
                self.compared.lock().unwrap().push(hash.to_string());
                self.inner.compare_hash(password, hash).await
            }
            async fn sign_for(
                &self,
                block_id: &str,
                claims: std::collections::BTreeMap<String, serde_json::Value>,
                expiry: std::time::Duration,
            ) -> Result<String, CryptoError> {
                self.inner.sign_for(block_id, claims, expiry).await
            }
            async fn verify_for(
                &self,
                block_id: &str,
                token: &str,
            ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, CryptoError>
            {
                self.inner.verify_for(block_id, token).await
            }
            async fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
                self.inner.random_bytes(n).await
            }
        }

        let compared = Arc::new(Mutex::new(Vec::new()));
        let ctx = TestContext::with_auth_and_crypto_service(Arc::new(Recording {
            inner: crate::test_support::real_crypto_service(),
            compared: Arc::clone(&compared),
        }))
        .await;
        user_with_malformed_hash(&ctx, "slow@example.com").await;
        let equalizer = crate::blocks::auth::timing_equalization_hash(&ctx)
            .await
            .expect("the crypto block hashes");
        compared.lock().unwrap().clear();

        let out = handle(
            &ctx,
            credentials("slow@example.com", "correct-horse-battery"),
        )
        .await;
        assert_eq!(
            detail_code(out).await.as_deref(),
            Some("invalid_credentials")
        );

        assert_eq!(
            *compared.lock().unwrap(),
            vec![MALFORMED_HASH.to_string(), equalizer.to_string()],
            "after the stored hash is rejected, one verification must run against the \
             equalization hash"
        );
    }

    /// A comparison the crypto service could not run says nothing about the
    /// password. Answering it "Invalid email or password" tells a user with
    /// the right password that it is wrong, and hides the outage behind what
    /// looks like ordinary failed logins. It is a 503 — and for an unknown
    /// email too, whose equalization comparison fails the same way, so the
    /// outage does not turn the status into an account-existence oracle.
    #[tokio::test]
    async fn a_crypto_outage_is_a_503_for_known_and_unknown_emails() {
        use crate::test_support::FailingServiceOpContext;

        let ctx = ctx_with_crypto().await;
        signup_user(&ctx, "known@example.com", "correct-horse-battery").await;
        // Derived before the outage, as a running deployment would have it.
        crate::blocks::auth::timing_equalization_hash(&ctx)
            .await
            .expect("the crypto block hashes");
        let down = FailingServiceOpContext::failing_with(
            ctx,
            "wafer-run/crypto",
            vec!["crypto.compare_hash"],
            wafer_run::WaferError::new(wafer_run::ErrorCode::Unavailable, "crypto is down"),
        );

        for email in ["known@example.com", "unknown@example.com"] {
            let status = output_http_status(
                handle(&down, credentials(email, "correct-horse-battery")).await,
            )
            .await;
            assert_eq!(
                status, 503,
                "{email}: an unrun comparison is not a wrong password"
            );
        }
    }

    /// A WRAP refusal of the comparison is the classified 403, not a wrong
    /// password: the deployment is missing a grant, and the operator needs to
    /// see that rather than a stream of failed logins.
    #[tokio::test]
    async fn a_refused_comparison_is_a_403() {
        use crate::test_support::FailingServiceOpContext;

        let ctx = ctx_with_crypto().await;
        signup_user(&ctx, "denied@example.com", "correct-horse-battery").await;
        let refused = FailingServiceOpContext::failing_with(
            ctx,
            "wafer-run/crypto",
            vec!["crypto.compare_hash"],
            wafer_run::WaferError::new(
                wafer_run::ErrorCode::PermissionDenied,
                "no grant for crypto.compare_hash",
            ),
        );

        let status = output_http_status(
            handle(
                &refused,
                credentials("denied@example.com", "correct-horse-battery"),
            )
            .await,
        )
        .await;
        assert_eq!(status, 403);
    }

    /// The control: a wrong password is still the plain invalid-credentials
    /// answer.
    #[tokio::test]
    async fn a_wrong_password_is_invalid_credentials() {
        let ctx = ctx_with_crypto().await;
        signup_user(&ctx, "typo@example.com", "correct-horse-battery").await;

        let out = handle(&ctx, credentials("typo@example.com", "wrong-horse-battery")).await;
        assert_eq!(
            detail_code(out).await.as_deref(),
            Some("invalid_credentials")
        );
    }

    /// An `Internal` from the crypto service that is not a malformed hash is
    /// its own fault while checking — a failed offload, say — and says
    /// nothing about the stored credential: it is the classified 503, and the
    /// log does not send an operator off to reset a password that is fine.
    #[tokio::test]
    async fn a_transient_crypto_fault_is_a_503_not_a_reset() {
        use crate::test_support::{CapturedEvents, FailingServiceOpContext};

        let ctx = ctx_with_crypto().await;
        signup_user(&ctx, "flaky@example.com", "correct-horse-battery").await;
        let faulty = FailingServiceOpContext::failing_with(
            ctx,
            "wafer-run/crypto",
            vec!["crypto.compare_hash"],
            wafer_run::WaferError::new(
                wafer_run::ErrorCode::Internal,
                "crypto blocking task failed: task panicked",
            ),
        );

        let captured = CapturedEvents::install();
        let status = output_http_status(
            handle(
                &faulty,
                credentials("flaky@example.com", "correct-horse-battery"),
            )
            .await,
        )
        .await;
        let events = captured.events();
        drop(captured);

        assert_eq!(status, 503);
        assert!(
            events.iter().all(|e| e
                .fields
                .get("message")
                .is_none_or(|m| !m.contains("password reset"))),
            "a transient fault must not be logged as an account needing a reset: {events:?}"
        );
    }

    /// Sign-in keeps its 503 when the password hasher cannot be reached,
    /// driven through the real crypto block's `CryptoError::Unavailable`
    /// mapping: never "invalid credentials".
    #[tokio::test]
    async fn an_unreachable_password_hasher_is_a_503() {
        use crate::test_support::HasherFault;

        let (ctx, hasher) = TestContext::with_auth_and_faulty_hasher().await;
        signup_user(&ctx, "kim@example.com", "correct-horse-battery").await;
        hasher.fail_compare(Some(HasherFault::Unreachable));

        let body =
            serde_json::json!({"email": "kim@example.com", "password": "correct-horse-battery"})
                .to_string();
        let out = handle(&ctx, InputStream::from_bytes(body.into_bytes())).await;

        assert_eq!(output_http_status(out).await, 503);
    }

    /// A role removal that commits while a sign-in is resolving its token —
    /// after the grants read has seen the role, before the token is signed —
    /// must not leave a token that carries the removed role and
    /// authenticates.
    ///
    /// The removal bumps `auth_version` after it lands, as every role change
    /// does. So the version the token carries has to be one read BEFORE the
    /// grants: the bump then puts the token behind, and its first use is
    /// refused. Read after the grants, it is the bumped version, and the
    /// token authenticates with the removed role until it expires.
    #[tokio::test]
    async fn a_role_removed_during_sign_in_leaves_a_token_that_is_refused() {
        use crate::{
            blocks::auth::bump_auth_version,
            platform_state::user_roles,
            test_support::{anon_msg, AfterDbOpContext},
        };

        const EMAIL: &str = "demoted@example.com";
        const PASSWORD: &str = "correct-horse-battery";
        let ctx = TestContext::with_auth()
            .await
            .with_admin_added()
            .await
            .with_sign_in_added();
        let uid = ctx.seed_account(EMAIL, PASSWORD, "user").await;
        user_roles::assign(&ctx.fixture(), &uid, "editor", "")
            .await
            .expect("grant editor");

        // The admin's removal, landing the moment the sign-in's grants read
        // has answered: the grant goes, then the version is bumped.
        let (fixture, holder) = (ctx.fixture(), uid.clone());
        let racing = AfterDbOpContext::new(
            ctx.clone()
                .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID),
            "database.list",
            user_roles::TABLE,
            async move {
                let grants = user_roles::list_for_user(&fixture, &holder)
                    .await
                    .expect("read the grants");
                let editor = grants
                    .iter()
                    .find(|g| g.role == "editor")
                    .expect("the editor grant is still there");
                user_roles::remove(&fixture, &editor.id)
                    .await
                    .expect("remove the grant");
                bump_auth_version(&fixture, &holder)
                    .await
                    .expect("bump auth_version");
            },
        );

        let body = serde_json::json!({"email": EMAIL, "password": PASSWORD}).to_string();
        let resp =
            output_json(handle(&racing, InputStream::from_bytes(body.into_bytes())).await).await;
        assert!(
            racing.fired(),
            "the removal must land inside the sign-in: {resp}"
        );
        assert!(
            resp["user"]["roles"]
                .as_array()
                .is_some_and(|roles| roles.iter().any(|r| r == "editor")),
            "the sign-in read the grants before the removal, so its token carries \
             the removed role: {resp}"
        );
        let token = resp["access_token"]
            .as_str()
            .expect("the sign-in succeeded")
            .to_string();

        // First use, through the router's verification like any request.
        let mut me = anon_msg("retrieve", "/b/auth/api/me");
        me.set_meta("http.header.authorization", format!("Bearer {token}"));
        assert_eq!(
            output_http_status(ctx.request(me).await).await,
            401,
            "a token carrying a role removed during its mint must be refused"
        );

        // The control: a sign-in after the removal authenticates on the same
        // route, so the refusal above is the stale version and nothing else.
        let fresh = ctx.sign_in(EMAIL, PASSWORD).await;
        let me = fresh.bearer(anon_msg("retrieve", "/b/auth/api/me"));
        assert_eq!(output_http_status(ctx.request(me).await).await, 200);
    }
}
