//! Row-level access over `wafer_run__auth__sessions`.
//!
//! `token_hash` is the sha256 of the raw token (a JWT for the live login flow,
//! a `wafer_session_*` opaque token for the planned cookie flow). The raw
//! token never lives past its issuance helper.
//!
//! ## Schema convention
//!
//! This module uses the **live TEXT-everything convention** — rows are
//! materialised through `db::create` (which calls `ensure_table` on first
//! insert) rather than the BLOB-keyed Plan A2 migration schema. The migration
//! file `001_auth_schema.sqlite.sql` still defines a stricter schema for tests
//! that opt into `with_auth()`, but production startup never applies migrations
//! (the auth block's `Init` lifecycle only seeds the admin user, not the
//! schema). Storing `token_hash` as a hex string in a TEXT column keeps the
//! repo working in both cases:
//!
//! - In production, `ensure_table` materialises a TEXT-everything table on
//!   first insert (mirroring how `users`, `user_roles`, and `tokens` already
//!   work).
//! - In tests using `with_auth()`, the migration's BLOB-keyed table already
//!   exists; SQLite's loose typing accepts the hex string in the BLOB column,
//!   and `ensure_table` adds the auto `id` column via `ALTER TABLE`.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::database as db;
use wafer_run::context::Context;

use super::{decode_hex, map_str, now_iso, RepoError};
use crate::util::hex_encode;

pub const TABLE: &str = "wafer_run__auth__sessions";

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub token_hash: Vec<u8>,
    pub user_id: String,
    pub created_at: String,
    pub last_used_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone)]
pub struct NewSession {
    pub token_hash: Vec<u8>,
    pub user_id: String,
    pub expires_at: String,
}

/// Decode a `token_hash` column value, accepting either:
/// - A TEXT hex string (live convention used by production rows + new inserts).
/// - A `Value::Array` of `u8`-coerced numbers (legacy migration BLOB rows
///   round-tripped via `json!(&[u8])`).
/// - A non-hex `Value::String` (legacy raw-bytes encoding).
fn decode_token_hash(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::String(s) => decode_hex(s).or_else(|| Some(s.as_bytes().to_vec())),
        Value::Array(arr) => Some(
            arr.iter()
                .filter_map(|x| x.as_u64().map(|n| n as u8))
                .collect(),
        ),
        _ => None,
    }
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<SessionRow, RepoError> {
    let token_hash = m
        .get("token_hash")
        .and_then(decode_token_hash)
        .ok_or_else(|| RepoError::Db("missing token_hash".into()))?;
    Ok(SessionRow {
        token_hash,
        user_id: super::map_opt_str(m, "user_id")
            .ok_or_else(|| RepoError::Db("missing user_id".into()))?,
        created_at: map_str(m, "created_at"),
        last_used_at: map_str(m, "last_used_at"),
        expires_at: map_str(m, "expires_at"),
    })
}

/// Build the column map written for both new inserts and update reads.
fn row_to_data(new: &NewSession, now: &str) -> HashMap<String, Value> {
    let mut data = HashMap::new();
    data.insert("token_hash".into(), json!(hex_encode(&new.token_hash)));
    data.insert("user_id".into(), json!(new.user_id));
    data.insert("created_at".into(), json!(now));
    data.insert("last_used_at".into(), json!(now));
    data.insert("expires_at".into(), json!(new.expires_at));
    data
}

/// Insert a new session row.
///
/// Each call creates a new row — sessions are not idempotent, every login or
/// session-issuance call gets a fresh row keyed by its own `token_hash`.
pub async fn insert(ctx: &dyn Context, new: NewSession) -> Result<(), RepoError> {
    let now = now_iso();
    let data = row_to_data(&new, &now);
    db::create(ctx, TABLE, data)
        .await
        .map_err(|e| RepoError::Db(format!("insert session: {e}")))?;
    Ok(())
}

/// Convenience wrapper for the JWT login path: hashes nothing (the caller
/// already computed `token_hash`), defaults `expires_at` to the access-token
/// lifetime in days, and forwards to [`insert`].
///
/// Naming mirrors `delete_for_user`/`list_for_user` for symmetry.
pub async fn create_for_user(
    ctx: &dyn Context,
    user_id: &str,
    token_hash: Vec<u8>,
    lifetime_days: u32,
) -> Result<(), RepoError> {
    let expires_at = (chrono::Utc::now() + chrono::Duration::days(lifetime_days as i64))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    insert(
        ctx,
        NewSession {
            token_hash,
            user_id: user_id.to_string(),
            expires_at,
        },
    )
    .await
}

async fn find_record_by_hash(
    ctx: &dyn Context,
    hash: &[u8],
) -> Result<Option<wafer_core::clients::database::Record>, RepoError> {
    use wafer_block::ErrorCode;
    match db::get_by_field(ctx, TABLE, "token_hash", json!(hex_encode(hash))).await {
        Ok(rec) => Ok(Some(rec)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(RepoError::Db(format!("session lookup: {e}"))),
    }
}

pub async fn find_by_token_hash(
    ctx: &dyn Context,
    hash: &[u8],
) -> Result<Option<SessionRow>, RepoError> {
    match find_record_by_hash(ctx, hash).await? {
        Some(r) => Ok(Some(row_from_map(&r.data)?)),
        None => Ok(None),
    }
}

/// Bumps `last_used_at` to the current time for the row identified by
/// `hash`. Silently no-ops if the row is missing — callers treat liveness
/// as a best-effort signal.
pub async fn touch_last_used(ctx: &dyn Context, hash: &[u8]) -> Result<(), RepoError> {
    let Some(record) = find_record_by_hash(ctx, hash).await? else {
        return Ok(());
    };
    let mut data = HashMap::new();
    data.insert("last_used_at".into(), json!(now_iso()));
    db::update(ctx, TABLE, &record.id, data)
        .await
        .map_err(|e| RepoError::Db(format!("session touch: {e}")))?;
    Ok(())
}

/// Deletes the session row identified by `token_hash`. Returns the number of
/// affected rows (0 if no such row exists).
pub async fn delete_by_token_hash(ctx: &dyn Context, hash: &[u8]) -> Result<u64, RepoError> {
    let Some(record) = find_record_by_hash(ctx, hash).await? else {
        return Ok(0);
    };
    db::delete(ctx, TABLE, &record.id)
        .await
        .map_err(|e| RepoError::Db(format!("session delete: {e}")))?;
    Ok(1)
}

/// Deletes rows whose `expires_at < cutoff`. Returns the number deleted.
///
/// `cutoff` is compared as an ISO-8601 string; rows store timestamps in the
/// same text format (see [`now_iso`]).
pub async fn delete_expired(ctx: &dyn Context, cutoff: &str) -> Result<u64, RepoError> {
    let filters = vec![Filter {
        field: "expires_at".into(),
        operator: FilterOp::LessThan,
        value: json!(cutoff),
    }];
    let records = db::list_all(ctx, TABLE, filters)
        .await
        .map_err(|e| RepoError::Db(format!("delete expired list: {e}")))?;
    let mut deleted = 0u64;
    for record in records {
        if db::delete(ctx, TABLE, &record.id).await.is_ok() {
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// Return all active sessions for `user_id`, ordered by `last_used_at` DESC
/// so the most recently active session sorts first.
pub async fn list_for_user(ctx: &dyn Context, user_id: &str) -> Result<Vec<SessionRow>, RepoError> {
    let records = db::list_sorted(
        ctx,
        TABLE,
        vec![Filter {
            field: "user_id".into(),
            operator: FilterOp::Equal,
            value: json!(user_id),
        }],
        vec![SortField {
            field: "last_used_at".into(),
            desc: true,
        }],
    )
    .await
    .map_err(|e| RepoError::Db(format!("session list_for_user: {e}")))?;
    records.iter().map(|r| row_from_map(&r.data)).collect()
}

/// Delete a session row, but only if it belongs to `user_id`. Returns 0 if
/// the row doesn't exist OR belongs to a different user — never reveals
/// which.
pub async fn delete_for_user(
    ctx: &dyn Context,
    user_id: &str,
    hash: &[u8],
) -> Result<u64, RepoError> {
    let Some(record) = find_record_by_hash(ctx, hash).await? else {
        return Ok(0);
    };
    let owner = record
        .data
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if owner != user_id {
        return Ok(0);
    }
    db::delete(ctx, TABLE, &record.id)
        .await
        .map_err(|e| RepoError::Db(format!("session delete_for_user: {e}")))?;
    Ok(1)
}

#[cfg(test)]
mod tests_phase_4 {
    use super::*;
    use crate::test_support::TestContext;

    fn fake_session(user_id: &str, hash_byte: u8) -> NewSession {
        NewSession {
            token_hash: vec![hash_byte; 32],
            user_id: user_id.into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
        }
    }

    /// Seed a user row directly via SQL so the test can pin a deterministic
    /// `user_id` — `db::create` would generate one. The row provides every
    /// NOT NULL column required by the auth migration.
    async fn seed_user(ctx: &TestContext, user_id: &str) {
        db::exec_raw(
            ctx,
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

    #[tokio::test]
    async fn list_for_user_returns_only_caller_sessions() {
        let ctx = TestContext::with_auth().await;
        for user_id in ["user-a", "user-b"] {
            seed_user(&ctx, user_id).await;
        }
        insert(&ctx, fake_session("user-a", 0x01)).await.unwrap();
        insert(&ctx, fake_session("user-a", 0x02)).await.unwrap();
        insert(&ctx, fake_session("user-b", 0x03)).await.unwrap();

        let a = list_for_user(&ctx, "user-a").await.unwrap();
        let b = list_for_user(&ctx, "user-b").await.unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 1);
    }

    #[tokio::test]
    async fn delete_for_user_refuses_other_users_session() {
        let ctx = TestContext::with_auth().await;
        for user_id in ["user-a", "user-b"] {
            seed_user(&ctx, user_id).await;
        }
        insert(&ctx, fake_session("user-a", 0x01)).await.unwrap();
        insert(&ctx, fake_session("user-b", 0x02)).await.unwrap();

        // user-a tries to revoke user-b's session — should affect 0 rows.
        let affected = delete_for_user(&ctx, "user-a", &[0x02; 32]).await.unwrap();
        assert_eq!(affected, 0);
        // user-b's session is still there.
        assert_eq!(list_for_user(&ctx, "user-b").await.unwrap().len(), 1);

        // user-a can revoke their own.
        let affected = delete_for_user(&ctx, "user-a", &[0x01; 32]).await.unwrap();
        assert_eq!(affected, 1);
        assert_eq!(list_for_user(&ctx, "user-a").await.unwrap().len(), 0);
    }

    /// `find_by_token_hash` round-trips a freshly-inserted row, and the row's
    /// `token_hash` is the same bytes that went in (proves the hex
    /// encode/decode is symmetric).
    #[tokio::test]
    async fn find_by_token_hash_roundtrip() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        let raw_hash = vec![0xaa; 32];
        insert(
            &ctx,
            NewSession {
                token_hash: raw_hash.clone(),
                user_id: "user-a".into(),
                expires_at: "2099-01-01T00:00:00Z".into(),
            },
        )
        .await
        .unwrap();
        let found = find_by_token_hash(&ctx, &raw_hash).await.unwrap();
        let row = found.expect("row missing after insert");
        assert_eq!(row.token_hash, raw_hash);
        assert_eq!(row.user_id, "user-a");
    }

    /// `delete_by_token_hash` returns 1 on hit, 0 on miss.
    #[tokio::test]
    async fn delete_by_token_hash_counts_rows() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", 0x01)).await.unwrap();
        assert_eq!(delete_by_token_hash(&ctx, &[0x99; 32]).await.unwrap(), 0);
        assert_eq!(delete_by_token_hash(&ctx, &[0x01; 32]).await.unwrap(), 1);
        assert_eq!(list_for_user(&ctx, "user-a").await.unwrap().len(), 0);
    }

    /// `create_for_user` writes a row that `list_for_user` can find, and the
    /// `expires_at` is in the future relative to `now_iso()`.
    #[tokio::test]
    async fn create_for_user_writes_visible_row() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        create_for_user(&ctx, "user-a", vec![0x42; 32], 1)
            .await
            .unwrap();
        let rows = list_for_user(&ctx, "user-a").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].token_hash, vec![0x42; 32]);
        // `expires_at` is one day in the future — strictly greater than now.
        assert!(rows[0].expires_at.as_str() > now_iso().as_str());
    }
}
