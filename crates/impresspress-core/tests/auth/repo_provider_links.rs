//! Provider-links repo — exercise upsert idempotency and find lookup
//! against in-memory SQLite after applying migration 001.

use impresspress_core::blocks::auth::{
    migrations,
    repo::{provider_links, users},
};

use crate::common::MigrationTestCtx;

async fn mk_user(ctx: &MigrationTestCtx, email: &str) -> String {
    users::insert(
        ctx,
        users::NewUser {
            email: email.into(),
            display_name: email.into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("insert user")
    .id
}

#[tokio::test]
async fn upsert_insert_then_update_same_provider_ref() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");
    let uid1 = mk_user(&ctx, "a@example.com").await;
    let uid2 = mk_user(&ctx, "b@example.com").await;
    let uid3 = mk_user(&ctx, "c@example.com").await;

    // First call: no prior link → inserts.
    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "github",
            provider_ref: "42",
            user_id: &uid1,
            provider_login: "alice",
            access_token: "tok1",
        },
    )
    .await
    .expect("upsert insert");
    let got = provider_links::find_by_provider_ref(&ctx, "github", "42")
        .await
        .expect("find")
        .expect("row present");
    assert_eq!(got.user_id, uid1);
    assert_eq!(got.access_token, "tok1");
    assert_eq!(got.provider_login, "alice");

    // Second call, same (provider, provider_ref), different user + login +
    // token → updates in place.
    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "github",
            provider_ref: "42",
            user_id: &uid2,
            provider_login: "alice-renamed",
            access_token: "tok2",
        },
    )
    .await
    .expect("upsert update");
    let got = provider_links::find_by_provider_ref(&ctx, "github", "42")
        .await
        .expect("find")
        .expect("row present");
    assert_eq!(got.user_id, uid2);
    assert_eq!(got.access_token, "tok2");
    assert_eq!(got.provider_login, "alice-renamed");

    // Rows with distinct provider_ref are independent.
    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "github",
            provider_ref: "99",
            user_id: &uid3,
            provider_login: "carol",
            access_token: "tokC",
        },
    )
    .await
    .expect("upsert carol");
    assert_eq!(
        provider_links::find_by_provider_ref(&ctx, "github", "99")
            .await
            .expect("find")
            .expect("row")
            .user_id,
        uid3
    );
    // Original row still intact.
    assert_eq!(
        provider_links::find_by_provider_ref(&ctx, "github", "42")
            .await
            .expect("find")
            .expect("row")
            .user_id,
        uid2
    );
}

#[tokio::test]
async fn find_missing_is_none() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");
    assert!(provider_links::find_by_provider_ref(&ctx, "github", "nope")
        .await
        .expect("find")
        .is_none());
}

#[tokio::test]
async fn provider_axis_is_independent() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");
    let uid_gh = mk_user(&ctx, "gh@example.com").await;
    let uid_goog = mk_user(&ctx, "goog@example.com").await;

    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "github",
            provider_ref: "1",
            user_id: &uid_gh,
            provider_login: "alice",
            access_token: "tg",
        },
    )
    .await
    .expect("gh upsert");
    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "google",
            provider_ref: "1",
            user_id: &uid_goog,
            provider_login: "alice@g",
            access_token: "to",
        },
    )
    .await
    .expect("google upsert");

    let gh = provider_links::find_by_provider_ref(&ctx, "github", "1")
        .await
        .expect("find")
        .expect("gh row");
    let goog = provider_links::find_by_provider_ref(&ctx, "google", "1")
        .await
        .expect("find")
        .expect("goog row");
    assert_eq!(gh.user_id, uid_gh);
    assert_eq!(goog.user_id, uid_goog);
}
