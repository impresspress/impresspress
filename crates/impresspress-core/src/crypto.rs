//! Impresspress auth-token policy on top of [`wafer_block_crypto::primitives`].
//!
//! The crypto primitives themselves (base64url, HMAC-SHA256, HS256 JWT
//! sign/verify, HKDF per-block key derivation, argon2id password hashing,
//! constant-time comparison, CSPRNG bytes) live in
//! `wafer_block_crypto::primitives` — the single source of truth shared by
//! the native runtime, impresspress-cloudflare, and impresspress-browser. Call them
//! directly; this module no longer mirrors them.
//!
//! What remains here is genuinely impresspress-specific policy: extracting auth
//! meta from a `Bearer` token in the HTTP pipeline — issuer check (SEC-038),
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

/// Extract JWT claims from an `Authorization: Bearer <token>` header and
/// set auth meta fields on the message.
///
/// Sets: `auth.user_id`, `auth.user_email`, `auth.user_roles`, and (when
/// present in the JWT) `auth.jti` + `auth.exp`.
///
/// Silently does nothing if the token is invalid, fails the issuer
/// check (SEC-038), is blocklisted (SEC-042), or isn't an `access`
/// token (allow-list: only `type == "access"` authenticates) — the
/// request continues as unauthenticated.
///
/// Verification uses [`JwtExpPolicy::Required`]: impresspress's token mints all
/// stamp `exp`, so an exp-less token was not produced by this stack and
/// accepting one would create a forever-valid credential.
///
/// [SEC-038] `expected_iss` is the deployment's canonical issuer
/// (`WAFER_RUN_SHARED__FRONTEND_URL`). Tokens whose `iss` claim doesn't
/// match are rejected as if they were unsigned — prevents a leaked
/// signing secret in dev/staging from authenticating against production
/// (and vice versa).
pub async fn extract_auth_meta(
    ctx: &dyn wafer_run::context::Context,
    auth_header: &str,
    jwt_secret: &str,
    expected_iss: &str,
    msg: &mut wafer_run::Message,
) {
    use wafer_run::*;

    let Some(token) = auth_header.strip_prefix("Bearer ") else {
        return;
    };

    // Session tokens (access + refresh) are minted by the `impresspress/auth-ui`
    // block — login, signup, bootstrap, refresh, and the oauth callback all
    // hit handlers dispatched in that block's context, and the crypto handler
    // at wafer-core/src/interfaces/crypto/handler.rs routes CRYPTO_SIGN
    // through `sign_for(caller_id, ...)`. So the verify key is HKDF-derived
    // from `AUTH_UI_BLOCK_ID`, not `AUTH_BLOCK_ID`.
    //
    // Production session tokens are ALWAYS signed with the auth-ui-derived key
    // (the crypto service's `sign_for(AUTH_UI_BLOCK_ID, ...)`). There is no
    // legitimate token signed with the raw master secret, so we verify against
    // the derived key only. The former master-secret fallback existed for test
    // fixtures and once masked a real regression (PR #170 silently reverted the
    // derived-key swap because tests only exercised the fallback branch).
    let derived_secret = primitives::derive_block_key(
        jwt_secret.as_bytes(),
        crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
    );
    let Ok(claims) =
        primitives::jwt_verify(token, derived_secret.as_bytes(), JwtExpPolicy::Required)
    else {
        return;
    };

    // Allow-list: only an explicit "access" token authenticates. A refresh
    // token — or any token whose `type` is missing or not "access" — is
    // rejected. A denylist ("reject only refresh") would silently accept any
    // future token type minted with the same key.
    let token_type = claims.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if token_type != "access" {
        return;
    }

    // [SEC-038] Require iss claim to match the deployment's expected issuer.
    // An empty expected_iss disables the check (defensive — should never be
    // empty in production, but a misconfigured deployment shouldn't 401
    // every request silently).
    if !expected_iss.is_empty() {
        let iss = claims.get("iss").and_then(|v| v.as_str()).unwrap_or("");
        if iss != expected_iss {
            return;
        }
    }

    // SEC-042: reject blocklisted JWTs after structural validation. A
    // blocklisted token was logged out before its natural exp; treat it
    // exactly as if it had expired (request continues as unauthenticated,
    // never as a different user).
    let jti = claims.get("jti").and_then(|v| v.as_str()).unwrap_or("");
    if !jti.is_empty() && crate::blocks::auth::repo::jwt_blocklist::contains(ctx, jti).await {
        return;
    }

    if let Some(sub) = claims.get("sub").and_then(|v| v.as_str()) {
        msg.set_meta(META_AUTH_USER_ID, sub);
    }
    if let Some(email) = claims.get("email").and_then(|v| v.as_str()) {
        msg.set_meta(META_AUTH_USER_EMAIL, email);
    }

    // Roles: prefer the structured `roles` array, fall back to the legacy
    // `role` scalar. Avoids allocating a `String` when the array is absent
    // or the legacy field is the only one present.
    if let Some(roles_arr) = claims.get("roles").and_then(|v| v.as_array()) {
        let joined = roles_arr
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(",");
        msg.set_meta(META_AUTH_USER_ROLES, &joined);
    } else if let Some(role) = claims.get("role").and_then(|v| v.as_str()) {
        msg.set_meta(META_AUTH_USER_ROLES, role);
    } else {
        msg.set_meta(META_AUTH_USER_ROLES, "");
    }

    // Stash jti + exp so logout can read them without re-verifying the JWT.
    if !jti.is_empty() {
        msg.set_meta(META_AUTH_JTI, jti);
    }
    if let Some(exp) = claims.get("exp").and_then(|v| v.as_i64()) {
        msg.set_meta(META_AUTH_EXP, exp.to_string());
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

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
        let mut claims = HashMap::new();
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
        let mut claims = HashMap::new();
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

    #[tokio::test]
    async fn extract_auth_meta_rejects_refresh_token() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth().await;
        let master = "test-secret";
        let derived = primitives::derive_block_key(
            master.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );
        let mut claims = HashMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-a"));
        claims.insert("type".to_string(), serde_json::json!("refresh"));
        let token =
            primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes()).unwrap();

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), master, "", &mut msg).await;
        assert_eq!(msg.get_meta(wafer_run::META_AUTH_USER_ID), "");
    }

    #[tokio::test]
    async fn extract_auth_meta_rejects_typeless_token() {
        // Allow-list: a token with no `type` claim is rejected (the old denylist
        // accepted it).
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth().await;
        let master = "test-secret";
        let derived = primitives::derive_block_key(
            master.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );
        let mut claims = HashMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-a"));
        let token =
            primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes()).unwrap();

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), master, "", &mut msg).await;
        assert_eq!(msg.get_meta(wafer_run::META_AUTH_USER_ID), "");
    }

    #[tokio::test]
    async fn extract_auth_meta_rejects_master_secret_signed_token() {
        // The master-secret fallback is removed: a token signed with the raw
        // master secret (not the auth-ui-derived key) no longer authenticates.
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth().await;
        let master = "test-secret";
        let mut claims = HashMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-a"));
        claims.insert("type".to_string(), serde_json::json!("access"));
        let token =
            primitives::jwt_sign(claims, Duration::from_secs(3600), master.as_bytes()).unwrap();

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), master, "", &mut msg).await;
        assert_eq!(msg.get_meta(wafer_run::META_AUTH_USER_ID), "");
    }

    #[tokio::test]
    async fn extract_auth_meta_sets_user_id_for_valid_access_token() {
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth().await;
        let secret = "test-secret";
        let token = sign_access_jwt(secret, "user-a", Some("jti-1"), 3600);
        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), secret, "", &mut msg).await;
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
        let ctx = crate::test_support::TestContext::with_auth().await;
        let master = "test-master-secret";
        let derived = primitives::derive_block_key(
            master.as_bytes(),
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
        );

        let mut claims = HashMap::new();
        claims.insert("sub".to_string(), serde_json::json!("user-prod"));
        claims.insert("type".to_string(), serde_json::json!("access"));
        claims.insert("jti".to_string(), serde_json::json!("jti-prod"));
        let token = primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes())
            .expect("sign with derived");

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), master, "", &mut msg).await;

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
        let ctx = crate::test_support::TestContext::with_auth().await;
        let secret = "test-secret";
        let token = sign_access_jwt(secret, "user-a", Some("jti-blocked"), 3600);

        // Pre-populate the blocklist with the jti.
        crate::blocks::auth::repo::jwt_blocklist::insert(
            &ctx,
            crate::blocks::auth::repo::jwt_blocklist::NewBlocklistEntry {
                jti: "jti-blocked",
                user_id: "user-a",
                expires_at: "2099-01-01T00:00:00Z",
            },
        )
        .await
        .expect("insert blocklist row");

        let mut msg = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {token}"), secret, "", &mut msg).await;
        // Blocklisted: no auth meta should be set — request continues as
        // anonymous, same as if the JWT had expired or been tampered with.
        assert_eq!(msg.get_meta(wafer_run::META_AUTH_USER_ID), "");
        assert_eq!(msg.get_meta(META_AUTH_JTI), "");
    }

    #[tokio::test]
    async fn extract_auth_meta_only_blocks_target_jti_for_user() {
        // Same user, two jti's — only the blocklisted one is rejected.
        use wafer_run::Message;
        let ctx = crate::test_support::TestContext::with_auth().await;
        let secret = "test-secret";
        crate::blocks::auth::repo::jwt_blocklist::insert(
            &ctx,
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
        extract_auth_meta(&ctx, &format!("Bearer {blocked}"), secret, "", &mut m1).await;
        assert_eq!(m1.get_meta(wafer_run::META_AUTH_USER_ID), "");

        let mut m2 = Message::new("http.request");
        extract_auth_meta(&ctx, &format!("Bearer {live}"), secret, "", &mut m2).await;
        assert_eq!(m2.get_meta(wafer_run::META_AUTH_USER_ID), "user-a");
    }
}
