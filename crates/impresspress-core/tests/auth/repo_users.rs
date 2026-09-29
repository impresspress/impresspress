//! Users repo — exercise insert / find_by_email / find_by_id against
//! in-memory SQLite after applying migration 001.

use impresspress_core::{
    blocks::auth::{migrations, repo::users},
    test_support::seed_user,
};

use crate::common::auth_fixture;

#[tokio::test]
async fn insert_then_find_by_email_and_id() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");

    let inserted = seed_user("a@example.com")
        .display_name("A")
        .insert(&ctx)
        .await;
    assert_eq!(inserted.email, "a@example.com");
    assert_eq!(inserted.role, "user");
    assert!(inserted.avatar_url.is_none());

    let by_email = users::find_by_email(&ctx, "a@example.com")
        .await
        .expect("find_by_email");
    assert_eq!(
        by_email.as_ref().map(|u| u.id.clone()),
        Some(inserted.id.clone())
    );

    let by_id = users::find_by_id(&ctx, &inserted.id)
        .await
        .expect("find_by_id");
    assert!(by_id.is_some());
    assert_eq!(by_id.as_ref().unwrap().email, "a@example.com");

    let missing = users::find_by_email(&ctx, "none@example.com")
        .await
        .expect("find_by_email missing");
    assert!(missing.is_none());

    let missing_id = users::find_by_id(&ctx, "nope")
        .await
        .expect("find_by_id missing");
    assert!(missing_id.is_none());
}

#[tokio::test]
async fn insert_with_avatar_roundtrips() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");

    let inserted = seed_user("b@example.com")
        .display_name("B")
        .avatar_url("https://example.com/a.png")
        .role("admin")
        .insert(&ctx)
        .await;

    let fetched = users::find_by_id(&ctx, &inserted.id)
        .await
        .expect("find_by_id")
        .expect("row present");
    assert_eq!(
        fetched.avatar_url.as_deref(),
        Some("https://example.com/a.png")
    );
    assert_eq!(fetched.role, "admin");
}
