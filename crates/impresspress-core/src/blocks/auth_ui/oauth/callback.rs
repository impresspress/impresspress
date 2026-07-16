//! GET /b/auth/oauth/callback — relocated from auth/oauth.rs::handle_oauth_callback
//! in Task 5.

use std::collections::HashMap;

use wafer_core::clients::{config, database as db, network};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{
        auth::{
            helpers::{
                email_domain_allowed, ensure_admin_role, initial_role_for, issue_tokens_and_cookie,
                signup_allowed,
            },
            repo::{oauth_pkce, provider_links, users},
            USERS_TABLE,
        },
        auth_ui::redirect::{default_post_login_redirect, is_safe_local_redirect},
    },
    http::{err_bad_request, err_forbidden, err_internal, err_internal_no_cause, ResponseBuilder},
    util::json_map,
};

pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    // Check ENABLE_OAUTH flag
    let enable_oauth = config::get_default(ctx, "WAFER_RUN_SHARED__ENABLE_OAUTH", "false").await;
    if enable_oauth != "true" && enable_oauth != "1" {
        return err_forbidden("OAuth login is not enabled");
    }

    let code = msg.query("code");
    let state = msg.query("state");
    if code.is_empty() || state.is_empty() {
        return err_bad_request("Missing code or state parameter");
    }

    // SEC-040: look up the server-side PKCE state by the opaque `state_id`
    // the provider echoed back. `take` is single-use (DELETE … RETURNING),
    // so a replayed callback or a stolen state_id can only redeem once,
    // and a state past `expires_at` is treated as missing.
    let pkce_row = match oauth_pkce::take(ctx, state).await {
        Ok(Some(row)) => row,
        Ok(None) => return err_bad_request("Invalid or expired OAuth state"),
        Err(e) => return err_internal("OAuth state lookup failed", e),
    };
    let provider = pkce_row.provider.clone();
    let code_verifier = pkce_row.code_verifier.clone();
    // Use the redirect_uri stored at start-time so the provider's exact-
    // match check passes even if the live config changed mid-flow.
    let redirect_uri = pkce_row.redirect_uri.clone();

    let Some(spec) = super::spec::lookup(&provider) else {
        return err_bad_request("Unsupported OAuth provider");
    };

    let client_id = config::get_default(
        ctx,
        &format!(
            "IMPRESSPRESS__AUTH_UI__OAUTH_{}_CLIENT_ID",
            provider.to_uppercase()
        ),
        "",
    )
    .await;
    let client_secret = config::get_default(
        ctx,
        &format!(
            "IMPRESSPRESS__AUTH_UI__OAUTH_{}_CLIENT_SECRET",
            provider.to_uppercase()
        ),
        "",
    )
    .await;

    if client_id.is_empty() || client_secret.is_empty() {
        return err_internal_no_cause("OAuth provider not fully configured");
    }

    // Phase 1: exchange the authorization code for a provider access token.
    let oauth_token = match exchange_code(
        ctx,
        spec,
        code,
        &client_id,
        &client_secret,
        &redirect_uri,
        &code_verifier,
    )
    .await
    {
        Ok(t) => t,
        Err(r) => return r,
    };

    // Phase 2: fetch the user's profile (with GitHub's /user/emails fallback).
    let info = match fetch_user_info(ctx, spec, &oauth_token).await {
        Ok(i) => i,
        Err(r) => return r,
    };

    // Phase 3: resolve the local user (link / email-merge / create), enforcing
    // the disabled-account and signup gates, and upsert the provider link.
    let user_id = match resolve_user(ctx, &provider, &oauth_token, &info).await {
        Ok(id) => id,
        Err(r) => return r,
    };

    // Update last_login_at on the users row (best-effort).
    let upd = json_map(serde_json::json!({
        "last_login_at": crate::util::now_rfc3339()
    }));
    if let Err(e) = db::update(ctx, USERS_TABLE, &user_id, upd).await {
        tracing::warn!("Failed to update last_login_at: {e}");
    }

    let email = info.email;

    // A WRAP denial or DB error here must not silently resolve to "no
    // roles" — that would 403 an admin or double-grant on the next login
    // (SB-3).
    let roles = match ensure_admin_role(ctx, &user_id, &email).await {
        Ok(r) => r,
        Err(e) => return err_internal("Failed to resolve user roles", e),
    };

    // Mint tokens, persist the refresh + session rows, build the cookie via
    // the shared issuance tail. Previously this flow hand-rolled token minting
    // and *omitted* the session row, so OAuth logins were invisible on the
    // userportal device list; routing through `issue_tokens_and_cookie` fixes
    // that by construction.
    let issued = match issue_tokens_and_cookie(
        ctx,
        &user_id,
        &email,
        &roles,
        &format!("oauth.{provider}"),
        None,
        0,
    )
    .await
    {
        Ok(i) => i,
        Err(r) => return r,
    };

    // Redirect to frontend — token is set via HttpOnly cookie only (not URL)
    let frontend_url = config::get_default(
        ctx,
        "WAFER_RUN_SHARED__FRONTEND_URL",
        "http://localhost:5173",
    )
    .await;
    // [SEC-036] Validate FRONTEND_URL before plugging it into a Location
    // header — a misconfigured (or attacker-controlled) value here would
    // turn every OAuth callback into an open redirect.
    if !is_safe_frontend_url(&frontend_url) {
        tracing::error!(
            frontend_url = %frontend_url,
            "WAFER_RUN_SHARED__FRONTEND_URL failed validation; refusing OAuth redirect"
        );
        return err_internal_no_cause("Frontend URL is not configured correctly");
    }
    let post_login_raw =
        config::get_default(ctx, "WAFER_RUN_SHARED__POST_LOGIN_REDIRECT", "/b/admin/").await;
    let admin_default = if is_safe_local_redirect(&post_login_raw) {
        post_login_raw
    } else {
        "/b/admin/".to_string()
    };
    // Role-aware default (#1 onboarding bug fix): a non-admin OAuth login
    // must never default into the admin-only destination above — see
    // `redirect::default_post_login_redirect`.
    let is_admin = roles.iter().any(|r| r == "admin");
    let post_login = default_post_login_redirect(is_admin, &admin_default);
    let redirect_url = format!("{}{}", frontend_url.trim_end_matches('/'), post_login);

    ResponseBuilder::new()
        .status(302)
        .set_cookie(&issued.cookie)
        .set_header("Location", &redirect_url)
        .json(&serde_json::json!({"redirect": redirect_url}))
}

/// The profile fields the callback needs from a provider's userinfo response,
/// already normalised (email lowercased, missing strings empty).
struct OAuthUserInfo {
    /// Lowercased verified email (after the GitHub `/user/emails` fallback).
    email: String,
    /// Display name, empty if the provider omitted it.
    name: String,
    /// Avatar URL, empty if the provider omitted it.
    avatar: String,
    /// Stable provider-side user id (`sub` for Google, `id` for GitHub /
    /// Microsoft), coerced to a string.
    provider_ref: String,
    /// Per-provider login handle (GitHub `login`, else the email local-part).
    provider_login: String,
}

/// Phase 1 — exchange the authorization `code` for a provider access token.
///
/// POSTs the token-exchange body (PKCE verifier included where the provider
/// uses it) and returns the `access_token` string. Any transport / parse
/// failure or a missing token is mapped to a ready-to-return [`OutputStream`].
async fn exchange_code(
    ctx: &dyn Context,
    spec: &super::spec::OAuthProviderSpec,
    code: &str,
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<String, OutputStream> {
    let token_body_str =
        spec.build_token_body(code, client_id, client_secret, redirect_uri, code_verifier);

    let mut headers = HashMap::new();
    headers.insert(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );
    headers.insert("Accept".to_string(), "application/json".to_string());

    let token_body_bytes = token_body_str.into_bytes();
    let token_resp = match network::do_request(
        ctx,
        "POST",
        spec.token_url,
        &headers,
        Some(&token_body_bytes),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return Err(err_internal("Token exchange failed", e)),
    };

    let token_data: serde_json::Value = match serde_json::from_slice(&token_resp.body) {
        Ok(d) => d,
        Err(_) => return Err(err_internal_no_cause("Failed to parse token response")),
    };

    let access_token_oauth = token_data
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if access_token_oauth.is_empty() {
        return Err(err_internal_no_cause("No access token in OAuth response"));
    }
    Ok(access_token_oauth.to_string())
}

/// Phase 2 — fetch and normalise the user's profile.
///
/// Calls the provider userinfo endpoint, then (for providers with an
/// `emails_url`, i.e. GitHub) falls back to `/user/emails` for a verified
/// primary address when the userinfo payload omits one. Returns the normalised
/// [`OAuthUserInfo`]; a missing email or stable id is an error.
async fn fetch_user_info(
    ctx: &dyn Context,
    spec: &super::spec::OAuthProviderSpec,
    oauth_token: &str,
) -> Result<OAuthUserInfo, OutputStream> {
    // Shared header set for every provider API call. GitHub's REST API rejects
    // requests without a User-Agent header (returns 403 + an HTML error body);
    // other providers accept it.
    let api_headers = || {
        let mut h = HashMap::new();
        h.insert(
            "Authorization".to_string(),
            spec.userinfo_auth_header(oauth_token),
        );
        h.insert("Accept".to_string(), "application/json".to_string());
        h.insert(
            "User-Agent".to_string(),
            concat!("impresspress-auth/", env!("CARGO_PKG_VERSION")).to_string(),
        );
        h
    };

    let info_resp =
        match network::do_request(ctx, "GET", spec.userinfo_url, &api_headers(), None).await {
            Ok(r) => r,
            Err(e) => return Err(err_internal("User info request failed", e)),
        };

    let user_info: serde_json::Value = match serde_json::from_slice(&info_resp.body) {
        Ok(d) => d,
        Err(e) => {
            // Log the SHA-256 hash of the body instead of the body itself —
            // a parse failure is rare and the raw body typically contains
            // the upstream email / provider IDs that we don't want to drop
            // into the error log surface.
            let body_hash = crate::util::sha256_hex(&info_resp.body);
            return Err(err_internal(
                "Failed to parse OAuth user info",
                format!(
                    "status={} parse={} body_len={} body_sha256={}",
                    info_resp.status_code,
                    e,
                    info_resp.body.len(),
                    body_hash
                ),
            ));
        }
    };

    let mut email = user_info
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let name = user_info
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let avatar = user_info
        .get("picture")
        .or_else(|| user_info.get("avatar_url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // GitHub's /user endpoint returns a null `email` for users who have
    // their primary email set to private. The authoritative list lives at
    // /user/emails — which is only returned when the `user:email` scope
    // was granted. Pick the first primary verified address. Only providers
    // with an `emails_url` (GitHub) carry this fallback.
    if let (true, Some(emails_url)) = (email.is_empty(), spec.emails_url) {
        if let Ok(emails_resp) =
            network::do_request(ctx, "GET", emails_url, &api_headers(), None).await
        {
            if let Ok(arr) = serde_json::from_slice::<serde_json::Value>(&emails_resp.body) {
                if let Some(entries) = arr.as_array() {
                    // Prefer primary+verified; fall back to any verified.
                    let pick = entries
                        .iter()
                        .find(|e| {
                            e.get("primary").and_then(|v| v.as_bool()).unwrap_or(false)
                                && e.get("verified").and_then(|v| v.as_bool()).unwrap_or(false)
                        })
                        .or_else(|| {
                            entries.iter().find(|e| {
                                e.get("verified").and_then(|v| v.as_bool()).unwrap_or(false)
                            })
                        });
                    if let Some(e) = pick {
                        if let Some(s) = e.get("email").and_then(|v| v.as_str()) {
                            email = s.to_lowercase();
                        }
                    }
                }
            }
        }
    }

    if email.is_empty() {
        return Err(err_internal_no_cause("No email returned by OAuth provider"));
    }

    // Extract the stable provider-side user identifier.
    // GitHub returns `id` as a JSON number; Google returns `sub` (string);
    // Microsoft returns `id` (string). Coerce to string in all cases.
    let provider_ref = match user_info.get("sub").or_else(|| user_info.get("id")) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    if provider_ref.is_empty() {
        return Err(err_internal_no_cause(
            "OAuth provider did not return a stable user id",
        ));
    }

    // Stable per-provider handle (GitHub `login`, others fall back to email local-part).
    let provider_login = user_info
        .get("login")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| email.split('@').next().unwrap_or(""))
        .to_string();

    Ok(OAuthUserInfo {
        email,
        name,
        avatar,
        provider_ref,
        provider_login,
    })
}

/// Phase 3 — resolve the local user id for this OAuth identity.
///
/// Tries, in order: an existing `(provider, provider_ref)` link; an
/// email-matched local account (rejected if disabled); otherwise creates a new
/// user (subject to the shared signup gates). Then upserts the provider link.
/// Returns the resolved local user id.
async fn resolve_user(
    ctx: &dyn Context,
    provider: &str,
    oauth_token: &str,
    info: &OAuthUserInfo,
) -> Result<String, OutputStream> {
    // --- Step 1: look up existing link by (provider, provider_ref) ---
    let existing_link =
        match provider_links::find_by_provider_ref(ctx, provider, &info.provider_ref).await {
            Ok(l) => l,
            Err(e) => return Err(err_internal("provider_links lookup failed", e)),
        };

    // --- Step 2 / 3: resolve user_id ---
    let user_id: String = if let Some(link) = existing_link {
        // Known provider link — reuse the bound user.
        link.user_id
    } else {
        // No link yet. Try email-based account merging.
        match users::find_by_email(ctx, &info.email).await {
            Ok(Some(existing_user)) => {
                // (removed) the per-branch `if !existing_user.is_active()` check
                // — the shared gate below now covers every branch.
                existing_user.id
            }
            Ok(None) => {
                // Brand-new user — enforce signup gates. Shared with the JSON
                // signup handler so the ALLOW_SIGNUP / ALLOWED_EMAIL_DOMAINS /
                // bootstrap-admin rules can't drift between the two flows.
                if !signup_allowed(ctx).await {
                    return Err(err_forbidden("Signups are currently disabled"));
                }

                if !email_domain_allowed(ctx, &info.email).await {
                    return Err(err_forbidden(
                        "Signups from this email domain are not allowed",
                    ));
                }

                // Determine role: admin if email matches bootstrap email.
                let role = initial_role_for(ctx, &info.email).await;

                let display_name = if info.name.is_empty() {
                    info.email.clone()
                } else {
                    info.name.clone()
                };
                let new_user = users::NewUser {
                    email: info.email.clone(),
                    display_name,
                    avatar_url: if info.avatar.is_empty() {
                        None
                    } else {
                        Some(info.avatar.clone())
                    },
                    role: role.to_string(),
                };
                match users::insert(ctx, new_user).await {
                    Ok(u) => {
                        // Assign role row in USER_ROLES_TABLE for legacy readers.
                        let role_data = json_map(serde_json::json!({
                            "user_id": u.id,
                            "role": role,
                            "assigned_at": crate::util::now_rfc3339()
                        }));
                        if let Err(e) =
                            db::create(ctx, crate::blocks::admin::USER_ROLES_TABLE, role_data).await
                        {
                            tracing::warn!("Failed to assign default role on OAuth signup: {e}");
                        }
                        u.id
                    }
                    Err(e) => return Err(err_internal("Failed to create user", e)),
                }
            }
            Err(e) => return Err(err_internal("User lookup failed", e)),
        }
    };

    // --- Lifecycle gate (single enforcement point) ---
    // Every branch above (existing link, email merge, new signup) converges
    // here. Verify the resolved account may authenticate BEFORE mutating the
    // provider link or issuing tokens. `is_active()` covers both `disabled`
    // and soft-delete (`deleted_at`). The existing-link branch previously
    // skipped this, letting disabled/deleted linked accounts re-authenticate.
    match users::find_by_id(ctx, &user_id).await {
        Ok(Some(u)) if u.is_active() => {}
        Ok(Some(_)) => return Err(err_forbidden("Account is disabled")),
        Ok(None) => return Err(err_forbidden("Account not found")),
        Err(e) => return Err(err_internal("User lookup failed", e)),
    }

    // --- Step 4: upsert the provider_links row ---
    if let Err(e) = provider_links::upsert(
        ctx,
        provider_links::NewLink {
            provider,
            provider_ref: &info.provider_ref,
            user_id: &user_id,
            provider_login: &info.provider_login,
            access_token: oauth_token,
        },
    )
    .await
    {
        // Log but don't fail — the user is authenticated; link persistence
        // is best-effort metadata. A failed upsert means re-login will
        // fall back to email-based merging on next attempt.
        tracing::warn!("Failed to upsert provider_links: {e}");
    }

    Ok(user_id)
}

/// [SEC-036] Validates `WAFER_RUN_SHARED__FRONTEND_URL` before it is used as
/// the origin half of an OAuth callback redirect.
///
/// The OAuth flow ends by issuing a `302 Location: {frontend_url}{post_login}`.
/// If `frontend_url` is attacker-controlled (admin UI mistake, env-var
/// injection, copy-paste of a phishing URL) this becomes an open redirect that
/// piggybacks on the trusted authentication step.
///
/// Accept only:
/// - `https://<host>` (any non-empty host), OR
/// - `http://localhost[:port]` / `http://127.0.0.1[:port]` for local dev.
///
/// Reject anything with a path beyond `/`, any query, any fragment, anything
/// containing CRLF/tab/other control characters, or any non-http(s) scheme.
fn is_safe_frontend_url(s: &str) -> bool {
    // Reject control characters outright — they enable header-injection even
    // if the rest of the URL parses cleanly.
    if s.chars().any(|c| c.is_control()) {
        return false;
    }
    let Ok(parsed) = url::Url::parse(s) else {
        return false;
    };
    let host = match parsed.host_str() {
        Some(h) if !h.is_empty() => h,
        _ => return false,
    };
    match parsed.scheme() {
        "https" => {}
        "http" => {
            if !(host == "localhost" || host == "127.0.0.1" || host == "[::1]") {
                return false;
            }
        }
        _ => return false,
    }
    // Forbid an embedded path — the redirect formats as
    // `{frontend_url}{post_login}` where post_login already starts with `/`.
    // Allowing a path on frontend_url would invite double-slashes and
    // injection of an unexpected prefix.
    if !(parsed.path().is_empty() || parsed.path() == "/") {
        return false;
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::is_safe_frontend_url;

    #[test]
    fn accepts_https_origins() {
        assert!(is_safe_frontend_url("https://app.example.com"));
        assert!(is_safe_frontend_url("https://app.example.com/"));
        assert!(is_safe_frontend_url("https://app.example.com:8443"));
    }

    #[test]
    fn accepts_http_localhost_for_dev() {
        assert!(is_safe_frontend_url("http://localhost:5173"));
        assert!(is_safe_frontend_url("http://localhost"));
        assert!(is_safe_frontend_url("http://127.0.0.1:3000"));
        assert!(is_safe_frontend_url("http://[::1]:5173"));
    }

    #[test]
    fn rejects_http_non_localhost() {
        assert!(!is_safe_frontend_url("http://evil.com"));
        assert!(!is_safe_frontend_url("http://example.com"));
    }

    #[test]
    fn rejects_non_http_schemes() {
        assert!(!is_safe_frontend_url("javascript:alert(1)"));
        assert!(!is_safe_frontend_url("data:text/html,<script>x</script>"));
        assert!(!is_safe_frontend_url("file:///etc/passwd"));
        assert!(!is_safe_frontend_url("ftp://example.com"));
    }

    #[test]
    fn rejects_paths_and_queries_and_fragments() {
        assert!(!is_safe_frontend_url("https://example.com/path"));
        assert!(!is_safe_frontend_url("https://example.com/?q=1"));
        assert!(!is_safe_frontend_url("https://example.com/#frag"));
    }

    #[test]
    fn rejects_empty_host() {
        assert!(!is_safe_frontend_url(""));
        assert!(!is_safe_frontend_url("https://"));
        assert!(!is_safe_frontend_url("not a url"));
    }

    #[test]
    fn rejects_control_characters() {
        assert!(!is_safe_frontend_url(
            "https://example.com\r\nLocation: https://evil.com"
        ));
        assert!(!is_safe_frontend_url("https://example.com\n"));
    }
}

/// End-to-end regression tests for the OAuth callback's two historical
/// security drifts (audit Top-10 #4):
///
/// 1. OAuth logins never created a session row, so they were invisible on the
///    userportal device list. [`oauth_login_creates_session_row`] proves the
///    row now exists after a successful Google callback.
/// 2. Disabled accounts could still authenticate via OAuth because the
///    callback checked `role == "disabled"` (a value nothing ever writes)
///    instead of the real `UserRow.disabled` flag.
///    [`disabled_user_cannot_oauth_in`] proves a disabled account is rejected
///    and no session is minted.
///
/// Both drive the real [`handle`] end-to-end through a mock `wafer-run/network`
/// block that returns canned Google token + userinfo responses, so a future
/// refactor that re-breaks either path fails here.
#[cfg(test)]
mod security_regression_tests {
    use std::{collections::HashMap, sync::Arc};

    use async_trait::async_trait;
    use wafer_core::interfaces::network::service::{
        NetworkError, NetworkService, Request, Response,
    };
    use wafer_run::{Block, Message};

    use super::handle;
    use crate::{
        blocks::auth::repo::{oauth_pkce, sessions, users},
        test_support::TestContext,
    };

    /// Mock network block: maps Google's token + userinfo URLs to canned JSON.
    /// The userinfo email is fixed so tests can pre-seed a matching user.
    struct MockGoogleNetwork {
        userinfo_email: String,
    }

    #[async_trait]
    impl NetworkService for MockGoogleNetwork {
        async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
            let body = if req.url.contains("oauth2.googleapis.com/token") {
                serde_json::json!({ "access_token": "mock-google-access-token" })
            } else if req.url.contains("googleapis.com/oauth2/v2/userinfo") {
                serde_json::json!({
                    "sub": "google-user-123",
                    "email": self.userinfo_email,
                    "name": "Mock Google User",
                })
            } else {
                return Err(NetworkError::Other(format!("unexpected URL: {}", req.url)));
            };
            Ok(Response {
                status_code: 200,
                headers: HashMap::new(),
                body: serde_json::to_vec(&body).unwrap(),
            })
        }
    }

    /// Build a ctx with auth migrations, a crypto block (token minting), a mock
    /// Google network block, OAuth enabled, and a seeded PKCE state row so the
    /// callback's single-use state redemption succeeds.
    ///
    /// `extra_config` is folded into the same `wafer-run/config` block as the
    /// OAuth flags below (e.g. `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL`
    /// for the role-aware-redirect tests) — it can't be layered on afterward
    /// via `TestContext::set_config`, which would replace this block wholesale
    /// and drop the OAuth flags the callback needs to get past its own gates.
    async fn ctx_for_oauth(userinfo_email: &str, extra_config: &[(&str, &str)]) -> TestContext {
        let mut ctx = TestContext::with_auth().await;

        // Crypto block — issue_tokens_and_cookie signs JWTs and pulls random
        // bytes for the rotation family / jti.
        let crypto_svc = Arc::new(
            wafer_block_crypto::service::Argon2JwtCryptoService::new(
                "test-jwt-secret-padded-to-min-32-bytes-aaaa".to_string(),
            )
            .expect("test secret is long enough"),
        );
        let crypto_block: Arc<dyn Block> = Arc::new(
            wafer_core::service_blocks::crypto::CryptoBlock::new(crypto_svc),
        );
        ctx.register_block("wafer-run/crypto", crypto_block);

        // Mock network block under the production block id.
        let net: Arc<dyn Block> = Arc::new(wafer_core::service_blocks::network::NetworkBlock::new(
            Arc::new(MockGoogleNetwork {
                userinfo_email: userinfo_email.to_string(),
            }),
        ));
        ctx.register_block("wafer-run/network", net);

        // Config block — the handler reads OAuth flags / client credentials via
        // `config::get_default`, which dispatches to the `wafer-run/config`
        // block (NOT the TestContext config_get snapshot). Register one backed
        // by an override map seeded with what the callback needs before it will
        // attempt the code exchange.
        use wafer_core::{
            interfaces::config::service::ConfigService,
            service_blocks::config::{ConfigBlock, EnvConfigService},
        };
        let cfg_svc = EnvConfigService::new();
        cfg_svc.set("WAFER_RUN_SHARED__ENABLE_OAUTH", "true");
        cfg_svc.set("IMPRESSPRESS__AUTH_UI__OAUTH_GOOGLE_CLIENT_ID", "client-id");
        cfg_svc.set(
            "IMPRESSPRESS__AUTH_UI__OAUTH_GOOGLE_CLIENT_SECRET",
            "client-secret",
        );
        for (k, v) in extra_config {
            cfg_svc.set(k, v);
        }
        let cfg_block: Arc<dyn Block> = Arc::new(ConfigBlock::new(Arc::new(cfg_svc)));
        ctx.register_block("wafer-run/config", cfg_block);

        // Seed a single-use PKCE state row keyed by the `state` query param.
        let expires = (chrono::Utc::now() + chrono::Duration::minutes(10))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        oauth_pkce::insert(
            &ctx,
            oauth_pkce::NewPkceState {
                state_id: "state-xyz",
                provider: "google",
                code_verifier: "verifier-abc",
                redirect_uri: "https://app.example.com/b/auth/oauth/callback",
                expires_at: &expires,
            },
        )
        .await
        .expect("seed pkce state");

        ctx
    }

    /// Build the callback request message carrying `code` + `state` query
    /// params (read via `msg.query(...)` → `req.query.*` meta).
    fn callback_msg() -> Message {
        let mut msg = Message::new("auth.oauth.callback");
        msg.set_meta("req.query.code", "auth-code-123");
        msg.set_meta("req.query.state", "state-xyz");
        msg
    }

    #[tokio::test]
    async fn oauth_login_creates_session_row() {
        // No pre-existing user: the callback creates one, then must persist a
        // session row (the drift that made OAuth logins invisible on the
        // userportal device list).
        let email = "newoauth@example.com";
        let ctx = ctx_for_oauth(email, &[]).await;

        let out = handle(&ctx, &callback_msg()).await;
        let status = crate::test_support::output_status(out).await;
        assert_eq!(status, 302, "successful OAuth callback should 302-redirect");

        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("OAuth callback created the user");

        let session_rows = sessions::list_for_user(&ctx, &user.id)
            .await
            .expect("list sessions ok");
        assert_eq!(
            session_rows.len(),
            1,
            "OAuth login must persist exactly one session row (regression: it persisted none)"
        );
    }

    /// #1 onboarding bug fix: a brand-new non-admin OAuth login must default
    /// into `/b/userportal/`, not the admin-only `/b/admin/` default.
    #[tokio::test]
    async fn oauth_login_non_admin_redirects_to_userportal() {
        let email = "oauthuser@example.com";
        let ctx = ctx_for_oauth(email, &[]).await;

        let out = handle(&ctx, &callback_msg()).await;
        let location = crate::test_support::output_header(out, "Location")
            .await
            .expect("302 redirect must set a Location header");
        assert!(
            location.ends_with("/b/userportal/"),
            "non-admin OAuth login must default to the user portal, not the \
             admin-only route: {location}"
        );
    }

    /// Companion to the above: an admin (email matches the configured
    /// bootstrap admin email) still gets the operator-configured admin
    /// default — the fix is role-aware, not a blanket redirect change.
    #[tokio::test]
    async fn oauth_login_admin_email_redirects_to_admin_home() {
        let email = "oauthadmin@example.com";
        let ctx = ctx_for_oauth(
            email,
            &[("WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL", email)],
        )
        .await;

        let out = handle(&ctx, &callback_msg()).await;
        let location = crate::test_support::output_header(out, "Location")
            .await
            .expect("302 redirect must set a Location header");
        assert!(
            location.ends_with("/b/admin/"),
            "admin OAuth login must still default to the admin home: {location}"
        );
    }

    #[tokio::test]
    async fn disabled_user_cannot_oauth_in() {
        // Seed a DISABLED user with the email the provider will return.
        let email = "disabled@example.com";
        let ctx = ctx_for_oauth(email, &[]).await;

        let user = users::insert(
            &ctx,
            users::NewUser {
                email: email.to_string(),
                display_name: "Disabled User".to_string(),
                avatar_url: None,
                role: "user".to_string(),
            },
        )
        .await
        .expect("seed user");
        // Flip the real `disabled` flag (the value the fixed check reads).
        let mut upd = crate::util::json_map(serde_json::json!({ "disabled": true }));
        crate::util::stamp_updated(&mut upd);
        wafer_core::clients::database::update(
            &ctx,
            crate::blocks::auth::USERS_TABLE,
            &user.id,
            upd,
        )
        .await
        .expect("disable user");
        // Sanity: the typed row now reports disabled.
        assert!(
            users::find_by_id(&ctx, &user.id)
                .await
                .unwrap()
                .unwrap()
                .disabled,
            "fixture user must be disabled"
        );

        // The callback rejects with a PermissionDenied error stream (mapped to
        // HTTP 403 at the boundary). Before the fix this returned a 302 login.
        let out = handle(&ctx, &callback_msg()).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "disabled account must be rejected at the OAuth callback (regression: it logged in)"
        );

        // And no session row was minted for the disabled user.
        let session_rows = sessions::list_for_user(&ctx, &user.id)
            .await
            .expect("list sessions ok");
        assert!(
            session_rows.is_empty(),
            "no session may be created for a disabled OAuth login"
        );
    }

    /// Regression test for the whole-branch review finding: credential
    /// *issuance* paths (login / refresh / OAuth) gated on `user.disabled`
    /// only, not on soft-delete. `db::soft_delete` leaves `local_credentials`
    /// and refresh tokens intact, so a soft-deleted user could still
    /// authenticate via OAuth email-matching and mint fresh tokens. The fix
    /// replaces `existing_user.disabled` with `!existing_user.is_active()`,
    /// which also covers `is_deleted()`.
    #[tokio::test]
    async fn soft_deleted_user_cannot_oauth_in() {
        // Seed a SOFT-DELETED (but not `disabled`) user with the email the
        // provider will return.
        let email = "softdeleted@example.com";
        let ctx = ctx_for_oauth(email, &[]).await;

        let user = users::insert(
            &ctx,
            users::NewUser {
                email: email.to_string(),
                display_name: "Soft Deleted User".to_string(),
                avatar_url: None,
                role: "user".to_string(),
            },
        )
        .await
        .expect("seed user");
        // Set `deleted_at` (soft-delete marker) — NOT `disabled`. Mirrors the
        // Task-1/2 lifecycle tests in `auth/repo/users.rs`.
        let mut upd = crate::util::json_map(serde_json::json!({
            "deleted_at": "2026-01-01T00:00:00Z"
        }));
        crate::util::stamp_updated(&mut upd);
        wafer_core::clients::database::update(
            &ctx,
            crate::blocks::auth::USERS_TABLE,
            &user.id,
            upd,
        )
        .await
        .expect("soft-delete user");
        // Sanity: the typed row now reports deleted/inactive but NOT disabled.
        let row = users::find_by_id(&ctx, &user.id).await.unwrap().unwrap();
        assert!(row.is_deleted(), "fixture user must be soft-deleted");
        assert!(!row.disabled, "fixture user must not be `disabled`");
        assert!(!row.is_active(), "soft-deleted user must not be active");

        // The callback must reject with a PermissionDenied error stream (mapped
        // to HTTP 403 at the boundary), the same as a disabled account. Before
        // the fix this only checked `existing_user.disabled` and returned a
        // 302 login for a soft-deleted user.
        let out = handle(&ctx, &callback_msg()).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "soft-deleted account must be rejected at the OAuth callback (regression: it logged in)"
        );

        // And no session row was minted for the soft-deleted user.
        let session_rows = sessions::list_for_user(&ctx, &user.id)
            .await
            .expect("list sessions ok");
        assert!(
            session_rows.is_empty(),
            "no session may be created for a soft-deleted OAuth login"
        );
    }

    /// Whole-branch regression: the existing-provider-link path reused
    /// `link.user_id` with NO lifecycle check. A disabled user who already had
    /// a provider link could re-authenticate and mint fresh tokens. The shared
    /// post-resolution gate must reject them.
    #[tokio::test]
    async fn disabled_pre_linked_user_cannot_oauth_in() {
        let email = "disabled-linked@example.com";
        let ctx = ctx_for_oauth(email, &[]).await;

        let user = users::insert(
            &ctx,
            users::NewUser {
                email: email.to_string(),
                display_name: "Disabled Linked User".to_string(),
                avatar_url: None,
                role: "user".to_string(),
            },
        )
        .await
        .expect("seed user");

        // Pre-existing provider link → callback takes the existing-link branch.
        // provider_ref must match the mock userinfo `sub` ("google-user-123").
        crate::blocks::auth::repo::provider_links::upsert(
            &ctx,
            crate::blocks::auth::repo::provider_links::NewLink {
                provider: "google",
                provider_ref: "google-user-123",
                user_id: &user.id,
                provider_login: "disabled-linked",
                access_token: "old-token",
            },
        )
        .await
        .expect("seed provider link");

        // Disable AFTER linking.
        let mut upd = crate::util::json_map(serde_json::json!({ "disabled": true }));
        crate::util::stamp_updated(&mut upd);
        wafer_core::clients::database::update(
            &ctx,
            crate::blocks::auth::USERS_TABLE,
            &user.id,
            upd,
        )
        .await
        .expect("disable user");

        let out = handle(&ctx, &callback_msg()).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "disabled pre-linked account must be rejected (regression: it logged in)"
        );
        let session_rows = sessions::list_for_user(&ctx, &user.id)
            .await
            .expect("list sessions ok");
        assert!(
            session_rows.is_empty(),
            "no session for disabled pre-linked login"
        );
    }

    /// Same, but soft-deleted (deleted_at set, disabled=false) and pre-linked.
    #[tokio::test]
    async fn soft_deleted_pre_linked_user_cannot_oauth_in() {
        let email = "softdel-linked@example.com";
        let ctx = ctx_for_oauth(email, &[]).await;

        let user = users::insert(
            &ctx,
            users::NewUser {
                email: email.to_string(),
                display_name: "Soft Deleted Linked User".to_string(),
                avatar_url: None,
                role: "user".to_string(),
            },
        )
        .await
        .expect("seed user");
        crate::blocks::auth::repo::provider_links::upsert(
            &ctx,
            crate::blocks::auth::repo::provider_links::NewLink {
                provider: "google",
                provider_ref: "google-user-123",
                user_id: &user.id,
                provider_login: "softdel-linked",
                access_token: "old-token",
            },
        )
        .await
        .expect("seed provider link");

        let mut upd = crate::util::json_map(serde_json::json!({
            "deleted_at": "2026-01-01T00:00:00Z"
        }));
        crate::util::stamp_updated(&mut upd);
        wafer_core::clients::database::update(
            &ctx,
            crate::blocks::auth::USERS_TABLE,
            &user.id,
            upd,
        )
        .await
        .expect("soft-delete user");

        let out = handle(&ctx, &callback_msg()).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "soft-deleted pre-linked account must be rejected (regression: it logged in)"
        );
        let session_rows = sessions::list_for_user(&ctx, &user.id)
            .await
            .expect("list sessions ok");
        assert!(
            session_rows.is_empty(),
            "no session for soft-deleted pre-linked login"
        );
    }
}
