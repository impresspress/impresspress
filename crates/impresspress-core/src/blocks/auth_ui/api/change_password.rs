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
            tokens::revoke_all_for_user(ctx, user_id).await.ok();
            ok_json(&serde_json::json!({"message": "Password changed successfully"}))
        }
        Err(e) => err_internal("Update failed", e),
    }
}
