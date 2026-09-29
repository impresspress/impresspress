//! Row-level access over `wafer_run__auth__local_credentials`.
//!
//! Holds the Argon2id `password_hash` for users who authenticate with
//! email + password. OAuth-only users have no row here. The `user_id` column
//! is the primary key and references `wafer_run__auth__users(id)` with
//! `ON DELETE CASCADE`.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::{
    db::{Filter, FilterOp},
    wire::database::BatchWrite,
};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::{db_failed, internal_error, map_bool, map_opt_str, map_str, now_iso};

pub const TABLE: &str = "wafer_run__auth__local_credentials";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCredentialRow {
    pub user_id: String,
    pub password_hash: String,
    pub must_reset: bool,
    pub created_at: String,
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<LocalCredentialRow, WaferError> {
    Ok(LocalCredentialRow {
        user_id: map_opt_str(m, "user_id").ok_or_else(|| internal_error("missing user_id"))?,
        password_hash: map_opt_str(m, "password_hash")
            .ok_or_else(|| internal_error("missing password_hash"))?,
        must_reset: map_bool(m, "must_reset"),
        created_at: map_str(m, "created_at"),
    })
}

/// Insert a local-credentials row for `user_id`. Fails if a row already
/// exists for that user (PK collision).
///
/// For an account that already exists. A new account gets its row in the
/// same write as the account (`users::insert_with_password`).
pub async fn insert(
    ctx: &dyn Context,
    user_id: &str,
    password_hash: &str,
    must_reset: bool,
) -> Result<(), WaferError> {
    db::create(ctx, TABLE, new_row(user_id, password_hash, must_reset))
        .await
        .map_err(|e| db_failed("local_credentials insert", e))?;
    Ok(())
}

/// [`insert`] as one write of a batch.
pub fn create_op(user_id: &str, password_hash: &str, must_reset: bool) -> BatchWrite {
    BatchWrite::Create {
        collection: TABLE.to_string(),
        data: new_row(user_id, password_hash, must_reset),
    }
}

fn new_row(user_id: &str, password_hash: &str, must_reset: bool) -> HashMap<String, Value> {
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("id".into(), json!(uuid::Uuid::now_v7().to_string()));
    data.insert("user_id".into(), json!(user_id));
    data.insert("password_hash".into(), json!(password_hash));
    data.insert("must_reset".into(), json!(must_reset));
    data.insert("created_at".into(), json!(now_iso()));
    data
}

/// Update the `password_hash` for `user_id`.
///
/// If no row exists yet (e.g. an OAuth-only user setting a password for the
/// first time), inserts a new row via [`insert`].
pub async fn update_password(
    ctx: &dyn Context,
    user_id: &str,
    new_hash: &str,
) -> Result<(), WaferError> {
    use wafer_block::ErrorCode;
    let filters = vec![Filter {
        field: "user_id".into(),
        operator: FilterOp::Equal,
        value: serde_json::json!(user_id),
    }];
    match db::get_by_field(ctx, TABLE, "user_id", json!(user_id)).await {
        Ok(_) => {
            let mut data: HashMap<String, Value> = HashMap::new();
            data.insert("password_hash".into(), json!(new_hash));
            db::update_by_filters(ctx, TABLE, filters, data)
                .await
                .map_err(|e| db_failed("local_credentials update_password", e))?;
            Ok(())
        }
        Err(e) if e.code == ErrorCode::NotFound => insert(ctx, user_id, new_hash, false).await,
        Err(e) => Err(db_failed("local_credentials lookup", e)),
    }
}

/// Whether `user_id` has a password at all — the question "can this account
/// sign in without its OAuth links", asked by
/// `userportal::pages::security::handle_unlink` before it removes one.
///
/// A count, not a row read. The caller needs one bool and has no business
/// holding an Argon2id digest to compute it; WRAP grants are per table, so
/// this is the only place the surface can be narrowed, and narrowing it here
/// means the hash never leaves the database.
pub async fn has_password(ctx: &dyn Context, user_id: &str) -> Result<bool, WaferError> {
    let n = db::count_by_field(ctx, TABLE, "user_id", json!(user_id))
        .await
        .map_err(|e| db_failed("local_credentials has_password", e))?;
    Ok(n > 0)
}

pub async fn find_by_user_id(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Option<LocalCredentialRow>, WaferError> {
    use wafer_block::ErrorCode;
    match db::get_by_field(ctx, TABLE, "user_id", json!(user_id)).await {
        Ok(rec) => Ok(Some(row_from_map(&rec.data)?)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(db_failed("local_credentials select", e)),
    }
}

#[cfg(test)]
mod typed_client_tests {
    use super::*;
    use crate::test_support::TestContext;

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        ctx.seed_auth_user(user_id).await;
    }

    #[tokio::test]
    async fn insert_then_find_round_trip_under_wrap() {
        // Seed user before enabling WRAP so the exec_raw fixture INSERT is not
        // subject to the WRAP check (same pattern as sessions.rs seed helpers).
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        let ctx = ctx.running_as("wafer-run/auth");
        insert(&ctx, "user-a", "$argon2id$dummy", false)
            .await
            .unwrap();
        let got = find_by_user_id(&ctx, "user-a").await.unwrap().unwrap();
        assert_eq!(got.user_id, "user-a");
        assert_eq!(got.password_hash, "$argon2id$dummy");
        assert!(!got.must_reset);
    }

    /// The unlink guard's question, answered without materialising the row:
    /// `handle_unlink` needs one bool and must not be handed an Argon2id
    /// digest to compute it.
    #[tokio::test]
    async fn has_password_is_true_only_once_a_credential_exists() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        let ctx = ctx.running_as("wafer-run/auth");

        assert!(
            !has_password(&ctx, "user-a").await.unwrap(),
            "an OAuth-only account has no password"
        );
        insert(&ctx, "user-a", "$argon2id$dummy", false)
            .await
            .unwrap();
        assert!(has_password(&ctx, "user-a").await.unwrap());
        assert!(
            !has_password(&ctx, "ghost").await.unwrap(),
            "an unknown user has no password either"
        );
    }

    #[tokio::test]
    async fn find_by_unknown_user_returns_none() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        assert!(find_by_user_id(&ctx, "ghost").await.unwrap().is_none());
    }

    /// Postgres returns BOOLEAN columns as JSON `bool`; sqlite returns INTEGER
    /// 0/1. `row_from_map` must accept both. Pure-shape test on the
    /// deserializer — no DB roundtrip — so the assertion holds regardless of
    /// which backend the test fixture happens to use.
    #[test]
    fn row_from_map_accepts_bool_int_and_string_must_reset() {
        let mk = |v: Value| {
            let mut m = HashMap::new();
            m.insert("user_id".into(), json!("u"));
            m.insert("password_hash".into(), json!("h"));
            m.insert("must_reset".into(), v);
            m.insert("created_at".into(), json!("2026-01-01T00:00:00Z"));
            row_from_map(&m).unwrap()
        };
        assert!(mk(json!(true)).must_reset, "JSON bool true (postgres)");
        assert!(!mk(json!(false)).must_reset, "JSON bool false (postgres)");
        assert!(mk(json!(1)).must_reset, "JSON int 1 (sqlite)");
        assert!(!mk(json!(0)).must_reset, "JSON int 0 (sqlite)");
        assert!(mk(json!("true")).must_reset, "string 'true'");
        assert!(mk(json!("1")).must_reset, "string '1'");
        assert!(!mk(json!("false")).must_reset, "string 'false'");
        assert!(!mk(Value::Null).must_reset, "missing/null defaults false");
    }
}
