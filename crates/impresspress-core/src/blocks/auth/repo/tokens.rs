//! Refresh-token storage for `wafer-run/auth`.
//!
//! Stores SHA-256 hashes of refresh tokens (SEC-032: never the raw JWT)
//! along with family-rotation metadata (SEC-039):
//!
//! - `token_hash` — `sha256_hex(raw_refresh_jwt)`, the lookup key on refresh.
//! - `family` — stable across rotations, lets us detect reuse: if a request
//!   arrives with a refresh token whose row is `revoked = 1` but the family
//!   still has any live row, the attacker is using a stolen-and-rotated
//!   token. We revoke the whole family.
//! - `generation` — increments on each rotation. The first token in a family
//!   has generation 0. Audit aid; not load-bearing for the reuse check.
//! - `revoked` — set when a token rotates (don't delete, the row is needed
//!   for reuse detection) or when an entire family is invalidated.
//!
//! See `migrations/003_refresh_tokens.{sqlite,postgres}.sql` for the schema.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::context::Context;

use super::{now_iso, RepoError};
use crate::util::{sha256_hex, RecordExt};

pub const TABLE: &str = "wafer_run__auth__tokens";

/// SHA-256 hash of a raw refresh-token JWT, hex-encoded.
///
/// Same encoding used for API keys (see
/// `auth::authenticate_api_key`) so a future audit script can grep for
/// a single hash format across the auth surface.
pub fn hash(raw_token: &str) -> String {
    sha256_hex(raw_token.as_bytes())
}

/// Insert a fresh refresh-token row at `generation` (0 for the first token
/// in a new family, `prev_generation + 1` on rotation).
pub async fn insert(
    ctx: &dyn Context,
    user_id: &str,
    raw_token: &str,
    family: &str,
    generation: i64,
    expires_at: &str,
) -> Result<(), RepoError> {
    let id = uuid::Uuid::now_v7().to_string();
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("id".into(), json!(id));
    data.insert("token_hash".into(), json!(hash(raw_token)));
    data.insert("user_id".into(), json!(user_id));
    data.insert("family".into(), json!(family));
    data.insert("generation".into(), json!(generation));
    data.insert("revoked".into(), json!(false));
    // `now_iso()` is the single auth-table timestamp writer (`…Z` form);
    // keeps `created_at` consistent with every other auth repo module.
    data.insert("created_at".into(), json!(now_iso()));
    data.insert("expires_at".into(), json!(expires_at));

    db::create(ctx, TABLE, data)
        .await
        .map_err(|e| RepoError::Db(format!("tokens insert: {e}")))?;
    Ok(())
}

/// A loaded refresh-token row.
#[derive(Debug, Clone)]
pub struct TokenRow {
    pub id: String,
    pub user_id: String,
    pub family: String,
    pub generation: i64,
    pub revoked: bool,
}

/// Look up a refresh-token row by the SHA-256 hash of the raw token.
/// Returns `Ok(None)` if no row matches.
pub async fn find_by_token(
    ctx: &dyn Context,
    raw_token: &str,
) -> Result<Option<TokenRow>, RepoError> {
    let filters = vec![Filter {
        field: "token_hash".into(),
        operator: FilterOp::Equal,
        value: json!(hash(raw_token)),
    }];
    let records = db::list_all(ctx, TABLE, filters)
        .await
        .map_err(|e| RepoError::Db(format!("tokens lookup: {e}")))?;
    Ok(records.into_iter().next().map(row_from_record))
}

/// True iff any non-revoked row exists in the given family.
///
/// Used by the refresh handler's reuse-detection branch: if a refresh
/// request hits a revoked row but `family_has_live_row` is true, the row
/// being presented was already rotated and the family is under attack —
/// revoke the whole family.
pub async fn family_has_live_row(ctx: &dyn Context, family: &str) -> Result<bool, RepoError> {
    let filters = vec![
        Filter {
            field: "family".into(),
            operator: FilterOp::Equal,
            value: json!(family),
        },
        Filter {
            field: "revoked".into(),
            operator: FilterOp::Equal,
            value: json!(false),
        },
    ];
    let records = db::list_all(ctx, TABLE, filters)
        .await
        .map_err(|e| RepoError::Db(format!("tokens family lookup: {e}")))?;
    Ok(!records.is_empty())
}

/// Mark a single row as revoked.
pub async fn revoke_by_id(ctx: &dyn Context, id: &str) -> Result<(), RepoError> {
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("revoked".into(), json!(true));
    db::update(ctx, TABLE, id, data)
        .await
        .map_err(|e| RepoError::Db(format!("tokens revoke_by_id: {e}")))?;
    Ok(())
}

/// Mark every row in `family` as revoked. Used both for normal logout-style
/// invalidation and for reuse-attack detection.
pub async fn revoke_family(ctx: &dyn Context, family: &str) -> Result<(), RepoError> {
    let filters = vec![Filter {
        field: "family".into(),
        operator: FilterOp::Equal,
        value: json!(family),
    }];
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("revoked".into(), json!(true));
    db::update_by_filters(ctx, TABLE, filters, data)
        .await
        .map_err(|e| RepoError::Db(format!("tokens revoke_family: {e}")))?;
    Ok(())
}

/// Mark every row owned by `user_id` as revoked. Used by logout,
/// password-reset, and password-change flows to invalidate sessions
/// across all the user's devices.
pub async fn revoke_all_for_user(ctx: &dyn Context, user_id: &str) -> Result<(), RepoError> {
    let filters = vec![Filter {
        field: "user_id".into(),
        operator: FilterOp::Equal,
        value: json!(user_id),
    }];
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("revoked".into(), json!(true));
    db::update_by_filters(ctx, TABLE, filters, data)
        .await
        .map_err(|e| RepoError::Db(format!("tokens revoke_all_for_user: {e}")))?;
    Ok(())
}

fn row_from_record(record: db::Record) -> TokenRow {
    let family = record.str_field("family").to_string();
    let user_id = record.str_field("user_id").to_string();
    let generation = record
        .data
        .get("generation")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let revoked = record.bool_field("revoked");
    TokenRow {
        id: record.id,
        user_id,
        family,
        generation,
        revoked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestContext;

    fn future_iso(secs: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::seconds(secs))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    async fn seed_user(ctx: &TestContext, id: &str, email: &str) {
        use crate::blocks::auth::USERS_TABLE;
        let mut data: HashMap<String, Value> = HashMap::new();
        data.insert("id".into(), json!(id));
        data.insert("email".into(), json!(email));
        data.insert("display_name".into(), json!(email));
        data.insert("role".into(), json!("user"));
        data.insert("email_verified".into(), json!(true));
        data.insert("created_at".into(), json!(crate::util::now_rfc3339()));
        data.insert("updated_at".into(), json!(crate::util::now_rfc3339()));
        db::create(ctx, USERS_TABLE, data).await.unwrap();
    }

    #[tokio::test]
    async fn insert_then_find_by_token_round_trips() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-1", "u1@example.com").await;
        insert(&ctx, "user-1", "raw-jwt", "fam-1", 0, &future_iso(3600))
            .await
            .unwrap();

        let row = find_by_token(&ctx, "raw-jwt").await.unwrap();
        let row = row.expect("row should exist");
        assert_eq!(row.user_id, "user-1");
        assert_eq!(row.family, "fam-1");
        assert_eq!(row.generation, 0);
        assert!(!row.revoked);
    }

    #[tokio::test]
    async fn raw_token_is_never_stored() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-1", "u1@example.com").await;
        let raw = "secret-refresh-token-do-not-store";
        insert(&ctx, "user-1", raw, "fam-1", 0, &future_iso(3600))
            .await
            .unwrap();

        // No row should contain the raw token. Verify by scanning every row.
        let records = db::list_all(&ctx, TABLE, vec![]).await.unwrap();
        assert_eq!(records.len(), 1, "exactly one row was inserted");
        let serialized = serde_json::to_string(&records[0].data).unwrap();
        assert!(
            !serialized.contains(raw),
            "raw token must not appear in any column; row = {serialized}"
        );
        // And the stored hash must match.
        assert_eq!(records[0].str_field("token_hash"), hash(raw));
    }

    #[tokio::test]
    async fn rotate_marks_old_revoked_and_keeps_family() {
        // Simulate the refresh handler's rotation: insert v0, then revoke
        // v0 + insert v1 under the same family with generation+1.
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-1", "u1@example.com").await;
        insert(&ctx, "user-1", "tok-v0", "fam-1", 0, &future_iso(3600))
            .await
            .unwrap();
        let old = find_by_token(&ctx, "tok-v0").await.unwrap().unwrap();
        revoke_by_id(&ctx, &old.id).await.unwrap();
        insert(&ctx, "user-1", "tok-v1", "fam-1", 1, &future_iso(3600))
            .await
            .unwrap();

        let old = find_by_token(&ctx, "tok-v0").await.unwrap().unwrap();
        assert!(old.revoked, "old row stays as a revoked tombstone");
        assert_eq!(old.family, "fam-1");
        assert_eq!(old.generation, 0);

        let new = find_by_token(&ctx, "tok-v1").await.unwrap().unwrap();
        assert!(!new.revoked);
        assert_eq!(new.family, "fam-1");
        assert_eq!(new.generation, 1);

        assert!(family_has_live_row(&ctx, "fam-1").await.unwrap());
    }

    #[tokio::test]
    async fn reuse_detection_revokes_whole_family() {
        // After rotation, presenting the OLD (revoked) token should reveal
        // a live family — the handler's response is to revoke the family.
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-1", "u1@example.com").await;
        insert(&ctx, "user-1", "tok-v0", "fam-1", 0, &future_iso(3600))
            .await
            .unwrap();
        let old = find_by_token(&ctx, "tok-v0").await.unwrap().unwrap();
        revoke_by_id(&ctx, &old.id).await.unwrap();
        insert(&ctx, "user-1", "tok-v1", "fam-1", 1, &future_iso(3600))
            .await
            .unwrap();

        // Reuse-attack signal: old token revoked, family still live.
        let old = find_by_token(&ctx, "tok-v0").await.unwrap().unwrap();
        assert!(old.revoked);
        assert!(family_has_live_row(&ctx, "fam-1").await.unwrap());

        revoke_family(&ctx, "fam-1").await.unwrap();

        let new = find_by_token(&ctx, "tok-v1").await.unwrap().unwrap();
        assert!(new.revoked, "rotation target should now be revoked too");
        assert!(!family_has_live_row(&ctx, "fam-1").await.unwrap());
    }

    #[tokio::test]
    async fn revoke_all_for_user_invalidates_every_family() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-1", "u1@example.com").await;
        insert(&ctx, "user-1", "tok-a", "fam-a", 0, &future_iso(3600))
            .await
            .unwrap();
        insert(&ctx, "user-1", "tok-b", "fam-b", 0, &future_iso(3600))
            .await
            .unwrap();

        revoke_all_for_user(&ctx, "user-1").await.unwrap();

        assert!(!family_has_live_row(&ctx, "fam-a").await.unwrap());
        assert!(!family_has_live_row(&ctx, "fam-b").await.unwrap());
    }
}
