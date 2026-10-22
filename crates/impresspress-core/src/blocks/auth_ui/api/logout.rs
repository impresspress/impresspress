//! POST /b/auth/api/logout — relocated from auth/login.rs in Task 5.

use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{
        auth::helpers::{clear_auth_cookie, end_all_families, end_presented_access_token},
        auth_ui::contracts::MessageResponse,
        crud,
    },
    http::ResponseBuilder,
};

pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id();
    if !user_id.is_empty() {
        // [B12] Logout is an all-devices operation: every session row goes,
        // then every refresh row is revoked (`end_all_families`, which owns
        // that order and the race it leaves). The row is the family's
        // liveness (`auth::repo::sessions`), so deleting it is what signs the
        // devices out: their access tokens stop authenticating and a rotation
        // racing it finds no row to touch and refuses. The refresh rows are
        // revoked, not deleted, so the tombstones remain for reuse detection
        // (SEC-032/039). A failure of either is not reported as a successful
        // logout.
        if let Err(e) = end_all_families(ctx, user_id)
            .await
            .map_err(|e| e.into_error())
        {
            tracing::error!(
                user_id = %user_id,
                error = %e,
                "logout: ending the account's devices failed"
            );
            return crud::db_error_internal(e, "Logout could not fully revoke the session");
        }

        // SEC-042: blocklist the presented access JWT's `jti` as well. The
        // row deletion above already ended it (an access token needs a live
        // family, `crypto::verify_access_token`); this is the per-token
        // record of the same end. Same fail-closed treatment.
        if let Err(e) = end_presented_access_token(ctx, msg).await {
            tracing::error!(
                user_id = %user_id,
                error = %e,
                "logout: jwt blocklist insert failed"
            );
            return crud::db_error_internal(e, "Logout could not fully revoke the session");
        }
    }

    let cookie = match clear_auth_cookie(ctx).await {
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
    use crate::{
        blocks::auth::repo::{jwt_blocklist, sessions, tokens},
        crypto::{META_AUTH_EXP, META_AUTH_JTI},
        test_support::{auth_msg, output_is_error, output_status, FailingDbOpContext, TestContext},
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
