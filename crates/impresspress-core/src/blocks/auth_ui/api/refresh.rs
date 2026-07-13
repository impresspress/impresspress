//! POST /b/auth/api/refresh — relocated from auth/login.rs in Task 5.
//!
//! Token-rotation flow for refresh JWTs. Implements the family-rotation
//! reuse-detection pattern (SEC-039):
//!
//! 1. Hash the incoming refresh token, look up the row by `token_hash` (SEC-032).
//! 2. If the row is `revoked = 1`, that token was already rotated away — a
//!    legitimate client would only have the *current* token. Revoke the
//!    entire family.
//! 3. If the row is live, mark it revoked and insert a new row under the
//!    same family ID with `generation + 1`. Return the new access + refresh
//!    pair.

use wafer_core::clients::{config, crypto};
use wafer_run::{context::Context, InputStream, OutputStream};

use crate::{
    blocks::{
        auth::{
            helpers::{ensure_admin_role, expected_issuer, issue_tokens_and_cookie},
            repo::{tokens, users},
        },
        errors::{error_response, ErrorCode},
    },
    http::{err_bad_request, err_internal, ResponseBuilder},
};

pub async fn handle(ctx: &dyn Context, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct RefreshReq {
        refresh_token: String,
    }
    let raw = input.collect_to_bytes().await;
    let body: RefreshReq = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Verify the JWT signature/expiry. A valid signature alone is not enough
    // — the row lookup below is the source of truth for "this token has not
    // been used or revoked yet".
    let Ok(claims) = crypto::verify(ctx, &body.refresh_token).await else {
        return error_response(ErrorCode::InvalidToken, "Invalid or expired refresh token");
    };

    let Some(user_id) = claims
        .get("user_id")
        .or_else(|| claims.get("sub"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return error_response(ErrorCode::InvalidToken, "Invalid refresh token");
    };

    let token_type = claims.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if token_type != "refresh" {
        return error_response(ErrorCode::InvalidToken, "Not a refresh token");
    }

    // [SEC-038] Require the iss claim to match this deployment. A refresh
    // token minted against a different WAFER_RUN_SHARED__FRONTEND_URL value
    // (e.g. a leaked staging secret) must not refresh into a production
    // access token.
    let expected_iss = expected_issuer(ctx).await;
    let iss = claims.get("iss").and_then(|v| v.as_str()).unwrap_or("");
    if iss != expected_iss {
        return error_response(ErrorCode::InvalidToken, "Invalid or expired refresh token");
    }

    // SEC-032: look up the row by SHA-256 hash of the JWT — the raw token
    // is never stored, only its hash.
    let row = match tokens::find_by_token(ctx, &body.refresh_token).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            // Signature was valid but no row exists — token was rotated and
            // its tombstone has since been wiped, or this is a forged
            // refresh token whose family we never minted. Either way, no
            // family to revoke; just refuse.
            return error_response(ErrorCode::InvalidToken, "Refresh token has been revoked");
        }
        Err(e) => {
            tracing::warn!("refresh: token lookup failed: {e}");
            return error_response(ErrorCode::InvalidToken, "Invalid refresh token");
        }
    };

    if row.revoked {
        // SEC-039: a revoked token surfaced. If the family still has a live
        // row, an attacker is replaying a stolen token after legitimate
        // rotation. Burn the whole family.
        if let Ok(true) = tokens::family_has_live_row(ctx, &row.family).await {
            tracing::warn!(
                user_id = %row.user_id,
                family = %row.family,
                "refresh: token reuse detected; revoking entire family"
            );
            let _ = tokens::revoke_family(ctx, &row.family).await;
        }
        return error_response(ErrorCode::InvalidToken, "Refresh token has been revoked");
    }

    // Get user and verify account is still active. Use the typed repo so
    // `disabled` / `email_verified` come off the row instead of a second
    // raw `db::get`.
    let user = match users::find_by_id(ctx, &user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return error_response(ErrorCode::NotAuthenticated, "User not found"),
        Err(_) => return error_response(ErrorCode::NotAuthenticated, "User not found"),
    };

    if !user.is_active() {
        return error_response(ErrorCode::AccountDisabled, "Account is disabled");
    }

    let require_verification =
        config::get_default(ctx, "WAFER_RUN__AUTH__REQUIRE_VERIFICATION", "false").await;
    if (require_verification == "true" || require_verification == "1") && !user.email_verified {
        return error_response(ErrorCode::EmailNotVerified, "Email not verified");
    }

    let email = user.email.clone();
    // A WRAP denial or DB error here must not silently resolve to "no
    // roles" — that would 403 an admin or double-grant on the next login
    // (SB-3).
    let roles = match ensure_admin_role(ctx, &user_id, &email).await {
        Ok(r) => r,
        Err(e) => return err_internal("Failed to resolve user roles", e),
    };

    // Preserve the original auth method across refresh — a token issued
    // via OAuth must remain "oauth.<provider>" forever, not silently
    // upgrade/downgrade. Default "password" handles refresh tokens
    // minted before this claim was added.
    let prior_auth_method = claims
        .get("auth_method")
        .and_then(|v| v.as_str())
        .unwrap_or("password")
        .to_string();

    // Atomic-ish rotation: mark the old row revoked *before* minting the
    // replacement, so we never leave two live rows in a family. If issuance
    // then fails the user simply gets logged out — a recoverable UX outcome.
    if let Err(e) = tokens::revoke_by_id(ctx, &row.id).await {
        tracing::warn!("refresh: failed to revoke prior token row: {e}");
        return error_response(ErrorCode::InvalidToken, "Could not rotate refresh token");
    }

    // Re-issue within the *preserved* family (SEC-039): passing
    // `Some(&row.family)` makes `generate_tokens` carry the existing family on
    // the new refresh JWT so its `family` claim agrees with the DB row that
    // anchors reuse detection. `generation = row.generation + 1` advances the
    // rotation counter. This is the same shared issuance tail every other
    // login flow uses, so the userportal session row is written here too.
    let issued = match issue_tokens_and_cookie(
        ctx,
        &user_id,
        &email,
        &roles,
        &prior_auth_method,
        Some(&row.family),
        row.generation + 1,
    )
    .await
    {
        Ok(i) => i,
        Err(r) => return r,
    };

    ResponseBuilder::new()
        .set_cookie(&issued.cookie)
        .json(&serde_json::json!({
            "access_token": issued.access_token,
            "refresh_token": issued.refresh_token,
            "token_type": "Bearer",
            "expires_in": issued.access_lifetime
        }))
}
