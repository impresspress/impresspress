//! `AuthServiceImpl::user_profile` — reads the users row, maps the account's
//! grants to its role, returns an empty orgs vector (Plan C populates it).

use std::sync::Arc;

use impresspress_core::{
    blocks::auth::{migrations, service::AuthServiceImpl},
    test_support::seed_user,
};
use wafer_core::interfaces::auth::service::{AuthError, AuthService, Role, UserId};
use wafer_run::context::Context;

use crate::common::auth_fixture;

#[tokio::test]
async fn user_profile_returns_row_with_empty_orgs() {
    let ctx: Arc<dyn Context> =
        Arc::new(auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await);
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let u = seed_user("p@e.com")
        .display_name("P")
        .avatar_url("https://a/x.png")
        .role(impresspress_core::blocks::auth::repo::users::InitialRole::Admin)
        .insert(ctx.as_ref())
        .await;

    let svc = AuthServiceImpl;
    let p = svc
        .user_profile(&*ctx, UserId(u.id.clone()))
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
    let ctx: Arc<dyn Context> =
        Arc::new(auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await);
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let svc = AuthServiceImpl;
    let err = svc
        .user_profile(&*ctx, UserId("nonexistent".to_string()))
        .await
        .expect_err("missing user");
    assert!(
        matches!(err, AuthError::NotFound),
        "expected NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn user_profile_maps_non_admin_role_to_user() {
    let ctx: Arc<dyn Context> =
        Arc::new(auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await);
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let u = seed_user("ordinary@e.com")
        .display_name("O")
        .insert(ctx.as_ref())
        .await;

    let svc = AuthServiceImpl;
    let p = svc
        .user_profile(&*ctx, UserId(u.id))
        .await
        .expect("profile");
    assert!(matches!(p.role, Role::User));
    assert!(p.avatar_url.is_none());
}

/// The profile's role is the account's grants, exactly as `require_role`
/// reads them: an account an admin granted `admin` through IAM is an admin
/// to `auth@v1` consumers, and stops being one when the grant is removed.
/// Before auth migration 016 the profile read only the inline column, so it
/// called a grant-only admin a user and a demoted bootstrap admin an admin.
#[tokio::test]
async fn user_profile_role_follows_the_admin_grant() {
    use impresspress_core::platform_state::user_roles;

    let raw = Arc::new(auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await);
    let ctx: Arc<dyn Context> = raw.clone();
    migrations::apply(ctx.as_ref()).await.expect("migrations");

    let u = seed_user("granted@e.com").insert(ctx.as_ref()).await;
    async fn role(ctx: &dyn Context, id: &str) -> Role {
        AuthServiceImpl
            .user_profile(ctx, UserId(id.to_string()))
            .await
            .expect("profile")
            .role
    }
    assert!(matches!(role(&*ctx, &u.id).await, Role::User));

    // Staged as the fixture: every grant after an account's own is admin's
    // IAM's to write.
    let user_roles::Assigned::Created(grant) =
        user_roles::assign(&raw.fixture(), &u.id, "admin", "admin_1")
            .await
            .expect("grant admin")
    else {
        panic!("the grant is new");
    };
    assert!(matches!(role(&*ctx, &u.id).await, Role::Admin));

    user_roles::remove(&raw.fixture(), &grant.id)
        .await
        .expect("remove the grant");
    assert!(matches!(role(&*ctx, &u.id).await, Role::User));
}
