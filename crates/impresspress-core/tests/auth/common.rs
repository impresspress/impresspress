//! Shared test helpers for the `wafer-run/auth` integration tests.
//!
//! Every test runs on the crate-wide
//! [`impresspress_core::test_support::TestContext`], so each call it makes
//! goes through the gates the runtime applies — `requires`, the target's
//! interface, and WRAP on every service op — as the block whose code it is.
//! [`auth_fixture`] adds what these tests need on top of admin's migrations:
//! a real `wafer-run/crypto` block over [`TEST_MASTER_SECRET`], and that
//! secret as `WAFER_RUN__AUTH__JWT_SECRET`, which the auth service reads
//! synchronously to verify the access token a request carries — so a token
//! minted through [`MintAccessToken::mint_access_token`] verifies against
//! the key `crypto::verify_access_token` derives.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use impresspress_core::test_support::TestContext;

/// The fixture crypto service's master secret. Long enough for the HMAC-SHA256
/// minimum-length check.
pub const TEST_MASTER_SECRET: &str = "test-jwt-secret-padded-to-min-32-bytes-aaaa";

/// The issuer every fixture-minted token carries: `expected_issuer` reads
/// `WAFER_RUN_SHARED__FRONTEND_URL` through the config client, which this
/// fixture leaves unset, so the declared default is what the verifier
/// compares against.
pub const TEST_ISSUER: &str = "http://localhost:5173";

/// A fixture with admin's migrations applied — the tracking table every
/// other block's `apply_if_blessed` upserts into, which production creates
/// first — a real crypto block over [`TEST_MASTER_SECRET`], and that secret
/// as the JWT master, running as `block`.
///
/// `wafer-run/auth` for the repository, service, migration and bootstrap
/// tests, whose code is the auth block's; `impresspress/auth-ui` for the
/// login and refresh handlers. Stage rows through
/// [`TestContext::fixture`]: raw SQL is the admin block's alone.
pub async fn auth_fixture(block: &str) -> TestContext {
    let mut ctx = TestContext::with_admin().await;
    let crypto = Arc::new(
        wafer_block_crypto::service::Argon2JwtCryptoService::new(TEST_MASTER_SECRET.to_string())
            .expect("test secret is long enough"),
    );
    ctx.register_block(
        "wafer-run/crypto",
        Arc::new(wafer_core::service_blocks::crypto::CryptoBlock::new(crypto)),
    );
    ctx.set_config(
        impresspress_core::blocks::auth::JWT_SECRET_KEY,
        TEST_MASTER_SECRET,
    );
    ctx.running_as(block)
}

/// Mint an access JWT the way `auth_ui` mints one: through the fixture's
/// real crypto service, called by `impresspress/auth-ui`, so the service
/// signs with `sign_for(AUTH_UI_BLOCK_ID, ..)` — the derived key
/// `crypto::verify_access_token` verifies against.
pub(crate) trait MintAccessToken {
    /// `sub`, `type`, and `iss` are filled in; `extra` adds or overrides
    /// anything else the case needs (`family`, `roles`, `auth_version`, ...).
    async fn mint_access_token(
        &self,
        sub: &str,
        extra: &[(&str, serde_json::Value)],
        ttl: Duration,
    ) -> String;
}

impl MintAccessToken for TestContext {
    async fn mint_access_token(
        &self,
        sub: &str,
        extra: &[(&str, serde_json::Value)],
        ttl: Duration,
    ) -> String {
        let mut claims: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        claims.insert("sub".to_string(), serde_json::json!(sub));
        claims.insert("type".to_string(), serde_json::json!("access"));
        claims.insert("iss".to_string(), serde_json::json!(TEST_ISSUER));
        for (k, v) in extra {
            claims.insert((*k).to_string(), v.clone());
        }
        let as_auth_ui = self
            .fixture()
            .running_as(impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        wafer_core::clients::crypto::sign(&as_auth_ui, &claims, ttl)
            .await
            .expect("fixture crypto service signs the access token")
    }
}

/// Sign claims verbatim with the auth-ui-derived key, stamping no `iat`/`exp`.
///
/// The one thing [`MintAccessToken::mint_access_token`] cannot express: a
/// token whose `exp` is already in the past. `jwt_sign` always stamps `exp` as
/// `now + expiry` and `Duration` cannot be negative, so an already-expired
/// token has to be assembled from the primitives.
pub fn sign_access_token_expired(sub: &str, exp_unix: i64) -> String {
    use wafer_block_crypto::primitives;

    let derived = primitives::derive_block_key(
        TEST_MASTER_SECRET.as_bytes(),
        impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID,
    );
    let payload = serde_json::json!({
        "sub": sub,
        "type": "access",
        "iss": TEST_ISSUER,
        "exp": exp_unix,
    });
    let header_b64 = primitives::b64url_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload_b64 = primitives::b64url_encode(payload.to_string().as_bytes());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = primitives::hmac_sha256(derived.as_bytes(), signing_input.as_bytes());
    format!("{signing_input}.{}", primitives::b64url_encode(&sig))
}
