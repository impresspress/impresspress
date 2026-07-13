//! Row-level access over `wafer_run__auth__provider_links`.
//!
//! One row per `(provider, provider_ref)` pair. The primary key matches the
//! spec §3 definition; `upsert` relies on `ON CONFLICT` against that PK so
//! an OAuth login by the same user from the same provider deterministically
//! refreshes `access_token`, `provider_login`, `user_id`, and `linked_at`.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::database as db;
use wafer_run::context::Context;

use super::{map_opt_str, map_str, now_iso, RepoError};

pub const TABLE: &str = "wafer_run__auth__provider_links";

/// Full row shape returned by [`find_by_provider_ref`]. `linked_at` is
/// included so higher layers can surface "last login via …" strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLink {
    pub provider: String,
    pub provider_ref: String,
    pub user_id: String,
    pub provider_login: String,
    pub access_token: String,
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
    pub access_token: &'a str,
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<ProviderLink, RepoError> {
    Ok(ProviderLink {
        provider: map_opt_str(m, "provider")
            .ok_or_else(|| RepoError::Db("missing provider".into()))?,
        provider_ref: map_opt_str(m, "provider_ref")
            .ok_or_else(|| RepoError::Db("missing provider_ref".into()))?,
        user_id: map_opt_str(m, "user_id")
            .ok_or_else(|| RepoError::Db("missing user_id".into()))?,
        provider_login: map_str(m, "provider_login"),
        access_token: map_str(m, "access_token"),
        linked_at: map_str(m, "linked_at"),
    })
}

/// Insert a link row, or update `user_id`, `provider_login`, `access_token`,
/// `linked_at` in place when a row with the same `(provider, provider_ref)`
/// already exists. Manual two-step (list → update_by_filters or create) since
/// `db::*` has no two-key upsert primitive.
pub async fn upsert(ctx: &dyn Context, new: NewLink<'_>) -> Result<(), RepoError> {
    let now = now_iso();
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
    let existing = db::list_all(ctx, TABLE, filters.clone())
        .await
        .map_err(|e| RepoError::Db(format!("provider_links upsert lookup: {e}")))?;

    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("user_id".into(), json!(new.user_id));
    data.insert("provider_login".into(), json!(new.provider_login));
    data.insert("access_token".into(), json!(new.access_token));
    data.insert("linked_at".into(), json!(now));

    if existing.is_empty() {
        // Insert: include the natural-key columns + a fresh synthetic id.
        let id = uuid::Uuid::now_v7().to_string();
        data.insert("id".into(), json!(id));
        data.insert("provider".into(), json!(new.provider));
        data.insert("provider_ref".into(), json!(new.provider_ref));
        db::create(ctx, TABLE, data)
            .await
            .map_err(|e| RepoError::Db(format!("provider_links insert: {e}")))?;
    } else {
        // Update by the same (provider, provider_ref) filters — no synthetic id needed.
        db::update_by_filters(ctx, TABLE, filters, data)
            .await
            .map_err(|e| RepoError::Db(format!("provider_links update: {e}")))?;
    }
    Ok(())
}

/// Look up a link by `(provider, provider_ref)`. Returns `Ok(None)` if no
/// matching row exists.
pub async fn find_by_provider_ref(
    ctx: &dyn Context,
    provider: &str,
    provider_ref: &str,
) -> Result<Option<ProviderLink>, RepoError> {
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
    let records = db::list_all(ctx, TABLE, filters)
        .await
        .map_err(|e| RepoError::Db(format!("provider_links find: {e}")))?;
    match records.first() {
        Some(r) => Ok(Some(row_from_map(&r.data)?)),
        None => Ok(None),
    }
}

/// Return all OAuth provider links owned by `user_id`, ordered by
/// `linked_at` ASC for stable rendering on the security page.
///
/// Uses the typed `db::list` client (not raw SQL) because this is the
/// only entry point in this module called cross-block — userportal's
/// `/b/userportal/security` page reads it. Raw SQL is admin-only under
/// WRAP, so the cross-block call must go through the namespaced path.
/// The auth-internal lookup (`find_by_provider_ref`) remains on raw SQL
/// because it's called only by auth itself (the resource owner —
/// own-resource access is always permitted, regardless of typed-vs-raw).
pub async fn list_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Vec<ProviderLink>, RepoError> {
    let records = db::list_sorted(
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
    )
    .await
    .map_err(|e| RepoError::Db(format!("provider_links list_for_user: {e}")))?;
    records.iter().map(|r| row_from_map(&r.data)).collect()
}

#[cfg(test)]
mod typed_client_tests {
    use super::*;
    use crate::test_support::TestContext;

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        wafer_core::clients::database::exec_raw(
            ctx,
            "INSERT INTO wafer_run__auth__users \
             (id, email, display_name, role, created_at, updated_at) \
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

    #[tokio::test]
    async fn upsert_inserts_then_updates_under_wrap() {
        // Seed BEFORE enabling WRAP — exec_raw fixture denied otherwise.
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        let ctx = ctx.with_wrap("wafer-run/auth", vec![], "impresspress/admin");

        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice",
                access_token: "tok-old",
            },
        )
        .await
        .unwrap();
        // Re-upsert with new access_token / login — should update in place,
        // not insert a duplicate.
        upsert(
            &ctx,
            NewLink {
                provider: "github",
                provider_ref: "gh-1",
                user_id: "user-a",
                provider_login: "alice2",
                access_token: "tok-new",
            },
        )
        .await
        .unwrap();
        let got = find_by_provider_ref(&ctx, "github", "gh-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.access_token, "tok-new");
        assert_eq!(got.provider_login, "alice2");
    }
}

#[cfg(test)]
mod tests_phase_4 {
    use super::*;
    use crate::test_support::TestContext;

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        wafer_core::clients::database::exec_raw(
            ctx,
            "INSERT INTO wafer_run__auth__users (id, email, display_name, role, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
            &[
                serde_json::json!(user_id),
                serde_json::json!(format!("{user_id}@example.com")),
                serde_json::json!(user_id),
                serde_json::json!("user"),
                serde_json::json!("2026-01-01T00:00:00Z"),
                serde_json::json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn list_for_user_returns_only_caller_links() {
        let ctx = TestContext::with_auth().await;
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
                access_token: "tok-a",
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
                access_token: "tok-b",
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
                access_token: "tok-c",
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
