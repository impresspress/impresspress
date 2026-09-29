use std::collections::HashMap;

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{
    contracts::{AdminUserListQuery, AdminUserListResponse, AdminUserView},
    ops,
};
use crate::{
    blocks::{
        auth::repo::users::{self, ActiveUserQuery},
        crud::{self, db_error, db_error_internal},
    },
    http::{err_bad_request, err_not_found, ok_json},
};

/// `GET /b/admin/api/users`.
pub(super) async fn handle_list(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = AdminUserListQuery::from_message(msg);

    // The `deleted_at IS NULL` predicate, the sort and the search shape all
    // live in `users::list_active_page`, shared with the SSR users tab.
    match users::list_active_page(
        ctx,
        &ActiveUserQuery {
            page: i64::from(query.page),
            page_size: query.page_size,
            search: query.search.clone(),
        },
    )
    .await
    {
        Ok(page) => {
            // Bulk-enrich with roles via a single `In`-filter query (was N+1:
            // one `list_all` per row), then project each row onto the closed
            // `AdminUserView` field list. The projection is what keeps
            // `verification_token` (and any column a future migration adds) off
            // the wire — the previous code echoed the whole row and removed one
            // field by name.
            let user_ids: Vec<&str> = page.rows.iter().map(|r| r.id.as_str()).collect();
            match ops::fetch_roles(ctx, &user_ids).await {
                Ok(roles_by_user) => {
                    ok_json(&AdminUserListResponse::from_page(&page, &roles_by_user))
                }
                Err(e) => db_error_internal(e, "Could not load the users' roles"),
            }
        }
        // A `NotFound` from a paginated list names no user of the caller's —
        // `db_error_internal`, not `db_error`.
        Err(e) => db_error_internal(e, "Database error"),
    }
}

/// `GET /b/admin/api/users/{id}`. `{id}` is read only as the route table
/// bound it.
pub(super) async fn handle_get(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "User") {
        Ok(value) => value,
        Err(response) => return response,
    };
    get_user(ctx, id).await
}

async fn get_user(ctx: &dyn Context, id: &str) -> OutputStream {
    match users::find_by_id(ctx, id).await {
        Ok(Some(row)) => {
            // Get roles via the shared single-query helper.
            let roles = match ops::fetch_roles(ctx, &[id]).await {
                Ok(mut roles) => roles.remove(id).unwrap_or_default(),
                Err(e) => return db_error_internal(e, "Could not load the user's roles"),
            };
            // Same projection as the list endpoint. This path used to emit a
            // third shape — the raw `{id, data: {…}}` record with `roles`
            // grafted on beside `data` rather than inside it — so the two read
            // paths disagreed about where a user's roles lived and both echoed
            // `verification_token`.
            ok_json(&AdminUserView::from_row(&row, roles))
        }
        Ok(None) => err_not_found("User not found"),
        Err(e) => db_error(e, "User not found", "Database error"),
    }
}

/// `PATCH /b/admin/api/users/{id}`. `{id}` is read only as the route table
/// bound it.
pub(super) async fn handle_update(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let id = match crud::path_id(msg, "User") {
        Ok(value) => value,
        Err(response) => return response,
    };

    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // The self-disable guard, safe-field whitelist, and audit-log write all
    // live in the shared ops layer so the SSR surface can't diverge.
    match ops::update_user_fields(ctx, msg, id, &body).await {
        Ok(row) => {
            // Same projection as GET (`get_user` / `handle_list`). The row
            // type carries only the columns `auth::repo::users` decodes, so
            // `verification_token` / `last_verification_sent` / `auth_version`
            // are not reachable from here at all — they used to ride along in
            // the raw record this handler echoed.
            //
            // The update has landed by now; a failed roles read still answers
            // 500 rather than a view claiming the user holds no roles.
            let roles = match ops::fetch_roles(ctx, &[id]).await {
                Ok(mut roles) => roles.remove(id).unwrap_or_default(),
                Err(e) => {
                    return db_error_internal(
                        e,
                        "User updated, but their roles could not be loaded",
                    )
                }
            };
            ok_json(&AdminUserView::from_row(&row, roles))
        }
        Err(out) => out,
    }
}

/// `DELETE /b/admin/api/users/{id}`. `{id}` is read only as the route table
/// bound it.
pub(super) async fn handle_delete(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "User") {
        Ok(value) => value,
        Err(response) => return response,
    };

    // Self-delete guard, soft-delete, and audit-log write live in the shared
    // ops layer (the JSON path previously logged nothing).
    match ops::delete_user(ctx, msg, id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(out) => out,
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{
        blocks::admin::test_support::routed,
        test_support::{admin_msg, output_http_status, output_json, TestContext},
    };

    /// Seed one user row carrying every column the table has, including the
    /// two the API must never publish.
    /// Seed a user carrying the three columns the untyped handler used to
    /// echo (`verification_token`, `last_verification_sent`, `auth_version`),
    /// so the field-set assertions below are not vacuous. `insert` dual-writes
    /// `display_name` and the `name` alias, so both read "Ada".
    async fn seed_user(ctx: &dyn Context) -> String {
        let user = users::insert(
            ctx,
            users::NewUser {
                email: "admin@example.com".to_string(),
                display_name: "Ada".to_string(),
                avatar_url: None,
                role: "user".to_string(),
                email_verified: true,
                verification_token_hash: Some("3d1f0ac0deadbeef".to_string()),
            },
        )
        .await
        .expect("seed user");
        users::set_verification_token(ctx, &user.id, "3d1f0ac0deadbeef", "2026-08-01T00:00:00Z")
            .await
            .expect("seed verification bookkeeping");
        for _ in 0..7 {
            users::bump_auth_version(ctx, &user.id)
                .await
                .expect("seed auth_version");
        }
        user.id
    }

    async fn users_ctx() -> TestContext {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        // Admin first: the migration runner records its state in
        // `block_settings`, which the admin schema creates.
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        crate::blocks::auth::migrations::apply(&ctx)
            .await
            .expect("apply auth migrations");
        ctx
    }

    /// A user whose roles cannot be read is a 500, not `"roles": []`.
    ///
    /// `break_list_reads` keeps the single-row user read working and fails the
    /// roles query, the shape a wobbling database gives this handler.
    #[tokio::test]
    async fn a_failed_roles_read_is_a_500_not_an_empty_roles_list() {
        let ctx = users_ctx().await;
        let id = seed_user(&ctx).await;
        crate::platform_state::user_roles::assign(&ctx, &id, "admin", "")
            .await
            .expect("grant");
        let failing = ctx.break_list_reads();

        let msg = routed(admin_msg("retrieve", &format!("/b/admin/api/users/{id}")));
        let out = wafer_run::Block::handle(
            &crate::blocks::admin::AdminBlock::new(),
            &failing,
            msg,
            InputStream::empty(),
        )
        .await;

        assert_eq!(output_http_status(out).await, 500);
    }

    /// The field set the endpoint publishes is exactly the contract's, no more.
    ///
    /// This is the assertion the `/openapi.json` schema rests on: the schema is
    /// derived from `AdminUserView`, so it is only true while the handler emits
    /// that type's fields and nothing else. Before the projection existed, the
    /// handler echoed the whole row, and this test's `verification_token` would
    /// have been on the wire.
    #[tokio::test]
    async fn list_publishes_exactly_the_contract_fields() {
        let ctx = users_ctx().await;
        seed_user(&ctx).await;

        let body =
            output_json(handle_list(&ctx, &admin_msg("retrieve", "/b/admin/api/users")).await)
                .await;

        let row = body["records"][0]
            .as_object()
            .expect("one user record on the wire");
        let mut got: Vec<&str> = row.keys().map(String::as_str).collect();
        got.sort_unstable();
        let mut want = vec![
            "avatar_url",
            "created_at",
            "deleted_at",
            "disabled",
            "display_name",
            "email",
            "email_verified",
            "id",
            "last_login_at",
            "name",
            "role",
            "roles",
            "updated_at",
        ];
        want.sort_unstable();
        assert_eq!(
            got, want,
            "the wire field set must equal AdminUserView's, or the published \
             schema describes something the handler does not emit"
        );

        // The envelope is unchanged from the untyped `RecordList` it replaced.
        assert!(body["total_count"].is_i64());
        assert!(body["page"].is_i64());
        assert!(body["page_size"].is_i64());
    }

    /// No credential material reaches the wire, checked against the serialized
    /// body rather than a field list so a nested or renamed leak still trips it.
    #[tokio::test]
    async fn list_never_emits_credential_columns() {
        let ctx = users_ctx().await;
        seed_user(&ctx).await;

        let body =
            output_json(handle_list(&ctx, &admin_msg("retrieve", "/b/admin/api/users")).await)
                .await;
        let raw = body.to_string();

        for leaked in ["verification_token", "3d1f0ac0deadbeef", "password_hash"] {
            assert!(
                !raw.contains(leaked),
                "GET /b/admin/api/users leaked `{leaked}`: {raw}"
            );
        }
    }

    /// `email_verified` / `disabled` are `INTEGER` on SQLite and `BOOLEAN` on
    /// Postgres. The schema says `boolean`, so the handler must normalize —
    /// echoing the column would make the schema false on one backend or the
    /// other.
    #[tokio::test]
    async fn list_normalizes_backend_dependent_column_types() {
        let ctx = users_ctx().await;
        seed_user(&ctx).await;

        let body =
            output_json(handle_list(&ctx, &admin_msg("retrieve", "/b/admin/api/users")).await)
                .await;

        assert_eq!(
            body["records"][0]["email_verified"],
            serde_json::json!(true)
        );
        assert_eq!(body["records"][0]["disabled"], serde_json::json!(false));
    }

    /// Both read paths project through the same view. `GET /…/users/{id}` used
    /// to emit a third shape (`{id, data: {…}, roles: […]}`) that echoed the
    /// same columns the list did.
    #[tokio::test]
    async fn get_by_id_uses_the_same_projection_as_list() {
        let ctx = users_ctx().await;
        let id = seed_user(&ctx).await;

        let body = output_json(get_user(&ctx, &id).await).await;

        assert_eq!(body["id"], serde_json::json!(id));
        assert_eq!(body["email"], serde_json::json!("admin@example.com"));
        assert!(
            body.get("data").is_none(),
            "the record envelope must not survive the projection: {body}"
        );
        assert!(
            !body.to_string().contains("verification_token"),
            "GET /b/admin/api/users/{{id}} leaked verification_token: {body}"
        );
    }

    /// `PUT /b/admin/api/users/{id}` used to `ok_json(&record)` the raw
    /// `db::Record` returned by `ops::update_user_fields` — the same table,
    /// same leak as the pre-fix `GET`, just a different handler. It must now
    /// project through `AdminUserView` like the read paths, so the three
    /// withheld columns can't reach the wire through this fourth shape.
    #[tokio::test]
    async fn update_uses_the_same_projection_as_get_and_list() {
        let ctx = users_ctx().await;
        let id = seed_user(&ctx).await;

        let input = InputStream::from_bytes(
            serde_json::to_vec(&serde_json::json!({"name": "Ada Updated"})).unwrap(),
        );
        let msg = routed(admin_msg("update", &format!("/b/admin/api/users/{id}")));
        let body = output_json(handle_update(&ctx, &msg, input).await).await;

        assert_eq!(body["id"], serde_json::json!(id));
        assert_eq!(body["name"], serde_json::json!("Ada Updated"));
        assert!(
            body.get("data").is_none(),
            "the record envelope must not survive the projection: {body}"
        );
        let raw = body.to_string();
        for leaked in ["verification_token", "3d1f0ac0deadbeef", "password_hash"] {
            assert!(
                !raw.contains(leaked),
                "PUT /b/admin/api/users/{{id}} leaked `{leaked}`: {raw}"
            );
        }
    }

    /// A context that reached the admin block with NO WRAP grants, so every
    /// typed database call it makes is refused by the same
    /// `wrap::check_access` the runtime applies. The auth schema is applied
    /// first, so the refusal is a denial and not a missing table.
    async fn denied_users_ctx() -> TestContext {
        users_ctx().await.running_as("test/ungranted")
    }

    /// The reason `RepoError` had to fold into `WaferError`.
    ///
    /// `auth::repo::users::find_by_id` used to answer with
    /// `RepoError::Db(String)`, which had already thrown the wafer code away
    /// — so by the time this handler saw the failure it could not tell a
    /// WRAP refusal from a decode fault, and answered `500 Internal server
    /// error (ref: …)` for both. An operator running a deployment whose
    /// admin block is missing its `wafer_run__auth__users` grant read that
    /// as an outage.
    #[tokio::test]
    async fn a_denied_user_read_is_403_not_500() {
        let ctx = denied_users_ctx().await;
        assert_eq!(
            output_http_status(get_user(&ctx, "any-id").await).await,
            403
        );
    }

    /// The same denial through every other auth-repo-backed admin user
    /// handler, so the fix is the repo layer's and not one handler's.
    #[tokio::test]
    async fn a_denied_user_list_is_403_not_500() {
        let ctx = denied_users_ctx().await;
        let out = handle_list(&ctx, &admin_msg("retrieve", "/b/admin/api/users")).await;
        assert_eq!(output_http_status(out).await, 403);
    }

    #[tokio::test]
    async fn a_denied_user_update_is_403_not_500() {
        let ctx = denied_users_ctx().await;
        let input = InputStream::from_bytes(
            serde_json::to_vec(&serde_json::json!({"name": "Ada Updated"})).unwrap(),
        );
        let msg = routed(admin_msg("update", "/b/admin/api/users/any-id"));
        assert_eq!(
            output_http_status(handle_update(&ctx, &msg, input).await).await,
            403
        );
    }

    #[tokio::test]
    async fn a_denied_user_delete_is_403_not_500() {
        let ctx = denied_users_ctx().await;
        let msg = routed(admin_msg("delete", "/b/admin/api/users/any-id"));
        assert_eq!(
            output_http_status(handle_delete(&ctx, &msg).await).await,
            403
        );
    }

    /// The granted path still answers as it did, so the 403 above is the
    /// denial and not a blanket refusal.
    #[tokio::test]
    async fn a_granted_read_of_a_missing_user_is_still_404() {
        let ctx = users_ctx().await;
        assert_eq!(
            output_http_status(get_user(&ctx, "no-such-user").await).await,
            404
        );
    }
}
