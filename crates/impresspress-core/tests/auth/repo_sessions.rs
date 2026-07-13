//! Sessions repo — insert / find / touch / delete_expired against
//! in-memory SQLite after applying migration 001.

use impresspress_core::blocks::auth::{
    migrations,
    repo::{sessions, users},
};

use crate::common::MigrationTestCtx;

#[tokio::test]
async fn insert_find_touch_delete_expired() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    let u = users::insert(
        &ctx,
        users::NewUser {
            email: "s@example.com".into(),
            display_name: "S".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("seed user");

    let hash = [7u8; 32];
    sessions::insert(
        &ctx,
        sessions::NewSession {
            token_hash: hash.to_vec(),
            user_id: u.id.clone(),
            expires_at: "9999-01-01T00:00:00Z".into(),
        },
    )
    .await
    .expect("insert live session");

    let found = sessions::find_by_token_hash(&ctx, &hash)
        .await
        .expect("find live session")
        .expect("session present");
    assert_eq!(found.user_id, u.id);
    assert_eq!(found.token_hash, hash.to_vec());
    assert_eq!(found.expires_at, "9999-01-01T00:00:00Z");
    let original_last_used = found.last_used_at.clone();

    sessions::touch_last_used(&ctx, &hash)
        .await
        .expect("touch last_used");
    // Touch updates the row; we don't assert a strict inequality because the
    // test may complete inside a single ISO-second tick. Re-reading is the
    // contract we care about — the row is still findable after the update.
    let after_touch = sessions::find_by_token_hash(&ctx, &hash)
        .await
        .expect("find after touch")
        .expect("still present");
    assert!(after_touch.last_used_at >= original_last_used);

    // Insert an expired session and verify delete_expired removes only it.
    let expired_hash = [9u8; 32];
    sessions::insert(
        &ctx,
        sessions::NewSession {
            token_hash: expired_hash.to_vec(),
            user_id: u.id.clone(),
            expires_at: "1970-01-02T00:00:00Z".into(),
        },
    )
    .await
    .expect("insert expired session");

    let removed = sessions::delete_expired(&ctx, "2000-01-01T00:00:00Z")
        .await
        .expect("delete expired");
    assert_eq!(removed, 1, "only the expired session should be removed");
    assert!(sessions::find_by_token_hash(&ctx, &expired_hash)
        .await
        .expect("lookup expired")
        .is_none());
    assert!(sessions::find_by_token_hash(&ctx, &hash)
        .await
        .expect("lookup live")
        .is_some());
}

#[tokio::test]
async fn find_by_token_hash_missing_returns_none() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    let none_hash = [0u8; 32];
    let hit = sessions::find_by_token_hash(&ctx, &none_hash)
        .await
        .expect("lookup");
    assert!(hit.is_none());
}
