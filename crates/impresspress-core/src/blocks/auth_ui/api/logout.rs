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
    http::ResponseBuilder,
};

pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id();
    if !user_id.is_empty() {
        // SEC-032/039: revoke (don't delete) the user's refresh-token rows
        // so the tombstones remain available for reuse detection. The
        // browser drops its cookie below either way; this just invalidates
        // any in-flight refresh attempts on other clients.
        tokens::revoke_all_for_user(ctx, user_id).await.ok();

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
            let _ = jwt_blocklist::insert(
                ctx,
                NewBlocklistEntry {
                    jti,
                    user_id,
                    expires_at: &expires_at_iso,
                },
            )
            .await;
        }
    }

    let cookie = build_auth_cookie("", 0, ctx).await;
    ResponseBuilder::new()
        .set_cookie(&cookie)
        .status(303)
        .set_header("Location", "/b/auth/login")
        .json(&serde_json::json!({"message": "Logged out successfully"}))
}
