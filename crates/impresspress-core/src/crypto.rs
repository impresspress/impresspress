//! Impresspress auth-token policy on top of [`wafer_block_crypto::primitives`].
//!
//! The crypto primitives themselves (base64url, HMAC-SHA256, HS256 JWT
//! sign/verify, HKDF per-block key derivation, argon2id password hashing,
//! constant-time comparison, CSPRNG bytes) live in
//! `wafer_block_crypto::primitives` — the single source of truth shared by
//! the native runtime, impresspress-cloudflare, and impresspress-browser. Call them
//! directly; this module no longer mirrors them.
//!
//! What remains here is genuinely impresspress-specific policy: verifying an
//! access token and extracting auth meta from a `Bearer` token in the HTTP
//! pipeline — issuer check (SEC-038),
//! JWT blocklist (SEC-042), role mapping, and derived-key-only verification
//! (per-block HKDF from the auth-ui block id; the master-secret fallback was
//! removed — F40).

use wafer_block_crypto::primitives::{self, JwtExpPolicy};

// ---------------------------------------------------------------------------
// Auth meta extraction
// ---------------------------------------------------------------------------

/// Meta key holding the access JWT's `jti` (SEC-042) when present. Read by
/// the logout handler to blocklist the in-flight token.
pub const META_AUTH_JTI: &str = "auth.jti";

/// Meta key holding the access JWT's `exp` (UNIX seconds, as a string) when
/// present. Read by the logout handler to set the blocklist row's
/// `expires_at` (only needs to live as long as the original JWT).
pub const META_AUTH_EXP: &str = "auth.exp";

/// Meta key holding the access JWT's `family` — the refresh-rotation family
/// this login belongs to — when present. Read by the userportal sessions page
/// to mark the row for the device making the request. Set only from a token
/// [`verify_access_token`] accepted, so it can never be spoofed by a caller
/// putting a family on the request itself.
pub const META_AUTH_FAMILY: &str = "auth.family";

/// [SEC-038] The canonical JWT `iss` value for this deployment: the issuer
/// every token it mints carries, and the one [`verify_access_token`] is
/// handed to reject any other.
///
/// `WAFER_RUN_SHARED__FRONTEND_URL` doubles as the issuer: it's the only
/// per-deployment URL admins reliably set, and treating it as the issuer
/// means a token minted in dev (`http://localhost:5173`) won't validate
/// against a production secret if one leaks between environments.
pub async fn expected_issuer(
    ctx: &dyn wafer_run::context::Context,
) -> Result<String, wafer_run::WaferError> {
    wafer_core::clients::config::get_default(
        ctx,
        crate::config_vars::FRONTEND_URL_KEY,
        "http://localhost:5173",
    )
    .await
}

/// The claims of a verified access token, in the shape both consumers need.
///
/// Produced by [`verify_access_token`] and nowhere else: a value of this type
/// means the token's signature, `type`, issuer, blocklist status and
/// `auth_version` have all been checked. `roles` is already joined the way the
/// meta wants it, and the string fields are empty (not absent) when the claim
/// was missing, because every reader treats the two the same.
#[derive(Debug, Clone)]
pub struct AccessClaims {
    /// `sub` — the user id. `None` when the token carries no subject.
    pub sub: Option<String>,
    /// `email`, or `None` when absent.
    pub email: Option<String>,
    /// The `roles` array joined with `,`, falling back to the legacy `role`
    /// scalar, or `""` when neither is present.
    pub roles: String,
    /// `jti` (SEC-042), or `""`.
    pub jti: String,
    /// `exp` in UNIX seconds. Always present in practice — verification uses
    /// [`JwtExpPolicy::Required`] — but typed as an `Option` because the claim
    /// is read back out of the decoded map rather than out of the policy.
    pub exp: Option<i64>,
    /// `family` — the refresh-rotation family this login belongs to, or `""`
    /// on a token minted before the claim existed.
    pub family: String,
    /// `auth_method` — how the session was established (`"password"`,
    /// `"oauth.github"`, …), or `""` on a token that does not carry it.
    pub auth_method: String,
}

/// Verify an access token: `Ok(Some(claims))` when it authenticates,
/// `Ok(None)` when it does not, and `Err` when the check could not be
/// completed.
///
/// The single gate every access JWT passes through, in this order:
///
/// 1. HS256 signature against the `impresspress/auth-ui`-derived key
///    (`HKDF(jwt_secret, AUTH_UI_BLOCK_ID)`), with
///    [`JwtExpPolicy::Required`]: impresspress's mints all stamp `exp`, so an
///    exp-less token was not produced by this stack and accepting one would
///    create a forever-valid credential. The former master-secret fallback is
///    gone (F40) — production tokens are always signed by auth-ui through the
///    crypto service's `sign_for(caller_id, ..)`.
/// 2. Allow-list on `type`: only an explicit `"access"` authenticates. A
///    refresh token — or any token whose `type` is missing or something else
///    — is rejected. A denylist would silently accept a future token type
///    minted with the same key.
/// 3. [SEC-038] `iss` equals `expected_iss` (the deployment's canonical
///    issuer, `WAFER_RUN_SHARED__FRONTEND_URL`), so a leaked dev/staging
///    secret cannot authenticate against production. An empty `expected_iss`
///    disables the check — defensive, for a misconfigured deployment that
///    would otherwise silently 401 every request.
/// 4. [SEC-042] `jti` is not blocklisted. A blocklisted token was logged out
///    before its natural `exp`; it is treated exactly as if it had expired.
/// 5. [P2c] The embedded `auth_version` is not behind the user's stored
///    value — a password change, disable, soft-delete or role change (all of
///    which call `blocks::auth::bump_auth_version`) invalidates every
///    already-issued access JWT here instead of waiting out its expiry. A
///    missing claim defaults to `0`, matching the column's default, so tokens
///    minted before the claim existed keep working until the first bump. The
///    read goes through `current_auth_version`'s short-lived cache, so this
///    costs no DB round trip per request.
///
/// Rules 4 and 5 read the database, and a read that fails is neither answer:
/// accepting the token could honour a revoked credential, and rejecting it
/// tells a signed-in caller they are signed out — the router redirects their
/// pages to the login form, so a database blip would sign every user out of
/// the UI. So a failed read is `Err`, already classified
/// for the client by `blocks::auth::credential_check_failed` (a WRAP refusal
/// keeps its 403 or 429, anything else is a 503), and the caller answers the
/// request with it instead of treating the caller as anonymous.
///
/// Both consumers call this and nothing else: [`extract_auth_meta`] (the
/// pipeline's per-request meta population) and
/// `blocks::auth::service::AuthServiceImpl` (the `auth@v1` credential the
/// framework auth block authenticates).
pub async fn verify_access_token(
    ctx: &dyn wafer_run::context::Context,
    token: &str,
    jwt_secret: &str,
    expected_iss: &str,
) -> Result<Option<AccessClaims>, wafer_run::WaferError> {
    // Session tokens (access + refresh) are minted by the `impresspress/auth-ui`
    // block — login, signup, bootstrap, refresh, and the oauth callback all
    // hit handlers dispatched in that block's context, and the crypto handler
    // at wafer-core/src/interfaces/crypto/handler.rs routes CRYPTO_SIGN
    // through `sign_for(caller_id, ...)`. So the verify key is HKDF-derived
    // from `AUTH_UI_BLOCK_ID`, not `AUTH_BLOCK_ID`.
    let derived_secret = primitives::derive_block_key(
        jwt_secret.as_bytes(),
        crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
    );
    let Ok(claims) =
        primitives::jwt_verify(token, derived_secret.as_bytes(), JwtExpPolicy::Required)
    else {
        return Ok(None);
    };

    let token_type = claims.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if token_type != "access" {
        return Ok(None);
    }

    if !expected_iss.is_empty() {
        let iss = claims.get("iss").and_then(|v| v.as_str()).unwrap_or("");
        if iss != expected_iss {
            return Ok(None);
        }
    }

    let jti = claims.get("jti").and_then(|v| v.as_str()).unwrap_or("");
    if !jti.is_empty()
        && crate::blocks::auth::repo::jwt_blocklist::contains(ctx, jti)
            .await
            .map_err(|e| {
                crate::blocks::auth::credential_check_failed(e, "auth: jwt blocklist lookup")
            })?
    {
        return Ok(None);
    }

    let sub = claims.get("sub").and_then(|v| v.as_str());
    if let Some(uid) = sub {
        let claim_version = claims
            .get(crate::blocks::auth::repo::users::AUTH_VERSION_FIELD)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let current = crate::blocks::auth::current_auth_version(ctx, uid)
            .await
            .map_err(|e| {
                crate::blocks::auth::credential_check_failed(e, "auth: auth_version lookup")
            })?;
        if claim_version < current {
            return Ok(None);
        }
    }

    // Roles: prefer the structured `roles` array, fall back to the legacy
    // `role` scalar.
    let roles = if let Some(roles_arr) = claims.get("roles").and_then(|v| v.as_array()) {
        roles_arr
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(",")
    } else {
        claims
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    Ok(Some(AccessClaims {
        sub: sub.map(str::to_owned),
        email: claims
            .get("email")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        roles,
        jti: jti.to_string(),
        exp: claims.get("exp").and_then(|v| v.as_i64()),
        family: claims
            .get("family")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        auth_method: claims
            .get("auth_method")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    }))
}

/// Extract JWT claims from an `Authorization: Bearer <token>` header and
/// set auth meta fields on the message.
///
/// Sets: `auth.user_id`, `auth.user_email`, `auth.user_roles`, and (when
/// present in the JWT) `auth.jti`, `auth.exp` and `auth.family`.
///
/// Sets nothing when [`verify_access_token`] refuses the token — the request
/// continues as unauthenticated. Returns its `Err` when the check could not
/// be completed, which the pipeline answers the request with: the caller is
/// neither authenticated nor anonymous until the check can run. Every
/// rejection rule and its reasoning lives there; this function is the
/// meta-setting shell over it.
pub async fn extract_auth_meta(
    ctx: &dyn wafer_run::context::Context,
    auth_header: &str,
    jwt_secret: &str,
    expected_iss: &str,
    msg: &mut wafer_run::Message,
) -> Result<(), wafer_run::WaferError> {
    use wafer_run::*;

    let Some(token) = auth_header.strip_prefix("Bearer ") else {
        return Ok(());
    };
    let Some(claims) = verify_access_token(ctx, token, jwt_secret, expected_iss).await? else {
        return Ok(());
    };

    if let Some(sub) = claims.sub.as_deref() {
        msg.set_meta(META_AUTH_USER_ID, sub);
    }
    if let Some(email) = claims.email.as_deref() {
        msg.set_meta(META_AUTH_USER_EMAIL, email);
    }
    // Always stamped, even empty: `util::is_admin` and the WebMCP tier filter
    // read this key, and an absent key and an empty one must not differ.
    msg.set_meta(META_AUTH_USER_ROLES, &claims.roles);

    // Stash jti + exp so logout can read them without re-verifying the JWT,
    // and the family so the userportal can mark the calling device.
    if !claims.jti.is_empty() {
        msg.set_meta(META_AUTH_JTI, &claims.jti);
    }
    if let Some(exp) = claims.exp {
        msg.set_meta(META_AUTH_EXP, exp.to_string());
    }
    if !claims.family.is_empty() {
        msg.set_meta(META_AUTH_FAMILY, &claims.family);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Duration};

    use super::*;

    // -- Consumer-side pinning tests --------------------------------------
    //
    // These pin the parts of the `wafer_block_crypto::primitives` contract
    // that impresspress's session-token handling depends on. The primitives
    // module carries its own exhaustive test suite; the point here is to
    // fail INSIDE impresspress if the producer's policy ever shifts under us
    // (the exp-required policy and the HKDF derivation format both have a
    // documented history of cross-component drift).

    #[test]
    fn pin_jwt_sign_verify_roundtrip() {
        let secret = b"test-secret-padded-to-32-bytes-or-more";
        let mut claims = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-123"));
        let token = primitives::jwt_sign(claims, Duration::from_secs(3600), secret).unwrap();
        let verified = primitives::jwt_verify(&token, secret, JwtExpPolicy::Required).unwrap();
        assert_eq!(verified["sub"], "user-123");
        assert!(verified.contains_key("iat"));
        assert!(verified.contains_key("exp"));
    }

    /// Pins the exp-required policy `extract_auth_meta` verifies with: an
    /// exp-less token is a forever-valid credential and must be rejected.
    /// (This policy was the subject of the historical impresspress ↔ wafer
    /// drift; see the `JwtExpPolicy` docs.)
    #[test]
    fn pin_jwt_verify_rejects_missing_exp() {
        let secret = b"test-secret-padded-to-32-bytes-or-more";
        // jwt_sign always stamps exp, so hand-craft an exp-less token.
        let payload_b64 = primitives::b64url_encode(br#"{"sub":"user-123"}"#);
        let header_b64 = primitives::b64url_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let signing_input = format!("{header_b64}.{payload_b64}");
        let sig = primitives::hmac_sha256(secret, signing_input.as_bytes());
        let token = format!("{signing_input}.{}", primitives::b64url_encode(&sig));

        let err = primitives::jwt_verify(&token, secret, JwtExpPolicy::Required)
            .expect_err("exp-less token must be rejected");
        assert!(err.to_string().contains("missing exp"), "got: {err}");
    }

    /// Pins the HKDF per-block derivation format (`wafer-jwt|{block_id}`,
    /// 32-byte output, lowercase hex). Cross-component contract: tokens
    /// minted by the CF Worker / browser / native runtime must all verify
    /// against the same derived key. Do not update the expected value to
    /// make this pass — fix the derivation instead.
    #[test]
    fn pin_derive_block_key_known_answer() {
        assert_eq!(
            primitives::derive_block_key(b"test-master-secret", "wafer-run/auth"),
            "d1890540d7b988dba070cf5f37336ab51bd061e3caf8f5b113e68a59dd764e80"
        );
    }

    // -- extract_auth_meta -------------------------------------------------
    //
    // SEC-042 blocklist tests (and the other extract_auth_meta_* tests below)
    // sign via `sign_access_jwt`, which derives the auth-ui key from the
    // master secret before signing — the same derivation
    // `extract_auth_meta` verifies against, since the master-secret fallback
    // no longer exists.

    fn sign_access_jwt(secret: &str, sub: &str, jti: Option<&str>, ttl_secs: u64) -> String {
        let mut claims = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!(sub));
        claims.insert("type".to_string(), serde_json::json!("access"));
        if let Some(j) = jti {
            claims.insert("jti".to_string(), serde_json::json!(j));
        }
        // Sign with the auth-ui-derived key — the only key `extract_auth_meta`
        // accepts now. `secret` is the master; derive the same key the verifier
        // will use.
        let derived = primitives::derive_block_key(
            secret.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );
        primitives::jwt_sign(claims, Duration::from_secs(ttl_secs), derived.as_bytes())
            .expect("test jwt_sign")
    }

    /// `verify_access_token` is the one place an access JWT is checked;
    /// `extract_auth_meta` is a meta-setting shell over it and
    /// `AuthServiceImpl::extract_creds` calls the same function. These pin
    /// the shared contract directly rather than through the meta side effect.
    #[tokio::test]
    async fn verify_access_token_accepts_a_minted_access_jwt() {
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let secret = "test-secret";
        let token = sign_access_jwt(secret, "user-a", Some("jti-1"), 3600);
        let claims = verify_access_token(&ctx, &token, secret, "")
            .await
            .expect("the check completes")
            .expect("a freshly minted access token must verify");
        assert_eq!(claims.sub.as_deref(), Some("user-a"));
        assert_eq!(claims.jti, "jti-1");
    }

    /// The session's `auth_method` claim reaches the caller: a consumer that
    /// gates on how the user signed in (an OAuth-only admin action) reads it
    /// here rather than re-verifying the token itself.
    #[tokio::test]
    async fn verify_access_token_returns_the_auth_method() {
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let secret = "test-secret";
        let token = sign_access_jwt_with(secret, |claims| {
            claims.insert("sub".to_string(), serde_json::json!("user-a"));
            claims.insert("auth_method".to_string(), serde_json::json!("oauth.github"));
        });
        let claims = verify_access_token(&ctx, &token, secret, "")
            .await
            .expect("the check completes")
            .expect("the token verifies");
        assert_eq!(claims.auth_method, "oauth.github");

        let without = sign_access_jwt(secret, "user-a", None, 3600);
        let claims = verify_access_token(&ctx, &without, secret, "")
            .await
            .expect("the check completes")
            .expect("the token verifies");
        assert_eq!(claims.auth_method, "");
    }

    #[tokio::test]
    async fn verify_access_token_rejects_a_refresh_jwt() {
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let master = "test-secret";
        let derived = primitives::derive_block_key(
            master.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );
        let mut claims = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-a"));
        claims.insert("type".to_string(), serde_json::json!("refresh"));
        let token =
            primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes()).unwrap();
        assert!(verify_access_token(&ctx, &token, master, "")
            .await
            .expect("the check completes")
            .is_none());
    }

    #[tokio::test]
    async fn verify_access_token_rejects_a_foreign_issuer() {
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let secret = "test-secret";
        let token = sign_access_jwt_with(secret, |claims| {
            claims.insert("sub".to_string(), serde_json::json!("user-a"));
            claims.insert("iss".to_string(), serde_json::json!("https://elsewhere"));
        });
        assert!(verify_access_token(&ctx, &token, secret, "https://here")
            .await
            .expect("the check completes")
            .is_none());
    }

    #[tokio::test]
    async fn verify_access_token_rejects_a_blocklisted_jti() {
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let secret = "test-secret";
        let token = sign_access_jwt(secret, "user-a", Some("jti-gone"), 3600);
        crate::blocks::auth::repo::jwt_blocklist::insert(
            &ctx.fixture(),
            crate::blocks::auth::repo::jwt_blocklist::NewBlocklistEntry {
                jti: "jti-gone",
                user_id: "user-a",
                expires_at: "2099-01-01T00:00:00Z",
            },
        )
        .await
        .expect("insert blocklist row");
        assert!(verify_access_token(&ctx, &token, secret, "")
            .await
            .expect("the check completes")
            .is_none());
    }

    #[tokio::test]
    async fn verify_access_token_rejects_a_stale_auth_version() {
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let secret = "test-secret";
        let user = crate::blocks::auth::repo::users::insert(
            &ctx.fixture(),
            crate::blocks::auth::repo::users::NewUser {
                email: "stale@example.com".into(),
                display_name: "Stale".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .expect("seed user");
        crate::blocks::auth::bump_auth_version(&ctx.fixture(), &user.id)
            .await
            .expect("bump");

        // Token minted before the bump: auth_version 0 against a stored 1.
        let uid = user.id.clone();
        let token = sign_access_jwt_with(secret, |claims| {
            claims.insert("sub".to_string(), serde_json::json!(uid));
            claims.insert(
                crate::blocks::auth::repo::users::AUTH_VERSION_FIELD.to_string(),
                serde_json::json!(0),
            );
        });
        assert!(verify_access_token(&ctx, &token, secret, "")
            .await
            .expect("the check completes")
            .is_none());
    }

    /// The current-session badge reads the family off the *verified* token,
    /// so the claim has to reach the message as meta.
    #[tokio::test]
    async fn extract_auth_meta_sets_the_family_from_the_verified_token() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let secret = "test-secret";
        let token = sign_access_jwt_with(secret, |claims| {
            claims.insert("sub".to_string(), serde_json::json!("user-a"));
            claims.insert("family".to_string(), serde_json::json!("fam-42"));
        });
        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), secret, "", &mut msg)
            .await
            .expect("the check completes");
        assert_eq!(msg.get_meta(META_AUTH_FAMILY), "fam-42");
    }

    /// A token with no `family` claim leaves the meta empty rather than
    /// stamping a blank value that a reader could mistake for a match.
    #[tokio::test]
    async fn extract_auth_meta_leaves_the_family_empty_when_the_token_has_none() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let secret = "test-secret";
        let token = sign_access_jwt(secret, "user-a", None, 3600);
        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), secret, "", &mut msg)
            .await
            .expect("the check completes");
        assert_eq!(msg.get_meta(META_AUTH_FAMILY), "");
    }

    /// `sign_access_jwt` with arbitrary extra claims. `type` is always
    /// `"access"`; the caller adds `sub` and whatever else the case needs.
    fn sign_access_jwt_with(
        secret: &str,
        fill: impl FnOnce(&mut BTreeMap<String, serde_json::Value>),
    ) -> String {
        let mut claims = BTreeMap::new();
        claims.insert("type".to_string(), serde_json::json!("access"));
        fill(&mut claims);
        let derived = primitives::derive_block_key(
            secret.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );
        primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes())
            .expect("test jwt_sign")
    }

    #[tokio::test]
    async fn extract_auth_meta_rejects_refresh_token() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let master = "test-secret";
        let derived = primitives::derive_block_key(
            master.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );
        let mut claims = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-a"));
        claims.insert("type".to_string(), serde_json::json!("refresh"));
        let token =
            primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes()).unwrap();

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), master, "", &mut msg)
            .await
            .expect("the check completes");
        assert_eq!(msg.get_meta(wafer_run::META_AUTH_USER_ID), "");
    }

    #[tokio::test]
    async fn extract_auth_meta_rejects_typeless_token() {
        // Allow-list: a token with no `type` claim is rejected (the old denylist
        // accepted it).
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let master = "test-secret";
        let derived = primitives::derive_block_key(
            master.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );
        let mut claims = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-a"));
        let token =
            primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes()).unwrap();

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), master, "", &mut msg)
            .await
            .expect("the check completes");
        assert_eq!(msg.get_meta(wafer_run::META_AUTH_USER_ID), "");
    }

    #[tokio::test]
    async fn extract_auth_meta_rejects_master_secret_signed_token() {
        // The master-secret fallback is removed: a token signed with the raw
        // master secret (not the auth-ui-derived key) no longer authenticates.
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let master = "test-secret";
        let mut claims = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-a"));
        claims.insert("type".to_string(), serde_json::json!("access"));
        let token =
            primitives::jwt_sign(claims, Duration::from_secs(3600), master.as_bytes()).unwrap();

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), master, "", &mut msg)
            .await
            .expect("the check completes");
        assert_eq!(msg.get_meta(wafer_run::META_AUTH_USER_ID), "");
    }

    #[tokio::test]
    async fn extract_auth_meta_sets_user_id_for_valid_access_token() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let secret = "test-secret";
        let token = sign_access_jwt(secret, "user-a", Some("jti-1"), 3600);
        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), secret, "", &mut msg)
            .await
            .expect("the check completes");
        assert_eq!(msg.get_meta(wafer_run::META_AUTH_USER_ID), "user-a");
        assert_eq!(msg.get_meta(META_AUTH_JTI), "jti-1");
        assert!(!msg.get_meta(META_AUTH_EXP).is_empty());
    }

    /// Regression test for the bcf96ce → d7107c4 regression: production user
    /// JWTs are signed by the `impresspress/auth-ui` block via the crypto
    /// service's `sign_for(caller_id, ...)`, which derives the signing key
    /// via `HKDF(master, AUTH_UI_BLOCK_ID)`. `extract_auth_meta` must derive
    /// the verify key from the SAME block id, not `wafer-run/auth`. The
    /// other `extract_auth_meta_*` tests sign with the master secret
    /// directly and hit the master-fallback branch — they don't exercise
    /// the per-block-derived-key path that production actually uses.
    #[tokio::test]
    async fn extract_auth_meta_verifies_token_signed_with_auth_ui_derived_key() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let master = "test-master-secret";
        let derived = primitives::derive_block_key(
            master.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );

        let mut claims = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-prod"));
        claims.insert("type".to_string(), serde_json::json!("access"));
        claims.insert("jti".to_string(), serde_json::json!("jti-prod"));
        let token = primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes())
            .expect("sign with derived");

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), master, "", &mut msg)
            .await
            .expect("the check completes");

        assert_eq!(
            msg.get_meta(wafer_run::META_AUTH_USER_ID),
            "user-prod",
            "extract_auth_meta must verify JWTs signed with the auth-ui-derived key — \
             the production sign path goes through sign_for(AUTH_UI_BLOCK_ID, ...)"
        );
        assert_eq!(msg.get_meta(META_AUTH_JTI), "jti-prod");
    }

    #[tokio::test]
    async fn extract_auth_meta_rejects_blocklisted_jti() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let secret = "test-secret";
        let token = sign_access_jwt(secret, "user-a", Some("jti-blocked"), 3600);

        // Pre-populate the blocklist with the jti.
        crate::blocks::auth::repo::jwt_blocklist::insert(
            &ctx.fixture(),
            crate::blocks::auth::repo::jwt_blocklist::NewBlocklistEntry {
                jti: "jti-blocked",
                user_id: "user-a",
                expires_at: "2099-01-01T00:00:00Z",
            },
        )
        .await
        .expect("insert blocklist row");

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), secret, "", &mut msg)
            .await
            .expect("the check completes");
        // Blocklisted: no auth meta should be set — request continues as
        // anonymous, same as if the JWT had expired or been tampered with.
        assert_eq!(msg.get_meta(wafer_run::META_AUTH_USER_ID), "");
        assert_eq!(msg.get_meta(META_AUTH_JTI), "");
    }

    #[tokio::test]
    async fn extract_auth_meta_only_blocks_target_jti_for_user() {
        // Same user, two jti's — only the blocklisted one is rejected.
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let secret = "test-secret";
        crate::blocks::auth::repo::jwt_blocklist::insert(
            &ctx.fixture(),
            crate::blocks::auth::repo::jwt_blocklist::NewBlocklistEntry {
                jti: "session-1",
                user_id: "user-a",
                expires_at: "2099-01-01T00:00:00Z",
            },
        )
        .await
        .unwrap();

        let blocked = sign_access_jwt(secret, "user-a", Some("session-1"), 3600);
        let live = sign_access_jwt(secret, "user-a", Some("session-2"), 3600);

        let mut m1 = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {blocked}"), secret, "", &mut m1)
            .await
            .expect("the check completes");
        assert_eq!(m1.get_meta(wafer_run::META_AUTH_USER_ID), "");

        let mut m2 = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {live}"), secret, "", &mut m2)
            .await
            .expect("the check completes");
        assert_eq!(m2.get_meta(wafer_run::META_AUTH_USER_ID), "user-a");
    }

    // -- auth_version (P2c: "Access JWTs outlive account and role changes") --

    /// Same as [`sign_access_jwt`] but with an explicit `auth_version` claim,
    /// so tests can mint a token "as of" a specific version.
    fn sign_access_jwt_with_version(
        secret: &str,
        sub: &str,
        auth_version: i64,
        ttl_secs: u64,
    ) -> String {
        let mut claims = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!(sub));
        claims.insert("type".to_string(), serde_json::json!("access"));
        claims.insert(
            crate::blocks::auth::repo::users::AUTH_VERSION_FIELD.to_string(),
            serde_json::json!(auth_version),
        );
        let derived = primitives::derive_block_key(
            secret.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );
        primitives::jwt_sign(claims, Duration::from_secs(ttl_secs), derived.as_bytes())
            .expect("test jwt_sign")
    }

    async fn seed_user(ctx: &crate::test_support::TestContext) -> String {
        crate::blocks::auth::repo::users::insert(
            &ctx.fixture(),
            crate::blocks::auth::repo::users::NewUser {
                email: "verify@example.com".into(),
                display_name: "Verify".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn extract_auth_meta_rejects_token_minted_before_a_bump() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let uid = seed_user(&ctx).await;
        let secret = "test-secret";

        // Minted "before" any bump — embeds the user's then-current
        // auth_version (0).
        let token = sign_access_jwt_with_version(secret, &uid, 0, 3600);

        let mut before = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), secret, "", &mut before)
            .await
            .expect("the check completes");
        assert_eq!(
            before.get_meta(wafer_run::META_AUTH_USER_ID),
            uid,
            "an unbumped token must authenticate"
        );

        // Password change / disable / soft-delete / role change all funnel
        // through this single call.
        crate::blocks::auth::bump_auth_version(&ctx.fixture(), &uid)
            .await
            .expect("bump auth_version");

        let mut after = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), secret, "", &mut after)
            .await
            .expect("the check completes");
        assert_eq!(
            after.get_meta(wafer_run::META_AUTH_USER_ID),
            "",
            "the SAME token minted before the bump must be rejected after it"
        );
    }

    #[tokio::test]
    async fn extract_auth_meta_accepts_a_token_minted_at_the_current_auth_version() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let uid = seed_user(&ctx).await;
        let secret = "test-secret";

        crate::blocks::auth::bump_auth_version(&ctx.fixture(), &uid)
            .await
            .expect("bump auth_version");

        // Minted AFTER the bump, embedding the now-current version (1).
        let token = sign_access_jwt_with_version(secret, &uid, 1, 3600);

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), secret, "", &mut msg)
            .await
            .expect("the check completes");
        assert_eq!(
            msg.get_meta(wafer_run::META_AUTH_USER_ID),
            uid,
            "a token minted at the current auth_version must authenticate"
        );
    }

    #[tokio::test]
    async fn extract_auth_meta_treats_a_missing_auth_version_claim_as_zero() {
        // A token minted before this feature shipped carries no
        // `auth_version` claim at all. It must still authenticate against a
        // freshly migrated user, whose `auth_version` column defaults to 0.
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let uid = seed_user(&ctx).await;
        let secret = "test-secret";

        let token = sign_access_jwt(secret, &uid, None, 3600);
        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), secret, "", &mut msg)
            .await
            .expect("the check completes");
        assert_eq!(msg.get_meta(wafer_run::META_AUTH_USER_ID), uid);
    }

    /// Regression test for the WRAP-grant gap that broke every authenticated
    /// request end-to-end (caught by native/browser E2E, not by any unit
    /// test).
    ///
    /// `extract_auth_meta` runs pre-dispatch in the `ImpresspressRouterBlock`
    /// context (id `impresspress/router`), so `current_auth_version`'s read
    /// of `wafer_run__auth__users` is WRAP-checked as the ROUTER's identity,
    /// not the auth block's own. The fixture runs as the router against the
    /// grants the deployment carries — `auth_grants()` among them — so a
    /// missing grant here fails the same way it fails in production: the
    /// read is refused, and so is every request bearing an access JWT.
    #[tokio::test]
    async fn extract_auth_meta_auth_version_read_is_wrap_authorized_for_the_router() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth()
            .await
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
        let uid = seed_user(&ctx).await;
        let secret = "test-secret";
        let token = sign_access_jwt_with_version(secret, &uid, 0, 3600);

        // Same underlying in-memory DB (shallow `Clone`), but every call
        // through `wrapped` is WRAP-checked as `impresspress/router` —
        // exactly what the request pipeline does in production.
        let wrapped = ctx
            .clone()
            .running_as(crate::blocks::router::ROUTER_BLOCK_ID);

        let mut msg = Message::new("http.request");
        extract_auth_meta(&wrapped, &format!("Bearer {token}"), secret, "", &mut msg)
            .await
            .expect(
                "the router's auth_version read must be WRAP-authorized — if this fails, the \
                 router's read grant on wafer_run__auth__users (auth::service::auth_grants) \
                 is missing or doesn't cover the caller/table pair",
            );
        assert_eq!(
            msg.get_meta(wafer_run::META_AUTH_USER_ID),
            uid,
            "a valid access token must authenticate under WRAP enforcement"
        );
    }
}
