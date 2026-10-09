use maud::{html, Markup};
use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream, WaferError};

use super::{admin_page, crumb, request_path_cell, status_code_badge_variant, user_cell};
use crate::{
    blocks::{admin::AUDIT_LOGS_TABLE as AUDIT_LOGS, auth::repo::users},
    platform_state::request_logs::{self, ErrorFilter},
    ui::{
        components::{self, badge, pagination, Badge, BadgeVariant},
        icons,
        shell::Topbar,
        templates::list_page,
    },
    util::{urlencode, RecordExt},
};

/// The query parameters the System Logs tab's two error filters set, and the
/// one value that turns each on. The rows each selects are a
/// [`request_logs::StatusClass`] — a `status_code` filter. The stored
/// `status` label is a display column no reader selects on, so neither
/// parameter is named after it.
const SERVER_ERRORS_PARAM: &str = "server_errors";
const CLIENT_ERRORS_PARAM: &str = "client_errors";
const FILTER_ON: &str = "1";

/// The error filters this request asked for.
fn error_filter(msg: &Message) -> ErrorFilter {
    ErrorFilter {
        server: msg.query(SERVER_ERRORS_PARAM) == FILTER_ON,
        client: msg.query(CLIENT_ERRORS_PARAM) == FILTER_ON,
    }
}

/// `/b/admin/logs` carrying the System Logs tab's active filters, so a
/// refresh, a search, a page step or a filter toggle keeps the rest of the
/// filter. `search` is omitted where the link's own control supplies it (the
/// search box appends its field to the URL it is given).
fn system_logs_href(search: &str, errors: ErrorFilter) -> String {
    let mut params: Vec<String> = Vec::new();
    if errors.server {
        params.push(format!("{SERVER_ERRORS_PARAM}={FILTER_ON}"));
    }
    if errors.client {
        params.push(format!("{CLIENT_ERRORS_PARAM}={FILTER_ON}"));
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

/// The Logs page's storage-access tab — where `/b/admin/storage` redirects.
pub const STORAGE_LOGS_HREF: &str = "/b/admin/logs?tab=storage";

pub async fn logs_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let tab = msg.query("tab");
    let active_tab = match tab {
        "audit" => "audit",
        "storage" => "storage",
        _ => "system",
    };

    let refresh_href = match active_tab {
        "audit" => "/b/admin/logs?tab=audit".to_string(),
        "storage" => STORAGE_LOGS_HREF.to_string(),
        _ => system_logs_href(msg.query("search"), error_filter(msg)),
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
    let tab_body = match active_tab {
        "system" => system_logs_tab(ctx, msg).await,
        "storage" => super::storage::storage_logs_tab(ctx, msg).await,
        _ => audit_logs_tab(ctx, msg).await,
    };
    let tab_body = match tab_body {
        Ok(markup) => markup,
        Err(e) => {
            return super::admin_error_page(ctx, msg, "Logs", e, "admin logs page: log read failed")
                .await
        }
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
            components::Tab {
                active: active_tab == "storage",
                href: STORAGE_LOGS_HREF,
                label: "Storage Access",
                icon: Some(icons::hard_drive()),
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
            actions: vec![refresh_action],
            subtitle: Some("System telemetry, admin audit trail and storage access"),
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
    let errors = error_filter(msg);

    let list = request_logs::paginated(ctx, page as i64, page_size as i64, &search, errors).await?;
    let user_ids: Vec<&str> = list.rows.iter().map(|r| r.user_id.as_str()).collect();
    let emails = users::emails_by_id(ctx, &user_ids).await?;

    // The search box appends its own `search` field to whatever URL it is
    // given, so it gets the filters without one; everything else carries all.
    let search_href = system_logs_href("", errors);
    let server_toggle_href = system_logs_href(
        &search,
        ErrorFilter {
            server: !errors.server,
            ..errors
        },
    );
    let client_toggle_href = system_logs_href(
        &search,
        ErrorFilter {
            client: !errors.client,
            ..errors
        },
    );
    let page_href = system_logs_href(&search, errors);
    let search_box = components::SearchInput {
        id: "system-logs-search",
        name: "search",
        label: "Search by path...",
        href: &search_href,
        value: &search,
        result_count: u64::try_from(list.total_count).unwrap_or(0),
    };
    let empty = match (errors.server, errors.client) {
        (true, true) => "No server or client errors logged",
        (true, false) => "No server errors logged",
        (false, true) => "No client errors logged",
        (false, false) => "No request logs yet",
    };

    Ok(html! {
        div .filter-bar {
            (search_box.render())
            div .filter-toggles role="group" aria-label="Show only" {
                (components::filter_toggle("Server errors", errors.server, &server_toggle_href))
                (components::filter_toggle("Client errors", errors.client, &client_toggle_href))
            }
        }

        @let rows: Vec<Vec<Markup>> = list.rows.iter().map(|row| {
            let created = row.created_at.as_str();
            let status_code = row.status_code;
            vec![
                Badge::new(status_code_badge_variant(status_code)).render(html! { (status_code) }),
                html! { span .font-medium { (row.method) } },
                request_path_cell(&row.path),
                html! { span .text-muted .tabular-nums { (row.duration_ms) "ms" } },
                user_cell(&row.user_id, &emails),
                html! { span .text-muted { (components::timestamp(created)) } },
            ]
        }).collect();

        (components::data_table::<fn(usize) -> Option<String>>(
            &SYSTEM_LOG_COLUMNS,
            rows,
            None,
            html! { p .text-center .text-muted { (empty) } },
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
            operator: FilterOp::ContainsIgnoreCase,
            value: serde_json::Value::String(search.clone()),
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
    let user_ids: Vec<&str> = list
        .records
        .iter()
        .map(|record| record.str_field("user_id"))
        .collect();
    let emails = users::emails_by_id(ctx, &user_ids).await?;
    let search_box = components::SearchInput {
        id: "audit-logs-search",
        name: "search",
        label: "Search by resource...",
        href: "/b/admin/logs?tab=audit",
        value: &search,
        result_count: u64::try_from(list.total_count).unwrap_or(0),
    };

    Ok(html! {
        div .filter-bar {
            (search_box.render())
        }

        @let rows: Vec<Vec<Markup>> = list.records.iter().map(|record| {
            let created = record.str_field("created_at");
            vec![
                badge(BadgeVariant::Info, record.str_field("action")),
                html! { (record.str_field("resource")) },
                user_cell(record.str_field("user_id"), &emails),
                html! { span .text-muted { (record.str_field("ip_address")) } },
                html! { span .text-muted { (components::timestamp(created)) } },
            ]
        }).collect();

        (components::data_table::<fn(usize) -> Option<String>>(
            &AUDIT_LOG_COLUMNS,
            rows,
            None,
            html! { p .text-center .text-muted { "No audit logs yet" } },
        ))

        @if let Some(per_page) = std::num::NonZeroU32::new(page_size as u32) {
            (pagination(list.page as u32, per_page, list.total_count as u32, &search_box.results_href()))
        }
    })
}

/// The two log tables' columns. Declared once each so the `<td data-label>`
/// the component stamps on every cell names the same column its header does.
const SYSTEM_LOG_COLUMNS: [components::TableCol<'static>; 6] = [
    components::TableCol::new("Status"),
    components::TableCol::new("Method"),
    components::TableCol::new("Path").primary(),
    components::TableCol::new("Duration"),
    components::TableCol::new("User").optional(),
    components::TableCol::new("Time"),
];

const AUDIT_LOG_COLUMNS: [components::TableCol<'static>; 5] = [
    components::TableCol::new("Action"),
    components::TableCol::new("Resource").primary(),
    components::TableCol::new("User"),
    components::TableCol::new("IP"),
    components::TableCol::new("Time"),
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

    /// Each filter narrows to its own class by `status_code`, and both
    /// together list every error row.
    #[tokio::test]
    async fn the_two_filters_narrow_to_their_own_class() {
        let unfiltered = logs_html(&[]).await;
        for path in ["/served-ok", "/served-missing", "/served-boom"] {
            assert!(unfiltered.contains(path), "{path}: {unfiltered}");
        }

        let server = logs_html(&[("server_errors", "1")]).await;
        assert!(server.contains("/served-boom"), "{server}");
        assert!(
            !server.contains("/served-ok") && !server.contains("/served-missing"),
            "a 200 and a 404 are not server errors: {server}"
        );

        let client = logs_html(&[("client_errors", "1")]).await;
        assert!(client.contains("/served-missing"), "{client}");
        assert!(
            !client.contains("/served-ok") && !client.contains("/served-boom"),
            "a 200 and a 500 are not client errors: {client}"
        );

        let both = logs_html(&[
            ("server_errors", "1"),
            ("client_errors", "1"),
            ("page_size", "1"),
        ])
        .await;
        assert!(
            both.contains("2 total"),
            "both filters list both error rows, and the count is of that set: {both}"
        );
    }

    /// Each filter is a toggle button whose `aria-pressed` is its state, and
    /// which leads to the same list with that one filter flipped.
    #[tokio::test]
    async fn each_filter_is_a_pressed_toggle_that_flips_only_itself() {
        let html = logs_html(&[("server_errors", "1"), ("search", "served")]).await;
        assert!(
            html.contains(r#"aria-pressed="true" hx-get="/b/admin/logs?search=served""#),
            "pressing the active server filter turns it off and keeps the search: {html}"
        );
        assert!(
            html.contains(
                r#"aria-pressed="false" hx-get="/b/admin/logs?server_errors=1&client_errors=1&search=served""#
            ),
            "pressing the client filter adds it beside the server filter: {html}"
        );
        assert!(!html.contains("4xx/5xx"), "{html}");
    }

    /// The links the dashboard emits and the filters the Logs page reads are
    /// one contract: whatever the dashboard's server-error card and charts
    /// link to must narrow the page. A link naming a parameter the page does
    /// not read is silent — it opens the unfiltered list — so nothing but a
    /// test that follows the link itself can catch the two drifting apart.
    #[tokio::test]
    async fn the_dashboard_error_links_filter_the_logs_page() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed(&ctx, "/served-ok", 200).await;
        seed(&ctx, "/served-missing", 404).await;
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
            error_links.iter().any(|l| l.contains("server_errors"))
                && error_links.iter().any(|l| l.contains("client_errors")),
            "the dashboard must link to both filtered Logs pages: {error_links:?}"
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
            let (wanted, unwanted) = if link.contains("server_errors") {
                ("/served-boom", "/served-missing")
            } else {
                ("/served-missing", "/served-boom")
            };
            assert!(page.contains(wanted), "{link}: {page}");
            assert!(!page.contains(unwanted), "{link}: {page}");
        }
    }

    /// The filters survive the controls on the page: paging and searching
    /// keep them.
    #[tokio::test]
    async fn the_active_filter_rides_on_every_link_the_page_emits() {
        // One row per page, so the next-page link is a real second page of
        // the two error rows rather than a clamp back to page 1.
        let html = logs_html(&[
            ("server_errors", "1"),
            ("client_errors", "1"),
            ("search", "served"),
            ("page_size", "1"),
        ])
        .await;

        assert!(
            html.contains("/b/admin/logs?server_errors=1&client_errors=1&search=served&page=2"),
            "the next page keeps every filter: {html}"
        );
        // The search box appends its own field, so its URL carries the error
        // filters alone — a `search` in it would be sent twice.
        assert!(
            html.contains(r#"hx-get="/b/admin/logs?server_errors=1&client_errors=1""#),
            "the search box and the Clear-search link keep the error filters: {html}"
        );

        // A search the URL must encode rides along the same way.
        let encoded = logs_html(&[
            ("client_errors", "1"),
            ("server_errors", "1"),
            ("search", "/served"),
            ("page_size", "1"),
        ])
        .await;
        assert!(
            encoded
                .contains("/b/admin/logs?server_errors=1&client_errors=1&search=%2Fserved&page="),
            "the search is form-encoded in the links: {encoded}"
        );
    }

    /// Without the parameters the page lists every row and offers both
    /// filters, neither pressed.
    #[tokio::test]
    async fn the_unfiltered_page_offers_both_filters() {
        let html = logs_html(&[("page_size", "1")]).await;
        assert_eq!(html.matches(r#"aria-pressed="false""#).count(), 2, "{html}");
        assert!(html.contains("/b/admin/logs?server_errors=1"), "{html}");
        assert!(html.contains("/b/admin/logs?client_errors=1"), "{html}");
        assert!(html.contains("3 total"), "{html}");
    }

    /// The collapsed unmatched-path label reads as what it stands for, and a
    /// signed-in request names its account by email, not by an id prefix.
    #[tokio::test]
    async fn rows_name_the_unmatched_route_and_the_user_by_email() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.seed_auth_user("u-1").await;
        request_logs::insert(
            &ctx,
            &NewRequestLog {
                method: "GET",
                path: crate::pipeline::UNMATCHED_PATH_LABEL,
                status_code: 404,
                error_message: "",
                duration_ms: 1,
                client_ip: "203.0.113.7",
                user_id: "u-1",
            },
        )
        .await
        .expect("seed");
        let html = render_logs(&ctx, &[]).await;
        assert!(html.contains("Unmatched route"), "{html}");
        assert!(!html.contains("&lt;unmatched&gt;"), "{html}");
        assert!(
            html.contains("u-1<wbr>@example"),
            "the fixture account's email, u-1@example.com: {html}"
        );
    }
}

#[cfg(test)]
mod storage_tab_tests {
    //! "No storage access logs yet." is what a deployment whose blocks have
    //! never touched storage renders. An outage rendered the same sentence.

    use super::*;
    use crate::test_support::{admin_msg, output_http_status, TestContext};

    #[tokio::test]
    async fn a_failing_access_log_read_renders_the_error_page_not_an_empty_log() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();
        let mut msg = admin_msg("retrieve", "/b/admin/logs");
        msg.set_meta("req.query.tab", "storage");
        assert_eq!(output_http_status(logs_page(&ctx, &msg).await).await, 500);
    }
}
