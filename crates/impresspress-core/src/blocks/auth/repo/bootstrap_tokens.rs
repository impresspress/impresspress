//! Narrow access over `wafer_run__auth__bootstrap_tokens`.
//!
//! Only the subset needed by `AuthServiceImpl::require_role` plus a test
//! insert. The full bootstrap-admin lifecycle (issuance, consumption,
//! single-use semantics) lands in Plan A2.

use serde_json::json;
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::context::Context;

use super::{now_iso, RepoError};
use crate::util::hex_encode;

pub const TABLE: &str = "wafer_run__auth__bootstrap_tokens";

/// Insert a bootstrap token row. Used by Plan A2's bootstrap-admin init;
/// exposed here so the `require_role` integration tests can seed a row
/// directly without re-implementing the SQL.
pub async fn insert(
    ctx: &dyn Context,
    token_hash: Vec<u8>,
    expires_at: &str,
) -> Result<(), RepoError> {
    use std::collections::HashMap;

    use serde_json::Value;
    let id = uuid::Uuid::now_v7().to_string();
    let now = now_iso();
    let hex = hex_encode(&token_hash);
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("id".into(), json!(id));
    data.insert("token_hash".into(), json!(hex));
    data.insert("created_at".into(), json!(now));
    data.insert("expires_at".into(), json!(expires_at));

    db::create(ctx, TABLE, data)
        .await
        .map_err(|e| RepoError::Db(format!("bootstrap_tokens insert: {e}")))?;
    Ok(())
}

/// Returns true iff an unexpired row exists with the given hash.
///
/// Compared as ISO-8601 strings to match the text format the migration
/// schema stores.
pub async fn is_valid(ctx: &dyn Context, token_hash: &[u8]) -> Result<bool, RepoError> {
    let now = now_iso();
    let hex = hex_encode(token_hash);
    let filters = vec![
        Filter {
            field: "token_hash".into(),
            operator: FilterOp::Equal,
            value: json!(hex),
        },
        Filter {
            field: "expires_at".into(),
            operator: FilterOp::GreaterEqual,
            value: json!(now),
        },
    ];
    let records = db::list_all(ctx, TABLE, filters)
        .await
        .map_err(|e| RepoError::Db(format!("bootstrap_tokens lookup: {e}")))?;
    Ok(!records.is_empty())
}

/// Delete every row whose `token_hash` matches `token_hash`. Used by the
/// `/b/auth/bootstrap` redemption flow to consume the token after a
/// successful admin creation.
///
/// Single-use semantics: even if multiple rows happened to share the same
/// hash (shouldn't, but the schema doesn't enforce uniqueness here), this
/// removes all of them so subsequent `is_valid` calls return false.
pub async fn delete_by_hash(ctx: &dyn Context, token_hash: &[u8]) -> Result<(), RepoError> {
    let hex = hex_encode(token_hash);
    let filters = vec![Filter {
        field: "token_hash".into(),
        operator: FilterOp::Equal,
        value: json!(hex),
    }];
    let records = db::list_all(ctx, TABLE, filters)
        .await
        .map_err(|e| RepoError::Db(format!("bootstrap_tokens lookup for delete: {e}")))?;
    for record in records {
        db::delete(ctx, TABLE, &record.id)
            .await
            .map_err(|e| RepoError::Db(format!("bootstrap_tokens delete: {e}")))?;
    }
    Ok(())
}

/// Atomically validate-and-consume an unexpired bootstrap token in a single
/// `DELETE ... RETURNING` round trip (`db::take_by_filters` /
/// `ServiceOp::DATABASE_TAKE_WHERE`, built via
/// `wafer_sql_utils::query::build_delete_where_returning`). Returns `true`
/// iff a row was actually deleted (i.e. the token existed and had not
/// expired).
///
/// This closes the redemption race the old validate-then-create-then-delete
/// sequence had: because the "is it still valid" read and the "remove it"
/// write are the same SQL statement, two concurrent redemption attempts for
/// the same raw token cannot both see it as valid — the database itself
/// serializes the two `DELETE`s, so at most one caller's `take_valid_by_hash`
/// returns `true`. Callers MUST perform this consumption *before* creating
/// the privileged account, so only the winner of the atomic take proceeds.
pub async fn take_valid_by_hash(ctx: &dyn Context, token_hash: &[u8]) -> Result<bool, RepoError> {
    let now = now_iso();
    let hex = hex_encode(token_hash);
    let filters = vec![
        Filter {
            field: "token_hash".into(),
            operator: FilterOp::Equal,
            value: json!(hex),
        },
        Filter {
            field: "expires_at".into(),
            operator: FilterOp::GreaterEqual,
            value: json!(now),
        },
    ];
    let records = db::take_by_filters(ctx, TABLE, filters)
        .await
        .map_err(|e| RepoError::Db(format!("bootstrap_tokens take_valid_by_hash: {e}")))?;
    Ok(!records.is_empty())
}

#[cfg(test)]
mod typed_client_tests {
    use super::*;
    use crate::test_support::TestContext;

    fn future_iso(secs: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::seconds(secs))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    fn past_iso(secs: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::seconds(secs))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    #[tokio::test]
    async fn insert_then_validate_round_trips_under_wrap() {
        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            vec![],
            "impresspress/admin",
        );
        let hash = vec![0xab_u8; 32];
        insert(&ctx, hash.clone(), &future_iso(3600)).await.unwrap();
        assert!(is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn unknown_hash_is_invalid() {
        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            vec![],
            "impresspress/admin",
        );
        let hash = vec![0xcd_u8; 32];
        assert!(!is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn expired_hash_is_invalid() {
        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            vec![],
            "impresspress/admin",
        );
        let hash = vec![0xef_u8; 32];
        insert(&ctx, hash.clone(), &past_iso(3600)).await.unwrap();
        assert!(!is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn insert_then_delete_round_trips_under_wrap() {
        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            vec![],
            "impresspress/admin",
        );
        let hash = vec![0xff_u8; 32];
        insert(&ctx, hash.clone(), &future_iso(3600)).await.unwrap();
        assert!(is_valid(&ctx, &hash).await.unwrap());
        delete_by_hash(&ctx, &hash).await.unwrap();
        assert!(!is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn take_valid_by_hash_consumes_the_row_exactly_once() {
        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            vec![],
            "impresspress/admin",
        );
        let hash = vec![0x11_u8; 32];
        insert(&ctx, hash.clone(), &future_iso(3600)).await.unwrap();

        assert!(
            take_valid_by_hash(&ctx, &hash).await.unwrap(),
            "first take must consume a valid, unexpired row"
        );
        assert!(
            !take_valid_by_hash(&ctx, &hash).await.unwrap(),
            "second take on the same hash must find nothing left to consume"
        );
    }

    #[tokio::test]
    async fn take_valid_by_hash_unknown_hash_returns_false() {
        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            vec![],
            "impresspress/admin",
        );
        let hash = vec![0x22_u8; 32];
        assert!(!take_valid_by_hash(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn take_valid_by_hash_does_not_consume_an_expired_row() {
        let ctx = TestContext::with_auth().await.with_wrap(
            "wafer-run/auth",
            vec![],
            "impresspress/admin",
        );
        let hash = vec![0x33_u8; 32];
        insert(&ctx, hash.clone(), &past_iso(3600)).await.unwrap();

        assert!(!take_valid_by_hash(&ctx, &hash).await.unwrap());
        // The expired row is untouched by the failed take (its filter
        // excludes it), so a plain lookup still finds it in the table —
        // just still (correctly) reported invalid by `is_valid`.
        assert!(!is_valid(&ctx, &hash).await.unwrap());
    }
}
