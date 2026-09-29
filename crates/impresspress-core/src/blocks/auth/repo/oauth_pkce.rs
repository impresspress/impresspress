//! Row-level access over `wafer_run__auth__oauth_pkce_states` (SEC-040).
//!
//! Holds OAuth PKCE state during the round-trip from the authorization
//! endpoint to the callback. The client only sees an opaque `state_id`;
//! the secret `code_verifier`, provider name, and redirect_uri live here,
//! keyed by `state_id` and bounded by `expires_at`.
//!
//! [`take`] reads and deletes in one `DELETE … RETURNING` statement, so a
//! given `state_id` can only be redeemed once. Rows past `expires_at` are
//! treated as missing and also dropped on lookup as a side effect — a
//! periodic sweeper ([`delete_expired`]) is additive, not load-bearing for
//! correctness.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::{db_failed, internal_error, map_opt_str, map_str, now_iso};

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

fn row_from_map(m: &HashMap<String, Value>) -> Result<PkceStateRow, WaferError> {
    Ok(PkceStateRow {
        provider: map_opt_str(m, "provider").ok_or_else(|| internal_error("missing provider"))?,
        code_verifier: map_opt_str(m, "code_verifier")
            .ok_or_else(|| internal_error("missing code_verifier"))?,
        redirect_uri: map_opt_str(m, "redirect_uri")
            .ok_or_else(|| internal_error("missing redirect_uri"))?,
        expires_at: map_str(m, "expires_at"),
    })
}

/// Insert a new PKCE state row.
///
/// PRIMARY-KEY collisions on `state_id` indicate a generator failure (the
/// caller is expected to pull fresh random bytes), surfaced as whatever
/// [`wafer_run::ErrorCode`] the backend classified the collision with.
pub async fn insert(ctx: &dyn Context, new: NewPkceState<'_>) -> Result<(), WaferError> {
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
        .map_err(|e| db_failed("oauth_pkce insert", e))?;
    Ok(())
}

/// Look up a PKCE state by `state_id` and simultaneously delete it.
///
/// Returns `Ok(None)` if the state is missing OR present-but-expired (the
/// expired row is still deleted as a side effect — single-use even on
/// timeout). Uses `db::take_by_filters` which dispatches to
/// `DELETE … WHERE … RETURNING *` (sqlite 3.35+, postgres) so the read
/// and delete are atomic in a single statement.
pub async fn take(ctx: &dyn Context, state_id: &str) -> Result<Option<PkceStateRow>, WaferError> {
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
    .map_err(|e| db_failed("oauth_pkce take", e))?;
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
/// Called by `auth::maintenance::sweep` — not required for correctness, since
/// [`take`] also drops expired rows on read, but an OAuth flow the user
/// abandons leaves a row nothing ever reads.
pub async fn delete_expired(ctx: &dyn Context, cutoff: &str) -> Result<u64, WaferError> {
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
    .map_err(|e| db_failed("oauth_pkce delete_expired", e))?;
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
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

    /// The same round-trip as
    /// [`insert_then_take_returns_row_and_deletes`], but over a file-backed
    /// database — the read/write-split topology every native deployment runs,
    /// and the one the in-memory fixture cannot produce (see
    /// [`TestContext::new_on_disk`]).
    ///
    /// [`take`] is a `DELETE … RETURNING`, a write. Dispatched down the read
    /// path it reaches a `SQLITE_OPEN_READ_ONLY` connection and fails, and
    /// `wafer-block-sqlite`'s pre-fix `run_fetch` dropped that per-row failure
    /// as if it were a decode error, so the call returned `Ok(vec![])` — no
    /// rows, no error. `take` reads that as `Ok(None)` and
    /// `auth_ui::oauth::callback` answers `Invalid or expired OAuth state`:
    /// on native, no OAuth sign-in could complete, and the state row it
    /// should have consumed stayed in the table until `delete_expired` swept
    /// it.
    #[tokio::test]
    async fn take_consumes_the_row_on_a_file_backed_database() {
        let ctx = TestContext::with_auth_on_disk()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        let expires = iso_plus_seconds(600);
        insert(
            &ctx,
            NewPkceState {
                state_id: "state-on-disk",
                provider: "github",
                code_verifier: "verifier-on-disk",
                redirect_uri: "https://example.test/b/auth/oauth/callback",
                expires_at: &expires,
            },
        )
        .await
        .expect("insert");

        let row = take(&ctx, "state-on-disk").await.expect("take").expect(
            "the take must reach the write connection and return the row it \
             deleted: a live PKCE state that reports itself missing fails \
             every OAuth callback on a native deployment",
        );
        assert_eq!(row.code_verifier, "verifier-on-disk");

        assert!(
            take(&ctx, "state-on-disk").await.expect("take").is_none(),
            "the redeemed state must be gone from the table, not merely \
             reported as taken"
        );
    }

    #[tokio::test]
    async fn take_returns_none_for_unknown_state_id() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        assert!(take(&ctx, "missing").await.expect("take").is_none());
    }

    #[tokio::test]
    async fn take_treats_expired_rows_as_missing() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
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
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
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
