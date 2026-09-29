//! Row-level access over `wafer_run__auth__provider_links`.
//!
//! One row per `(provider, provider_ref)` pair, which the table holds
//! `UNIQUE`. [`upsert`] updates the row for that pair in place when it
//! exists, so an OAuth login by the same user from the same provider
//! refreshes `provider_login`, `user_id`, and `linked_at`.
//!
//! The table's `access_token` column is never given a token. A provider
//! access token is a live bearer credential for the user's account at that
//! provider, the sign-in flow is done with it once the profile is fetched,
//! and nothing reads it back — so storing it would only leave a credential
//! wherever this table can be read. [`upsert`] writes the column empty, and
//! auth migration `014_clear_provider_access_tokens` empties the rows written
//! before it did. The column itself stays, `NOT NULL` as migration 001
//! declares it: SQLite cannot guard a `DROP COLUMN` with `IF EXISTS`, and auth
//! migrations re-run in full whenever the block's SQL changes.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::{
    db::{Filter, FilterOp, SortField},
    wire::database::BatchWrite,
};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::{db_failed, internal_error, map_opt_str, map_str, now_iso};
use crate::db_read::{self, Bound};

pub const TABLE: &str = "wafer_run__auth__provider_links";

/// Full row shape returned by [`find_by_provider_ref`]. `linked_at` is
/// included so higher layers can surface "last login via …" strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLink {
    pub provider: String,
    pub provider_ref: String,
    pub user_id: String,
    pub provider_login: String,
    pub linked_at: String,
}

/// Insert/update payload for [`upsert`]. All fields are borrowed so the
/// caller avoids allocating clones of handler-owned strings.
#[derive(Debug, Clone, Copy)]
pub struct NewLink<'a> {
    pub provider: &'a str,
    pub provider_ref: &'a str,
    pub user_id: &'a str,
    pub provider_login: &'a str,
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<ProviderLink, WaferError> {
    Ok(ProviderLink {
        provider: map_opt_str(m, "provider").ok_or_else(|| internal_error("missing provider"))?,
        provider_ref: map_opt_str(m, "provider_ref")
            .ok_or_else(|| internal_error("missing provider_ref"))?,
        user_id: map_opt_str(m, "user_id").ok_or_else(|| internal_error("missing user_id"))?,
        provider_login: map_str(m, "provider_login"),
        linked_at: map_str(m, "linked_at"),
    })
}

/// Insert a link row, or update `user_id`, `provider_login`, `linked_at` in
/// place (and empty `access_token`, see the module doc) when a row with the
/// same `(provider, provider_ref)` already exists. Manual two-step (list → update_by_filters or create) since
/// `db::*` has no two-key upsert primitive.
pub async fn upsert(ctx: &dyn Context, new: NewLink<'_>) -> Result<(), WaferError> {
    let filters = vec![
        Filter {
            field: "provider".into(),
            operator: FilterOp::Equal,
            value: json!(new.provider),
        },
        Filter {
            field: "provider_ref".into(),
            operator: FilterOp::Equal,
            value: json!(new.provider_ref),
        },
    ];
    let existing = db_read::list_bounded(
        ctx,
        TABLE,
        filters.clone(),
        Bound::UniqueKey("provider_links UNIQUE (provider, provider_ref)"),
    )
    .await
    .map_err(|e| db_failed("provider_links upsert lookup", e))?;

    if existing.is_empty() {
        db::create(ctx, TABLE, new_row(new))
            .await
            .map_err(|e| db_failed("provider_links insert", e))?;
    } else {
        // Update by the same (provider, provider_ref) filters — no synthetic id needed.
        let mut data: HashMap<String, Value> = HashMap::new();
        data.insert("user_id".into(), json!(new.user_id));
        data.insert("provider_login".into(), json!(new.provider_login));
        data.insert("access_token".into(), json!(""));
        data.insert("linked_at".into(), json!(now_iso()));
        db::update_by_filters(ctx, TABLE, filters, data)
            .await
            .map_err(|e| db_failed("provider_links update", e))?;
    }
    Ok(())
}

/// A new link row as one write of a batch: for an account created by the
/// sign-in that links it, so the account and the identity that owns it land
/// together. A pair that is already linked fails the batch with
/// `AlreadyExists` (the `(provider, provider_ref)` UNIQUE constraint).
pub fn create_op(new: NewLink<'_>) -> BatchWrite {
    BatchWrite::Create {
        collection: TABLE.to_string(),
        data: new_row(new),
    }
}

/// The row a new link inserts: the natural key, a fresh synthetic id, and
/// the columns [`upsert`] refreshes.
fn new_row(new: NewLink<'_>) -> HashMap<String, Value> {
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("id".into(), json!(uuid::Uuid::now_v7().to_string()));
    data.insert("provider".into(), json!(new.provider));
    data.insert("provider_ref".into(), json!(new.provider_ref));
    data.insert("user_id".into(), json!(new.user_id));
    data.insert("provider_login".into(), json!(new.provider_login));
    data.insert("access_token".into(), json!(""));
    data.insert("linked_at".into(), json!(now_iso()));
    data
}

/// Look up a link by `(provider, provider_ref)`. Returns `Ok(None)` if no
/// matching row exists.
pub async fn find_by_provider_ref(
    ctx: &dyn Context,
    provider: &str,
    provider_ref: &str,
) -> Result<Option<ProviderLink>, WaferError> {
    let filters = vec![
        Filter {
            field: "provider".into(),
            operator: FilterOp::Equal,
            value: json!(provider),
        },
        Filter {
            field: "provider_ref".into(),
            operator: FilterOp::Equal,
            value: json!(provider_ref),
        },
    ];
    let records = db_read::list_bounded(
        ctx,
        TABLE,
        filters,
        Bound::UniqueKey("provider_links UNIQUE (provider, provider_ref)"),
    )
    .await
    .map_err(|e| db_failed("provider_links find", e))?;
    match records.first() {
        Some(r) => Ok(Some(row_from_map(&r.data)?)),
        None => Ok(None),
    }
}

/// Return all OAuth provider links owned by `user_id`, ordered by
/// `linked_at` ASC for stable rendering on the security page.
///
/// Reads through the typed database client (via `db_read`), not raw SQL:
/// userportal's `/b/userportal/security` page calls it cross-block, and raw
/// SQL is admin-only under WRAP.
pub async fn list_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Vec<ProviderLink>, WaferError> {
    let records = db_read::list_bounded_sorted(
        ctx,
        TABLE,
        vec![Filter {
            field: "user_id".into(),
            operator: FilterOp::Equal,
            value: json!(user_id),
        }],
        vec![SortField {
            field: "linked_at".into(),
            desc: false,
        }],
        Bound::OnePer("OAuth provider identity one user has linked"),
    )
    .await
    .map_err(|e| db_failed("provider_links list_for_user", e))?;
    records.iter().map(|r| row_from_map(&r.data)).collect()
}

/// Delete every link this user holds with `provider`, returning how many rows
/// went. Scoped to `user_id` in the statement itself, so a caller cannot
/// unlink somebody else's account by naming their provider.
///
/// Keyed on the provider rather than on `(provider, provider_ref)`: the
/// account surface offers "unlink Google", and a user who has somehow bound
/// two Google identities to one account means all of them by that. Typed
/// `db::delete_by_filters_count` for the same reason [`list_for_user`] is
/// typed — it is called from `userportal`, cross-block, where raw SQL is
/// admin-only under WRAP.
pub async fn delete_for_user(
    ctx: &dyn Context,
    user_id: &str,
    provider: &str,
) -> Result<u64, WaferError> {
    let n = db::delete_by_filters_count(
        ctx,
        TABLE,
        vec![
            Filter {
                field: "user_id".into(),
                operator: FilterOp::Equal,
                value: json!(user_id),
            },
            Filter {
                field: "provider".into(),
                operator: FilterOp::Equal,
                value: json!(provider),
            },
        ],
    )
    .await
    .map_err(|e| db_failed("provider_links delete_for_user", e))?;
    Ok(n.max(0) as u64)
}

#[cfg(test)]
mod typed_client_tests {
    use super::*;
    use crate::test_support::TestContext;

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        ctx.seed_auth_user(user_id).await;
    }

    #[tokio::test]
    async fn upsert_inserts_then_updates_under_wrap() {
        // Seed BEFORE enabling WRAP — exec_raw fixture denied otherwise.
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        let ctx = ctx.running_as("wafer-run/auth");

        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice",
            },
        )
        .await
        .unwrap();
        // Re-upsert with a new login — should update in place, not insert a
        // duplicate.
        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice2",
            },
        )
        .await
        .unwrap();
        let got = find_by_provider_ref(&ctx, "github", "gh-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.provider_login, "alice2");
    }

    /// The stored `access_token` column, read raw — `ProviderLink` has no
    /// field for it, which is the point.
    async fn stored_access_token(ctx: &TestContext, provider_ref: &str) -> String {
        let rec = db::get_by_field(ctx, TABLE, "provider_ref", json!(provider_ref))
            .await
            .expect("link row exists");
        map_str(&rec.data, "access_token")
    }

    /// A link row written before tokens stopped being stored still holds one.
    /// The next sign-in through that link must clear it, not leave the
    /// credential sitting in the table.
    #[tokio::test]
    async fn upsert_clears_a_token_an_existing_row_still_holds() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        let mut legacy = HashMap::new();
        for (k, v) in [
            ("id", "link-1"),
            ("provider", "github"),
            ("provider_ref", "gh-1"),
            ("user_id", "user-a"),
            ("provider_login", "alice"),
            ("access_token", "gho_live_bearer_token"),
            ("linked_at", "2026-01-01T00:00:00Z"),
        ] {
            legacy.insert(k.to_string(), json!(v));
        }
        db::create(&ctx, TABLE, legacy)
            .await
            .expect("seed legacy row");
        assert_eq!(
            stored_access_token(&ctx, "gh-1").await,
            "gho_live_bearer_token"
        );

        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice",
            },
        )
        .await
        .unwrap();

        assert_eq!(stored_access_token(&ctx, "gh-1").await, "");
    }
}

#[cfg(test)]
mod tests_phase_4 {
    use super::*;
    use crate::test_support::TestContext;

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        ctx.seed_auth_user(user_id).await;
    }

    #[tokio::test]
    async fn list_for_user_returns_only_caller_links() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        for u in ["user-a", "user-b"] {
            seed_user(&ctx, u).await;
        }
        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice",
            },
        )
        .await
        .unwrap();
        upsert(
            &ctx,
            NewLink {
                provider: "google",
                provider_ref: "gg-1",
                user_id: "user-a",
                provider_login: "alice@example.com",
            },
        )
        .await
        .unwrap();
        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-2",
                user_id: "user-b",
                provider_login: "bob",
            },
        )
        .await
        .unwrap();

        let a = list_for_user(&ctx, "user-a").await.unwrap();
        let providers: Vec<&str> = a.iter().map(|l| l.provider.as_str()).collect();
        assert_eq!(providers.len(), 2);
        assert!(providers.contains(&"github"));
        assert!(providers.contains(&"google"));

        let b = list_for_user(&ctx, "user-b").await.unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].provider, "github");
        assert_eq!(b[0].provider_login, "bob");

        let c = list_for_user(&ctx, "user-c").await.unwrap();
        assert!(c.is_empty());
    }
}
