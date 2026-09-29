//! GET /b/auth/oauth/callback — relocated from auth/oauth.rs::handle_oauth_callback
//! in Task 5.

use std::collections::HashMap;

use wafer_core::clients::{config, network};
use wafer_run::{context::Context, Message, OutputStream, WaferError};

use crate::{
    blocks::{
        auth::{
            config::REQUIRE_VERIFICATION_KEY,
            helpers::{
                email_domain_allowed, initial_role_for, issue_tokens_and_cookie, signup_allowed,
                touch_last_login_after_response, RoleSource, Rotation, SessionLifetime, TokenGrant,
            },
            repo::{oauth_pkce, provider_links, users},
        },
        auth_ui::redirect::{configured_admin_default, default_post_login_redirect},
        crud,
        errors::{impresspress_error_code_to_wafer, ErrorCode},
    },
    config_vars::{ENABLE_OAUTH_KEY, FRONTEND_URL_KEY},
    http::{err_bad_request, err_forbidden, err_internal, err_internal_no_cause, ResponseBuilder},
};

/// A refusal that also expires this flow's binding cookie.
///
/// Every answer a callback gives after it has matched a binding ends the
/// flow: the single-use state is gone either way, so the cookie that pointed
/// at it is spent. Leaving it behind means the browser keeps offering a
/// binding for a state that no longer exists, and — since each flow gets its
/// own cookie — a user who retries collects one per attempt.
///
/// A `WaferError` carries response meta the same way a success does
/// (`wafer_block::http_codec` renders both), so a refusal can set a cookie.
fn refuse(code: ErrorCode, message: &str, clear_binding: &str) -> OutputStream {
    let mut err = WaferError::new(impresspress_error_code_to_wafer(code), message.to_string())
        .with_detail_code(code.as_str());
    err.meta
        .push(wafer_block::response::cookie_meta(clear_binding));
    OutputStream::error(err)
}

/// The local account an OAuth identity resolved to: its row, as the
/// lifecycle gate read it.
///
/// The row's address matters on its own: everything downstream that keys on
/// "who is this" — the session's email claim, the bootstrap-admin comparison —
/// must read the row's address, not the one the provider reported. They
/// differ whenever a linked account's address has since changed, and a
/// provider that asserts nothing (`spec::EmailAssertion::None`) can report any
/// address at all. The row's `auth_version` is the one the session's access
/// token carries (see `TokenGrant`).
struct ResolvedAccount {
    user: users::UserRow,
}

pub async fn handle(
    limiter: &crate::blocks::rate_limit::UserRateLimiter,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    // Check ENABLE_OAUTH flag
    let enable_oauth = match crate::config_vars::get_bool(ctx, ENABLE_OAUTH_KEY, false).await {
        Ok(enabled) => enabled,
        Err(e) => return crud::db_error_internal(e, "Could not read the OAuth switch"),
    };
    if !enable_oauth {
        return err_forbidden("OAuth login is not enabled");
    }

    let code = msg.query("code");
    let state = msg.query("state");
    if code.is_empty() || state.is_empty() {
        return err_bad_request("Missing code or state parameter");
    }

    // The `state` is only half the proof. It says a flow this deployment
    // started is being completed; the binding cookie says it is being
    // completed by the browser that started it. Checked BEFORE the take, so a
    // forged callback cannot burn someone else's single-use state either —
    // and without clearing any cookie, since the binding it failed to present
    // belongs to a flow that may still be in flight in another tab.
    let bound = match super::state_binding::matches(ctx, msg, state).await {
        Ok(bound) => bound,
        Err(e) => return crud::db_error_internal(e, "Could not check the OAuth state binding"),
    };
    if !bound {
        return err_bad_request("OAuth state does not belong to this browser");
    }

    // From here the flow is spent however it ends. Every answer this handler
    // *decides* — the 302 and each `refuse` below — carries the expiry of its
    // binding cookie. The deployment's own failures do not — the
    // `err_internal` 500s, and the 403/429 `crud::db_error_internal` answers
    // for a database call WRAP refused or a quota stopped: the flow is
    // unfinished rather than concluded, and the cookie expires on its own
    // with the state it names.
    let clear_binding = match super::state_binding::clear(ctx, state).await {
        Ok(clear_binding) => clear_binding,
        Err(e) => return crud::db_error_internal(e, "Could not build the OAuth binding cookie"),
    };

    // Resolved before the state is taken: a misconfigured session lifetime is
    // the deployment's failure, so it answers a 500 and leaves the single-use
    // state (and its binding cookie) unspent (see `SessionLifetime`).
    let lifetime = match SessionLifetime::resolve_or_error(ctx).await {
        Ok(lifetime) => lifetime,
        Err(r) => return r,
    };

    // SEC-040: look up the server-side PKCE state by the opaque `state_id`
    // the provider echoed back. `take` is single-use (DELETE … RETURNING),
    // so a replayed callback or a stolen state_id can only redeem once,
    // and a state past `expires_at` is treated as missing.
    let pkce_row = match oauth_pkce::take(ctx, state).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return refuse(
                ErrorCode::InvalidInput,
                "Invalid or expired OAuth state",
                &clear_binding,
            )
        }
        Err(e) => return crud::db_error_internal(e, "OAuth state lookup failed"),
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
    let client_id = match client_id {
        Ok(client_id) => client_id,
        Err(e) => return crud::db_error_internal(e, "Could not read the OAuth client id"),
    };
    let client_secret = config::get_default(
        ctx,
        &format!(
            "IMPRESSPRESS__AUTH_UI__OAUTH_{}_CLIENT_SECRET",
            provider.to_uppercase()
        ),
        "",
    )
    .await;
    let client_secret = match client_secret {
        Ok(client_secret) => client_secret,
        Err(e) => return crud::db_error_internal(e, "Could not read the OAuth client secret"),
    };

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

    // Phase 2: fetch the user's profile, and what the provider promises
    // about the address on it. This is the provider token's only use: it is
    // dropped when this request ends and never stored (see
    // `auth::repo::provider_links`).
    let info = match fetch_user_info(ctx, spec, &oauth_token).await {
        Ok(i) => i,
        Err(r) => return r,
    };

    // Phase 3: resolve the local user (link / email-merge / create), enforcing
    // the signup, disabled-account and email-verification gates, and upsert
    // the provider link.
    let account = match resolve_user(limiter, ctx, msg, &provider, &info, &clear_binding).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let ResolvedAccount { user } = account;
    let (user_id, email) = (user.id.clone(), user.email.clone());

    // Update last_login_at on the users row (best-effort, after the response).
    touch_last_login_after_response(ctx, &user_id);

    // Bootstrap-admin promotion needs BOTH halves to be true, and neither is
    // free. The address compared is the ACCOUNT's, never the one the provider
    // reported — otherwise anyone able to name an address at a provider names
    // the one in `BOOTSTRAP_ADMIN_EMAIL` and is granted admin, on a fresh
    // account or grafted onto the one they already had. And the provider must
    // actually assert that address: Microsoft's `email` claim is a mutable
    // tenant attribute (`spec::EmailAssertion::None`), so a sign-in there
    // proves nothing that should move a role.
    //
    // A WRAP denial or DB error in either read must not silently resolve to
    // "no roles" — that would 403 an admin or double-grant on the next login
    // (SB-3).
    let source = if info.email_verified {
        RoleSource::CheckBootstrapAdmin { email: &email }
    } else {
        RoleSource::Stored
    };
    let grant = match TokenGrant::resolve(ctx, &user, source).await {
        Ok(grant) => grant,
        Err(e) => return crud::db_error_internal(e, "Failed to resolve user roles"),
    };

    // Mint tokens, persist the refresh + session rows, build the cookie via
    // the shared issuance tail. Previously this flow hand-rolled token minting
    // and *omitted* the session row, so OAuth logins were invisible on the
    // userportal device list; routing through `issue_tokens_and_cookie` fixes
    // that by construction.
    let issued = match issue_tokens_and_cookie(
        ctx,
        &lifetime,
        &user_id,
        &email,
        &grant,
        &format!("oauth.{provider}"),
        Rotation::NewFamily,
    )
    .await
    {
        Ok(i) => i,
        Err(r) => return r,
    };

    // Redirect to frontend — token is set via HttpOnly cookie only (not URL)
    let frontend_url =
        match config::get_default(ctx, FRONTEND_URL_KEY, "http://localhost:5173").await {
            Ok(frontend_url) => frontend_url,
            Err(e) => return crud::db_error_internal(e, "Could not read the frontend URL"),
        };
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
    let admin_default = match configured_admin_default(ctx).await {
        Ok(admin_default) => admin_default,
        Err(e) => return crud::db_error_internal(e, "Could not read the post-login redirect"),
    };
    // Role-aware default (#1 onboarding bug fix): a non-admin OAuth login
    // must never default into the admin-only destination above — see
    // `redirect::default_post_login_redirect`.
    let is_admin = grant.roles().iter().any(|r| r == "admin");
    let post_login = default_post_login_redirect(is_admin, &admin_default);
    let redirect_url = format!("{}{}", frontend_url.trim_end_matches('/'), post_login);

    ResponseBuilder::new()
        .status(302)
        .set_cookie(&issued.cookie)
        // The binding cookie has done its job for this flow; a single-use
        // binding must not outlive the state it was minted for.
        .set_cookie(&clear_binding)
        .set_header("Location", &redirect_url)
        .json(&serde_json::json!({"redirect": redirect_url}))
}

/// The profile fields the callback needs from a provider's userinfo response,
/// already normalised (email lowercased, missing strings empty).
struct OAuthUserInfo {
    /// Lowercased email address the provider returned.
    email: String,
    /// Whether the provider asserted that the account holder controls
    /// [`email`](Self::email) — resolved from the provider's
    /// [`EmailAssertion`](super::spec::EmailAssertion). `false` means "not
    /// asserted", which is also what an absent or malformed claim produces.
    email_verified: bool,
    /// Display name, empty if the provider omitted it.
    name: String,
    /// Avatar URL, empty if the provider omitted it.
    avatar: String,
    /// Stable provider-side user id — `sub` from Microsoft's OIDC userinfo,
    /// `id` from Google's v2 userinfo and from GitHub — coerced to a string.
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
/// Calls the provider userinfo endpoint and resolves the address together with
/// what the provider promises about it, per the provider's
/// [`EmailAssertion`](super::spec::EmailAssertion): a boolean claim in the
/// same payload (Google), a separate per-address list (GitHub `/user/emails`),
/// or nothing at all (Microsoft). Returns the normalised [`OAuthUserInfo`]; a
/// missing email or stable id is an error, an unverifiable one is not — it is
/// reported as such and the caller decides what it may be used for.
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
    // Only a provider that declares a verification claim gets to assert one,
    // and only a literal `true` counts: an absent, `false` or non-boolean
    // value is "not asserted".
    let mut email_verified = match spec.email_assertion {
        super::spec::EmailAssertion::Claim(field) => user_info
            .get(field)
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        super::spec::EmailAssertion::AddressList(_) | super::spec::EmailAssertion::None => false,
    };
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

    // For an `AddressList` provider the userinfo payload is not authoritative
    // about the address at all: GitHub's `/user` returns the *public profile*
    // address, which is null when the user keeps it private and carries no
    // confirmation either way. `/user/emails` (granted by the `user:email`
    // scope) is the list that does, so it is always consulted, and only an
    // entry flagged `verified` is taken — preferring the primary one. If the
    // call fails or yields nothing verified, the profile address stands as an
    // unverified address rather than a verified one.
    if let super::spec::EmailAssertion::AddressList(emails_url) = spec.email_assertion {
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
                            email_verified = true;
                        }
                    }
                }
            }
        }
    }

    if email.is_empty() {
        return Err(err_internal_no_cause("No email returned by OAuth provider"));
    }

    // Extract the stable provider-side user identifier. The spelling is per
    // endpoint, not per vendor: Google's v2 userinfo answers `id` (a string),
    // GitHub's `/user` answers `id` as a JSON number, and Microsoft's OIDC
    // userinfo answers the OIDC `sub`. Coerce to string in all cases.
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
        email_verified,
        name,
        avatar,
        provider_ref,
        provider_login,
    })
}

/// Phase 3 — resolve the local user id for this OAuth identity.
///
/// Tries, in order: an existing `(provider, provider_ref)` link; an
/// email-matched local account (only when both sides have proven the address —
/// see below); otherwise creates a new user (subject to the shared signup
/// gates). Every branch then passes the lifecycle and verification gates
/// before the provider link is upserted. Returns the resolved local user id.
async fn resolve_user(
    limiter: &crate::blocks::rate_limit::UserRateLimiter,
    ctx: &dyn Context,
    msg: &Message,
    provider: &str,
    info: &OAuthUserInfo,
    clear_binding: &str,
) -> Result<ResolvedAccount, OutputStream> {
    // --- Step 1: look up existing link by (provider, provider_ref) ---
    let existing_link =
        match provider_links::find_by_provider_ref(ctx, provider, &info.provider_ref).await {
            Ok(l) => l,
            Err(e) => return Err(crud::db_error_internal(e, "provider_links lookup failed")),
        };

    // --- Step 2 / 3: resolve user_id ---
    // `created` is whether this sign-in created the account — and with it,
    // its provider link.
    let (user_id, created): (String, bool) = if let Some(link) = existing_link {
        // Known provider link — reuse the bound user.
        (link.user_id, false)
    } else {
        // No link yet. An account already holding this address may be adopted
        // only when BOTH sides have PROVEN control of the mailbox.
        //
        // Without that, the address is just a string two parties happen to
        // type, and matching on it hands the account to whichever of them
        // arrives second: an attacker who signs up locally with
        // `victim@example.com` inherits the victim's real account the moment
        // the victim signs in with a provider — the classic pre-account
        // takeover. It runs the other way too: a provider that will hand out
        // a token for an address it never confirmed (Microsoft, see
        // `spec::EmailAssertion`) lets its own users claim any local account
        // by address alone.
        //
        // The local half reads `email_is_proven`, NOT `email_verified`.
        // `email_verified` is a policy flag — `api::signup` writes it
        // `!REQUIRE_VERIFICATION`, so on the default configuration every
        // password signup carries it having proved nothing and with no mail
        // ever sent. Gating on it would leave the takeover wide open on
        // exactly the deployments that never turned verification on.
        //
        // A refusal here does tell the caller that an account with this
        // address exists. That is unavoidable — `users.email` is UNIQUE, so
        // the alternative is not silence but a failed insert — and it is the
        // lesser disclosure by a wide margin.
        match users::find_by_email(ctx, &info.email).await {
            Ok(Some(_)) if !info.email_verified => {
                return Err(refuse(
                    ErrorCode::EmailAlreadyExists,
                    &format!(
                        "An account already uses this email address, and {provider} does not \
                         confirm that this address is yours."
                    ),
                    clear_binding,
                ));
            }
            Ok(Some(existing_user)) if !existing_user.email_is_proven() => {
                // Deliberately gives no instruction about the other account:
                // if this refusal is doing its job, the account belongs to
                // someone who registered the address without owning it, and
                // "go and sign in to it" is the last advice to give.
                return Err(refuse(
                    ErrorCode::EmailAlreadyExists,
                    "An account already uses this email address and has never confirmed it. \
                     For your safety this sign-in cannot join that account.",
                    clear_binding,
                ));
            }
            Ok(Some(existing_user)) => (existing_user.id, false),
            Ok(None) => {
                // Brand-new user — enforce signup gates. Shared with the JSON
                // signup handler so the ALLOW_SIGNUP / ALLOWED_EMAIL_DOMAINS /
                // bootstrap-admin rules can't drift between the two flows.
                let signup_allowed = signup_allowed(ctx)
                    .await
                    .map_err(|e| crud::db_error_internal(e, "Could not read the signup switch"))?;
                if !signup_allowed {
                    return Err(refuse(
                        ErrorCode::Forbidden,
                        "Signups are currently disabled",
                        clear_binding,
                    ));
                }

                let domain_allowed = email_domain_allowed(ctx, &info.email).await.map_err(|e| {
                    crud::db_error_internal(e, "Could not read the allowed email domains")
                })?;
                if !domain_allowed {
                    return Err(refuse(
                        ErrorCode::Forbidden,
                        "Signups from this email domain are not allowed",
                        clear_binding,
                    ));
                }

                // Determine role: admin if the address matches the
                // bootstrap-admin email AND the provider actually asserts
                // that address. Without the assertion this is "whoever can
                // type the admin's address at a provider that never checks
                // one" — an admin account for the asking.
                let role = if info.email_verified {
                    initial_role_for(ctx, &info.email).await.map_err(|e| {
                        crud::db_error_internal(e, "Could not read the bootstrap admin email")
                    })?
                } else {
                    "user"
                };

                let require_verification =
                    crate::config_vars::get_bool(ctx, REQUIRE_VERIFICATION_KEY, false)
                        .await
                        .map_err(|e| {
                            crud::db_error_internal(e, "Could not read the verification policy")
                        })?;
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
                    // The policy flag, written exactly as `api::signup`
                    // writes it: the provider's assertion satisfies the
                    // policy, and so does a deployment that asks for no
                    // verification at all. What the flag does NOT claim is
                    // that anyone proved anything — that is recorded
                    // separately, below, and only when a provider asserted
                    // it.
                    email_verified: info.email_verified || !require_verification,
                    verification_token_hash: None,
                };
                // The initial role is the inline `users.role` that
                // `get_user_roles` reads first; a `user_roles` row means a
                // grant beyond it, so none is written at signup — the same
                // rows password signup produces.
                //
                // The account, the link that is its only way in, and — when
                // the provider asserted the address — the proof of it, in
                // one atomic write. Written apart, a failure after the
                // account left one with no link and no password; the next
                // sign-in then met it down the email path above, and a
                // provider that asserts nothing is refused adoption there
                // for good.
                let proof = info.email_verified.then(|| users::proof::oauth(provider));
                let inserted = users::insert_with_ops(ctx, new_user, |id| {
                    let mut ops = vec![provider_links::create_op(provider_links::NewLink {
                        provider,
                        provider_ref: &info.provider_ref,
                        user_id: id,
                        provider_login: &info.provider_login,
                    })];
                    ops.extend(
                        proof
                            .as_deref()
                            .map(|proof| users::record_email_proof_op(id, proof)),
                    );
                    ops
                })
                .await;
                match inserted {
                    Ok(u) => (u.id, true),
                    Err(e) => return Err(crud::db_error_internal(e, "Failed to create user")),
                }
            }
            Err(e) => return Err(crud::db_error_internal(e, "User lookup failed")),
        }
    };

    // --- Lifecycle + verification gates (single enforcement point) ---
    // Every branch above (existing link, email merge, new signup) converges
    // here — including the existing-link branch, which authenticates a user
    // it never looked up. Verify the resolved account may authenticate BEFORE
    // mutating the provider link or issuing tokens. `is_active()` covers both
    // `disabled` and soft-delete (`deleted_at`).
    let account = match users::find_by_id(ctx, &user_id).await {
        Ok(Some(u)) if u.is_active() => u,
        Ok(Some(_)) => {
            return Err(refuse(
                ErrorCode::AccountDisabled,
                "Account is disabled",
                clear_binding,
            ))
        }
        // Forbidden, not NotFound: this is the same refusal as the line
        // above — the resolved account may not authenticate — and a 404 on
        // an authentication endpoint tells a caller which accounts exist.
        // The row is gone between resolution and this read, which is a
        // deleted account, not a missing page.
        Ok(None) => {
            return Err(refuse(
                ErrorCode::Forbidden,
                "Account not found",
                clear_binding,
            ))
        }
        Err(e) => return Err(crud::db_error_internal(e, "User lookup failed")),
    };

    // --- Step 4: bind the provider identity to the account ---
    // Before the verification gate, deliberately: the link records which
    // provider identity this account is, which is true whether or not the
    // account may sign in yet. An account this sign-in created already has
    // its link (written with it, above); a known link is refreshed here; an
    // adopted account gets its link here.
    if !created {
        if let Err(e) = provider_links::upsert(
            ctx,
            provider_links::NewLink {
                provider,
                provider_ref: &info.provider_ref,
                user_id: &user_id,
                provider_login: &info.provider_login,
            },
        )
        .await
        {
            // Log but don't fail — the account is resolved and the sign-in
            // can proceed. A failed refresh leaves the known link as it was;
            // a failed adoption link means the next sign-in resolves the
            // account by address again, which succeeds under the same
            // adoption rule that admitted this one.
            tracing::warn!("Failed to upsert provider_links: {e}");
        }
    }

    // A provider assertion about the account's OWN address is a proof, so an
    // account that gets one records it — including an account created by an
    // earlier sign-in, before the assertion was read. The address comparison
    // is the point: an assertion about some other address says nothing about
    // this row.
    let verified_now = if info.email_verified && account.email == info.email {
        if !account.email_is_proven() {
            let proof = users::proof::oauth(provider);
            if let Err(e) = users::record_email_proof(ctx, &user_id, &proof).await {
                return Err(crud::db_error_internal(
                    e,
                    "Failed to record the verified email",
                ));
            }
        }
        true
    } else {
        account.email_verified
    };

    // The same verification policy `api::login` and `api::refresh` enforce,
    // read off the same column they read. A callback that issues tokens to an
    // unverified account while refresh rejects it at the first rotation is
    // not a lenient sign-in; it is a sign-in that logs the user back out
    // minutes later, every time, with no way out of the loop.
    let require_verification =
        match crate::config_vars::get_bool(ctx, REQUIRE_VERIFICATION_KEY, false).await {
            Ok(required) => required,
            Err(e) => {
                return Err(crud::db_error_internal(
                    e,
                    "Could not read the verification policy",
                ))
            }
        };
    if require_verification && !verified_now {
        // Refusing is not enough on its own. This account may have been
        // created moments ago by a provider that asserts nothing, so it has
        // no password, no verification mail and no other route to ever become
        // verified — a permanent lockout in place of the login/logout loop.
        // Mail the link (the shared path owns the resend cooldown, so a retry
        // loop cannot turn into a mail flood) and say so.
        use crate::blocks::auth_ui::api::{
            log_email_not_sent, send_verification_email, VerificationMail,
        };
        match send_verification_email(limiter, ctx, msg, &user_id, &account.email).await {
            Ok(VerificationMail::Sent | VerificationMail::WithinCooldown) => {}
            // Recorded, never answered: this response is a refusal whose body
            // must not vary with whether the mail went out.
            Ok(VerificationMail::NotSent(failure)) => {
                log_email_not_sent("oauth-verification", &user_id, &failure);
            }
            Err(e) => {
                tracing::error!(user_id = %user_id, error = %e, "oauth-verification: could not mint the verification token");
            }
        }
        return Err(refuse(
            ErrorCode::EmailNotVerified,
            "This site requires a verified email address. Check your inbox for the \
             verification link, then sign in again.",
            clear_binding,
        ));
    }

    Ok(ResolvedAccount { user: account })
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

/// End-to-end tests for the OAuth callback's security gates.
///
/// Every test drives the real handlers — [`handle`], and `api::signup` /
/// `api::verify` for the local accounts an OAuth identity meets — through
/// mock `wafer-run/network` and `impresspress/email` blocks serving each
/// provider's real payload shapes. A fixture that wrote a users row itself
/// could assert whatever state it liked; these assert the state the shipped
/// signup handler actually produces, which is the whole subject of the
/// adoption rule.
///
/// What they hold in place:
///
/// * **Browser binding** — a callback is only honoured in the browser that
///   ran the start endpoint, so a `code`/`state` pair captured in the
///   attacker's browser cannot be replayed at the victim's (login CSRF).
/// * **Account adoption** — an OAuth identity joins an existing local account
///   only when the provider asserts a verified address AND someone actually
///   proved the local one (pre-account takeover).
/// * **Role grants** — a bootstrap-admin match keys on the account's own
///   address and needs a provider assertion behind it.
/// * **Verification policy** — the callback applies
///   `WAFER_RUN__AUTH__REQUIRE_VERIFICATION` as login and refresh do, and
///   mails the link rather than stranding an account that has no other way
///   to get one.
/// * **Provider wiring** — Microsoft signs in through the OIDC userinfo
///   endpoint; Graph `/v1.0/me`, which returns no `email` at all, cannot
///   produce a sign-in.
/// * **Lifecycle** — disabled and soft-deleted accounts are rejected on every
///   branch, and a successful login leaves exactly one session row.
#[cfg(test)]
mod security_regression_tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use wafer_core::interfaces::network::service::{
        NetworkError, NetworkService, Request, Response,
    };
    use wafer_run::{Block, BlockCategory, BlockInfo, InputStream, LifecycleEvent, Message};

    use super::handle;
    use crate::{
        blocks::{
            auth::{
                config::BOOTSTRAP_ADMIN_EMAIL_KEY,
                repo::{oauth_pkce, provider_links, sessions, users},
            },
            rate_limit::UserRateLimiter,
        },
        config_vars::ENABLE_OAUTH_KEY,
        test_support::TestContext,
    };

    /// A fresh outbound-mail budget for one call site.
    ///
    /// Per call rather than shared, for the reason `api::test_mail_request`
    /// gives: most of these tests use the mail path incidentally, and one
    /// shared limiter would make a later test in the file silently stop
    /// sending. A test that IS about the budget would build its own.
    fn limiter() -> UserRateLimiter {
        UserRateLimiter::new()
    }

    /// The `state_id` every fixture seeds, and the one `callback_msg` binds to.
    const STATE_ID: &str = "state-xyz";

    /// Stable provider-side user ids the mock returns, per provider. Google's
    /// v2 userinfo names it `id` (the OIDC `sub` is the *OIDC* endpoint's
    /// spelling); Microsoft's OIDC userinfo names it `sub`; GitHub's `id` is
    /// a JSON number.
    const GOOGLE_ID: &str = "google-user-123";
    const MICROSOFT_SUB: &str = "microsoft-user-456";
    const GITHUB_ID: &str = "4242";

    const FIXTURE_PASSWORD: &str = "correct-horse-battery";

    // ---------------------------------------------------------------
    // Mock provider network
    // ---------------------------------------------------------------

    /// Mock network block serving one provider's token + profile endpoints.
    ///
    /// Any other URL is an error, which is what makes these tests able to say
    /// *which* endpoint the flow talks to: a handler pointed at the wrong
    /// userinfo URL gets an error or an unusable payload, never a pass.
    struct MockOAuthNetwork {
        email: String,
        /// Google: the value of the `verified_email` claim. GitHub: the
        /// `verified` flag on the `/user/emails` entry. Microsoft asserts
        /// nothing, so it is unused there.
        provider_verified: bool,
        /// GitHub only: whether `/user` exposes the address. A user who keeps
        /// their address private gets `null` there, which is what makes
        /// `/user/emails` the only place an address can be found.
        profile_email_public: bool,
    }

    #[async_trait]
    impl NetworkService for MockOAuthNetwork {
        async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
            let url = req.url.as_str();
            let body = if url.ends_with("/token") || url.ends_with("oauth/access_token") {
                serde_json::json!({ "access_token": "mock-access-token" })
            } else if url == "https://www.googleapis.com/oauth2/v2/userinfo" {
                serde_json::json!({
                    "id": GOOGLE_ID,
                    "email": self.email,
                    "verified_email": self.provider_verified,
                    "name": "Mock Google User",
                })
            } else if url == "https://graph.microsoft.com/oidc/userinfo" {
                // Microsoft's OIDC userinfo claims. No `email_verified`:
                // Microsoft does not make that assertion.
                serde_json::json!({
                    "sub": MICROSOFT_SUB,
                    "email": self.email,
                    "name": "Mock Microsoft User",
                })
            } else if url == "https://graph.microsoft.com/v1.0/me" {
                // Graph's own user resource, for comparison: `mail` and
                // `userPrincipalName`, and no `email` key anywhere.
                serde_json::json!({
                    "id": MICROSOFT_SUB,
                    "displayName": "Mock Microsoft User",
                    "mail": self.email,
                    "userPrincipalName": self.email,
                })
            } else if url == "https://api.github.com/user" {
                // The public profile address — null for a user who keeps it
                // private, and in no case something GitHub vouches for.
                serde_json::json!({
                    "id": GITHUB_ID.parse::<i64>().unwrap(),
                    "login": "mockgh",
                    "email": if self.profile_email_public {
                        serde_json::Value::String(self.email.clone())
                    } else {
                        serde_json::Value::Null
                    },
                    "avatar_url": "https://avatars.example/mockgh.png",
                })
            } else if url == "https://api.github.com/user/emails" {
                serde_json::json!([{
                    "email": self.email,
                    "primary": true,
                    "verified": self.provider_verified,
                }])
            } else {
                return Err(NetworkError::Other(format!("unexpected URL: {url}")));
            };
            Ok(Response {
                status_code: 200,
                headers: HashMap::new(),
                body: serde_json::to_vec(&body).unwrap(),
            })
        }
    }

    // ---------------------------------------------------------------
    // Mock email block
    // ---------------------------------------------------------------

    /// One `email.send_template` call, as the auth handlers make it.
    #[derive(Debug, Clone)]
    struct SentMail {
        template: String,
        to: String,
        token: String,
    }

    /// What the deployment mailed, in order. Shared with the registered
    /// `impresspress/email` mock, so a test can both assert a mail went out
    /// and read the token out of it — which is the only way to redeem a
    /// verification link the way a real user does.
    #[derive(Clone, Default)]
    struct MailLog(Arc<Mutex<Vec<SentMail>>>);

    impl MailLog {
        fn all(&self) -> Vec<SentMail> {
            self.0.lock().expect("mail log").clone()
        }

        fn last_for(&self, to: &str, template: &str) -> Option<SentMail> {
            self.all()
                .into_iter()
                .rev()
                .find(|m| m.to == to && m.template == template)
        }
    }

    struct MockEmailBlock {
        log: MailLog,
    }

    #[async_trait]
    impl Block for MockEmailBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(
                "impresspress/email",
                "0.0.1",
                "service@v1",
                "records the mail this deployment sends",
            )
            .category(BlockCategory::Service)
        }

        async fn handle(
            &self,
            _ctx: &dyn super::Context,
            _msg: Message,
            input: InputStream,
        ) -> wafer_run::OutputStream {
            let raw = match input.collect_to_bytes().await {
                Ok(bytes) => bytes,
                Err(e) => return wafer_run::OutputStream::error(e),
            };
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) {
                self.log.0.lock().expect("mail log").push(SentMail {
                    template: v["template"].as_str().unwrap_or_default().to_string(),
                    to: v["to"].as_str().unwrap_or_default().to_string(),
                    token: v["token"].as_str().unwrap_or_default().to_string(),
                });
            }
            crate::http::ok_empty()
        }

        async fn lifecycle(
            &self,
            _ctx: &dyn super::Context,
            _e: LifecycleEvent,
        ) -> Result<(), wafer_run::WaferError> {
            Ok(())
        }
    }

    // ---------------------------------------------------------------
    // Fixture
    // ---------------------------------------------------------------

    /// One OAuth sign-in attempt, described: which provider answers, with
    /// which address, whether that provider vouches for it, and any extra
    /// config the deployment carries.
    struct OauthFlow {
        provider: &'static str,
        email: &'static str,
        provider_verified: bool,
        profile_email_public: bool,
        config: Vec<(String, String)>,
    }

    impl OauthFlow {
        fn google(email: &'static str) -> Self {
            Self {
                provider: "google",
                email,
                provider_verified: true,
                profile_email_public: false,
                config: Vec::new(),
            }
        }

        fn github(email: &'static str) -> Self {
            Self {
                provider: "github",
                email,
                provider_verified: true,
                profile_email_public: false,
                config: Vec::new(),
            }
        }

        fn microsoft(email: &'static str) -> Self {
            Self {
                provider: "microsoft",
                email,
                // Microsoft returns no verification claim whatever the
                // account looks like; the field is inert here.
                provider_verified: false,
                profile_email_public: false,
                config: Vec::new(),
            }
        }

        /// Whether the provider vouches for the address (Google's
        /// `verified_email`, GitHub's per-address `verified`).
        fn provider_verified(mut self, verified: bool) -> Self {
            self.provider_verified = verified;
            self
        }

        /// GitHub only: publish the address on the profile endpoint, as a
        /// user who has not made it private does.
        fn profile_email_public(mut self) -> Self {
            self.profile_email_public = true;
            self
        }

        fn config(mut self, key: &str, value: &str) -> Self {
            self.config.push((key.to_string(), value.to_string()));
            self
        }

        /// This deployment requires a verified email address.
        fn require_verification(self) -> Self {
            self.config(
                crate::blocks::auth::config::REQUIRE_VERIFICATION_KEY,
                "true",
            )
        }

        /// Build a ctx with auth migrations, a crypto block (token minting and
        /// password hashing), mock network + email blocks, OAuth enabled, and
        /// a seeded PKCE state row so the callback's single-use state
        /// redemption succeeds.
        ///
        /// The extra config is folded into the same `wafer-run/config` block
        /// as the OAuth flags — it can't be layered on afterward via
        /// `TestContext::set_config`, which would replace this block wholesale
        /// and drop the OAuth flags the callback needs to get past its own
        /// gates.
        async fn ctx_and_mail(&self) -> (TestContext, MailLog) {
            // Runs as auth-ui: the callback mints the session in that block.
            let mut ctx = TestContext::with_auth()
                .await
                .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);

            // Crypto block — issue_tokens_and_cookie signs JWTs, signup
            // hashes passwords, and both pull random bytes.
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
            let net: Arc<dyn Block> =
                Arc::new(wafer_core::service_blocks::network::NetworkBlock::new(
                    Arc::new(MockOAuthNetwork {
                        email: self.email.to_string(),
                        provider_verified: self.provider_verified,
                        profile_email_public: self.profile_email_public,
                    }),
                ));
            ctx.register_block("wafer-run/network", net);

            let mail = MailLog::default();
            let email_block: Arc<dyn Block> = Arc::new(MockEmailBlock { log: mail.clone() });
            ctx.register_block("impresspress/email", email_block);

            // Config block — the handler reads OAuth flags / client
            // credentials via `config::get_default`, which dispatches to the
            // `wafer-run/config` block (NOT the TestContext config_get
            // snapshot).
            use wafer_core::{
                interfaces::config::service::ConfigService,
                service_blocks::config::{ConfigBlock, EnvConfigService},
            };
            let cfg_svc = EnvConfigService::new();
            cfg_svc.set(ENABLE_OAUTH_KEY, "true");
            let upper = self.provider.to_uppercase();
            cfg_svc.set(
                &format!("IMPRESSPRESS__AUTH_UI__OAUTH_{upper}_CLIENT_ID"),
                "client-id",
            );
            cfg_svc.set(
                &format!("IMPRESSPRESS__AUTH_UI__OAUTH_{upper}_CLIENT_SECRET"),
                "client-secret",
            );
            for (k, v) in &self.config {
                cfg_svc.set(k, v);
            }
            let cfg_block: Arc<dyn Block> = Arc::new(ConfigBlock::new(Arc::new(cfg_svc)));
            ctx.register_block("wafer-run/config", cfg_block);

            seed_state(&ctx, STATE_ID, self.provider).await;
            (ctx, mail)
        }

        async fn ctx(&self) -> TestContext {
            self.ctx_and_mail().await.0
        }
    }

    /// Seed a single-use PKCE state row keyed by `state_id`.
    async fn seed_state(ctx: &TestContext, state_id: &str, provider: &str) {
        let expires = (chrono::Utc::now() + chrono::Duration::minutes(10))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        oauth_pkce::insert(
            ctx,
            oauth_pkce::NewPkceState {
                state_id,
                provider,
                code_verifier: "verifier-abc",
                redirect_uri: "https://app.example.com/b/auth/oauth/callback",
                expires_at: &expires,
            },
        )
        .await
        .expect("seed pkce state");
    }

    /// How many sessions this account has. A password signup on a deployment
    /// that does not require verification auto-logs-in, so a fixture account
    /// starts with one — what a refused OAuth sign-in must not do is add
    /// another.
    async fn session_count(ctx: &TestContext, user_id: &str) -> usize {
        sessions::list_for_user(ctx, user_id)
            .await
            .expect("list sessions ok")
            .len()
    }

    /// The `Cookie` header a browser sends back for a `Set-Cookie` value —
    /// the name/value pair, without the attributes.
    fn cookie_header_for(set_cookie: &str) -> &str {
        set_cookie.split(';').next().unwrap_or("")
    }

    /// The callback request a browser makes on its way back from the
    /// provider: `code` + `state` query params, plus the binding cookie the
    /// start endpoint set for `state_id`.
    async fn callback_msg(ctx: &TestContext) -> Message {
        let set_cookie = crate::blocks::auth_ui::oauth::state_binding::issue(ctx, STATE_ID, 600)
            .await
            .expect("binding cookie");
        let mut msg = callback_msg_unbound();
        msg.set_meta("http.header.cookie", cookie_header_for(&set_cookie));
        msg
    }

    /// The same callback with no binding cookie at all — the shape a forged
    /// callback arrives in, since the attacker's cookie is in the attacker's
    /// browser.
    fn callback_msg_unbound() -> Message {
        let mut msg = Message::new("auth.oauth.callback");
        msg.set_meta("req.query.code", "auth-code-123");
        msg.set_meta("req.query.state", STATE_ID);
        // The outbound-mail limiter keys on the client IP, and a request
        // without one lands in the shared `UNKNOWN_IP` bucket a deployment is
        // never supposed to use. Carry one, as the pipeline does.
        msg.set_meta(wafer_block::meta::META_REQ_CLIENT_IP, "203.0.113.7");
        msg
    }

    /// Every `Set-Cookie` the response carries, as the HTTP boundary renders
    /// them — including on a refusal, which carries its headers on the error.
    async fn set_cookies(out: wafer_run::OutputStream) -> Vec<String> {
        wafer_block::http_codec::collect_http_response(out)
            .await
            .headers
            .into_iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v)
            .collect()
    }

    /// The single-use state redemption is the callback's first database
    /// call. A WRAP denial there is the deployment's missing grant — a 403 —
    /// not the 500 an outage is, and it is refused before the flow reaches
    /// the provider. Driven through the block's own route table.
    #[tokio::test]
    async fn state_redemption_denial_is_403_not_500() {
        use crate::test_support::FailingDbOpContext;

        let ctx = OauthFlow::google("denied@example.com").ctx().await;
        let mut msg = crate::test_support::anon_msg("retrieve", "/b/auth/oauth/callback");
        for entry in callback_msg(&ctx).await.meta {
            msg.set_meta(&entry.key, &entry.value);
        }
        let ctx = FailingDbOpContext::failing_with(
            ctx,
            vec![("database.take_where", oauth_pkce::TABLE)],
            wafer_run::WaferError::new(
                wafer_run::ErrorCode::PermissionDenied,
                "WRAP: no grant on the PKCE state table",
            ),
        );
        let out = crate::blocks::auth_ui::AuthUiBlock::default()
            .handle(&ctx, msg, InputStream::empty())
            .await;
        assert_eq!(crate::test_support::output_http_status(out).await, 403);
    }

    // ---------------------------------------------------------------
    // Local-account fixtures — through the real handlers
    // ---------------------------------------------------------------

    /// Register a local password account the way a user does: through
    /// `api::signup::handle`. The row then carries exactly what that handler
    /// writes for this deployment's configuration — including
    /// `email_verified = !REQUIRE_VERIFICATION`, the policy flag that is NOT
    /// evidence of anything.
    async fn signup_password_account(ctx: &TestContext, email: &str) {
        let body = serde_json::json!({ "email": email, "password": FIXTURE_PASSWORD }).to_string();
        let (signup_limiter, signup_msg) = crate::blocks::auth_ui::api::test_mail_request();
        // Signup mails its verification link after the response; run it, as
        // the platform would, so the link is in the mail log.
        crate::deferred::queue_for_test();
        let out = crate::blocks::auth_ui::api::signup::handle(
            &signup_limiter,
            ctx,
            &signup_msg,
            wafer_run::InputStream::from_bytes(body.into_bytes()),
        )
        .await;
        crate::blocks::auth_ui::api::run_deferred().await;
        assert_eq!(
            crate::test_support::output_status(out).await,
            201,
            "the signup fixture must succeed, or the test below proves nothing"
        );
    }

    /// Prove the address on a password account the only way a user can:
    /// redeem the link that was mailed to it, through `api::verify::handle`.
    async fn prove_address_by_email_link(ctx: &TestContext, mail: &MailLog, email: &str) {
        let sent = mail
            .last_for(email, "verification")
            .expect("signup must have mailed a verification link");
        let mut msg = Message::new("auth.verify");
        msg.set_meta("req.query.token", &sent.token);
        let out =
            crate::blocks::auth_ui::api::verify::handle(ctx, &msg, wafer_run::InputStream::empty())
                .await;
        assert_eq!(
            crate::test_support::output_status(out).await,
            200,
            "the verification link must be redeemable"
        );
        assert!(
            users::find_by_email(ctx, email)
                .await
                .expect("user lookup ok")
                .expect("user present")
                .email_is_proven(),
            "redeeming the link must record the proof"
        );
    }

    // ---------------------------------------------------------------
    // Browser binding (login CSRF)
    // ---------------------------------------------------------------

    /// The two halves agree: the cookie the real start handler sets is the
    /// one the real callback demands. A round trip through both handlers, so
    /// neither can drift from the other's idea of the binding.
    #[tokio::test]
    async fn start_binds_the_flow_and_the_callback_accepts_it() {
        let email = "roundtrip@example.com";
        let ctx = OauthFlow::google(email).ctx().await;

        let mut start_msg = Message::new("auth.oauth.login");
        start_msg.set_meta("req.query.provider", "google");
        let started = wafer_block::http_codec::collect_http_response(
            crate::blocks::auth_ui::oauth::start::handle(&ctx, &start_msg).await,
        )
        .await;
        assert_eq!(
            started.status, 302,
            "the start endpoint is navigated to, not fetched: it must redirect \
             to the provider so its cookie is set first-party"
        );
        let header = |name: &str| {
            started
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        let set_cookie =
            header("set-cookie").expect("the start endpoint must bind the flow to this browser");
        let auth_url = header("location").expect("the start endpoint must redirect");
        assert!(
            auth_url.starts_with("https://accounts.google.com/"),
            "the redirect must go to the provider: {auth_url}"
        );

        let state = auth_url
            .split("&state=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .expect("authorize URL carries the state")
            .to_string();

        let mut msg = callback_msg_unbound();
        msg.set_meta("req.query.state", &state);
        msg.set_meta("http.header.cookie", cookie_header_for(&set_cookie));

        let status = crate::test_support::output_status(handle(&limiter(), &ctx, &msg).await).await;
        assert_eq!(
            status, 302,
            "a callback carrying the cookie the start endpoint set must complete"
        );
    }

    /// Login CSRF: an attacker runs the authorize step in their own browser
    /// and hands the victim the resulting callback URL. The victim's browser
    /// has no binding cookie, so the callback must refuse — otherwise the
    /// victim is silently signed in to the attacker's account.
    #[tokio::test]
    async fn callback_without_the_binding_cookie_is_refused() {
        let email = "csrf-victim@example.com";
        let ctx = OauthFlow::google(email).ctx().await;

        let out = handle(&limiter(), &ctx, &callback_msg_unbound()).await;
        assert!(
            crate::test_support::output_is_error(out, "InvalidArgument").await,
            "a callback from a browser that never started the flow must be refused"
        );

        assert!(
            users::find_by_email(&ctx, email)
                .await
                .expect("user lookup ok")
                .is_none(),
            "no account may be created by an unbound callback"
        );
        // The refusal happens before the take, so the victim's own pending
        // flow (if any) is not burned by the forgery.
        assert!(
            oauth_pkce::take(&ctx, STATE_ID)
                .await
                .expect("take ok")
                .is_some(),
            "an unbound callback must not consume the single-use state"
        );
    }

    /// A cookie from some other flow is not a binding either.
    #[tokio::test]
    async fn callback_with_a_foreign_binding_cookie_is_refused() {
        let ctx = OauthFlow::google("csrf-victim2@example.com").ctx().await;

        let foreign =
            crate::blocks::auth_ui::oauth::state_binding::issue(&ctx, "some-other-state", 600)
                .await
                .expect("binding cookie");
        let mut msg = callback_msg_unbound();
        msg.set_meta("http.header.cookie", cookie_header_for(&foreign));

        assert!(
            crate::test_support::output_is_error(
                handle(&limiter(), &ctx, &msg).await,
                "InvalidArgument"
            )
            .await,
            "a binding cookie minted for another state must not redeem this one"
        );
    }

    /// The refusal's cookie is keyed by the cookie it sets
    /// (`resp.set_cookie.{name}`), the key `wafer-run`'s flow merge identifies
    /// cookies by. A positional `resp.set_cookie.0` collides with any other
    /// producer's first cookie when metas merge, and one of the two is lost.
    #[tokio::test]
    async fn a_refusal_keys_its_cookie_by_name() {
        let clear = "__Host-oauth_state_abc=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0; Secure";
        let out = super::refuse(crate::blocks::errors::ErrorCode::Forbidden, "no", clear);
        let err = match out.collect_buffered().await {
            Err(wafer_run::streams::output::TerminalNotResponse::Error(err)) => err,
            other => panic!("a refusal is an error terminal: {other:?}"),
        };
        let cookie_keys: Vec<&str> = err
            .meta
            .iter()
            .map(|e| e.key.as_str())
            .filter(|key| key.starts_with("resp.set_cookie."))
            .collect();
        assert_eq!(
            cookie_keys,
            ["resp.set_cookie.__Host-oauth_state_abc;Path=/"],
            "{:?}",
            err.meta
        );
    }

    /// A rejected flow is over, so its binding is spent: the refusal expires
    /// the cookie. Otherwise every failed attempt leaves one behind, and a
    /// browser only holds so many.
    #[tokio::test]
    async fn a_refused_sign_in_expires_its_binding_cookie() {
        let email = "refused-cookie@example.com";
        let ctx = OauthFlow::microsoft(email).ctx().await;
        signup_password_account(&ctx, email).await;

        // Microsoft asserts nothing, so this sign-in is refused adoption.
        let cookies = set_cookies(handle(&limiter(), &ctx, &callback_msg(&ctx).await).await).await;
        assert!(
            cookies
                .iter()
                .any(|c| c.contains("oauth_state") && c.contains("Max-Age=0")),
            "a refusal must expire the binding cookie it consumed: {cookies:?}"
        );
    }

    // ---------------------------------------------------------------
    // Account adoption (pre-account takeover)
    // ---------------------------------------------------------------

    /// Pre-account takeover, on the DEFAULT configuration.
    ///
    /// `REQUIRE_VERIFICATION` is off, so `api::signup` writes
    /// `email_verified = true` and mails nothing: the attacker registers the
    /// victim's address, proves nothing, and the row claims to be verified.
    /// An adoption rule reading that flag hands the victim's Google sign-in
    /// straight into the attacker's account. The rule reads the proof
    /// instead, and there is none.
    #[tokio::test]
    async fn an_unproven_local_account_is_not_adopted_on_the_default_configuration() {
        let email = "preclaimed@example.com";
        let ctx = OauthFlow::google(email).ctx().await;

        signup_password_account(&ctx, email).await;
        let squatted = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("signup created the row");
        assert!(
            squatted.email_verified,
            "precondition: with verification off, signup marks the row verified"
        );
        assert!(
            !squatted.email_is_proven(),
            "precondition: nobody proved anything — no mail was even sent"
        );

        let sessions_before = session_count(&ctx, &squatted.id).await;
        let out = handle(&limiter(), &ctx, &callback_msg(&ctx).await).await;
        assert!(
            crate::test_support::output_is_error(out, "AlreadyExists").await,
            "an OAuth identity must not adopt an account nobody proved"
        );

        assert_eq!(
            session_count(&ctx, &squatted.id).await,
            sessions_before,
            "no session may be minted for the account that was not adopted"
        );
        assert!(
            provider_links::find_by_provider_ref(&ctx, "google", GOOGLE_ID)
                .await
                .expect("link lookup ok")
                .is_none(),
            "no provider link may be written for a refused adoption"
        );
    }

    /// The legitimate case: the local account's address was proven by
    /// redeeming a mailed link, and the provider asserts the same address, so
    /// the identity joins it rather than failing on the UNIQUE email or
    /// creating a second account.
    #[tokio::test]
    async fn a_proven_local_account_is_adopted_once() {
        let email = "proven-local@example.com";
        let (ctx, mail) = OauthFlow::google(email)
            .require_verification()
            .ctx_and_mail()
            .await;

        signup_password_account(&ctx, email).await;
        prove_address_by_email_link(&ctx, &mail, email).await;
        let existing = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("user present");

        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(status, 302, "a proven account may be adopted");

        let link = provider_links::find_by_provider_ref(&ctx, "google", GOOGLE_ID)
            .await
            .expect("link lookup ok")
            .expect("the adopted account is linked to the provider identity");
        assert_eq!(
            link.user_id, existing.id,
            "the link must bind to the existing account, not a new one"
        );
        assert_eq!(
            users::find_by_email(&ctx, email)
                .await
                .expect("user lookup ok")
                .expect("user present")
                .id,
            existing.id,
            "adoption must not duplicate the account"
        );
    }

    /// The way out, end to end, on the default configuration.
    ///
    /// The account starts flag-verified and unproven, which is what
    /// `api::signup` writes when verification is off — so it is refused
    /// adoption. Its owner asks for a verification link and redeems it,
    /// through the real resend and verify handlers, and the same OAuth
    /// sign-in then succeeds. Without this the no-backfill decision would be
    /// a dead end: every pre-existing account permanently unlinkable, fixable
    /// only by an operator editing the database.
    #[tokio::test]
    async fn an_account_can_prove_its_address_later_and_then_be_adopted() {
        let email = "recovering@example.com";
        let (ctx, mail) = OauthFlow::google(email).ctx_and_mail().await;

        signup_password_account(&ctx, email).await;
        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("signup created the row");
        assert!(
            user.email_verified,
            "precondition: with verification off, signup flags the row verified"
        );
        assert!(
            !user.email_is_proven(),
            "precondition: no mail was sent, so nothing proved the address"
        );

        // Refused, because nobody proved the address.
        let refused = handle(&limiter(), &ctx, &callback_msg(&ctx).await).await;
        assert!(
            crate::test_support::output_is_error(refused, "AlreadyExists").await,
            "an unproven account is not adoptable yet"
        );

        // The owner asks for a link and redeems it — the real handlers, and
        // the real mailed token.
        let resend = serde_json::json!({ "email": email }).to_string();
        let (resend_limiter, resend_msg) = crate::blocks::auth_ui::api::test_mail_request();
        let _ = crate::blocks::auth_ui::api::verify::handle_resend(
            &resend_limiter,
            &ctx,
            &resend_msg,
            wafer_run::InputStream::from_bytes(resend.into_bytes()),
        )
        .await
        .collect_buffered()
        .await;
        // The link is minted and mailed after the response; the signup
        // fixture left deferred work queued, so run it as the platform would.
        crate::blocks::auth_ui::api::run_deferred().await;
        prove_address_by_email_link(&ctx, &mail, email).await;

        // And now the same sign-in completes, into the same account.
        seed_state(&ctx, STATE_ID, "google").await;
        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(status, 302, "a proven account is adoptable");
        assert_eq!(
            provider_links::find_by_provider_ref(&ctx, "google", GOOGLE_ID)
                .await
                .expect("link lookup ok")
                .expect("link written")
                .user_id,
            user.id,
            "the identity joins the account that proved the address"
        );
    }

    /// A provider that makes no verification assertion cannot adopt an
    /// account either, however well-proven the local row is: its `email`
    /// claim is a mutable profile attribute, not proof of the mailbox.
    #[tokio::test]
    async fn a_provider_without_an_assertion_cannot_adopt_an_account() {
        let email = "ms-adopt@example.com";
        let (ctx, mail) = OauthFlow::microsoft(email)
            .require_verification()
            .ctx_and_mail()
            .await;

        signup_password_account(&ctx, email).await;
        prove_address_by_email_link(&ctx, &mail, email).await;
        let existing = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("user present");

        let sessions_before = session_count(&ctx, &existing.id).await;
        let out = handle(&limiter(), &ctx, &callback_msg(&ctx).await).await;
        assert!(
            crate::test_support::output_is_error(out, "AlreadyExists").await,
            "Microsoft asserts nothing about the address, so it cannot claim an account by it"
        );
        assert_eq!(
            session_count(&ctx, &existing.id).await,
            sessions_before,
            "no session for a refused adoption"
        );
    }

    /// Same refusal when the provider does assert, but says the address is
    /// NOT verified.
    #[tokio::test]
    async fn an_unverified_provider_address_cannot_adopt_an_account() {
        let email = "unverified-google@example.com";
        let (ctx, mail) = OauthFlow::google(email)
            .provider_verified(false)
            .require_verification()
            .ctx_and_mail()
            .await;

        signup_password_account(&ctx, email).await;
        prove_address_by_email_link(&ctx, &mail, email).await;

        let out = handle(&limiter(), &ctx, &callback_msg(&ctx).await).await;
        assert!(
            crate::test_support::output_is_error(out, "AlreadyExists").await,
            "an address the provider itself flags unverified cannot claim an account"
        );
    }

    // ---------------------------------------------------------------
    // Role grants
    // ---------------------------------------------------------------

    /// Bootstrap-admin escalation. `BOOTSTRAP_ADMIN_EMAIL` names the account
    /// that gets admin; a provider that asserts nothing about the address it
    /// reports must not be able to name it. Microsoft's `email` is a tenant
    /// attribute an administrator can point at anything, so a sign-in
    /// claiming the admin address is a claim, not a proof.
    #[tokio::test]
    async fn an_unasserted_provider_address_cannot_claim_the_bootstrap_admin_role() {
        let admin_email = "admin@example.com";
        let ctx = OauthFlow::microsoft(admin_email)
            .config(BOOTSTRAP_ADMIN_EMAIL_KEY, admin_email)
            .ctx()
            .await;

        let location = crate::test_support::output_header(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
            "Location",
        )
        .await
        .expect("302 redirect must set a Location header");
        assert!(
            location.ends_with("/b/userportal/"),
            "an unasserted address must not land in the admin home: {location}"
        );

        let created = users::find_by_email(&ctx, admin_email)
            .await
            .expect("user lookup ok")
            .expect("the account is still created");
        assert_eq!(
            created.role, "user",
            "the initial role must not be admin when nothing asserted the address"
        );
        let roles = crate::blocks::auth::helpers::get_user_roles(&ctx, &created.id)
            .await
            .expect("roles read ok");
        assert!(
            !roles.iter().any(|r| r == "admin"),
            "no admin grant may follow an unasserted address: {roles:?}"
        );
    }

    /// And the grant keys on the ACCOUNT's address, not the provider's claim
    /// about it. A linked account keeps its own address; a provider reporting
    /// a different one — even one it asserts — is describing a mailbox, not
    /// this account.
    #[tokio::test]
    async fn the_bootstrap_admin_grant_keys_on_the_account_address() {
        let admin_email = "admin@example.com";
        let account_email = "bob@example.com";
        // Google asserts `admin@example.com`, but the linked account is Bob's.
        let ctx = OauthFlow::google(admin_email)
            .config(BOOTSTRAP_ADMIN_EMAIL_KEY, admin_email)
            .ctx()
            .await;

        signup_password_account(&ctx, account_email).await;
        let bob = users::find_by_email(&ctx, account_email)
            .await
            .expect("user lookup ok")
            .expect("signup created the row");
        provider_links::upsert(
            &ctx,
            provider_links::NewLink {
                provider: "google",
                provider_ref: GOOGLE_ID,
                user_id: &bob.id,
                provider_login: "bob",
            },
        )
        .await
        .expect("seed provider link");

        let location = crate::test_support::output_header(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
            "Location",
        )
        .await
        .expect("302 redirect must set a Location header");
        assert!(
            location.ends_with("/b/userportal/"),
            "the admin grant must read the account's address, not the provider's: {location}"
        );
        let roles = crate::blocks::auth::helpers::get_user_roles(&ctx, &bob.id)
            .await
            .expect("roles read ok");
        assert!(
            !roles.iter().any(|r| r == "admin"),
            "Bob's account must not become admin because a provider named the \
             admin address: {roles:?}"
        );
    }

    // ---------------------------------------------------------------
    // Provider wiring
    // ---------------------------------------------------------------

    /// Microsoft sign-in works, and works through the OIDC userinfo endpoint.
    /// Graph `/v1.0/me` — which the mock also serves, in its real shape —
    /// carries `mail` / `userPrincipalName` and no `email`, so a flow pointed
    /// at it ends at "No email returned by OAuth provider" for every user.
    #[tokio::test]
    async fn microsoft_signs_in_through_the_oidc_userinfo_endpoint() {
        let email = "msuser@example.com";
        let ctx = OauthFlow::microsoft(email).ctx().await;

        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(status, 302, "a Microsoft sign-in must complete");

        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("the Microsoft callback created the account");
        assert!(
            !user.email_is_proven(),
            "Microsoft asserts nothing about the address, so nothing proved it"
        );
        let link = provider_links::find_by_provider_ref(&ctx, "microsoft", MICROSOFT_SUB)
            .await
            .expect("link lookup ok")
            .expect("the link keys on the OIDC `sub`");
        assert_eq!(link.user_id, user.id);
    }

    /// The account a sign-in creates, its provider link and any proof are
    /// one write. As separate writes, a failed link left an account with no
    /// link and no password; for a provider that asserts nothing (Microsoft)
    /// the retry then met that account down the email path and was refused
    /// adoption for good.
    #[tokio::test]
    async fn a_failed_link_write_leaves_no_account_and_the_retry_signs_in() {
        use crate::blocks::auth::repo::test_faults::{drop_trigger, fail_inserts_into};

        let email = "halfway-ms@example.com";
        let ctx = OauthFlow::microsoft(email).ctx().await;
        let trigger = fail_inserts_into(&ctx, provider_links::TABLE).await;

        let status = crate::test_support::output_http_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(
            status, 500,
            "the sign-in whose account write failed is told so"
        );
        assert!(
            users::find_by_email(&ctx, email)
                .await
                .expect("user lookup ok")
                .is_none(),
            "no account may outlive its failed link write"
        );

        drop_trigger(&ctx, &trigger).await;
        seed_state(&ctx, STATE_ID, "microsoft").await;
        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(status, 302, "the retry signs in");
        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("the retry created the account");
        let link = provider_links::find_by_provider_ref(&ctx, "microsoft", MICROSOFT_SUB)
            .await
            .expect("link lookup ok")
            .expect("with its link");
        assert_eq!(link.user_id, user.id);
    }

    /// An account created by a provider that asserts its address carries the
    /// proof from its first write, not from a later one that could fail.
    #[tokio::test]
    async fn a_created_account_carries_its_provider_proof_from_the_first_write() {
        let email = "proven-at-birth@example.com";
        let ctx = OauthFlow::google(email).ctx().await;
        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(status, 302);
        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("account created");
        assert_eq!(user.email_verified_by.as_deref(), Some("oauth.google"));
        assert!(user.email_verified);
    }

    /// GitHub's profile address is not authoritative: the flow reads
    /// `/user/emails` and takes the verified primary entry, which is also
    /// what makes the resulting account's address proven.
    #[tokio::test]
    async fn github_takes_the_verified_primary_address() {
        let email = "ghuser@example.com";
        let ctx = OauthFlow::github(email).ctx().await;

        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(status, 302, "a GitHub sign-in must complete");

        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("the GitHub callback created the account");
        assert_eq!(
            user.email_verified_by.as_deref(),
            Some("oauth.github"),
            "a GitHub address flagged verified on /user/emails is proven, by GitHub"
        );
    }

    /// The same address with the `verified` flag cleared creates an account
    /// nobody proved — the list is read for the flag, not merely for an
    /// address.
    #[tokio::test]
    async fn github_unverified_address_creates_an_unproven_account() {
        let email = "gh-unverified@example.com";
        let ctx = OauthFlow::github(email)
            .provider_verified(false)
            .profile_email_public()
            .ctx()
            .await;

        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(status, 302);

        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("account created");
        assert!(
            !user.email_is_proven(),
            "an unverified GitHub address must not produce a proven account"
        );
    }

    // ---------------------------------------------------------------
    // Verification policy
    // ---------------------------------------------------------------

    /// With `REQUIRE_VERIFICATION` on and a provider that asserts nothing,
    /// the callback must refuse — issuing tokens here only produced a sign-in
    /// that `api::refresh` threw out at the first rotation — AND mail the
    /// verification link. The account it just created has no password and no
    /// other route to a link, so refusing without sending one trades a
    /// login/logout loop for a permanent lockout.
    #[tokio::test]
    async fn require_verification_refuses_an_unasserted_login_and_mails_the_link() {
        let email = "needs-verification@example.com";
        let (ctx, mail) = OauthFlow::microsoft(email)
            .require_verification()
            .ctx_and_mail()
            .await;

        let out = handle(&limiter(), &ctx, &callback_msg(&ctx).await).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "an unverified account must be refused, not signed in and logged out again"
        );

        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("the signup itself is allowed");
        assert_eq!(
            session_count(&ctx, &user.id).await,
            0,
            "no session may be minted for an account the policy refuses"
        );

        let sent = mail
            .last_for(email, "verification")
            .expect("the refusal must mail the link this account has no other way to get");
        assert!(
            !sent.token.is_empty(),
            "the mailed link must carry a usable token"
        );

        // And the link works: redeeming it proves the address, after which
        // the same sign-in completes. The refusal is a step in a flow, not a
        // dead end.
        prove_address_by_email_link(&ctx, &mail, email).await;
        seed_state(&ctx, STATE_ID, "microsoft").await;
        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(status, 302, "after verifying, the sign-in completes");
    }

    /// The mail the callback sends is ordinary transactional mail and spends
    /// the deployment's outbound budget like any other. An OAuth flow with a
    /// send of its own, outside `send_template_email`, would be the cheapest
    /// way to drain that budget: no account, no password, no captcha — just a
    /// provider round trip per message.
    ///
    /// The `auth_email` category's override is one per hour here, and the
    /// single token is spent before the sign-in by another request from the
    /// same IP. The callback must find the bucket empty and send nothing — not because of
    /// the resend cooldown (this account has never been mailed) but because
    /// the budget is the same budget.
    #[tokio::test]
    async fn the_callbacks_mail_spends_the_shared_outbound_budget() {
        use crate::blocks::rate_limit::{
            check_rate_limit, ip_identity, RateLimit, RateLimitOutcome,
        };

        let email = "budgeted@example.com";
        let (ctx, mail) = OauthFlow::microsoft(email)
            .require_verification()
            .config(&RateLimit::override_key("auth_email"), "1/3600")
            .ctx_and_mail()
            .await;
        let shared = limiter();

        // Somebody on this IP sends the one message the hour allows.
        let request = callback_msg_unbound();
        let spent = check_rate_limit(
            &shared,
            &ctx,
            &ip_identity(&request),
            "auth_email",
            RateLimit::AUTH_EMAIL,
        )
        .await;
        assert!(
            matches!(spent, RateLimitOutcome::Allowed(_)),
            "precondition: the first message of the hour is allowed"
        );

        let out = handle(&shared, &ctx, &callback_msg(&ctx).await).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "the unverified sign-in is still refused"
        );
        assert!(
            mail.all().is_empty(),
            "the callback's link must be refused by the shared budget, not sent \
             beside it: {:?}",
            mail.all()
        );
    }

    /// And the provider's assertion satisfies that same policy: a Google
    /// sign-in under `REQUIRE_VERIFICATION` completes, because the row it
    /// creates records the assertion as the proof.
    #[tokio::test]
    async fn require_verification_admits_a_provider_verified_login() {
        let email = "google-verified@example.com";
        let (ctx, mail) = OauthFlow::google(email)
            .require_verification()
            .ctx_and_mail()
            .await;

        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(
            status, 302,
            "a provider-verified address satisfies the verification policy"
        );

        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("account created");
        assert_eq!(
            user.email_verified_by.as_deref(),
            Some("oauth.google"),
            "the provider's assertion must be recorded as the proof"
        );
        assert!(
            mail.all().is_empty(),
            "nothing to verify, so no mail: {:?}",
            mail.all()
        );
    }

    /// A row left unproven by an earlier sign-in is carried over on the next
    /// one rather than stranded: the provider still asserts the address, so
    /// the account records it and the policy admits it.
    #[tokio::test]
    async fn a_provider_assertion_upgrades_an_unproven_linked_account() {
        let email = "legacy-oauth@example.com";
        let ctx = OauthFlow::google(email).require_verification().ctx().await;

        signup_password_account(&ctx, email).await;
        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("signup created the row");
        assert!(!user.email_is_proven(), "precondition: nothing proved it");
        provider_links::upsert(
            &ctx,
            provider_links::NewLink {
                provider: "google",
                provider_ref: GOOGLE_ID,
                user_id: &user.id,
                provider_login: "legacy",
            },
        )
        .await
        .expect("seed provider link");

        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(status, 302, "the linked account signs in");
        assert_eq!(
            users::find_by_id(&ctx, &user.id)
                .await
                .expect("user lookup ok")
                .expect("user present")
                .email_verified_by
                .as_deref(),
            Some("oauth.google"),
            "the provider's assertion must be recorded on the existing row"
        );
    }

    // ---------------------------------------------------------------
    // Identity resolution
    // ---------------------------------------------------------------

    /// First sign-in: the callback creates the account AND the provider link,
    /// persists a session row — OAuth logins are visible on the userportal
    /// device list like every other login — sets both the session cookie and
    /// the binding expiry, and consumes the single-use state.
    #[tokio::test]
    async fn first_login_creates_the_user_the_link_and_a_session() {
        let email = "newoauth@example.com";
        let ctx = OauthFlow::google(email).ctx().await;

        let parts = wafer_block::http_codec::collect_http_response(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(
            parts.status, 302,
            "successful OAuth callback should 302-redirect"
        );

        let cookies: Vec<&String> = parts
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v)
            .collect();
        assert!(
            cookies.iter().any(|c| c.starts_with("auth_token=")),
            "the sign-in must set the session cookie: {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .any(|c| c.contains("oauth_state") && c.contains("Max-Age=0")),
            "and expire the binding it consumed: {cookies:?}"
        );

        assert!(
            oauth_pkce::take(&ctx, STATE_ID)
                .await
                .expect("take ok")
                .is_none(),
            "the single-use state must be gone, not merely reported as taken"
        );

        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("OAuth callback created the user");

        let link = provider_links::find_by_provider_ref(&ctx, "google", GOOGLE_ID)
            .await
            .expect("link lookup ok")
            .expect("OAuth callback created the provider link");
        assert_eq!(link.user_id, user.id);

        let session_rows = sessions::list_for_user(&ctx, &user.id)
            .await
            .expect("list sessions ok");
        assert_eq!(
            session_rows.len(),
            1,
            "OAuth login must persist exactly one session row"
        );
    }

    /// The provider's access token is a bearer credential for the user's
    /// account at that provider. The sign-in is done with it once the profile
    /// is fetched, so the link row the callback writes must not carry it: the
    /// mock provider hands out `mock-access-token`, and the stored column,
    /// read raw, must not hold it.
    #[tokio::test]
    async fn the_provider_access_token_is_not_stored() {
        use wafer_core::clients::database as db;

        let ctx = OauthFlow::google("tokenless@example.com").ctx().await;
        let status = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(status, 302, "the sign-in itself succeeds");

        let rec = db::get_by_field(
            &ctx,
            provider_links::TABLE,
            "provider_ref",
            serde_json::json!(GOOGLE_ID),
        )
        .await
        .expect("the callback wrote the link row");
        assert_eq!(
            rec.data.get("access_token"),
            Some(&serde_json::json!("")),
            "the link row must not store the provider's access token: {:?}",
            rec.data
        );
    }

    /// Second sign-in with the same provider identity reuses the linked
    /// account: one user row, one link row, a session per login.
    #[tokio::test]
    async fn re_login_reuses_the_linked_account() {
        let email = "returning@example.com";
        let ctx = OauthFlow::google(email).ctx().await;

        let first = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(first, 302);
        let user = users::find_by_email(&ctx, email)
            .await
            .expect("user lookup ok")
            .expect("user present");

        // A second flow needs its own single-use state.
        seed_state(&ctx, STATE_ID, "google").await;
        let second = crate::test_support::output_status(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
        )
        .await;
        assert_eq!(second, 302, "a returning user signs in again");

        assert_eq!(
            users::find_by_email(&ctx, email)
                .await
                .expect("user lookup ok")
                .expect("user present")
                .id,
            user.id,
            "re-login must not create a second account"
        );
        let link = provider_links::find_by_provider_ref(&ctx, "google", GOOGLE_ID)
            .await
            .expect("link lookup ok")
            .expect("link present");
        assert_eq!(link.user_id, user.id);
        assert_eq!(
            sessions::list_for_user(&ctx, &user.id)
                .await
                .expect("list sessions ok")
                .len(),
            2,
            "each login leaves its own session row"
        );
    }

    /// #1 onboarding bug fix: a brand-new non-admin OAuth login must default
    /// into `/b/userportal/`, not the admin-only `/b/admin/` default.
    #[tokio::test]
    async fn oauth_login_non_admin_redirects_to_userportal() {
        let email = "oauthuser@example.com";
        let ctx = OauthFlow::google(email).ctx().await;

        let location = crate::test_support::output_header(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
            "Location",
        )
        .await
        .expect("302 redirect must set a Location header");
        assert!(
            location.ends_with("/b/userportal/"),
            "non-admin OAuth login must default to the user portal, not the \
             admin-only route: {location}"
        );
    }

    /// Companion to the above: an admin — the bootstrap-admin address, on a
    /// provider that asserts it — still gets the operator-configured admin
    /// default. The fix is role-aware, not a blanket redirect change.
    #[tokio::test]
    async fn oauth_login_admin_email_redirects_to_admin_home() {
        let email = "oauthadmin@example.com";
        let ctx = OauthFlow::google(email)
            .config(BOOTSTRAP_ADMIN_EMAIL_KEY, email)
            .ctx()
            .await;

        let location = crate::test_support::output_header(
            handle(&limiter(), &ctx, &callback_msg(&ctx).await).await,
            "Location",
        )
        .await
        .expect("302 redirect must set a Location header");
        assert!(
            location.ends_with("/b/admin/"),
            "admin OAuth login must still default to the admin home: {location}"
        );
    }

    // ---------------------------------------------------------------
    // Lifecycle
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn disabled_user_cannot_oauth_in() {
        // A DISABLED account holding the address the provider will return,
        // already linked to the provider identity so the flow reaches the
        // lifecycle gate under test rather than the adoption rule.
        let email = "disabled@example.com";
        let ctx = OauthFlow::google(email).ctx().await;
        let user = linked_account(&ctx, email).await;

        users::set_disabled(&ctx, &user.id, true)
            .await
            .expect("disable user");
        assert!(
            users::find_by_id(&ctx, &user.id)
                .await
                .unwrap()
                .unwrap()
                .disabled,
            "fixture user must be disabled"
        );

        // The callback rejects with a PermissionDenied error stream (mapped to
        // HTTP 403 at the boundary).
        let sessions_before = session_count(&ctx, &user.id).await;
        let out = handle(&limiter(), &ctx, &callback_msg(&ctx).await).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "disabled account must be rejected at the OAuth callback"
        );

        // And no session row was minted for the disabled user.
        assert_eq!(
            session_count(&ctx, &user.id).await,
            sessions_before,
            "no session may be created for a disabled OAuth login"
        );
    }

    /// Credential *issuance* paths (login / refresh / OAuth) must gate on
    /// soft-delete too, not just `disabled`. `db::soft_delete` leaves
    /// `local_credentials` and refresh tokens intact, so a soft-deleted user
    /// could otherwise authenticate and mint fresh tokens.
    #[tokio::test]
    async fn soft_deleted_user_cannot_oauth_in() {
        let email = "softdeleted@example.com";
        let ctx = OauthFlow::google(email).ctx().await;
        let user = linked_account(&ctx, email).await;

        // Soft-delete (stamps `deleted_at`) — NOT `disabled`. Mirrors the
        // lifecycle tests in `auth/repo/users.rs`.
        users::soft_delete(&ctx, &user.id)
            .await
            .expect("soft-delete user");
        let row = users::find_by_id(&ctx, &user.id).await.unwrap().unwrap();
        assert!(row.is_deleted(), "fixture user must be soft-deleted");
        assert!(!row.disabled, "fixture user must not be `disabled`");
        assert!(!row.is_active(), "soft-deleted user must not be active");

        let sessions_before = session_count(&ctx, &user.id).await;
        let out = handle(&limiter(), &ctx, &callback_msg(&ctx).await).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "soft-deleted account must be rejected at the OAuth callback"
        );

        assert_eq!(
            session_count(&ctx, &user.id).await,
            sessions_before,
            "no session may be created for a soft-deleted OAuth login"
        );
    }

    /// A password account already linked to the provider identity the mock
    /// returns — the branch that reuses `link.user_id`, which is the one path
    /// that can authenticate a user it never read.
    async fn linked_account(ctx: &TestContext, email: &str) -> users::UserRow {
        signup_password_account(ctx, email).await;
        let user = users::find_by_email(ctx, email)
            .await
            .expect("user lookup ok")
            .expect("signup created the row");
        provider_links::upsert(
            ctx,
            provider_links::NewLink {
                provider: "google",
                provider_ref: GOOGLE_ID,
                user_id: &user.id,
                provider_login: "linked",
            },
        )
        .await
        .expect("seed provider link");
        user
    }
}
