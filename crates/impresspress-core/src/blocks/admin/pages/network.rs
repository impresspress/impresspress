use maud::{html, Markup};
use wafer_block::{
    db::{Filter, FilterOp, ListOptions, SortField},
    wire::database as wire,
};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::admin::REQUEST_LOGS_TABLE as REQUEST_LOGS,
    ui::{components, icons},
    util::RecordExt,
};

/// Render JUST the network monitoring body. The parent `settings_page`
/// handler wraps this in the form-less `tabbed_page` shell. This tab is
/// read-only monitoring — it renders no `<form>` and has nothing to save.
pub async fn settings_body(ctx: &dyn Context, msg: &Message) -> Markup {
    html! {
        div .filter-bar style="margin-bottom:0.5rem" {
            button .btn .btn-secondary .btn-sm
                hx-get="/b/admin/settings/network"
                hx-target="#content"
            { (icons::refresh_cw()) " Refresh" }
        }

        (components::tab_navigation(vec![components::Tab {
            active: true,
            href: "/b/admin/settings/network",
            label: "Inbound",
            icon: Some(icons::arrow_down_left()),
        }]))

        div #network-tab-content {
            (network_inbound_tab(ctx, msg).await)
        }
    }
}

async fn network_inbound_tab(ctx: &dyn Context, msg: &Message) -> Markup {
    let search = msg.query("search").to_string();

    let filters = if search.is_empty() {
        vec![]
    } else {
        vec![wire::FilterNode::Leaf(wire::FilterDef {
            field: "path".into(),
            operator: "like".into(),
            value: serde_json::json!(format!("%{search}%")),
        })]
    };

    let req = wire::AggregateRequest {
        collection: REQUEST_LOGS.to_string(),
        select_columns: vec!["method".into(), "path".into()],
        aggregates: vec![
            wire::AggregateColumnDef::Count {
                alias: "cnt".into(),
            },
            wire::AggregateColumnDef::Avg {
                field: "duration_ms".into(),
                alias: "avg_ms".into(),
            },
            wire::AggregateColumnDef::CaseWhenSum {
                when: vec![wire::FilterNode::Leaf(wire::FilterDef {
                    field: "status_code".into(),
                    operator: "gte".into(),
                    value: serde_json::json!(400),
                })],
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
        limit: 50,
    };
    let summary = db::aggregate(ctx, req).await.unwrap_or_default();

    html! {
        div .filter-bar {
            (components::search_input_with_value("search", "Search by path...", "/b/admin/settings/network", "#content", &search))
        }

        style { (maud::PreEscaped("
            .expand-row { cursor: pointer; }
            .expand-row:hover { background: var(--bg-secondary, #f8fafc); }
            .detail-rows td { background: var(--bg-secondary, #f8fafc); font-size: 12px; }
            .detail-rows[hidden] { display: none; }
        ")) }
        // Delegated click handler — the row carries `data-detail-*` attributes
        // (maud-escaped) instead of an `onclick` JS-string literal, which maud
        // does NOT escape and so let an attacker-controlled request path break
        // out and run script in an admin's session. Bound once per document.
        script { (maud::PreEscaped("
            if (!window.__networkDetailBound) {
                window.__networkDetailBound = true;
                document.addEventListener('click', function (e) {
                    var row = e.target.closest('.expand-row[data-detail-target]');
                    if (!row) return;
                    var detail = document.getElementById(row.dataset.detailTarget);
                    if (!detail) return;
                    var dr = detail.closest('tr');
                    if (!dr.hidden) { dr.hidden = true; return; }
                    dr.hidden = false;
                    if (!detail.innerHTML) {
                        htmx.ajax('GET', row.dataset.detailUrl, {target: '#' + row.dataset.detailTarget, swap: 'innerHTML'});
                    }
                });
            }
        ")) }

        div .table-container {
            table .table {
                thead {
                    tr {
                        th style="width:30px" { "" }
                        th { "Method" }
                        th { "Path" }
                        th { "Requests" }
                        th { "Avg Duration" }
                        th { "Errors" }
                        th { "Last Seen" }
                    }
                }
                tbody {
                    @if summary.is_empty() {
                        tr {
                            td colspan="7" .text-center .text-muted style="padding: 2rem;" { "No inbound requests yet" }
                        }
                    }
                    @for row in &summary {
                        @let method = row.data.get("method").and_then(|v| v.as_str()).unwrap_or("");
                        @let path = row.data.get("path").and_then(|v| v.as_str()).unwrap_or("");
                        @let cnt = row.data.get("cnt").and_then(|v| v.as_i64()).unwrap_or(0);
                        // `db::aggregate`'s Avg has no result-cast (unlike the old
                        // `cast_as: Some("INTEGER")` builder path), so AVG(duration_ms)
                        // comes back as a JSON float; `as_i64()` is always `None` for the
                        // `Number::Float` variant, so read it as f64 and truncate. The old
                        // `CAST(AVG(duration_ms) AS INTEGER)` truncated toward zero;
                        // `duration_ms` is always >= 0, so `as i64` (which also truncates
                        // toward zero) is exact parity — no `.round()`.
                        @let avg_ms = row.data.get("avg_ms").and_then(|v| v.as_f64()).map(|v| v as i64).unwrap_or(0);
                        @let errors = row.data.get("errors").and_then(|v| v.as_i64()).unwrap_or(0);
                        @let last_seen = row.data.get("last_seen").and_then(|v| v.as_str()).unwrap_or("");
                        (inbound_row(method, path, cnt, avg_ms, errors, last_seen))
                    }
                }
            }
        }
    }
}

/// Render one inbound-summary row: the clickable row plus its lazily-loaded
/// detail row. `method`/`path` come from the request log and are
/// attacker-controlled (any HTTP request with a crafted path is logged), so
/// they appear only in maud-escaped attribute/text contexts. The row carries
/// `data-detail-target`/`data-detail-url` that the delegated click handler
/// reads — never an `onclick` JS-string literal (maud doesn't escape JS-string
/// context, which was a stored-XSS sink).
fn inbound_row(
    method: &str,
    path: &str,
    cnt: i64,
    avg_ms: i64,
    errors: i64,
    last_seen: &str,
) -> Markup {
    let row_id = format!("inbound-{}-{}", method, path.replace('/', "_"));
    let detail_url = format!("/b/admin/network/detail/inbound?method={method}&path={path}");
    html! {
        tr .expand-row data-detail-target=(row_id) data-detail-url=(detail_url) {
            td .text-muted { (icons::chevron_right()) }
            td .text-sm .font-medium { (method.to_uppercase()) }
            td .text-sm { (path) }
            td .text-sm {
                span .badge .badge-info { (cnt) }
            }
            td .text-muted .text-sm { (avg_ms) "ms" }
            td .text-sm {
                @if errors > 0 {
                    span .badge .badge-danger { (errors) }
                } @else {
                    span .text-muted { "0" }
                }
            }
            td .text-muted .text-sm { (last_seen.get(..19).unwrap_or(last_seen)) }
        }
        tr .detail-rows hidden {
            td colspan="7" style="padding:0" {
                div id=(row_id) {}
            }
        }
    }
}

/// Htmx fragment: individual requests for a given inbound path.
pub async fn network_inbound_detail(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let method = msg.query("method").to_string();
    let path = msg.query("path").to_string();
    let offset: i64 = msg.query("offset").parse().unwrap_or(0);
    let limit: i64 = 20;

    let rows = db::list(
        ctx,
        REQUEST_LOGS,
        &ListOptions {
            columns: Some(vec![
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
                    value: serde_json::json!(&method),
                },
                Filter {
                    field: "path".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(&path),
                },
            ],
            sort: vec![SortField {
                field: "created_at".into(),
                desc: true,
            }],
            limit: limit + 1, // fetch one extra to detect "has more"
            offset,
            skip_count: true,
            ..Default::default()
        },
    )
    .await
    .map(|r| r.records)
    .unwrap_or_default();

    let has_more = rows.len() as i64 > limit;
    let display_rows = if has_more {
        &rows[..limit as usize]
    } else {
        &rows
    };

    let markup = html! {
        table .table style="margin:0" {
            thead {
                tr {
                    th { "Status" }
                    th { "Duration" }
                    th { "IP" }
                    th { "User" }
                    th { "Time" }
                }
            }
            tbody {
                @for record in display_rows {
                    @let status_code = record.i64_field("status_code");
                    @let duration = record.i64_field("duration_ms");
                    @let client_ip = record.data.get("client_ip").and_then(|v| v.as_str()).unwrap_or("");
                    @let user_id = record.data.get("user_id").and_then(|v| v.as_str()).unwrap_or("");
                    @let created = record.data.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
                    tr {
                        td {
                            span .badge .(if status_code >= 500 { "badge-danger" } else if status_code >= 400 { "badge-warning" } else { "badge-success" }) {
                                (status_code)
                            }
                        }
                        td .text-muted { (duration) "ms" }
                        td .text-muted { (client_ip) }
                        td .text-muted {
                            @if !user_id.is_empty() {
                                (user_id.get(..8).unwrap_or(user_id))
                            }
                        }
                        td .text-muted { (created.get(..19).unwrap_or(created)) }
                    }
                }
            }
        }
        @if has_more {
            @let next_offset = offset + limit;
            div style="text-align:center;padding:8px" {
                button .btn .btn-secondary .btn-sm
                    hx-get={"/b/admin/network/detail/inbound?method=" (method) "&path=" (path) "&offset=" (next_offset)}
                    hx-target="closest div"
                    hx-swap="outerHTML"
                { "Load more" }
            }
        }
    };
    crate::ui::html_response(markup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_row_has_no_js_string_xss_sink() {
        // Attacker-controlled request path crafted to break out of the old
        // `onclick="toggleDetail('…')"` JS-string literal.
        let html = inbound_row(
            "GET",
            "'); alert(document.cookie); //",
            1,
            2,
            0,
            "2026-01-01T00:00:00Z",
        )
        .into_string();

        // The JS-string sink is gone entirely.
        assert!(
            !html.contains("onclick"),
            "must not emit an onclick JS-string sink: {html}"
        );
        // Replaced by maud-escaped data-* attributes the delegated handler reads.
        assert!(
            html.contains("data-detail-target="),
            "row must carry data-detail-target: {html}"
        );
        assert!(
            html.contains("data-detail-url="),
            "row must carry data-detail-url: {html}"
        );
        // maud escapes the attribute value (e.g. the URL's `&`), proving the
        // path lands in escaped attribute context, not a raw/JS sink.
        assert!(
            html.contains("method=GET&amp;path="),
            "detail URL must be HTML-escaped in the attribute: {html}"
        );
        assert!(
            !html.contains("method=GET&path="),
            "a raw unescaped query would mean an injection sink survived: {html}"
        );
    }
}
