//! `AuthServiceImpl::user_profile` — reads users row, maps role, returns
//! an empty orgs vector (Plan C populates it).

use std::sync::Arc;

use impresspress_core::blocks::auth::{
    migrations,
    repo::users,
    service::{AuthServiceImpl, BlockState},
};
use wafer_core::interfaces::auth::service::{AuthError, AuthService, Role, UserId};
use wafer_run::context::Context;

use crate::common::MigrationTestCtx;

#[tokio::test]
async fn user_profile_returns_row_with_empty_orgs() {
    let ctx: Arc<dyn Context> = Arc::new(MigrationTestCtx::new().await);
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let u = users::insert(
        ctx.as_ref(),
        users::NewUser {
            email: "p@e.com".into(),
            display_name: "P".into(),
            avatar_url: Some("https://a/x.png".into()),
            role: "admin".into(),
        },
    )
    .await
    .expect("seed user");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let p = svc
        .user_profile(UserId(u.id.clone()))
        .await
        .expect("profile");
    assert_eq!(p.id, UserId(u.id));
    assert_eq!(p.email, "p@e.com");
    assert_eq!(p.display_name, "P");
    assert_eq!(p.avatar_url.as_deref(), Some("https://a/x.png"));
    assert!(matches!(p.role, Role::Admin));
    assert!(p.orgs.is_empty(), "orgs populated by Plan C");
}

#[tokio::test]
async fn user_profile_missing_user_is_not_found() {
    let ctx: Arc<dyn Context> = Arc::new(MigrationTestCtx::new().await);
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let err = svc
        .user_profile(UserId("nonexistent".to_string()))
        .await
        .expect_err("missing user");
    assert!(
        matches!(err, AuthError::NotFound),
        "expected NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn user_profile_maps_non_admin_role_to_user() {
    let ctx: Arc<dyn Context> = Arc::new(MigrationTestCtx::new().await);
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let u = users::insert(
        ctx.as_ref(),
        users::NewUser {
            email: "ordinary@e.com".into(),
            display_name: "O".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("seed user");

    let svc = AuthServiceImpl::new(BlockState::for_test(ctx.clone()));
    let p = svc.user_profile(UserId(u.id)).await.expect("profile");
    assert!(matches!(p.role, Role::User));
    assert!(p.avatar_url.is_none());
}
