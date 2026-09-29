//! `AuthServiceImpl::require_user` — the credential production actually
//! issues.
//!
//! Every authenticated request reaching impresspress carries an `auth_token`
//! cookie that `blocks::router` turns into `Authorization: Bearer <access
//! JWT>`. So a Bearer that verifies as an access JWT resolves to its `sub`,
//! any other Bearer is looked up as a personal access token, and a request
//! with no Bearer is unauthenticated. The `wafer_session` cookie this service
//! used to accept was issued by nothing in the repository.

use std::{sync::Arc, time::Duration};

use impresspress_core::{
    blocks::auth::{
        migrations,
        repo::{jwt_blocklist, pats, users},
        service::{hash_token, AuthServiceImpl, BlockState},
    },
    test_support::{seed_user, TestContext},
};
use sha2::{Digest, Sha256};
use wafer_core::interfaces::auth::service::{AuthError, AuthService};
use wafer_run::{context::Context, Message};

use crate::common::{
    auth_fixture, sign_access_token_expired, MintAccessToken, TEST_ISSUER, TEST_MASTER_SECRET,
};

fn msg_with_bearer(token: &str) -> Message {
    let mut m = Message::new("auth.require_user");
    m.set_meta("http.header.authorization", format!("Bearer {token}"));
    m
}

/// A seeded account's id — every case here identifies its user by id, never
/// by profile.
async fn seed_user_id(ctx: &dyn Context, email: &str) -> String {
    seed_user(email).display_name("R").insert(ctx).await.id
}

async fn fixture() -> (Arc<TestContext>, Arc<dyn Context>) {
    let raw = Arc::new(auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await);
    let ctx: Arc<dyn Context> = raw.clone();
    migrations::apply(ctx.as_ref()).await.expect("migrations");
    (raw, ctx)
}

#[tokio::test]
async fn require_user_accepts_an_access_jwt_and_a_pat_and_rejects_missing_creds() {
    let (raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "r@example.com").await;

    let access = raw
        .mint_access_token(&uid, &[], Duration::from_secs(3600))
        .await;

    // PAT — no expiry.
    let pat_raw = "wafer_pat_abc";
    let pat_hash = hash_token(pat_raw);
    pats::insert(
        ctx.as_ref(),
        pats::NewPat {
            token_hash: pat_hash.clone(),
            user_id: uid.clone(),
            name: "ci".into(),
            scopes: vec!["publish".into()],
            expires_at: None,
        },
    )
    .await
    .expect("seed pat");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));

    // Access JWT → user.
    let got = svc
        .require_user(&msg_with_bearer(&access))
        .await
        .expect("access jwt auth");
    assert_eq!(got.0, uid);

    // Bearer that is not a JWT → PAT lookup → user.
    let got = svc
        .require_user(&msg_with_bearer(pat_raw))
        .await
        .expect("bearer pat auth");
    assert_eq!(got.0, uid);

    // No creds → Unauthorized.
    let err = svc
        .require_user(&Message::new("x"))
        .await
        .expect_err("missing creds should fail");
    assert!(
        matches!(err, AuthError::Unauthorized),
        "expected Unauthorized, got {err:?}"
    );

    // Unknown bearer → not a JWT, no PAT row → Unauthorized.
    let err = svc
        .require_user(&msg_with_bearer("not-a-token"))
        .await
        .expect_err("unknown bearer");
    assert!(matches!(err, AuthError::Unauthorized));

    // Sanity: hash_token is sha256(raw).
    let expected = Sha256::digest(pat_raw.as_bytes()).to_vec();
    assert_eq!(pat_hash, expected);
}

/// The credential a browser actually sends is the `auth_token` cookie.
/// `blocks::router` resolves it into a Bearer *value it hands
/// `pipeline::handle_request`* and does not restamp the header on the
/// `Message`, so a service method reached on a cookie-authenticated request
/// sees no `Authorization` header at all. Without the cookie fallback this
/// service would answer `Unauthorized` to the one credential every signed-in
/// browser request carries — the trap the spec's "why route rather than stub"
/// paragraph warns about, just moved one layer down.
#[tokio::test]
async fn require_user_accepts_the_auth_token_cookie() {
    let (raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "cookie-jwt@example.com").await;
    let access = raw
        .mint_access_token(&uid, &[], Duration::from_secs(3600))
        .await;

    let mut msg = Message::new("auth.require_user");
    msg.set_meta("http.header.cookie", format!("auth_token={access}"));

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let got = svc
        .require_user(&msg)
        .await
        .expect("the auth_token cookie is the credential a browser sends");
    assert_eq!(got.0, uid);
}

/// The cookie is a *source* for the token, not a bypass: it goes through the
/// same verification, so a refresh token in the cookie authenticates nobody.
#[tokio::test]
async fn the_auth_token_cookie_is_still_verified() {
    let (raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "cookie-refresh@example.com").await;
    let refresh = raw
        .mint_access_token(
            &uid,
            &[("type", serde_json::json!("refresh"))],
            Duration::from_secs(3600),
        )
        .await;

    let mut msg = Message::new("auth.require_user");
    msg.set_meta("http.header.cookie", format!("auth_token={refresh}"));

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .require_user(&msg)
        .await
        .expect_err("a refresh token in the cookie must not authenticate");
    assert!(matches!(err, AuthError::Unauthorized), "got {err:?}");
}

/// A `wafer_session` cookie is not a credential. Nothing in the repository
/// ever issued one; this pins that presenting one authenticates nobody, so a
/// future re-introduction has to be deliberate.
#[tokio::test]
async fn require_user_ignores_a_wafer_session_cookie() {
    let (_raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "cookie@example.com").await;
    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));

    let mut msg = Message::new("auth.require_user");
    msg.set_meta("http.header.cookie", format!("wafer_session={uid}"));

    let err = svc
        .require_user(&msg)
        .await
        .expect_err("a session cookie must not authenticate");
    assert!(matches!(err, AuthError::Unauthorized), "got {err:?}");
}

/// The allow-list on `type`: a refresh JWT is a valid signature over valid
/// claims and still must not authenticate. It falls through to the PAT
/// lookup, finds nothing, and is refused.
#[tokio::test]
async fn require_user_rejects_a_refresh_jwt() {
    let (raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "refresh@example.com").await;

    let refresh = raw
        .mint_access_token(
            &uid,
            &[("type", serde_json::json!("refresh"))],
            Duration::from_secs(3600),
        )
        .await;

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .require_user(&msg_with_bearer(&refresh))
        .await
        .expect_err("a refresh token must not authenticate");
    assert!(matches!(err, AuthError::Unauthorized), "got {err:?}");
}

#[tokio::test]
async fn require_user_rejects_an_expired_access_jwt() {
    let (_raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "expired@example.com").await;

    let expired = sign_access_token_expired(&uid, chrono::Utc::now().timestamp() - 60);

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .require_user(&msg_with_bearer(&expired))
        .await
        .expect_err("an expired token must not authenticate");
    assert!(matches!(err, AuthError::Unauthorized), "got {err:?}");
}

/// SEC-042: logout blocklists the in-flight token's `jti`. The service must
/// honour the same blocklist the pipeline does, or a logged-out token would
/// keep authenticating on the `auth@v1` surface.
#[tokio::test]
async fn require_user_rejects_a_blocklisted_jti() {
    let (raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "blocked@example.com").await;

    let token = raw
        .mint_access_token(
            &uid,
            &[("jti", serde_json::json!("jti-blocked"))],
            Duration::from_secs(3600),
        )
        .await;
    jwt_blocklist::insert(
        ctx.as_ref(),
        jwt_blocklist::NewBlocklistEntry {
            jti: "jti-blocked",
            user_id: &uid,
            expires_at: "2099-01-01T00:00:00Z",
        },
    )
    .await
    .expect("blocklist the jti");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .require_user(&msg_with_bearer(&token))
        .await
        .expect_err("a blocklisted jti must not authenticate");
    assert!(matches!(err, AuthError::Unauthorized), "got {err:?}");
}

/// P2c: a password change, disable, soft-delete or role change bumps
/// `auth_version`, which invalidates every already-issued access JWT.
#[tokio::test]
async fn require_user_rejects_a_stale_auth_version() {
    let (raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "stale@example.com").await;

    let token = raw
        .mint_access_token(
            &uid,
            &[(users::AUTH_VERSION_FIELD, serde_json::json!(0))],
            Duration::from_secs(3600),
        )
        .await;
    users::bump_auth_version(ctx.as_ref(), &uid)
        .await
        .expect("bump auth_version");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .require_user(&msg_with_bearer(&token))
        .await
        .expect_err("a token behind the user's auth_version must not authenticate");
    assert!(matches!(err, AuthError::Unauthorized), "got {err:?}");
}

/// [SEC-038] A token minted against another deployment's issuer does not
/// authenticate here even when the signing secret matches.
#[tokio::test]
async fn require_user_rejects_a_foreign_issuer() {
    let (raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "iss@example.com").await;
    assert_eq!(
        TEST_ISSUER, "http://localhost:5173",
        "the fixture issuer must be `expected_issuer`'s declared default"
    );

    let token = raw
        .mint_access_token(
            &uid,
            &[("iss", serde_json::json!("https://elsewhere.example"))],
            Duration::from_secs(3600),
        )
        .await;

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .require_user(&msg_with_bearer(&token))
        .await
        .expect_err("a foreign issuer must not authenticate");
    assert!(matches!(err, AuthError::Unauthorized), "got {err:?}");
}

/// `ensure_active` runs on the JWT path too: a token minted before the
/// account was disabled must stop working, exactly as it does for a PAT.
#[tokio::test]
async fn require_user_rejects_a_disabled_account_on_the_jwt_path() {
    let (raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "disabled@example.com").await;

    let token = raw
        .mint_access_token(&uid, &[], Duration::from_secs(3600))
        .await;
    users::set_disabled(ctx.as_ref(), &uid, true)
        .await
        .expect("disable the account");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .require_user(&msg_with_bearer(&token))
        .await
        .expect_err("a disabled account must not authenticate");
    assert!(matches!(err, AuthError::Unauthorized), "got {err:?}");
}

#[tokio::test]
async fn require_user_rejects_an_expired_pat() {
    let (_raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "e@example.com").await;

    let expired_pat_raw = "expired-pat";
    pats::insert(
        ctx.as_ref(),
        pats::NewPat {
            token_hash: hash_token(expired_pat_raw),
            user_id: uid.clone(),
            name: "old".into(),
            scopes: vec!["publish".into()],
            expires_at: Some("1970-01-02T00:00:00Z".into()),
        },
    )
    .await
    .expect("seed expired pat");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .require_user(&msg_with_bearer(expired_pat_raw))
        .await
        .expect_err("expired pat");
    assert!(matches!(err, AuthError::Unauthorized));
}

/// The verifier is keyed on the deployment's own secret: a well-formed access
/// token signed with a different master secret is not a JWT as far as this
/// service is concerned, so it falls through to the PAT lookup and fails.
#[tokio::test]
async fn require_user_rejects_a_token_signed_with_a_foreign_secret() {
    use wafer_block_crypto::primitives;

    let (_raw, ctx) = fixture().await;
    let uid = seed_user_id(ctx.as_ref(), "foreign@example.com").await;
    assert_ne!(TEST_MASTER_SECRET, "some-other-deployments-master-secret");

    let derived = primitives::derive_block_key(
        b"some-other-deployments-master-secret",
        impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID,
    );
    let mut claims = std::collections::BTreeMap::new();
    claims.insert("sub".to_string(), serde_json::json!(uid));
    claims.insert("type".to_string(), serde_json::json!("access"));
    claims.insert("iss".to_string(), serde_json::json!(TEST_ISSUER));
    let token = primitives::jwt_sign(claims, Duration::from_secs(3600), derived.as_bytes())
        .expect("sign with a foreign key");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .require_user(&msg_with_bearer(&token))
        .await
        .expect_err("a foreign-secret token must not authenticate");
    assert!(matches!(err, AuthError::Unauthorized), "got {err:?}");
}
