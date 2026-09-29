//! Constant-statement-count retention and operational status.

use serde::Serialize;
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::{config::SecurityReadiness, repo};
use crate::util::json_map;

/// Outcome of one retention pass, as returned by
/// `POST /b/tickets/api/admin/retention/prune`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct MaintenanceResult {
    /// Whether every delete in the pass succeeded. A `false` here is answered
    /// with HTTP 503 so a scheduler retries.
    pub complete: bool,
    /// Expired analyses removed.
    pub analyses_deleted: i64,
    /// Expired audit events removed.
    pub events_deleted: i64,
    /// Expired tickets removed. Tickets under legal hold never expire.
    pub tickets_deleted: i64,
    /// Stale submission rate-limit counters removed.
    pub rate_counters_deleted: i64,
    /// Names of the deletes that failed (`"analyses"`, `"events"`,
    /// `"tickets"`, `"rate-counters"`).
    pub errors: Vec<String>,
}

/// The stored record of the last retention pass.
// `id` is not published: the row is a singleton keyed on the literal
// `"singleton"`, so the column carries no information.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct MaintenanceState {
    /// `YYYY-MM-DD` of the last pass, or `""` before the first one.
    pub last_pruned_day: String,
    /// RFC 3339 timestamp of the last pass, or `null` before the first one.
    pub last_pruned_at: Option<String>,
    /// Comma-joined names of the deletes that failed on the last pass, or `""`
    /// when it completed.
    pub last_prune_error: String,
}

impl MaintenanceState {
    /// Project the `impresspress__tickets__maintenance` singleton row.
    fn from_record(record: &db::Record) -> Self {
        use crate::util::RecordExt;

        Self {
            last_pruned_day: record.str_field("last_pruned_day").to_string(),
            last_pruned_at: match record.data.get("last_pruned_at") {
                Some(serde_json::Value::String(value)) => Some(value.clone()),
                _ => None,
            },
            last_prune_error: record.str_field("last_prune_error").to_string(),
        }
    }
}

/// Response body of `GET /b/tickets/api/admin/status`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct OperationalStatus {
    /// Whether protected public reporting is currently able to accept a
    /// submission, and what is missing when it is not.
    pub security: SecurityReadiness,
    /// Tickets still in the `"new"` state.
    pub new_tickets: i64,
    /// Tickets at `"urgent"` priority, in any state.
    pub urgent_tickets: i64,
    /// Tickets in `"new"`, `"triaged"` or `"investigating"`.
    pub open_tickets: i64,
    /// The last retention pass, or `null` when none has run.
    pub last_maintenance: Option<MaintenanceState>,
    /// Whether an audit-timeline write has failed since the flag was last
    /// cleared. While true, the timeline may be incomplete.
    pub audit_degraded: bool,
}

/// Database operations performed by a normal maintenance pass: three ticket-owned
/// expiry deletes, one auth rate-counter delete, and one singleton status write.
pub const STATEMENT_COUNT: usize = 5;

/// Run one retention pass as of `now`.
///
/// `now` is a parameter so a test can place the cutoffs against the rows it
/// seeds; the routes pass the current time.
pub async fn prune(ctx: &dyn Context, now: chrono::DateTime<chrono::Utc>) -> MaintenanceResult {
    let now_text = now.to_rfc3339();
    // The auth rate-counter cutoff is RFC 3339, the one timestamp text every
    // backend reads: Postgres binds it to the TIMESTAMPTZ `updated_at` column
    // (and refuses any other spelling), and SQLite compares `updated_at` as
    // text. On SQLite a row stamped by SQL `CURRENT_TIMESTAMP`
    // (`YYYY-MM-DD HH:MM:SS`, a space where RFC 3339 has `T`) sorts below
    // every cutoff of its own calendar date, so such a row can be swept up to
    // a day early — still at least 48 hours after its last write. Sweeping a
    // counter only resets it, and the declared windows (a minute to an hour,
    // unless an operator configures longer) end long before that.
    let rate_cutoff = (now - chrono::Duration::hours(72)).to_rfc3339();
    let mut result = MaintenanceResult {
        complete: true,
        analyses_deleted: 0,
        events_deleted: 0,
        tickets_deleted: 0,
        rate_counters_deleted: 0,
        errors: Vec::new(),
    };

    prune_table(
        ctx,
        repo::ANALYSES,
        &now_text,
        "analyses",
        &mut result.analyses_deleted,
        &mut result.errors,
    )
    .await;
    prune_table(
        ctx,
        repo::EVENTS,
        &now_text,
        "events",
        &mut result.events_deleted,
        &mut result.errors,
    )
    .await;
    prune_table(
        ctx,
        repo::TICKETS,
        &now_text,
        "tickets",
        &mut result.tickets_deleted,
        &mut result.errors,
    )
    .await;
    match crate::blocks::auth::repo::rate_limits::delete_updated_before(ctx, &rate_cutoff).await {
        Ok(count) => result.rate_counters_deleted = count,
        Err(error) => {
            tracing::warn!(error = %error, "ticket maintenance rate-counter prune failed");
            result.errors.push("rate-counters".into());
        }
    }
    result.complete = result.errors.is_empty();
    store_result(ctx, &result).await;
    result
}

pub async fn status(ctx: &dyn Context) -> Result<OperationalStatus, WaferError> {
    let security = SecurityReadiness::load(ctx).await?;
    let new_tickets = repo::count_tickets(ctx, vec![repo::eq("status", "new")]).await?;
    let urgent_tickets = repo::count_tickets(ctx, vec![repo::eq("priority", "urgent")]).await?;
    let open_tickets = repo::count_tickets(
        ctx,
        vec![wafer_block::db::Filter {
            field: "status".into(),
            operator: wafer_block::db::FilterOp::In,
            value: serde_json::json!(["new", "triaged", "investigating"]),
        }],
    )
    .await?;
    // No row yet is an instance that has never run maintenance; a read that
    // failed is not that, and must not report the audit trail as healthy.
    let stored = match db::get(ctx, repo::MAINTENANCE, "singleton").await {
        Ok(record) => Some(record),
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => None,
        Err(error) => return Err(error),
    };
    let audit_degraded = stored
        .as_ref()
        .is_some_and(|record| super::service::bool_field(record, "audit_degraded"));
    Ok(OperationalStatus {
        security,
        new_tickets,
        urgent_tickets,
        open_tickets,
        last_maintenance: stored.as_ref().map(MaintenanceState::from_record),
        audit_degraded,
    })
}

async fn prune_table(
    ctx: &dyn Context,
    table: &str,
    cutoff: &str,
    label: &str,
    count: &mut i64,
    errors: &mut Vec<String>,
) {
    match db::delete_by_filters_count(ctx, table, vec![repo::before("expires_at", cutoff)]).await {
        Ok(deleted) => *count = deleted,
        Err(error) => {
            tracing::warn!(table = label, error = %error, "ticket maintenance prune failed");
            errors.push(label.to_string());
        }
    }
}

async fn store_result(ctx: &dyn Context, result: &MaintenanceResult) {
    let now = chrono::Utc::now();
    let data = json_map(serde_json::json!({
        "last_pruned_day": now.format("%Y-%m-%d").to_string(),
        "last_pruned_at": now.to_rfc3339(),
        "last_prune_error": if result.complete { String::new() } else { result.errors.join(",") },
    }));
    if db::update(ctx, repo::MAINTENANCE, "singleton", data.clone())
        .await
        .is_err()
    {
        let mut create = data;
        create.insert("id".into(), serde_json::json!("singleton"));
        if let Err(error) = db::create(ctx, repo::MAINTENANCE, create).await {
            tracing::warn!(error = %error, "ticket maintenance status write failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{json, Value};
    use wafer_core::clients::database as db;

    use super::prune;
    use crate::{blocks::auth::repo::rate_limits, test_support::TestContext};

    async fn counter(ctx: &TestContext, id: &str, updated_at: &str) {
        let mut row: HashMap<String, Value> = HashMap::new();
        row.insert("id".into(), json!(id));
        row.insert("key".into(), json!(format!("{id}-key")));
        row.insert("count".into(), json!(1));
        row.insert("window_start".into(), json!(0));
        row.insert("created_at".into(), json!(updated_at));
        row.insert("updated_at".into(), json!(updated_at));
        db::create(ctx, rate_limits::TABLE, row)
            .await
            .expect("seed a rate counter");
    }

    /// The rate-counter cutoff is RFC 3339, which is what Postgres's
    /// TIMESTAMPTZ binding accepts and what an RFC 3339 `updated_at` compares
    /// against correctly as text. A counter written earlier on the cutoff's
    /// own calendar date is swept; a `YYYY-MM-DD HH:MM:SS` cutoff
    /// (`CURRENT_TIMESTAMP`'s spelling) sorts below that row, because `T`
    /// sorts above the space, and would keep it. A row stamped in that
    /// spelling on a later date stays either way, and so does a counter the
    /// real windowed upsert has just written.
    #[tokio::test]
    async fn the_rate_counter_sweep_cuts_off_in_rfc3339() {
        let ctx = TestContext::with_tickets().await;
        let wall_clock = chrono::Utc::now().timestamp();
        rate_limits::windowed_increment(&ctx, "just-written", "just-written-key", wall_clock, 0)
            .await
            .expect("the windowed upsert writes a counter");
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-25T12:00:00Z")
            .expect("a fixed instant")
            .with_timezone(&chrono::Utc);
        // The cutoff is 72 hours earlier: 2026-09-22T12:00:00+00:00.
        counter(&ctx, "old", "2026-09-20T00:00:00+00:00").await;
        counter(
            &ctx,
            "earlier-on-the-cutoff-date",
            "2026-09-22T06:00:00+00:00",
        )
        .await;
        counter(
            &ctx,
            "later-on-the-cutoff-date",
            "2026-09-22T18:00:00+00:00",
        )
        .await;
        counter(&ctx, "current-timestamp-spelling", "2026-09-24 09:00:00").await;

        let result = prune(&ctx, now).await;

        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.rate_counters_deleted, 2);
        let mut left: Vec<String> = crate::db_read::list_every(&ctx, rate_limits::TABLE, vec![])
            .await
            .expect("read the counters")
            .into_iter()
            .map(|row| row.id)
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "current-timestamp-spelling",
                "just-written",
                "later-on-the-cutoff-date"
            ]
        );
    }
}
