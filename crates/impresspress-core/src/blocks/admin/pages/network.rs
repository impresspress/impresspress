use std::collections::HashMap;

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use super::{request_path_cell, status_code_badge_variant, user_cell};
use crate::{
    blocks::auth::repo::users,
    platform_state::request_logs::{self, PathSort, PathSummary},
    ui::{
        components::{self, Badge},
        icons,
    },
    util::urlencode,
};

/// Routes per page.
const PAGE_SIZE: usize = 50;

/// The most routes one render reads. Every route is read so the page can
/// group, total, sort and page them; a deployment with more distinct
/// `(method, path)` pairs than this is shown the first `ROUTE_CAP` in the
/// chosen order, and told so ([`request_logs::PathSummaries::capped`]).
const ROUTE_CAP: usize = 1000;

/// Where this page lives; every control on it links back here.
const NETWORK_HREF: &str = "/b/admin/settings/network";

/// The page's controls, as the request asked for them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Query {
    search: String,
    sort: PathSort,
    errors_only: bool,
    page: usize,
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
) -> Result<Markup, wafer_run::WaferError> {
    let query = Query::from_msg(msg);
    let summary =
        request_logs::summarise_by_path(ctx, &query.search, query.sort, ROUTE_CAP).await?;
    let routes: Vec<PathSummary> = summary
        .rows
        .into_iter()
        .filter(|r| !query.errors_only || r.server_errors + r.client_errors > 0)
        .collect();
    let listing = Listing::build(routes, query.sort, query.page);

    let sort_href = |sort: PathSort| {
        Query {
            sort,
            ..query.clone()
        }
        .href(true)
    };
    let errors_href = Query {
        errors_only: !query.errors_only,
        ..query.clone()
    }
    .href(true);
    let page_href = query.href(true);

    Ok(html! {
        div .filter-bar {
            (components::search_input_with_value("search", "Search by path...", &query.href(false), "#content", &query.search))
            div .network-controls {
                div .filter-toggles role="group" aria-labelledby="network-sort-label" {
                    span #network-sort-label .text-sm .text-muted { "Sort by" }
                    (components::filter_toggle("Requests", query.sort == PathSort::Requests, &sort_href(PathSort::Requests)))
                    (components::filter_toggle("Errors", query.sort == PathSort::Errors, &sort_href(PathSort::Errors)))
                    (components::filter_toggle("Recent", query.sort == PathSort::Recent, &sort_href(PathSort::Recent)))
                }
                (components::filter_toggle("Errors only", query.errors_only, &errors_href))
                button .btn .btn--secondary .btn--sm
                    type="button"
                    hx-get=(page_href)
                    hx-target="#content"
                { (icons::refresh_cw()) " Refresh" }
            }
        }

        @if listing.total == 0 {
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
                "Showing " (listing.first) "\u{2013}" (listing.last) " of " (listing.total)
                @if listing.total == 1 { " route" } @else { " routes" }
                @if summary.capped {
                    " \u{2014} the first " (ROUTE_CAP) " in this order; search to narrow"
                }
            }
            @for group in &listing.groups {
                (group_section(group))
            }
            @if let Some(per_page) = std::num::NonZeroU32::new(PAGE_SIZE as u32) {
                (components::pagination(listing.page as u32, per_page, listing.total as u32, &page_href))
            }
        }

        script { (maud::PreEscaped(NETWORK_JS)) }
    })
}

/// The routes, grouped by the block that serves them and cut to one page.
struct Listing {
    /// The groups with a route on this page, in order, each holding only its
    /// routes on this page (and its totals over all of them).
    groups: Vec<Group>,
    /// Routes in the whole (filtered) set.
    total: usize,
    /// 1-based positions of the first and last route on this page.
    first: usize,
    last: usize,
    /// The page shown, clamped to the last one.
    page: usize,
}

/// One block's routes.
struct Group {
    /// The `{block}` of the routes' `/b/{block}/` prefix, or `None` for the
    /// routes no block prefix names (`/`, the unmatched-route collapse).
    block: Option<String>,
    /// Totals over every route of this block in the set, not just this page.
    routes: usize,
    requests: i64,
    server_errors: i64,
    client_errors: i64,
    last_seen: String,
    /// This block's routes on this page.
    rows: Vec<PathSummary>,
}

impl Listing {
    /// Group `routes` (in the order the read returned them) by owning block,
    /// order the groups by `sort` over their totals, and cut the flattened
    /// result to page `page`.
    fn build(routes: Vec<PathSummary>, sort: PathSort, page: usize) -> Self {
        let mut groups: Vec<Group> = Vec::new();
        let mut index: HashMap<Option<String>, usize> = HashMap::new();
        for route in routes {
            let block = owning_block(&route.path).map(str::to_string);
            let at = *index.entry(block.clone()).or_insert_with(|| {
                groups.push(Group {
                    block,
                    routes: 0,
                    requests: 0,
                    server_errors: 0,
                    client_errors: 0,
                    last_seen: String::new(),
                    rows: Vec::new(),
                });
                groups.len() - 1
            });
            let group = &mut groups[at];
            group.routes += 1;
            group.requests += route.count;
            group.server_errors += route.server_errors;
            group.client_errors += route.client_errors;
            if route.last_seen > group.last_seen {
                group.last_seen.clone_from(&route.last_seen);
            }
            group.rows.push(route);
        }
        // Descending on the key, then by name so equal groups keep one order;
        // the unprefixed group sorts after every block on a tie.
        groups.sort_by(|a, b| {
            let key = match sort {
                PathSort::Requests => b.requests.cmp(&a.requests),
                PathSort::Errors => {
                    (b.server_errors + b.client_errors).cmp(&(a.server_errors + a.client_errors))
                }
                PathSort::Recent => b.last_seen.cmp(&a.last_seen),
            };
            key.then_with(|| match (&a.block, &b.block) {
                (Some(a), Some(b)) => a.cmp(b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
        });

        let total: usize = groups.iter().map(|g| g.rows.len()).sum();
        let pages = total.div_ceil(PAGE_SIZE).max(1);
        let page = page.min(pages);
        let skip = (page - 1) * PAGE_SIZE;
        let mut seen = 0usize;
        let mut shown = Vec::new();
        for mut group in groups {
            let len = group.rows.len();
            let from = skip.saturating_sub(seen).min(len);
            let to = (skip + PAGE_SIZE).saturating_sub(seen).min(len);
            seen += len;
            if from < to {
                group.rows = group.rows.drain(from..to).collect();
                shown.push(group);
            }
        }
        let last = (skip + PAGE_SIZE).min(total);
        Listing {
            groups: shown,
            total,
            first: if total == 0 { 0 } else { skip + 1 },
            last,
            page,
        }
    }
}

/// The block a request path belongs to: the `{block}` of `/b/{block}/…`,
/// which is how the router hands a request to a block. `None` for a path
/// outside that space.
fn owning_block(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/b/")?;
    let block = rest.split('/').next().unwrap_or("");
    (!block.is_empty()).then_some(block)
}

/// `"1 request"` / `"3 requests"`.
fn count_of(n: i64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// One block's routes: a heading whose button collapses them, the block's
/// totals, and the table.
fn group_section(group: &Group) -> Markup {
    let name = group.block.as_deref().unwrap_or("Other");
    let body_id = format!(
        "network-group-{:08x}",
        components::fnv1a(group.block.as_deref().unwrap_or(""))
    );
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
                    @if group.block.is_some() {
                        span .network-group__name { "/b/" (name) "/" }
                    } @else {
                        span .network-group__name { "Other paths" }
                    }
                }
            }
            p .network-group__totals {
                (count_of(group.routes as i64, "route", "routes"))
                " \u{b7} " (count_of(group.requests, "request", "requests"))
                " \u{b7} " (count_of(group.server_errors, "server error", "server errors"))
                " \u{b7} " (count_of(group.client_errors, "client error", "client errors"))
                @if !group.last_seen.is_empty() {
                    " \u{b7} last " (components::timestamp(&group.last_seen))
                }
                @if group.rows.len() < group.routes {
                    span .text-muted { " (" (group.rows.len()) " on this page)" }
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

    /// Routes group under the block their `/b/{block}/` prefix names, with
    /// totals over the block's every route; the rest go to one "Other" group
    /// listed after the blocks on a tie.
    #[test]
    fn routes_group_by_owning_block_with_totals() {
        let listing = Listing::build(
            vec![
                route("GET", "/b/admin/users", 10, 1, 0, "2026-01-02T00:00:00Z"),
                route("GET", "/b/auth/login", 6, 0, 2, "2026-01-03T00:00:00Z"),
                route("POST", "/b/admin/users", 5, 0, 1, "2026-01-01T00:00:00Z"),
                route("GET", "<unmatched>", 4, 0, 4, "2026-01-01T00:00:00Z"),
                route("GET", "/", 1, 0, 0, "2026-01-01T00:00:00Z"),
            ],
            PathSort::Requests,
            1,
        );
        let names: Vec<Option<&str>> = listing.groups.iter().map(|g| g.block.as_deref()).collect();
        assert_eq!(names, [Some("admin"), Some("auth"), None]);
        let admin = &listing.groups[0];
        assert_eq!(
            (
                admin.routes,
                admin.requests,
                admin.server_errors,
                admin.client_errors
            ),
            (2, 15, 1, 1)
        );
        assert_eq!(admin.last_seen, "2026-01-02T00:00:00Z");
        assert_eq!(
            listing.groups[2].rows.len(),
            2,
            "/ and the unmatched collapse"
        );
        assert_eq!((listing.total, listing.first, listing.last), (5, 1, 5));

        let by_recent = Listing::build(
            vec![
                route("GET", "/b/admin/users", 10, 0, 0, "2026-01-02T00:00:00Z"),
                route("GET", "/b/auth/login", 6, 0, 0, "2026-01-03T00:00:00Z"),
            ],
            PathSort::Recent,
            1,
        );
        assert_eq!(by_recent.groups[0].block.as_deref(), Some("auth"));
    }

    /// The page shows 50 routes and says which of how many; a later page
    /// picks up where it left off, totals still over the whole group.
    #[test]
    fn the_listing_pages_by_fifty_and_counts_the_whole_set() {
        let routes: Vec<PathSummary> = (0..120)
            .map(|i| {
                route(
                    "GET",
                    &format!("/b/admin/r{i:03}"),
                    200 - i,
                    0,
                    0,
                    "2026-01-01T00:00:00Z",
                )
            })
            .collect();
        let first = Listing::build(routes.clone(), PathSort::Requests, 1);
        assert_eq!(
            (first.total, first.first, first.last, first.page),
            (120, 1, 50, 1)
        );
        assert_eq!(first.groups[0].rows.len(), 50);
        assert_eq!(
            first.groups[0].routes, 120,
            "totals are over the whole group"
        );

        let third = Listing::build(routes.clone(), PathSort::Requests, 3);
        assert_eq!((third.first, third.last), (101, 120));
        assert_eq!(third.groups[0].rows[0].path, "/b/admin/r100");

        let past_the_end = Listing::build(routes, PathSort::Requests, 9);
        assert_eq!(
            past_the_end.page, 3,
            "a page past the end clamps to the last"
        );
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
        settings_body(ctx, &msg)
            .await
            .expect("network body")
            .into_string()
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
        test_support::{admin_msg, output_http_status, TestContext},
    };

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
