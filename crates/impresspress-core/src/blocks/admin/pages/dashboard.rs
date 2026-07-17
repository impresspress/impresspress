use std::collections::HashMap;

use maud::html;
use wafer_block::{
    db::{Filter, FilterOp, FilterTree, ListOptions, SortField},
    wire::database as wire,
};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream};

use super::{admin_page, crumb};
use crate::{
    blocks::{admin::REQUEST_LOGS_TABLE as REQUEST_LOGS, auth::USERS_TABLE as USERS},
    ui::{
        shell::Topbar,
        templates::{dashboard_page, PageHeader, StatTile},
        SiteConfig, UserInfo,
    },
    util::RecordExt,
};

/// Encode client-side [`Filter`]s as all-leaf wire [`FilterNode`](wire::FilterNode)s
/// for a typed `db::aggregate` request. Mirrors `wafer-core`'s internal
/// `to_wire_filters` conversion (not exported for block code to reuse).
fn to_wire_filters(filters: &[Filter]) -> Vec<wire::FilterNode> {
    filters
        .iter()
        .map(|f| {
            let operator = match f.operator {
                FilterOp::Equal => "eq",
                FilterOp::NotEqual => "neq",
                FilterOp::GreaterThan => "gt",
                FilterOp::GreaterEqual => "gte",
                FilterOp::LessThan => "lt",
                FilterOp::LessEqual => "lte",
                FilterOp::Like => "like",
                FilterOp::In => "in",
                FilterOp::IsNull => "is_null",
                FilterOp::IsNotNull => "is_not_null",
            };
            wire::FilterNode::Leaf(wire::FilterDef {
                field: f.field.clone(),
                operator: operator.to_string(),
                value: f.value.clone(),
            })
        })
        .collect()
}

/// Render a 30-day column bar chart card. `data` is ordered
/// chronologically; bars are normalized against the max count.
fn bar_chart_card(
    title: &str,
    subtitle: &str,
    data: &[(String, i64)],
    color_var: &str,
    view_href: &str,
) -> maud::Markup {
    let max = data.iter().map(|(_, v)| *v).max().unwrap_or(0).max(1);
    let fmt_short = |s: &str| -> String {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(|d| d.format("%b %-d").to_string())
            .unwrap_or_else(|_| s.to_string())
    };
    let first_label = data.first().map(|(d, _)| fmt_short(d)).unwrap_or_default();
    let last_label = data.last().map(|(d, _)| fmt_short(d)).unwrap_or_default();
    html! {
        section .card {
            header .card__head {
                div {
                    h3 .card__title { (title) }
                    p style="margin:0;font-size:var(--text-xs);color:var(--text-muted)" { (subtitle) }
                }
                a .btn .btn-ghost .btn-sm .card__actions href=(view_href) { "View" }
            }
            div .card__body {
                table .charts-css .column style=(format!("--chart-color: {color_var}")) {
                    tbody {
                        @for (day, val) in data {
                            tr data-tooltip=(format!("{day}: {val}")) {
                                td style=(format!("--size: {:.4}", *val as f64 / max as f64)) {
                                    (val)
                                }
                            }
                        }
                    }
                }
                div .charts-css__range {
                    span { (first_label) }
                    span { (last_label) }
                }
            }
        }
    }
}

/// Trailing 30-day window as `(oldest_day, oldest_day_midnight_iso)`.
/// `oldest_day` anchors the zero-fill; the ISO string is the `created_at >=`
/// lower bound shared by every 30-day query.
fn window_30d() -> (chrono::NaiveDate, String) {
    let today = chrono::Utc::now().date_naive();
    let start = today - chrono::Duration::days(29);
    (start, format!("{start}T00:00:00"))
}

/// Run ONE grouped-by-day aggregate over the trailing 30-day window and return
/// the per-day rows (one [`wire::Record`] per day that has data). `aggregates`
/// may carry several columns — e.g. a plain `Count` alongside a conditional
/// `CaseWhenSum` — so a single statement can back multiple daily series over
/// the same table. Callers project each alias out with [`series_from_rows`],
/// which zero-fills the days with no rows.
async fn daily_grouped_30d(
    ctx: &dyn Context,
    table: &str,
    start_iso: &str,
    extra_filters: Vec<Filter>,
    aggregates: Vec<wire::AggregateColumnDef>,
) -> Vec<wire::Record> {
    let mut filters = vec![Filter {
        field: "created_at".into(),
        operator: FilterOp::GreaterEqual,
        value: serde_json::json!(start_iso),
    }];
    filters.extend(extra_filters);

    let req = wire::AggregateRequest {
        collection: table.to_string(),
        select_columns: vec![],
        aggregates,
        filters: to_wire_filters(&filters),
        group_by: vec![wire::GroupByDef::DateBucket {
            field: "created_at".into(),
        }],
        sort: vec![],
        limit: 0,
    };
    db::aggregate(ctx, req).await.unwrap_or_default()
}

/// Project one aggregate `alias` out of grouped daily `rows` into a zero-filled
/// 30-entry series ordered oldest → newest (matching the chart's x-axis).
/// A missing day, or a group whose conditional sum was `NULL`, reads as `0`.
fn series_from_rows(
    rows: &[wire::Record],
    alias: &str,
    start: chrono::NaiveDate,
) -> Vec<(String, i64)> {
    let by_day: HashMap<String, i64> = rows
        .iter()
        .filter_map(|r| {
            let day = r.data.get("created_at").and_then(|v| v.as_str())?;
            let cnt = r.data.get(alias).and_then(|v| v.as_i64()).unwrap_or(0);
            Some((day.to_string(), cnt))
        })
        .collect();

    (0..30)
        .map(|i| {
            let date = (start + chrono::Duration::days(i))
                .format("%Y-%m-%d")
                .to_string();
            let count = by_day.get(&date).copied().unwrap_or(0);
            (date, count)
        })
        .collect()
}

/// Header-tile USER counts in ONE statement: `(total_active, active_today)`.
///
/// Both counts share the `deleted_at IS NULL` predicate, so it becomes the
/// query's `WHERE` and the "created today" restriction rides along as a
/// conditional `CaseWhenSum` — replacing the two separate `db::count`
/// round-trips with a single aggregate that returns the same two numbers.
async fn user_counts(ctx: &dyn Context, today_start: &str) -> (i64, i64) {
    let active = [Filter {
        field: "deleted_at".into(),
        operator: FilterOp::IsNull,
        value: serde_json::Value::Null,
    }];
    let created_today = [Filter {
        field: "created_at".into(),
        operator: FilterOp::GreaterEqual,
        value: serde_json::json!(today_start),
    }];
    let req = wire::AggregateRequest {
        collection: USERS.to_string(),
        select_columns: vec![],
        aggregates: vec![
            wire::AggregateColumnDef::Count {
                alias: "total".into(),
            },
            wire::AggregateColumnDef::CaseWhenSum {
                when: to_wire_filters(&created_today),
                alias: "today".into(),
            },
        ],
        filters: to_wire_filters(&active),
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    let rows = db::aggregate(ctx, req).await.unwrap_or_default();
    let row = rows.first();
    let read = |k: &str| {
        row.and_then(|r| r.data.get(k))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    };
    (read("total"), read("today"))
}

/// Header-tile REQUEST_LOGS metrics for today in ONE statement:
/// `(requests, errors, avg_ms)`.
///
/// All three share the `created_at >= today_start` predicate, so it becomes the
/// `WHERE`; the error tally rides along as a conditional `CaseWhenSum` and the
/// latency as an `Avg` — replacing two `db::count`s plus a separate average
/// aggregate with a single round-trip.
async fn request_counts(ctx: &dyn Context, today_start: &str) -> (i64, i64, f64) {
    let today = [Filter {
        field: "created_at".into(),
        operator: FilterOp::GreaterEqual,
        value: serde_json::json!(today_start),
    }];
    let is_error = [Filter {
        field: "status".into(),
        operator: FilterOp::Equal,
        value: serde_json::json!("ERROR"),
    }];
    let req = wire::AggregateRequest {
        collection: REQUEST_LOGS.to_string(),
        select_columns: vec![],
        aggregates: vec![
            wire::AggregateColumnDef::Count {
                alias: "requests".into(),
            },
            wire::AggregateColumnDef::CaseWhenSum {
                when: to_wire_filters(&is_error),
                alias: "errors".into(),
            },
            wire::AggregateColumnDef::Avg {
                field: "duration_ms".into(),
                alias: "avg_val".into(),
            },
        ],
        filters: to_wire_filters(&today),
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    let rows = db::aggregate(ctx, req).await.unwrap_or_default();
    let row = rows.first();
    let requests = row
        .and_then(|r| r.data.get("requests"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let errors = row
        .and_then(|r| r.data.get("errors"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let avg_ms = row
        .and_then(|r| r.data.get("avg_val"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    (requests, errors, avg_ms)
}

pub async fn dashboard(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let config = SiteConfig::load(ctx).await;
    let user = UserInfo::from_message(msg);

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let today_start = format!("{today}T00:00:00");

    // Every read below is independent, so we issue them concurrently with
    // `futures::join!`. Concurrency alone doesn't cut the D1 *statement* count,
    // though — each round-trip is a billed statement on Cloudflare — so the
    // header tiles now fold their several per-filter counts into ONE aggregate
    // per table (conditional `CaseWhenSum` columns), and the two REQUEST_LOGS
    // chart series come from ONE grouped-by-day statement. That is six D1
    // statements to render the whole page (was ten): two consolidated header
    // aggregates, two recent-row lists, and two daily grouped aggregates.
    let (start_30d, start_iso) = window_30d();

    let user_counts_fut = user_counts(ctx, &today_start);
    let request_counts_fut = request_counts(ctx, &today_start);

    let users_daily_fut = daily_grouped_30d(
        ctx,
        USERS,
        &start_iso,
        vec![Filter {
            field: "deleted_at".into(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }],
        vec![wire::AggregateColumnDef::Count {
            alias: "cnt".into(),
        }],
    );
    let requests_daily_fut = daily_grouped_30d(
        ctx,
        REQUEST_LOGS,
        &start_iso,
        vec![],
        vec![
            wire::AggregateColumnDef::Count {
                alias: "requests".into(),
            },
            wire::AggregateColumnDef::CaseWhenSum {
                when: to_wire_filters(&[Filter {
                    field: "status".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!("ERROR"),
                }]),
                alias: "errors".into(),
            },
        ],
    );

    let recent_users_opts = ListOptions {
        columns: Some(vec!["id".into(), "email".into(), "created_at".into()]),
        filters: vec![Filter {
            field: "deleted_at".into(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }],
        sort: vec![SortField {
            field: "created_at".into(),
            desc: true,
        }],
        limit: 5,
        skip_count: true,
        ..Default::default()
    };
    let recent_users_fut = db::list(ctx, USERS, &recent_users_opts);

    let recent_errors_opts = ListOptions {
        columns: Some(vec![
            "status_code".into(),
            "method".into(),
            "path".into(),
            "duration_ms".into(),
            "created_at".into(),
        ]),
        filter_tree: Some(vec![FilterTree::Any(vec![
            FilterTree::Leaf(Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("ERROR"),
            }),
            FilterTree::Leaf(Filter {
                field: "status_code".into(),
                operator: FilterOp::GreaterEqual,
                value: serde_json::json!(400),
            }),
        ])]),
        sort: vec![SortField {
            field: "created_at".into(),
            desc: true,
        }],
        limit: 5,
        skip_count: true,
        ..Default::default()
    };
    let recent_errors_fut = db::list(ctx, REQUEST_LOGS, &recent_errors_opts);

    let (
        (user_count, new_users_today),
        (requests_today, errors_today, avg_ms),
        recent_users_r,
        recent_errors_r,
        users_daily_rows,
        requests_daily_rows,
    ) = futures::join!(
        user_counts_fut,
        request_counts_fut,
        recent_users_fut,
        recent_errors_fut,
        users_daily_fut,
        requests_daily_fut,
    );

    let recent_users = recent_users_r.map(|rl| rl.records).unwrap_or_default();
    let recent_errors = recent_errors_r.map(|rl| rl.records).unwrap_or_default();

    // Two grouped statements back all three charts: the USERS series comes from
    // its own daily aggregate; the REQUEST_LOGS "requests" and "errors" series
    // are two aliases projected out of the *same* per-day rows.
    let new_users_daily = series_from_rows(&users_daily_rows, "cnt", start_30d);
    let requests_daily = series_from_rows(&requests_daily_rows, "requests", start_30d);
    let errors_daily = series_from_rows(&requests_daily_rows, "errors", start_30d);

    let user_count_str = user_count.to_string();
    let new_users_str = new_users_today.to_string();
    let requests_str = requests_today.to_string();
    let errors_str = errors_today.to_string();
    let avg_ms_str = format!("{avg_ms:.0}ms");

    let stats = vec![
        StatTile {
            label: "Total Users",
            value: &user_count_str,
            trend: None,
        },
        StatTile {
            label: "New Today",
            value: &new_users_str,
            trend: None,
        },
        StatTile {
            label: "Requests Today",
            value: &requests_str,
            trend: None,
        },
        StatTile {
            label: "Errors Today",
            value: &errors_str,
            trend: None,
        },
        StatTile {
            label: "Avg Response",
            value: &avg_ms_str,
            trend: None,
        },
    ];

    let recent_users_card = html! {
        section .card {
            header .card__head {
                h3 .card__title { "Recent Users" }
                a .btn .btn-ghost .btn-sm href="/b/admin/users" { "View all" }
            }
            div .card__body {
                @if recent_users.is_empty() {
                    p .text-muted .text-sm { "No users yet" }
                } @else {
                    div .table-container {
                        table .table {
                            tbody {
                                @for record in &recent_users {
                                    @let email = record.str_field("email");
                                    @let created = record.str_field("created_at");
                                    tr {
                                        td .text-sm { (email) }
                                        td .text-muted .text-sm .text-right { (created.get(..10).unwrap_or(created)) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    let recent_errors_card = html! {
        section .card {
            header .card__head {
                h3 .card__title { "Recent Errors" }
                a .btn .btn-ghost .btn-sm .card__actions href="/b/admin/logs?status=ERROR" { "View all" }
            }
            div .card__body {
                @if recent_errors.is_empty() {
                    p .text-muted .text-sm { "No errors recently" }
                } @else {
                    div .table-container {
                        table .table {
                            thead {
                                tr {
                                    th { "Status" }
                                    th { "Method" }
                                    th { "Path" }
                                    th { "Time" }
                                }
                            }
                            tbody {
                                @for record in &recent_errors {
                                    @let code = record.i64_field("status_code");
                                    @let method = record.str_field("method");
                                    @let path = record.str_field("path");
                                    @let created = record.str_field("created_at");
                                    tr {
                                        td {
                                            span .badge .(if code >= 500 { "badge-danger" } else { "badge-warning" }) { (code) }
                                        }
                                        td .text-sm .font-medium { (method.to_uppercase()) }
                                        td .text-sm { (path) }
                                        td .text-muted .text-sm { (created.get(..19).unwrap_or(created)) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    let charts_section = html! {
        div .dashboard-charts {
            (bar_chart_card("New users", "Last 30 days", &new_users_daily, "var(--primary-color)", "/b/admin/users"))
            (bar_chart_card("Requests", "Last 30 days", &requests_daily, "var(--accent-warning)", "/b/admin/logs"))
            (bar_chart_card("Errors", "Last 30 days", &errors_daily, "var(--accent-danger)", "/b/admin/logs?status=ERROR"))
        }
    };

    let body = dashboard_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        stats,
        recent_users_card,
        recent_errors_card,
        None,
        Some(charts_section),
    );

    admin_page(
        "Dashboard",
        &config,
        "/b/admin/",
        user.as_ref(),
        Topbar {
            crumbs: crumb("Dashboard"),
            primary_action: None,
            subtitle: Some("System overview"),
            show_palette: true,
        },
        body,
        msg,
    )
}

#[cfg(test)]
mod tests {
    //! Correctness: the consolidated aggregates return byte-for-byte the same
    //! numbers the previous per-filter `db::count` / per-metric grouped queries
    //! produced. Each consolidated helper is checked against the equivalent
    //! separate `db::count` calls over the same seeded in-memory database, and
    //! against hand-computed expectations for the fixed seed.

    use std::collections::HashMap;

    use serde_json::json;
    use wafer_block::db::{Filter, FilterOp};
    use wafer_core::clients::database as db;

    use super::{
        daily_grouped_30d, request_counts, series_from_rows, user_counts, window_30d, wire,
        REQUEST_LOGS, USERS,
    };
    use crate::test_support::TestContext;

    async fn seed_user(ctx: &TestContext, id: &str, created_at: &str, deleted_at: Option<&str>) {
        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        data.insert("id".into(), json!(id));
        data.insert("email".into(), json!(format!("{id}@example.test")));
        data.insert("display_name".into(), json!(id));
        data.insert("created_at".into(), json!(created_at));
        if let Some(ts) = deleted_at {
            data.insert("deleted_at".into(), json!(ts));
        }
        db::create(ctx, USERS, data)
            .await
            .unwrap_or_else(|e| panic!("seed user {id}: {e}"));
    }

    async fn seed_req(
        ctx: &TestContext,
        id: &str,
        status: &str,
        duration_ms: i64,
        created_at: &str,
    ) {
        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        data.insert("id".into(), json!(id));
        data.insert("method".into(), json!("GET"));
        data.insert("path".into(), json!("/x"));
        data.insert("status".into(), json!(status));
        data.insert(
            "status_code".into(),
            json!(if status == "ERROR" { 500 } else { 200 }),
        );
        data.insert("duration_ms".into(), json!(duration_ms));
        data.insert("created_at".into(), json!(created_at));
        db::create(ctx, REQUEST_LOGS, data)
            .await
            .unwrap_or_else(|e| panic!("seed request_log {id}: {e}"));
    }

    /// Value for `date` in a `(date, count)` series, or `-1` if the day is absent.
    fn day_value(series: &[(String, i64)], date: &str) -> i64 {
        series
            .iter()
            .find(|(d, _)| d == date)
            .map(|(_, c)| *c)
            .unwrap_or(-1)
    }

    fn sum(series: &[(String, i64)]) -> i64 {
        series.iter().map(|(_, c)| c).sum()
    }

    #[tokio::test]
    async fn consolidated_aggregates_match_per_filter_queries() {
        let ctx = TestContext::with_auth().await;

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

        // Users: 3 active today, 2 active 5d ago, 1 active 40d ago (outside the
        // 30-day window), 2 deleted today (excluded by `deleted_at IS NULL`).
        for i in 0..3 {
            seed_user(&ctx, &format!("u_today_{i}"), &at(0), None).await;
        }
        for i in 0..2 {
            seed_user(&ctx, &format!("u_5d_{i}"), &at(5), None).await;
        }
        seed_user(&ctx, "u_40d", &at(40), None).await;
        for i in 0..2 {
            seed_user(&ctx, &format!("u_del_{i}"), &at(0), Some(&at(0))).await;
        }

        // Requests: today 4 (durations 100/200/300/400, one ERROR); 10d ago 2
        // (ok, 50/50); 40d ago 5 (outside the window).
        seed_req(&ctx, "r_t0", "OK", 100, &at(0)).await;
        seed_req(&ctx, "r_t1", "OK", 200, &at(0)).await;
        seed_req(&ctx, "r_t2", "OK", 300, &at(0)).await;
        seed_req(&ctx, "r_t3", "ERROR", 400, &at(0)).await;
        seed_req(&ctx, "r_10d_0", "OK", 50, &at(10)).await;
        seed_req(&ctx, "r_10d_1", "OK", 50, &at(10)).await;
        for i in 0..5 {
            seed_req(&ctx, &format!("r_40d_{i}"), "OK", 999, &at(40)).await;
        }

        // --- header tile counts: consolidated vs. separate per-filter counts ---
        let active = [Filter {
            field: "deleted_at".into(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }];
        let active_today = [
            Filter {
                field: "deleted_at".into(),
                operator: FilterOp::IsNull,
                value: serde_json::Value::Null,
            },
            Filter {
                field: "created_at".into(),
                operator: FilterOp::GreaterEqual,
                value: json!(&today_start),
            },
        ];
        let total_expected = db::count(&ctx, USERS, &active).await.unwrap();
        let new_expected = db::count(&ctx, USERS, &active_today).await.unwrap();
        let (total, new_today) = user_counts(&ctx, &today_start).await;
        assert_eq!(
            (total, new_today),
            (total_expected, new_expected),
            "user_counts must match separate db::count calls"
        );
        assert_eq!((total, new_today), (6, 3), "hand-computed user counts");

        let req_today = [Filter {
            field: "created_at".into(),
            operator: FilterOp::GreaterEqual,
            value: json!(&today_start),
        }];
        let err_today = [
            Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: json!("ERROR"),
            },
            Filter {
                field: "created_at".into(),
                operator: FilterOp::GreaterEqual,
                value: json!(&today_start),
            },
        ];
        let requests_expected = db::count(&ctx, REQUEST_LOGS, &req_today).await.unwrap();
        let errors_expected = db::count(&ctx, REQUEST_LOGS, &err_today).await.unwrap();
        let (requests, errors, avg_ms) = request_counts(&ctx, &today_start).await;
        assert_eq!(requests, requests_expected, "requests count");
        assert_eq!(errors, errors_expected, "errors count");
        assert_eq!((requests, errors), (4, 1), "hand-computed request counts");
        assert!(
            (avg_ms - 250.0).abs() < 1e-9,
            "avg of today's durations = 250, got {avg_ms}"
        );

        // --- daily chart series ---
        let (start_30d, start_iso) = window_30d();

        let users_rows = daily_grouped_30d(
            &ctx,
            USERS,
            &start_iso,
            vec![Filter {
                field: "deleted_at".into(),
                operator: FilterOp::IsNull,
                value: serde_json::Value::Null,
            }],
            vec![wire::AggregateColumnDef::Count {
                alias: "cnt".into(),
            }],
        )
        .await;
        let new_users_daily = series_from_rows(&users_rows, "cnt", start_30d);
        assert_eq!(new_users_daily.len(), 30, "30-entry zero-filled series");
        assert_eq!(day_value(&new_users_daily, &day(0)), 3, "3 users today");
        assert_eq!(day_value(&new_users_daily, &day(5)), 2, "2 users 5d ago");
        assert_eq!(sum(&new_users_daily), 5, "40d-ago user + deleted excluded");

        let req_rows = daily_grouped_30d(
            &ctx,
            REQUEST_LOGS,
            &start_iso,
            vec![],
            vec![
                wire::AggregateColumnDef::Count {
                    alias: "requests".into(),
                },
                wire::AggregateColumnDef::CaseWhenSum {
                    when: super::to_wire_filters(&[Filter {
                        field: "status".into(),
                        operator: FilterOp::Equal,
                        value: json!("ERROR"),
                    }]),
                    alias: "errors".into(),
                },
            ],
        )
        .await;
        // Both series come out of the SAME grouped rows — the whole point of the
        // consolidation.
        let requests_daily = series_from_rows(&req_rows, "requests", start_30d);
        let errors_daily = series_from_rows(&req_rows, "errors", start_30d);
        assert_eq!(day_value(&requests_daily, &day(0)), 4, "4 requests today");
        assert_eq!(
            day_value(&requests_daily, &day(10)),
            2,
            "2 requests 10d ago"
        );
        assert_eq!(sum(&requests_daily), 6, "40d-ago requests excluded");
        assert_eq!(day_value(&errors_daily, &day(0)), 1, "1 error today");
        assert_eq!(sum(&errors_daily), 1, "one error total in window");
    }
}
