//! Orgs repo — exercise `find_by_name` + `upsert_claimed` against in-memory
//! SQLite after applying migration 001 (+ 002 which seeds reserved orgs).

use impresspress_core::blocks::auth::{
    migrations,
    repo::{
        orgs::{self, NewClaim, OrgsRepoError},
        users,
    },
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
async fn find_by_name_returns_none_for_unknown() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");
    let row = orgs::find_by_name(&ctx, "does-not-exist").await.unwrap();
    assert!(row.is_none());
}

#[tokio::test]
async fn upsert_claimed_inserts_then_finds() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");
    let uid = mk_user(&ctx, "u@x.com").await;
    let row = orgs::upsert_claimed(
        &ctx,
        NewClaim {
            name: "acme",
            owner_user_id: &uid,
            verified_via: "github",
            verified_ref: "acme",
        },
    )
    .await
    .expect("upsert_claimed");
    assert_eq!(row.name, "acme");
    assert_eq!(row.owner_user_id.as_deref(), Some(uid.as_str()));
    assert_eq!(row.verified_via.as_deref(), Some("github"));
    assert_eq!(row.verified_ref.as_deref(), Some("acme"));
    assert!(!row.is_reserved);

    let again = orgs::find_by_name(&ctx, "acme")
        .await
        .unwrap()
        .expect("row present");
    assert_eq!(again.id, row.id);
}

#[tokio::test]
async fn upsert_claimed_conflict_on_name() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");
    let a = mk_user(&ctx, "a@x.com").await;
    let b = mk_user(&ctx, "b@x.com").await;
    orgs::upsert_claimed(
        &ctx,
        NewClaim {
            name: "acme",
            owner_user_id: &a,
            verified_via: "github",
            verified_ref: "acme",
        },
    )
    .await
    .expect("first claim ok");
    let err = orgs::upsert_claimed(
        &ctx,
        NewClaim {
            name: "acme",
            owner_user_id: &b,
            verified_via: "github",
            verified_ref: "acme-org",
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, OrgsRepoError::NameTaken), "got {err:?}");
}

#[tokio::test]
async fn upsert_claimed_conflict_on_verified_ref() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");
    let a = mk_user(&ctx, "a@x.com").await;
    let b = mk_user(&ctx, "b@x.com").await;
    orgs::upsert_claimed(
        &ctx,
        NewClaim {
            name: "acme",
            owner_user_id: &a,
            verified_via: "github",
            verified_ref: "acme",
        },
    )
    .await
    .expect("first claim ok");
    let err = orgs::upsert_claimed(
        &ctx,
        NewClaim {
            name: "acme-sh",
            owner_user_id: &b,
            verified_via: "github",
            verified_ref: "acme",
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, OrgsRepoError::AlreadyClaimed), "got {err:?}");
}

#[tokio::test]
async fn reserved_orgs_do_not_block_claiming_same_provider_ref() {
    // Reserved orgs don't participate in the verified_via/verified_ref
    // conflict — the partial unique index has `WHERE is_reserved = 0`. A
    // reserved row with no provider ref shouldn't block a real claim.
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");
    // Migration 002 seeded 'wafer-run' as reserved with NULL verified_ref.
    let uid = mk_user(&ctx, "u@x.com").await;
    let row = orgs::upsert_claimed(
        &ctx,
        NewClaim {
            name: "wafer-run-fork",
            owner_user_id: &uid,
            verified_via: "github",
            verified_ref: "wafer-run-fork",
        },
    )
    .await
    .expect("claim distinct name with distinct ref");
    assert!(!row.is_reserved);
}
