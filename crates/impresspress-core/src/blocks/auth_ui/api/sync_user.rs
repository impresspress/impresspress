//! POST /b/auth/api/oauth/sync-user — relocated from auth/login.rs in Task 5.

use wafer_core::clients::config;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::auth::repo::users,
    http::{err_bad_request, err_forbidden, err_internal, err_unauthorized, ok_json},
};

pub async fn handle(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    // Internal endpoint for OAuth user sync — requires INTERNAL_SECRET
    let expected_secret = config::get_default(ctx, "WAFER_RUN__AUTH__INTERNAL_SECRET", "").await;
    if expected_secret.is_empty() {
        return err_forbidden("INTERNAL_SECRET not configured — internal endpoints are disabled");
    }
    let provided_secret = msg.header("x-internal-secret");
    if !wafer_block_crypto::primitives::constant_time_eq(
        provided_secret.as_bytes(),
        expected_secret.as_bytes(),
    ) {
        return err_unauthorized("Invalid internal secret");
    }

    // `provider` may still be present in the request body; serde ignores
    // unknown fields, and the old `oauth_provider` column it fed had no
    // readers, so it is intentionally not deserialized here.
    #[derive(serde::Deserialize)]
    struct SyncReq {
        email: String,
        name: Option<String>,
    }
    let raw = input.collect_to_bytes().await;
    let body: SyncReq = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    let email_lower = body.email.trim().to_lowercase();
    // `find_by_email` maps NOT_FOUND → Ok(None) and surfaces every other
    // backend error (WRAP denial, connection blip) as Err — collapsing those
    // to "user not found" would race a duplicate insert past the unique-email
    // constraint and corrupt the table.
    let user = match users::find_by_email(ctx, &email_lower).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            // Create through the typed insert so the row matches every other
            // user row (id, dual display_name+name write, role=user default).
            // The provider arg is intentionally dropped: the old `oauth_provider`
            // column had no readers anywhere in the workspace.
            match users::insert(
                ctx,
                users::NewUser {
                    email: email_lower.clone(),
                    display_name: body.name.unwrap_or_default(),
                    avatar_url: None,
                    role: "user".to_string(),
                },
            )
            .await
            {
                Ok(u) => u,
                Err(e) => return err_internal("Create failed", e.to_string()),
            }
        }
        Err(e) => return err_internal("User lookup failed", e.to_string()),
    };

    ok_json(&serde_json::json!({"id": user.id, "email": user.email}))
}
