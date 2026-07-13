//! Row-level access over `wafer_run__auth__orgs`.
//!
//! Migration 001 creates the table with `UNIQUE(name)` and a partial unique
//! index over `(verified_via, verified_ref) WHERE is_reserved = 0` — this repo
//! surfaces the two distinct conflicts as two distinct error variants so the
//! HTTP layer (Plan C Cluster 2) can map each to its own 409.

use std::collections::HashMap;

use serde_json::{json, Value};
use uuid::Uuid;
use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::database as db;
use wafer_run::context::Context;

use super::{map_bool, map_opt_str, map_str, now_iso};

pub const TABLE: &str = "wafer_run__auth__orgs";

/// Full row shape returned by [`find_by_name`] and [`upsert_claimed`].
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

/// Errors surfaced by this repo. Two distinct unique-constraint violations
/// produce two distinct variants — callers in Plan C Cluster 2 need to map
/// each to its proper 409 message.
#[derive(thiserror::Error, Debug)]
pub enum OrgsRepoError {
    #[error("org with that name already exists")]
    NameTaken,
    #[error("that provider org is already claimed by another user")]
    AlreadyClaimed,
    #[error("db: {0}")]
    Db(String),
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<OrgRow, OrgsRepoError> {
    Ok(OrgRow {
        id: map_opt_str(m, "id").ok_or_else(|| OrgsRepoError::Db("missing id".into()))?,
        name: map_opt_str(m, "name").ok_or_else(|| OrgsRepoError::Db("missing name".into()))?,
        owner_user_id: map_opt_str(m, "owner_user_id"),
        verified_via: map_opt_str(m, "verified_via"),
        verified_ref: map_opt_str(m, "verified_ref"),
        is_reserved: map_bool(m, "is_reserved"),
        created_at: map_str(m, "created_at"),
    })
}

/// Look up a single org by its `name` column (UNIQUE). Returns `Ok(None)` if
/// no such row exists.
pub async fn find_by_name(ctx: &dyn Context, name: &str) -> Result<Option<OrgRow>, OrgsRepoError> {
    let rows = db::list_all(
        ctx,
        TABLE,
        vec![Filter {
            field: "name".into(),
            operator: FilterOp::Equal,
            value: json!(name),
        }],
    )
    .await
    .map_err(|e| OrgsRepoError::Db(format!("orgs find_by_name: {e}")))?;
    match rows.into_iter().next() {
        Some(r) => Ok(Some(row_from_map(&r.data)?)),
        None => Ok(None),
    }
}

/// Return all orgs owned by `user_id`, ordered by `created_at` ASC for
/// stable rendering. Empty Vec if the user owns none.
pub async fn list_for_user(ctx: &dyn Context, user_id: &str) -> Result<Vec<OrgRow>, OrgsRepoError> {
    let records = db::list_sorted(
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
    )
    .await
    .map_err(|e| OrgsRepoError::Db(format!("orgs list_for_user: {e}")))?;
    records.iter().map(|r| row_from_map(&r.data)).collect()
}

/// Payload for [`upsert_claimed`]. Borrowed fields — caller keeps ownership.
#[derive(Debug, Clone, Copy)]
pub struct NewClaim<'a> {
    pub name: &'a str,
    pub owner_user_id: &'a str,
    pub verified_via: &'a str,
    pub verified_ref: &'a str,
}

/// Insert a new claimed (non-reserved) org row. The two unique-constraint
/// conflicts are distinguished by running pre-checks against the table before
/// the INSERT: the `wafer-run/database` surface doesn't currently let us
/// inspect the constraint name that fired, so we fail fast with the right
/// variant instead of relying on the underlying error string shape.
///
/// Returns the inserted [`OrgRow`].
pub async fn upsert_claimed(
    ctx: &dyn Context,
    claim: NewClaim<'_>,
) -> Result<OrgRow, OrgsRepoError> {
    // 1) (verified_via, verified_ref) already claimed → AlreadyClaimed.
    let n = db::count(
        ctx,
        TABLE,
        &[
            Filter {
                field: "verified_via".into(),
                operator: FilterOp::Equal,
                value: json!(claim.verified_via),
            },
            Filter {
                field: "verified_ref".into(),
                operator: FilterOp::Equal,
                value: json!(claim.verified_ref),
            },
            Filter {
                field: "is_reserved".into(),
                operator: FilterOp::Equal,
                value: json!(false),
            },
        ],
    )
    .await
    .map_err(|e| OrgsRepoError::Db(format!("orgs claim-check: {e}")))?;
    if n > 0 {
        return Err(OrgsRepoError::AlreadyClaimed);
    }

    // 2) `name` already taken (by any row, reserved or claimed) → NameTaken.
    if find_by_name(ctx, claim.name).await?.is_some() {
        return Err(OrgsRepoError::NameTaken);
    }

    // 3) Insert.
    let id = Uuid::now_v7().to_string();
    let now = now_iso();
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("id".into(), json!(id));
    data.insert("name".into(), json!(claim.name));
    data.insert("owner_user_id".into(), json!(claim.owner_user_id));
    data.insert("verified_via".into(), json!(claim.verified_via));
    data.insert("verified_ref".into(), json!(claim.verified_ref));
    data.insert("is_reserved".into(), json!(false));
    data.insert("created_at".into(), json!(now));
    db::create(ctx, TABLE, data)
        .await
        .map_err(|e| OrgsRepoError::Db(format!("orgs insert: {e}")))?;

    find_by_name(ctx, claim.name)
        .await?
        .ok_or_else(|| OrgsRepoError::Db("insert returned no row".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestContext;

    #[tokio::test]
    async fn find_by_name_returns_none_for_missing_org() {
        let ctx = TestContext::with_auth().await;
        let result = find_by_name(&ctx, "nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn find_by_name_returns_inserted_org() {
        let ctx = TestContext::with_auth().await;

        // Create a user first (foreign key constraint on owner_user_id)
        db::exec_raw(
            &ctx,
            "INSERT INTO wafer_run__auth__users (id, email, display_name, role, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
            &[
                json!("user-a"),
                json!("alice@example.com"),
                json!("Alice"),
                json!("user"),
                json!("2026-01-01T00:00:00Z"),
                json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .unwrap();

        upsert_claimed(
            &ctx,
            NewClaim {
                name: "acme",
                owner_user_id: "user-a",
                verified_via: "github",
                verified_ref: "gh-1",
            },
        )
        .await
        .unwrap();

        let row = find_by_name(&ctx, "acme").await.unwrap().unwrap();
        assert_eq!(row.name, "acme");
        assert_eq!(row.owner_user_id.as_deref(), Some("user-a"));
        assert_eq!(row.verified_via.as_deref(), Some("github"));
        assert_eq!(row.verified_ref.as_deref(), Some("gh-1"));
    }

    #[tokio::test]
    async fn list_for_user_returns_only_caller_orgs_ordered_by_created_at() {
        let ctx = TestContext::with_auth().await;

        // Seed users (FK constraint on owner_user_id).
        for user_id in ["user-a", "user-b"] {
            db::exec_raw(
                &ctx,
                "INSERT INTO wafer_run__auth__users (id, email, display_name, role, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                &[
                    json!(user_id),
                    json!(format!("{user_id}@example.com")),
                    json!(user_id),
                    json!("user"),
                    json!("2026-01-01T00:00:00Z"),
                    json!("2026-01-01T00:00:00Z"),
                ],
            )
            .await
            .unwrap();
        }

        // user-a claims two orgs; user-b claims one.
        upsert_claimed(
            &ctx,
            NewClaim {
                name: "alpha",
                owner_user_id: "user-a",
                verified_via: "github",
                verified_ref: "gh-1",
            },
        )
        .await
        .unwrap();
        upsert_claimed(
            &ctx,
            NewClaim {
                name: "beta",
                owner_user_id: "user-a",
                verified_via: "google",
                verified_ref: "gg-2",
            },
        )
        .await
        .unwrap();
        upsert_claimed(
            &ctx,
            NewClaim {
                name: "gamma",
                owner_user_id: "user-b",
                verified_via: "github",
                verified_ref: "gh-3",
            },
        )
        .await
        .unwrap();

        let a = list_for_user(&ctx, "user-a").await.unwrap();
        let b = list_for_user(&ctx, "user-b").await.unwrap();
        let c = list_for_user(&ctx, "user-c").await.unwrap();

        let names_a: Vec<&str> = a.iter().map(|o| o.name.as_str()).collect();
        let names_b: Vec<&str> = b.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names_a, vec!["alpha", "beta"]);
        assert_eq!(names_b, vec!["gamma"]);
        assert!(c.is_empty());
    }
}
