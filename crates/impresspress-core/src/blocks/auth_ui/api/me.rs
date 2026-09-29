//! GET / PATCH /b/auth/api/me — relocated from auth/login.rs in Task 5.

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::{
        auth::{
            helpers::get_user_roles,
            repo::users::{self, UserRow},
        },
        auth_ui::contracts::{MeResponse, MeUser, UpdateMeRequest},
        crud,
    },
    http::{err_bad_request, err_not_found, ok_json},
};

/// The one projection both handlers share. `PATCH` used to build its own
/// flat `json!` object while `GET` returned `{user: {...}}`, so the two
/// paths published different shapes for the same resource and only one of
/// them had a schema. Routing both through here means the read and the
/// write path cannot drift on what they say about the caller.
fn me_response(user: UserRow, roles: Vec<String>) -> MeResponse {
    MeResponse {
        user: MeUser {
            id: user.id,
            email: user.email,
            name: user.display_name,
            roles,
            created_at: user.created_at,
            avatar_url: user.avatar_url.unwrap_or_default(),
        },
    }
}

pub async fn handle_get(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id();
    if user_id.is_empty() {
        return crate::http::err_unauthenticated("Not authenticated");
    }
    let user = match users::find_by_id(ctx, user_id).await {
        Ok(Some(user)) => user,
        // A valid token whose row is gone: the account was deleted while the
        // access token was still live.
        Ok(None) => return err_not_found("User not found"),
        // A read that could not run is not a deleted account. 404 is the one
        // answer a signed-in caller must never get from an outage — no
        // client retries it, and it reads as "you no longer exist".
        Err(e) => return crud::db_error_internal(e, "Could not load the signed-in user"),
    };
    let roles = match get_user_roles(ctx, user_id).await {
        Ok(r) => r,
        Err(e) => return crud::db_error_internal(e, "Failed to resolve user roles"),
    };
    ok_json(&me_response(user, roles))
}

pub async fn handle_update(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let user_id = msg.user_id();
    if user_id.is_empty() {
        return crate::http::err_unauthenticated("Not authenticated");
    }

    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: UpdateMeRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // `name` dual-writes display_name + the legacy name alias inside
    // update_profile.
    match users::update_profile(
        ctx,
        user_id,
        body.name.as_deref(),
        body.avatar_url.as_deref(),
    )
    .await
    {
        Ok(user) => {
            let roles = match get_user_roles(ctx, user_id).await {
                Ok(r) => r,
                Err(e) => return crud::db_error_internal(e, "Failed to resolve user roles"),
            };
            ok_json(&me_response(user, roles))
        }
        Err(e) => crud::db_error(e, "User not found", "Update failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::auth::repo::users::NewUser,
        test_support::{anon_msg, auth_msg, output_is_error, output_json, TestContext},
    };

    async fn seed_user(ctx: &dyn Context) -> UserRow {
        users::insert(
            ctx,
            NewUser {
                email: "ada@example.com".to_string(),
                display_name: "Ada".to_string(),
                avatar_url: None,
                role: "user".to_string(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .expect("seed user")
    }

    fn body(json: serde_json::Value) -> InputStream {
        InputStream::from_bytes(serde_json::to_vec(&json).unwrap())
    }

    /// `PATCH` used to return a flat user object while `GET` returned
    /// `{user: {...}}`. Both now go through `me_response`, so the update
    /// response must be exactly what a subsequent `GET` returns.
    #[tokio::test]
    async fn update_returns_the_same_envelope_as_get() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let user = seed_user(&ctx).await;

        let updated = output_json(
            handle_update(
                &ctx,
                &auth_msg("update", "/b/auth/api/me", &user.id),
                body(serde_json::json!({
                    "name": "Ada Updated",
                    "avatar_url": "https://example.com/a.png"
                })),
            )
            .await,
        )
        .await;
        let fetched =
            output_json(handle_get(&ctx, &auth_msg("retrieve", "/b/auth/api/me", &user.id)).await)
                .await;

        assert_eq!(updated["user"]["name"], serde_json::json!("Ada Updated"));
        assert_eq!(
            updated["user"]["avatar_url"],
            serde_json::json!("https://example.com/a.png")
        );
        assert_eq!(
            updated, fetched,
            "PATCH and GET must publish the same projection of the same row"
        );
        assert!(
            updated.get("id").is_none(),
            "the flat pre-fix shape must not survive: {updated}"
        );
    }

    /// The typed body replaces a `HashMap` peek that silently treated a
    /// non-string `name` as absent. The published schema says `string`, so
    /// the handler must refuse what the schema refuses.
    #[tokio::test]
    async fn update_rejects_a_body_the_schema_rejects() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let user = seed_user(&ctx).await;

        let out = handle_update(
            &ctx,
            &auth_msg("update", "/b/auth/api/me", &user.id),
            body(serde_json::json!({"name": 42})),
        )
        .await;
        assert!(output_is_error(out, "InvalidArgument").await);
    }

    /// Every auth-ui API handler's own identity check answers what the
    /// router's gate does: 401, the challenge, and `not_authenticated`.
    #[tokio::test]
    async fn the_auth_api_handlers_refuse_no_identity_with_the_challenge() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let empty = || InputStream::from_bytes(b"{}".to_vec());
        let answers = [
            (
                "GET me",
                handle_get(&ctx, &anon_msg("retrieve", "/b/auth/api/me")).await,
            ),
            (
                "PATCH me",
                handle_update(&ctx, &anon_msg("update", "/b/auth/api/me"), empty()).await,
            ),
            (
                "change password",
                super::super::change_password::handle(
                    &ctx,
                    &anon_msg("create", "/b/auth/api/change-password"),
                    empty(),
                )
                .await,
            ),
            (
                "list api keys",
                super::super::api_keys::handle_list(
                    &ctx,
                    &anon_msg("retrieve", "/b/auth/api/api-keys"),
                )
                .await,
            ),
            (
                "create api key",
                super::super::api_keys::handle_create(
                    &ctx,
                    &anon_msg("create", "/b/auth/api/api-keys"),
                    empty(),
                )
                .await,
            ),
        ];
        for (label, out) in answers {
            let parts = wafer_block::http_codec::collect_http_response(out).await;
            let body = String::from_utf8_lossy(&parts.body).into_owned();
            assert_eq!(parts.status, 401, "{label}: {body}");
            assert!(
                parts.headers.iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("WWW-Authenticate")
                        && value == crate::http::WWW_AUTHENTICATE
                }),
                "{label}: {:?}",
                parts.headers
            );
            assert!(
                body.contains(r#""code":"not_authenticated""#),
                "{label}: {body}"
            );
        }
    }

    #[tokio::test]
    async fn update_requires_a_signed_in_caller() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let out = handle_update(
            &ctx,
            &anon_msg("update", "/b/auth/api/me"),
            body(serde_json::json!({"name": "x"})),
        )
        .await;
        assert!(output_is_error(out, "Unauthenticated").await);
    }

    /// A read that could not run is not "your account does not exist".
    /// `handle_get` collapsed both into `404 User not found`, so a database
    /// outage told a signed-in caller their account was gone — on a status
    /// no client retries and with nothing anywhere near the response to say
    /// an outage had happened.
    #[tokio::test]
    async fn an_unreadable_user_row_is_an_outage_not_a_missing_account() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let user = seed_user(&ctx).await;
        // The lookup under test is the handler's first read, so a database
        // whose reads all fail lands on it and on nothing earlier.
        let failing = ctx.break_reads();

        let out = handle_get(&failing, &auth_msg("retrieve", "/b/auth/api/me", &user.id)).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a failed lookup of the caller's own row must not answer 404"
        );
    }
}
