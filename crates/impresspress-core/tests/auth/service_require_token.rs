//! `AuthServiceImpl::require_token(scope)` — enforces PAT scope membership
//! and refuses a credential that carries no scopes at all.

use std::{sync::Arc, time::Duration};

use impresspress_core::{
    blocks::auth::{
        migrations,
        repo::pats,
        service::{hash_token, AuthServiceImpl, BlockState},
    },
    test_support::seed_user,
};
use wafer_core::interfaces::auth::service::{AuthError, AuthService, TokenScope};
use wafer_run::{context::Context, Message};

use crate::common::{auth_fixture, MintAccessToken};

fn bearer(tok: &str) -> Message {
    let mut m = Message::new("auth.require_token");
    m.set_meta("http.header.authorization", format!("Bearer {tok}"));
    m
}

#[tokio::test]
async fn require_token_enforces_scope_and_rejects_an_access_jwt() {
    let raw_ctx = Arc::new(auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await);
    let ctx: Arc<dyn Context> = raw_ctx.clone();
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let u = seed_user("t@example.com")
        .display_name("T")
        .insert(ctx.as_ref())
        .await;

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

    // A session access JWT is a valid credential of the wrong kind: scopes
    // live only on PATs, so this is Forbidden (the caller's credentials are
    // valid but cannot carry a scope), not Unauthorized.
    let access = raw_ctx
        .mint_access_token(&u.id, &[], Duration::from_secs(3600))
        .await;
    let err = svc
        .require_token(&bearer(&access), TokenScope::Publish)
        .await
        .expect_err("an access jwt carries no scopes");
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
