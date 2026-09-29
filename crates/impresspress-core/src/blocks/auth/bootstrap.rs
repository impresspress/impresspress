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
use wafer_run::{context::Context, WaferError};

use super::{
    config::AuthConfig,
    repo::{bootstrap_tokens, users},
    service::hash_token,
};

/// Run the bootstrap step. Idempotent: returns `Ok(())` without side-effects
/// when the `users` table is already populated.
pub async fn run(ctx: &dyn Context, cfg: &AuthConfig) -> Result<(), WaferError> {
    let user_count = users::count(ctx).await.map_err(bootstrap_failed)?;
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
    create_admin(ctx, email, &hash).await
}

/// Create the verified admin account `email` whose password hashes to
/// `password_hash`, the account and its credential in one write. The
/// redemption route (`auth_ui::api::bootstrap`) hashes first itself, so a
/// crypto outage is answered before the single-use token is spent.
pub(crate) async fn create_admin(
    ctx: &dyn Context,
    email: &str,
    password_hash: &str,
) -> Result<(), WaferError> {
    // Stored as signup stores it. `BOOTSTRAP_ADMIN_EMAIL` is whatever the
    // operator typed, and a mixed-case row is one no login finds (they look
    // the normalized address up) and one signup does not see as taken.
    let email = users::normalize_email(email);

    // The account and its password in one atomic write. As two writes, a
    // failure between them left an admin with no password — and since `run`
    // only bootstraps an EMPTY users table, no later boot would repair it.
    //
    // A bootstrapped admin is inherently trusted (the operator set
    // BOOTSTRAP_ADMIN_PASSWORD), so it is verified on creation; without
    // that, /b/userportal/security would show them unverified on first
    // login.
    users::insert_with_password(
        ctx,
        users::NewUser {
            email,
            display_name: "Admin".to_string(),
            avatar_url: None,
            role: "admin".to_string(),
            email_verified: true,
            verification_token_hash: None,
        },
        password_hash,
    )
    .await
    .map_err(bootstrap_failed)?;
    Ok(())
}

async fn bootstrap_with_token(ctx: &dyn Context, token: &str) -> Result<(), WaferError> {
    let expires = chrono::Utc::now() + chrono::Duration::hours(24);
    let expires_iso = expires.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    bootstrap_tokens::insert(ctx, hash_token(token), &expires_iso)
        .await
        .map_err(bootstrap_failed)?;
    Ok(())
}

/// `error` prefixed with this step, keeping its code: a WRAP denial on the
/// redemption route (`auth_ui::api::bootstrap`) is a 403 there, not a 500.
fn bootstrap_failed(error: WaferError) -> WaferError {
    super::repo::db_failed("auth bootstrap", error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::auth::repo::{
            local_credentials,
            test_faults::{drop_trigger, fail_inserts_into},
        },
        test_support::TestContext,
    };

    /// The admin and its password are one write. As two, a failure between
    /// them left an admin with no password in a users table that is no
    /// longer empty — and `run` bootstraps only an empty one, so no later
    /// boot could repair it.
    #[tokio::test]
    async fn a_failed_password_write_leaves_the_table_empty_for_the_next_boot() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let trigger = fail_inserts_into(&ctx, local_credentials::TABLE).await;

        bootstrap_with_email_password(&ctx, "admin@example.com", "correct-horse-battery")
            .await
            .expect_err("the password write fails");
        assert_eq!(
            users::count(&ctx).await.expect("count users"),
            0,
            "no admin row may outlive its failed password write"
        );

        drop_trigger(&ctx, &trigger).await;
        bootstrap_with_email_password(&ctx, "admin@example.com", "correct-horse-battery")
            .await
            .expect("the next attempt bootstraps");
        let admin = users::find_by_email(&ctx, "admin@example.com")
            .await
            .expect("lookup")
            .expect("the admin exists");
        assert!(
            local_credentials::has_password(&ctx, &admin.id)
                .await
                .expect("credentials lookup"),
            "and it has its password"
        );
    }

    /// `BOOTSTRAP_ADMIN_EMAIL` as the operator typed it, mixed case and
    /// padded: the admin is stored under the normalized address, so the
    /// login (which normalizes what it is given) finds it and signup sees
    /// the address as taken.
    #[tokio::test]
    async fn a_mixed_case_bootstrap_address_is_stored_normalized() {
        let ctx = TestContext::with_auth_and_crypto().await;
        bootstrap_with_email_password(&ctx, " Admin@Example.COM ", "correct-horse-battery")
            .await
            .expect("bootstrap");

        assert!(
            users::find_by_email(&ctx, "admin@example.com")
                .await
                .expect("lookup")
                .is_some(),
            "the admin is stored under the normalized address"
        );
        let login = serde_json::json!({
            "email": "Admin@Example.COM",
            "password": "correct-horse-battery",
        });
        let signed_in = crate::test_support::output_json(
            crate::blocks::auth_ui::api::login::handle(
                &ctx,
                wafer_run::InputStream::from_bytes(serde_json::to_vec(&login).expect("body")),
            )
            .await,
        )
        .await;
        assert!(
            signed_in["access_token"].is_string(),
            "the bootstrapped admin can sign in: {signed_in}"
        );
    }
}
