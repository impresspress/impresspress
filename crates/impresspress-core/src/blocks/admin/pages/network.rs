use std::collections::HashMap;

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use super::{request_path_cell, status_code_badge_variant, user_cell};
use crate::{
    blocks::auth::repo::users,
    platform_state::request_logs::{self, PathSort, PathSummary, RouteQuery},
    ui::{
        components::{self, Badge},
        icons,
    },
    util::urlencode,
};

/// Routes per page.
const PAGE_SIZE: u32 = 50;

/// Where this page lives; every control on it links back here.
const NETWORK_HREF: &str = "/b/admin/settings/network";

/// The page's controls, as the request asked for them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Query {
    search: String,
    sort: PathSort,
    errors_only: bool,
    page: u32,
}

impl Query {
    fn from_msg(msg: &Message) -> Self {
        Self {
            search: msg.query("search").to_string(),
            sort: match msg.query("sort") {
                "errors" => PathSort::Errors,
                "recent" => PathSort::Recent,
                _ => PathSort::Requests,
            },
            errors_only: msg.query("errors") == "1",
            page: msg.query("page").parse().unwrap_or(1).max(1),
        }
    }

    /// This page with these controls. `search` is left out where the
    /// control that uses the link supplies it (the search box appends its
    /// own field), and `page` always is: changing what is listed starts it
    /// over, and the pagination appends its own.
    /// [`href`](Self::href) with the search, at page `page` (the first page
    /// carries no `page`).
    fn href_to_page(&self, page: u32) -> String {
        let href = self.href(true);
        if page <= 1 {
            href
        } else if href.contains('?') {
            format!("{href}&page={page}")
        } else {
            format!("{href}?page={page}")
        }
    }

    fn href(&self, with_search: bool) -> String {
        let mut params: Vec<String> = Vec::new();
        match self.sort {
            PathSort::Requests => {}
            PathSort::Errors => params.push("sort=errors".into()),
            PathSort::Recent => params.push("sort=recent".into()),
        }
        if self.errors_only {
            params.push("errors=1".into());
        }
        if with_search && !self.search.is_empty() {
            params.push(format!("search={}", urlencode(&self.search)));
        }
        if params.is_empty() {
            NETWORK_HREF.to_string()
        } else {
            format!("{NETWORK_HREF}?{}", params.join("&"))
        }
    }
}

/// What the network tab renders: its body, or — for a page number past the
/// last page — the URL of the last page, which the parent redirects to so the
/// address bar names the page on screen.
pub enum NetworkBody {
    Page(Markup),
    Moved(String),
}

/// Render JUST the network monitoring body. The parent `settings_page`
/// handler wraps this in the form-less `tabbed_page` shell. This tab is
/// read-only monitoring — it renders no `<form>` and has nothing to save.
///
/// Returns `Err` when the monitoring read behind it fails. An outage is
/// precisely when an operator opens this page, and an empty inbound table
/// reads as "nothing has reached this deployment", so the parent renders the
/// error page instead.
pub async fn settings_body(
    ctx: &dyn Context,
    msg: &Message,
) -> Result<NetworkBody, wafer_run::WaferError> {
    let query = Query::from_msg(msg);
    let read = |page: u32| {
        request_logs::route_page(
            ctx,
            RouteQuery {
                search: &query.search,
                sort: query.sort,
                errors_only: query.errors_only,
                limit: PAGE_SIZE,
                offset: i64::from(page - 1) * i64::from(PAGE_SIZE),
            },
        )
    };
    // A page past the last route (a stale link, a filter that shrank the
    // list) comes back empty and so without a count: the first page says
    // how many routes there are, and the request is sent to the last page
    // that has any.
    let page = query.page;
    let routes = read(page).await?;
    if routes.rows.is_empty() && page > 1 {
        let total = read(1).await?.total;
        let last_page = u32::try_from(total.max(1) - 1).unwrap_or(0) / PAGE_SIZE + 1;
        return Ok(NetworkBody::Moved(query.href_to_page(last_page)));
    }
    let totals = request_logs::block_totals(ctx, &query.search).await?;
    let groups = group_by_block(routes.rows);
    let total = usize::try_from(routes.total).unwrap_or(0);
    let first = (page as usize - 1) * PAGE_SIZE as usize + 1;
    let last = first + groups.iter().map(|g| g.rows.len()).sum::<usize>() - 1;

    let sort_href = |sort: PathSort| {
        Query {
            sort,
            ..query.clone()
        }
        .href(true)
    };
    let sorts = [
        ("Requests", PathSort::Requests),
        ("Errors", PathSort::Errors),
        ("Recent", PathSort::Recent),
    ];
    let sort_hrefs: Vec<String> = sorts.iter().map(|(_, sort)| sort_href(*sort)).collect();
    // One of three orders, so links to the three orderings with the current
    // one `aria-current` — not three toggles, which would read as three
    // independent switches.
    let sort_links = components::FilterLinks::new(
        "Sort by",
        sorts
            .iter()
            .zip(&sort_hrefs)
            .map(|((label, sort), href)| components::Tab {
                active: query.sort == *sort,
                href,
                label,
                icon: None,
            })
            .collect(),
    )
    .visible_label("network-sort-label")
    .swap("#content")
    .render();
    let errors_href = Query {
        errors_only: !query.errors_only,
        ..query.clone()
    }
    .href(true);
    let page_href = query.href(true);
    let search_href = query.href(false);
    let search_box = components::SearchInput {
        id: "network-search",
        name: "search",
        label: "Search by path...",
        href: &search_href,
        value: &query.search,
    };

    Ok(NetworkBody::Page(html! {
        div .filter-bar {
            (search_box.render())
            div .network-controls {
                (sort_links)
                (components::filter_toggle("Errors only", query.errors_only, &errors_href))
                button .btn .btn--secondary .btn--sm
                    type="button"
                    hx-get=(page_href)
                    hx-target="#content"
                { (icons::refresh_cw()) " Refresh" }
            }
        }

        @if groups.is_empty() {
            (components::empty_state(
                icons::inbox(),
                if query.errors_only { "No failing routes" } else { "No inbound requests yet" },
                if query.errors_only {
                    "No route matching this view has answered with an error."
                } else {
                    "Requests this deployment serves are listed here by path."
                },
                None,
            ))
        } @else {
            p .network-summary {
                "Showing " (first) "\u{2013}" (last) " of " (total)
                @if total == 1 { " route" } @else { " routes" }
            }
            @for group in &groups {
                (group_section(group, totals.iter().find(|t| t.block == group.block)))
            }
            @if let Some(per_page) = std::num::NonZeroU32::new(PAGE_SIZE) {
                (components::pagination(page, per_page, u32::try_from(total).unwrap_or(u32::MAX), &page_href))
            }
        }

        script { (maud::PreEscaped(NETWORK_JS)) }
    }))
}

/// The routes of one block on this page, in the order the read returned them.
struct Group {
    /// `""` for the paths no block owns ([`request_logs::owning_block`]).
    block: String,
    rows: Vec<PathSummary>,
}

/// Gather a page of routes, which the read ordered by the chosen sort across
/// every block, under one heading per block. The blocks come in the order of
/// their first route on the page, so the page still leads with the route
/// that ranks first; within a block the routes keep the read's order.
fn group_by_block(rows: Vec<PathSummary>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for row in rows {
        match groups.iter_mut().find(|group| group.block == row.block) {
            Some(group) => group.rows.push(row),
            None => groups.push(Group {
                block: row.block.clone(),
                rows: vec![row],
            }),
        }
    }
    groups
}

/// `"1 request"` / `"3 requests"`.
fn count_of(n: i64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// One block's routes on this page: a heading whose button collapses them,
/// the block's totals over every route the search matches (`totals`, from
/// [`request_logs::block_totals`]), and the table.
fn group_section(group: &Group, totals: Option<&request_logs::BlockTotals>) -> Markup {
    let body_id = format!("network-group-{:08x}", components::fnv1a(&group.block));
    let rows: Vec<components::TableRow> = group.rows.iter().map(inbound_row).collect();
    html! {
        section .network-group {
            h3 .network-group__head {
                button .network-group__toggle
                    type="button"
                    aria-expanded="true"
                    aria-controls=(body_id)
                    data-action="network-group-toggle"
                {
                    span .network-group__chevron aria-hidden="true" { (icons::chevron_down()) }
                    @if group.block.is_empty() {
                        span .network-group__name { "Other paths" }
                    } @else {
                        span .network-group__name { "/b/" (group.block) "/" }
                    }
                }
            }
            @if let Some(t) = totals {
                p .network-group__totals {
                    (count_of(t.requests, "request", "requests"))
                    " \u{b7} " (count_of(t.server_errors, "server error", "server errors"))
                    " \u{b7} " (count_of(t.client_errors, "client error", "client errors"))
                    @if !t.last_seen.is_empty() {
                        " \u{b7} last " (components::timestamp(&t.last_seen))
                    }
                }
            }
            div id=(body_id) {
                (components::DataTable::new(&INBOUND_COLUMNS).rows(rows).render())
            }
        }
    }
}

/// The detail endpoint for one route, every value form-encoded: a path is
/// attacker-controlled and may hold `&`, `#`, `?` or `%`, any of which would
/// otherwise split or truncate the query.
fn detail_url(method: &str, path: &str, offset: Option<i64>) -> String {
    let mut url = format!(
        "/b/admin/network/detail/inbound?method={}&path={}",
        urlencode(method),
        urlencode(path)
    );
    if let Some(offset) = offset {
        url.push_str(&format!("&offset={offset}"));
    }
    url
}

/// The element id of a route's detail row: a hash of what the route is, so
/// it is a valid id and selector whatever characters the path holds, and
/// stable across a re-render.
fn detail_id(method: &str, path: &str) -> String {
    format!(
        "network-detail-{:08x}",
        components::fnv1a(&format!("{method} {path}"))
    )
}

/// One route: its expand button, the route (method and path), its counts
/// and timing, and the detail row the button opens.
///
/// `method`/`path` come from the request log and are attacker-controlled (any
/// HTTP request with a crafted path is logged), so they appear only in
/// maud-escaped attribute and text contexts, never in script.
///
/// `avg_ms` and `last_seen` are per-run values — a latency measured by the
/// running deployment and the wall-clock time of a request it served — so
/// each is wrapped in an element the visual-baseline suite keys its masks on
/// (`crates/impresspress-web/tests/e2e/visual-baseline.spec.ts`; it masks a
/// `<time>` directly and the whole cell around a `data-volatile-metric`). The
/// wrappers are inside the cell, because `components::data_table` emits the
/// `<td>` itself and takes only the cell's inner markup.
fn inbound_row(route: &PathSummary) -> components::TableRow {
    let method = route.method.to_uppercase();
    let id = detail_id(&route.method, &route.path);
    let route_name = if route.path == crate::pipeline::UNMATCHED_PATH_LABEL {
        format!("{method} unmatched routes")
    } else {
        format!("{method} {}", route.path)
    };
    components::TableRow::new(vec![
        html! {
            button .btn .btn--ghost .btn--icon .network-row__toggle
                type="button"
                aria-expanded="false"
                aria-controls=(id)
                aria-label=(format!("Requests to {route_name}"))
                data-action="network-detail"
                data-detail-url=(detail_url(&route.method, &route.path, None))
            { (icons::chevron_right()) }
        },
        // Below 720px the row is a two-line card: the route, then this
        // line, which stands in for the count cells CSS hides there
        // (`.network-route__meta`); the last-seen time is in the group's
        // totals and the route's detail.
        html! {
            span .network-route__method { (method) }
            " "
            (request_path_cell(&route.path))
            span .network-route__meta {
                (count_of(route.count, "request", "requests"))
                " \u{b7} " (route.server_errors) " server"
                " \u{b7} " (route.client_errors) " client"
            }
        },
        html! { span .tabular-nums { (route.count) } },
        html! { span .tabular-nums { (route.server_errors) } },
        html! { span .tabular-nums { (route.client_errors) } },
        html! { span .text-muted .tabular-nums { span data-volatile-metric { (route.avg_ms) "ms" } } },
        html! { span .text-muted { (components::timestamp(&route.last_seen)) } },
    ])
    .classes("network-row")
    .after(html! {
        tr .network-detail-row id=(id) hidden {
            td colspan=(INBOUND_COLUMNS.len()) {
                div data-network-detail {}
            }
        }
    })
}

/// The route expand and group collapse buttons, delegated and bound once
/// per document. Each button names what it controls in `aria-controls` (an
/// id derived by hashing, so it is a plain `[a-z0-9-]` id whatever the path
/// held) and states its state in `aria-expanded`; a route's detail is loaded
/// on its first opening from the button's own `data-detail-url`, an
/// attribute maud escapes — never a URL spliced into script.
const NETWORK_JS: &str = r#"
(function () {
  if (window.__networkBound) return;
  window.__networkBound = true;
  document.addEventListener('click', function (e) {
    var btn = e.target instanceof Element && e.target.closest('[data-action="network-detail"], [data-action="network-group-toggle"]');
    if (!btn) return;
    var target = document.getElementById(btn.getAttribute('aria-controls'));
    if (!target) return;
    var open = btn.getAttribute('aria-expanded') !== 'true';
    btn.setAttribute('aria-expanded', open ? 'true' : 'false');
    target.hidden = !open;
    if (open && btn.getAttribute('data-action') === 'network-detail') {
      var pane = target.querySelector('[data-network-detail]');
      if (pane && !pane.hasChildNodes()) {
        htmx.ajax('GET', btn.getAttribute('data-detail-url'), { target: pane, swap: 'innerHTML' });
      }
    }
  });
})();
"#;

/// Render one row of the per-request detail table: status, duration, client
/// IP, user and timestamp for a single logged request.
///
/// `duration` and `created` are produced by whichever run is looking at the
/// page — during a visual-baseline capture that is the capture run itself —
/// so each is wrapped in an element the baseline suite keys its masks on. The wrapper
/// sits inside the cell rather than on the `<td>` for the same reason as in
/// [`inbound_row`]: `components::data_table` owns the `<td>`.
fn detail_row(row: &request_logs::RequestLogRow, emails: &HashMap<String, String>) -> Vec<Markup> {
    vec![
        Badge::new(status_code_badge_variant(row.status_code)).render(html! { (row.status_code) }),
        html! { span .text-muted .tabular-nums { span data-volatile-metric { (row.duration_ms) "ms" } } },
        html! { span .text-muted { (row.client_ip) } },
        user_cell(&row.user_id, emails),
        html! { span .text-muted { (components::timestamp(&row.created_at)) } },
    ]
}

/// The route table's columns. The first is the expand button and has no
/// visible header.
const INBOUND_COLUMNS: [components::TableCol<'static>; 7] = [
    components::TableCol::new("Details").actions().width("44px"),
    components::TableCol::new("Route").primary(),
    // Fixed widths for every column but the route, so the per-block tables
    // stacked on the page line their columns up.
    components::TableCol::new("Requests").width("96px"),
    components::TableCol::new("Server errors").width("96px"),
    components::TableCol::new("Client errors").width("96px"),
    components::TableCol::new("Avg duration").width("96px"),
    components::TableCol::new("Last seen").width("180px"),
];

const DETAIL_COLUMNS: [components::TableCol<'static>; 5] = [
    components::TableCol::new("Status"),
    components::TableCol::new("Duration"),
    components::TableCol::new("IP"),
    components::TableCol::new("User").optional(),
    components::TableCol::new("Time"),
];

/// Htmx fragment: individual requests for a given inbound path.
///
/// This is a fragment htmx swaps into an expanded row, not a page
/// navigation, so a failed read goes through the one database-error door
/// rather than swapping a whole styled 500 page into a table cell. An empty
/// swap would have read as "this path has never been called".
pub async fn network_inbound_detail(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let method = msg.query("method").to_string();
    let path = msg.query("path").to_string();
    let offset: i64 = msg.query("offset").parse().unwrap_or(0);
    let limit: u32 = 20;

    // One more than the page shows, to learn whether a next page exists.
    let rows = match request_logs::list_for_path(ctx, &method, &path, offset, limit + 1).await {
        Ok(rows) => rows,
        Err(e) => return crate::blocks::crud::db_error_internal(e, "Network inbound detail"),
    };

    let has_more = rows.len() > limit as usize;
    let display_rows = if has_more {
        &rows[..limit as usize]
    } else {
        &rows
    };
    let user_ids: Vec<&str> = display_rows.iter().map(|r| r.user_id.as_str()).collect();
    let emails = match users::emails_by_id(ctx, &user_ids).await {
        Ok(emails) => emails,
        Err(e) => return crate::blocks::crud::db_error_internal(e, "Network inbound detail"),
    };

    let rows: Vec<Vec<Markup>> = display_rows
        .iter()
        .map(|row| detail_row(row, &emails))
        .collect();

    let markup = html! {
        (components::data_table::<fn(usize) -> Option<String>>(
            &DETAIL_COLUMNS,
            rows,
            None,
            html! { p .text-center .text-muted { "No requests logged for this path" } },
        ))
        @if has_more {
            div .text-center .p-2 {
                button .btn .btn--secondary .btn--sm
                    type="button"
                    hx-get=(detail_url(&method, &path, Some(offset + i64::from(limit))))
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
    use crate::{
        platform_state::request_logs::NewRequestLog,
        test_support::{admin_msg, TestContext},
    };

    fn route(
        method: &str,
        path: &str,
        count: i64,
        server: i64,
        client: i64,
        last: &str,
    ) -> PathSummary {
        PathSummary {
            block: request_logs::owning_block(path).into(),
            method: method.into(),
            path: path.into(),
            count,
            avg_ms: 2,
            server_errors: server,
            client_errors: client,
            last_seen: last.into(),
        }
    }

    fn rendered_inbound_row(method: &str, path: &str) -> String {
        inbound_row(&route(method, path, 1, 0, 0, "2026-01-01T00:00:00Z"))
            .render(&INBOUND_COLUMNS, None)
            .into_string()
    }

    /// The value of `attr="…"` on the first element carrying it.
    fn attr<'a>(html: &'a str, attr: &str) -> &'a str {
        let key = format!(r#"{attr}=""#);
        let at = html
            .find(&key)
            .unwrap_or_else(|| panic!("no {attr}: {html}"))
            + key.len();
        &html[at..at + html[at..].find('"').unwrap()]
    }

    /// The expand button and the row it opens agree on one id for any path —
    /// with `.`, `:`, `{`, `&` or a quote in it — and the id is a plain token
    /// a CSS selector or `getElementById` takes as is. The old id replaced
    /// `/` alone and was used as a selector, so a path with a `.` or a `:`
    /// opened nothing.
    #[test]
    fn every_path_gets_a_matching_selector_safe_id() {
        for path in [
            "/b/storage/photos/a.png",
            "/b/files/x:y",
            "/b/storage/api/buckets/{bucket}/objects",
            "/b/storage/api/buckets/a&b/objects",
            "/b/x/\"quoted\"#frag?q=1",
        ] {
            let html = rendered_inbound_row("GET", path);
            let controls = attr(&html, "aria-controls");
            assert!(
                controls
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{path}: id {controls:?} must be a plain token"
            );
            assert!(
                html.contains(&format!(
                    r#"<tr class="network-detail-row" id="{controls}" hidden>"#
                )),
                "{path}: the button must control its own detail row: {html}"
            );
        }
        assert_ne!(
            detail_id("GET", "/a.b"),
            detail_id("GET", "/a_b"),
            "paths the old `/`-only rewrite told apart stay apart"
        );
    }

    /// The detail URL carries method and path form-encoded, so a path with
    /// `&`, `#` or `?` reaches the handler whole — the old URL spliced it in
    /// raw, so `a&b` arrived as path `a` plus a stray parameter.
    #[test]
    fn the_detail_url_round_trips_any_path() {
        for path in [
            "/b/storage/api/buckets/a&b/objects",
            "/b/x/{id}",
            "/b/x/a#b?c=d%20e",
        ] {
            let url = detail_url("GET", path, Some(20));
            let query = url.split_once('?').unwrap().1;
            let params = crate::util::parse_form_body(query.as_bytes());
            assert_eq!(params["path"], path, "{url}");
            assert_eq!(params["method"], "GET");
            assert_eq!(params["offset"], "20");
        }
    }

    /// The expand control is a real, labelled button that says whether it is
    /// open; the row it opens starts hidden. No click handler on a `<tr>`
    /// and no JS-string sink: the URL rides in a maud-escaped attribute.
    #[test]
    fn a_route_expands_through_a_labelled_button() {
        let html = rendered_inbound_row("GET", "'); alert(document.cookie); //");
        assert!(html.contains(r#"<button class="btn btn--ghost btn--icon network-row__toggle" type="button" aria-expanded="false""#), "{html}");
        assert!(
            html.contains(r#"aria-label="Requests to GET &#39;); alert(document.cookie); //""#)
                || html.contains(r#"aria-label="Requests to GET '); alert(document.cookie); //""#),
            "{html}"
        );
        assert!(!html.contains("onclick"), "{html}");
        assert!(
            html.contains(
                "data-detail-url=\"/b/admin/network/detail/inbound?method=GET&amp;path=%27%29%3B"
            ),
            "{html}"
        );
    }

    /// Counts are plain tabular figures, not red badges.
    #[test]
    fn counts_are_plain_numbers() {
        let html = inbound_row(&route(
            "GET",
            "/b/admin/x",
            12,
            3,
            4,
            "2026-01-01T00:00:00Z",
        ))
        .render(&INBOUND_COLUMNS, None)
        .into_string();
        assert!(!html.contains("badge"), "{html}");
        for n in ["12", "3", "4"] {
            assert!(
                html.contains(&format!(r#"<span class="tabular-nums">{n}</span>"#)),
                "{html}"
            );
        }
    }

    async fn seed(ctx: &TestContext, method: &str, path: &str, status_code: i64) {
        request_logs::insert(
            ctx,
            &NewRequestLog {
                method,
                path,
                status_code,
                error_message: "",
                duration_ms: 1,
                client_ip: "203.0.113.7",
                user_id: "",
            },
        )
        .await
        .expect("seed a request log row");
    }

    async fn page(ctx: &TestContext, query: &[(&str, &str)]) -> String {
        let mut msg = admin_msg("retrieve", NETWORK_HREF);
        for (name, value) in query {
            msg.set_meta(format!("req.query.{name}"), *value);
        }
        match settings_body(ctx, &msg).await.expect("network body") {
            NetworkBody::Page(body) => body.into_string(),
            NetworkBody::Moved(to) => panic!("expected a page, was sent to {to}"),
        }
    }

    /// Where a request for `query` is sent, when it is.
    async fn moved_to(ctx: &TestContext, query: &[(&str, &str)]) -> Option<String> {
        let mut msg = admin_msg("retrieve", NETWORK_HREF);
        for (name, value) in query {
            msg.set_meta(format!("req.query.{name}"), *value);
        }
        match settings_body(ctx, &msg).await.expect("network body") {
            NetworkBody::Page(_) => None,
            NetworkBody::Moved(to) => Some(to),
        }
    }

    /// The rendered page: the summary line, the groups, errors-only, and the
    /// controls' links keeping each other.
    #[tokio::test]
    async fn the_page_summarises_filters_and_keeps_its_controls() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed(&ctx, "GET", "/b/admin/ok", 200).await;
        seed(&ctx, "GET", "/b/admin/ok", 200).await;
        seed(&ctx, "POST", "/b/auth/fails", 500).await;
        seed(&ctx, "GET", "/b/storage/a.b", 404).await;

        let all = page(&ctx, &[]).await;
        assert!(all.contains("Showing 1\u{2013}3 of 3 routes"), "{all}");
        for group in ["/b/admin/", "/b/auth/", "/b/storage/"] {
            assert!(
                all.contains(&format!(
                    r#"<span class="network-group__name">{group}</span>"#
                )),
                "{group}: {all}"
            );
        }
        assert!(
            all.contains(r#"aria-pressed="false""#) && all.contains("Errors only"),
            "{all}"
        );

        let failing = page(&ctx, &[("errors", "1"), ("sort", "errors")]).await;
        assert!(
            failing.contains("Showing 1\u{2013}2 of 2 routes"),
            "{failing}"
        );
        assert!(
            !failing.contains("/b/admin/ok"),
            "a route with no errors is not listed: {failing}"
        );
        assert!(
            failing.contains(r#"hx-get="/b/admin/settings/network?sort=errors""#),
            "turning errors-only off keeps the sort: {failing}"
        );
        assert!(
            failing.contains(r#"hx-get="/b/admin/settings/network?sort=recent&amp;errors=1""#),
            "changing the sort keeps errors-only: {failing}"
        );
        assert!(
            !failing.contains("Manage network access rules"),
            "{failing}"
        );
    }

    /// The page is cut in SQL: 50 routes, "Showing 51–60 of 60" on page 2,
    /// grouped under each block's heading with that block's totals over all
    /// its routes; a page past the end shows the first page.
    #[tokio::test]
    async fn routes_page_by_fifty_grouped_by_block_with_block_totals() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        for i in 0..55 {
            seed(&ctx, "GET", &format!("/b/admin/r{i:02}"), 200).await;
        }
        for i in 0..5 {
            seed(&ctx, "GET", &format!("/b/auth/a{i}"), 404).await;
        }

        let first = page(&ctx, &[]).await;
        assert!(
            first.contains("Showing 1\u{2013}50 of 60 routes"),
            "{first}"
        );
        assert!(
            first.contains("55 requests \u{b7} 0 server errors \u{b7} 0 client errors"),
            "the admin heading totals all 55 of its routes, not the 50 on this page: {first}"
        );
        let second = page(&ctx, &[("page", "2")]).await;
        assert!(
            second.contains("Showing 51\u{2013}60 of 60 routes"),
            "{second}"
        );
        assert!(
            second.contains(r#"<span class="network-group__name">/b/admin/</span>"#)
                && second.contains(r#"<span class="network-group__name">/b/auth/</span>"#),
            "the page that crosses blocks heads both: {second}"
        );
        assert!(
            second.contains("5 requests \u{b7} 0 server errors \u{b7} 5 client errors"),
            "{second}"
        );
        assert_eq!(
            moved_to(&ctx, &[("page", "9"), ("sort", "errors")])
                .await
                .as_deref(),
            Some("/b/admin/settings/network?sort=errors&page=2"),
            "a page past the end is sent to the last page, keeping the view"
        );
        assert_eq!(moved_to(&ctx, &[("page", "2")]).await, None);
    }

    /// "Errors only" is a `HAVING` in the read, not a filter over a capped
    /// prefix: a failing route that ranks below 60 busier ones by requests
    /// is the one route listed.
    #[tokio::test]
    async fn errors_only_finds_a_failing_route_beyond_the_first_page() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        for i in 0..60 {
            for _ in 0..2 {
                seed(&ctx, "GET", &format!("/b/admin/busy{i:02}"), 200).await;
            }
        }
        seed(&ctx, "GET", "/b/admin/zz-quiet-failure", 500).await;

        let unfiltered = page(&ctx, &[]).await;
        assert!(
            !unfiltered.contains("zz-quiet-failure"),
            "by requests it ranks last, past the first page: {unfiltered}"
        );
        let failing = page(&ctx, &[("errors", "1")]).await;
        assert!(
            failing.contains("Showing 1\u{2013}1 of 1 route"),
            "{failing}"
        );
        assert!(failing.contains("zz-quiet-failure"), "{failing}");
    }

    /// The sort ranks routes across blocks, not within each: on a busy
    /// deployment a failing route of a block late in the alphabet is on the
    /// first page under "Sort by Errors", and its block's heading with it.
    #[tokio::test]
    async fn sort_by_errors_ranks_across_blocks() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        for i in 0..60 {
            seed(&ctx, "GET", &format!("/b/admin/busy{i:02}"), 200).await;
        }
        seed(&ctx, "GET", "/b/zeta/broken", 500).await;

        let by_requests = page(&ctx, &[]).await;
        assert!(!by_requests.contains("/b/zeta/broken"), "{by_requests}");
        let by_errors = page(&ctx, &[("sort", "errors")]).await;
        let zeta = by_errors
            .find(r#"<span class="network-group__name">/b/zeta/</span>"#)
            .unwrap_or_else(|| panic!("the zeta heading on page 1: {by_errors}"));
        let admin = by_errors
            .find(r#"<span class="network-group__name">/b/admin/</span>"#)
            .expect("the admin heading");
        assert!(
            zeta < admin,
            "the block of the first-ranked route leads: {by_errors}"
        );
        assert!(
            by_errors.contains("1 request \u{b7} 1 server error"),
            "{by_errors}"
        );
    }

    /// The sort is a set of links, the current one `aria-current`, not three
    /// independent toggles.
    #[tokio::test]
    async fn the_sort_is_links_with_the_current_one_marked() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed(&ctx, "GET", "/b/admin/x", 200).await;
        let html = page(&ctx, &[("sort", "recent")]).await;
        assert_eq!(html.matches(r#"aria-current="true""#).count(), 1, "{html}");
        assert!(
            html.contains(
                r##"href="/b/admin/settings/network?sort=recent" hx-get="/b/admin/settings/network?sort=recent" hx-target="#content" hx-push-url="true" aria-current="true""##
            ),
            "{html}"
        );
        assert_eq!(
            html.matches("aria-pressed").count(),
            1,
            "only Errors only is a toggle: {html}"
        );
    }

    /// Methods read as HTTP verbs, as the pipeline stores them.
    #[tokio::test]
    async fn the_route_names_the_http_verb() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed(&ctx, "DELETE", "/b/admin/x", 200).await;
        let html = page(&ctx, &[]).await;
        assert!(
            html.contains(r#"<span class="network-route__method">DELETE</span>"#),
            "{html}"
        );
    }

    /// The baseline masks: the per-route average and the per-request
    /// duration carry `data-volatile-metric` inside their cells; the stamps
    /// are `<time>`s.
    #[test]
    fn rows_mark_the_values_the_baseline_run_itself_produces() {
        let html = inbound_row(&route("GET", "/b/admin/", 4, 0, 0, "2026-01-01T00:00:00Z"))
            .render(&INBOUND_COLUMNS, None)
            .into_string();
        assert!(
            html.contains("<span data-volatile-metric>2ms</span>"),
            "{html}"
        );
        assert!(
            html.contains(r#"<time class="datetime" datetime="2026-01-01T00:00:00.000Z""#),
            "{html}"
        );

        let row = request_logs::RequestLogRow {
            id: "r".into(),
            flow_id: String::new(),
            method: "GET".into(),
            path: "/".into(),
            block: String::new(),
            status: "OK".into(),
            status_code: 200,
            duration_ms: 12,
            error_message: String::new(),
            client_ip: "127.0.0.1".into(),
            user_id: String::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: String::new(),
        };
        let html = components::TableRow::new(detail_row(&row, &HashMap::new()))
            .render(&DETAIL_COLUMNS, None)
            .into_string();
        assert!(
            html.contains("<span data-volatile-metric>12ms</span>"),
            "{html}"
        );
        assert!(html.contains(r#"<time class="datetime""#), "{html}");
    }
}

#[cfg(test)]
mod outage_tests {
    //! `/b/admin/settings/network` is a monitoring page, so an outage is
    //! exactly when an operator opens it — and an empty inbound table read as
    //! "no traffic has reached this deployment".

    use crate::{
        blocks::admin::pages::settings::settings_page,
        test_support::{admin_msg, output_header, output_http_status, TestContext},
    };

    /// A page number past the last page is a redirect to the last page —
    /// `303` for a page load, `HX-Redirect` for an htmx swap — so the address
    /// bar never says `page=9` over page 1.
    #[tokio::test]
    async fn a_page_past_the_end_redirects_to_the_last_page() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let past = || {
            let mut msg = admin_msg("retrieve", "/b/admin/settings/network");
            msg.set_meta("req.query.page", "9");
            msg
        };
        let out = settings_page(&ctx, &past(), "network").await;
        assert_eq!(output_http_status(out).await, 303);
        let out = settings_page(&ctx, &past(), "network").await;
        assert_eq!(
            output_header(out, "Location").await.as_deref(),
            Some("/b/admin/settings/network"),
            "no routes at all: the last page is the first"
        );

        let mut htmx = past();
        htmx.set_meta("http.header.hx-request", "true");
        let out = settings_page(&ctx, &htmx, "network").await;
        assert_eq!(
            output_header(out, "HX-Redirect").await.as_deref(),
            Some("/b/admin/settings/network")
        );
    }

    #[tokio::test]
    async fn a_failing_inbound_summary_renders_the_error_page_not_no_traffic() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();
        let msg = admin_msg("retrieve", "/b/admin/settings/network");
        assert_eq!(
            output_http_status(settings_page(&ctx, &msg, "network").await).await,
            500
        );
    }

    /// The per-path detail fragment htmx swaps into an expanded row. A failed
    /// read used to swap in an empty table, which reads as "this path has
    /// never been called".
    #[tokio::test]
    async fn a_failing_detail_fragment_is_an_error_not_an_empty_table() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();
        let msg = admin_msg("retrieve", "/b/admin/settings/network/detail");
        assert_eq!(
            output_http_status(super::network_inbound_detail(&ctx, &msg).await).await,
            500
        );
    }
}
