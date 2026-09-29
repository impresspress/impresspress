//! `impresspress__admin__request_logs`: one row per inbound request the
//! pipeline served, written best-effort from the response tail and read by
//! the admin logs, network and dashboard pages.
//!
//! The pipeline's row ([`NewRequestLog`], borrowed because it is built on
//! the hot path) and its [`NewRequestLog::to_data`] are the only writer; the
//! inline/queued switch stays in `pipeline.rs`, which hands a queued row's
//! [`TABLE`] and map to the request's `after_response` scope, for the
//! platform to write after the response. The readers own
//! the list and aggregate shapes the three pages used to build by hand, and
//! return typed rows and summaries so an alias column is spelled once.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::{
    db::{Filter, FilterOp, ListOptions, SortField},
    wire::database as wire,
};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::Page;
use crate::util::{daily_grouped, to_wire_filters, RecordExt};

pub const TABLE: &str = "impresspress__admin__request_logs";

/// The lowest status code a row counts as an error at: every 4xx and 5xx.
/// [`is_error_status`] and every reader's filter compare against it, and the
/// `status` column's label is derived from it, so a row's label, the
/// dashboard's error tiles, the network page's error column and the "Recent
/// Errors" card cannot disagree about one row.
/// [`RequestLogPolicy::Errors`](crate::pipeline::RequestLogPolicy::Errors)
/// (5xx only) is a separate rule, about which rows are stored at all.
pub const ERROR_STATUS_FLOOR: i64 = 400;

/// Whether a response with this status code is an error row.
pub fn is_error_status(status_code: i64) -> bool {
    status_code >= ERROR_STATUS_FLOOR
}

/// The `status` column's label for a status code: `ERROR` for an error row
/// ([`is_error_status`]), `OK` otherwise. Derived here, at the one place a
/// row is encoded, so no writer can hand-write a label that contradicts the
/// code. No reader selects on the label: it is a display column for the SQL
/// explorer, and rows written before it was derived can carry a label that
/// contradicts their code.
pub fn status_label(status_code: i64) -> &'static str {
    if is_error_status(status_code) {
        "ERROR"
    } else {
        "OK"
    }
}

/// One request-log row as the pipeline writes it. Bundled into a struct so
/// `write_request_log` stays a two-argument call (the row shape is shared by
/// the buffered response tail and the streamed-download branch). It carries
/// no label: [`to_data`](Self::to_data) derives the `status` column from
/// `status_code`.
pub struct NewRequestLog<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// The status code the client was served.
    pub status_code: i64,
    pub error_message: &'a str,
    pub duration_ms: i64,
    pub client_ip: &'a str,
    pub user_id: &'a str,
}

impl NewRequestLog<'_> {
    /// The column map this row inserts as. No `id`: the platform's `create`
    /// (and the Cloudflare drain's `create_many`) synthesises one, and the
    /// queued path must stay a plain map the drain can batch.
    pub fn to_data(&self) -> HashMap<String, Value> {
        let mut data = HashMap::new();
        data.insert("method".to_string(), json!(self.method));
        data.insert("path".to_string(), json!(self.path));
        data.insert("status".to_string(), json!(status_label(self.status_code)));
        data.insert("status_code".to_string(), json!(self.status_code));
        data.insert("duration_ms".to_string(), json!(self.duration_ms));
        data.insert("error_message".to_string(), json!(self.error_message));
        data.insert("client_ip".to_string(), json!(self.client_ip));
        data.insert("user_id".to_string(), json!(self.user_id));
        crate::util::stamp_created(&mut data);
        data
    }
}

/// One stored row. Every column defaults when absent: the readers project
/// only the columns a page renders, so a decoded row may carry empty
/// strings and zeros for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestLogRow {
    pub id: String,
    pub flow_id: String,
    pub method: String,
    pub path: String,
    pub status: String,
    pub status_code: i64,
    pub duration_ms: i64,
    pub error_message: String,
    pub client_ip: String,
    pub user_id: String,
    pub created_at: String,
    pub updated_at: String,
}

impl RequestLogRow {
    pub fn from_record(id: &str, data: &HashMap<String, Value>) -> Self {
        Self {
            id: id.to_string(),
            flow_id: data.str_field("flow_id").to_string(),
            method: data.str_field("method").to_string(),
            path: data.str_field("path").to_string(),
            status: data.str_field("status").to_string(),
            status_code: data.i64_field("status_code"),
            duration_ms: data.i64_field("duration_ms"),
            error_message: data.str_field("error_message").to_string(),
            client_ip: data.str_field("client_ip").to_string(),
            user_id: data.str_field("user_id").to_string(),
            created_at: data.str_field("created_at").to_string(),
            updated_at: data.str_field("updated_at").to_string(),
        }
    }
}

/// One `(method, path)` group of the network page's inbound summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSummary {
    pub method: String,
    pub path: String,
    pub count: i64,
    /// Mean duration truncated toward zero — `CAST(AVG(duration_ms) AS
    /// INTEGER)` parity with the builder path this replaced.
    pub avg_ms: i64,
    pub errors: i64,
    pub last_seen: String,
}

/// The dashboard's header tiles for the current day.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TodayCounts {
    pub requests: i64,
    pub errors: i64,
    pub avg_ms: f64,
}

/// One day of the dashboard's request and error series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyCounts {
    /// `YYYY-MM-DD`.
    pub day: String,
    pub requests: i64,
    pub errors: i64,
}

fn newest_first() -> Vec<SortField> {
    vec![SortField {
        field: "created_at".into(),
        desc: true,
    }]
}

/// The one error predicate every reader filters on: [`is_error_status`] as
/// a filter on the stored `status_code`.
fn is_error() -> Filter {
    Filter {
        field: "status_code".into(),
        operator: FilterOp::GreaterEqual,
        value: json!(ERROR_STATUS_FLOOR),
    }
}

fn since(iso: &str) -> Filter {
    Filter {
        field: "created_at".into(),
        operator: FilterOp::GreaterEqual,
        value: json!(iso),
    }
}

/// Write one row. Best-effort is the pipeline's decision, not this one's: a
/// failed write is returned.
pub async fn insert(ctx: &dyn Context, row: &NewRequestLog<'_>) -> Result<(), WaferError> {
    db::create(ctx, TABLE, row.to_data()).await.map(|_| ())
}

/// Page `page` of `page_size` rows, newest first, optionally narrowed to
/// paths containing `path_search` and, when `errors_only`, to error rows
/// ([`is_error_status`]). The admin logs page.
pub async fn paginated(
    ctx: &dyn Context,
    page: i64,
    page_size: i64,
    path_search: &str,
    errors_only: bool,
) -> Result<Page<RequestLogRow>, WaferError> {
    let mut filters = Vec::new();
    if !path_search.is_empty() {
        filters.push(Filter {
            field: "path".into(),
            operator: FilterOp::Like,
            value: json!(format!("%{path_search}%")),
        });
    }
    if errors_only {
        filters.push(is_error());
    }
    let list = db::paginated_list(ctx, TABLE, page, page_size, filters, newest_first()).await?;
    Ok(Page {
        rows: list
            .records
            .iter()
            .map(|r| RequestLogRow::from_record(&r.id, &r.data))
            .collect(),
        total_count: list.total_count,
        page: list.page,
        page_size: list.page_size,
    })
}

/// The `limit` most recent error rows ([`is_error_status`]). The dashboard's
/// "Recent Errors" card.
pub async fn list_recent_errors(
    ctx: &dyn Context,
    limit: u32,
) -> Result<Vec<RequestLogRow>, WaferError> {
    let opts = ListOptions {
        columns: Some(vec![
            "id".into(),
            "status_code".into(),
            "method".into(),
            "path".into(),
            "duration_ms".into(),
            "created_at".into(),
        ]),
        filters: vec![is_error()],
        sort: newest_first(),
        limit: Some(limit),
        skip_count: true,
        ..Default::default()
    };
    let list = db::list(ctx, TABLE, &opts).await?;
    Ok(list
        .records
        .iter()
        .map(|r| RequestLogRow::from_record(&r.id, &r.data))
        .collect())
}

/// Rows for one `(method, path)`, newest first, from `offset`, at most
/// `limit`. The network page's expandable detail (which asks for one more
/// than it shows to learn whether a next page exists).
pub async fn list_for_path(
    ctx: &dyn Context,
    method: &str,
    path: &str,
    offset: i64,
    limit: u32,
) -> Result<Vec<RequestLogRow>, WaferError> {
    let opts = ListOptions {
        columns: Some(vec![
            "id".into(),
            "status_code".into(),
            "duration_ms".into(),
            "client_ip".into(),
            "user_id".into(),
            "created_at".into(),
        ]),
        filters: vec![
            Filter {
                field: "method".into(),
                operator: FilterOp::Equal,
                value: json!(method),
            },
            Filter {
                field: "path".into(),
                operator: FilterOp::Equal,
                value: json!(path),
            },
        ],
        sort: newest_first(),
        limit: Some(limit),
        offset,
        skip_count: true,
        ..Default::default()
    };
    let list = db::list(ctx, TABLE, &opts).await?;
    Ok(list
        .records
        .iter()
        .map(|r| RequestLogRow::from_record(&r.id, &r.data))
        .collect())
}

/// The `limit` busiest `(method, path)` groups, optionally narrowed to paths
/// containing `path_search`: request count, mean duration, error count and
/// the newest timestamp of each. The network page's inbound summary, in one
/// grouped statement.
pub async fn summarise_by_path(
    ctx: &dyn Context,
    path_search: &str,
    limit: i64,
) -> Result<Vec<PathSummary>, WaferError> {
    let filters = if path_search.is_empty() {
        vec![]
    } else {
        vec![wire::FilterNode::Leaf(wire::FilterDef {
            field: "path".into(),
            operator: "like".into(),
            value: json!(format!("%{path_search}%")),
            column: None,
        })]
    };
    let req = wire::AggregateRequest {
        collection: TABLE.to_string(),
        select_columns: vec!["method".into(), "path".into()],
        aggregates: vec![
            wire::AggregateColumnDef::Count {
                alias: "cnt".into(),
            },
            wire::AggregateColumnDef::Avg {
                field: "duration_ms".into(),
                alias: "avg_ms".into(),
                cast_as: None,
            },
            wire::AggregateColumnDef::CaseWhenSum {
                when: to_wire_filters(&[is_error()]),
                alias: "errors".into(),
            },
            wire::AggregateColumnDef::Max {
                field: "created_at".into(),
                alias: "last_seen".into(),
            },
        ],
        filters,
        group_by: vec![
            wire::GroupByDef::Column("method".into()),
            wire::GroupByDef::Column("path".into()),
        ],
        sort: vec![wire::SortFieldDef {
            field: "cnt".into(),
            desc: true,
        }],
        limit,
    };
    let rows = db::aggregate(ctx, req).await?;
    Ok(rows
        .iter()
        .map(|r| PathSummary {
            method: r.data.str_field("method").to_string(),
            path: r.data.str_field("path").to_string(),
            count: r.data.i64_field("cnt"),
            // The Avg is requested uncast, so AVG(duration_ms) comes back as
            // a JSON float; `as_i64()` is always `None` for the
            // `Number::Float` variant, so read it as f64 and truncate. A
            // `BIGINT` cast would round on PostgreSQL and truncate on SQLite;
            // truncating here gives one answer on every backend, and
            // `duration_ms` is always >= 0, so `as i64` (which truncates
            // toward zero) needs no `.round()`.
            avg_ms: r
                .data
                .get("avg_ms")
                .and_then(|v| v.as_f64())
                .map(|v| v as i64)
                .unwrap_or(0),
            errors: r.data.i64_field("errors"),
            last_seen: r.data.str_field("last_seen").to_string(),
        })
        .collect())
}

/// Requests, errors ([`is_error_status`]) and mean duration since `since` (an
/// ISO timestamp, the start of today) in one statement. The dashboard's
/// header tiles.
pub async fn today_counts(ctx: &dyn Context, since_iso: &str) -> Result<TodayCounts, WaferError> {
    let req = wire::AggregateRequest {
        collection: TABLE.to_string(),
        select_columns: vec![],
        aggregates: vec![
            wire::AggregateColumnDef::Count {
                alias: "requests".into(),
            },
            wire::AggregateColumnDef::CaseWhenSum {
                when: to_wire_filters(&[is_error()]),
                alias: "errors".into(),
            },
            wire::AggregateColumnDef::Avg {
                field: "duration_ms".into(),
                alias: "avg_val".into(),
                cast_as: None,
            },
        ],
        filters: to_wire_filters(&[since(since_iso)]),
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    let rows = db::aggregate(ctx, req).await?;
    let row = rows.first();
    Ok(TodayCounts {
        requests: row.map(|r| r.data.i64_field("requests")).unwrap_or(0),
        errors: row.map(|r| r.data.i64_field("errors")).unwrap_or(0),
        avg_ms: row
            .and_then(|r| r.data.get("avg_val"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
    })
}

/// Requests and errors ([`is_error_status`]) per day since `since` (one
/// entry per day that has rows), from one grouped statement. The dashboard's
/// request and error series come from the same rows.
pub async fn daily_counts(
    ctx: &dyn Context,
    since_iso: &str,
) -> Result<Vec<DailyCounts>, WaferError> {
    let rows = daily_grouped(
        ctx,
        TABLE,
        since_iso,
        vec![],
        vec![
            wire::AggregateColumnDef::Count {
                alias: "requests".into(),
            },
            wire::AggregateColumnDef::CaseWhenSum {
                when: to_wire_filters(&[is_error()]),
                alias: "errors".into(),
            },
        ],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| DailyCounts {
            day: r.data.str_field("created_at").to_string(),
            requests: r.data.i64_field("requests"),
            errors: r.data.i64_field("errors"),
        })
        .collect())
}
#[cfg(test)]
mod tests {
    use wafer_core::clients::database as db;

    use super::*;
    use crate::test_support::{FailingDbOpContext, TestContext};

    fn probe(status_code: i64, duration_ms: i64) -> NewRequestLog<'static> {
        NewRequestLog {
            method: "GET",
            path: "/probe",
            status_code,
            error_message: if is_error_status(status_code) {
                "boom"
            } else {
                ""
            },
            duration_ms,
            client_ip: "203.0.113.7",
            user_id: "u-1",
        }
    }

    /// Seed a row at a chosen time, the way a fixture must: through the
    /// codec, then pinning `id`/`created_at` on the map the owning module
    /// spells.
    async fn seed_at(ctx: &TestContext, id: &str, row: NewRequestLog<'_>, at: &str) {
        let mut data = row.to_data();
        data.insert("id".to_string(), serde_json::json!(id));
        data.insert("created_at".to_string(), serde_json::json!(at));
        data.insert("updated_at".to_string(), serde_json::json!(at));
        db::create(ctx, TABLE, data)
            .await
            .unwrap_or_else(|e| panic!("seed request_log {id}: {e}"));
    }

    /// Seed a row whose stored `status` label is the caller's rather than the
    /// one [`status_label`] derives — the shape a writer from before the
    /// label was derived left behind, and the only way to tell a
    /// code-reading filter from a label-reading one.
    async fn seed_labelled(ctx: &TestContext, id: &str, code: i64, label: &str, at: &str) {
        let mut data = probe(code, 10).to_data();
        data.insert("id".to_string(), serde_json::json!(id));
        data.insert("status".to_string(), serde_json::json!(label));
        data.insert("created_at".to_string(), serde_json::json!(at));
        data.insert("updated_at".to_string(), serde_json::json!(at));
        db::create(ctx, TABLE, data)
            .await
            .unwrap_or_else(|e| panic!("seed request_log {id}: {e}"));
    }

    /// The codec: every column `insert` writes comes back through
    /// `paginated`, integers as integers and strings as strings.
    #[tokio::test]
    async fn insert_and_paginated_round_trip_every_column() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        insert(&ctx, &probe(500, 42)).await.expect("insert");

        let page = paginated(&ctx, 1, 20, "", false).await.expect("paginated");
        assert_eq!(page.total_count, 1);
        assert_eq!((page.page, page.page_size), (1, 20));
        let row = &page.rows[0];
        assert!(!row.id.is_empty());
        assert_eq!(row.flow_id, "", "the pipeline writes no flow id");
        assert_eq!(row.method, "GET");
        assert_eq!(row.path, "/probe");
        assert_eq!(row.status, "ERROR");
        assert_eq!(row.status_code, 500);
        assert_eq!(row.duration_ms, 42);
        assert_eq!(row.error_message, "boom");
        assert_eq!(row.client_ip, "203.0.113.7");
        assert_eq!(row.user_id, "u-1");
        assert!(!row.created_at.is_empty());
        assert_eq!(row.created_at, row.updated_at);

        let again = RequestLogRow::from_record(&row.id, &probe(500, 42).to_data());
        assert_eq!(again.status_code, 500);
        assert_eq!(again.method, "GET");
    }

    /// A write failure is reported to the pipeline, which decides (today:
    /// best-effort) what to do with it.
    #[tokio::test]
    async fn insert_surfaces_write_errors() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let failing = FailingDbOpContext::new(ctx, vec![("database.create", TABLE)]);
        assert!(insert(&failing, &probe(200, 1)).await.is_err());
    }

    #[tokio::test]
    async fn paginated_filters_on_the_path_and_pages_newest_first() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed_at(&ctx, "r1", probe(200, 1), "2026-01-01T00:00:00Z").await;
        seed_at(&ctx, "r2", probe(200, 1), "2026-01-02T00:00:00Z").await;
        let mut other = probe(200, 1);
        other.path = "/other";
        seed_at(&ctx, "r3", other, "2026-01-03T00:00:00Z").await;

        let page = paginated(&ctx, 1, 1, "", false).await.expect("page 1");
        assert_eq!(page.total_count, 3);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].id, "r3", "newest first");

        let probes = paginated(&ctx, 1, 20, "prob", false)
            .await
            .expect("filtered");
        assert_eq!(probes.total_count, 2);
        assert!(probes.rows.iter().all(|r| r.path == "/probe"));
    }

    /// `errors_only` narrows the list to error rows by their `status_code`,
    /// composes with the path search, and counts the narrowed set.
    #[tokio::test]
    async fn paginated_errors_only_selects_by_code_not_by_the_stored_label() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed_labelled(&ctx, "served_200", 200, "OK", "2026-01-01T00:00:00Z").await;
        seed_labelled(
            &ctx,
            "served_404_labelled_ok",
            404,
            "OK",
            "2026-01-02T00:00:00Z",
        )
        .await;
        seed_labelled(
            &ctx,
            "served_302_labelled_error",
            302,
            "ERROR",
            "2026-01-03T00:00:00Z",
        )
        .await;

        let all = paginated(&ctx, 1, 20, "", false).await.expect("all rows");
        assert_eq!(all.total_count, 3);

        let errors = paginated(&ctx, 1, 20, "", true).await.expect("error rows");
        assert_eq!(
            errors
                .rows
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["served_404_labelled_ok"],
            "only the row whose code is >= 400, whatever its label says",
        );
        assert_eq!(errors.total_count, 1, "the count is of the narrowed set");

        // The two filters compose: same rows, narrowed by path as well.
        assert_eq!(
            paginated(&ctx, 1, 20, "prob", true)
                .await
                .expect("error rows on /probe")
                .total_count,
            1
        );
        assert_eq!(
            paginated(&ctx, 1, 20, "nope", true)
                .await
                .expect("error rows on a path that matches nothing")
                .total_count,
            0
        );
    }

    #[tokio::test]
    async fn list_for_path_pages_one_path_newest_first() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed_at(&ctx, "r1", probe(200, 1), "2026-01-01T00:00:00Z").await;
        seed_at(&ctx, "r2", probe(200, 2), "2026-01-02T00:00:00Z").await;
        let mut other = probe(200, 3);
        other.path = "/other";
        seed_at(&ctx, "r3", other, "2026-01-03T00:00:00Z").await;

        let first = list_for_path(&ctx, "GET", "/probe", 0, 1)
            .await
            .expect("list");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, "r2");
        let rest = list_for_path(&ctx, "GET", "/probe", 1, 10)
            .await
            .expect("list");
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, "r1");
    }

    /// The aggregates the admin dashboard and network page render equal the
    /// numbers separate `db::count` calls produce over the same rows, and
    /// hand-computed expectations for the fixed seed.
    #[tokio::test]
    async fn aggregates_match_per_filter_counts() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let today = chrono::Utc::now().date_naive();
        // Noon timestamps so a stored `...T12:00:00` sorts after `today_start`
        // (`...T00:00:00`) yet buckets to the same day under SQLite's `date()`.
        let at = |ago: i64| {
            (today - chrono::Duration::days(ago))
                .format("%Y-%m-%dT12:00:00")
                .to_string()
        };
        let day = |ago: i64| {
            (today - chrono::Duration::days(ago))
                .format("%Y-%m-%d")
                .to_string()
        };
        let today_start = format!("{}T00:00:00", today.format("%Y-%m-%d"));
        let start_30d = format!(
            "{}T00:00:00",
            (today - chrono::Duration::days(29)).format("%Y-%m-%d")
        );

        // Today 4 (durations 100/200/300/400, one 500); 10d ago 2 (ok,
        // 50/50); 40d ago 5 (outside the 30-day window).
        seed_at(&ctx, "r_t0", probe(200, 100), &at(0)).await;
        seed_at(&ctx, "r_t1", probe(200, 200), &at(0)).await;
        seed_at(&ctx, "r_t2", probe(200, 300), &at(0)).await;
        seed_at(&ctx, "r_t3", probe(500, 400), &at(0)).await;
        seed_at(&ctx, "r_10d_0", probe(200, 50), &at(10)).await;
        seed_at(&ctx, "r_10d_1", probe(200, 50), &at(10)).await;
        for i in 0..5 {
            seed_at(&ctx, &format!("r_40d_{i}"), probe(200, 999), &at(40)).await;
        }

        // --- today's tile counts vs. separate per-filter counts ---
        let today_filter = Filter {
            field: "created_at".into(),
            operator: FilterOp::GreaterEqual,
            value: serde_json::json!(&today_start),
        };
        let error_filter = Filter {
            field: "status_code".into(),
            operator: FilterOp::GreaterEqual,
            value: serde_json::json!(400),
        };
        let requests_expected = db::count(&ctx, TABLE, std::slice::from_ref(&today_filter))
            .await
            .unwrap();
        let errors_expected = db::count(&ctx, TABLE, &[error_filter, today_filter])
            .await
            .unwrap();
        let counts = today_counts(&ctx, &today_start)
            .await
            .expect("today_counts");
        assert_eq!(counts.requests, requests_expected);
        assert_eq!(counts.errors, errors_expected);
        assert_eq!((counts.requests, counts.errors), (4, 1), "hand-computed");
        assert!(
            (counts.avg_ms - 250.0).abs() < 1e-9,
            "avg of today's durations = 250, got {}",
            counts.avg_ms
        );

        // --- the daily series ---
        let daily = daily_counts(&ctx, &start_30d).await.expect("daily_counts");
        let on = |d: &str| daily.iter().find(|row| row.day == d);
        assert_eq!(on(&day(0)).map(|r| (r.requests, r.errors)), Some((4, 1)));
        assert_eq!(on(&day(10)).map(|r| (r.requests, r.errors)), Some((2, 0)));
        assert_eq!(
            daily.iter().map(|r| r.requests).sum::<i64>(),
            6,
            "40d-ago excluded"
        );
        assert_eq!(daily.iter().map(|r| r.errors).sum::<i64>(), 1);

        // --- the per-path summary (every row shares one method+path) ---
        let summary = summarise_by_path(&ctx, "", 50).await.expect("summary");
        assert_eq!(summary.len(), 1);
        let s = &summary[0];
        assert_eq!((s.method.as_str(), s.path.as_str()), ("GET", "/probe"));
        assert_eq!(s.count, 11);
        assert_eq!(s.errors, 1);
        assert_eq!(s.last_seen, at(0));
        // CAST(AVG(duration_ms) AS INTEGER) parity: the eleven durations sum
        // to 6095, a mean of 554.09…, truncated toward zero.
        assert_eq!(s.avg_ms, 554);
        assert!(summarise_by_path(&ctx, "nope", 50)
            .await
            .expect("filtered summary")
            .is_empty());

        // --- recent errors: the 500 row and nothing else ---
        let recent = list_recent_errors(&ctx, 5).await.expect("recent errors");
        assert_eq!(
            recent.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["r_t3"]
        );
    }

    /// Every reader counts a row by its `status_code`, whatever its stored
    /// label says. Rows the pipeline wrote before the label was derived carry
    /// the code the client was served beside a hand-written label: a buffered
    /// 500 page labelled `OK`, and an error answered with a sub-400 override
    /// labelled `ERROR`. Reading the code classifies both by the response
    /// that was actually sent, with no backfill.
    #[tokio::test]
    async fn every_reader_classifies_a_mislabelled_row_by_its_code() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let today = chrono::Utc::now().date_naive();
        let at = format!("{}T12:00:00", today.format("%Y-%m-%d"));
        let today_start = format!("{}T00:00:00", today.format("%Y-%m-%d"));

        // Two errors stored as OK and one non-error stored as ERROR, so a
        // label-reading count (1) cannot coincide with the code-reading one.
        seed_labelled(&ctx, "served_500_labelled_ok", 500, "OK", &at).await;
        seed_labelled(&ctx, "served_404_labelled_ok", 404, "OK", &at).await;
        seed_labelled(&ctx, "served_302_labelled_error", 302, "ERROR", &at).await;

        let counts = today_counts(&ctx, &today_start)
            .await
            .expect("today_counts");
        assert_eq!((counts.requests, counts.errors), (3, 2), "today_counts");

        let daily = daily_counts(&ctx, &today_start)
            .await
            .expect("daily_counts");
        assert_eq!(
            daily
                .iter()
                .map(|r| (r.requests, r.errors))
                .collect::<Vec<_>>(),
            vec![(3, 2)],
            "daily_counts",
        );

        let summary = summarise_by_path(&ctx, "", 50).await.expect("summary");
        assert_eq!(
            summary
                .iter()
                .map(|s| (s.count, s.errors))
                .collect::<Vec<_>>(),
            vec![(3, 2)],
            "summarise_by_path",
        );

        let mut recent: Vec<String> = list_recent_errors(&ctx, 5)
            .await
            .expect("recent errors")
            .into_iter()
            .map(|r| r.id)
            .collect();
        recent.sort();
        assert_eq!(
            recent,
            vec!["served_404_labelled_ok", "served_500_labelled_ok"],
            "list_recent_errors",
        );
    }

    /// The label is a function of the code alone.
    #[test]
    fn the_stored_label_is_derived_from_the_code() {
        for (code, label) in [
            (200, "OK"),
            (302, "OK"),
            (399, "OK"),
            (400, "ERROR"),
            (500, "ERROR"),
        ] {
            assert_eq!(
                probe(code, 1).to_data().get("status"),
                Some(&serde_json::json!(label)),
                "{code}",
            );
        }
    }
}
