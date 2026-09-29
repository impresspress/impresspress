//! POST /b/auth/api/logout — relocated from auth/login.rs in Task 5.

use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{
        auth::{
            helpers::build_auth_cookie,
            repo::{
                jwt_blocklist::{self, NewBlocklistEntry},
                sessions, tokens,
            },
        },
        auth_ui::contracts::MessageResponse,
        crud,
    },
    crypto::{META_AUTH_EXP, META_AUTH_JTI},
    http::ResponseBuilder,
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
            return crud::db_error_internal(e, "Logout could not fully revoke the session");
        }

        // [B12] Logout is an all-devices operation — the line above revokes
        // every refresh family the user has — so every session row goes with
        // them. Leaving the rows behind is what made `/b/userportal/sessions`
        // list devices as signed in for weeks after the tokens that kept them
        // alive had been revoked. Same fail-closed treatment as the
        // revocation itself: a list that still shows a revoked device is a
        // security-relevant lie, not a cosmetic one.
        if let Err(e) = sessions::delete_all_for_user(ctx, user_id).await {
            tracing::error!(
                user_id = %user_id,
                error = %e,
                "logout: session-row deletion failed"
            );
            return crud::db_error_internal(e, "Logout could not fully revoke the session");
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
                match crate::blocks::auth::helpers::access_token_lifetime_secs(ctx).await {
                    Ok(secs) => secs,
                    Err(e) => {
                        return crud::db_error_internal(
                            e,
                            "Logout could not read the access-token lifetime",
                        )
                    }
                };
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
                return crud::db_error_internal(e, "Logout could not fully revoke the session");
            }
        }
    }

    let cookie = match build_auth_cookie("", 0, ctx).await {
        Ok(cookie) => cookie,
        Err(e) => return crud::db_error_internal(e, "Logout could not build the cookie"),
    };
    ResponseBuilder::new()
        .set_cookie(&cookie)
        .status(303)
        .set_header("Location", "/b/auth/login")
        .json(&MessageResponse {
            message: "Logged out successfully".to_string(),
        })
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let msg = crate::test_support::anon_msg("update", "/b/auth/api/logout");
        let out = handle(&ctx, &msg).await;
        assert_eq!(output_status(out).await, 303);
    }

    #[tokio::test]
    async fn refresh_token_revocation_failure_does_not_report_success() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
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

    /// [B12] The device list and the refresh tokens have to agree: logout
    /// revokes every family, so it must remove every row too.
    #[tokio::test]
    async fn logout_deletes_the_users_session_rows() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.seed_auth_user("user-1").await;
        ctx.seed_auth_user("user-2").await;
        for (user_id, family) in [
            ("user-1", "fam-a"),
            ("user-1", "fam-b"),
            ("user-2", "fam-c"),
        ] {
            sessions::insert(
                &ctx,
                sessions::NewSession {
                    family: family.into(),
                    user_id: user_id.into(),
                    auth_method: "password".into(),
                    expires_at: "2099-01-01T00:00:00Z".into(),
                },
            )
            .await
            .expect("seed session row");
        }

        let out = handle(&ctx, &auth_msg("update", "/b/auth/api/logout", "user-1")).await;
        assert_eq!(output_status(out).await, 303);

        assert!(
            sessions::list_for_user(&ctx, "user-1")
                .await
                .unwrap()
                .is_empty(),
            "logout revokes every family, so it removes every device row"
        );
        assert_eq!(
            sessions::list_for_user(&ctx, "user-2").await.unwrap().len(),
            1,
            "another user's devices are untouched"
        );
    }

    /// A session-row deletion failure is not reported as a successful logout:
    /// a device list that still shows a revoked device is a lie about who is
    /// signed in.
    #[tokio::test]
    async fn session_row_deletion_failure_does_not_report_success() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let failing =
            FailingDbOpContext::new(ctx, vec![("database.delete_where_count", sessions::TABLE)]);

        let out = handle(
            &failing,
            &auth_msg("update", "/b/auth/api/logout", "user-1"),
        )
        .await;
        assert!(output_is_error(out, "Internal").await);
    }

    #[tokio::test]
    async fn successful_revocation_still_redirects() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
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
