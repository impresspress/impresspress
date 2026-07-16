//! POST /b/auth/api/change-password — relocated from auth/login.rs in Task 5.

use wafer_core::clients::{crypto, database as db};
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::{
        auth::{
            repo::{local_credentials, tokens},
            USERS_TABLE,
        },
        errors::{error_response, ErrorCode},
    },
    http::{err_bad_request, err_internal, err_not_found, ok_json},
};

pub async fn handle(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let user_id = msg.user_id();
    if user_id.is_empty() {
        return error_response(ErrorCode::NotAuthenticated, "Not authenticated");
    }

    #[derive(serde::Deserialize)]
    struct ChangePwReq {
        current_password: String,
        new_password: String,
    }
    let raw = input.collect_to_bytes().await;
    let body: ChangePwReq = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    if let Err((code, msg)) =
        super::password_policy::validate_new_password(ctx, &body.new_password).await
    {
        return error_response(code, &msg);
    }

    // Verify user exists
    match db::get(ctx, USERS_TABLE, user_id).await {
        Ok(_) => {}
        Err(_) => return err_not_found("User not found"),
    };

    // Fetch existing credential row — must have one to change password.
    let cred = match local_credentials::find_by_user_id(ctx, user_id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return error_response(
                ErrorCode::InvalidCredentials,
                "No password set for this account",
            )
        }
        Err(e) => return err_internal("Credential lookup failed", e),
    };

    if crypto::compare_hash(ctx, &body.current_password, &cred.password_hash)
        .await
        .is_err()
    {
        return error_response(
            ErrorCode::InvalidCredentials,
            "Current password is incorrect",
        );
    }

    let new_hash = match crypto::hash(ctx, &body.new_password).await {
        Ok(h) => h,
        Err(e) => return err_internal("Hash failed", e),
    };

    match local_credentials::update_password(ctx, user_id, &new_hash).await {
        Ok(_) => {
            // Revoke all refresh tokens — force re-login with new password.
            // SEC-032/039: mark rows revoked (don't delete) so the
            // reuse-detection tombstones survive.
            //
            // The credential row has already been updated at this point, so
            // a revocation failure must NOT be reported as success: a
            // refresh token obtained before the password change (e.g. by an
            // attacker who had transient access) would otherwise stay valid
            // even though the user was told their account was secured.
            // There is no cross-op transaction primitive available to block
            // code (`wafer_core::clients::database` has no multi-statement
            // transaction client), so this is the best available durable
            // partial-failure signal: log it and surface a non-success
            // response rather than swallowing it with `.ok()`.
            match tokens::revoke_all_for_user(ctx, user_id).await {
                Ok(()) => ok_json(&serde_json::json!({"message": "Password changed successfully"})),
                Err(e) => {
                    tracing::error!(
                        user_id = %user_id,
                        error = %e,
                        "password changed but refresh-token revocation failed"
                    );
                    err_internal("Password changed but session revocation failed", e)
                }
            }
        }
        Err(e) => err_internal("Update failed", e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        blocks::auth_ui::api::signup,
        test_support::{
            auth_msg, collect_or_panic, output_is_error, output_json, FailingDbOpContext,
            TestContext,
        },
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

    fn body(current: &str, new: &str) -> InputStream {
        InputStream::from_bytes(
            serde_json::json!({"current_password": current, "new_password": new})
                .to_string()
                .into_bytes(),
        )
    }

    #[tokio::test]
    async fn revocation_failure_does_not_report_success() {
        let ctx = ctx_with_crypto().await;
        let user_id = signup_user(&ctx, "alice@example.com", "original-horse-battery1").await;

        // Fail only the refresh-token revocation write
        // (`tokens::revoke_all_for_user` issues a `database.update_where`
        // against the tokens table); the password credential update itself
        // — a *different* `database.update_where` call, against
        // `local_credentials` — still succeeds.
        let failing = FailingDbOpContext::new(ctx, vec![("database.update_where", tokens::TABLE)]);

        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);
        let out = handle(
            &failing,
            &msg,
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        assert!(
            output_is_error(out, "Internal").await,
            "a revocation failure must not be reported as success"
        );
    }

    #[tokio::test]
    async fn successful_revocation_still_reports_success() {
        let ctx = ctx_with_crypto().await;
        let user_id = signup_user(&ctx, "bob@example.com", "original-horse-battery1").await;

        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);
        let out = handle(
            &ctx,
            &msg,
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        let buf = collect_or_panic(out).await;
        let json: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
        assert_eq!(json["message"], "Password changed successfully");
    }
}
