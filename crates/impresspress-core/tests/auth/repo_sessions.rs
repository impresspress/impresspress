//! Sessions repo against in-memory SQLite after applying the auth
//! migrations — the family-keyed shape migration 012 installs (B12).

use impresspress_core::{
    blocks::auth::{migrations, repo::sessions},
    test_support::seed_user,
};

use crate::common::auth_fixture;

#[tokio::test]
async fn insert_find_touch_delete_expired() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");
    let uid = seed_user("s@example.com")
        .display_name("S")
        .insert(&ctx)
        .await
        .id;

    sessions::insert(
        &ctx,
        sessions::NewSession {
            family: "fam-live".into(),
            user_id: uid.clone(),
            auth_method: "password".into(),
            expires_at: "9999-01-01T00:00:00Z".into(),
        },
    )
    .await
    .expect("insert live session");

    let found = sessions::find_for_user(&ctx, &uid, "fam-live")
        .await
        .expect("find live session")
        .expect("session present");
    assert_eq!(found.user_id, uid);
    assert_eq!(found.family, "fam-live");
    assert_eq!(found.auth_method, "password");
    assert_eq!(found.expires_at, "9999-01-01T00:00:00Z");
    let original_last_used = found.last_used_at.clone();
    let original_created = found.created_at.clone();

    assert_eq!(
        sessions::touch(&ctx, "fam-live", "2100-01-01T00:00:00Z")
            .await
            .expect("touch the family"),
        1
    );
    // The test may complete inside a single ISO-second tick, so `last_used_at`
    // is asserted as non-decreasing rather than strictly greater. What the
    // touch must do exactly is carry the new expiry and leave `created_at`
    // alone — a rotation is not a new sign-in.
    let after_touch = sessions::find_for_user(&ctx, &uid, "fam-live")
        .await
        .expect("find after touch")
        .expect("still present");
    assert!(after_touch.last_used_at >= original_last_used);
    assert_eq!(after_touch.created_at, original_created);
    assert_eq!(after_touch.expires_at, "2100-01-01T00:00:00Z");

    // Insert an expired session and verify delete_expired removes only it.
    sessions::insert(
        &ctx,
        sessions::NewSession {
            family: "fam-dead".into(),
            user_id: uid.clone(),
            auth_method: "password".into(),
            expires_at: "1970-01-02T00:00:00Z".into(),
        },
    )
    .await
    .expect("insert expired session");

    let removed = sessions::delete_expired(&ctx, "2000-01-01T00:00:00Z")
        .await
        .expect("delete expired");
    assert_eq!(removed, 1, "only the expired session should be removed");
    let remaining = sessions::list_for_user(&ctx, &uid)
        .await
        .expect("list remaining");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].family, "fam-live");
}

#[tokio::test]
async fn find_for_user_missing_family_returns_none() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");
    let uid = seed_user("missing@example.com")
        .display_name("S")
        .insert(&ctx)
        .await
        .id;

    let hit = sessions::find_for_user(&ctx, &uid, "fam-nope")
        .await
        .expect("lookup");
    assert!(hit.is_none());
}

/// `touch` reporting zero is the signal issuance uses to insert a row
/// instead, which is what makes a device re-appear on the list after its row
/// was swept or dropped by migration 012.
#[tokio::test]
async fn touch_on_an_unknown_family_reports_zero_rather_than_erroring() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");

    assert_eq!(
        sessions::touch(&ctx, "fam-never-existed", "2100-01-01T00:00:00Z")
            .await
            .expect("touch must not error on a missing row"),
        0
    );
}
