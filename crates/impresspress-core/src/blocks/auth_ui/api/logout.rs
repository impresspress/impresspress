//! POST /b/auth/api/logout — relocated from auth/login.rs in Task 5.

use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::auth::{
        helpers::build_auth_cookie,
        repo::{
            jwt_blocklist::{self, NewBlocklistEntry},
            tokens,
        },
    },
    crypto::{META_AUTH_EXP, META_AUTH_JTI},
    http::{err_internal, ResponseBuilder},
};

pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id();
    if !user_id.is_empty() {
        // SEC-032/039: revoke (don't delete) the user's refresh-token rows
        // so the tombstones remain available for reuse detection. A
        // revocation failure must NOT be reported as a successful logout —
        // the whole point of this call is invalidating refresh tokens on
        // every device; silently discarding the failure here (the old
        // `.ok()`) left revocable-in-name-only sessions live everywhere
        // while telling the caller everything worked.
        if let Err(e) = tokens::revoke_all_for_user(ctx, user_id).await {
            tracing::error!(
                user_id = %user_id,
                error = %e,
                "logout: refresh-token revocation failed"
            );
            return err_internal("Logout could not fully revoke the session", e);
        }

        // SEC-042: the currently-presented access JWT stays structurally
        // valid until its natural exp. Blocklist its `jti` so subsequent
        // requests with the same token are rejected by `extract_auth_meta`.
        //
        // Only the in-flight JWT is blocklisted (per-jti, not per-user) so
        // other live sessions for the same user are unaffected.
        let jti = msg.get_meta(META_AUTH_JTI);
        let exp_str = msg.get_meta(META_AUTH_EXP);
        if !jti.is_empty() {
            // Convert exp (UNIX seconds) to ISO-8601 so we can prune by
            // string comparison consistent with other auth tables. Fall
            // back to "now + access_token_lifetime" if exp is missing or
            // unparseable. A fixed 1-day fallback would evict the row
            // while the JWT was still valid when the configured access
            // lifetime is extended past 24h, silently re-enabling a
            // logged-out token.
            let access_lifetime =
                crate::blocks::auth::helpers::access_token_lifetime_secs(ctx).await;
            let expires_at = exp_str
                .parse::<i64>()
                .ok()
                .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
                .unwrap_or_else(|| {
                    chrono::Utc::now() + chrono::Duration::seconds(access_lifetime as i64)
                });
            let expires_at_iso = expires_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
            // Same fail-closed treatment as the refresh-token revocation
            // above: if the currently-presented JWT can't be blocklisted,
            // it stays valid until its natural exp — logout must not claim
            // success in that case.
            if let Err(e) = jwt_blocklist::insert(
                ctx,
                NewBlocklistEntry {
                    jti,
                    user_id,
                    expires_at: &expires_at_iso,
                },
            )
            .await
            {
                tracing::error!(
                    user_id = %user_id,
                    jti = %jti,
                    error = %e,
                    "logout: jwt blocklist insert failed"
                );
                return err_internal("Logout could not fully revoke the session", e);
            }
        }
    }

    let cookie = build_auth_cookie("", 0, ctx).await;
    ResponseBuilder::new()
        .set_cookie(&cookie)
        .status(303)
        .set_header("Location", "/b/auth/login")
        .json(&serde_json::json!({"message": "Logged out successfully"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        auth_msg, output_is_error, output_status, FailingDbOpContext, TestContext,
    };

    #[tokio::test]
    async fn anonymous_logout_still_succeeds() {
        // No auth.user_id meta at all — nothing to revoke, must still
        // clear the cookie and redirect (existing behavior).
        let ctx = TestContext::with_auth().await;
        let msg = crate::test_support::anon_msg("update", "/b/auth/api/logout");
        let out = handle(&ctx, &msg).await;
        assert_eq!(output_status(out).await, 303);
    }

    #[tokio::test]
    async fn refresh_token_revocation_failure_does_not_report_success() {
        let ctx = TestContext::with_auth().await;
        let failing = FailingDbOpContext::new(ctx, vec![("database.update_where", tokens::TABLE)]);

        let msg = auth_msg("update", "/b/auth/api/logout", "user-1");
        let out = handle(&failing, &msg).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a refresh-token revocation failure must not be reported as a successful logout"
        );
    }

    #[tokio::test]
    async fn jwt_blocklist_insert_failure_does_not_report_success() {
        let ctx = TestContext::with_auth().await;
        let failing = FailingDbOpContext::new(ctx, vec![("database.create", jwt_blocklist::TABLE)]);

        let mut msg = auth_msg("update", "/b/auth/api/logout", "user-1");
        msg.set_meta(META_AUTH_JTI, "jti-1");
        msg.set_meta(
            META_AUTH_EXP,
            (chrono::Utc::now().timestamp() + 3600).to_string(),
        );
        let out = handle(&failing, &msg).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a JWT blocklist insert failure must not be reported as a successful logout"
        );
    }

    #[tokio::test]
    async fn successful_revocation_still_redirects() {
        let ctx = TestContext::with_auth().await;
        let mut msg = auth_msg("update", "/b/auth/api/logout", "user-1");
        msg.set_meta(META_AUTH_JTI, "jti-1");
        msg.set_meta(
            META_AUTH_EXP,
            (chrono::Utc::now().timestamp() + 3600).to_string(),
        );
        let out = handle(&ctx, &msg).await;
        assert_eq!(output_status(out).await, 303);
    }
}
