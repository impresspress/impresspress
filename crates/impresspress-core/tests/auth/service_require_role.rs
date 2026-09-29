//! `AuthServiceImpl::require_role` — user role check + bootstrap-token
//! fast path for Admin.

use std::{sync::Arc, time::Duration};

use impresspress_core::{
    blocks::auth::{
        migrations,
        repo::bootstrap_tokens,
        service::{hash_token, AuthServiceImpl, BlockState},
    },
    test_support::seed_user,
};
use wafer_core::interfaces::auth::service::{AuthError, AuthService, Role};
use wafer_run::{context::Context, Message};

use crate::common::{auth_fixture, MintAccessToken};

fn bearer(tok: &str) -> Message {
    let mut m = Message::new("auth.require_role");
    m.set_meta("http.header.authorization", format!("Bearer {tok}"));
    m
}

#[tokio::test]
async fn require_role_user_admin_and_bootstrap_token() {
    let raw_ctx = Arc::new(auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await);
    let ctx: Arc<dyn Context> = raw_ctx.clone();
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let admin = seed_user("admin@e.com")
        .display_name("A")
        .role("admin")
        .insert(ctx.as_ref())
        .await;
    let plain = seed_user("user@e.com")
        .display_name("U")
        .insert(ctx.as_ref())
        .await;

    // The credential production issues: an access JWT per user.
    let admin_jwt = raw_ctx
        .mint_access_token(&admin.id, &[], Duration::from_secs(3600))
        .await;
    let user_jwt = raw_ctx
        .mint_access_token(&plain.id, &[], Duration::from_secs(3600))
        .await;

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));

    // Admin's token meets Role::Admin. The role comes from the merged role
    // resolution (`users.role` plus the roles table), not from the token's
    // own `roles` claim, so a stale claim cannot grant admin.
    let got = svc
        .require_role(&bearer(&admin_jwt), Role::Admin)
        .await
        .expect("admin jwt");
    assert_eq!(got.0, admin.id);

    // Plain user does not.
    let err = svc
        .require_role(&bearer(&user_jwt), Role::Admin)
        .await
        .expect_err("plain user");
    assert!(
        matches!(err, AuthError::Forbidden),
        "expected Forbidden, got {err:?}"
    );

    // Any authenticated user meets Role::User.
    let got = svc
        .require_role(&bearer(&user_jwt), Role::User)
        .await
        .expect("user jwt");
    assert_eq!(got.0, plain.id);

    // Bootstrap-token fast path — an unexpired row grants Admin.
    let bt_raw = "bootstrap-raw";
    bootstrap_tokens::insert(ctx.as_ref(), hash_token(bt_raw), "9999-01-01T00:00:00Z")
        .await
        .expect("seed bootstrap token");
    svc.require_role(&bearer(bt_raw), Role::Admin)
        .await
        .expect("bootstrap-token grants admin");

    // Expired bootstrap → Unauthorized/Forbidden (falls through to the JWT
    // verify, which refuses it, then the PAT lookup, which misses).
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

/// A user granted admin only through the roles table (the admin IAM UI) is
/// admin to `auth@v1` consumers too — the token's own `roles` claim is never
/// consulted. [F19]
#[tokio::test]
async fn require_role_admin_comes_from_the_roles_table_not_the_token_claim() {
    let raw_ctx = Arc::new(auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await);
    let ctx: Arc<dyn Context> = raw_ctx.clone();
    migrations::apply(ctx.as_ref()).await.expect("migrations");
    impresspress_core::blocks::admin::migrations::apply(ctx.as_ref())
        .await
        .expect("admin migrations (user_roles lives in the platform tables)");

    let u = seed_user("claims@e.com")
        .display_name("C")
        .insert(ctx.as_ref())
        .await;

    // The token claims admin; the database does not agree.
    let lying = raw_ctx
        .mint_access_token(
            &u.id,
            &[("roles", serde_json::json!(["admin"]))],
            Duration::from_secs(3600),
        )
        .await;

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .require_role(&bearer(&lying), Role::Admin)
        .await
        .expect_err("an `admin` roles claim must not grant admin on its own");
    assert!(matches!(err, AuthError::Forbidden), "got {err:?}");

    // Grant admin in the roles table; the same token now satisfies the check.
    impresspress_core::platform_state::user_roles::assign(ctx.as_ref(), &u.id, "admin", "")
        .await
        .expect("grant admin through the roles table");
    let got = svc
        .require_role(&bearer(&lying), Role::Admin)
        .await
        .expect("roles-table admin");
    assert_eq!(got.0, u.id);
}
