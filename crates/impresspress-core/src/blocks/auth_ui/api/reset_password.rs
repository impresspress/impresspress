//! POST /b/auth/api/reset-password — relocated from auth/login.rs in Task 5.

use wafer_core::clients::crypto;
use wafer_run::{context::Context, InputStream, OutputStream};

use crate::{
    blocks::{
        auth::{
            bump_auth_version,
            repo::{local_credentials, tokens, users},
        },
        errors::{error_response, ErrorCode},
    },
    http::{err_bad_request, err_internal, ok_json},
    util::sha256_hex,
};

pub async fn handle(ctx: &dyn Context, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        token: String,
        new_password: String,
    }
    let raw = input.collect_to_bytes().await;
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    if let Err((code, msg)) =
        super::password_policy::validate_new_password(ctx, &body.new_password).await
    {
        return error_response(code, &msg);
    }

    // Find user by reset token. The DB column stores `sha256_hex(raw)`;
    // hash the supplied token the same way before comparing.
    let Ok(Some(user)) = users::find_by_reset_token(ctx, &sha256_hex(body.token.as_bytes())).await
    else {
        return error_response(ErrorCode::InvalidToken, "Invalid or expired reset token");
    };

    // Check expiry — reject if missing or malformed (tokens must have an expiry)
    if user.reset_token_expires.is_empty() {
        return error_response(
            ErrorCode::TokenExpired,
            "Reset token has expired. Please request a new one.",
        );
    }
    match chrono::DateTime::parse_from_rfc3339(&user.reset_token_expires) {
        Ok(exp) => {
            if chrono::Utc::now() > exp.with_timezone(&chrono::Utc) {
                return error_response(
                    ErrorCode::TokenExpired,
                    "Reset token has expired. Please request a new one.",
                );
            }
        }
        Err(_) => {
            return error_response(
                ErrorCode::TokenExpired,
                "Reset token has expired. Please request a new one.",
            );
        }
    }

    // Hash new password
    let new_hash = match crypto::hash(ctx, &body.new_password).await {
        Ok(h) => h,
        Err(e) => return err_internal("Hash failed", e),
    };

    // Update credential row (typed path, no password_hash on users table).
    if let Err(e) = local_credentials::update_password(ctx, &user.id, &new_hash).await {
        return err_internal("Failed to update password", e);
    }

    // Clear reset token on the users row.
    if let Err(e) = users::clear_reset_token(ctx, &user.id).await {
        return err_internal("Failed to clear reset token", e.to_string());
    }

    // Revoke all refresh tokens — invalidate any stolen sessions.
    // SEC-032/039: mark rows revoked (don't delete) so the reuse-detection
    // tombstones survive across the password reset.
    //
    // The credential row has already been updated at this point, so a
    // revocation failure must NOT be reported as success — this is the
    // account-recovery path, so fail-closed here matters even more than in
    // `change_password`. Same treatment: log it and surface a non-success
    // response rather than swallowing it with `.ok()`.
    if let Err(e) = tokens::revoke_all_for_user(ctx, &user.id).await {
        tracing::error!(
            user_id = %user.id,
            error = %e,
            "password reset but refresh-token revocation failed"
        );
        return err_internal("Password reset but session revocation failed", e);
    }

    // P2c: invalidate already-issued access JWTs too — refresh revocation
    // alone doesn't touch a still-live access token, which would otherwise
    // keep authenticating with the old password's blessing until its
    // natural expiry. Same fail-closed treatment: the credential has
    // already changed, so a failed bump must not be reported as success.
    if let Err(e) = bump_auth_version(ctx, &user.id).await {
        tracing::error!(
            user_id = %user.id,
            error = %e,
            "password reset but auth_version bump failed"
        );
        return err_internal("Password reset but session invalidation failed", e);
    }

    ok_json(&serde_json::json!({"message": "Password reset successfully"}))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        blocks::auth_ui::api::signup,
        test_support::{output_is_error, output_json, FailingDbOpContext, TestContext},
    };

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

    /// Sign a user up through the real signup handler and return their id.
    async fn signup_user(ctx: &TestContext, email: &str, password: &str) -> String {
        let body = serde_json::json!({"email": email, "password": password}).to_string();
        let out = signup::handle(ctx, InputStream::from_bytes(body.into_bytes())).await;
        let json = output_json(out).await;
        json["user"]["id"]
            .as_str()
            .expect("signup response carries user.id")
            .to_string()
    }

    /// Issue a raw reset token for `user_id`, persisting only its hash (this
    /// is exactly what `forgot_password::handle` does), and return the raw
    /// token for submission to `reset_password::handle`.
    async fn issue_reset_token(ctx: &TestContext, user_id: &str) -> String {
        let raw = "test-raw-reset-token-0123456789abcdef";
        let expires = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        users::set_reset_token(ctx, user_id, &sha256_hex(raw.as_bytes()), &expires)
            .await
            .unwrap();
        raw.to_string()
    }

    fn body(token: &str, new_password: &str) -> InputStream {
        InputStream::from_bytes(
            serde_json::json!({"token": token, "new_password": new_password})
                .to_string()
                .into_bytes(),
        )
    }

    /// P2c: a successful password reset must bump the user's auth_version so
    /// an access JWT minted before the reset stops authenticating (mirrors
    /// `change_password.rs`'s `successful_password_change_bumps_auth_version`).
    /// This is the account-recovery path — the MORE security-critical of the
    /// two, since a stale-token attacker surviving a reset is exactly the
    /// threat this feature exists to close.
    #[tokio::test]
    async fn successful_password_reset_bumps_auth_version() {
        let ctx = ctx_with_crypto().await;
        let user_id = signup_user(&ctx, "dave@example.com", "original-horse-battery1").await;
        assert_eq!(users::auth_version(&ctx, &user_id).await.unwrap(), 0);

        let token = issue_reset_token(&ctx, &user_id).await;
        let out = handle(&ctx, body(&token, "new-horse-battery-2026")).await;
        let json = output_json(out).await;
        assert_eq!(json["message"], "Password reset successfully");

        assert_eq!(
            users::auth_version(&ctx, &user_id).await.unwrap(),
            1,
            "password reset must bump auth_version"
        );
    }

    /// A revocation failure must not be reported as success — matches
    /// `change_password.rs`'s `revocation_failure_does_not_report_success`.
    /// Proves the `.ok()` swallow at the old `reset_password.rs:83` is gone.
    #[tokio::test]
    async fn revocation_failure_does_not_report_success() {
        let ctx = ctx_with_crypto().await;
        let user_id = signup_user(&ctx, "erin@example.com", "original-horse-battery1").await;
        let token = issue_reset_token(&ctx, &user_id).await;

        // Fail only the refresh-token revocation write; the credential
        // update itself (a different `database.update_where` call, against
        // `local_credentials`) still succeeds.
        let failing = FailingDbOpContext::new(ctx, vec![("database.update_where", tokens::TABLE)]);

        let out = handle(&failing, body(&token, "new-horse-battery-2026")).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a revocation failure must not be reported as success"
        );
    }
}
