//! `AuthServiceImpl::require_token(scope)` — rejects session cookies,
//! enforces PAT scope membership.

use std::sync::Arc;

use impresspress_core::blocks::auth::{
    migrations,
    repo::{pats, sessions, users},
    service::{hash_token, AuthServiceImpl, BlockState},
};
use wafer_core::interfaces::auth::service::{AuthError, AuthService, TokenScope};
use wafer_run::{context::Context, Message};

use crate::common::MigrationTestCtx;

fn bearer(tok: &str) -> Message {
    let mut m = Message::new("auth.require_token");
    m.set_meta("http.header.authorization", format!("Bearer {tok}"));
    m
}

fn cookie(tok: &str) -> Message {
    let mut m = Message::new("auth.require_token");
    m.set_meta("http.header.cookie", format!("wafer_session={tok}"));
    m
}

#[tokio::test]
async fn require_token_enforces_scope_and_rejects_session_cookies() {
    let ctx: Arc<dyn Context> = Arc::new(MigrationTestCtx::new().await);
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let u = users::insert(
        ctx.as_ref(),
        users::NewUser {
            email: "t@example.com".into(),
            display_name: "T".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("seed user");

    // PAT with publish scope → Ok.
    let ok_raw = "wafer_pat_ok";
    pats::insert(
        ctx.as_ref(),
        pats::NewPat {
            token_hash: hash_token(ok_raw),
            user_id: u.id.clone(),
            name: "ci".into(),
            scopes: vec!["publish".into()],
            expires_at: None,
        },
    )
    .await
    .expect("seed ok pat");

    // PAT without publish → Forbidden.
    let noscope_raw = "wafer_pat_noscope";
    pats::insert(
        ctx.as_ref(),
        pats::NewPat {
            token_hash: hash_token(noscope_raw),
            user_id: u.id.clone(),
            name: "readonly".into(),
            scopes: vec!["read".into()],
            expires_at: None,
        },
    )
    .await
    .expect("seed noscope pat");

    // Session cookie → Forbidden (scopes only apply to PATs).
    let sess_raw = "sess-raw";
    sessions::insert(
        ctx.as_ref(),
        sessions::NewSession {
            token_hash: hash_token(sess_raw),
            user_id: u.id.clone(),
            expires_at: "9999-01-01T00:00:00Z".into(),
        },
    )
    .await
    .expect("seed session");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));

    let got = svc
        .require_token(&bearer(ok_raw), TokenScope::Publish)
        .await
        .expect("scoped pat");
    assert_eq!(got.0, u.id);

    let err = svc
        .require_token(&bearer(noscope_raw), TokenScope::Publish)
        .await
        .expect_err("pat missing scope");
    assert!(
        matches!(err, AuthError::Forbidden),
        "expected Forbidden, got {err:?}"
    );

    let err = svc
        .require_token(&cookie(sess_raw), TokenScope::Publish)
        .await
        .expect_err("session rejected");
    assert!(
        matches!(err, AuthError::Forbidden),
        "expected Forbidden, got {err:?}"
    );

    let err = svc
        .require_token(&Message::new("x"), TokenScope::Publish)
        .await
        .expect_err("no creds");
    assert!(matches!(err, AuthError::Unauthorized));
}
