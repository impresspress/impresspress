//! /b/auth/api/api-keys — relocated from auth/api_keys.rs in Task 5.
//!
//! Admin user-management still calls these routes via htmx (see
//! `impresspress-core/src/blocks/admin/pages/users.rs`). PAT migration is a
//! follow-up; for PR 5 we relocate rather than delete.

use wafer_core::clients::crypto;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::auth::repo::api_keys,
    http::{err_bad_request, err_forbidden, err_internal, err_not_found, ok_json},
    util::{hex_encode, sha256_hex},
};

pub async fn handle_list(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id();
    if user_id.is_empty() {
        return crate::blocks::errors::error_response(
            crate::blocks::errors::ErrorCode::NotAuthenticated,
            "Authentication required",
        );
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
        Err(e) => err_internal("Database error", e.to_string()),
    }
}

pub async fn handle_create(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let user_id = msg.user_id();
    if user_id.is_empty() {
        return crate::blocks::errors::error_response(
            crate::blocks::errors::ErrorCode::NotAuthenticated,
            "Authentication required",
        );
    }

    #[derive(serde::Deserialize)]
    struct CreateKeyReq {
        name: String,
        expires_at: Option<String>,
    }
    let raw = input.collect_to_bytes().await;
    let parsed = crate::util::parse_body_value(&raw);
    let body: CreateKeyReq = match serde_json::from_value(parsed) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    if body.name.is_empty() {
        return err_bad_request("API key name is required");
    }

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
            expires_at: body.expires_at.as_deref(),
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
                // Inline JS handler for the copy button. The key text lives
                // in #new-api-key — read `innerText` (not the JS string) so
                // we never have to escape the key into a JS literal, and so
                // the button works even if the swap re-renders without the
                // original closure scope.
                let copy_js = "\
                    var el=document.getElementById('new-api-key');\
                    var t=el?el.innerText:'';\
                    if(t&&navigator.clipboard){\
                        navigator.clipboard.writeText(t).then(function(){\
                            var b=event.currentTarget;b.textContent='Copied';\
                            setTimeout(function(){b.textContent='Copy'},1500);\
                        });\
                    }";
                let markup = maud::html! {
                    div .card style="margin-bottom: var(--spacing-md)" {
                        div .card__head { h3 .card__title { "Key created — save it now" } }
                        div .card__body {
                            p style="margin:0 0 var(--spacing-sm); font-size: 13px; color: var(--text-secondary)" {
                                "This is the only time the full key will be shown. Copy it now."
                            }
                            div style="display:flex; gap: var(--spacing-sm); align-items: stretch" {
                                code #new-api-key style="flex:1 1 auto; padding: var(--spacing-sm); background: var(--bg-secondary); border-radius: var(--radius-md); font-family: ui-monospace, Menlo, monospace; font-size: 13px; word-break: break-all; user-select: all" {
                                    (key_for_display)
                                }
                                button type="button" .btn .btn-secondary .btn-sm
                                    style="flex: 0 0 auto"
                                    onclick=(copy_js)
                                { "Copy" }
                            }
                            p style="margin: var(--spacing-sm) 0 0; font-size: var(--text-xs); color: var(--text-muted)" {
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
        Err(e) => err_internal("Database error", e.to_string()),
    }
}

pub async fn handle_revoke(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let path = msg.path();
    let id = path.rsplit_once('/').map(|(_, id)| id).unwrap_or("");
    if id.is_empty() {
        return err_bad_request("Missing key ID");
    }
    let user_id = msg.user_id();

    // Verify ownership
    let Ok(Some(key)) = api_keys::find_by_id(ctx, id).await else {
        return err_not_found("API key not found");
    };
    if key.user_id != user_id && !crate::util::is_admin(msg) {
        return err_forbidden("Cannot revoke another user's API key");
    }

    match api_keys::revoke(ctx, id).await {
        Ok(_) => ok_json(&serde_json::json!({"message": "API key revoked"})),
        Err(e) => err_internal("Database error", e.to_string()),
    }
}

pub async fn handle_delete(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let path = msg.path();
    let id = path.rsplit_once('/').map(|(_, id)| id).unwrap_or("");
    if id.is_empty() {
        return err_bad_request("Missing key ID");
    }
    let user_id = msg.user_id();

    // Verify ownership
    let Ok(Some(key)) = api_keys::find_by_id(ctx, id).await else {
        return err_not_found("API key not found");
    };
    if key.user_id != user_id && !crate::util::is_admin(msg) {
        return err_forbidden("Cannot delete another user's API key");
    }

    match api_keys::delete(ctx, id).await {
        Ok(_) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) => err_internal("Database error", e.to_string()),
    }
}
