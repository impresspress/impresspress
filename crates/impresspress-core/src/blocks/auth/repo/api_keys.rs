//! Row-level access over `wafer_run__auth__api_keys`.
//!
//! API keys authenticate programmatic callers via an `Authorization: Bearer
//! sb_…` header. The raw key is shown to the user exactly once; only its
//! deterministic SHA-256 hex (`key_hash`) is persisted, so the lookup on every
//! request is by hash. Consumed by `auth_ui/api/api_keys.rs` (the CRUD
//! endpoints) and `auth::authenticate_api_key` (the pipeline preprocessor).

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::{db_failed, internal_error, iso, map_opt_str, map_str, now_iso, parse_iso};
use crate::db_read::{self, Bound};

pub const TABLE: &str = "wafer_run__auth__api_keys";

/// A loaded API-key row. `key_hash` is included so the pipeline can compare it,
/// but the CRUD endpoints strip it before serialising to clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub key_prefix: String,
    pub key_hash: String,
    pub created_at: String,
    /// Absolute expiry as [`super::iso`] writes it, or `None` for
    /// non-expiring keys. Rows written before that was the only writer can
    /// hold any string at all — [`ApiKeyRow::is_expired`] is what reads it.
    pub expires_at: Option<String>,
    /// Set when the key was revoked; `None` while active.
    pub revoked_at: Option<String>,
}

impl ApiKeyRow {
    /// True iff the key has been revoked.
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.as_deref().is_some_and(|s| !s.is_empty())
    }

    /// True iff the key carries an expiry that `now` has reached.
    ///
    /// The stored text is parsed rather than string-compared. String order is
    /// time order only within one format and one offset, and this column is
    /// the one auth column that ever held a caller's own string:
    /// `…T20:00:00+09:00` is 11:00 UTC but sorts after `…T12:00:00Z`, so a
    /// key an hour dead read as live.
    ///
    /// An expiry that does not parse counts as expired. A key whose end date
    /// cannot be read is a key with no enforceable end, and `"never"` —
    /// which sorts after every timestamp — is exactly how one got minted.
    ///
    /// The comparison is `>=`, not `>`: a key expires AT the instant it
    /// names, not one tick after it. The text comparison this replaced was
    /// `>`, so a key stayed valid through its own expiry second.
    pub fn is_expired(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        match self.expires_at.as_deref().filter(|exp| !exp.is_empty()) {
            Some(exp) => parse_iso(exp).is_none_or(|exp| now >= exp),
            None => false,
        }
    }
}

/// Insert payload for [`insert`]. Borrowed fields — the caller keeps ownership.
#[derive(Debug, Clone, Copy)]
pub struct NewApiKey<'a> {
    pub user_id: &'a str,
    pub name: &'a str,
    pub key_hash: &'a str,
    pub key_prefix: &'a str,
    /// Optional absolute expiry, as an instant. An instant rather than a
    /// string so no caller can hand this table a timestamp it cannot read
    /// back: [`insert`] is the only thing that formats it, with
    /// [`super::iso`].
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<ApiKeyRow, WaferError> {
    Ok(ApiKeyRow {
        id: map_opt_str(m, "id").ok_or_else(|| internal_error("missing id"))?,
        user_id: map_str(m, "user_id"),
        name: map_str(m, "name"),
        key_prefix: map_str(m, "key_prefix"),
        key_hash: map_str(m, "key_hash"),
        created_at: map_str(m, "created_at"),
        expires_at: map_opt_str(m, "expires_at"),
        revoked_at: map_opt_str(m, "revoked_at"),
    })
}

/// Insert a new API-key row and return it.
pub async fn insert(ctx: &dyn Context, new: NewApiKey<'_>) -> Result<ApiKeyRow, WaferError> {
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("user_id".into(), json!(new.user_id));
    data.insert("name".into(), json!(new.name));
    data.insert("key_hash".into(), json!(new.key_hash));
    data.insert("key_prefix".into(), json!(new.key_prefix));
    data.insert("created_at".into(), json!(now_iso()));
    if let Some(exp) = new.expires_at {
        data.insert("expires_at".into(), json!(iso(exp)));
    }
    let rec = db::create(ctx, TABLE, data)
        .await
        .map_err(|e| db_failed("api_keys insert", e))?;
    row_from_map(&rec.data)
}

/// Look up an API key by its `key_hash` (the SHA-256 hex of the raw key).
/// Returns `Ok(None)` when no row matches.
pub async fn find_by_key_hash(
    ctx: &dyn Context,
    key_hash: &str,
) -> Result<Option<ApiKeyRow>, WaferError> {
    use wafer_block::ErrorCode;
    match db::get_by_field(ctx, TABLE, "key_hash", json!(key_hash)).await {
        Ok(rec) => Ok(Some(row_from_map(&rec.data)?)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(db_failed("api_keys find_by_key_hash", e)),
    }
}

/// Look up an API key by its primary `id`. Returns `Ok(None)` when missing.
pub async fn find_by_id(ctx: &dyn Context, id: &str) -> Result<Option<ApiKeyRow>, WaferError> {
    use wafer_block::ErrorCode;
    match db::get(ctx, TABLE, id).await {
        Ok(rec) => Ok(Some(row_from_map(&rec.data)?)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(db_failed("api_keys find_by_id", e)),
    }
}

/// List a user's API keys, newest first (most recent `created_at` at the top).
/// `key_hash` is populated on the rows; callers serialising to clients must
/// not leak it.
pub async fn list_for_user(ctx: &dyn Context, user_id: &str) -> Result<Vec<ApiKeyRow>, WaferError> {
    let records = db_read::list_bounded_sorted(
        ctx,
        TABLE,
        vec![Filter {
            field: "user_id".into(),
            operator: FilterOp::Equal,
            value: json!(user_id),
        }],
        vec![SortField {
            field: "created_at".into(),
            desc: true,
        }],
        Bound::OnePer("API key one user issued"),
    )
    .await
    .map_err(|e| db_failed("api_keys list_for_user", e))?;
    records.iter().map(|r| row_from_map(&r.data)).collect()
}

/// List the `limit` most recently created API keys across every user,
/// newest first — the admin IAM page's API-keys tab, which is a
/// deployment-wide view rather than one account's. `key_hash` is populated
/// on the rows; the tab renders `key_prefix` only.
pub async fn list_recent(ctx: &dyn Context, limit: u32) -> Result<Vec<ApiKeyRow>, WaferError> {
    let list = db::list(
        ctx,
        TABLE,
        &ListOptions {
            sort: vec![SortField {
                field: "created_at".into(),
                desc: true,
            }],
            limit: Some(limit),
            skip_count: true,
            ..Default::default()
        },
    )
    .await
    .map_err(|e| db_failed("api_keys list_recent", e))?;
    list.records.iter().map(|r| row_from_map(&r.data)).collect()
}

/// Mark an API key revoked (stamps `revoked_at` with [`super::now_iso`]).
pub async fn revoke(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("revoked_at".into(), json!(now_iso()));
    db::update(ctx, TABLE, id, data)
        .await
        .map_err(|e| db_failed("api_keys revoke", e))?;
    Ok(())
}

/// Hard-delete an API-key row by id.
pub async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id)
        .await
        .map_err(|e| db_failed("api_keys delete", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestContext;

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        ctx.seed_auth_user(user_id).await;
    }

    #[tokio::test]
    async fn insert_then_find_by_key_hash_and_id() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        let row = insert(
            &ctx,
            NewApiKey {
                user_id: "user-a",
                name: "ci",
                key_hash: "deadbeef",
                key_prefix: "sb_deadbe",
                expires_at: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(row.user_id, "user-a");
        assert!(!row.is_revoked());

        let by_hash = find_by_key_hash(&ctx, "deadbeef").await.unwrap().unwrap();
        assert_eq!(by_hash.id, row.id);
        let by_id = find_by_id(&ctx, &row.id).await.unwrap().unwrap();
        assert_eq!(by_id.key_hash, "deadbeef");
        assert!(find_by_key_hash(&ctx, "nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_revoke_and_delete() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        let a = insert(
            &ctx,
            NewApiKey {
                user_id: "user-a",
                name: "a",
                key_hash: "h-a",
                key_prefix: "sb_a",
                expires_at: None,
            },
        )
        .await
        .unwrap();
        insert(
            &ctx,
            NewApiKey {
                user_id: "user-a",
                name: "b",
                key_hash: "h-b",
                key_prefix: "sb_b",
                expires_at: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(list_for_user(&ctx, "user-a").await.unwrap().len(), 2);

        revoke(&ctx, &a.id).await.unwrap();
        let a_after = find_by_id(&ctx, &a.id).await.unwrap().unwrap();
        assert!(a_after.is_revoked());

        delete(&ctx, &a.id).await.unwrap();
        assert!(find_by_id(&ctx, &a.id).await.unwrap().is_none());
        assert_eq!(list_for_user(&ctx, "user-a").await.unwrap().len(), 1);
    }

    fn at(ts: &str) -> chrono::DateTime<chrono::Utc> {
        super::parse_iso(ts).expect("test timestamp")
    }

    fn row_expiring(expires_at: Option<&str>) -> ApiKeyRow {
        ApiKeyRow {
            id: "1".into(),
            user_id: "u".into(),
            name: "n".into(),
            key_prefix: "p".into(),
            key_hash: "h".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            expires_at: expires_at.map(str::to_owned),
            revoked_at: None,
        }
    }

    #[test]
    fn is_expired_reads_the_instant_the_expiry_names() {
        // No expiry, and the two shapes of "unset", never expire.
        assert!(!row_expiring(None).is_expired(at("2030-01-01T00:00:00Z")));
        assert!(!row_expiring(Some("")).is_expired(at("2030-01-01T00:00:00Z")));

        let row = row_expiring(Some("2026-06-01T00:00:00Z"));
        assert!(row.is_expired(at("2026-06-02T00:00:00Z")));
        assert!(!row.is_expired(at("2026-05-31T00:00:00Z")));

        // A stored offset names an instant, and 20:00+09:00 is 11:00Z — a
        // string compare put it an hour into the future instead.
        let offset = row_expiring(Some("2026-06-01T20:00:00+09:00"));
        assert!(offset.is_expired(at("2026-06-01T12:00:00Z")));
        assert!(!offset.is_expired(at("2026-06-01T10:00:00Z")));
    }

    #[test]
    fn an_unreadable_expiry_is_expired() {
        // `"never"` sorts after every timestamp, so a string compare made it
        // the one expiry that never arrived.
        for stored in ["never", "2026-06-01", "soon", "0"] {
            assert!(
                row_expiring(Some(stored)).is_expired(at("2026-06-02T00:00:00Z")),
                "{stored} is not a readable expiry, so the key has no enforceable end"
            );
        }
    }

    #[tokio::test]
    async fn insert_stores_an_expiry_in_the_one_format_the_column_holds() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        let row = insert(
            &ctx,
            NewApiKey {
                user_id: "user-a",
                name: "ci",
                key_hash: "h-exp",
                key_prefix: "sb_exp",
                expires_at: Some(at("2026-06-01T20:00:00+09:00")),
            },
        )
        .await
        .unwrap();
        assert_eq!(row.expires_at.as_deref(), Some("2026-06-01T11:00:00Z"));
    }
}
