//! `impresspress/auth-ui` — SSR pages + JSON API + OAuth flows + bootstrap token
//! redemption for impresspress auth.
//!
//! Plan A2 PR 5 splits the legacy `wafer-run/auth` block into two halves:
//!
//! - **Framework auth** (`wafer-run/auth`, lives in `wafer-run` proper):
//!   service-shaped block exposing `auth@v1` (`require_user`/`require_role`/
//!   `require_token`) over impresspress's `auth::service::AuthServiceImpl`,
//!   which authenticates the access JWT this block mints. Owns `JWT_SECRET`,
//!   `REQUIRE_VERIFICATION` and `ALLOWED_EMAIL_DOMAINS`. No HTTP routes.
//!
//! - **auth-ui** (this module): all `/b/auth/*` HTTP routes. Reads and writes
//!   the auth tables through `auth::repo::*` under WRAP grant — it does not
//!   call `auth@v1`, and no caller of that interface exists anywhere in the
//!   workspace today.
//!
//! Declares the full `BlockInfo` (endpoints, requires, OAuth-creds
//! config_keys) from [`ROUTES`], runs the per-user/IP rate-limit check keyed
//! on the matched [`Route`], and dispatches every `/b/auth/*` route to a leaf
//! module under `api/`, `pages/`, or `oauth/`. The framework `wafer-run/auth`
//! block (in `auth/`) owns the auth *service*; this block owns the HTTP
//! surface.

pub mod api;
pub mod contracts;
pub mod oauth;
pub mod pages;
pub mod redirect;
#[cfg(test)]
mod tests;

use wafer_run::{
    context::Context, BlockInfo, ConfigVar, HttpMethod, InputType, InstanceMode, Message,
    OutputStream,
};

use super::rate_limit::{apply_route_limit, LimitKey, RateLimit, UserRateLimiter};
use crate::{
    endpoint_match::{self, request_schema_of, response_schema_of, EndpointRoute},
    http::{err_not_found, ok_json},
};

pub const AUTH_UI_BLOCK_ID: &str = "impresspress/auth-ui";

/// [B12] Message kind that forces one auth retention pass and answers with the
/// [`SweepResult`](crate::blocks::auth::maintenance::SweepResult).
///
/// Mirrors `tickets.maintenance`. The sweep already runs on its own, throttled
/// from token issuance, so this exists for an operator who wants a pass now and
/// for the Cloudflare Worker's `scheduled` handler — not because anything
/// depends on it.
///
/// It is handled here rather than on the framework `wafer-run/auth` block
/// because that block routes every message through wafer-core's own `auth@v1`
/// handler, and wafer-run is pinned. auth-ui already holds the WRAP grants for
/// every auth table, so this is also where the pass can actually run.
pub const MAINTENANCE_MESSAGE_KIND: &str = "auth.maintenance";

/// The exact message a scheduler sends to run one retention pass.
///
/// A constructor rather than a documented recipe because the sender lives in
/// another crate (`impresspress-cloudflare`'s `scheduled` handler) and there
/// is no route, schema or snapshot gate covering a bare message kind: spelled
/// at the call site, a typo would be a silent no-op that answers `NotFound`
/// once a day forever.
pub fn maintenance_message() -> Message {
    Message::new(MAINTENANCE_MESSAGE_KIND)
}

/// Decode the [`SweepResult`](crate::blocks::auth::maintenance::SweepResult)
/// out of what the handler answered.
///
/// The pair to [`maintenance_message`], for the same reason: the caller that
/// needs this is in another crate, and a `SweepResult` that failed to decode
/// must not be reported as a pass that removed nothing.
pub async fn sweep_result_from_output(
    output: OutputStream,
) -> Result<crate::blocks::auth::maintenance::SweepResult, String> {
    let buffered = output
        .collect_buffered()
        .await
        .map_err(|terminal| format!("auth maintenance did not answer a response: {terminal:?}"))?;
    serde_json::from_slice(&buffered.body)
        .map_err(|error| format!("auth maintenance answer is not a SweepResult: {error}"))
}

/// Handler for one row of [`ROUTES`]. `Verify` serves both the `GET` and the
/// `POST` row of `/b/auth/api/verify` (the token arrives in the query string
/// or in the body).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    AdminSettingsPage,
    AdminSaveSettings,
    LoginPage,
    SignupPage,
    ChangePasswordPage,
    OrgsPage,
    ResetPasswordPage,
    BootstrapPage,
    OauthStart,
    OauthCallback,
    Login,
    Signup,
    Refresh,
    Logout,
    Me,
    UpdateMe,
    ChangePassword,
    ListApiKeys,
    CreateApiKey,
    RevokeApiKey,
    DeleteApiKey,
    Verify,
    ResendVerification,
    ForgotPassword,
    ResetPassword,
    OauthProviders,
    Bootstrap,
}

/// Query-parameter schema for `GET /b/auth/oauth/login`. Hand-written: the
/// handler reads `provider` straight off the query string
/// (`oauth::start::handle`), so there is no deserialized request struct to
/// derive from. The accepted values are read from [`oauth::spec`] rather
/// than restated, because that table is what `start.rs` looks the provider
/// up in — a provider added there must not need a second edit here to
/// become documented.
fn oauth_start_query_schema() -> serde_json::Value {
    let providers: Vec<&str> = oauth::spec::OAUTH_PROVIDERS
        .iter()
        .map(|p| p.name)
        .collect();
    serde_json::json!({
        "type": "object",
        "required": ["provider"],
        "properties": {
            "provider": {
                "type": "string",
                "description": "OAuth provider to start the flow with",
                "enum": providers
            }
        }
    })
}

/// The block's HTTP surface: what `handle()` dispatches on and what
/// `info().endpoints` is generated from. Wire paths; `{id}` is bound into
/// `req.param.*` for the api-key handlers' `msg.var` reader.
///
/// Every row names the level the central router enforces. A `public` row is
/// a decision recorded next to the row: the handler gates itself by a token,
/// signature or shared secret, or the endpoint exists precisely for a caller
/// with no session yet (login, signup, "forgot password"). The JSON API
/// schemas are DERIVED from the types the handlers actually deserialize into
/// and serialize out of, declared in [`contracts`], so they cannot drift from
/// the handlers.
///
/// What is still undeclared, so nobody reads the coverage as complete: the
/// four api-key rows, `GET`/`POST /b/auth/api/verify`,
/// `GET /b/auth/api/oauth/providers` and `POST /b/auth/api/bootstrap`
/// publish no schema at all; the password-reset and change-password rows
/// publish a response schema but not their request bodies. The
/// change-password row's published response covers its JSON branch only —
/// the same endpoint answers an htmx caller with an HTML fragment
/// (`api::change_password`), which is a browser affordance rather than part
/// of the JSON API this document describes.
const ROUTES: &[EndpointRoute<Route>] = &[
    // ── Admin settings ── declared `Admin` so the central router enforces the
    // tier; the handler re-checks nothing. (The auth-ui prefix route is
    // Public, so this declared level is the gate for the admin surface.)
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/auth/admin/settings",
        Route::AdminSettingsPage,
    )
    .summary("Auth settings page"),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/auth/admin/settings",
        Route::AdminSaveSettings,
    )
    .summary("Save auth settings"),
    // ── SSR pages ──
    EndpointRoute::public(HttpMethod::Get, "/b/auth/login", Route::LoginPage).summary("Login page"),
    EndpointRoute::public(HttpMethod::Get, "/b/auth/signup", Route::SignupPage)
        .summary("Signup page"),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/auth/change-password",
        Route::ChangePasswordPage,
    )
    .summary("Change password page"),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/auth/orgs", Route::OrgsPage)
        .summary("Claimed organizations"),
    // Public: logged-out by definition. `pages/reset_password.rs` renders the
    // form only when a `token` query parameter is present; the token itself
    // is verified by `POST /b/auth/api/reset-password`.
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/auth/reset-password",
        Route::ResetPasswordPage,
    )
    .summary("Password reset form"),
    EndpointRoute::public(HttpMethod::Get, "/b/auth/bootstrap", Route::BootstrapPage)
        .summary("Bootstrap token redemption form"),
    // ── OAuth browser redirects ──
    // Navigated to, never fetched: it answers `302` to the provider and sets
    // the cookie binding the flow to this browser, which only a top-level
    // navigation can store first-party. No response body, so no output
    // schema — `oauth/start.rs` has the full rationale.
    EndpointRoute::public(HttpMethod::Get, "/b/auth/oauth/login", Route::OauthStart)
        .summary("Start OAuth flow")
        .query_params(oauth_start_query_schema)
        .tags(&["auth"]),
    // Public: the provider redirects the browser here with no session by
    // design; `oauth/callback.rs` consumes the single-use PKCE state.
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/auth/oauth/callback",
        Route::OauthCallback,
    )
    .summary("OAuth provider callback"),
    // ── JSON API ──
    EndpointRoute::public(HttpMethod::Post, "/b/auth/api/login", Route::Login)
        .summary("Authenticate with email/password")
        .input(request_schema_of::<contracts::LoginRequest>)
        .output(response_schema_of::<contracts::LoginResponse>)
        .tags(&["auth"]),
    EndpointRoute::public(HttpMethod::Post, "/b/auth/api/signup", Route::Signup)
        .summary("Create account")
        .input(request_schema_of::<contracts::SignupRequest>)
        .output(response_schema_of::<contracts::SignupResponse>)
        .tags(&["auth"]),
    // Public: takes no `Authorization` header, only a `refresh_token` body
    // field.
    EndpointRoute::public(HttpMethod::Post, "/b/auth/api/refresh", Route::Refresh)
        .summary("Rotate an access/refresh token pair")
        .input(request_schema_of::<contracts::RefreshRequest>)
        .output(response_schema_of::<contracts::RefreshResponse>)
        .tags(&["auth"]),
    EndpointRoute::authenticated(HttpMethod::Post, "/b/auth/api/logout", Route::Logout)
        .summary("Sign out")
        .output(response_schema_of::<contracts::MessageResponse>)
        .tags(&["auth"]),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/auth/api/me", Route::Me)
        .summary("Get current user")
        .output(response_schema_of::<contracts::MeResponse>)
        .tags(&["auth"]),
    // PATCH is what the SDK sends; `update` is the action both PUT and PATCH
    // map to.
    EndpointRoute::authenticated(HttpMethod::Patch, "/b/auth/api/me", Route::UpdateMe)
        .summary("Update current user profile")
        .input(request_schema_of::<contracts::UpdateMeRequest>)
        .output(response_schema_of::<contracts::MeResponse>)
        .tags(&["auth"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/auth/api/change-password",
        Route::ChangePassword,
    )
    .summary("Change password")
    .output(response_schema_of::<contracts::MessageResponse>)
    .tags(&["auth"]),
    // ── API keys (admin user-management still hits these via htmx) ──
    EndpointRoute::authenticated(HttpMethod::Get, "/b/auth/api/api-keys", Route::ListApiKeys)
        .summary("List API keys"),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/auth/api/api-keys",
        Route::CreateApiKey,
    )
    .summary("Create API key")
    .input(request_schema_of::<contracts::CreateApiKeyRequest>),
    // Authenticated; `api/api_keys.rs` additionally refuses a key the caller
    // does not own.
    EndpointRoute::authenticated(
        HttpMethod::Patch,
        "/b/auth/api/api-keys/{id}",
        Route::RevokeApiKey,
    )
    .summary("Revoke API key"),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/auth/api/api-keys/{id}",
        Route::DeleteApiKey,
    )
    .summary("Delete API key"),
    // ── Email verification ── public: `api/verify.rs` consumes a single-use
    // verification token, from the query string on GET or the body on POST.
    EndpointRoute::public(HttpMethod::Get, "/b/auth/api/verify", Route::Verify)
        .summary("Verify email address"),
    EndpointRoute::public(HttpMethod::Post, "/b/auth/api/verify", Route::Verify)
        .summary("Verify email address"),
    // Public: issues a verification token to the address's owner and answers
    // one constant body whatever the account's state (unregistered, already
    // verified, inside its 60 s cooldown), so nothing about an address can be
    // learned from the response; `api::verify::resend_tests` pins that. IP
    // rate-limited (see `rate_limit_for`).
    EndpointRoute::public(
        HttpMethod::Post,
        "/b/auth/api/resend-verification",
        Route::ResendVerification,
    )
    .summary("Re-send the verification email")
    .output(response_schema_of::<contracts::MessageResponse>)
    .tags(&["auth"]),
    // ── Password reset ── public for the same reasons as the verification
    // pair: `forgot-password` issues a token to the address's owner behind a
    // constant response; `reset-password` consumes it (hash match + expiry).
    EndpointRoute::public(
        HttpMethod::Post,
        "/b/auth/api/forgot-password",
        Route::ForgotPassword,
    )
    .summary("Request a password reset email")
    .output(response_schema_of::<contracts::MessageResponse>)
    .tags(&["auth"]),
    EndpointRoute::public(
        HttpMethod::Post,
        "/b/auth/api/reset-password",
        Route::ResetPassword,
    )
    .summary("Reset password with a reset token")
    .output(response_schema_of::<contracts::MessageResponse>)
    .tags(&["auth"]),
    // ── OAuth API ──
    // Public: reports which providers are configured, the same fact the
    // login page renders as buttons for anonymous visitors.
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/auth/api/oauth/providers",
        Route::OauthProviders,
    )
    .summary("Configured OAuth providers"),
    // ── Bootstrap admin token redemption ──
    EndpointRoute::public(HttpMethod::Post, "/b/auth/api/bootstrap", Route::Bootstrap)
        .summary("Redeem bootstrap admin token"),
];

/// The rate-limit bucket a matched route spends, `(key, category, default
/// limit)`, or `None` for a route this layer does not limit. Applied after
/// `dispatch` has chosen the variant, so the block matches a path exactly
/// once. IP-keyed buckets guard endpoints a caller reaches without a session;
/// user-keyed buckets guard authenticated ones. The match is exhaustive so a
/// new row is a rate-limit decision, not an omission.
const fn rate_limit_for(route: Route) -> Option<(LimitKey, &'static str, RateLimit)> {
    match route {
        // Login / signup / bootstrap redemption, and the token-issuing and
        // token-consuming password-reset and verification endpoints, share
        // the `auth` bucket.
        Route::Login
        | Route::Signup
        | Route::Bootstrap
        | Route::ForgotPassword
        | Route::ResetPassword
        | Route::ResendVerification
        | Route::Verify => Some((LimitKey::Ip, "auth", RateLimit::AUTH)),
        // Each OAuth start writes a PKCE state row that stays until a callback
        // redeems it or a maintenance sweep finds it expired; this bounds that
        // growth. Its own bucket, because a GET is reached by far more than a
        // sign-in attempt (an `<img>` tag, a link prefetcher, a crawler, a user
        // retrying the provider) and none of that should spend the password
        // login budget.
        Route::OauthStart => Some((LimitKey::Ip, "oauth_start", RateLimit::AUTH)),
        // Token refresh has its own (looser) category.
        Route::Refresh => Some((LimitKey::Ip, "refresh", RateLimit::REFRESH)),
        Route::Me | Route::ListApiKeys => Some((LimitKey::User, "auth_read", RateLimit::API_READ)),
        Route::UpdateMe
        | Route::ChangePassword
        | Route::CreateApiKey
        | Route::RevokeApiKey
        | Route::DeleteApiKey => Some((LimitKey::User, "auth_write", RateLimit::API_WRITE)),
        Route::AdminSettingsPage
        | Route::AdminSaveSettings
        | Route::LoginPage
        | Route::SignupPage
        | Route::ChangePasswordPage
        | Route::OrgsPage
        | Route::ResetPasswordPage
        | Route::BootstrapPage
        | Route::OauthCallback
        | Route::Logout
        | Route::OauthProviders => None,
    }
}

/// Spend `route`'s rate-limit bucket for this request. `Some(response)` is
/// the 429 to return; `None` means proceed: no bucket for this route, or
/// `apply_route_limit` let the request through.
async fn apply_rate_limit(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
    msg: &Message,
    route: Route,
) -> Option<OutputStream> {
    let (key, category, limit) = rate_limit_for(route)?;
    apply_route_limit(limiter, ctx, msg, key, category, limit).await
}

/// Block config key: the Google OAuth client ID.
pub(crate) const OAUTH_GOOGLE_CLIENT_ID_KEY: &str = "IMPRESSPRESS__AUTH_UI__OAUTH_GOOGLE_CLIENT_ID";
/// Block config key: the Google OAuth client secret.
pub(crate) const OAUTH_GOOGLE_CLIENT_SECRET_KEY: &str =
    "IMPRESSPRESS__AUTH_UI__OAUTH_GOOGLE_CLIENT_SECRET";
/// Block config key: the GitHub OAuth client ID.
pub(crate) const OAUTH_GITHUB_CLIENT_ID_KEY: &str = "IMPRESSPRESS__AUTH_UI__OAUTH_GITHUB_CLIENT_ID";
/// Block config key: the GitHub OAuth client secret.
pub(crate) const OAUTH_GITHUB_CLIENT_SECRET_KEY: &str =
    "IMPRESSPRESS__AUTH_UI__OAUTH_GITHUB_CLIENT_SECRET";
/// Block config key: the Microsoft OAuth client ID.
pub(crate) const OAUTH_MICROSOFT_CLIENT_ID_KEY: &str =
    "IMPRESSPRESS__AUTH_UI__OAUTH_MICROSOFT_CLIENT_ID";
/// Block config key: the Microsoft OAuth client secret.
pub(crate) const OAUTH_MICROSOFT_CLIENT_SECRET_KEY: &str =
    "IMPRESSPRESS__AUTH_UI__OAUTH_MICROSOFT_CLIENT_SECRET";
/// Block config key: the OAuth callback URL registered with each provider.
pub(crate) const OAUTH_REDIRECT_URI_KEY: &str = "IMPRESSPRESS__AUTH_UI__OAUTH_REDIRECT_URI";

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
        ConfigVar::new(OAUTH_GOOGLE_CLIENT_ID_KEY, "Google OAuth client ID", "")
            .name("Google Client ID")
            .optional(),
        ConfigVar::new(
            OAUTH_GOOGLE_CLIENT_SECRET_KEY,
            "Google OAuth client secret",
            "",
        )
        .name("Google Client Secret")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(OAUTH_GITHUB_CLIENT_ID_KEY, "GitHub OAuth client ID", "")
            .name("GitHub Client ID")
            .optional(),
        ConfigVar::new(
            OAUTH_GITHUB_CLIENT_SECRET_KEY,
            "GitHub OAuth client secret",
            "",
        )
        .name("GitHub Client Secret")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            OAUTH_MICROSOFT_CLIENT_ID_KEY,
            "Microsoft OAuth client ID",
            "",
        )
        .name("Microsoft Client ID")
        .optional(),
        ConfigVar::new(
            OAUTH_MICROSOFT_CLIENT_SECRET_KEY,
            "Microsoft OAuth client secret",
            "",
        )
        .name("Microsoft Client Secret")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(OAUTH_REDIRECT_URI_KEY, "OAuth callback URL", "")
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
    fields: { limiter: std::sync::Arc<UserRateLimiter> },
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
             grant.",
        )
        .endpoints(endpoint_match::declare(ROUTES))
        .config_keys(config_vars())
        .admin_url("/b/auth/admin/settings")
    },
    handle: |this, ctx, mut msg, input| {
        if msg.kind == MAINTENANCE_MESSAGE_KIND {
            return ok_json(&crate::blocks::auth::maintenance::sweep(ctx).await);
        }
        // Auth is enforced centrally by `route_to_block` from each row's
        // declared `AuthLevel`. `{id}` is bound into `req.param.*` for the
        // api-key handlers' `msg.var` reader.
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return err_not_found("not found");
        };
        if let Some(limited) = apply_rate_limit(&this.limiter, ctx, &msg, route).await {
            return limited;
        }
        match route {
            Route::AdminSettingsPage => pages::settings::handle_get(ctx, &msg).await,
            Route::AdminSaveSettings => pages::settings::handle_post(ctx, &msg, input).await,
            Route::LoginPage => pages::login::handle(ctx, &msg).await,
            Route::SignupPage => pages::signup::handle(ctx, &msg).await,
            Route::ChangePasswordPage => {
                if msg.user_id().is_empty() {
                    return pages::login::handle(ctx, &msg).await;
                }
                pages::change_password::handle(ctx, &msg).await
            }
            Route::OrgsPage => pages::orgs::handle(ctx, &msg).await,
            Route::ResetPasswordPage => pages::reset_password::handle(ctx, &msg).await,
            Route::BootstrapPage => pages::bootstrap::handle_get(ctx, &msg).await,
            Route::OauthStart => oauth::start::handle(ctx, &msg).await,
            Route::OauthCallback => oauth::callback::handle(&this.limiter, ctx, &msg).await,
            Route::Login => api::login::handle(ctx, input).await,
            Route::Signup => api::signup::handle(&this.limiter, ctx, &msg, input).await,
            Route::Refresh => api::refresh::handle(ctx, input).await,
            Route::Logout => api::logout::handle(ctx, &msg).await,
            Route::Me => api::me::handle_get(ctx, &msg).await,
            Route::UpdateMe => api::me::handle_update(ctx, &msg, input).await,
            Route::ChangePassword => api::change_password::handle(ctx, &msg, input).await,
            Route::ListApiKeys => api::api_keys::handle_list(ctx, &msg).await,
            Route::CreateApiKey => api::api_keys::handle_create(ctx, &msg, input).await,
            Route::RevokeApiKey => api::api_keys::handle_revoke(ctx, &msg).await,
            Route::DeleteApiKey => api::api_keys::handle_delete(ctx, &msg).await,
            Route::Verify => api::verify::handle(ctx, &msg, input).await,
            Route::ResendVerification => {
                api::verify::handle_resend(&this.limiter, ctx, &msg, input).await
            }
            Route::ForgotPassword => {
                api::forgot_password::handle(&this.limiter, ctx, &msg, input).await
            }
            Route::ResetPassword => api::reset_password::handle(ctx, input).await,
            Route::OauthProviders => oauth::providers::handle(ctx).await,
            Route::Bootstrap => api::bootstrap::handle(ctx, &msg, input).await,
        }
    },
    // No `lifecycle`: auth-ui owns no schema (auth tables belong to the
    // framework `wafer-run/auth` block), so the `Block` no-op default
    // applies.
}

#[cfg(test)]
mod test_support {
    use wafer_run::Message;

    /// Run `msg` through the block's own route table so `{id}` is bound the
    /// way it is on the wire, then hand the message to a handler directly.
    /// Panics when no row matches: a test that sends an unroutable path
    /// would otherwise exercise the handler's "missing id" branch by
    /// accident.
    pub(super) fn routed(mut msg: Message) -> Message {
        let route = crate::endpoint_match::dispatch(&mut msg, super::ROUTES);
        assert!(
            route.is_some(),
            "no auth-ui route matches {} {}",
            msg.action(),
            msg.path()
        );
        msg
    }
}

/// The scheduler-facing half of this block: the message a cron sends and the
/// answer it reads back.
///
/// There is no route, no schema and no snapshot covering a bare message kind,
/// so nothing else notices if the kind, the block id or the answer's shape
/// moves. The Cloudflare `scheduled` handler is a `worker::Env` away from any
/// test lane; what it does that is testable is exactly this pair, and it is
/// tested here over the retention sweep's own four-table fixture.
#[cfg(test)]
mod scheduled_maintenance_tests {
    use wafer_run::{Block, InputStream};

    use super::*;
    use crate::{
        blocks::auth::repo::{sessions, sessions::NewSession, tokens},
        test_support::TestContext,
    };

    fn iso(offset_secs: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::seconds(offset_secs))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    #[tokio::test]
    async fn the_scheduled_message_runs_a_sweep_and_its_answer_decodes_to_the_counts() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.seed_auth_user("user-a").await;
        for (family, expires_at) in [("fam-dead", iso(-60)), ("fam-live", iso(3600))] {
            sessions::insert(
                &ctx,
                NewSession {
                    family: family.into(),
                    user_id: "user-a".into(),
                    auth_method: "password".into(),
                    expires_at,
                },
            )
            .await
            .expect("seed session");
        }
        tokens::insert(&ctx, "user-a", "tok-dead", "tok-dead", 0, &iso(-60))
            .await
            .expect("seed token");

        let output = AuthUiBlock::new()
            .handle(&ctx, maintenance_message(), InputStream::empty())
            .await;
        let result = sweep_result_from_output(output)
            .await
            .expect("the scheduled message answers a SweepResult");

        assert_eq!(
            result,
            crate::blocks::auth::maintenance::SweepResult {
                complete: true,
                sessions_deleted: 1,
                tokens_deleted: 1,
                jwt_blocklist_deleted: 0,
                oauth_pkce_deleted: 0,
                errors: Vec::new(),
            },
            "the cron's message must reach the sweep, not fall through to routing"
        );
        assert_eq!(
            sessions::list_for_user(&ctx, "user-a").await.unwrap().len(),
            1,
            "the live session survives"
        );
    }

    /// The handler answers a `SweepResult` and only a `SweepResult`. An
    /// unroutable message — which is what a mistyped kind becomes — must be
    /// reported as a failure, never decoded into a pass that removed nothing.
    #[tokio::test]
    async fn an_answer_that_is_not_a_sweep_result_is_a_failure_not_an_empty_pass() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let mut mistyped = Message::new("auth.maintenence");
        mistyped.set_meta("req.action", "retrieve");
        mistyped.set_meta("req.resource", "/b/auth/api/whatever");

        let output = AuthUiBlock::new()
            .handle(&ctx, mistyped, InputStream::empty())
            .await;
        let error = sweep_result_from_output(output)
            .await
            .expect_err("a non-response terminal must not decode");
        assert!(
            error.contains("did not answer a response"),
            "unexpected error: {error}"
        );
    }
}

#[cfg(test)]
mod rate_limit_tests {
    use super::*;

    #[test]
    fn public_bootstrap_redemption_is_ip_rate_limited() {
        let (key, category, limit) =
            rate_limit_for(Route::Bootstrap).expect("bootstrap redemption spends a bucket");
        assert_eq!(key, LimitKey::Ip);
        assert_eq!(category, "auth");
        assert_eq!(limit.max_requests, RateLimit::AUTH.max_requests);
        assert_eq!(limit.window, RateLimit::AUTH.window);
    }

    #[test]
    fn bootstrap_form_get_does_not_spend_the_redemption_budget() {
        assert!(rate_limit_for(Route::BootstrapPage).is_none());
    }

    /// Every limited route's bucket, by method and wire path. Every row's
    /// bucket must be the one listed here, and every row not listed must
    /// spend nothing, so a change to `rate_limit_for` is a change to this
    /// list too.
    #[test]
    fn rate_limits_are_the_listed_assignments() {
        use HttpMethod::{Delete, Get, Patch, Post};
        let assignments: &[(HttpMethod, &str, LimitKey, &str)] = &[
            (Post, "/b/auth/api/login", LimitKey::Ip, "auth"),
            (Post, "/b/auth/api/signup", LimitKey::Ip, "auth"),
            (Post, "/b/auth/api/bootstrap", LimitKey::Ip, "auth"),
            (Post, "/b/auth/api/refresh", LimitKey::Ip, "refresh"),
            (Post, "/b/auth/api/forgot-password", LimitKey::Ip, "auth"),
            (Post, "/b/auth/api/reset-password", LimitKey::Ip, "auth"),
            (
                Post,
                "/b/auth/api/resend-verification",
                LimitKey::Ip,
                "auth",
            ),
            (Get, "/b/auth/api/verify", LimitKey::Ip, "auth"),
            (Post, "/b/auth/api/verify", LimitKey::Ip, "auth"),
            (Get, "/b/auth/oauth/login", LimitKey::Ip, "oauth_start"),
            (Get, "/b/auth/api/me", LimitKey::User, "auth_read"),
            (Get, "/b/auth/api/api-keys", LimitKey::User, "auth_read"),
            (Patch, "/b/auth/api/me", LimitKey::User, "auth_write"),
            (
                Patch,
                "/b/auth/api/api-keys/{id}",
                LimitKey::User,
                "auth_write",
            ),
            (
                Delete,
                "/b/auth/api/api-keys/{id}",
                LimitKey::User,
                "auth_write",
            ),
            (
                Post,
                "/b/auth/api/change-password",
                LimitKey::User,
                "auth_write",
            ),
            (Post, "/b/auth/api/api-keys", LimitKey::User, "auth_write"),
        ];
        for (method, path, _, _) in assignments {
            assert!(
                ROUTES
                    .iter()
                    .any(|row| row.method == *method && row.template == *path),
                "{method} {path} is not a row"
            );
        }
        for row in ROUTES {
            let expected = assignments
                .iter()
                .find(|(method, path, _, _)| *method == row.method && *path == row.template)
                .map(|(_, _, key, category)| (*key, *category));
            let actual = rate_limit_for(row.handler).map(|(key, category, _)| (key, category));
            assert_eq!(actual, expected, "{} {}", row.method, row.template);
        }
    }

    /// The `RateLimit` each category resolves to is the constant the old
    /// table named for it.
    #[test]
    fn rate_limit_categories_keep_their_defaults() {
        for row in ROUTES {
            let Some((_, category, limit)) = rate_limit_for(row.handler) else {
                continue;
            };
            let expected = match category {
                "auth" | "oauth_start" => RateLimit::AUTH,
                "refresh" => RateLimit::REFRESH,
                "auth_read" => RateLimit::API_READ,
                "auth_write" => RateLimit::API_WRITE,
                other => panic!("unexpected category {other} for {}", row.template),
            };
            assert_eq!(
                limit.max_requests, expected.max_requests,
                "{}",
                row.template
            );
            assert_eq!(limit.window, expected.window, "{}", row.template);
        }
    }
}

#[cfg(test)]
mod table_tests {
    use wafer_run::Block as _;

    use super::*;

    /// `info().endpoints` is generated from `ROUTES`; nothing else declares
    /// an endpoint for this block.
    #[test]
    fn info_endpoints_come_from_the_table() {
        let declared = AuthUiBlock::new().info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }

    /// [B14] `POST /b/auth/api/oauth/sync-user` is gone: neither declared nor
    /// dispatched.
    ///
    /// It created a user from an email behind an `x-internal-secret` header
    /// checked against `WAFER_RUN__AUTH__INTERNAL_SECRET` — a config var no
    /// `ConfigVar` declared and `auth_grants()` granted no `Config` read for,
    /// so under WRAP the read returned `""` and the handler answered 403 to
    /// everyone. Nothing in `crates/`, `packages/`, `examples/`, `docs/` or
    /// `.github/` ever called it, and what it did (create-or-find a user by
    /// email for an OAuth identity) is what `oauth::callback::resolve_user`
    /// does behind PKCE. Declaring and granting the secret would have kept an
    /// unauthenticated-shaped user-creation surface alive for a caller that
    /// does not exist.
    #[test]
    fn sync_user_is_neither_declared_nor_dispatched() {
        assert!(
            !ROUTES
                .iter()
                .any(|r| r.template == "/b/auth/api/oauth/sync-user"),
            "the sync-user row must be gone from the declaration"
        );

        let mut msg = Message::new("http.request");
        msg.set_meta("req.action", "create");
        msg.set_meta("req.resource", "/b/auth/api/oauth/sync-user");
        assert!(
            endpoint_match::dispatch(&mut msg, ROUTES).is_none(),
            "no row may match the deleted path"
        );
    }
}

#[cfg(test)]
mod outbound_mail_wiring_tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use wafer_run::{Block, InputStream, Message, OutputStream};

    use super::*;
    use crate::{
        blocks::auth::repo::users::{self, NewUser},
        test_support::{anon_msg, output_json, TestContext},
    };

    /// Stands in for `impresspress/email` and counts the sends that reached
    /// it. Answers `{"sent": true}`, so nothing here is refused for any
    /// reason other than the budget under test.
    struct CountingEmail(Arc<AtomicUsize>);

    #[wafer_block::wafer_async_trait]
    impl Block for CountingEmail {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("impresspress/email", "0.0.1", "service@v1", "counting stub")
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            assert_eq!(msg.kind, "email.send_template");
            self.0.fetch_add(1, Ordering::SeqCst);
            ok_json(&serde_json::json!({ "sent": true }))
        }
    }

    /// The budget only bounds anything if `handle()` hands the send helper
    /// the block's OWN limiter and the request's own `Message`. A throwaway
    /// `UserRateLimiter::new()` per request, or a message carrying no client
    /// IP, would leave every unit test in `api::tests` green while the limit
    /// did nothing per request — so this drives the real route, repeatedly,
    /// through `routing::route_to_block`, and counts what reached the email
    /// block.
    #[tokio::test]
    async fn the_mail_budget_survives_across_requests_to_the_real_route() {
        let mut ctx = TestContext::with_auth_and_crypto().await;
        users::insert(
            &ctx,
            NewUser {
                email: "known@example.com".into(),
                display_name: "Known".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: true,
                verification_token_hash: None,
            },
        )
        .await
        .expect("insert user");

        let sends = Arc::new(AtomicUsize::new(0));
        ctx.register_block(
            "impresspress/email",
            Arc::new(CountingEmail(Arc::clone(&sends))),
        );
        ctx.register_block("impresspress/auth-ui", Arc::new(AuthUiBlock::new()));

        let budget = crate::blocks::rate_limit::RateLimit::AUTH_EMAIL.max_requests as usize;
        let mut bodies = Vec::new();
        // The send runs after each response; run it, as the platform would,
        // before the next request.
        crate::deferred::queue_for_test();
        for _ in 0..budget + 1 {
            let mut msg = anon_msg("create", "/b/auth/api/forgot-password");
            msg.set_meta(wafer_block::meta::META_REQ_CLIENT_IP, "203.0.113.9");
            let body = serde_json::to_vec(&serde_json::json!({ "email": "known@example.com" }))
                .expect("serialize body");
            bodies.push(
                output_json(
                    ctx.dispatch_resolved_with_input(msg, InputStream::from_bytes(body))
                        .await,
                )
                .await,
            );
            api::run_deferred().await;
        }

        assert_eq!(
            sends.load(Ordering::SeqCst),
            budget,
            "the {budget}-per-window budget must be spent across requests, not reset by each one"
        );
        assert!(
            bodies.windows(2).all(|w| w[0] == w[1]),
            "the refused request must answer the same constant body as the others: a response \
             that changed once the budget ran out would be an enumeration oracle"
        );
    }
}

#[cfg(test)]
mod oauth_start_limit_tests {
    use std::sync::Arc;

    use wafer_core::clients::database as db;
    use wafer_run::{InputStream, Message};

    use super::*;
    use crate::{
        blocks::auth::repo::oauth_pkce,
        config_vars::ENABLE_OAUTH_KEY,
        test_support::{anon_msg, output_http_status, TestContext},
    };

    /// The start request a browser navigates to, from one client address.
    fn start_msg() -> Message {
        let mut msg = anon_msg("retrieve", "/b/auth/oauth/login");
        msg.set_meta("req.query.provider", "google");
        msg.set_meta(wafer_block::meta::META_REQ_CLIENT_IP, "203.0.113.21");
        msg
    }

    /// Every OAuth start writes a PKCE state row, and nothing but a callback
    /// or a later maintenance sweep removes it. Driven through the real route
    /// with OAuth enabled and a provider configured, so each request below the
    /// limit genuinely starts a flow: the one past it must be refused before it
    /// writes a row.
    #[tokio::test]
    async fn oauth_start_is_ip_rate_limited_before_it_writes_state() {
        let mut ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.set_config(ENABLE_OAUTH_KEY, "true");
        ctx.set_config(OAUTH_GOOGLE_CLIENT_ID_KEY, "client-id");
        ctx.register_block("impresspress/auth-ui", Arc::new(AuthUiBlock::new()));

        let budget = RateLimit::AUTH.max_requests as usize;
        for n in 0..budget {
            let status = output_http_status(
                ctx.dispatch_resolved_with_input(start_msg(), InputStream::empty())
                    .await,
            )
            .await;
            assert_eq!(
                status, 302,
                "request {n} is inside the budget and starts a flow"
            );
        }
        let status = output_http_status(
            ctx.dispatch_resolved_with_input(start_msg(), InputStream::empty())
                .await,
        )
        .await;
        assert_eq!(status, 429, "the request past the budget must be refused");

        let rows = db::count(&ctx, oauth_pkce::TABLE, &[])
            .await
            .expect("count PKCE rows");
        assert_eq!(
            rows, budget as i64,
            "the refused request must not have written a PKCE state row"
        );
    }

    /// OAuth starts spend their own bucket. A client address that has used
    /// up the OAuth-start budget can still try a password login.
    #[tokio::test]
    async fn oauth_starts_do_not_spend_the_login_budget() {
        let mut ctx = TestContext::with_auth_and_crypto().await;
        ctx.set_config(ENABLE_OAUTH_KEY, "true");
        ctx.set_config(OAUTH_GOOGLE_CLIENT_ID_KEY, "client-id");
        ctx.register_block("impresspress/auth-ui", Arc::new(AuthUiBlock::new()));

        for _ in 0..=RateLimit::AUTH.max_requests {
            output_http_status(
                ctx.dispatch_resolved_with_input(start_msg(), InputStream::empty())
                    .await,
            )
            .await;
        }
        assert_eq!(
            output_http_status(
                ctx.dispatch_resolved_with_input(start_msg(), InputStream::empty())
                    .await,
            )
            .await,
            429,
            "precondition: the OAuth-start budget is spent"
        );

        let mut login = anon_msg("create", "/b/auth/api/login");
        login.set_meta(wafer_block::meta::META_REQ_CLIENT_IP, "203.0.113.21");
        let body = serde_json::to_vec(&serde_json::json!({
            "email": "nobody@example.com",
            "password": "not-the-password",
        }))
        .expect("serialize body");
        let status = output_http_status(
            ctx.dispatch_resolved_with_input(login, InputStream::from_bytes(body))
                .await,
        )
        .await;
        assert_eq!(
            status, 401,
            "the login is judged on its credentials, not refused for the OAuth starts"
        );
    }
}
