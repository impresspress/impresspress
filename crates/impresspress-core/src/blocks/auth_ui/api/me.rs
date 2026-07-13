//! GET / UPDATE /b/auth/api/me — relocated from auth/login.rs in Task 5.

use std::collections::HashMap;

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::{
        auth::{helpers::get_user_roles, repo::users},
        errors::{error_response, ErrorCode},
    },
    http::{err_bad_request, err_internal, err_not_found, ok_json},
};

pub async fn handle_get(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id();
    if user_id.is_empty() {
        return error_response(ErrorCode::NotAuthenticated, "Not authenticated");
    }
    let Ok(Some(user)) = users::find_by_id(ctx, user_id).await else {
        return err_not_found("User not found");
    };
    let roles = match get_user_roles(ctx, user_id).await {
        Ok(r) => r,
        Err(e) => return err_internal("Failed to resolve user roles", e),
    };
    ok_json(&serde_json::json!({
        "user": {
            "id": user.id,
            "email": user.email,
            "name": user.display_name,
            "roles": roles,
            "created_at": user.created_at,
            "avatar_url": user.avatar_url.unwrap_or_default()
        }
    }))
}

pub async fn handle_update(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let user_id = msg.user_id();
    if user_id.is_empty() {
        return error_response(ErrorCode::NotAuthenticated, "Not authenticated");
    }

    let raw = input.collect_to_bytes().await;
    let body: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Only `name` and `avatar_url` are user-editable. `name` dual-writes
    // display_name + the legacy name alias inside update_profile.
    let name = body.get("name").and_then(|v| v.as_str());
    let avatar_url = body.get("avatar_url").and_then(|v| v.as_str());

    match users::update_profile(ctx, user_id, name, avatar_url).await {
        Ok(user) => {
            let roles = match get_user_roles(ctx, user_id).await {
                Ok(r) => r,
                Err(e) => return err_internal("Failed to resolve user roles", e),
            };
            ok_json(&serde_json::json!({
                "id": user.id,
                "email": user.email,
                "name": user.display_name,
                "roles": roles
            }))
        }
        Err(e) => err_internal("Update failed", e.to_string()),
    }
}
