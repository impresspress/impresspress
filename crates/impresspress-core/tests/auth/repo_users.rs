//! Users repo — exercise insert / find_by_email / find_by_id against
//! in-memory SQLite after applying migration 001.

use impresspress_core::blocks::auth::{migrations, repo::users};

use crate::common::MigrationTestCtx;

#[tokio::test]
async fn insert_then_find_by_email_and_id() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    let inserted = users::insert(
        &ctx,
        users::NewUser {
            email: "a@example.com".into(),
            display_name: "A".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("insert user");
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
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    let inserted = users::insert(
        &ctx,
        users::NewUser {
            email: "b@example.com".into(),
            display_name: "B".into(),
            avatar_url: Some("https://example.com/a.png".into()),
            role: "admin".into(),
        },
    )
    .await
    .expect("insert");

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
