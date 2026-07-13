//! Row-level access over `wafer_run__auth__oauth_pkce_states` (SEC-040).
//!
//! Holds OAuth PKCE state during the round-trip from the authorization
//! endpoint to the callback. The client only sees an opaque `state_id`;
//! the secret `code_verifier`, provider name, and redirect_uri live here,
//! keyed by `state_id` and bounded by `expires_at`.
//!
//! [`take`] performs select-then-delete so a given `state_id` can only
//! be redeemed once. Rows past `expires_at` are treated as missing and
//! also dropped on lookup as a side effect — a periodic sweeper is
//! additive, not load-bearing for correctness.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::context::Context;

use super::{map_opt_str, map_str, now_iso, RepoError};

pub const TABLE: &str = "wafer_run__auth__oauth_pkce_states";

/// Payload for [`insert`].
#[derive(Debug, Clone)]
pub struct NewPkceState<'a> {
    pub state_id: &'a str,
    pub provider: &'a str,
    pub code_verifier: &'a str,
    pub redirect_uri: &'a str,
    /// Absolute expiry time as ISO-8601 (`%Y-%m-%dT%H:%M:%SZ`).
    pub expires_at: &'a str,
}

/// Row returned by [`take`]: everything the callback needs to complete the
/// token exchange. `state_id` is excluded because the caller already has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceStateRow {
    pub provider: String,
    pub code_verifier: String,
    pub redirect_uri: String,
    pub expires_at: String,
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<PkceStateRow, RepoError> {
    Ok(PkceStateRow {
        provider: map_opt_str(m, "provider")
            .ok_or_else(|| RepoError::Db("missing provider".into()))?,
        code_verifier: map_opt_str(m, "code_verifier")
            .ok_or_else(|| RepoError::Db("missing code_verifier".into()))?,
        redirect_uri: map_opt_str(m, "redirect_uri")
            .ok_or_else(|| RepoError::Db("missing redirect_uri".into()))?,
        expires_at: map_str(m, "expires_at"),
    })
}

/// Insert a new PKCE state row.
///
/// PRIMARY-KEY collisions on `state_id` indicate a generator failure (the
/// caller is expected to pull fresh random bytes), surfaced as `RepoError::Db`.
pub async fn insert(ctx: &dyn Context, new: NewPkceState<'_>) -> Result<(), RepoError> {
    let now = now_iso();
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("state_id".into(), json!(new.state_id));
    data.insert("provider".into(), json!(new.provider));
    data.insert("code_verifier".into(), json!(new.code_verifier));
    data.insert("redirect_uri".into(), json!(new.redirect_uri));
    data.insert("created_at".into(), json!(now));
    data.insert("expires_at".into(), json!(new.expires_at));
    db::create(ctx, TABLE, data)
        .await
        .map_err(|e| RepoError::Db(format!("oauth_pkce insert: {e}")))?;
    Ok(())
}

/// Look up a PKCE state by `state_id` and simultaneously delete it.
///
/// Returns `Ok(None)` if the state is missing OR present-but-expired (the
/// expired row is still deleted as a side effect — single-use even on
/// timeout). Uses `db::take_by_filters` which dispatches to
/// `DELETE … WHERE … RETURNING *` (sqlite 3.35+, postgres) so the read
/// and delete are atomic in a single statement.
pub async fn take(ctx: &dyn Context, state_id: &str) -> Result<Option<PkceStateRow>, RepoError> {
    let rows = db::take_by_filters(
        ctx,
        TABLE,
        vec![Filter {
            field: "state_id".into(),
            operator: FilterOp::Equal,
            value: json!(state_id),
        }],
    )
    .await
    .map_err(|e| RepoError::Db(format!("oauth_pkce take: {e}")))?;
    let Some(r) = rows.into_iter().next() else {
        return Ok(None);
    };
    let row = row_from_map(&r.data)?;
    if row.expires_at.as_str() < now_iso().as_str() {
        // Row was present but expired — already deleted as a side effect.
        return Ok(None);
    }
    Ok(Some(row))
}

/// Deletes all rows whose `expires_at < cutoff`. Returns the number deleted.
/// Intended for a background sweeper (TODO: not yet wired) — not required
/// for correctness since [`take`] also drops expired rows on read.
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
    .map_err(|e| RepoError::Db(format!("oauth_pkce delete_expired: {e}")))?;
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
    async fn insert_then_take_returns_row_and_deletes() {
        let ctx = TestContext::with_auth().await;
        let expires = iso_plus_seconds(600);
        insert(
            &ctx,
            NewPkceState {
                state_id: "state-1",
                provider: "github",
                code_verifier: "verifier-abc",
                redirect_uri: "https://example.test/b/auth/oauth/callback",
                expires_at: &expires,
            },
        )
        .await
        .expect("insert");

        let row = take(&ctx, "state-1")
            .await
            .expect("take")
            .expect("row present");
        assert_eq!(row.provider, "github");
        assert_eq!(row.code_verifier, "verifier-abc");
        assert_eq!(
            row.redirect_uri,
            "https://example.test/b/auth/oauth/callback"
        );

        // Second take returns None — single-use.
        assert!(take(&ctx, "state-1").await.expect("take").is_none());
    }

    #[tokio::test]
    async fn take_returns_none_for_unknown_state_id() {
        let ctx = TestContext::with_auth().await;
        assert!(take(&ctx, "missing").await.expect("take").is_none());
    }

    #[tokio::test]
    async fn take_treats_expired_rows_as_missing() {
        let ctx = TestContext::with_auth().await;
        // Insert with expires_at in the past.
        let past = iso_plus_seconds(-10);
        insert(
            &ctx,
            NewPkceState {
                state_id: "state-expired",
                provider: "google",
                code_verifier: "v",
                redirect_uri: "https://example.test/cb",
                expires_at: &past,
            },
        )
        .await
        .expect("insert");

        assert!(take(&ctx, "state-expired").await.expect("take").is_none());
        // And the row is gone (single-use even on timeout).
        assert!(take(&ctx, "state-expired").await.expect("take").is_none());
    }

    #[tokio::test]
    async fn delete_expired_drops_only_expired_rows() {
        let ctx = TestContext::with_auth().await;
        let past = iso_plus_seconds(-60);
        let future = iso_plus_seconds(600);
        insert(
            &ctx,
            NewPkceState {
                state_id: "old",
                provider: "github",
                code_verifier: "v1",
                redirect_uri: "https://example.test/cb",
                expires_at: &past,
            },
        )
        .await
        .unwrap();
        insert(
            &ctx,
            NewPkceState {
                state_id: "new",
                provider: "github",
                code_verifier: "v2",
                redirect_uri: "https://example.test/cb",
                expires_at: &future,
            },
        )
        .await
        .unwrap();

        let cutoff = iso_plus_seconds(0);
        let deleted = delete_expired(&ctx, &cutoff).await.expect("sweep");
        assert_eq!(deleted, 1);

        // Old gone, new still takeable.
        assert!(take(&ctx, "old").await.unwrap().is_none());
        let row = take(&ctx, "new").await.unwrap().expect("present");
        assert_eq!(row.code_verifier, "v2");
    }
}
