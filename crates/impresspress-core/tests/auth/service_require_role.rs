//! `AuthServiceImpl::require_role` — user role check + bootstrap-token
//! fast path for Admin.

use std::sync::Arc;

use impresspress_core::blocks::auth::{
    migrations,
    repo::{bootstrap_tokens, sessions, users},
    service::{hash_token, AuthServiceImpl, BlockState},
};
use wafer_core::interfaces::auth::service::{AuthError, AuthService, Role};
use wafer_run::{context::Context, Message};

use crate::common::MigrationTestCtx;

fn cookie(tok: &str) -> Message {
    let mut m = Message::new("auth.require_role");
    m.set_meta("http.header.cookie", format!("wafer_session={tok}"));
    m
}

fn bearer(tok: &str) -> Message {
    let mut m = Message::new("auth.require_role");
    m.set_meta("http.header.authorization", format!("Bearer {tok}"));
    m
}

#[tokio::test]
async fn require_role_user_admin_and_bootstrap_token() {
    let ctx: Arc<dyn Context> = Arc::new(MigrationTestCtx::new().await);
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let admin = users::insert(
        ctx.as_ref(),
        users::NewUser {
            email: "admin@e.com".into(),
            display_name: "A".into(),
            avatar_url: None,
            role: "admin".into(),
        },
    )
    .await
    .expect("seed admin");
    let plain = users::insert(
        ctx.as_ref(),
        users::NewUser {
            email: "user@e.com".into(),
            display_name: "U".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("seed user");

    for (user_id, raw) in [(&admin.id, "s-admin"), (&plain.id, "s-user")] {
        sessions::insert(
            ctx.as_ref(),
            sessions::NewSession {
                token_hash: hash_token(raw),
                user_id: user_id.clone(),
                expires_at: "9999-01-01T00:00:00Z".into(),
            },
        )
        .await
        .expect("seed session");
    }

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));

    // Admin session meets Role::Admin.
    let got = svc
        .require_role(&cookie("s-admin"), Role::Admin)
        .await
        .expect("admin session");
    assert_eq!(got.0, admin.id);

    // Plain user session does not.
    let err = svc
        .require_role(&cookie("s-user"), Role::Admin)
        .await
        .expect_err("plain user");
    assert!(
        matches!(err, AuthError::Forbidden),
        "expected Forbidden, got {err:?}"
    );

    // Any authenticated user meets Role::User.
    let got = svc
        .require_role(&cookie("s-user"), Role::User)
        .await
        .expect("user session");
    assert_eq!(got.0, plain.id);

    // Bootstrap-token fast path — an unexpired row grants Admin.
    let bt_raw = "bootstrap-raw";
    bootstrap_tokens::insert(ctx.as_ref(), hash_token(bt_raw), "9999-01-01T00:00:00Z")
        .await
        .expect("seed bootstrap token");
    svc.require_role(&bearer(bt_raw), Role::Admin)
        .await
        .expect("bootstrap-token grants admin");

    // Expired bootstrap → Unauthorized/Forbidden (falls through to PAT
    // lookup, which misses).
    let expired_raw = "bootstrap-expired";
    bootstrap_tokens::insert(
        ctx.as_ref(),
        hash_token(expired_raw),
        "1970-01-02T00:00:00Z",
    )
    .await
    .expect("seed expired bootstrap");
    let err = svc
        .require_role(&bearer(expired_raw), Role::Admin)
        .await
        .expect_err("expired bootstrap should fail");
    assert!(
        matches!(err, AuthError::Unauthorized | AuthError::Forbidden),
        "expected Unauthorized/Forbidden, got {err:?}"
    );
}
