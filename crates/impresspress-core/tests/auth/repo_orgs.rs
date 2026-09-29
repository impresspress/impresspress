//! Orgs repo against in-memory SQLite after applying every auth migration
//! (002 seeds the reserved orgs).
//!
//! No route claims an org yet, so the only writer of this table is migration
//! 002. What a future claim will lean on is the schema: `UNIQUE(name)` keeps
//! a reserved name unclaimable, and the partial unique index over
//! `(verified_via, verified_ref) WHERE is_reserved = 0` makes one provider
//! org claimable once while leaving reserved rows out of that rule. The
//! writes below are test-fixture inserts of the row a claim would write.

use std::collections::HashMap;

use impresspress_core::{
    blocks::auth::{migrations, repo::orgs},
    test_support::{seed_user, TestContext},
};
use serde_json::{json, Value};
use wafer_core::clients::database as db;
use wafer_run::WaferError;

use crate::common::auth_fixture;

async fn insert_org(
    ctx: &TestContext,
    name: &str,
    owner_user_id: Option<&str>,
    verified: Option<(&str, &str)>,
    is_reserved: bool,
) -> Result<(), WaferError> {
    let data: HashMap<String, Value> = [
        ("id", json!(format!("org-{name}"))),
        ("name", json!(name)),
        ("owner_user_id", json!(owner_user_id)),
        ("verified_via", json!(verified.map(|v| v.0))),
        ("verified_ref", json!(verified.map(|v| v.1))),
        ("is_reserved", json!(is_reserved)),
        ("created_at", json!("2026-01-01T00:00:00Z")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    db::create(ctx, orgs::TABLE, data).await.map(|_| ())
}

#[tokio::test]
async fn find_by_name_returns_none_for_unknown() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");
    let row = orgs::find_by_name(&ctx, "does-not-exist").await.unwrap();
    assert!(row.is_none());
}

#[tokio::test]
async fn a_reserved_seed_reads_back_reserved_and_unowned() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");
    let row = orgs::find_by_name(&ctx, "wafer-run")
        .await
        .expect("find_by_name")
        .expect("migration 002 seeds wafer-run");
    assert!(row.is_reserved, "{row:?}");
    assert_eq!(row.owner_user_id, None, "{row:?}");
    assert_eq!(row.verified_ref, None, "{row:?}");
}

#[tokio::test]
async fn a_reserved_name_cannot_be_claimed() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");
    let uid = seed_user("u@x.com").insert(&ctx).await.id;

    insert_org(
        &ctx,
        "wafer-run",
        Some(&uid),
        Some(("github", "wafer-run")),
        false,
    )
    .await
    .expect_err("UNIQUE(name) must refuse a claim of a reserved name");
    // The database service reports a constraint violation as a scrubbed
    // `Internal`, so the error cannot say which constraint fired. The control
    // does: the identical row under an unreserved name lands, so the name is
    // the only thing the refusal can have been about.
    insert_org(
        &ctx,
        "wafer-run-fork",
        Some(&uid),
        Some(("github", "wafer-run")),
        false,
    )
    .await
    .expect("the same claim under an unreserved name must land");

    let row = orgs::find_by_name(&ctx, "wafer-run")
        .await
        .expect("find_by_name")
        .expect("reserved row still present");
    assert!(
        row.is_reserved,
        "the reserved row must be untouched: {row:?}"
    );
    assert_eq!(row.owner_user_id, None, "{row:?}");
}

#[tokio::test]
async fn a_provider_org_is_claimable_once_but_reserved_rows_are_exempt() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration apply");
    let a = seed_user("a@x.com").insert(&ctx).await.id;
    let b = seed_user("b@x.com").insert(&ctx).await.id;

    // Two reserved rows naming the same provider org: the index's
    // `WHERE is_reserved = 0` leaves them out, so both land.
    insert_org(&ctx, "reserved-a", None, Some(("github", "shared")), true)
        .await
        .expect("a reserved row is outside the claim index");
    insert_org(&ctx, "reserved-b", None, Some(("github", "shared")), true)
        .await
        .expect("a second reserved row with the same ref is outside it too");

    // A claim of that same provider org is not blocked by the reserved rows...
    insert_org(&ctx, "acme", Some(&a), Some(("github", "shared")), false)
        .await
        .expect("reserved rows must not block the first real claim");
    // ...but a second claim of it by anyone is.
    insert_org(&ctx, "acme-sh", Some(&b), Some(("github", "shared")), false)
        .await
        .expect_err("one provider org is claimable once");
    // Control, for the reason given in `a_reserved_name_cannot_be_claimed`:
    // the same row naming a different provider org lands, so the refusal was
    // the claim index.
    insert_org(&ctx, "acme-sh", Some(&b), Some(("github", "other")), false)
        .await
        .expect("a claim of a different provider org must land");

    let owned: Vec<String> = orgs::list_for_user(&ctx, &a)
        .await
        .expect("list_for_user")
        .into_iter()
        .map(|o| o.name)
        .collect();
    assert_eq!(owned, vec!["acme".to_string()]);
    let owned_b: Vec<String> = orgs::list_for_user(&ctx, &b)
        .await
        .expect("list_for_user")
        .into_iter()
        .map(|o| o.name)
        .collect();
    assert_eq!(owned_b, vec!["acme-sh".to_string()]);
}
