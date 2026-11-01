//! First-run bootstrap for the `wafer-run/auth` block.
//!
//! On `Init`, if the `wafer_run__auth__users` table is empty, [`run`] picks
//! one of these paths based on which bootstrap vars are set:
//!
//! 1. `BOOTSTRAP_ADMIN_EMAIL` + `BOOTSTRAP_ADMIN_PASSWORD` — hash the password
//!    via `wafer-run/crypto` and create the admin account, its `admin`
//!    grant, its `local_credentials` row and its address proof
//!    ([`create_admin`]).
//! 2. `BOOTSTRAP_ADMIN_TOKEN` — store sha256(token) in `bootstrap_tokens`
//!    with a 24h expiry, unless that token's row is already there. The holder
//!    later redeems it at `POST /b/auth/api/bootstrap`
//!    (`auth_ui::api::bootstrap`), which creates the admin through
//!    [`create_admin`] too.
//! 3. `BOOTSTRAP_ADMIN_EMAIL` alone — refused: `Init` fails. Nothing would
//!    create the first admin, and no sign-in grants a role from the email.
//! 4. Nothing set, or a password without an email — log and do nothing;
//!    leaves the operator to provision with a token.
//!
//! These are the only two ways an admin is created without an admin: no
//! signup, login, refresh or OAuth sign-in reads the bootstrap email.
//!
//! If the `users` table is non-empty, [`run`] is a no-op regardless of
//! config — bootstrap is a first-run mechanism only, never a "re-seed"
//! trigger. So every one of these values is spent once an account exists,
//! and a later settings edit cannot fail a running site's `Init`.
//!
//! # When it runs
//!
//! Auth's `Init` calls [`run`] when [`should_run`] says so:
//!
//! - on a **provisioning build** (`migration_helper::is_provisioning_build`):
//!   each native start, each browser build, Cloudflare's deploy funnel;
//! - when auth's own migrations ran at this `Init`
//!   (`MigrationOutcome::Applied`): a fresh auth schema, even on Cloudflare's
//!   request path;
//! - and on **any** build while a bootstrap key is configured
//!   ([`AuthConfig::names_a_bootstrap_admin`]). That is how a credential
//!   configured after the first deploy still bootstraps: on Cloudflare the
//!   Worker captures no bootstrap key of its own, so it arrives from a
//!   forwarding consumer's `request_config`
//!   (`builder::request_path::add_request_config`), lands on the request's
//!   boot map, and the next cold isolate's `Init` reads it. It costs that
//!   isolate one `users` row read ([`users::any_exists`], plus the table's
//!   column introspection on a cold schema cache) for as long as the key
//!   stays bound, plus, on the token path with no account yet, the token
//!   row's `INSERT … ON CONFLICT DO NOTHING` until the token is redeemed —
//!   so clear the spent key on the consumer (`wrangler secret delete`).
//!
//! A Cloudflare cold isolate with no bootstrap key configured reads nothing
//! from `users`. "Has the bootstrap happened" is not stamped anywhere: the
//! `users` table is the fact, and a stamp written from the request path
//! would bump the fleet's config version.
//!
//! The token step installs a token once, on the first `Init` that sees it
//! with no account yet, and every later one finds that row and leaves it alone
//! ([`bootstrap_tokens::insert_if_absent`]). The token stays redeemable for
//! 24 hours from that first install, however often the runtime is rebuilt;
//! a token that expired unredeemed stays expired, and the operator
//! configures a different one.
//!
//! Configuring a different token installs a row of its own and does not
//! revoke the earlier one. That is a known gap, not a design: until the
//! earlier row expires it can still be redeemed — even after another token
//! has created the admin, minting a second admin — and it still answers
//! `AuthServiceImpl::require_role(Admin)` as a bearer. `run` does not revoke
//! on rotation, because an isolate still on the old config during a rollout
//! would delete the new row and re-install its own; the fix belongs to
//! redemption (spend every row once the first account exists) and to
//! `require_role` (honour a token only while `users` is empty).

use wafer_core::clients::crypto;
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{
    config::{
        AuthConfig, BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY,
        BOOTSTRAP_ADMIN_TOKEN_KEY,
    },
    repo::{bootstrap_tokens, local_credentials, users},
    service::hash_token,
};
use crate::migration_helper::{self, MigrationOutcome};

/// Why [`run`] did not complete.
///
/// Two variants because `Init` answers them differently: a misconfiguration
/// is the operator's to fix and is reported in its own words, while a
/// failed read or write is a backend fault, masked and logged like every
/// other (`service::backend_error`).
#[derive(Debug)]
pub enum RunError {
    /// The configured values cannot create the first admin. The error is
    /// `InvalidArgument`, and its message names the keys to set.
    Misconfigured(WaferError),
    /// Counting the users or provisioning the admin failed.
    Failed(WaferError),
}

/// Whether auth's `Init` runs [`run`] at all: on a provisioning build or an
/// `Init` whose migrations ran now (`migration_helper::provisions`), or
/// whenever a bootstrap key is configured (module docs, "When it runs").
pub fn should_run(ctx: &dyn Context, outcome: MigrationOutcome, cfg: &AuthConfig) -> bool {
    migration_helper::provisions(ctx, outcome) || cfg.names_a_bootstrap_admin()
}

/// Run the bootstrap step. Idempotent: returns `Ok(())` without side-effects
/// when the `users` table already holds an account, which it asks with one
/// row read ([`users::any_exists`]).
pub async fn run(ctx: &dyn Context, cfg: &AuthConfig) -> Result<(), RunError> {
    if users::any_exists(ctx)
        .await
        .map_err(|e| RunError::Failed(bootstrap_failed(e)))?
    {
        tracing::debug!("auth: bootstrap skipped, the users table already has an account");
        return Ok(());
    }

    match (
        cfg.bootstrap_admin_email.as_deref(),
        cfg.bootstrap_admin_password.as_deref(),
        cfg.bootstrap_admin_token.as_deref(),
    ) {
        (Some(email), Some(password), _) => {
            bootstrap_with_email_password(ctx, email, password)
                .await
                .map_err(RunError::Failed)?;
            tracing::info!("auth: bootstrapped admin user: {email}");
        }
        (_, _, Some(token)) => {
            if bootstrap_with_token(ctx, token)
                .await
                .map_err(RunError::Failed)?
            {
                tracing::info!("auth: bootstrap token installed; expires in 24h");
            } else {
                tracing::debug!(
                    "auth: bootstrap token already installed; its first install's expiry stands"
                );
            }
        }
        // The operator named an admin and nothing would create it: no
        // sign-in grants a role from the email. Booting would leave the site
        // without the admin its operator configured, so `Init` fails and
        // says what is missing.
        (Some(_), None, None) => {
            return Err(RunError::Misconfigured(WaferError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "{BOOTSTRAP_ADMIN_EMAIL_KEY} is set without {BOOTSTRAP_ADMIN_PASSWORD_KEY} \
                     or {BOOTSTRAP_ADMIN_TOKEN_KEY}; nothing would create the first admin"
                ),
            )));
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
/// `password_hash`: the account, its credential and its address proof
/// (`proof::BOOTSTRAP`) in one write. The redemption route
/// (`auth_ui::api::bootstrap`) hashes first itself, so a crypto outage is
/// answered before the single-use token is spent.
pub(crate) async fn create_admin(
    ctx: &dyn Context,
    email: &str,
    password_hash: &str,
) -> Result<(), WaferError> {
    // Stored as signup stores it. `BOOTSTRAP_ADMIN_EMAIL` is whatever the
    // operator typed, and a mixed-case row is one no login finds (they look
    // the normalized address up) and one signup does not see as taken.
    let email = users::normalize_email(email);

    // The account, its `admin` grant, its password and its proof in one
    // atomic write. As separate writes, a failure between them left an admin
    // with no password, or an account that is not an admin — and since `run`
    // only bootstraps an EMPTY users table, no later boot would repair it.
    // The grant is an ordinary `user_roles` row, so another admin can remove
    // it through IAM like any other.
    //
    // A bootstrapped admin is inherently trusted: the operator chose this
    // address in config, or the holder of the operator's single-use token
    // typed it. So it is verified on creation (without that,
    // /b/userportal/security would show them unverified on first login), and
    // its address counts as proven, naming the act (`proof::BOOTSTRAP`). The
    // proof is what lets the admin later sign in with a provider that asserts
    // this address: the OAuth callback adopts only a proven account. A
    // provider that does not assert it is still refused there.
    users::insert_with_ops(
        ctx,
        users::NewUser {
            email,
            display_name: "Admin".to_string(),
            avatar_url: None,
            email_verified: true,
            verification_token_hash: None,
        },
        users::InitialRole::Admin,
        |id| {
            vec![
                local_credentials::create_op(id, password_hash, false),
                users::record_email_proof_op(id, users::proof::BOOTSTRAP),
            ]
        },
    )
    .await
    .map_err(bootstrap_failed)?;
    Ok(())
}

/// Install `token` for 24 hours from now, unless it is installed already.
/// `true` when this call installed it.
async fn bootstrap_with_token(ctx: &dyn Context, token: &str) -> Result<bool, WaferError> {
    let expires = chrono::Utc::now() + chrono::Duration::hours(24);
    let expires_iso = expires.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    bootstrap_tokens::insert_if_absent(ctx, &hash_token(token), &expires_iso)
        .await
        .map_err(bootstrap_failed)
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
            test_faults::{drop_trigger, fail_inserts_into, fail_updates_of},
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

    /// The address proof is part of the same write: a bootstrapped admin
    /// without it could never be adopted by an OAuth provider that asserts
    /// its address, and since `run` bootstraps only an empty users table, no
    /// later boot would add it. The proof is the batch's last statement, an
    /// update of the row the first one created, so failing it must undo the
    /// account and its password too.
    #[tokio::test]
    async fn a_failed_proof_write_leaves_the_table_empty_for_the_next_boot() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let trigger = fail_updates_of(&ctx, users::TABLE).await;

        bootstrap_with_email_password(&ctx, "admin@example.com", "correct-horse-battery")
            .await
            .expect_err("the proof write fails");
        assert_eq!(
            users::count(&ctx).await.expect("count users"),
            0,
            "no admin row may outlive its failed proof write"
        );

        drop_trigger(&ctx, &trigger).await;
        bootstrap_with_email_password(&ctx, "admin@example.com", "correct-horse-battery")
            .await
            .expect("the next attempt bootstraps");
        let admin = users::find_by_email(&ctx, "admin@example.com")
            .await
            .expect("lookup")
            .expect("the admin exists");
        assert_eq!(
            admin.email_verified_by.as_deref(),
            Some(users::proof::BOOTSTRAP),
            "and its address is proven by the bootstrap"
        );
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
                &crate::test_support::json_post_msg("/b/auth/api/login"),
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
