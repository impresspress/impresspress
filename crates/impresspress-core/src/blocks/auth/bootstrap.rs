//! First-run bootstrap for the `wafer-run/auth` block.
//!
//! On `Init`, if the `wafer_run__auth__users` table is empty, [`run`] picks
//! one of three paths based on which bootstrap env vars are set:
//!
//! 1. `BOOTSTRAP_ADMIN_EMAIL` + `BOOTSTRAP_ADMIN_PASSWORD` — hash the password
//!    via `wafer-run/crypto` and create an admin user + `local_credentials`.
//! 2. `BOOTSTRAP_ADMIN_TOKEN` — store sha256(token) in `bootstrap_tokens`
//!    with a 24h expiry. The holder later redeems it by presenting the raw
//!    token as a `Bearer` header (see `AuthServiceImpl::require_role`).
//! 3. None set — log and do nothing; leaves the operator to provision via UI
//!    / CLI.
//!
//! If the `users` table is non-empty, [`run`] is a no-op regardless of env —
//! bootstrap is a first-run mechanism only, never a "re-seed" trigger.

use wafer_core::clients::crypto;
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{
    config::AuthConfig,
    repo::{bootstrap_tokens, local_credentials, users},
    service::hash_token,
};

/// Run the bootstrap step. Idempotent: returns `Ok(())` without side-effects
/// when the `users` table is already populated.
pub async fn run(ctx: &dyn Context, cfg: &AuthConfig) -> Result<(), WaferError> {
    let user_count = users::count(ctx).await.map_err(internal)?;
    if user_count > 0 {
        tracing::debug!("auth: bootstrap skipped, users table already has {user_count} row(s)");
        return Ok(());
    }

    match (
        cfg.bootstrap_admin_email.as_deref(),
        cfg.bootstrap_admin_password.as_deref(),
        cfg.bootstrap_admin_token.as_deref(),
    ) {
        (Some(email), Some(password), _) => {
            bootstrap_with_email_password(ctx, email, password).await?;
            tracing::info!("auth: bootstrapped admin user: {email}");
        }
        (_, _, Some(token)) => {
            bootstrap_with_token(ctx, token).await?;
            tracing::info!("auth: bootstrap token installed; expires in 24h");
        }
        _ => {
            tracing::info!("auth: no bootstrap admin configured (users table empty)");
        }
    }
    Ok(())
}

pub(crate) async fn bootstrap_with_email_password(
    ctx: &dyn Context,
    email: &str,
    password: &str,
) -> Result<(), WaferError> {
    let hash = crypto::hash(ctx, password).await?;
    let id = uuid::Uuid::now_v7().to_string();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Write the admin user row directly with the union of Plan A2 columns
    // (display_name, role, email_verified) AND the legacy columns the rest
    // of impresspress still reads (`name`, `disabled`, `deleted_at`).
    // The single `db::create` call invokes the backend's `ensure_table`
    // which auto-adds any missing columns — so even when migration 001
    // already created the Plan A2 schema, this insert materializes the
    // legacy columns for downstream readers.
    //
    // Bypassing `repo::users::insert` here is intentional: bootstrap is a
    // one-shot operator action, not a steady-state user-creation flow.
    // Each legacy field is removed when its readers migrate to Plan A2:
    //   - `name` — userportal/profile.rs reads `display_name` first then
    //     `name`; admin pages still mix.
    //   - `disabled` / `deleted_at` — admin pages soft-delete + status.
    let mut data: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    data.insert("id".to_string(), serde_json::Value::String(id.clone()));
    data.insert(
        "email".to_string(),
        serde_json::Value::String(email.to_string()),
    );
    data.insert(
        "display_name".to_string(),
        serde_json::Value::String("Admin".to_string()),
    );
    data.insert(
        "avatar_url".to_string(),
        serde_json::Value::String(String::new()),
    );
    data.insert(
        "role".to_string(),
        serde_json::Value::String("admin".to_string()),
    );
    // Bootstrapped admins are inherently trusted — they're the operator who
    // set BOOTSTRAP_ADMIN_PASSWORD. Marking verified avoids the unverified
    // state on /b/userportal/security on first login.
    data.insert("email_verified".to_string(), serde_json::Value::Bool(true));
    data.insert(
        "created_at".to_string(),
        serde_json::Value::String(now.clone()),
    );
    data.insert("updated_at".to_string(), serde_json::Value::String(now));
    // Legacy companion columns — see comment block above.
    data.insert(
        "name".to_string(),
        serde_json::Value::String("Admin".to_string()),
    );
    data.insert("disabled".to_string(), serde_json::Value::Bool(false));
    data.insert("deleted_at".to_string(), serde_json::Value::Null);

    wafer_core::clients::database::create(ctx, users::TABLE, data).await?;
    local_credentials::insert(ctx, &id, &hash, false)
        .await
        .map_err(internal)?;
    Ok(())
}

async fn bootstrap_with_token(ctx: &dyn Context, token: &str) -> Result<(), WaferError> {
    let expires = chrono::Utc::now() + chrono::Duration::hours(24);
    let expires_iso = expires.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    bootstrap_tokens::insert(ctx, hash_token(token), &expires_iso)
        .await
        .map_err(internal)?;
    Ok(())
}

fn internal<E: std::fmt::Display>(e: E) -> WaferError {
    WaferError::new(ErrorCode::Internal, format!("auth bootstrap: {e}"))
}
