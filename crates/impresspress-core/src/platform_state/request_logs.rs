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
use wafer_core::clients::database::{self as db, RecordData};
use wafer_run::{context::Context, WaferError};

use super::Page;
use crate::util::{daily_grouped, to_wire_filters, RecordExt};

pub const TABLE: &str = "impresspress__admin__request_logs";

/// The lowest client-error status code: 400.
pub const CLIENT_ERROR_FLOOR: i64 = 400;

/// The lowest server-error status code: 500.
pub const SERVER_ERROR_FLOOR: i64 = 500;

/// `429 Too Many Requests`: the client error the dashboard counts on its own,
/// because a run of them is a rate limiter turning someone away rather than a
/// broken link.
pub const TOO_MANY_REQUESTS: i64 = 429;

/// Which side of the exchange a logged status code blames.
///
/// Every reader classifies a row by its stored `status_code` through this one
/// rule — the label column, the dashboard's tiles and series, the network
/// page's counts and the logs page's filters — so no two of them can disagree
/// about one row. A server error (5xx) is this deployment failing; a client
/// error (4xx) is a request it refused, mostly a mistyped or probing URL, a
/// missing credential or a rate limit.
/// [`RequestLogPolicy::Errors`](crate::pipeline::RequestLogPolicy::Errors)
/// keeps server errors only, and is a separate rule about which rows are
/// stored at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusClass {
    /// Below 400: served.
    Served,
    /// 400–499: refused.
    ClientError,
    /// 500 and up: failed.
    ServerError,
}

impl StatusClass {
    pub fn of(status_code: i64) -> Self {
        if status_code >= SERVER_ERROR_FLOOR {
            Self::ServerError
        } else if status_code >= CLIENT_ERROR_FLOOR {
            Self::ClientError
        } else {
            Self::Served
        }
    }

    /// Whether the row is an error of either kind.
    pub fn is_error(self) -> bool {
        self != Self::Served
    }

    /// The class as filters on the stored `status_code`, AND-combined.
    fn filters(self) -> Vec<Filter> {
        match self {
            Self::Served => vec![code(FilterOp::LessThan, CLIENT_ERROR_FLOOR)],
            Self::ClientError => vec![
                code(FilterOp::GreaterEqual, CLIENT_ERROR_FLOOR),
                code(FilterOp::LessThan, SERVER_ERROR_FLOOR),
            ],
            Self::ServerError => vec![code(FilterOp::GreaterEqual, SERVER_ERROR_FLOOR)],
        }
    }
}

/// Which error rows a list is narrowed to: the logs page's two filters,
/// "Server errors" and "Client errors", each on or off. Both off is every
/// row, not none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ErrorFilter {
    pub server: bool,
    pub client: bool,
}

impl ErrorFilter {
    /// Every row.
    pub const NONE: Self = Self {
        server: false,
        client: false,
    };

    /// The filters on `status_code` this selection reads as. Both classes
    /// together are one contiguous range (4xx and 5xx), so one filter.
    fn filters(self) -> Vec<Filter> {
        match (self.server, self.client) {
            (false, false) => vec![],
            (true, false) => StatusClass::ServerError.filters(),
            (false, true) => StatusClass::ClientError.filters(),
            (true, true) => vec![code(FilterOp::GreaterEqual, CLIENT_ERROR_FLOOR)],
        }
    }
}

fn code(operator: FilterOp, value: i64) -> Filter {
    Filter {
        field: "status_code".into(),
        operator,
        value: json!(value),
    }
}

/// The `status` column's label for a status code: `ERROR` for an error row of
/// either class ([`StatusClass::is_error`]), `OK` otherwise. Derived here, at the one place a
/// row is encoded, so no writer can hand-write a label that contradicts the
/// code. No reader selects on the label: it is a display column for the SQL
/// explorer, and rows written before it was derived can carry a label that
/// contradicts their code.
pub fn status_label(status_code: i64) -> &'static str {
    if StatusClass::of(status_code).is_error() {
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
    /// The HTTP method the client sent (`GET`, `POST`, …), as the request
    /// head spelled it.
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
        data.insert("block".to_string(), json!(owning_block(self.path)));
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

/// The block a request path was addressed to: the `{block}` of
/// `/b/{block}/…` (or of a bare `/b/{block}`), which is how the router hands
/// a request to a block; `""` for any other path — `/`, a static asset, the
/// unmatched-route label. Derived here, where a row is encoded, so the
/// `block` column cannot disagree with the path beside it; admin migration
/// 008 gave the rows written before the column the same value.
pub fn owning_block(path: &str) -> &str {
    path.strip_prefix("/b/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
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
    pub block: String,
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
    pub fn from_record(id: &str, data: &RecordData) -> Self {
        Self {
            id: id.to_string(),
            flow_id: data.str_field("flow_id").to_string(),
            method: data.str_field("method").to_string(),
            path: data.str_field("path").to_string(),
            block: data.str_field("block").to_string(),
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

/// One `(block, method, path)` group of the network page's route listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSummary {
    pub block: String,
    pub method: String,
    pub path: String,
    pub count: i64,
    /// Mean duration truncated toward zero — `CAST(AVG(duration_ms) AS
    /// INTEGER)` parity with the builder path this replaced.
    pub avg_ms: i64,
    pub server_errors: i64,
    pub client_errors: i64,
    pub last_seen: String,
}

/// How the network page orders the routes within each block, each
/// descending (ties by path, then method).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSort {
    /// Busiest first.
    Requests,
    /// Most errors (of either class) first.
    Errors,
    /// Most recently seen first.
    Recent,
}

impl PathSort {
    /// The aggregate alias [`route_page`] sorts on.
    fn alias(self) -> &'static str {
        match self {
            Self::Requests => "cnt",
            Self::Errors => "errors",
            Self::Recent => "last_seen",
        }
    }
}

/// Which routes the network page lists, and which page of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteQuery<'a> {
    /// Narrow to paths containing this; empty for every path.
    pub search: &'a str,
    pub sort: PathSort,
    /// Only routes that answered at least one error (of either class).
    pub errors_only: bool,
    /// Routes per page, at least 1.
    pub limit: u32,
    /// Routes to skip before this page.
    pub offset: i64,
}

/// One page of routes, and how many routes the query matched in all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePage {
    /// Ordered by block, then by the query's sort.
    pub rows: Vec<PathSummary>,
    /// Every route the query matched, across all pages. `0` when the page is
    /// empty: a page past the last route carries no count.
    pub total: i64,
}

/// One block's totals over every route of it the query's search matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockTotals {
    /// `""` for the paths no block owns.
    pub block: String,
    pub requests: i64,
    pub server_errors: i64,
    pub client_errors: i64,
    pub last_seen: String,
}

/// The dashboard's header tiles for the current day.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TodayCounts {
    pub requests: i64,
    pub server_errors: i64,
    pub client_errors: i64,
    /// Of `client_errors`, the [`TOO_MANY_REQUESTS`] rows.
    pub rate_limited: i64,
    pub avg_ms: f64,
}

/// One day of the dashboard's request and error series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyCounts {
    /// `YYYY-MM-DD`.
    pub day: String,
    pub requests: i64,
    pub server_errors: i64,
    pub client_errors: i64,
}

fn newest_first() -> Vec<SortField> {
    vec![SortField {
        field: "created_at".into(),
        desc: true,
    }]
}

/// A conditional count of the rows of one status class.
fn count_of(class: StatusClass, alias: &str) -> wire::AggregateColumnDef {
    wire::AggregateColumnDef::CaseWhenSum {
        when: to_wire_filters(&class.filters()),
        alias: alias.into(),
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
/// paths containing `path_search` and to the error classes `errors` selects.
/// The admin logs page.
pub async fn paginated(
    ctx: &dyn Context,
    page: i64,
    page_size: i64,
    path_search: &str,
    errors: ErrorFilter,
) -> Result<Page<RequestLogRow>, WaferError> {
    let mut filters = Vec::new();
    if !path_search.is_empty() {
        filters.push(Filter {
            field: "path".into(),
            operator: FilterOp::Like,
            value: json!(format!("%{path_search}%")),
        });
    }
    filters.extend(errors.filters());
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

/// The `limit` most recent server-error rows ([`StatusClass::ServerError`]).
/// The dashboard's "Recent server errors" card.
pub async fn list_recent_server_errors(
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
        filters: StatusClass::ServerError.filters(),
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

/// [`insert`] with a chosen `created_at` (and `updated_at`): a fixture for
/// tests that need rows from a past day, which `insert` cannot write.
#[cfg(test)]
pub(crate) async fn insert_at(
    ctx: &dyn Context,
    id: &str,
    row: &NewRequestLog<'_>,
    at: &str,
) -> Result<(), WaferError> {
    let mut data = row.to_data();
    data.insert("id".to_string(), json!(id));
    data.insert("created_at".to_string(), json!(at));
    data.insert("updated_at".to_string(), json!(at));
    db::create(ctx, TABLE, data).await.map(|_| ())
}

/// When the oldest stored request was logged (its `created_at`), or `None`
/// when the log is empty: how far back the dashboard's request and error
/// series actually go. One row, ascending, no count.
pub async fn first_logged_at(ctx: &dyn Context) -> Result<Option<String>, WaferError> {
    let opts = ListOptions {
        columns: Some(vec!["created_at".into()]),
        sort: vec![SortField {
            field: "created_at".into(),
            desc: false,
        }],
        limit: Some(1),
        skip_count: true,
        ..Default::default()
    };
    let list = db::list(ctx, TABLE, &opts).await?;
    Ok(list
        .records
        .first()
        .map(|r| r.data.str_field("created_at").to_string()))
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

/// The `path LIKE %search%` filter, or nothing for an empty search.
fn path_search(search: &str) -> Vec<wire::FilterNode> {
    if search.is_empty() {
        return vec![];
    }
    to_wire_filters(&[Filter {
        field: "path".into(),
        operator: FilterOp::Like,
        value: json!(format!("%{search}%")),
    }])
}

/// The per-route counts both route reads select.
fn route_counts() -> Vec<wire::AggregateColumnDef> {
    vec![
        wire::AggregateColumnDef::Count {
            alias: "cnt".into(),
        },
        count_of(StatusClass::ServerError, "server_errors"),
        count_of(StatusClass::ClientError, "client_errors"),
        wire::AggregateColumnDef::CaseWhenSum {
            when: to_wire_filters(
                &ErrorFilter {
                    server: true,
                    client: true,
                }
                .filters(),
            ),
            alias: "errors".into(),
        },
        wire::AggregateColumnDef::Max {
            field: "created_at".into(),
            alias: "last_seen".into(),
        },
    ]
}

/// One page of `(block, method, path)` groups — request count, mean
/// duration, server- and client-error counts and the newest timestamp of
/// each — ordered by block and then by `query.sort`, with the number of
/// groups the query matched, in one grouped statement. The network page's
/// route listing.
///
/// Grouping, ordering, "errors only" (a `HAVING` on the error count) and the
/// page cut all happen in SQL, so a failing route is found however far down
/// a busy deployment's routes it ranks.
pub async fn route_page(ctx: &dyn Context, query: RouteQuery<'_>) -> Result<RoutePage, WaferError> {
    let mut aggregates = route_counts();
    aggregates.push(wire::AggregateColumnDef::Avg {
        field: "duration_ms".into(),
        alias: "avg_ms".into(),
        cast_as: None,
    });
    aggregates.push(wire::AggregateColumnDef::CountGroups {
        alias: "total".into(),
    });
    let having = if query.errors_only {
        to_wire_filters(&[Filter {
            field: "errors".into(),
            operator: FilterOp::GreaterThan,
            value: json!(0),
        }])
    } else {
        vec![]
    };
    let sort = |field: &str, desc: bool| wire::SortFieldDef {
        field: field.into(),
        desc,
    };
    let req = wire::AggregateRequest {
        collection: TABLE.to_string(),
        select_columns: vec!["block".into(), "method".into(), "path".into()],
        aggregates,
        filters: path_search(query.search),
        group_by: vec![
            wire::GroupByDef::Column("block".into()),
            wire::GroupByDef::Column("method".into()),
            wire::GroupByDef::Column("path".into()),
        ],
        sort: vec![
            sort("block", false),
            sort(query.sort.alias(), true),
            sort("path", false),
            sort("method", false),
        ],
        limit: i64::from(query.limit.max(1)),
        having,
        offset: query.offset.max(0),
    };
    let rows = db::aggregate(ctx, req).await?;
    Ok(RoutePage {
        total: rows.first().map_or(0, |r| r.data.i64_field("total")),
        rows: rows
            .iter()
            .map(|r| PathSummary {
                block: r.data.str_field("block").to_string(),
                method: r.data.str_field("method").to_string(),
                path: r.data.str_field("path").to_string(),
                count: r.data.i64_field("cnt"),
                // The Avg is requested uncast, so AVG(duration_ms) comes back
                // as a JSON float; `as_i64()` is always `None` for the
                // `Number::Float` variant, so read it as f64 and truncate. A
                // `BIGINT` cast would round on PostgreSQL and truncate on
                // SQLite; truncating here gives one answer on every backend,
                // and `duration_ms` is always >= 0, so `as i64` (which
                // truncates toward zero) needs no `.round()`.
                avg_ms: r
                    .data
                    .get("avg_ms")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as i64)
                    .unwrap_or(0),
                server_errors: r.data.i64_field("server_errors"),
                client_errors: r.data.i64_field("client_errors"),
                last_seen: r.data.str_field("last_seen").to_string(),
            })
            .collect(),
    })
}

/// Each block's totals over its rows whose path contains `search`: requests,
/// server and client errors, and the newest timestamp. The network page's
/// group headings, in one grouped statement (one row per block, so bounded
/// by the blocks a deployment runs).
pub async fn block_totals(ctx: &dyn Context, search: &str) -> Result<Vec<BlockTotals>, WaferError> {
    let req = wire::AggregateRequest {
        collection: TABLE.to_string(),
        select_columns: vec!["block".into()],
        aggregates: route_counts(),
        filters: path_search(search),
        group_by: vec![wire::GroupByDef::Column("block".into())],
        sort: vec![wire::SortFieldDef {
            field: "block".into(),
            desc: false,
        }],
        limit: 0,
        having: vec![],
        offset: 0,
    };
    let rows = db::aggregate(ctx, req).await?;
    Ok(rows
        .iter()
        .map(|r| BlockTotals {
            block: r.data.str_field("block").to_string(),
            requests: r.data.i64_field("cnt"),
            server_errors: r.data.i64_field("server_errors"),
            client_errors: r.data.i64_field("client_errors"),
            last_seen: r.data.str_field("last_seen").to_string(),
        })
        .collect())
}

/// Requests, server errors, client errors (and of those the rate-limited
/// ones) and mean duration since `since` (an ISO timestamp, the start of
/// today) in one statement. The dashboard's header tiles.
pub async fn today_counts(ctx: &dyn Context, since_iso: &str) -> Result<TodayCounts, WaferError> {
    let req = wire::AggregateRequest {
        collection: TABLE.to_string(),
        select_columns: vec![],
        aggregates: vec![
            wire::AggregateColumnDef::Count {
                alias: "requests".into(),
            },
            count_of(StatusClass::ServerError, "server_errors"),
            count_of(StatusClass::ClientError, "client_errors"),
            wire::AggregateColumnDef::CaseWhenSum {
                when: to_wire_filters(&[code(FilterOp::Equal, TOO_MANY_REQUESTS)]),
                alias: "rate_limited".into(),
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
        having: vec![],
        offset: 0,
    };
    let rows = db::aggregate(ctx, req).await?;
    let row = rows.first();
    let field = |name: &str| row.map(|r| r.data.i64_field(name)).unwrap_or(0);
    Ok(TodayCounts {
        requests: field("requests"),
        server_errors: field("server_errors"),
        client_errors: field("client_errors"),
        rate_limited: field("rate_limited"),
        avg_ms: row
            .and_then(|r| r.data.get("avg_val"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
    })
}

/// Requests, server errors and client errors per day since `since` (one
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
            count_of(StatusClass::ServerError, "server_errors"),
            count_of(StatusClass::ClientError, "client_errors"),
        ],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| DailyCounts {
            day: r.data.str_field("created_at").to_string(),
            requests: r.data.i64_field("requests"),
            server_errors: r.data.i64_field("server_errors"),
            client_errors: r.data.i64_field("client_errors"),
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
            error_message: if StatusClass::of(status_code).is_error() {
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
        insert_at(ctx, id, &row, at)
            .await
            .unwrap_or_else(|e| panic!("seed request_log {id}: {e}"));
    }

    /// The oldest stored row, whatever its status; `None` on an empty log.
    #[tokio::test]
    async fn first_logged_at_is_the_oldest_row_or_none() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        assert_eq!(first_logged_at(&ctx).await.unwrap(), None);
        seed_at(&ctx, "b", probe(200, 1), "2026-02-01T00:00:00Z").await;
        seed_at(&ctx, "a", probe(404, 1), "2026-01-05T09:00:00Z").await;
        assert_eq!(
            first_logged_at(&ctx).await.unwrap().as_deref(),
            Some("2026-01-05T09:00:00Z")
        );
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

        let page = paginated(&ctx, 1, 20, "", ErrorFilter::NONE)
            .await
            .expect("paginated");
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

        let again =
            RequestLogRow::from_record(&row.id, &probe(500, 42).to_data().into_iter().collect());
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

        let page = paginated(&ctx, 1, 1, "", ErrorFilter::NONE)
            .await
            .expect("page 1");
        assert_eq!(page.total_count, 3);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].id, "r3", "newest first");

        let probes = paginated(&ctx, 1, 20, "prob", ErrorFilter::NONE)
            .await
            .expect("filtered");
        assert_eq!(probes.total_count, 2);
        assert!(probes.rows.iter().all(|r| r.path == "/probe"));
    }

    /// The two error filters narrow the list by `status_code` — server
    /// errors to 5xx, client errors to 4xx, both to either — whatever the
    /// stored label says, compose with the path search, and count the
    /// narrowed set.
    #[tokio::test]
    async fn paginated_error_filters_select_by_code_not_by_the_stored_label() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed_labelled(&ctx, "served_200", 200, "OK", "2026-01-01T00:00:00Z").await;
        seed_labelled(
            &ctx,
            "refused_404_labelled_ok",
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
        seed_labelled(
            &ctx,
            "failed_500_labelled_ok",
            500,
            "OK",
            "2026-01-04T00:00:00Z",
        )
        .await;
        seed_labelled(&ctx, "refused_499", 499, "ERROR", "2026-01-05T00:00:00Z").await;

        let ids = |page: Page<RequestLogRow>| {
            let mut ids: Vec<String> = page.rows.into_iter().map(|r| r.id).collect();
            ids.sort();
            ids
        };
        let list = |errors: ErrorFilter, search: &'static str| {
            let ctx = &ctx;
            async move { paginated(ctx, 1, 20, search, errors).await.expect("list") }
        };
        const SERVER: ErrorFilter = ErrorFilter {
            server: true,
            client: false,
        };
        const CLIENT: ErrorFilter = ErrorFilter {
            server: false,
            client: true,
        };
        const BOTH: ErrorFilter = ErrorFilter {
            server: true,
            client: true,
        };

        assert_eq!(
            list(ErrorFilter::NONE, "").await.total_count,
            5,
            "no filter is every row"
        );
        assert_eq!(ids(list(SERVER, "").await), vec!["failed_500_labelled_ok"]);
        assert_eq!(
            ids(list(CLIENT, "").await),
            vec!["refused_404_labelled_ok", "refused_499"],
            "499 is the last client error, 500 the first server error",
        );
        assert_eq!(
            ids(list(BOTH, "").await),
            vec![
                "failed_500_labelled_ok",
                "refused_404_labelled_ok",
                "refused_499"
            ],
        );
        assert_eq!(
            list(CLIENT, "").await.total_count,
            2,
            "the count is of the narrowed set"
        );

        // The filters compose with the path search.
        assert_eq!(list(BOTH, "prob").await.total_count, 3);
        assert_eq!(list(SERVER, "nope").await.total_count, 0);
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

        // Today 6 (durations 100/200/300/400/25/25: one 500, a 404 and a
        // 429); 10d ago 2 (ok, 50/50); 40d ago 5 (outside the 30-day window).
        seed_at(&ctx, "r_t0", probe(200, 100), &at(0)).await;
        seed_at(&ctx, "r_t1", probe(200, 200), &at(0)).await;
        seed_at(&ctx, "r_t2", probe(200, 300), &at(0)).await;
        seed_at(&ctx, "r_t3", probe(500, 400), &at(0)).await;
        seed_at(&ctx, "r_t4", probe(404, 25), &at(0)).await;
        seed_at(&ctx, "r_t5", probe(429, 25), &at(0)).await;
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
        let today_count = |mut filters: Vec<Filter>| {
            filters.push(today_filter.clone());
            let ctx = &ctx;
            async move { db::count(ctx, TABLE, &filters).await.unwrap() }
        };
        let counts = today_counts(&ctx, &today_start)
            .await
            .expect("today_counts");
        assert_eq!(counts.requests, today_count(vec![]).await);
        assert_eq!(
            counts.server_errors,
            today_count(StatusClass::ServerError.filters()).await
        );
        assert_eq!(
            counts.client_errors,
            today_count(StatusClass::ClientError.filters()).await
        );
        assert_eq!(
            counts.rate_limited,
            today_count(vec![code(FilterOp::Equal, TOO_MANY_REQUESTS)]).await
        );
        assert_eq!(
            (
                counts.requests,
                counts.server_errors,
                counts.client_errors,
                counts.rate_limited
            ),
            (6, 1, 2, 1),
            "hand-computed"
        );
        assert!(
            (counts.avg_ms - 175.0).abs() < 1e-9,
            "avg of today's durations = 175, got {}",
            counts.avg_ms
        );

        // --- the daily series ---
        let daily = daily_counts(&ctx, &start_30d).await.expect("daily_counts");
        let on = |d: &str| daily.iter().find(|row| row.day == d);
        let split = |r: &DailyCounts| (r.requests, r.server_errors, r.client_errors);
        assert_eq!(on(&day(0)).map(split), Some((6, 1, 2)));
        assert_eq!(on(&day(10)).map(split), Some((2, 0, 0)));
        assert_eq!(
            daily.iter().map(|r| r.requests).sum::<i64>(),
            8,
            "40d-ago excluded"
        );

        // --- the per-path summary (every row shares one method+path) ---
        let summary = route_page(&ctx, every_route(PathSort::Requests))
            .await
            .expect("summary");
        assert_eq!(summary.total, 1);
        assert_eq!(summary.rows.len(), 1);
        let s = &summary.rows[0];
        assert_eq!((s.method.as_str(), s.path.as_str()), ("GET", "/probe"));
        assert_eq!(s.count, 13);
        assert_eq!((s.server_errors, s.client_errors), (1, 2));
        assert_eq!(s.last_seen, at(0));
        // CAST(AVG(duration_ms) AS INTEGER) parity: the thirteen durations
        // sum to 6145, a mean of 472.69…, truncated toward zero.
        assert_eq!(s.avg_ms, 472);
        assert!(route_page(
            &ctx,
            RouteQuery {
                search: "nope",
                ..every_route(PathSort::Requests)
            }
        )
        .await
        .expect("filtered summary")
        .rows
        .is_empty());

        // --- recent server errors: the 500 row and nothing else ---
        let recent = list_recent_server_errors(&ctx, 5)
            .await
            .expect("recent errors");
        assert_eq!(
            recent.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["r_t3"]
        );
    }

    /// Every route, first page of 50, no search, no error filter.
    fn every_route(sort: PathSort) -> RouteQuery<'static> {
        RouteQuery {
            search: "",
            sort,
            errors_only: false,
            limit: 50,
            offset: 0,
        }
    }

    /// The block a path was addressed to, as the row stores it.
    #[test]
    fn the_block_is_the_first_segment_after_b() {
        for (path, block) in [
            ("/b/admin/users", "admin"),
            ("/b/admin", "admin"),
            ("/b/", ""),
            ("/", ""),
            ("<unmatched>", ""),
            ("/static/x.css", ""),
        ] {
            assert_eq!(owning_block(path), block, "{path}");
            let row = NewRequestLog {
                path,
                ..probe(200, 1)
            };
            assert_eq!(row.to_data().get("block"), Some(&json!(block)), "{path}");
        }
    }

    /// Routes come back ordered by block, then by the key asked for within
    /// it; the page is cut by limit and offset, and every page carries the
    /// number of routes in all.
    #[tokio::test]
    async fn the_route_page_orders_within_blocks_and_pages_in_sql() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let row = |path: &'static str, code: i64| NewRequestLog {
            path,
            ..probe(code, 1)
        };
        // admin: /busy 3 requests (oldest), /failing 2 (both errors),
        // /recent 1 (newest). auth: /login 5.
        for (i, at) in [
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:01Z",
            "2026-01-01T00:00:02Z",
        ]
        .iter()
        .enumerate()
        {
            seed_at(&ctx, &format!("busy{i}"), row("/b/admin/busy", 200), at).await;
        }
        seed_at(
            &ctx,
            "fail0",
            row("/b/admin/failing", 500),
            "2026-01-02T00:00:00Z",
        )
        .await;
        seed_at(
            &ctx,
            "fail1",
            row("/b/admin/failing", 404),
            "2026-01-02T00:00:01Z",
        )
        .await;
        seed_at(
            &ctx,
            "recent",
            row("/b/admin/recent", 200),
            "2026-01-03T00:00:00Z",
        )
        .await;
        for i in 0..5 {
            seed_at(
                &ctx,
                &format!("login{i}"),
                row("/b/auth/login", 200),
                "2026-01-01T00:00:00Z",
            )
            .await;
        }

        let order = |query: RouteQuery<'static>| {
            let ctx = &ctx;
            async move {
                let page = route_page(ctx, query).await.expect("route page");
                (
                    page.rows.into_iter().map(|r| r.path).collect::<Vec<_>>(),
                    page.total,
                )
            }
        };
        assert_eq!(
            order(every_route(PathSort::Requests)).await,
            (
                vec![
                    "/b/admin/busy".to_string(),
                    "/b/admin/failing".into(),
                    "/b/admin/recent".into(),
                    "/b/auth/login".into()
                ],
                4
            ),
            "admin before auth, busiest first within admin"
        );
        assert_eq!(
            order(every_route(PathSort::Errors)).await.0[0],
            "/b/admin/failing"
        );
        assert_eq!(
            order(every_route(PathSort::Recent)).await.0[0],
            "/b/admin/recent"
        );

        let second = RouteQuery {
            limit: 2,
            offset: 2,
            ..every_route(PathSort::Requests)
        };
        assert_eq!(
            order(second).await,
            (
                vec!["/b/admin/recent".to_string(), "/b/auth/login".into()],
                4
            ),
            "the second page of two, with the count of all four"
        );

        let failing = RouteQuery {
            errors_only: true,
            ..every_route(PathSort::Requests)
        };
        assert_eq!(
            order(failing).await,
            (vec!["/b/admin/failing".to_string()], 1),
            "errors only is a HAVING: the count is of the failing routes"
        );

        let past = RouteQuery {
            offset: 50,
            ..every_route(PathSort::Requests)
        };
        assert_eq!(order(past).await, (vec![], 0), "a page past the end");

        let totals = block_totals(&ctx, "").await.expect("block totals");
        let summed = |block: &str| {
            totals
                .iter()
                .find(|t| t.block == block)
                .map(|t| (t.requests, t.server_errors, t.client_errors))
        };
        assert_eq!(summed("admin"), Some((6, 1, 1)));
        assert_eq!(summed("auth"), Some((5, 0, 0)));
        assert_eq!(
            block_totals(&ctx, "login")
                .await
                .expect("searched totals")
                .len(),
            1,
            "the totals follow the search"
        );
    }

    /// Migrations 007 and 008 over rows written before them: an action name
    /// becomes its HTTP method, an `update` row (PUT or PATCH, nobody can
    /// say) is dropped, and every row gets the block its path names. Both
    /// re-run without changing anything.
    #[tokio::test]
    async fn migrations_007_and_008_bring_old_rows_onto_the_new_shape() {
        use crate::blocks::admin::migrations::{
            ddl_files, REQUEST_LOGS_BLOCK, REQUEST_LOGS_HTTP_METHOD, SQLITE_MIGRATIONS,
        };
        let db: std::sync::Arc<dyn wafer_core::interfaces::database::service::DatabaseService> =
            std::sync::Arc::new(
                wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                    .expect("open in-memory sqlite"),
            );
        let all = ddl_files("sqlite");
        let at = |name: &str| {
            SQLITE_MIGRATIONS
                .iter()
                .position(|(n, _)| *n == name)
                .expect("wired into SQLITE_MIGRATIONS")
        };
        let (method_at, block_at) = (at(REQUEST_LOGS_HTTP_METHOD), at(REQUEST_LOGS_BLOCK));
        crate::migration_helper::apply_ddl_via_service(&db, &all[..method_at])
            .await
            .expect("the migrations before 007");

        for (id, method, path) in [
            ("r", "retrieve", "/b/admin/users"),
            ("c", "create", "/b/auth/api/login"),
            ("d", "delete", "/b/admin/x"),
            ("u", "update", "/b/admin/variables/K"),
            ("g", "GET", "/"),
            ("n", "retrieve", "<unmatched>"),
        ] {
            let mut data = HashMap::new();
            for (column, value) in [
                ("id", json!(id)),
                ("method", json!(method)),
                ("path", json!(path)),
                ("created_at", json!("2026-01-01T00:00:00Z")),
                ("updated_at", json!("2026-01-01T00:00:00Z")),
            ] {
                data.insert(column.to_string(), value);
            }
            db.create(TABLE, data).await.expect("seed a pre-007 row");
        }

        for run in ["first", "second"] {
            crate::migration_helper::apply_ddl_via_service(&db, &all[..=block_at])
                .await
                .unwrap_or_else(|e| panic!("{run} run of 001-008: {e}"));
            let mut rows: Vec<(String, String, String)> = db
                .list(TABLE, &ListOptions::default())
                .await
                .expect("list")
                .records
                .into_iter()
                .map(|r| {
                    (
                        r.id.clone(),
                        r.data.str_field("method").to_string(),
                        r.data.str_field("block").to_string(),
                    )
                })
                .collect();
            rows.sort();
            let expected: Vec<(String, String, String)> = [
                ("c", "POST", "auth"),
                ("d", "DELETE", "admin"),
                ("g", "GET", ""),
                ("n", "GET", ""),
                ("r", "GET", "admin"),
            ]
            .iter()
            .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
            .collect();
            assert_eq!(rows, expected, "{run} run");
        }
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
        assert_eq!(
            (counts.requests, counts.server_errors, counts.client_errors),
            (3, 1, 1),
            "today_counts"
        );

        let daily = daily_counts(&ctx, &today_start)
            .await
            .expect("daily_counts");
        assert_eq!(
            daily
                .iter()
                .map(|r| (r.requests, r.server_errors, r.client_errors))
                .collect::<Vec<_>>(),
            vec![(3, 1, 1)],
            "daily_counts",
        );

        let summary = route_page(&ctx, every_route(PathSort::Requests))
            .await
            .expect("summary");
        assert_eq!(
            summary
                .rows
                .iter()
                .map(|s| (s.count, s.server_errors, s.client_errors))
                .collect::<Vec<_>>(),
            vec![(3, 1, 1)],
            "route_page",
        );

        let recent: Vec<String> = list_recent_server_errors(&ctx, 5)
            .await
            .expect("recent errors")
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(
            recent,
            vec!["served_500_labelled_ok"],
            "list_recent_server_errors"
        );
    }

    /// The class boundaries every reader shares.
    #[test]
    fn status_classes_split_at_400_and_500() {
        for (code, class) in [
            (200, StatusClass::Served),
            (399, StatusClass::Served),
            (400, StatusClass::ClientError),
            (429, StatusClass::ClientError),
            (499, StatusClass::ClientError),
            (500, StatusClass::ServerError),
            (503, StatusClass::ServerError),
        ] {
            assert_eq!(StatusClass::of(code), class, "{code}");
        }
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
