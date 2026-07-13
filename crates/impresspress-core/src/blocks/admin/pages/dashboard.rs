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

/// Run a daily-count query over the trailing 30-day window and zero-fill
/// missing days. Returns 30 entries ordered oldest → newest.
async fn daily_counts_30d(
    ctx: &dyn Context,
    table: &str,
    extra_filters: Vec<Filter>,
) -> Vec<(String, i64)> {
    use chrono::Duration;

    let today = chrono::Utc::now().date_naive();
    let start = today - Duration::days(29);
    let start_iso = format!("{start}T00:00:00");

    let mut filters = vec![Filter {
        field: "created_at".into(),
        operator: FilterOp::GreaterEqual,
        value: serde_json::json!(start_iso),
    }];
    filters.extend(extra_filters);

    let req = wire::AggregateRequest {
        collection: table.to_string(),
        select_columns: vec![],
        aggregates: vec![wire::AggregateColumnDef::Count {
            alias: "cnt".into(),
        }],
        filters: to_wire_filters(&filters),
        group_by: vec![wire::GroupByDef::DateBucket {
            field: "created_at".into(),
        }],
        sort: vec![],
        limit: 0,
    };
    let rows = db::aggregate(ctx, req).await.unwrap_or_default();

    let counts: HashMap<String, i64> = rows
        .iter()
        .filter_map(|r| {
            let day = r.data.get("created_at").and_then(|v| v.as_str())?;
            let cnt = r.data.get("cnt").and_then(|v| v.as_i64())?;
            Some((day.to_string(), cnt))
        })
        .collect();

    (0..30)
        .map(|i| {
            let date = (start + Duration::days(i)).format("%Y-%m-%d").to_string();
            let count = counts.get(&date).copied().unwrap_or(0);
            (date, count)
        })
        .collect()
}

pub async fn dashboard(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let config = SiteConfig::load(ctx).await;
    let user = UserInfo::from_message(msg);

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let today_start = format!("{today}T00:00:00");

    // Build the seven independent queries used for the header tiles + recent
    // panels. None of them depend on each other's results, so we issue them
    // concurrently with `futures::join!` instead of awaiting one at a time.
    // This used to be 7 sequential round-trips on every dashboard load — a
    // measurable D1 amplification source on Cloudflare Workers.

    let user_count_filters = [Filter {
        field: "deleted_at".into(),
        operator: FilterOp::IsNull,
        value: serde_json::Value::Null,
    }];
    let user_count_fut = db::count(ctx, USERS, &user_count_filters);

    let new_users_filters = [
        Filter {
            field: "deleted_at".into(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        },
        Filter {
            field: "created_at".into(),
            operator: FilterOp::GreaterEqual,
            value: serde_json::json!(&today_start),
        },
    ];
    let new_users_fut = db::count(ctx, USERS, &new_users_filters);

    let requests_filters = [Filter {
        field: "created_at".into(),
        operator: FilterOp::GreaterEqual,
        value: serde_json::json!(&today_start),
    }];
    let requests_fut = db::count(ctx, REQUEST_LOGS, &requests_filters);

    let errors_filters = [
        Filter {
            field: "status".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!("ERROR"),
        },
        Filter {
            field: "created_at".into(),
            operator: FilterOp::GreaterEqual,
            value: serde_json::json!(&today_start),
        },
    ];
    let errors_fut = db::count(ctx, REQUEST_LOGS, &errors_filters);

    let avg_ms_fut = db::aggregate(
        ctx,
        wire::AggregateRequest {
            collection: REQUEST_LOGS.to_string(),
            select_columns: vec![],
            aggregates: vec![wire::AggregateColumnDef::Avg {
                field: "duration_ms".into(),
                alias: "avg_val".into(),
            }],
            filters: vec![wire::FilterNode::Leaf(wire::FilterDef {
                field: "created_at".into(),
                operator: "gte".into(),
                value: serde_json::json!(&today_start),
            })],
            group_by: vec![],
            sort: vec![],
            limit: 0,
        },
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
        user_count_r,
        new_users_r,
        requests_r,
        errors_r,
        avg_ms_r,
        recent_users_r,
        recent_errors_r,
    ) = futures::join!(
        user_count_fut,
        new_users_fut,
        requests_fut,
        errors_fut,
        avg_ms_fut,
        recent_users_fut,
        recent_errors_fut,
    );

    let user_count = user_count_r.unwrap_or(0);
    let new_users_today = new_users_r.unwrap_or(0);
    let requests_today = requests_r.unwrap_or(0);
    let errors_today = errors_r.unwrap_or(0);
    let avg_ms = avg_ms_r
        .ok()
        .and_then(|r| {
            r.first()
                .and_then(|r| r.data.get("avg_val").and_then(|v| v.as_f64()))
        })
        .unwrap_or(0.0);
    let recent_users = recent_users_r.map(|rl| rl.records).unwrap_or_default();
    let recent_errors = recent_errors_r.map(|rl| rl.records).unwrap_or_default();

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

    // 30-day daily series for charts
    let new_users_daily = daily_counts_30d(
        ctx,
        USERS,
        vec![Filter {
            field: "deleted_at".into(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }],
    )
    .await;
    let requests_daily = daily_counts_30d(ctx, REQUEST_LOGS, vec![]).await;
    let errors_daily = daily_counts_30d(
        ctx,
        REQUEST_LOGS,
        vec![Filter {
            field: "status".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!("ERROR"),
        }],
    )
    .await;

    let charts_section = html! {
        div .dashboard-charts {
            (bar_chart_card("New users", "Last 30 days", &new_users_daily, "var(--primary-color)", "/b/admin/users"))
            (bar_chart_card("Requests", "Last 30 days", &requests_daily, "var(--accent-info)", "/b/admin/logs"))
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
