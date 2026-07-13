//! POST /b/auth/api/forgot-password — relocated from auth/login.rs in Task 5.

use wafer_core::clients::crypto;
use wafer_run::{context::Context, InputStream, OutputStream};

use crate::{
    blocks::auth::repo::users,
    http::{err_bad_request, err_internal, ok_json},
    util::{hex_encode, sha256_hex},
};

pub async fn handle(ctx: &dyn Context, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        email: String,
    }
    let raw = input.collect_to_bytes().await;
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    let email_lower = body.email.trim().to_lowercase();
    let safe_msg = "If that email is registered, a password reset link has been sent.";

    let Ok(Some(user)) = users::find_by_email(ctx, &email_lower).await else {
        return ok_json(&serde_json::json!({"message": safe_msg}));
    };

    // Generate reset token (expires in 1 hour). The raw token goes in the
    // email link; only its SHA-256 hex digest is persisted, so a leak of
    // the row (admin SQL explorer, backup, log dump, any block with read
    // grant on the users table) does not become a password-reset oracle.
    let reset_token = match crypto::random_bytes(ctx, 32).await {
        Ok(bytes) => hex_encode(&bytes),
        Err(e) => return err_internal("Token generation failed", e),
    };
    let reset_token_hash = sha256_hex(reset_token.as_bytes());

    let expires = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    if let Err(e) = users::set_reset_token(ctx, &user.id, &reset_token_hash, &expires).await {
        return err_internal("Failed to store reset token", e.to_string());
    }

    // Send the raw token in the email; the hash lives only in the DB.
    super::send_template_email(ctx, "password_reset", &email_lower, &reset_token).await;

    ok_json(&serde_json::json!({"message": safe_msg}))
}
