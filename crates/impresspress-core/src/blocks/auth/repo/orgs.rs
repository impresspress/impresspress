//! Row-level access over `wafer_run__auth__orgs`.
//!
//! Read-only: the rows come from migration 002's reserved-name seeds. No
//! route claims an org yet; the claim write and its conflict errors belong
//! with the route that ships it. Migration 001's `UNIQUE(name)` and the
//! partial unique index over `(verified_via, verified_ref) WHERE is_reserved
//! = 0` are the constraints that write will meet.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_run::{context::Context, WaferError};

use super::{db_failed, internal_error, map_bool, map_opt_str, map_str};
use crate::db_read::{self, Bound};

pub const TABLE: &str = "wafer_run__auth__orgs";

/// Full row shape returned by [`find_by_name`] and [`list_for_user`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgRow {
    pub id: String,
    pub name: String,
    pub owner_user_id: Option<String>,
    pub verified_via: Option<String>,
    pub verified_ref: Option<String>,
    pub is_reserved: bool,
    pub created_at: String,
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<OrgRow, WaferError> {
    Ok(OrgRow {
        id: map_opt_str(m, "id").ok_or_else(|| internal_error("missing id"))?,
        name: map_opt_str(m, "name").ok_or_else(|| internal_error("missing name"))?,
        owner_user_id: map_opt_str(m, "owner_user_id"),
        verified_via: map_opt_str(m, "verified_via"),
        verified_ref: map_opt_str(m, "verified_ref"),
        is_reserved: map_bool(m, "is_reserved"),
        created_at: map_str(m, "created_at"),
    })
}

/// Look up a single org by its `name` column (UNIQUE). Returns `Ok(None)` if
/// no such row exists.
pub async fn find_by_name(ctx: &dyn Context, name: &str) -> Result<Option<OrgRow>, WaferError> {
    let rows = db_read::list_bounded(
        ctx,
        TABLE,
        vec![Filter {
            field: "name".into(),
            operator: FilterOp::Equal,
            value: json!(name),
        }],
        Bound::UniqueKey("orgs.name is declared UNIQUE"),
    )
    .await
    .map_err(|e| db_failed("orgs find_by_name", e))?;
    match rows.into_iter().next() {
        Some(r) => Ok(Some(row_from_map(&r.data)?)),
        None => Ok(None),
    }
}

/// Return all orgs owned by `user_id`, ordered by `created_at` ASC for
/// stable rendering. Empty Vec if the user owns none.
pub async fn list_for_user(ctx: &dyn Context, user_id: &str) -> Result<Vec<OrgRow>, WaferError> {
    let records = db_read::list_bounded_sorted(
        ctx,
        TABLE,
        vec![Filter {
            field: "owner_user_id".into(),
            operator: FilterOp::Equal,
            value: json!(user_id),
        }],
        vec![SortField {
            field: "created_at".into(),
            desc: false,
        }],
        Bound::OnePer("org one user owns"),
    )
    .await
    .map_err(|e| db_failed("orgs list_for_user", e))?;
    records.iter().map(|r| row_from_map(&r.data)).collect()
}

/// Row seeding for tests: with no claim route there is no production write
/// to go through, so tests insert the row the claim would have written.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::HashMap;

    use serde_json::{json, Value};
    use wafer_core::clients::database as db;
    use wafer_run::context::Context;

    use super::TABLE;

    /// Insert a claimed (non-reserved) org owned by `owner_user_id`.
    pub(crate) async fn seed_claimed_org(
        ctx: &dyn Context,
        name: &str,
        owner_user_id: &str,
        verified_via: &str,
        verified_ref: &str,
        created_at: &str,
    ) {
        let data: HashMap<String, Value> = [
            ("id", json!(format!("org-{name}"))),
            ("name", json!(name)),
            ("owner_user_id", json!(owner_user_id)),
            ("verified_via", json!(verified_via)),
            ("verified_ref", json!(verified_ref)),
            ("is_reserved", json!(false)),
            ("created_at", json!(created_at)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        db::create(ctx, TABLE, data)
            .await
            .expect("seed claimed org");
    }
}

#[cfg(test)]
mod tests {
    use super::{fixtures::seed_claimed_org, *};
    use crate::test_support::TestContext;

    #[tokio::test]
    async fn find_by_name_returns_none_for_missing_org() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        let result = find_by_name(&ctx, "nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn find_by_name_returns_inserted_org() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);

        // Create a user first (foreign key constraint on owner_user_id)
        ctx.seed_auth_user("user-a").await;
        seed_claimed_org(
            &ctx,
            "acme",
            "user-a",
            "github",
            "gh-1",
            "2026-01-01T00:00:00Z",
        )
        .await;

        let row = find_by_name(&ctx, "acme").await.unwrap().unwrap();
        assert_eq!(row.name, "acme");
        assert_eq!(row.owner_user_id.as_deref(), Some("user-a"));
        assert_eq!(row.verified_via.as_deref(), Some("github"));
        assert_eq!(row.verified_ref.as_deref(), Some("gh-1"));
        assert!(!row.is_reserved);
    }

    #[tokio::test]
    async fn list_for_user_returns_only_caller_orgs_ordered_by_created_at() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);

        // Seed users (FK constraint on owner_user_id).
        for user_id in ["user-a", "user-b"] {
            ctx.seed_auth_user(user_id).await;
        }

        // user-a owns two orgs, seeded newest-first so the order is the
        // sort's doing; user-b owns one.
        seed_claimed_org(
            &ctx,
            "beta",
            "user-a",
            "google",
            "gg-2",
            "2026-01-02T00:00:00Z",
        )
        .await;
        seed_claimed_org(
            &ctx,
            "alpha",
            "user-a",
            "github",
            "gh-1",
            "2026-01-01T00:00:00Z",
        )
        .await;
        seed_claimed_org(
            &ctx,
            "gamma",
            "user-b",
            "github",
            "gh-3",
            "2026-01-03T00:00:00Z",
        )
        .await;

        let a = list_for_user(&ctx, "user-a").await.unwrap();
        let b = list_for_user(&ctx, "user-b").await.unwrap();
        let c = list_for_user(&ctx, "user-c").await.unwrap();

        let names_a: Vec<&str> = a.iter().map(|o| o.name.as_str()).collect();
        let names_b: Vec<&str> = b.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names_a, vec!["alpha", "beta"]);
        assert_eq!(names_b, vec!["gamma"]);
        assert!(c.is_empty());
    }

    #[tokio::test]
    async fn list_for_user_surfaces_a_read_failure() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID)
            .break_reads();
        assert!(
            list_for_user(&ctx, "user-a").await.is_err(),
            "a failed read must not look like a user with no orgs"
        );
    }
}
