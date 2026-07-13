//! `AuthServiceImpl::require_user` — extracts Bearer / `wafer_session`
//! cookie, looks up in PATs / sessions, returns `UserId`.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use impresspress_core::blocks::auth::{
    migrations,
    repo::{pats, sessions, users},
    service::{hash_token, AuthServiceImpl, BlockState},
};
use wafer_core::interfaces::auth::service::{AuthError, AuthService};
use wafer_run::{context::Context, Message};

use crate::common::MigrationTestCtx;

fn msg_with_cookie(cookie: &str) -> Message {
    let mut m = Message::new("auth.require_user");
    m.set_meta("http.header.cookie", format!("wafer_session={cookie}"));
    m
}

fn msg_with_bearer(token: &str) -> Message {
    let mut m = Message::new("auth.require_user");
    m.set_meta("http.header.authorization", format!("Bearer {token}"));
    m
}

#[tokio::test]
async fn require_user_accepts_session_cookie_and_pat_and_rejects_missing() {
    let ctx: Arc<dyn Context> = Arc::new(MigrationTestCtx::new().await);
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let u = users::insert(
        ctx.as_ref(),
        users::NewUser {
            email: "r@example.com".into(),
            display_name: "R".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("seed user");

    // Session token — insert a live row.
    let sess_raw = "sess-raw-token";
    let sess_hash = hash_token(sess_raw);
    sessions::insert(
        ctx.as_ref(),
        sessions::NewSession {
            token_hash: sess_hash.clone(),
            user_id: u.id.clone(),
            expires_at: "9999-01-01T00:00:00Z".into(),
        },
    )
    .await
    .expect("seed session");

    // PAT — no expiry.
    let pat_raw = "wafer_pat_abc";
    let pat_hash = hash_token(pat_raw);
    pats::insert(
        ctx.as_ref(),
        pats::NewPat {
            token_hash: pat_hash.clone(),
            user_id: u.id.clone(),
            name: "ci".into(),
            scopes: vec!["publish".into()],
            expires_at: None,
        },
    )
    .await
    .expect("seed pat");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));

    // Cookie → user.
    let got = svc
        .require_user(&msg_with_cookie(sess_raw))
        .await
        .expect("cookie auth");
    assert_eq!(got.0, u.id);

    // Bearer → user.
    let got = svc
        .require_user(&msg_with_bearer(pat_raw))
        .await
        .expect("bearer auth");
    assert_eq!(got.0, u.id);

    // No creds → Unauthorized.
    let err = svc
        .require_user(&Message::new("x"))
        .await
        .expect_err("missing creds should fail");
    assert!(
        matches!(err, AuthError::Unauthorized),
        "expected Unauthorized, got {err:?}"
    );

    // Unknown bearer → Unauthorized.
    let err = svc
        .require_user(&msg_with_bearer("not-a-token"))
        .await
        .expect_err("unknown bearer");
    assert!(matches!(err, AuthError::Unauthorized));

    // Sanity: hash_token is sha256(raw).
    let expected = Sha256::digest(pat_raw.as_bytes()).to_vec();
    assert_eq!(pat_hash, expected);
}

#[tokio::test]
async fn require_user_rejects_expired_session_and_pat() {
    let ctx: Arc<dyn Context> = Arc::new(MigrationTestCtx::new().await);
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let u = users::insert(
        ctx.as_ref(),
        users::NewUser {
            email: "e@example.com".into(),
            display_name: "E".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("seed user");

    let expired_sess_raw = "expired-sess";
    sessions::insert(
        ctx.as_ref(),
        sessions::NewSession {
            token_hash: hash_token(expired_sess_raw),
            user_id: u.id.clone(),
            expires_at: "1970-01-02T00:00:00Z".into(),
        },
    )
    .await
    .expect("seed expired session");

    let expired_pat_raw = "expired-pat";
    pats::insert(
        ctx.as_ref(),
        pats::NewPat {
            token_hash: hash_token(expired_pat_raw),
            user_id: u.id.clone(),
            name: "old".into(),
            scopes: vec!["publish".into()],
            expires_at: Some("1970-01-02T00:00:00Z".into()),
        },
    )
    .await
    .expect("seed expired pat");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));

    let err = svc
        .require_user(&msg_with_cookie(expired_sess_raw))
        .await
        .expect_err("expired session");
    assert!(matches!(err, AuthError::Unauthorized));

    let err = svc
        .require_user(&msg_with_bearer(expired_pat_raw))
        .await
        .expect_err("expired pat");
    assert!(matches!(err, AuthError::Unauthorized));
}
