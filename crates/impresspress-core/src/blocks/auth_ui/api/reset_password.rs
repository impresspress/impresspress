//! POST /b/auth/api/reset-password — relocated from auth/login.rs in Task 5.

use wafer_core::clients::crypto;
use wafer_run::{context::Context, InputStream, OutputStream};

use crate::{
    blocks::{
        auth::repo::{local_credentials, tokens, users},
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
    tokens::revoke_all_for_user(ctx, &user.id).await.ok();

    ok_json(&serde_json::json!({"message": "Password reset successfully"}))
}
