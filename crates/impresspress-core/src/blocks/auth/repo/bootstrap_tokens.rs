//! Row-level access over `wafer_run__auth__bootstrap_tokens`.
//!
//! A row is one installed bootstrap token: sha256 of the raw token
//! (`token_hash`, UNIQUE) and when it stops being redeemable (`expires_at`).
//! [`insert_if_absent`] installs one (`auth::bootstrap::run`, on `Init`),
//! [`is_valid`] answers whether a presented token is live (the redemption
//! route's pre-check and `AuthServiceImpl::require_role`), and
//! [`take_valid_by_hash`] spends one (`POST /b/auth/api/bootstrap`).

use serde_json::json;
use wafer_block::{
    db::{Filter, FilterOp},
    wire::database::OnConflict,
};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::{db_failed, now_iso};
use crate::{
    db_read::{self, Bound},
    util::hex_encode,
};

pub const TABLE: &str = "wafer_run__auth__bootstrap_tokens";

/// Install a row for `token_hash` expiring at `expires_at`, unless a row for
/// that hash is already there. `true` when this call installed it, `false`
/// when it found one and left it exactly as it was.
///
/// One `INSERT … ON CONFLICT (token_hash) DO NOTHING RETURNING *`, so the
/// "is it there" and the "install it" cannot be split by another writer: two
/// runtimes installing the same token at once (two Cloudflare isolates, two
/// native processes over one PostgreSQL) leave one row, and neither fails.
///
/// The existing row is never rewritten, expired or not. `auth::bootstrap::run`
/// calls this on every `Init` while no account exists, and every runtime build
/// runs `Init`; a rewrite would restart the token's 24 hours on each of them.
pub async fn insert_if_absent(
    ctx: &dyn Context,
    token_hash: &[u8],
    expires_at: &str,
) -> Result<bool, WaferError> {
    let now = now_iso();
    let installed = db::upsert_returning(
        ctx,
        TABLE,
        vec![
            ("id".to_string(), json!(uuid::Uuid::now_v7().to_string())),
            ("token_hash".to_string(), json!(hex_encode(token_hash))),
            ("created_at".to_string(), json!(now)),
            ("expires_at".to_string(), json!(expires_at)),
        ],
        vec!["token_hash".to_string()],
        // No columns to set: `DO NOTHING` on a conflict.
        OnConflict::SetColumns(Vec::new()),
    )
    .await
    .map_err(|e| db_failed("bootstrap_tokens insert", e))?;
    Ok(installed.is_some())
}

/// Returns true iff an unexpired row exists with the given hash.
///
/// Compared as ISO-8601 strings to match the text format the migration
/// schema stores.
pub async fn is_valid(ctx: &dyn Context, token_hash: &[u8]) -> Result<bool, WaferError> {
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
    let records = db_read::list_bounded(
        ctx,
        TABLE,
        filters,
        Bound::UniqueKey("bootstrap_tokens.token_hash is declared UNIQUE"),
    )
    .await
    .map_err(|e| db_failed("bootstrap_tokens lookup", e))?;
    Ok(!records.is_empty())
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
pub async fn take_valid_by_hash(ctx: &dyn Context, token_hash: &[u8]) -> Result<bool, WaferError> {
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
        .map_err(|e| db_failed("bootstrap_tokens take_valid_by_hash", e))?;
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
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let hash = vec![0xab_u8; 32];
        assert!(insert_if_absent(&ctx, &hash, &future_iso(3600))
            .await
            .unwrap());
        assert!(is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn unknown_hash_is_invalid() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let hash = vec![0xcd_u8; 32];
        assert!(!is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn expired_hash_is_invalid() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let hash = vec![0xef_u8; 32];
        assert!(insert_if_absent(&ctx, &hash, &past_iso(3600))
            .await
            .unwrap());
        assert!(!is_valid(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn take_valid_by_hash_consumes_the_row_exactly_once() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let hash = vec![0x11_u8; 32];
        assert!(insert_if_absent(&ctx, &hash, &future_iso(3600))
            .await
            .unwrap());

        assert!(
            take_valid_by_hash(&ctx, &hash).await.unwrap(),
            "first take must consume a valid, unexpired row"
        );
        assert!(
            !take_valid_by_hash(&ctx, &hash).await.unwrap(),
            "second take on the same hash must find nothing left to consume"
        );
    }

    /// The same single-use contract as
    /// [`take_valid_by_hash_consumes_the_row_exactly_once`], but over a
    /// file-backed database — the read/write-split topology every native
    /// deployment runs, and the one the in-memory fixture above cannot
    /// produce (see [`TestContext::new_on_disk`]).
    ///
    /// `take_valid_by_hash` is a `DELETE … RETURNING`, a write. Dispatched
    /// down the read path it reaches a `SQLITE_OPEN_READ_ONLY` connection and
    /// fails, and `wafer-block-sqlite`'s pre-fix `run_fetch` dropped that
    /// per-row failure as if it were a decode error, so the whole call
    /// returned `Ok(vec![])` — no rows, no error. That is `Ok(false)` here,
    /// and the redemption handler (`POST /b/auth/api/bootstrap`,
    /// [`crate::blocks::auth_ui::api::bootstrap`]) answers `Ok(false)` with
    /// `err_unauthorized("invalid or expired bootstrap token")`: on native,
    /// redemption of a perfectly valid token could not succeed at all. The
    /// failure is closed, not open — the token is never accepted, so it is
    /// also never replayed — but it is invisible to every in-memory test,
    /// which is why this one exists.
    #[tokio::test]
    async fn take_valid_by_hash_consumes_the_row_on_a_file_backed_database() {
        let ctx = TestContext::with_auth_on_disk()
            .await
            .running_as("wafer-run/auth");
        let hash = vec![0x44_u8; 32];
        assert!(insert_if_absent(&ctx, &hash, &future_iso(3600))
            .await
            .unwrap());

        assert!(
            take_valid_by_hash(&ctx, &hash).await.unwrap(),
            "the take must reach the write connection and delete the row: a \
             valid, unexpired bootstrap token that reports itself unconsumable \
             makes redemption answer 401 on every native deployment"
        );
        assert!(
            !is_valid(&ctx, &hash).await.unwrap(),
            "the redeemed token must be gone from the table, not merely \
             reported as taken"
        );
        assert!(
            !take_valid_by_hash(&ctx, &hash).await.unwrap(),
            "second take on the same hash must find nothing left to consume"
        );
    }

    #[tokio::test]
    async fn take_valid_by_hash_unknown_hash_returns_false() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let hash = vec![0x22_u8; 32];
        assert!(!take_valid_by_hash(&ctx, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn take_valid_by_hash_does_not_consume_an_expired_row() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let hash = vec![0x33_u8; 32];
        assert!(insert_if_absent(&ctx, &hash, &past_iso(3600))
            .await
            .unwrap());

        assert!(!take_valid_by_hash(&ctx, &hash).await.unwrap());
        // The expired row is untouched by the failed take (its filter
        // excludes it), so a plain lookup still finds it in the table —
        // just still (correctly) reported invalid by `is_valid`.
        assert!(!is_valid(&ctx, &hash).await.unwrap());
    }
}
