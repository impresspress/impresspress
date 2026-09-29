//! /b/auth/api/api-keys — relocated from auth/api_keys.rs in Task 5.
//!
//! Admin user-management's Create API Key form still posts [`handle_create`]
//! via htmx (see `impresspress-core/src/blocks/admin/pages/users.rs`); its
//! Revoke button is served by the admin block's own route, which answers with
//! the re-rendered tab. PAT migration is a
//! follow-up; for PR 5 we relocate rather than delete.

use wafer_core::clients::crypto;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::{auth::repo::api_keys, auth_ui::contracts, crud},
    http::{err_bad_request, err_forbidden, err_internal, err_not_found, ok_json},
    util::{hex_encode, sha256_hex},
};

/// Read the request's `expires_at` as the instant it names, or answer `400`.
///
/// The column used to take the caller's string as it stood and the lookup
/// string-compared it against the clock, which gets two whole classes of
/// value wrong: an offset (`…T20:00:00+09:00` is 11:00 UTC but sorts after
/// `…T12:00:00Z`) and a non-timestamp (`"never"` sorts after every
/// timestamp, so the key never expired). Refusing what cannot be read, here,
/// is what keeps the stored column to instants the lookup can compare.
///
/// An absent field and an empty one both mean "no expiry" — an HTML form
/// posts an untouched field as `""`, and no caller means "expire this key at
/// a timestamp I did not give".
fn parse_expiry(raw: Option<&str>) -> Result<Option<chrono::DateTime<chrono::Utc>>, OutputStream> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw) else {
        return Err(err_bad_request(&format!(
            "expires_at must be an RFC 3339 timestamp (e.g. 2027-01-31T09:00:00Z), got {raw:?}"
        )));
    };
    let parsed = parsed.with_timezone(&chrono::Utc);
    if parsed <= chrono::Utc::now() {
        return Err(err_bad_request(&format!(
            "expires_at must be in the future, got {raw:?}"
        )));
    }
    Ok(Some(parsed))
}

pub async fn handle_list(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id();
    if user_id.is_empty() {
        return crate::http::err_unauthenticated("Authentication required");
    }
    match api_keys::list_for_user(ctx, user_id).await {
        Ok(rows) => {
            // Serialise each row WITHOUT key_hash — the secret never leaves
            // the DB. Shape mirrors the previous `db::list` ListResult payload
            // (records[].data minus key_hash, plus total_count).
            let total_count = rows.len() as i64;
            let records: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|k| {
                    serde_json::json!({
                        "id": k.id,
                        "data": {
                            "user_id": k.user_id,
                            "name": k.name,
                            "key_prefix": k.key_prefix,
                            "created_at": k.created_at,
                            "expires_at": k.expires_at,
                            "revoked_at": k.revoked_at,
                        }
                    })
                })
                .collect();
            // page/page_size mirror the previous RecordList payload (single
            // unpaginated page of the caller's keys).
            ok_json(&serde_json::json!({
                "records": records,
                "total_count": total_count,
                "page": 1,
                "page_size": total_count,
            }))
        }
        Err(e) => crud::db_error_internal(e, "Could not list the API keys"),
    }
}

/// `POST /b/auth/api/api-keys`. Body: [`contracts::CreateApiKeyRequest`] —
/// a non-empty `name`, and an optional `expires_at` that must be an RFC 3339
/// timestamp in the future ([`parse_expiry`]). Answers the raw key once, as
/// JSON, or as the reveal card when the caller is htmx.
pub async fn handle_create(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let user_id = msg.user_id();
    if user_id.is_empty() {
        return crate::http::err_unauthenticated("Authentication required");
    }

    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let parsed = match crate::util::parse_body_value(&raw) {
        Ok(value) => value,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    let body: contracts::CreateApiKeyRequest = match serde_json::from_value(parsed) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    if body.name.is_empty() {
        return err_bad_request("API key name is required");
    }
    let expires_at = match parse_expiry(body.expires_at.as_deref()) {
        Ok(value) => value,
        Err(response) => return response,
    };

    // Generate random key
    let random_bytes = match crypto::random_bytes(ctx, 24).await {
        Ok(b) => b,
        Err(e) => return err_internal("Failed to generate key", e),
    };
    let key_string = format!("sb_{}", hex_encode(&random_bytes));

    // Use deterministic SHA-256 hash for key lookup (not argon2, which is non-deterministic)
    let key_hash = sha256_hex(key_string.as_bytes());
    let key_prefix = key_string[..10].to_string();

    let insert_result = api_keys::insert(
        ctx,
        api_keys::NewApiKey {
            user_id,
            name: &body.name,
            key_hash: &key_hash,
            key_prefix: &key_prefix,
            expires_at,
        },
    )
    .await;

    match insert_result {
        Ok(record) => {
            // htmx form callers want HTML back so the swap renders cleanly.
            // Programmatic JSON callers (no HX-Request header) get the JSON
            // payload as before so existing API consumers don't break.
            if !msg.get_meta("http.header.hx-request").is_empty() {
                let key_for_display = key_string.clone();
                let name = record.name;
                let markup = maud::html! {
                    div .card .api-key-card {
                        div .card__head { h2 .card__title { "Key created — save it now" } }
                        div .card__body {
                            p .api-key-copy-hint .text-13 {
                                "This is the only time the full key will be shown. Copy it now."
                            }
                            div .api-key-reveal-row {
                                code #new-api-key .api-key-value .text-13 {
                                    (key_for_display)
                                }
                                // Chrome's `copy-text` verb reads the key
                                // out of `#new-api-key`, so the key itself is
                                // never written into an attribute — which is
                                // also what the inline handler this replaced
                                // was careful to avoid.
                                button type="button" .btn .btn--secondary .btn--sm .flex-none
                                    data-action="copy-text" data-copy-source="new-api-key"
                                { "Copy" }
                            }
                            p .api-key-name-hint {
                                "Name: " (name)
                            }
                        }
                    }
                };
                let trigger = r#"{"showToast":{"message":"API key created","type":"success"},"closeModal":{"id":"create-api-key"}}"#;
                crate::http::ResponseBuilder::new()
                    .set_header("HX-Trigger", trigger)
                    .body(
                        markup.into_string().into_bytes(),
                        "text/html; charset=utf-8",
                    )
            } else {
                ok_json(&serde_json::json!({
                    "id": record.id,
                    "key": key_string,
                    "name": record.name,
                    "key_prefix": record.key_prefix,
                    "message": "Save this key — it won't be shown again"
                }))
            }
        }
        Err(e) => crud::db_error_internal(e, "Could not create the API key"),
    }
}

/// `PATCH /b/auth/api/api-keys/{id}`. `{id}` is read only as the route table
/// bound it.
pub async fn handle_revoke(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Key") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let user_id = msg.user_id();

    // Verify ownership. A read that could not run is not a missing key: the
    // key is still live, and answering 404 tells its owner it has already
    // been removed while the revoke they came to perform did not happen.
    let key = match api_keys::find_by_id(ctx, id).await {
        Ok(Some(key)) => key,
        Ok(None) => return err_not_found("API key not found"),
        Err(e) => return crud::db_error_internal(e, "Could not load the API key"),
    };
    if key.user_id != user_id && !crate::util::is_admin(msg) {
        return err_forbidden("Cannot revoke another user's API key");
    }

    match api_keys::revoke(ctx, id).await {
        Ok(_) => ok_json(&serde_json::json!({"message": "API key revoked"})),
        Err(e) => crud::db_error(e, "API key not found", "Could not revoke the API key"),
    }
}

/// `DELETE /b/auth/api/api-keys/{id}`. `{id}` is read only as the route table
/// bound it.
pub async fn handle_delete(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Key") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let user_id = msg.user_id();

    // Verify ownership. Same rule as `handle_revoke`: a failed read is an
    // outage, not a key that has already gone.
    let key = match api_keys::find_by_id(ctx, id).await {
        Ok(Some(key)) => key,
        Ok(None) => return err_not_found("API key not found"),
        Err(e) => return crud::db_error_internal(e, "Could not load the API key"),
    };
    if key.user_id != user_id && !crate::util::is_admin(msg) {
        return err_forbidden("Cannot delete another user's API key");
    }

    match api_keys::delete(ctx, id).await {
        Ok(_) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) => crud::db_error(e, "API key not found", "Could not delete the API key"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::{auth::repo::users, auth_ui::test_support::routed},
        test_support::{auth_msg, output_is_error, output_json, TestContext},
    };

    const KEYS_PATH: &str = "/b/auth/api/api-keys";

    /// A user row with no keys; returns its id.
    async fn seed_user(ctx: &TestContext) -> String {
        users::insert(
            ctx,
            users::NewUser {
                email: "owner@example.com".into(),
                display_name: "Owner".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .expect("seed user")
        .id
    }

    /// `POST /b/auth/api/api-keys` with a JSON body, through the route table.
    async fn create(ctx: &TestContext, owner: &str, body: serde_json::Value) -> OutputStream {
        handle_create(
            ctx,
            &routed(auth_msg("create", KEYS_PATH, owner)),
            InputStream::from_bytes(body.to_string().into_bytes()),
        )
        .await
    }

    /// A user row plus one API key it owns; returns `(user_id, key_id)`.
    async fn seed_user_with_key(ctx: &TestContext) -> (String, String) {
        let user_id = seed_user(ctx).await;
        let key = api_keys::insert(
            ctx,
            api_keys::NewApiKey {
                user_id: &user_id,
                name: "ci",
                key_hash: "not-a-real-hash",
                key_prefix: "sb_0000000",
                expires_at: None,
            },
        )
        .await
        .expect("seed api key");
        (user_id, key.id)
    }

    /// `handle_revoke` reads `{id}` only as the route table bound it: the
    /// same message is refused unrouted and revokes the key once it has been
    /// through `ROUTES`.
    #[tokio::test]
    async fn revoke_reads_only_the_bound_id() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let (owner, key_id) = seed_user_with_key(&ctx).await;
        let path = format!("/b/auth/api/api-keys/{key_id}");

        let unrouted = handle_revoke(&ctx, &auth_msg("update", &path, &owner)).await;
        assert!(
            output_is_error(unrouted, "InvalidArgument").await,
            "nothing bound means nothing to revoke"
        );
        let key = api_keys::find_by_id(&ctx, &key_id).await.unwrap().unwrap();
        assert!(key.revoked_at.is_none(), "an unrouted call must not revoke");

        let through_table = handle_revoke(&ctx, &routed(auth_msg("update", &path, &owner))).await;
        assert_eq!(
            output_json(through_table).await["message"],
            "API key revoked"
        );
        let key = api_keys::find_by_id(&ctx, &key_id).await.unwrap().unwrap();
        assert!(key.revoked_at.is_some());
    }

    /// Same contract for `handle_delete`.
    #[tokio::test]
    async fn delete_reads_only_the_bound_id() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let (owner, key_id) = seed_user_with_key(&ctx).await;
        let path = format!("/b/auth/api/api-keys/{key_id}");

        let unrouted = handle_delete(&ctx, &auth_msg("delete", &path, &owner)).await;
        assert!(
            output_is_error(unrouted, "InvalidArgument").await,
            "nothing bound means nothing to delete"
        );
        assert!(
            api_keys::find_by_id(&ctx, &key_id).await.unwrap().is_some(),
            "an unrouted call must not delete"
        );

        let through_table = handle_delete(&ctx, &routed(auth_msg("delete", &path, &owner))).await;
        assert_eq!(output_json(through_table).await["deleted"], true);
        assert!(api_keys::find_by_id(&ctx, &key_id).await.unwrap().is_none());
    }

    /// The ownership lookup is the only thing standing between the caller and
    /// someone else's key, and its failure used to answer `404 API key not
    /// found`. The owner was told their key had already been removed, and the
    /// revoke they came to perform silently did not happen.
    #[tokio::test]
    async fn revoke_reports_an_unreadable_key_row_as_an_outage() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let (owner, key_id) = seed_user_with_key(&ctx).await;
        let path = format!("/b/auth/api/api-keys/{key_id}");
        // The ownership lookup is the handler's first read, so a database
        // whose reads all fail lands on it and on nothing earlier.
        let failing = ctx.break_reads();

        let out = handle_revoke(&failing, &routed(auth_msg("update", &path, &owner))).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a failed ownership lookup must not answer 404: the key is still there"
        );
    }

    /// An expiry the lookup cannot compare is not an expiry. The column took
    /// the caller's string as it stood, and the lookup compared it to the
    /// clock as text: `"never"` sorts after every timestamp, so the key it
    /// was minted with outlived every clock reading there will ever be.
    #[tokio::test]
    async fn create_refuses_an_expiry_that_is_not_a_timestamp() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let owner = seed_user(&ctx).await;

        for expires_at in ["never", "2027-01-31", "tomorrow", "0"] {
            let out = create(
                &ctx,
                &owner,
                serde_json::json!({
                    "name": "ci",
                    "expires_at": expires_at,
                }),
            )
            .await;
            assert!(
                output_is_error(out, "InvalidArgument").await,
                "{expires_at:?} is not a readable expiry"
            );
        }
        assert!(
            api_keys::list_for_user(&ctx, &owner)
                .await
                .unwrap()
                .is_empty(),
            "a refused request must not mint a key"
        );
    }

    /// An offset names an instant, and the row must record that instant.
    /// Stored verbatim, `…T20:00:00+09:00` sorted eight hours ahead of the
    /// 11:00 UTC it actually names, so the key stayed live for those hours.
    #[tokio::test]
    async fn create_stores_an_offset_expiry_as_the_instant_it_names() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let owner = seed_user(&ctx).await;
        // Nine hours ahead of UTC, so the wall clock written here is always
        // ahead of the UTC spelling of the same instant.
        let offset = chrono::FixedOffset::east_opt(9 * 3600).expect("+09:00");
        let instant = chrono::Utc::now() + chrono::Duration::days(30);
        let sent = instant.with_timezone(&offset).to_rfc3339();

        let out = create(
            &ctx,
            &owner,
            serde_json::json!({
                "name": "ci",
                "expires_at": sent,
            }),
        )
        .await;
        assert_eq!(output_json(out).await["name"], "ci");

        let keys = api_keys::list_for_user(&ctx, &owner).await.unwrap();
        let [key] = keys.as_slice() else {
            panic!("exactly one key, got {}", keys.len())
        };
        assert_eq!(
            key.expires_at.as_deref(),
            Some(instant.format("%Y-%m-%dT%H:%M:%SZ").to_string().as_str()),
            "sent {sent}, which is that instant in UTC"
        );
        assert!(!key.is_expired(instant - chrono::Duration::seconds(1)));
        assert!(key.is_expired(instant));
    }

    /// A key that is already dead when it is minted is a request that meant
    /// something else — most often a timezone the caller did not intend.
    #[tokio::test]
    async fn create_refuses_an_expiry_already_past() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let owner = seed_user(&ctx).await;
        let offset = chrono::FixedOffset::east_opt(9 * 3600).expect("+09:00");
        let past = (chrono::Utc::now() - chrono::Duration::hours(1))
            .with_timezone(&offset)
            .to_rfc3339();

        let out = create(
            &ctx,
            &owner,
            serde_json::json!({"name": "ci", "expires_at": past}),
        )
        .await;

        assert!(
            output_is_error(out, "InvalidArgument").await,
            "{past} is an hour ago, whatever its offset makes it look like"
        );
        assert!(api_keys::list_for_user(&ctx, &owner)
            .await
            .unwrap()
            .is_empty());
    }

    /// A blank field is what an HTML form posts for "I did not fill this
    /// in", and omitting it means the same. Neither is an expiry.
    #[tokio::test]
    async fn create_reads_an_absent_or_blank_expiry_as_no_expiry() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let owner = seed_user(&ctx).await;

        for body in [
            serde_json::json!({"name": "omitted"}),
            serde_json::json!({"name": "blank", "expires_at": ""}),
            serde_json::json!({"name": "spaces", "expires_at": "  "}),
        ] {
            let out = create(&ctx, &owner, body).await;
            assert!(output_json(out).await["key"]
                .as_str()
                .is_some_and(|k| k.starts_with("sb_")));
        }
        let keys = api_keys::list_for_user(&ctx, &owner).await.unwrap();
        assert_eq!(keys.len(), 3);
        assert!(keys.iter().all(|k| k.expires_at.is_none()));
    }

    /// Same contract for `handle_delete`.
    #[tokio::test]
    async fn delete_reports_an_unreadable_key_row_as_an_outage() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let (owner, key_id) = seed_user_with_key(&ctx).await;
        let path = format!("/b/auth/api/api-keys/{key_id}");
        let failing = ctx.break_reads();

        let out = handle_delete(&failing, &routed(auth_msg("delete", &path, &owner))).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a failed ownership lookup must not answer 404: the key is still there"
        );
    }
}
