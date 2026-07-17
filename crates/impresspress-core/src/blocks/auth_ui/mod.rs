//! `impresspress/auth-ui` — SSR pages + JSON API + OAuth flows + bootstrap token
//! redemption for impresspress auth.
//!
//! Plan A2 PR 5 splits the legacy `wafer-run/auth` block into two halves:
//!
//! - **Framework auth** (`wafer-run/auth`, lives in `wafer-run` proper):
//!   service-shaped block exposing `auth@v1` (`require_user`/`require_role`/
//!   token issue+verify). Owns `JWT_SECRET`, `REQUIRE_VERIFICATION`,
//!   `ALLOWED_EMAIL_DOMAINS`, `INTERNAL_SECRET`. No HTTP routes.
//!
//! - **auth-ui** (this module): all `/b/auth/*` HTTP routes. Reads/writes
//!   auth tables via `repo::*` under WRAP grant. Calls the framework auth
//!   block via the `auth@v1` typed client for identity primitives.
//!
//! Declares the full `BlockInfo` (endpoints, requires,
//! OAuth-creds config_keys), runs the per-user/IP rate-limit middleware,
//! and dispatches every `/b/auth/*` route to a leaf module under `api/`,
//! `pages/`, or `oauth/`. The framework `wafer-run/auth` block (in
//! `auth/`) owns the auth *service*; this block owns the HTTP surface.

pub mod api;
pub mod oauth;
pub mod pages;
pub mod redirect;

use wafer_run::{AuthLevel, BlockEndpoint, BlockInfo, ConfigVar, InputType, InstanceMode};

use super::rate_limit::{
    check_route_limits, LimitKey, RateLimit, RateLimitOutcome, RouteLimit, UserRateLimiter,
};
use crate::{endpoint_match, http::err_not_found};

pub const AUTH_UI_BLOCK_ID: &str = "impresspress/auth-ui";

/// Declarative rate-limit table for the auth-ui HTTP surface, replacing the
/// hand-rolled five-arm match. Rules are tried top-down; the first
/// `(action, path)` match wins (see [`check_route_limits`]). IP-keyed rules
/// guard unauthenticated endpoints; User-keyed rules guard authenticated ones.
const RATE_LIMIT_ROUTES: &[RouteLimit] = &[
    // Unauthenticated sensitive endpoints: login / signup — keyed by IP.
    RouteLimit {
        matches: |a, p| a == "create" && matches!(p, "/auth/api/login" | "/auth/api/signup"),
        key: LimitKey::Ip,
        category: "auth",
        limit: RateLimit::AUTH,
    },
    // Token refresh — keyed by IP, its own (looser) category.
    RouteLimit {
        matches: |a, p| a == "create" && p == "/auth/api/refresh",
        key: LimitKey::Ip,
        category: "refresh",
        limit: RateLimit::REFRESH,
    },
    // Forgot/reset password + verification — keyed by IP, shares the auth bucket.
    RouteLimit {
        matches: |a, p| match p {
            "/auth/api/forgot-password"
            | "/auth/api/reset-password"
            | "/auth/api/resend-verification" => a == "create",
            "/auth/api/verify" => a == "retrieve" || a == "create",
            _ => false,
        },
        key: LimitKey::Ip,
        category: "auth",
        limit: RateLimit::AUTH,
    },
    // Authenticated read endpoints — keyed by user_id.
    RouteLimit {
        matches: |a, p| a == "retrieve" && matches!(p, "/auth/api/me" | "/auth/api/api-keys"),
        key: LimitKey::User,
        category: "auth_read",
        limit: RateLimit::API_READ,
    },
    // Authenticated write endpoints — keyed by user_id. Catches every update /
    // delete plus the two non-update write endpoints. Ordered last so the
    // read rule above wins for retrieves.
    RouteLimit {
        matches: |a, p| {
            a == "update"
                || a == "delete"
                || (a == "create"
                    && matches!(p, "/auth/api/change-password" | "/auth/api/api-keys"))
        },
        key: LimitKey::User,
        category: "auth_write",
        limit: RateLimit::API_WRITE,
    },
];

/// The auth-ui block's own declared config vars (OAuth provider creds). Single
/// source of truth for both `BlockInfo::config_keys` and the admin settings
/// page (rendered via `ui::settings_form`, not a parallel tuple table).
///
/// OAuth provider creds live under the auth-ui prefix
/// (`IMPRESSPRESS__AUTH_UI__OAUTH_*`) to keep the prefix-equals-block-name
/// invariant the runtime enforces (see `block_name_to_var_prefix`). The
/// auth-identity vars JWT_SECRET / REQUIRE_VERIFICATION / ALLOWED_EMAIL_DOMAINS
/// are `WAFER_RUN__AUTH__*` and declared in `auth::config` instead.
pub(crate) fn config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            "IMPRESSPRESS__AUTH_UI__OAUTH_GOOGLE_CLIENT_ID",
            "Google OAuth client ID",
            "",
        )
        .name("Google Client ID")
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__AUTH_UI__OAUTH_GOOGLE_CLIENT_SECRET",
            "Google OAuth client secret",
            "",
        )
        .name("Google Client Secret")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__AUTH_UI__OAUTH_GITHUB_CLIENT_ID",
            "GitHub OAuth client ID",
            "",
        )
        .name("GitHub Client ID")
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__AUTH_UI__OAUTH_GITHUB_CLIENT_SECRET",
            "GitHub OAuth client secret",
            "",
        )
        .name("GitHub Client Secret")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__AUTH_UI__OAUTH_MICROSOFT_CLIENT_ID",
            "Microsoft OAuth client ID",
            "",
        )
        .name("Microsoft Client ID")
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__AUTH_UI__OAUTH_MICROSOFT_CLIENT_SECRET",
            "Microsoft OAuth client secret",
            "",
        )
        .name("Microsoft Client Secret")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__AUTH_UI__OAUTH_REDIRECT_URI",
            "OAuth callback URL",
            "",
        )
        .name("OAuth Redirect URI")
        .input_type(InputType::Url)
        .optional(),
    ]
}

crate::impresspress_feature_block! {
    /// Impresspress auth HTTP surface — SSR pages + JSON API + OAuth + bootstrap
    /// (`impresspress/auth-ui`). The auth *service* primitive lives in the
    /// framework `wafer-run/auth` block.
    pub struct AuthUiBlock;
    fields: { limiter: UserRateLimiter },
    name: "impresspress/auth-ui",
    info: |_this| {
        BlockInfo::new(
            AUTH_UI_BLOCK_ID,
            "0.0.1",
            "http-handler@v1",
            "SSR auth pages + login/signup/oauth/bootstrap handlers",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec![
            "wafer-run/database".into(),
            "wafer-run/crypto".into(),
            "wafer-run/config".into(),
            "wafer-run/network".into(),
            "impresspress/email".into(),
            "wafer-run/auth".into(),
        ])
        .category(wafer_run::BlockCategory::Feature)
        .description(
            "Impresspress auth HTTP surface (SSR pages, JSON API, OAuth, bootstrap \
             token redemption). Reads/writes auth tables via repo::* under WRAP \
             grant. Calls wafer-run/auth via auth@v1 for require_user/role/token.",
        )
        .endpoints(vec![
            // Admin settings — declared `Admin` so the central router enforces
            // the tier; the handler no longer re-checks `is_admin`. (The
            // auth-ui prefix route is Public, so this declared level is the
            // gate for the admin settings surface.)
            BlockEndpoint::get("/b/auth/admin/settings")
                .summary("Auth settings page")
                .auth(AuthLevel::Admin),
            BlockEndpoint::post("/b/auth/admin/settings")
                .summary("Save auth settings")
                .auth(AuthLevel::Admin),
            // SSR pages
            BlockEndpoint::get("/b/auth/login").summary("Login page"),
            BlockEndpoint::get("/b/auth/signup").summary("Signup page"),
            BlockEndpoint::get("/b/auth/change-password")
                .summary("Change password page")
                .auth(AuthLevel::Authenticated),
            BlockEndpoint::get("/b/auth/orgs")
                .summary("Claimed organizations")
                .auth(AuthLevel::Authenticated),
            BlockEndpoint::get("/b/auth/oauth/login").summary("Start OAuth flow"),
            // JSON API — schemas below mirror the real request/response
            // shapes read from the handlers (`api/login.rs`, `api/signup.rs`,
            // `api/me.rs`, `api/refresh.rs`, `api/logout.rs`), same pattern
            // as `blocks/messages/mod.rs`. These are the core developer-facing
            // auth endpoints; full schema coverage of every `/b/auth/*` route
            // (OAuth, api-keys, password reset, bootstrap) is a follow-up.
            BlockEndpoint::post("/b/auth/api/login")
                .summary("Authenticate with email/password")
                .input_schema(serde_json::json!({
                    "type": "object",
                    "required": ["email", "password"],
                    "properties": {
                        "email": {"type": "string", "format": "email"},
                        "password": {"type": "string"}
                    }
                }))
                .output_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "access_token": {"type": "string"},
                        "refresh_token": {"type": "string"},
                        "token_type": {"type": "string", "const": "Bearer"},
                        "expires_in": {"type": "integer", "description": "Access token lifetime in seconds"},
                        "default_redirect": {"type": "string", "description": "Role-aware post-login redirect path"},
                        "user": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "email": {"type": "string"},
                                "roles": {"type": "array", "items": {"type": "string"}},
                                "name": {"type": "string"}
                            }
                        }
                    }
                }))
                .tags(&["auth"]),
            BlockEndpoint::post("/b/auth/api/signup")
                .summary("Create account")
                .input_schema(serde_json::json!({
                    "type": "object",
                    "required": ["email", "password"],
                    "properties": {
                        "email": {"type": "string", "format": "email"},
                        "password": {"type": "string"},
                        "name": {"type": "string", "description": "Optional display name"}
                    }
                }))
                .output_schema(serde_json::json!({
                    "type": "object",
                    "description": "Auto-logs in (issues tokens) unless email verification is required, in which case only email_verified/message/user are returned.",
                    "properties": {
                        "email_verified": {"type": "boolean"},
                        "message": {"type": "string", "description": "Present when verification is required or the email is already registered"},
                        "access_token": {"type": "string"},
                        "refresh_token": {"type": "string"},
                        "token_type": {"type": "string", "const": "Bearer"},
                        "expires_in": {"type": "integer"},
                        "default_redirect": {"type": "string"},
                        "user": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "email": {"type": "string"},
                                "roles": {"type": "array", "items": {"type": "string"}},
                                "name": {"type": "string"}
                            }
                        }
                    }
                }))
                .tags(&["auth"]),
            BlockEndpoint::post("/b/auth/api/logout")
                .summary("Sign out")
                .auth(AuthLevel::Authenticated)
                .output_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "message": {"type": "string"}
                    }
                }))
                .tags(&["auth"]),
            BlockEndpoint::get("/b/auth/api/me")
                .summary("Get current user")
                .auth(AuthLevel::Authenticated)
                .output_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "user": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "email": {"type": "string"},
                                "name": {"type": "string"},
                                "roles": {"type": "array", "items": {"type": "string"}},
                                "created_at": {"type": "string", "format": "date-time"},
                                "avatar_url": {"type": "string"}
                            }
                        }
                    }
                }))
                .tags(&["auth"]),
            // Was previously undeclared entirely (dispatched in `handle()`
            // but absent from `.endpoints`), which meant it was excluded
            // from `/openapi.json` AND from the per-endpoint access-tier
            // table. Declaring it here (default `AuthLevel::Public`, matching
            // its actual undeclared-backstop behavior — the `/b/auth/` prefix
            // route is Public and this endpoint takes no `Authorization`
            // header, only a `refresh_token` body field) is a documentation
            // fix only; it does not change effective access.
            BlockEndpoint::post("/b/auth/api/refresh")
                .summary("Rotate an access/refresh token pair")
                .input_schema(serde_json::json!({
                    "type": "object",
                    "required": ["refresh_token"],
                    "properties": {
                        "refresh_token": {"type": "string"}
                    }
                }))
                .output_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "access_token": {"type": "string"},
                        "refresh_token": {"type": "string"},
                        "token_type": {"type": "string", "const": "Bearer"},
                        "expires_in": {"type": "integer", "description": "Access token lifetime in seconds"}
                    }
                }))
                .tags(&["auth"]),
            BlockEndpoint::post("/b/auth/api/change-password")
                .summary("Change password")
                .auth(AuthLevel::Authenticated),
            BlockEndpoint::get("/b/auth/api/api-keys")
                .summary("List API keys")
                .auth(AuthLevel::Authenticated),
            BlockEndpoint::post("/b/auth/api/api-keys")
                .summary("Create API key")
                .auth(AuthLevel::Authenticated),
            // Bootstrap token redemption (filled in Task 6)
            BlockEndpoint::get("/b/auth/bootstrap").summary("Bootstrap token redemption form"),
            BlockEndpoint::post("/b/auth/api/bootstrap").summary("Redeem bootstrap admin token"),
        ])
        .config_keys(config_vars())
        .admin_url("/b/auth/admin/settings")
    },
    handle: |this, ctx, msg, input| {
        let action = msg.action().to_string();
        // Normalize: /b/auth/... → /auth/...
        let raw_path = msg.path().to_string();
        let path = if let Some(stripped) = raw_path.strip_prefix("/b") {
            stripped.to_string()
        } else {
            raw_path
        };

        // Apply per-user/IP rate limiting via the declarative RATE_LIMIT_ROUTES
        // table (see its doc comment for the rule set).
        //
        // `RateLimitOutcome::Allowed(headers)` is discarded here — injecting
        // X-RateLimit-* response headers needs a streaming-middleware shape we
        // don't have yet. Tracked as a single follow-up, not a per-route TODO.
        if let Some(RateLimitOutcome::Limited(r)) = check_route_limits(
            &this.limiter,
            ctx,
            &msg,
            action.as_str(),
            path.as_str(),
            RATE_LIMIT_ROUTES,
        )
        .await
        {
            return r;
        }

        match (action.as_str(), path.as_str()) {
            // ── Admin settings ───────────────────────────────────────
            // Admin tier enforced centrally from the declared
            // `AuthLevel::Admin` on `GET|POST /b/auth/admin/settings`.
            ("retrieve", "/auth/admin/settings") => pages::settings::handle_get(ctx, &msg).await,
            ("create", "/auth/admin/settings") => pages::settings::handle_post(ctx, input).await,
            // ── SSR pages (HTML) ──────────────────────────────────────
            ("retrieve", "/auth/login") => pages::login::handle(ctx, &msg).await,
            ("retrieve", "/auth/signup") => pages::signup::handle(ctx, &msg).await,
            ("retrieve", "/auth/change-password") => {
                if msg.user_id().is_empty() {
                    return pages::login::handle(ctx, &msg).await;
                }
                pages::change_password::handle(ctx, &msg).await
            }
            ("retrieve", "/auth/orgs") => pages::orgs::handle(ctx, &msg).await,
            ("retrieve", "/auth/reset-password") => pages::reset_password::handle(ctx, &msg).await,
            // Bootstrap token redemption (NEW — filled in Task 6)
            ("retrieve", "/auth/bootstrap") => pages::bootstrap::handle_get(ctx, &msg).await,
            // OAuth browser redirects
            ("retrieve", "/auth/oauth/login") => oauth::start::handle(ctx, &msg).await,
            ("retrieve", "/auth/oauth/callback") => oauth::callback::handle(ctx, &msg).await,

            // ── JSON API under /auth/api/ ─────────────────────────────
            ("create", "/auth/api/login") => api::login::handle(ctx, input).await,
            ("create", "/auth/api/signup") => api::signup::handle(ctx, input).await,
            ("create", "/auth/api/refresh") => api::refresh::handle(ctx, input).await,
            ("create", "/auth/api/logout") => api::logout::handle(ctx, &msg).await,
            ("retrieve", "/auth/api/me") => api::me::handle_get(ctx, &msg).await,
            ("update", "/auth/api/me") => api::me::handle_update(ctx, &msg, input).await,
            ("create", "/auth/api/change-password") => {
                api::change_password::handle(ctx, &msg, input).await
            }
            // API keys (admin user-management still hits these via htmx)
            ("retrieve", "/auth/api/api-keys") => api::api_keys::handle_list(ctx, &msg).await,
            ("create", "/auth/api/api-keys") => {
                api::api_keys::handle_create(ctx, &msg, input).await
            }
            ("update", p)
                if endpoint_match::match_template("/auth/api/api-keys/{id}", p).is_some() =>
            {
                api::api_keys::handle_revoke(ctx, &msg).await
            }
            ("delete", p)
                if endpoint_match::match_template("/auth/api/api-keys/{id}", p).is_some() =>
            {
                api::api_keys::handle_delete(ctx, &msg).await
            }
            // Email verification
            ("retrieve" | "create", "/auth/api/verify") => {
                api::verify::handle(ctx, &msg, input).await
            }
            ("create", "/auth/api/resend-verification") => {
                api::verify::handle_resend(ctx, input).await
            }
            // Password reset
            ("create", "/auth/api/forgot-password") => {
                api::forgot_password::handle(ctx, input).await
            }
            ("create", "/auth/api/reset-password") => api::reset_password::handle(ctx, input).await,
            // OAuth API
            ("retrieve", "/auth/api/oauth/providers") => oauth::providers::handle(ctx).await,
            ("create", "/auth/api/oauth/sync-user") => {
                api::sync_user::handle(ctx, &msg, input).await
            }
            // Bootstrap admin token redemption (NEW — filled in Task 6)
            ("create", "/auth/api/bootstrap") => api::bootstrap::handle(ctx, &msg, input).await,
            _ => err_not_found("not found"),
        }
    },
    // No `lifecycle`: auth-ui owns no schema (auth tables belong to the
    // framework `wafer-run/auth` block), so the `Block` no-op default
    // applies.
}
