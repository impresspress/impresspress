use maud::{html, Markup};
use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream, WaferError};

use super::{admin_page, crumb, status_code_badge_variant};
use crate::{
    blocks::{admin::AUDIT_LOGS_TABLE as AUDIT_LOGS, crud},
    platform_state::request_logs,
    ui::{
        components::{self, badge, pagination, Badge, BadgeVariant},
        icons,
        shell::Topbar,
        templates::list_page,
    },
    util::{urlencode, RecordExt},
};

/// The query parameter the System Logs tab narrows to error rows with, and
/// the one value that turns it on. The rows it selects are the ones
/// [`request_logs::is_error_status`] answers for — a `status_code` filter.
/// The stored `status` label is a display column no reader selects on, so the
/// parameter is not named after it.
const ERRORS_PARAM: &str = "errors";
const ERRORS_ON: &str = "1";

/// Whether this request asked for error rows only.
fn errors_only(msg: &Message) -> bool {
    msg.query(ERRORS_PARAM) == ERRORS_ON
}

/// `/b/admin/logs` carrying the System Logs tab's active filters, so a
/// refresh, a search, a page step or the filter toggle itself keeps the rest
/// of the filter. `search` is omitted where the link's own control supplies
/// it (the search box appends its field to the URL it is given).
fn system_logs_href(search: &str, errors_only: bool) -> String {
    let mut params: Vec<String> = Vec::new();
    if errors_only {
        params.push(format!("{ERRORS_PARAM}={ERRORS_ON}"));
    }
    if !search.is_empty() {
        params.push(format!("search={}", urlencode(search)));
    }
    if params.is_empty() {
        "/b/admin/logs".to_string()
    } else {
        format!("/b/admin/logs?{}", params.join("&"))
    }
}

pub async fn logs_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let tab = msg.query("tab");
    let active_tab = match tab {
        "audit" => "audit",
        _ => "system",
    };

    let refresh_href = if active_tab == "audit" {
        "/b/admin/logs?tab=audit".to_string()
    } else {
        system_logs_href(msg.query("search"), errors_only(msg))
    };
    let refresh_action = html! {
        button .btn .btn--secondary .btn--sm
            hx-get=(refresh_href)
            hx-target="#content"
        { (icons::refresh_cw()) " Refresh" }
    };

    // "No request logs yet" is what an untouched deployment renders; a log
    // that could not be read must not borrow it, nor print the failure's own
    // text into the page.
    let tab_body = if active_tab == "system" {
        system_logs_tab(ctx, msg).await
    } else {
        audit_logs_tab(ctx, msg).await
    };
    let tab_body = match tab_body {
        Ok(markup) => markup,
        Err(e) => return crud::db_error_page(msg, e, "admin logs page: log read failed"),
    };

    let tabs_and_body = html! {
        (components::tab_navigation(vec![
            components::Tab {
                active: active_tab == "system",
                href: "/b/admin/logs",
                label: "System Logs",
                icon: Some(icons::server()),
            },
            components::Tab {
                active: active_tab == "audit",
                href: "/b/admin/logs?tab=audit",
                label: "Audit Logs",
                icon: Some(icons::file_text()),
            },
        ]))

        div #logs-tab-content { (tab_body) }
    };

    let body = list_page(None, tabs_and_body, None);

    admin_page(
        ctx,
        msg,
        "Logs",
        Topbar {
            crumbs: crumb("Logs"),
            primary_action: Some(refresh_action),
            subtitle: Some("System telemetry and admin audit trail"),
            show_palette: true,
        },
        body,
    )
    .await
}

/// `Err` when the read failed; [`logs_page`] answers it, never an empty table.
async fn system_logs_tab(ctx: &dyn Context, msg: &Message) -> Result<Markup, WaferError> {
    let (page, page_size, _) = msg.pagination_params(50);
    let search = msg.query("search").to_string();
    let errors_only = errors_only(msg);

    let list =
        request_logs::paginated(ctx, page as i64, page_size as i64, &search, errors_only).await?;

    // The search box appends its own `search` field to whatever URL it is
    // given, so it gets the filter without one; everything else carries both.
    let search_href = system_logs_href("", errors_only);
    let all_rows_href = system_logs_href(&search, false);
    let errors_href = system_logs_href(&search, true);
    let page_href = system_logs_href(&search, errors_only);

    Ok(html! {
        div .filter-bar {
            (components::search_input_with_value("search", "Search by path...", &search_href, "#content", &search))

            @if errors_only {
                div .flex .items-center .gap-2 .mb-2 .text-sm {
                    span .text-muted { "Errors only (status " (request_logs::ERROR_STATUS_FLOOR) "+)" }
                    a .btn .btn--ghost .btn--sm
                        href=(all_rows_href)
                        hx-get=(all_rows_href)
                        hx-target="#content"
                    { (icons::x()) " Show all" }
                }
            } @else {
                a .btn .btn--ghost .btn--sm
                    href=(errors_href)
                    hx-get=(errors_href)
                    hx-target="#content"
                { (icons::triangle_alert()) " Errors only" }
            }
        }

        @let rows: Vec<Vec<Markup>> = list.rows.iter().map(|row| {
            let path = row.path.as_str();
            let user_id = row.user_id.as_str();
            let created = row.created_at.as_str();
            let status_code = row.status_code;
            vec![
                Badge::new(status_code_badge_variant(status_code)).render(html! { (status_code) }),
                html! { span .font-medium { (row.method.to_uppercase()) } },
                html! { (path) },
                html! { span .text-muted { (row.duration_ms) "ms" } },
                html! {
                    @if !user_id.is_empty() {
                        span .text-muted { (user_id.get(..8).unwrap_or(user_id)) }
                    }
                },
                html! { span .text-muted { (created.get(..19).unwrap_or(created)) } },
            ]
        }).collect();

        (components::data_table::<fn(usize) -> Option<String>>(
            &SYSTEM_LOG_COLUMNS,
            rows,
            None,
            html! {
                p .text-center .text-muted {
                    @if errors_only { "No error request logs" } @else { "No request logs yet" }
                }
            },
        ))

        @if let Some(per_page) = std::num::NonZeroU32::new(page_size as u32) {
            (pagination(list.page as u32, per_page, list.total_count as u32, &page_href))
        }
    })
}

/// `Err` when the read failed; [`logs_page`] answers it, never an empty table.
async fn audit_logs_tab(ctx: &dyn Context, msg: &Message) -> Result<Markup, WaferError> {
    let (page, page_size, _) = msg.pagination_params(50);
    let search = msg.query("search").to_string();

    let mut filters = Vec::new();
    if !search.is_empty() {
        filters.push(Filter {
            field: "resource".into(),
            operator: FilterOp::Like,
            value: serde_json::Value::String(format!("%{search}%")),
        });
    }

    let sort = vec![SortField {
        field: "created_at".into(),
        desc: true,
    }];
    let list = db::paginated_list(
        ctx,
        AUDIT_LOGS,
        page as i64,
        page_size as i64,
        filters,
        sort,
    )
    .await?;

    Ok(html! {
        div .filter-bar {
            (components::search_input_with_value("search", "Search by resource...", "/b/admin/logs?tab=audit", "#content", &search))
        }

        @let rows: Vec<Vec<Markup>> = list.records.iter().map(|record| {
            let user_id = record.str_field("user_id");
            let created = record.str_field("created_at");
            vec![
                badge(BadgeVariant::Info, record.str_field("action")),
                html! { (record.str_field("resource")) },
                html! { span .text-muted { (user_id.get(..8).unwrap_or(user_id)) } },
                html! { span .text-muted { (record.str_field("ip_address")) } },
                html! { span .text-muted { (created.get(..19).unwrap_or(created)) } },
            ]
        }).collect();

        (components::data_table::<fn(usize) -> Option<String>>(
            &AUDIT_LOG_COLUMNS,
            rows,
            None,
            html! { p .text-center .text-muted { "No audit logs yet" } },
        ))

        @if let Some(per_page) = std::num::NonZeroU32::new(page_size as u32) {
            (pagination(list.page as u32, per_page, list.total_count as u32, "/b/admin/logs?tab=audit"))
        }
    })
}

/// The two log tables' columns. Declared once each so the `<td data-label>`
/// the component stamps on every cell names the same column its header does.
const SYSTEM_LOG_COLUMNS: [components::TableCol<'static>; 6] = [
    components::TableCol {
        label: "Status",
        width: None,
    },
    components::TableCol {
        label: "Method",
        width: None,
    },
    components::TableCol {
        label: "Path",
        width: None,
    },
    components::TableCol {
        label: "Duration",
        width: None,
    },
    components::TableCol {
        label: "User",
        width: None,
    },
    components::TableCol {
        label: "Time",
        width: None,
    },
];

const AUDIT_LOG_COLUMNS: [components::TableCol<'static>; 5] = [
    components::TableCol {
        label: "Action",
        width: None,
    },
    components::TableCol {
        label: "Resource",
        width: None,
    },
    components::TableCol {
        label: "User",
        width: None,
    },
    components::TableCol {
        label: "IP",
        width: None,
    },
    components::TableCol {
        label: "Time",
        width: None,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::admin::test_support::browser_request,
        platform_state::request_logs::NewRequestLog,
        test_support::{admin_msg, TestContext},
        util::parse_form_body,
    };

    /// One stored request, with the status code the client was served.
    async fn seed(ctx: &TestContext, path: &str, status_code: i64) {
        request_logs::insert(
            ctx,
            &NewRequestLog {
                method: "GET",
                path,
                status_code,
                error_message: "",
                duration_ms: 1,
                client_ip: "203.0.113.7",
                user_id: "",
            },
        )
        .await
        .unwrap_or_else(|e| panic!("seed {path}: {e}"));
    }

    /// One 200 row and two error rows (a 404 and a 500), and the rendered
    /// Logs page for `query`. Two error rows so a page of one still has a
    /// next page under the filter.
    async fn logs_html(query: &[(&str, &str)]) -> String {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed(&ctx, "/served-ok", 200).await;
        seed(&ctx, "/served-missing", 404).await;
        seed(&ctx, "/served-boom", 500).await;
        render_logs(&ctx, query).await
    }

    /// The Logs page for `query`, over `ctx`, through the block's own route.
    async fn render_logs(ctx: &TestContext, query: &[(&str, &str)]) -> String {
        let mut msg = admin_msg("retrieve", "/b/admin/logs");
        for (name, value) in query {
            msg.set_meta(format!("req.query.{name}"), *value);
        }
        let parts = browser_request(ctx, msg).await;
        assert_eq!(parts.status, 200);
        // Attribute values are HTML-escaped; compare against what a browser
        // would request.
        String::from_utf8(parts.body)
            .expect("UTF-8 page")
            .replace("&amp;", "&")
    }

    /// Every `/b/admin/logs` link `html` emits, in document order.
    fn logs_links(html: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = html;
        while let Some(pos) = rest.find("\"/b/admin/logs") {
            let after = &rest[pos + 1..];
            let end = after.find('"').expect("attribute value is terminated");
            out.push(after[..end].to_string());
            rest = &after[end..];
        }
        out
    }

    /// The filter the dashboard's error links ask for: only rows whose
    /// `status_code` is an error, by the one rule every reader uses.
    #[tokio::test]
    async fn the_logs_page_narrows_to_error_rows() {
        let unfiltered = logs_html(&[]).await;
        assert!(unfiltered.contains("/served-ok"), "{unfiltered}");
        assert!(unfiltered.contains("/served-boom"), "{unfiltered}");

        let filtered = logs_html(&[("errors", "1")]).await;
        assert!(
            !filtered.contains("/served-ok"),
            "a 200 row must not be listed under the error filter: {filtered}"
        );
        assert!(
            filtered.contains("/served-boom") && filtered.contains("/served-missing"),
            "both error rows must still be listed: {filtered}"
        );
        assert!(
            filtered.contains("2 total"),
            "the count is of the filtered set: {filtered}"
        );
    }

    /// The link the dashboard emits and the filter the Logs page reads are
    /// one contract: whatever the dashboard's "Recent Errors" and error-chart
    /// cards link to must narrow the page. A link naming a parameter the page
    /// does not read is silent — it opens the unfiltered list — so nothing but
    /// a test that follows the link itself can catch the two drifting apart.
    #[tokio::test]
    async fn the_dashboard_error_links_filter_the_logs_page() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed(&ctx, "/served-ok", 200).await;
        seed(&ctx, "/served-boom", 500).await;

        let dashboard = browser_request(&ctx, admin_msg("retrieve", "/b/admin/")).await;
        assert_eq!(dashboard.status, 200);
        let html = String::from_utf8(dashboard.body)
            .expect("UTF-8 page")
            .replace("&amp;", "&");
        let error_links: Vec<String> = logs_links(&html)
            .into_iter()
            .filter(|link| link.contains('?'))
            .collect();
        assert!(
            !error_links.is_empty(),
            "the dashboard must link to the filtered Logs page: {html}"
        );

        for link in error_links {
            let (_, query) = link.split_once('?').expect("filtered by the loop above");
            let params = parse_form_body(query.as_bytes());
            let pairs: Vec<(&str, &str)> = params
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let page = render_logs(&ctx, &pairs).await;
            assert!(
                !page.contains("/served-ok"),
                "{link} must narrow the Logs page to error rows: {page}"
            );
            assert!(page.contains("/served-boom"), "{link}: {page}");
        }
    }

    /// The filter survives the controls on the page: paging, searching,
    /// refreshing and clearing the search all keep it, and the page offers a
    /// way out of it.
    #[tokio::test]
    async fn the_active_filter_rides_on_every_link_the_page_emits() {
        // One row per page, so the next-page link is a real second page of
        // the two error rows rather than a clamp back to page 1.
        let html = logs_html(&[("errors", "1"), ("search", "served"), ("page_size", "1")]).await;

        assert!(
            html.contains("/b/admin/logs?errors=1&search=served&page=2"),
            "the next page keeps both filters: {html}"
        );
        // The search box appends its own field, so its URL carries the error
        // filter alone — a `search` in it would be sent twice.
        assert!(
            html.contains("hx-get=\"/b/admin/logs?errors=1\""),
            "the search box and the Clear-search link keep the error filter: {html}"
        );
        assert!(
            html.contains("/b/admin/logs?search=served\""),
            "the page offers a way back to all rows, keeping the search: {html}"
        );
        assert!(
            html.contains("Errors only"),
            "the active filter is named on the page: {html}"
        );
        // The search box's own Clear and the error filter's way out are two
        // controls; they must not share a label.
        assert_eq!(
            html.matches(" Clear<").count(),
            1,
            "only the search box offers Clear: {html}"
        );
        assert!(
            html.contains(" Show all<"),
            "the error filter's way out is labelled apart from Clear: {html}"
        );

        // A search the URL must encode rides along the same way.
        let encoded = logs_html(&[("errors", "1"), ("search", "a b")]).await;
        assert!(
            encoded.contains("/b/admin/logs?errors=1&search=a+b&page="),
            "the search is form-encoded in the links: {encoded}"
        );
    }

    /// Without the parameter the page lists every row and offers the filter.
    #[tokio::test]
    async fn the_unfiltered_page_offers_the_filter() {
        let html = logs_html(&[]).await;
        assert!(
            html.contains("/b/admin/logs?errors=1"),
            "the filter must be reachable from the page: {html}"
        );
        assert!(html.contains("3 total"), "{html}");
    }
}
