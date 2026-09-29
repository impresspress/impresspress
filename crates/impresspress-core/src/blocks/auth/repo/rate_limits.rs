//! Row-level access over `wafer_run__auth__rate_limits`.
//!
//! Fixed-window counters keyed by user/IP, written on the Cloudflare
//! (`wasm32`) path only — the native `UserRateLimiter` keeps its counters in
//! an in-memory `Mutex<HashMap>` and never touches the database.
//!
//! [`windowed_increment`] is the one fixed-window upsert, shared by the
//! request limiter in `blocks/rate_limit.rs` and the ticket abuse limiter in
//! `blocks/tickets/abuse.rs`. It takes `now` from the caller rather than
//! reading a clock, because both call sites are `cfg(target_arch = "wasm32")`
//! (`std::time` panics there and `js_sys` does not exist on the host) — a
//! parameter is what lets the shared upsert be compiled, and tested, on the
//! host.
use serde_json::{json, Value};
use wafer_block::{
    db::{Filter, FilterOp},
    wire::database::OnConflict,
};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::{db_failed, internal_error};

pub const TABLE: &str = "wafer_run__auth__rate_limits";

/// Increment `key`'s counter inside the fixed window that ends at `now`, and
/// return the counter's value afterwards.
///
/// One statement, so one D1 round trip: an atomic `INSERT … ON CONFLICT DO
/// UPDATE … RETURNING *` (the server renders the dialect-portable `CASE WHEN`
/// from `OnConflict::WindowedCounter`) that answers the row as it left it. A
/// window older than `window_cutoff` resets the counter to one instead of
/// incrementing it. The count is the one this call's own statement produced,
/// so concurrent calls on one key each see a different value.
///
/// `id` is the caller's row id for a brand-new counter; on conflict the
/// existing row's id wins, so it only has to be unique, and the two callers
/// derive it from their own key namespace.
///
/// The statement always inserts or updates, so its row carries a count of at
/// least one. A failure, or an answer without that row, surfaces as `Err`,
/// and the caller decides whether to fail open.
pub async fn windowed_increment(
    ctx: &dyn Context,
    id: &str,
    key: &str,
    now: i64,
    window_cutoff: i64,
) -> Result<i64, WaferError> {
    let row = db::upsert_returning(
        ctx,
        TABLE,
        vec![
            ("id".to_string(), json!(id)),
            ("key".to_string(), json!(key)),
        ],
        vec!["key".to_string()],
        OnConflict::WindowedCounter {
            count_field: "count".to_string(),
            window_field: "window_start".to_string(),
            now,
            window_cutoff,
            created_fields: vec!["created_at".to_string()],
            updated_fields: vec!["updated_at".to_string()],
        },
    )
    .await
    .map_err(|e| db_failed("rate_limits windowed upsert", e))?;

    row.as_ref()
        .and_then(|r| r.data.get("count"))
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            internal_error(format!(
                "rate_limits windowed upsert answered no counter row: {row:?}"
            ))
        })
}

/// Drop counter rows whose `updated_at` is strictly before `cutoff`, and
/// report how many went. The retention sweep behind ticket maintenance.
pub async fn delete_updated_before(ctx: &dyn Context, cutoff: &str) -> Result<i64, WaferError> {
    db::delete_by_filters_count(
        ctx,
        TABLE,
        vec![Filter {
            field: "updated_at".to_string(),
            operator: FilterOp::LessThan,
            value: json!(cutoff),
        }],
    )
    .await
    .map_err(|e| db_failed("rate_limits prune", e))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use wafer_block::{BlockInfo, InputStream, Message, OutputStream};

    use super::*;
    use crate::{
        db_read,
        test_support::{AfterDbOpContext, TestContext},
    };

    /// Wraps a [`TestContext`] and records the op of every call it makes to
    /// the database block, so a test can count what one repository call
    /// sends. Each is at least one D1 round trip.
    #[derive(Clone)]
    struct DbOpLog {
        inner: TestContext,
        ops: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Context for DbOpLog {
        fn check_resource_access(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> Result<(), WaferError> {
            self.inner
                .check_resource_access(resource, resource_type, access)
        }

        fn resource_access_admitted(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> bool {
            self.inner
                .resource_access_admitted(resource, resource_type, access)
        }

        async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
            if name == "wafer-run/database" {
                self.ops
                    .lock()
                    .expect("op log")
                    .push(msg.action().to_string());
            }
            self.inner.call_block(name, msg, input).await
        }

        fn is_cancelled(&self) -> bool {
            self.inner.is_cancelled()
        }

        fn registered_blocks(&self) -> &[BlockInfo] {
            self.inner.registered_blocks()
        }

        fn config_get(&self, key: &str) -> Option<&str> {
            self.inner.config_get(key)
        }

        fn clone_arc(&self) -> Arc<dyn Context> {
            Arc::new(self.clone())
        }
    }

    async fn ctx() -> TestContext {
        TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID)
    }

    /// The behaviour both call sites depend on and neither could test:
    /// repeated hits inside one window accumulate, and a hit whose window
    /// has rolled over starts again from one.
    #[tokio::test]
    async fn increments_within_the_window_and_resets_after_it() {
        let ctx = ctx().await;
        let now = 1_000_000i64;
        let window = 60i64;

        assert_eq!(
            windowed_increment(&ctx, "id-1", "k", now, now - window)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            windowed_increment(&ctx, "id-2", "k", now + 1, now + 1 - window)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            windowed_increment(&ctx, "id-3", "k", now + 5, now + 5 - window)
                .await
                .unwrap(),
            3
        );

        // Same key, a window that started after the stored `window_start`:
        // the counter resets rather than carrying the old total forward.
        let later = now + 10 * window;
        assert_eq!(
            windowed_increment(&ctx, "id-4", "k", later, later - window)
                .await
                .unwrap(),
            1,
            "a hit past the window cutoff must reset the counter"
        );
    }

    /// An increment, and the read of the count it produced, is one call to
    /// the database: the `INSERT … ON CONFLICT … RETURNING *`. On D1 that is
    /// one round trip in front of every rate-limited route, sign-in included.
    #[tokio::test]
    async fn an_increment_is_one_database_call() {
        let ctx = ctx().await;
        let now = 3_000_000i64;
        // A warm counter: the increment under test finds the row and updates
        // it, the path every request after a key's first takes.
        windowed_increment(&ctx, "w1", "warm", now, now - 60)
            .await
            .unwrap();

        let log = DbOpLog {
            inner: ctx,
            ops: Arc::new(Mutex::new(Vec::new())),
        };
        let count = windowed_increment(&log, "w2", "warm", now + 1, now + 1 - 60)
            .await
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            *log.ops.lock().unwrap(),
            vec!["database.upsert".to_string()],
            "one statement, no read-back"
        );
    }

    /// Two requests on one key whose statements land back to back: each is
    /// charged its own hit, and each sees the count its own hit produced.
    /// The second lands after the first's statement has answered and before
    /// the first has returned, the gap a separate read-back of the counter
    /// would leave open: that read would see the second hit too, and both
    /// requests would report 2.
    #[tokio::test]
    async fn back_to_back_increments_each_see_their_own_count() {
        let ctx = ctx().await;
        let now = 4_000_000i64;
        let cutoff = now - 60;

        let second = Arc::new(Mutex::new(None));
        let (racer, seen) = (ctx.clone(), second.clone());
        let first = AfterDbOpContext::new(ctx.clone(), "database.upsert", TABLE, async move {
            let count = windowed_increment(&racer, "r2", "raced", now, cutoff)
                .await
                .expect("the second increment");
            *seen.lock().unwrap() = Some(count);
        });

        let first_count = windowed_increment(&first, "r1", "raced", now, cutoff)
            .await
            .unwrap();
        assert!(
            first.fired(),
            "the second increment must land inside the first"
        );
        assert_eq!(
            (first_count, *second.lock().unwrap()),
            (1, Some(2)),
            "each request reports the count its own hit produced"
        );
        assert_eq!(
            windowed_increment(&ctx, "r3", "raced", now, cutoff)
                .await
                .unwrap(),
            3,
            "both hits were charged"
        );
    }

    /// Many requests on one key at once: the statement is atomic, so no hit
    /// is lost and no two requests see the same count.
    #[tokio::test]
    async fn concurrent_increments_count_every_hit_once() {
        let ctx = ctx().await;
        let now = 5_000_000i64;
        let cutoff = now - 60;
        let hits = 16usize;

        let mut counts = futures::future::join_all((0..hits).map(|i| {
            let ctx = ctx.clone();
            async move {
                windowed_increment(&ctx, &format!("c{i}"), "crowd", now, cutoff)
                    .await
                    .unwrap()
            }
        }))
        .await;
        counts.sort_unstable();
        assert_eq!(counts, (1..=hits as i64).collect::<Vec<_>>());
    }

    /// A counter past its window restarts at one in the same single statement,
    /// and the requests after it count up from there, not from the old total.
    #[tokio::test]
    async fn a_rolled_window_restarts_the_count_in_one_call() {
        let ctx = ctx().await;
        let now = 6_000_000i64;
        for id in ["o1", "o2", "o3"] {
            windowed_increment(&ctx, id, "rolled", now, now - 60)
                .await
                .unwrap();
        }

        let later = now + 600;
        let log = DbOpLog {
            inner: ctx.clone(),
            ops: Arc::new(Mutex::new(Vec::new())),
        };
        assert_eq!(
            windowed_increment(&log, "n1", "rolled", later, later - 60)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            *log.ops.lock().unwrap(),
            vec!["database.upsert".to_string()]
        );
        assert_eq!(
            windowed_increment(&ctx, "n2", "rolled", later + 1, later + 1 - 60)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn keys_are_counted_independently() {
        let ctx = ctx().await;
        let now = 2_000_000i64;
        let cutoff = now - 60;
        windowed_increment(&ctx, "a1", "a", now, cutoff)
            .await
            .unwrap();
        windowed_increment(&ctx, "a2", "a", now, cutoff)
            .await
            .unwrap();
        assert_eq!(
            windowed_increment(&ctx, "b1", "b", now, cutoff)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn prune_deletes_only_rows_older_than_the_cutoff() {
        let ctx = ctx().await;
        let mut old: HashMap<String, Value> = HashMap::new();
        old.insert("id".into(), json!("old"));
        old.insert("key".into(), json!("old-key"));
        old.insert("count".into(), json!(1));
        old.insert("window_start".into(), json!(0));
        old.insert("created_at".into(), json!("2026-01-01T00:00:00Z"));
        old.insert("updated_at".into(), json!("2026-01-01T00:00:00Z"));
        db::create(&ctx, TABLE, old).await.unwrap();

        let mut fresh: HashMap<String, Value> = HashMap::new();
        fresh.insert("id".into(), json!("fresh"));
        fresh.insert("key".into(), json!("fresh-key"));
        fresh.insert("count".into(), json!(1));
        fresh.insert("window_start".into(), json!(0));
        fresh.insert("created_at".into(), json!("2026-06-01T00:00:00Z"));
        fresh.insert("updated_at".into(), json!("2026-06-01T00:00:00Z"));
        db::create(&ctx, TABLE, fresh).await.unwrap();

        assert_eq!(
            delete_updated_before(&ctx, "2026-03-01T00:00:00Z")
                .await
                .unwrap(),
            1
        );
        let left = db_read::list_every(&ctx, TABLE, vec![]).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "fresh");
    }
}
