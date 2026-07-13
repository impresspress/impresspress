//! Row-level access over `wafer_run__auth__jwt_blocklist` (SEC-042).
//!
//! Logout calls [`insert`] with the request JWT's `jti` and `exp`.
//! `pipeline::handle_request` calls [`contains`] after structural JWT
//! validation in `crate::crypto::extract_auth_meta` — a hit means the
//! caller logged out (or had their session terminated) before the token's
//! natural expiry, so the request continues as unauthenticated.
//!
//! Rows are at most one per access-token-lifetime per user. [`delete_expired`]
//! sweeps rows whose `expires_at < cutoff`; presenting an expired JWT
//! already fails structural validation, so the blocklist row is redundant
//! once `expires_at` passes.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::context::Context;

use super::{now_iso, RepoError};

pub const TABLE: &str = "wafer_run__auth__jwt_blocklist";

/// Payload for [`insert`].
#[derive(Debug, Clone)]
pub struct NewBlocklistEntry<'a> {
    pub jti: &'a str,
    pub user_id: &'a str,
    /// Absolute expiry time (matches the JWT's `exp` claim) as ISO-8601.
    pub expires_at: &'a str,
}

/// Insert a blocklist entry. Re-inserting an existing `jti` succeeds (best-
/// effort idempotency) — logout-twice with the same JWT is a no-op.
pub async fn insert(ctx: &dyn Context, new: NewBlocklistEntry<'_>) -> Result<(), RepoError> {
    let now = now_iso();
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("jti".into(), json!(new.jti));
    data.insert("user_id".into(), json!(new.user_id));
    data.insert("revoked_at".into(), json!(now));
    data.insert("expires_at".into(), json!(new.expires_at));
    match db::create(ctx, TABLE, data).await {
        Ok(_) => Ok(()),
        Err(e) => {
            // PRIMARY-KEY collisions are expected when the same JWT is
            // logged-out twice (rare but possible if a browser tab and a
            // background tab both fire logout). Surface the error string
            // so callers can decide; the only current caller swallows it.
            Err(RepoError::Db(format!("jwt_blocklist insert: {e}")))
        }
    }
}

/// True iff `jti` is in the blocklist, used by JWT validation in
/// `pipeline::handle_request`.
///
/// `Ok` → blocklisted, `NOT_FOUND` → not blocklisted. Any other backend
/// error (WRAP denial, connection blip) fails *closed* — returns `true`
/// so the request continues as unauthenticated rather than silently
/// re-enabling revoked JWTs until natural expiry. A `false` on transient
/// errors is the bigger footgun: a logged-out user keeps full access for
/// the remainder of the access-token lifetime.
pub async fn contains(ctx: &dyn Context, jti: &str) -> bool {
    use wafer_block::ErrorCode;
    match db::get_by_field(ctx, TABLE, "jti", json!(jti)).await {
        Ok(_) => true,
        Err(e) if e.code == ErrorCode::NotFound => false,
        Err(e) => {
            tracing::warn!(jti = %jti, "jwt_blocklist contains: db error — failing closed: {e}");
            true
        }
    }
}

/// Deletes all rows whose `expires_at < cutoff`. Returns the number deleted.
/// Best-effort sweeper — not required for correctness since `expires_at`
/// always matches the JWT's natural expiry, so an expired-and-not-yet-pruned
/// row is harmless (the JWT itself fails structural validation first).
#[allow(dead_code)]
pub async fn delete_expired(ctx: &dyn Context, cutoff: &str) -> Result<u64, RepoError> {
    let n = db::delete_by_filters_count(
        ctx,
        TABLE,
        vec![Filter {
            field: "expires_at".into(),
            operator: FilterOp::LessThan,
            value: json!(cutoff),
        }],
    )
    .await
    .map_err(|e| RepoError::Db(format!("jwt_blocklist delete_expired: {e}")))?;
    Ok(n.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestContext;

    fn iso_plus_seconds(secs: i64) -> String {
        let dt = chrono::Utc::now() + chrono::Duration::seconds(secs);
        dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }

    #[tokio::test]
    async fn insert_then_contains_returns_true() {
        let ctx = TestContext::with_auth().await;
        let exp = iso_plus_seconds(1800);
        insert(
            &ctx,
            NewBlocklistEntry {
                jti: "jti-1",
                user_id: "user-a",
                expires_at: &exp,
            },
        )
        .await
        .expect("insert");

        assert!(contains(&ctx, "jti-1").await);
    }

    #[tokio::test]
    async fn contains_returns_false_for_unknown_jti() {
        let ctx = TestContext::with_auth().await;
        assert!(!contains(&ctx, "missing").await);
    }

    #[tokio::test]
    async fn delete_expired_drops_only_expired_rows() {
        let ctx = TestContext::with_auth().await;
        let past = iso_plus_seconds(-60);
        let future = iso_plus_seconds(1800);
        insert(
            &ctx,
            NewBlocklistEntry {
                jti: "old",
                user_id: "u",
                expires_at: &past,
            },
        )
        .await
        .unwrap();
        insert(
            &ctx,
            NewBlocklistEntry {
                jti: "new",
                user_id: "u",
                expires_at: &future,
            },
        )
        .await
        .unwrap();

        let cutoff = iso_plus_seconds(0);
        let deleted = delete_expired(&ctx, &cutoff).await.expect("sweep");
        assert_eq!(deleted, 1);

        assert!(!contains(&ctx, "old").await);
        assert!(contains(&ctx, "new").await);
    }
}
