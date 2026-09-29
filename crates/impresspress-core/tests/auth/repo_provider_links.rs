//! Provider-links repo — exercise upsert idempotency and find lookup
//! against in-memory SQLite after applying migration 001.

use impresspress_core::{
    blocks::auth::{migrations, repo::provider_links},
    test_support::seed_user,
};

use crate::common::auth_fixture;

#[tokio::test]
async fn upsert_insert_then_update_same_provider_ref() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");
    let uid1 = seed_user("a@example.com").insert(&ctx).await.id;
    let uid2 = seed_user("b@example.com").insert(&ctx).await.id;
    let uid3 = seed_user("c@example.com").insert(&ctx).await.id;

    // First call: no prior link → inserts.
    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "github",
            provider_ref: "42",
            user_id: &uid1,
            provider_login: "alice",
        },
    )
    .await
    .expect("upsert insert");
    let got = provider_links::find_by_provider_ref(&ctx, "github", "42")
        .await
        .expect("find")
        .expect("row present");
    assert_eq!(got.user_id, uid1);
    assert_eq!(got.provider_login, "alice");

    // Second call, same (provider, provider_ref), different user + login →
    // updates in place.
    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "github",
            provider_ref: "42",
            user_id: &uid2,
            provider_login: "alice-renamed",
        },
    )
    .await
    .expect("upsert update");
    let got = provider_links::find_by_provider_ref(&ctx, "github", "42")
        .await
        .expect("find")
        .expect("row present");
    assert_eq!(got.user_id, uid2);
    assert_eq!(got.provider_login, "alice-renamed");

    // Rows with distinct provider_ref are independent.
    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "github",
            provider_ref: "99",
            user_id: &uid3,
            provider_login: "carol",
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
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");
    assert!(provider_links::find_by_provider_ref(&ctx, "github", "nope")
        .await
        .expect("find")
        .is_none());
}

#[tokio::test]
async fn provider_axis_is_independent() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");
    let uid_gh = seed_user("gh@example.com").insert(&ctx).await.id;
    let uid_goog = seed_user("goog@example.com").insert(&ctx).await.id;

    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "github",
            provider_ref: "1",
            user_id: &uid_gh,
            provider_login: "alice",
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
