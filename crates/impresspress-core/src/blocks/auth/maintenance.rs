//! [B12] Retention for the four auth tables that carry an `expires_at`.
//!
//! `sessions`, `tokens`, `jwt_blocklist` and `oauth_pkce` all accumulate rows
//! that nothing will ever read again, and until this module none of them had a
//! non-test caller for its `delete_expired`: two carried `#[allow(dead_code)]`
//! and a third a "TODO: not yet wired". On a busy deployment `tokens` grows
//! fastest — rotation revokes rather than deletes, so every 30-minute refresh
//! left a tombstone forever.
//!
//! Two entry points, both of which exist on every platform today:
//!
//! 1. [`sweep_if_due`], deferred by `helpers::issue_tokens_and_cookie` to run
//!    after the sign-in's response ([`crate::deferred`]). Every deployment
//!    logs people in, so no operator has to remember anything and no
//!    scheduler has to exist. Throttled to [`SWEEP_INTERVAL_SECS`] through
//!    the `repo::maintenance` singleton, so a login storm costs one pass.
//! 2. The `auth.maintenance` message kind on `impresspress/auth-ui`, mirroring
//!    `tickets.maintenance`, for an operator or a future cron to force a pass.
//!    It lives on auth-ui rather than the framework `wafer-run/auth` block
//!    because that block routes every message through wafer-core's own
//!    `auth@v1` handler, which impresspress cannot extend.
//!
//! On Cloudflare a third caller reaches entry point 2: the Worker's
//! `scheduled` handler, on the `[triggers] crons` schedule the generated
//! wrangler config carries. It adds no code path here — it sends the same
//! message an operator does. Nothing in this module needs it; a deployment
//! with logins prunes without it, and a deployment without logins does not.

use serde::{Deserialize, Serialize};
use wafer_run::context::Context;

use super::repo::{jwt_blocklist, maintenance, oauth_pkce, sessions, tokens};

/// How long a pass covers: at most one sweep an hour, however many logins
/// arrive. Each pass is four filtered deletes, so the cost of running it a
/// little too often is small and the cost of never running it is unbounded
/// table growth — an hour is the conservative end of that trade.
pub const SWEEP_INTERVAL_SECS: i64 = 3_600;

/// What one retention pass removed.
///
/// Every count is reported even when another table's delete failed: a partial
/// pass still made progress, and `errors` names exactly which tables did not.
/// `complete` is what an operator or scheduler reads to decide whether to
/// retry.
/// `Deserialize` as well as `Serialize`: the pass runs inside a block and its
/// answer is read back outside one — by an operator's HTTP client, and by the
/// Cloudflare `scheduled` handler, which decodes it through
/// [`auth_ui::sweep_result_from_output`](crate::blocks::auth_ui::sweep_result_from_output)
/// to log the counts.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SweepResult {
    /// Whether every delete in the pass succeeded.
    pub complete: bool,
    /// Expired login families removed from the userportal device list.
    pub sessions_deleted: u64,
    /// Expired refresh-token rows removed, revoked tombstones included.
    pub tokens_deleted: u64,
    /// Blocklisted JWTs whose natural expiry has passed.
    pub jwt_blocklist_deleted: u64,
    /// OAuth PKCE states for flows the user abandoned.
    pub oauth_pkce_deleted: u64,
    /// Names of the deletes that failed (`"sessions"`, `"tokens"`,
    /// `"jwt_blocklist"`, `"oauth_pkce"`).
    pub errors: Vec<String>,
}

/// Delete every row in the four tables whose `expires_at` is in the past.
///
/// Each table is an independent `db::delete_by_filters_count` — one statement
/// per table, no list-then-delete-per-row — so a pass is four round trips
/// regardless of how many rows it removes. A failure on one table is recorded
/// and the pass continues: the tables are unrelated, and refusing to prune
/// three of them because the fourth is unavailable helps nobody.
pub async fn sweep(ctx: &dyn Context) -> SweepResult {
    // A single cutoff for the whole pass, in the `…Z` form every auth table
    // writes (`repo::now_iso`), so the string comparison the filter does is
    // against the same shape the rows hold.
    let cutoff = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let mut result = SweepResult {
        complete: true,
        ..SweepResult::default()
    };

    match sessions::delete_expired(ctx, &cutoff).await {
        Ok(n) => result.sessions_deleted = n,
        Err(e) => record_failure(&mut result, "sessions", &e.to_string()),
    }
    match tokens::delete_expired(ctx, &cutoff).await {
        Ok(n) => result.tokens_deleted = n,
        Err(e) => record_failure(&mut result, "tokens", &e.to_string()),
    }
    match jwt_blocklist::delete_expired(ctx, &cutoff).await {
        Ok(n) => result.jwt_blocklist_deleted = n,
        Err(e) => record_failure(&mut result, "jwt_blocklist", &e.to_string()),
    }
    match oauth_pkce::delete_expired(ctx, &cutoff).await {
        Ok(n) => result.oauth_pkce_deleted = n,
        Err(e) => record_failure(&mut result, "oauth_pkce", &e.to_string()),
    }

    result.complete = result.errors.is_empty();
    result
}

fn record_failure(result: &mut SweepResult, table: &str, error: &str) {
    tracing::warn!(table = %table, error = %error, "auth maintenance prune failed");
    result.errors.push(table.to_string());
}

/// Run [`sweep`] if [`SWEEP_INTERVAL_SECS`] has elapsed since the last pass,
/// and record the new stamp. Returns the pass's result, or `None` when the
/// window has not elapsed.
///
/// The stamp is written *before* the sweep runs, so a pass that fails partway
/// does not make every subsequent login retry it — the next window catches
/// whatever it missed, and the four deletes are idempotent anyway. A failure
/// to read or write the stamp skips the sweep rather than running it
/// unthrottled: an unreadable throttle on the login path must not turn into
/// four deletes per login.
pub async fn sweep_if_due(ctx: &dyn Context) -> Option<SweepResult> {
    let last = match maintenance::last_swept_at(ctx).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("auth maintenance: could not read the sweep stamp, skipping: {e}");
            return None;
        }
    };
    let now = chrono::Utc::now();
    if !due(&last, now) {
        return None;
    }
    if let Err(e) =
        maintenance::record_sweep(ctx, &now.format("%Y-%m-%dT%H:%M:%SZ").to_string()).await
    {
        tracing::warn!("auth maintenance: could not record the sweep stamp, skipping: {e}");
        return None;
    }
    Some(sweep(ctx).await)
}

/// Whether a sweep is due given the recorded stamp.
///
/// An empty stamp (no pass has run) and an unparseable one both mean "sweep":
/// a stamp nobody can read is not evidence that retention happened.
fn due(last_swept_at: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    match chrono::DateTime::parse_from_rfc3339(last_swept_at) {
        Ok(last) => (now - last.with_timezone(&chrono::Utc)).num_seconds() >= SWEEP_INTERVAL_SECS,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::auth::repo::{
            jwt_blocklist::NewBlocklistEntry, oauth_pkce::NewPkceState, sessions::NewSession,
        },
        test_support::{FailingDbOpContext, TestContext},
    };

    fn iso(offset_secs: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::seconds(offset_secs))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    /// One expired and one live row in each of the four tables.
    async fn seed_four_tables(ctx: &TestContext) {
        ctx.seed_auth_user("user-a").await;

        for (family, expires_at) in [("fam-dead", iso(-60)), ("fam-live", iso(3600))] {
            sessions::insert(
                ctx,
                NewSession {
                    family: family.into(),
                    user_id: "user-a".into(),
                    auth_method: "password".into(),
                    expires_at,
                },
            )
            .await
            .expect("seed session");
        }

        for (raw, expires_at) in [("tok-dead", iso(-60)), ("tok-live", iso(3600))] {
            tokens::insert(ctx, "user-a", raw, raw, 0, &expires_at)
                .await
                .expect("seed token");
        }

        for (jti, expires_at) in [("jti-dead", iso(-60)), ("jti-live", iso(3600))] {
            jwt_blocklist::insert(
                ctx,
                NewBlocklistEntry {
                    jti,
                    user_id: "user-a",
                    expires_at: &expires_at,
                },
            )
            .await
            .expect("seed blocklist entry");
        }

        for (state, expires_at) in [("pkce-dead", iso(-60)), ("pkce-live", iso(3600))] {
            oauth_pkce::insert(
                ctx,
                NewPkceState {
                    state_id: state,
                    provider: "github",
                    code_verifier: "v",
                    redirect_uri: "/",
                    expires_at: &expires_at,
                },
            )
            .await
            .expect("seed pkce state");
        }
    }

    #[tokio::test]
    async fn sweep_deletes_only_the_expired_rows_in_all_four_tables() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        seed_four_tables(&ctx).await;

        let result = sweep(&ctx).await;
        assert_eq!(
            result,
            SweepResult {
                complete: true,
                sessions_deleted: 1,
                tokens_deleted: 1,
                jwt_blocklist_deleted: 1,
                oauth_pkce_deleted: 1,
                errors: Vec::new(),
            }
        );

        // The live rows survived.
        let live_sessions = sessions::list_for_user(&ctx, "user-a").await.unwrap();
        assert_eq!(live_sessions.len(), 1);
        assert_eq!(live_sessions[0].family, "fam-live");
        assert!(tokens::find_by_token(&ctx, "tok-live")
            .await
            .unwrap()
            .is_some());
        assert!(tokens::find_by_token(&ctx, "tok-dead")
            .await
            .unwrap()
            .is_none());
        assert!(jwt_blocklist::contains(&ctx, "jti-live").await.unwrap());
        assert!(!jwt_blocklist::contains(&ctx, "jti-dead").await.unwrap());
        assert!(oauth_pkce::take(&ctx, "pkce-live").await.unwrap().is_some());
    }

    /// One unavailable table does not stop the other three from being pruned,
    /// and the pass says so rather than reporting success.
    #[tokio::test]
    async fn a_failing_table_is_named_and_the_others_still_run() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        seed_four_tables(&ctx).await;
        let failing =
            FailingDbOpContext::new(ctx, vec![("database.delete_where_count", tokens::TABLE)]);

        let result = sweep(&failing).await;
        assert!(!result.complete);
        assert_eq!(result.errors, vec!["tokens".to_string()]);
        assert_eq!(result.sessions_deleted, 1);
        assert_eq!(result.jwt_blocklist_deleted, 1);
        assert_eq!(result.oauth_pkce_deleted, 1);
    }

    #[tokio::test]
    async fn the_first_sweep_runs_and_the_second_inside_the_window_is_skipped() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        seed_four_tables(&ctx).await;

        let first = sweep_if_due(&ctx).await.expect("no stamp means overdue");
        assert_eq!(first.sessions_deleted, 1);

        assert!(
            sweep_if_due(&ctx).await.is_none(),
            "a second login within the hour must not sweep again"
        );
    }

    #[tokio::test]
    async fn a_stamp_older_than_the_window_is_due_again() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        seed_four_tables(&ctx).await;
        maintenance::record_sweep(&ctx, &iso(-(SWEEP_INTERVAL_SECS + 60)))
            .await
            .expect("stamp an old pass");

        assert!(sweep_if_due(&ctx).await.is_some());
    }

    #[test]
    fn an_empty_or_unparseable_stamp_is_due() {
        let now = chrono::Utc::now();
        assert!(due("", now), "no pass has ever run");
        assert!(
            due("not a timestamp", now),
            "a stamp nobody can read is not evidence of a pass"
        );
        assert!(!due(&now.to_rfc3339(), now), "a pass just ran");
    }

    /// An unreadable throttle skips the sweep. Running unthrottled instead
    /// would put four deletes on every single login.
    #[tokio::test]
    async fn an_unreadable_stamp_skips_rather_than_sweeps() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        seed_four_tables(&ctx).await;
        let failing = FailingDbOpContext::new(ctx, vec![("database.get", maintenance::TABLE)]);

        assert!(sweep_if_due(&failing).await.is_none());
        assert_eq!(
            sessions::list_for_user(&failing, "user-a")
                .await
                .unwrap()
                .len(),
            2,
            "nothing was pruned"
        );
    }
}
