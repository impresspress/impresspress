//! Personal access tokens repo — insert / find / touch against in-memory
//! SQLite after applying migration 001.

use impresspress_core::blocks::auth::{
    migrations,
    repo::{pats, users},
};

use crate::common::MigrationTestCtx;

#[tokio::test]
async fn pat_insert_find_with_scopes() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    let u = users::insert(
        &ctx,
        users::NewUser {
            email: "p@example.com".into(),
            display_name: "P".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("seed user");

    let hash = [3u8; 32];
    pats::insert(
        &ctx,
        pats::NewPat {
            token_hash: hash.to_vec(),
            user_id: u.id.clone(),
            name: "ci".into(),
            scopes: vec!["publish".into()],
            expires_at: None,
        },
    )
    .await
    .expect("insert pat");

    let found = pats::find_by_token_hash(&ctx, &hash)
        .await
        .expect("find pat")
        .expect("pat present");
    assert_eq!(found.user_id, u.id);
    assert_eq!(found.name, "ci");
    assert_eq!(found.scopes, vec!["publish".to_string()]);
    assert_eq!(found.token_hash, hash.to_vec());
    assert!(found.expires_at.is_none());
    assert!(found.last_used_at.is_none());

    pats::touch_last_used(&ctx, &hash).await.expect("touch pat");
    let after_touch = pats::find_by_token_hash(&ctx, &hash)
        .await
        .expect("find after touch")
        .expect("still present");
    assert!(after_touch.last_used_at.is_some());
}

#[tokio::test]
async fn pat_insert_with_multi_scope_and_expiry() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    let u = users::insert(
        &ctx,
        users::NewUser {
            email: "m@example.com".into(),
            display_name: "M".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("seed user");

    let hash = [4u8; 32];
    pats::insert(
        &ctx,
        pats::NewPat {
            token_hash: hash.to_vec(),
            user_id: u.id.clone(),
            name: "release".into(),
            scopes: vec!["publish".into(), "read".into()],
            expires_at: Some("2099-12-31T23:59:59Z".into()),
        },
    )
    .await
    .expect("insert pat");

    let found = pats::find_by_token_hash(&ctx, &hash)
        .await
        .expect("find pat")
        .expect("pat present");
    assert_eq!(
        found.scopes,
        vec!["publish".to_string(), "read".to_string()]
    );
    assert_eq!(found.expires_at.as_deref(), Some("2099-12-31T23:59:59Z"));
}

#[tokio::test]
async fn pat_find_missing_returns_none() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    let none_hash = [0u8; 32];
    let hit = pats::find_by_token_hash(&ctx, &none_hash)
        .await
        .expect("lookup");
    assert!(hit.is_none());
}

#[tokio::test]
async fn pat_token_hash_is_stored_as_hex_string_not_json_byte_array() {
    // Regression guard for the 2026-05-14 rust best-practices review (L215).
    // Prior shape was `json!(new.token_hash)` which serialised to
    // `[12, 34, ...]`. Hex strings are the format the rest of the auth
    // surface uses (sessions, tokens, bootstrap_tokens) and the format
    // `decode_bytes` now expects from string-shaped rows.
    use wafer_core::clients::database as db;

    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    let u = users::insert(
        &ctx,
        users::NewUser {
            email: "hex@example.com".into(),
            display_name: "Hex".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("seed user");

    let raw_hash = [0xab_u8; 32];
    pats::insert(
        &ctx,
        pats::NewPat {
            token_hash: raw_hash.to_vec(),
            user_id: u.id.clone(),
            name: "wire-format".into(),
            scopes: vec![],
            expires_at: None,
        },
    )
    .await
    .expect("insert pat");

    let rows = db::list_all(&ctx, pats::TABLE, vec![])
        .await
        .expect("list pats");
    let stored = rows[0].data.get("token_hash").expect("token_hash present");
    let s = stored
        .as_str()
        .expect("token_hash is a string, not a JSON array");
    assert_eq!(s.len(), 64, "hex digest is 64 chars");
    assert!(
        s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "hex digest is lowercase ascii-hex: got {s:?}"
    );
    assert_eq!(s, "ab".repeat(32));
}
