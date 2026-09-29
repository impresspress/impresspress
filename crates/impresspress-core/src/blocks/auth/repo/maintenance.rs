//! The auth sweeper's singleton bookkeeping row.
//!
//! One row, id `"singleton"`, seeded by `012_sessions_family.sqlite.sql`. It
//! holds the timestamp of the last retention pass so
//! [`super::super::maintenance::sweep_if_due`] can run the sweep at most once
//! an hour from a path — token issuance — that runs on every login and every
//! refresh. Same shape as `impresspress__tickets__maintenance`.
//!
//! This is a table the auth block owns rather than a row in
//! `impresspress__admin__variables`, because WRAP grants are per table: the
//! variables table holds `WAFER_RUN__AUTH__JWT_SECRET`, and buying a GC
//! timestamp with write access to the deployment's signing key is not a trade
//! worth making. auth-ui's existing `wafer_run__auth__*` wildcard already
//! covers this one.

use std::collections::HashMap;

use serde_json::json;
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::db_failed;

pub const TABLE: &str = "wafer_run__auth__maintenance";

/// The one row's id. The table is a singleton, so the column carries no
/// information beyond "this is the row".
pub const SINGLETON_ID: &str = "singleton";

/// The ISO-8601 timestamp of the last completed sweep, or `""` when none has
/// run (the migration seeds the row with the column's `''` default).
///
/// A missing row is `Ok("")` rather than an error: the caller's next move is
/// to sweep, which is also the right move if the row somehow went away.
pub async fn last_swept_at(ctx: &dyn Context) -> Result<String, WaferError> {
    use wafer_block::ErrorCode;

    match db::get(ctx, TABLE, SINGLETON_ID).await {
        Ok(record) => Ok(crate::util::RecordExt::str_field(&record, "last_swept_at").to_string()),
        Err(e) if e.code == ErrorCode::NotFound => Ok(String::new()),
        Err(e) => Err(db_failed("maintenance last_swept_at", e)),
    }
}

/// Stamp `at` as the time of the last completed sweep.
///
/// Update-then-create, the shape `tickets::maintenance::store_result` uses:
/// the migration seeds the row, but a database restored without it must not
/// leave the throttle permanently unwritable.
pub async fn record_sweep(ctx: &dyn Context, at: &str) -> Result<(), WaferError> {
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("last_swept_at".into(), json!(at));
    if db::update(ctx, TABLE, SINGLETON_ID, data.clone())
        .await
        .is_ok()
    {
        return Ok(());
    }
    data.insert("id".into(), json!(SINGLETON_ID));
    db::create(ctx, TABLE, data)
        .await
        .map_err(|e| db_failed("maintenance record_sweep", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestContext;

    #[tokio::test]
    async fn the_seeded_row_starts_empty_and_round_trips_a_stamp() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        assert_eq!(
            last_swept_at(&ctx).await.unwrap(),
            "",
            "the migration seeds the singleton with no sweep recorded"
        );

        record_sweep(&ctx, "2026-09-06T12:00:00Z").await.unwrap();
        assert_eq!(last_swept_at(&ctx).await.unwrap(), "2026-09-06T12:00:00Z");

        record_sweep(&ctx, "2026-09-06T13:00:00Z").await.unwrap();
        assert_eq!(
            last_swept_at(&ctx).await.unwrap(),
            "2026-09-06T13:00:00Z",
            "a second pass overwrites rather than inserting a second row"
        );
    }
}
