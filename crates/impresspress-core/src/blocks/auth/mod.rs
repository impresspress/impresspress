//! `wafer-run/auth` — service module.
//!
//! Plan A2 PR 5 split the old monolithic `AuthBlock` in two:
//!
//! - The framework `wafer_core::service_blocks::auth::AuthBlock` wraps
//!   `service::AuthServiceImpl` and owns the `wafer-run/auth` block id
//!   (registered via `crate::blocks::register_auth`). It has no HTTP routes.
//! - `crate::blocks::auth_ui::AuthUiBlock` owns every `/b/auth/*` HTTP
//!   route (login, signup, OAuth, API keys, settings, dashboard, orgs, …).
//!
//! What lives in this module after the split:
//!
//! - Module decls for the supporting layers (`bootstrap`, `config`,
//!   `migrations`, `repo`, `service`).
//! - Constants other blocks still reference (`AUTH_BLOCK_ID`, `JWT_SECRET_KEY`,
//!   the four `*_TABLE` re-exports from `repo/{api_keys,rate_limits,tokens,
//!   users}.rs`, `DUMMY_HASH`).
//! - `helpers` — token/cookie/role utilities consumed by `auth_ui::api::*`.
//! - `brand_panel` — shared UI panel consumed by `auth_ui::pages::*`.
//! - `authenticate_api_key` — called by `crate::pipeline` to populate auth
//!   meta from an `Authorization: Bearer <api-key>` header.

pub mod bootstrap;
pub mod config;
pub mod migrations;
pub mod repo;
pub mod service;

use std::{collections::HashMap, time::Duration};

use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::{config as config_client, crypto, database as db};

use crate::util::{hex_encode, json_map};

/// Refresh-token lifetime (7 days). Mirrored in [`helpers::generate_tokens`]
/// when signing the JWT and in [`helpers::store_refresh_token`] when writing
/// the row's `expires_at`. Centralised here so the two stay in lockstep.
pub(crate) const REFRESH_TOKEN_TTL_SECS: u64 = 604_800;

pub const AUTH_BLOCK_ID: &str = "wafer-run/auth";

/// Config key for the JWT signing secret used by the auth block.
/// Owner: the `wafer-run/auth` block. Read by the ImpresspressRouter
/// for token validation and by the Cloudflare adapter to seed the
/// crypto service.
pub const JWT_SECRET_KEY: &str = "WAFER_RUN__AUTH__JWT_SECRET";

// Cross-block table-name re-exports. Each auth table is owned by its repo
// module (`repo/users.rs`, `repo/tokens.rs`, etc.). These aliases keep
// existing crate-local consumers (admin/, userportal/, products/,
// rate_limit/, auth_ui/api/*) on stable identifiers without forcing them
// to import the qualified `repo::*::TABLE` path.
// Only consumer is `rate_limit::UserRateLimiter::check` on wasm32; native
// code path doesn't reference it. Re-export separately so we can attach
// the dead-code allow on the import binding.
#[allow(unused_imports)]
pub(crate) use repo::rate_limits::TABLE as RATE_LIMITS_TABLE;
pub(crate) use repo::{api_keys::TABLE as API_KEYS_TABLE, users::TABLE as USERS_TABLE};

/// Pre-computed Argon2id hash used for timing equalization when user is not found.
pub(crate) const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

use crate::blocks::admin::USER_ROLES_TABLE;

// --- Shared helpers used by auth_ui::api::* and auth_ui::oauth::* ---

/// Token / cookie / role / role-mint helpers shared by the auth_ui HTTP
/// handlers.
///
/// **`auth_method` values** stamped onto access + refresh JWTs (see
/// [`generate_tokens`]) — handlers that care about authentication strength
/// match on these strings:
/// - `"password"` — email + password login or signup.
/// - `"oauth.<provider>"` — OAuth callback. `<provider>` is one of
///   `google`, `github`, `microsoft`.
/// - `"bootstrap"` — bootstrap-token redemption (see [`bootstrap`]).
pub(crate) mod helpers {
    use super::*;

    /// Resolve `user_id`'s merged role set: the inline `users.role` (the
    /// bootstrap path) plus any rows in the legacy `USER_ROLES_TABLE`
    /// (multi-role history / admin-IAM grants), deduped since both can
    /// produce `"admin"` for the bootstrapped admin.
    ///
    /// Both reads propagate `Err` instead of swallowing it (SB-3): a WRAP
    /// denial or transient DB error on `USER_ROLES_TABLE` must not look
    /// identical to "user has no roles" — that would silently 403 every
    /// admin (`AuthServiceImpl::require_role`), re-insert a duplicate admin
    /// row on every login (`ensure_admin_role`), and stamp empty roles on
    /// API keys (`authenticate_api_key`). `NotFound` on the inline-role read
    /// is the one case that is genuinely "no role from this source", not a
    /// failure, and stays non-fatal.
    pub(crate) async fn get_user_roles(
        ctx: &dyn wafer_run::context::Context,
        user_id: &str,
    ) -> Result<Vec<String>, repo::RepoError> {
        use wafer_block::ErrorCode;

        use crate::util::RecordExt;

        let mut roles: Vec<String> = Vec::new();
        match db::get(ctx, USERS_TABLE, user_id).await {
            Ok(rec) => {
                let inline = rec.str_field("role");
                if !inline.is_empty() {
                    roles.push(inline.to_string());
                }
            }
            Err(e) if e.code == ErrorCode::NotFound => {}
            Err(e) => {
                return Err(repo::RepoError::Db(format!(
                    "get_user_roles: users lookup: {e}"
                )))
            }
        }

        let filters = vec![Filter {
            field: "user_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id.to_string()),
        }];
        let records = db::list_all(ctx, USER_ROLES_TABLE, filters)
            .await
            .map_err(|e| repo::RepoError::Db(format!("get_user_roles: roles table lookup: {e}")))?;
        for rec in &records {
            if let Some(role) = rec.data.get("role").and_then(|v| v.as_str()) {
                if !roles.iter().any(|r| r == role) {
                    roles.push(role.to_string());
                }
            }
        }
        Ok(roles)
    }

    /// Resolve user roles, idempotently granting `admin` if the user's email
    /// matches the configured `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL`
    /// and they don't already have it.
    ///
    /// This closes a real footgun: roles are normally only assigned at signup,
    /// so changing the configured admin email after a user already exists
    /// never elevates them. With this helper, every login re-checks the rule
    /// and grants admin once when appropriate.
    ///
    /// Intentionally **upgrade-only**: never removes a role, never demotes.
    /// Unsetting the admin email does not revoke admin from anyone — that has
    /// to be done explicitly via the admin UI / DB. Removing roles silently
    /// on login would be an availability foot-gun (one typo in env locks
    /// everyone out).
    ///
    /// Propagates [`repo::RepoError`] (SB-3) when the underlying roles read
    /// fails — a WRAP denial or DB error must not be mistaken for "user has
    /// no admin row yet" and drive a duplicate insert into `USER_ROLES_TABLE`.
    pub(crate) async fn ensure_admin_role(
        ctx: &dyn wafer_run::context::Context,
        user_id: &str,
        email: &str,
    ) -> Result<Vec<String>, repo::RepoError> {
        // Read the bootstrap-admin email *before* the role lookup. The
        // common case in production is "unset" — early-return then,
        // skipping the second `db::create` path entirely. Authenticated
        // routes mint tokens often enough that the saved DB reads accumulate.
        let admin_email =
            config_client::get_default(ctx, "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL", "")
                .await;

        let mut roles = get_user_roles(ctx, user_id).await?;

        if admin_email.is_empty()
            || !email.eq_ignore_ascii_case(&admin_email)
            || roles.iter().any(|r| r == "admin")
        {
            return Ok(roles);
        }

        // Email matches and admin role is missing — grant it.
        let role_data = json_map(serde_json::json!({
            "user_id": user_id,
            "role": "admin",
            "assigned_at": crate::util::now_rfc3339(),
        }));
        match db::create(ctx, USER_ROLES_TABLE, role_data).await {
            Ok(_) => {
                tracing::info!(
                    user_id = %user_id,
                    email = %email,
                    "granted admin role on login (email matches ADMIN_EMAIL)"
                );
                roles.push("admin".to_string());
            }
            Err(e) => {
                tracing::warn!(
                    user_id = %user_id,
                    "failed to grant admin role on login: {e}"
                );
            }
        }
        Ok(roles)
    }

    /// Whether new-account registration is allowed
    /// (`WAFER_RUN_SHARED__ALLOW_SIGNUP`, default on). The single signup toggle
    /// across the JSON signup endpoint and the OAuth callback's
    /// brand-new-user branch — `WAFER_RUN_SHARED__AUTH__SIGNUP_ENABLED` was a
    /// dead duplicate with the opposite default and has been removed.
    pub(crate) async fn signup_allowed(ctx: &dyn wafer_run::context::Context) -> bool {
        let raw = config_client::get_default(ctx, "WAFER_RUN_SHARED__ALLOW_SIGNUP", "true").await;
        raw == "true" || raw == "1"
    }

    /// Whether `email`'s domain is permitted to register.
    ///
    /// When `WAFER_RUN__AUTH__ALLOWED_EMAIL_DOMAINS` is unset (the default)
    /// every domain is allowed. When set to a comma-separated allow-list, only
    /// matching domains pass. `email` is expected pre-lowercased; the domain is
    /// the substring after the last `@` (empty for a malformed address, which
    /// then fails a non-empty allow-list).
    pub(crate) async fn email_domain_allowed(
        ctx: &dyn wafer_run::context::Context,
        email: &str,
    ) -> bool {
        let allowed =
            config_client::get_default(ctx, "WAFER_RUN__AUTH__ALLOWED_EMAIL_DOMAINS", "").await;
        if allowed.is_empty() {
            return true;
        }
        let domain = email.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
        allowed.split(',').any(|d| d.trim() == domain)
    }

    /// The role a newly registered user should receive: `"admin"` when `email`
    /// matches the configured `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL`,
    /// otherwise `"user"`. Shared by the JSON signup and OAuth-callback create
    /// paths so the bootstrap-admin rule can't drift between them.
    pub(crate) async fn initial_role_for(
        ctx: &dyn wafer_run::context::Context,
        email: &str,
    ) -> &'static str {
        use super::config::BOOTSTRAP_ADMIN_EMAIL_KEY;
        let admin_email = config_client::get_default(ctx, BOOTSTRAP_ADMIN_EMAIL_KEY, "").await;
        if !admin_email.is_empty() && email.eq_ignore_ascii_case(&admin_email) {
            "admin"
        } else {
            "user"
        }
    }

    /// Resolve the configured access-token lifetime (SEC-042). Reads
    /// `WAFER_RUN__AUTH__ACCESS_TOKEN_LIFETIME_SECS`; falls back to the
    /// declared default (30 min) if unset or unparseable.
    pub(crate) async fn access_token_lifetime_secs(ctx: &dyn wafer_run::context::Context) -> u64 {
        use super::config::{ACCESS_TOKEN_LIFETIME_SECS_DEFAULT, ACCESS_TOKEN_LIFETIME_SECS_KEY};
        let raw = config_client::get_default(ctx, ACCESS_TOKEN_LIFETIME_SECS_KEY, "").await;
        raw.parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(ACCESS_TOKEN_LIFETIME_SECS_DEFAULT)
    }

    /// Returns (access_token, refresh_token, family).
    ///
    /// `auth_method` records *how* the user authenticated for this token —
    /// `"password"` for email/password login or signup, `"oauth.<provider>"`
    /// for OAuth (e.g. `"oauth.github"`). The claim rides on both access and
    /// refresh tokens so it survives refresh, letting downstream gates (like
    /// the wafer registry's publish endpoint) require a stronger method.
    ///
    /// `family` selects the refresh-token rotation family for the SEC-039
    /// reuse-detection ladder: pass `None` to mint a brand-new family (initial
    /// login / signup / OAuth / bootstrap), or `Some(existing)` to re-issue
    /// within an established family on refresh rotation so the new refresh
    /// JWT's `family` claim agrees with the DB row that anchors reuse
    /// detection.
    ///
    /// Access tokens carry a random `jti` so logout can blocklist the
    /// in-flight JWT (SEC-042) without affecting other live sessions for
    /// the same user.
    pub(crate) async fn generate_tokens(
        ctx: &dyn wafer_run::context::Context,
        user_id: &str,
        email: &str,
        roles: &[String],
        auth_method: &str,
        family: Option<&str>,
    ) -> std::result::Result<(String, String, String), wafer_run::OutputStream> {
        let family = match family {
            Some(f) => f.to_string(),
            None => match crypto::random_bytes(ctx, 16).await {
                Ok(bytes) => hex_encode(&bytes),
                Err(e) => return Err(wafer_run::OutputStream::error(e)),
            },
        };
        // SEC-042: per-token random id so logout can revoke a single JWT
        // without touching other live sessions for the same user.
        let jti = match crypto::random_bytes(ctx, 16).await {
            Ok(bytes) => hex_encode(&bytes),
            Err(e) => return Err(wafer_run::OutputStream::error(e)),
        };

        let access_lifetime_secs = access_token_lifetime_secs(ctx).await;

        // [SEC-038] Stamp `iss` on every token we mint so the read side can
        // reject tokens minted by a different deployment (e.g. a sibling
        // env's leaked secret) instead of trusting any signature with the
        // same HMAC key.
        let issuer = expected_issuer(ctx).await;

        let mut access_claims = HashMap::new();
        access_claims.insert(
            "user_id".to_string(),
            serde_json::Value::String(user_id.to_string()),
        );
        access_claims.insert(
            "sub".to_string(),
            serde_json::Value::String(user_id.to_string()),
        );
        access_claims.insert(
            "email".to_string(),
            serde_json::Value::String(email.to_string()),
        );
        access_claims.insert("roles".to_string(), serde_json::json!(roles));
        access_claims.insert(
            "type".to_string(),
            serde_json::Value::String("access".to_string()),
        );
        access_claims.insert(
            "auth_method".to_string(),
            serde_json::Value::String(auth_method.to_string()),
        );
        access_claims.insert("jti".to_string(), serde_json::Value::String(jti));
        access_claims.insert("iss".to_string(), serde_json::Value::String(issuer.clone()));

        let access_token = crypto::sign(
            ctx,
            &access_claims,
            Duration::from_secs(access_lifetime_secs),
        )
        .await
        .map_err(wafer_run::OutputStream::error)?;

        let mut refresh_claims = HashMap::new();
        refresh_claims.insert(
            "user_id".to_string(),
            serde_json::Value::String(user_id.to_string()),
        );
        refresh_claims.insert(
            "sub".to_string(),
            serde_json::Value::String(user_id.to_string()),
        );
        refresh_claims.insert(
            "type".to_string(),
            serde_json::Value::String("refresh".to_string()),
        );
        refresh_claims.insert(
            "family".to_string(),
            serde_json::Value::String(family.clone()),
        );
        refresh_claims.insert(
            "auth_method".to_string(),
            serde_json::Value::String(auth_method.to_string()),
        );
        refresh_claims.insert("iss".to_string(), serde_json::Value::String(issuer.clone()));

        let refresh_token = crypto::sign(
            ctx,
            &refresh_claims,
            Duration::from_secs(super::REFRESH_TOKEN_TTL_SECS),
        )
        .await
        .map_err(wafer_run::OutputStream::error)?;

        Ok((access_token, refresh_token, family))
    }

    /// [SEC-038] Resolve the canonical JWT `iss` value for this deployment.
    ///
    /// `WAFER_RUN_SHARED__FRONTEND_URL` doubles as the issuer: it's the only
    /// per-deployment URL admins reliably set, and treating it as the issuer
    /// means a token minted in dev (`http://localhost:5173`) won't validate
    /// against a production secret if one leaks between environments.
    pub(crate) async fn expected_issuer(ctx: &dyn wafer_run::context::Context) -> String {
        config_client::get_default(
            ctx,
            "WAFER_RUN_SHARED__FRONTEND_URL",
            "http://localhost:5173",
        )
        .await
    }

    /// Persist a freshly minted refresh token.
    ///
    /// Stores only the SHA-256 hash of the raw JWT (SEC-032); the JWT itself
    /// never lands in the database. New families start at `generation = 0`;
    /// rotation from `auth_ui::api::refresh::handle` calls this with the same
    /// `family` and `generation = prev + 1` (SEC-039).
    pub(crate) async fn store_refresh_token(
        ctx: &dyn wafer_run::context::Context,
        user_id: &str,
        token: &str,
        family: &str,
        generation: i64,
    ) {
        let expires_at = (chrono::Utc::now()
            + chrono::Duration::seconds(super::REFRESH_TOKEN_TTL_SECS as i64))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
        if let Err(e) =
            super::repo::tokens::insert(ctx, user_id, token, family, generation, &expires_at).await
        {
            tracing::warn!("Failed to store refresh token: {e}");
        }
    }

    pub(crate) async fn build_auth_cookie(
        token: &str,
        max_age: u64,
        ctx: &dyn wafer_run::context::Context,
    ) -> String {
        let env =
            config_client::get_default(ctx, "WAFER_RUN_SHARED__ENVIRONMENT", "development").await;
        let secure = env.to_lowercase() != "development";
        format!(
            "auth_token={}; HttpOnly; Path=/; SameSite=Lax; Max-Age={}{}",
            token,
            max_age,
            if secure { "; Secure" } else { "" }
        )
    }

    /// Resolve the configured minimum signup password length
    /// (`WAFER_RUN_SHARED__AUTH__PASSWORD_MIN_LENGTH`). Falls back to the
    /// declared default (8) if unset or unparseable. Read by the signup
    /// handler so the admin-visible config var is actually enforced instead of
    /// a hardcoded literal.
    pub(crate) async fn password_min_length(ctx: &dyn wafer_run::context::Context) -> usize {
        use super::config::{PASSWORD_MIN_LENGTH_DEFAULT, PASSWORD_MIN_LENGTH_KEY};
        let raw = config_client::get_default(ctx, PASSWORD_MIN_LENGTH_KEY, "").await;
        raw.parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(PASSWORD_MIN_LENGTH_DEFAULT as usize)
    }

    /// Resolve the configured session-row lifetime in days
    /// (`WAFER_RUN_SHARED__AUTH__SESSION_LIFETIME_DAYS`). Falls back to the
    /// declared default if unset or unparseable. The session row is the
    /// userportal device-list signal; its expiry is independent of the JWT
    /// access-token lifetime (which is gated by [`access_token_lifetime_secs`]).
    pub(crate) async fn session_lifetime_days(ctx: &dyn wafer_run::context::Context) -> u32 {
        use super::config::{SESSION_LIFETIME_DAYS_DEFAULT, SESSION_LIFETIME_DAYS_KEY};
        let raw = config_client::get_default(ctx, SESSION_LIFETIME_DAYS_KEY, "").await;
        raw.parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(SESSION_LIFETIME_DAYS_DEFAULT)
    }

    /// Outcome of [`issue_tokens_and_cookie`]: the freshly minted token pair,
    /// the access-token lifetime (seconds) and the ready-to-set `auth_token`
    /// cookie. Callers add only their response shape (JSON body vs. 302
    /// redirect). The rotation family is persisted internally (on the refresh
    /// row); no caller needs it back, so it is intentionally not surfaced here.
    pub(crate) struct IssuedLogin {
        pub access_token: String,
        pub refresh_token: String,
        pub access_lifetime: u64,
        pub cookie: String,
    }

    /// Shared token-issuance tail for every login flow (password login, signup,
    /// bootstrap redemption, OAuth callback, and refresh rotation).
    ///
    /// Mints the access + refresh JWTs, persists the refresh-token row, writes
    /// the userportal session row, and builds the `auth_token` cookie — the
    /// exact sequence that was previously copy-pasted across all five handlers
    /// (and which the OAuth copy had drifted from, silently omitting the
    /// session row). Centralising it guarantees every authentication path is
    /// visible on the userportal device list.
    ///
    /// `family` follows [`generate_tokens`]: `None` mints a brand-new rotation
    /// family (initial authentication), `Some(existing)` re-issues within an
    /// established family (refresh rotation). `generation` is the refresh-row
    /// generation to persist (`0` for a new family, `prev + 1` on rotation).
    ///
    /// The session-row write failing does not abort issuance — it is a UX
    /// signal, not a security gate (auth is entirely JWT-based today) — but it
    /// is logged.
    pub(crate) async fn issue_tokens_and_cookie(
        ctx: &dyn wafer_run::context::Context,
        user_id: &str,
        email: &str,
        roles: &[String],
        auth_method: &str,
        family: Option<&str>,
        generation: i64,
    ) -> std::result::Result<IssuedLogin, wafer_run::OutputStream> {
        use super::{repo::sessions, service::hash_token};

        let (access_token, refresh_token, issued_family) =
            generate_tokens(ctx, user_id, email, roles, auth_method, family).await?;

        store_refresh_token(ctx, user_id, &refresh_token, &issued_family, generation).await;

        let lifetime_days = session_lifetime_days(ctx).await;
        if let Err(e) =
            sessions::create_for_user(ctx, user_id, hash_token(&access_token), lifetime_days).await
        {
            tracing::warn!(
                user_id = %user_id,
                auth_method = %auth_method,
                "failed to persist session row for login: {e}"
            );
        }

        let access_lifetime = access_token_lifetime_secs(ctx).await;
        let cookie = build_auth_cookie(&access_token, access_lifetime, ctx).await;

        Ok(IssuedLogin {
            access_token,
            refresh_token,
            access_lifetime,
            cookie,
        })
    }
}

/// Authenticate a request using an API key.
///
/// Hashes the key with SHA-256, looks it up in the database by key_hash,
/// checks it's not revoked/expired, and sets auth meta on the message.
/// Silently does nothing if the key is invalid (request continues as
/// unauthenticated), matching JWT behavior.
pub async fn authenticate_api_key(
    ctx: &dyn wafer_run::context::Context,
    api_key: &str,
    msg: &mut wafer_run::Message,
) {
    use wafer_run::*;

    use crate::util::sha256_hex;

    let key_hash = sha256_hex(api_key.as_bytes());

    // Look up by key_hash via the typed api_keys repo. A real DB error (WRAP
    // denial, connection blip) would otherwise silently demote the request to
    // anonymous — that's still the right fallback for availability, but it
    // must be observable, so the repo logs and returns None-equivalent here.
    let key_row = match repo::api_keys::find_by_key_hash(ctx, &key_hash).await {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("authenticate_api_key: lookup failed: {e}");
            return;
        }
    };

    // Reject revoked or expired keys.
    if key_row.is_revoked() {
        return;
    }
    if key_row.is_expired(&crate::util::now_rfc3339()) {
        return;
    }

    // Look up the user to get email and roles.
    if key_row.user_id.is_empty() {
        return;
    }
    let user = match repo::users::find_by_id(ctx, &key_row.user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(user_id = %key_row.user_id, "authenticate_api_key: user lookup failed: {e}");
            return;
        }
    };

    // Deleted or disabled accounts must not authenticate, even with a
    // still-valid API key. Login/refresh/OAuth already enforce this on their
    // own row loads; this is the same gate for the key path.
    if !user.is_active() {
        return;
    }

    // Fetch roles from user_roles collection (roles are not stored on the
    // user record). Mirrors the key_row/user lookups above: a real DB error
    // (WRAP denial, connection blip) must not silently stamp an empty/wrong
    // roles list on an otherwise-valid key — log it and fall back to
    // anonymous instead (SB-3).
    let roles = match helpers::get_user_roles(ctx, &key_row.user_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(user_id = %key_row.user_id, "authenticate_api_key: roles lookup failed: {e}");
            return;
        }
    };
    let roles_str = roles.join(",");

    // Set auth meta (same fields as JWT auth)
    msg.set_meta(META_AUTH_USER_ID, &key_row.user_id);
    msg.set_meta(META_AUTH_USER_EMAIL, &user.email);
    msg.set_meta(META_AUTH_USER_ROLES, &roles_str);
}

use crate::ui::{templates::BrandPanel, SiteConfig};

/// Shared brand panel used by `auth_ui::pages::*` (login / signup / reset /
/// OAuth / change-password / bootstrap).
/// Shared auth-split brand panel. `tagline` is page-specific — every caller
/// passes copy that matches what the page actually does (e.g. "Sign in to
/// continue." on the login page, "Create your account." on signup); it used
/// to be hardcoded to the login copy and rendered unchanged on signup,
/// bootstrap, password-reset, and verify pages too.
pub(crate) fn brand_panel<'a>(config: &'a SiteConfig, tagline: &'a str) -> BrandPanel<'a> {
    BrandPanel {
        logo_html: None,
        headline: &config.app_name,
        tagline: Some(tagline),
    }
}

#[cfg(test)]
mod api_key_lifecycle_tests {
    use wafer_run::{Message, META_AUTH_USER_ID};

    use super::{
        authenticate_api_key,
        repo::{api_keys, users},
    };
    use crate::{test_support::TestContext, util::sha256_hex};

    async fn seed_user_and_key(ctx: &TestContext, raw_key: &str) -> String {
        let user = users::insert(
            ctx,
            users::NewUser {
                email: "key@e.co".into(),
                display_name: "Key".into(),
                avatar_url: None,
                role: "user".into(),
            },
        )
        .await
        .unwrap();
        let key_hash = sha256_hex(raw_key.as_bytes());
        api_keys::insert(
            ctx,
            api_keys::NewApiKey {
                user_id: &user.id,
                name: "test-key",
                key_hash: &key_hash,
                key_prefix: "sb_test",
                expires_at: None,
            },
        )
        .await
        .unwrap();
        user.id
    }

    #[tokio::test]
    async fn active_user_key_authenticates() {
        // SB-3: `get_user_roles` now surfaces (rather than swallows) a
        // denied read of the admin-owned USER_ROLES_TABLE, so this WRAP
        // fixture must carry the real grant admin declares for the auth
        // block (`ResourceGrant::read_write(AUTH_BLOCK_ID, USER_ROLES_TABLE)`
        // in `blocks/admin/mod.rs`) — sourced from the real block so the
        // fixture can't drift from production.
        use wafer_run::Block;

        use crate::blocks::admin::AdminBlock;

        let grants = AdminBlock::new().info().grants;
        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            grants,
            "impresspress/admin",
        );
        let uid = seed_user_and_key(&ctx, "raw-active-key").await;

        let mut msg = Message::new("http");
        authenticate_api_key(&ctx, "raw-active-key", &mut msg).await;
        assert_eq!(msg.get_meta(META_AUTH_USER_ID), uid);
    }

    #[tokio::test]
    async fn disabled_user_key_is_rejected() {
        use wafer_core::clients::database as db;

        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            vec![],
            "impresspress/admin",
        );
        let uid = seed_user_and_key(&ctx, "raw-disabled-key").await;

        let mut patch = std::collections::HashMap::new();
        patch.insert("disabled".to_string(), serde_json::json!(true));
        db::update(&ctx, users::TABLE, &uid, patch).await.unwrap();

        let mut msg = Message::new("http");
        authenticate_api_key(&ctx, "raw-disabled-key", &mut msg).await;
        // No auth meta stamped → request stays anonymous.
        assert_eq!(msg.get_meta(META_AUTH_USER_ID), "");
    }
}

// SB-3: `get_user_roles` used to swallow both DB reads with `if let
// Ok(...)`, so a WRAP-grant regression or transient DB error yielded an
// empty/partial roles list indistinguishable from "user genuinely has no
// roles" — silently 403ing every admin (`require_role`), re-inserting a
// duplicate admin row on every login (`ensure_admin_role`), and stamping
// empty roles on API keys (`authenticate_api_key`). These tests pin the
// fix: a denied/failed roles read is now an `Err`, not an empty `Vec`.
#[cfg(test)]
mod get_user_roles_error_surfacing_tests {
    use super::helpers::{ensure_admin_role, get_user_roles};
    use crate::test_support::TestContext;

    #[tokio::test]
    async fn denied_roles_table_read_is_an_error_not_empty_roles() {
        // Auth owns `wafer_run__auth__users` (Rule 3 own-resource — always
        // reachable) but not `impresspress__admin__user_roles` (admin-owned).
        // In production, admin's own block-level grant
        // (`ResourceGrant::read_write(AUTH_BLOCK_ID, USER_ROLES_TABLE)` in
        // `blocks/admin/mod.rs`) makes that read succeed; passing no grants
        // here simulates that grant regressing/missing.
        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            Vec::new(),
            "impresspress/admin",
        );

        let res = get_user_roles(&ctx, "some-user-id").await;
        assert!(
            res.is_err(),
            "a denied/failed roles read must be an Err, not empty roles"
        );
    }

    #[tokio::test]
    async fn ensure_admin_role_propagates_denied_roles_read_instead_of_inserting() {
        // If the roles-table read fails, `ensure_admin_role` must not
        // silently treat that as "no admin row yet" and insert a duplicate
        // — it must propagate the error and skip the insert entirely.
        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            Vec::new(),
            "impresspress/admin",
        );

        let res = ensure_admin_role(&ctx, "some-user-id", "admin@example.com").await;
        assert!(
            res.is_err(),
            "ensure_admin_role must propagate a denied roles read instead of \
             proceeding to (possibly duplicate-)insert the admin grant"
        );
    }
}
