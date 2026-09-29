use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use super::status_code_badge_variant;
use crate::{
    platform_state::request_logs,
    ui::{
        components::{self, Badge, BadgeVariant},
        icons,
    },
};

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
    let inbound = network_inbound_tab(ctx, msg).await?;
    Ok(html! {
        div .filter-bar .mb-2 {
            button .btn .btn--secondary .btn--sm
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
            (inbound)
        }
    })
}

async fn network_inbound_tab(
    ctx: &dyn Context,
    msg: &Message,
) -> Result<Markup, wafer_run::WaferError> {
    let search = msg.query("search").to_string();

    let summary = request_logs::summarise_by_path(ctx, &search, 50).await?;

    Ok(html! {
        div .filter-bar {
            (components::search_input_with_value("search", "Search by path...", "/b/admin/settings/network", "#content", &search))
        }

        style { (maud::PreEscaped("
            .expand-row { cursor: pointer; }
            .expand-row:hover { background: var(--bg-secondary, #f8fafc); }
            .detail-rows td { background: var(--bg-secondary, #f8fafc); font-size: 12px; }
            .detail-rows[hidden] { display: none; }
        ")) }
        // Delegated click handler — the detail pane carries a `data-detail-url`
        // attribute (maud-escaped) instead of an `onclick` JS-string literal,
        // which maud does NOT escape and so let an attacker-controlled request
        // path break out and run script in an admin's session. Bound once per
        // document.
        //
        // This page stated that rule and was the only one applying it. It is
        // now how every page in the tree is written: the shared vocabulary and
        // the general argument live in `ui/assets/chrome.js`, and
        // `ui::tests::pages_carry_no_event_handler_attributes` keeps the next
        // page from reintroducing the sink. This handler keeps its own
        // `data-detail-url` attribute rather than a `data-action` verb — the
        // attribute IS the operand here, and there is only one behaviour.
        //
        // The clicked row and its detail row are siblings because
        // `TableRow::after` emits the second immediately after the first, so
        // the handler walks to `nextElementSibling` rather than resolving an
        // id it was handed. `components::data_table` owns the `<tr>` and takes
        // no caller attributes; the pane inside the detail row is markup this
        // page writes, so that is where the URL lives.
        script { (maud::PreEscaped("
            if (!window.__networkDetailBound) {
                window.__networkDetailBound = true;
                document.addEventListener('click', function (e) {
                    var row = e.target.closest('tr.expand-row');
                    if (!row) return;
                    var dr = row.nextElementSibling;
                    if (!dr || !dr.classList.contains('detail-rows')) return;
                    var detail = dr.querySelector('[data-detail-url]');
                    if (!detail) return;
                    if (!dr.hidden) { dr.hidden = true; return; }
                    dr.hidden = false;
                    if (!detail.innerHTML) {
                        htmx.ajax('GET', detail.dataset.detailUrl, {target: '#' + detail.id, swap: 'innerHTML'});
                    }
                });
            }
        ")) }

        @let rows: Vec<components::TableRow> = summary.iter().map(|row| {
            inbound_row(&row.method, &row.path, row.count, row.avg_ms, row.errors, &row.last_seen)
        }).collect();

        (components::DataTable::new(&INBOUND_COLUMNS)
            .rows(rows)
            .empty(html! { p .text-center .text-muted { "No inbound requests yet" } })
            .render())
    })
}

/// Render one inbound-summary row: the clickable row plus its lazily-loaded
/// detail row. `method`/`path` come from the request log and are
/// attacker-controlled (any HTTP request with a crafted path is logged), so
/// they appear only in maud-escaped attribute/text contexts. The `<tr>` itself
/// carries no attribute the handler reads — `components::data_table` owns the
/// row element and takes no caller attributes — so the detail URL lives on the
/// pane inside the detail row this function writes, in a maud-escaped
/// `data-detail-url`, and the delegated click handler reaches it by walking to
/// `nextElementSibling`. Never an `onclick` JS-string literal (maud doesn't
/// escape JS-string context, which was a stored-XSS sink).
///
/// `avg_ms` and `last_seen` are per-run values — a latency measured by the
/// running deployment and the wall-clock time of a request it served — so
/// each is wrapped in an element the visual-baseline suite keys its masks on
/// (`crates/impresspress-web/tests/e2e/visual-baseline.spec.ts`; it masks a
/// `<time>` directly and the whole cell around a `data-volatile-metric`). The
/// wrappers are inside the cell, not attributes on the `<td>`, because
/// `components::data_table` emits the `<td>` itself and takes only the
/// cell's inner markup, which it carries through verbatim.
fn inbound_row(
    method: &str,
    path: &str,
    cnt: i64,
    avg_ms: i64,
    errors: i64,
    last_seen: &str,
) -> components::TableRow {
    let row_id = format!("inbound-{}-{}", method, path.replace('/', "_"));
    let detail_url = format!("/b/admin/network/detail/inbound?method={method}&path={path}");
    components::TableRow::new(vec![
        html! { span .text-muted { (icons::chevron_right()) } },
        html! { span .font-medium { (method.to_uppercase()) } },
        html! { (components::breakable_id(path)) },
        Badge::new(BadgeVariant::Info).render(html! { (cnt) }),
        html! { span .text-muted { span data-volatile-metric { (avg_ms) "ms" } } },
        html! {
            @if errors > 0 {
                (Badge::new(BadgeVariant::Danger).render(html! { (errors) }))
            } @else {
                span .text-muted { "0" }
            }
        },
        html! {
            span .text-muted {
                time datetime=(last_seen) { (last_seen.get(..19).unwrap_or(last_seen)) }
            }
        },
    ])
    .classes("expand-row")
    .after(html! {
        tr .detail-rows hidden {
            td colspan=(INBOUND_COLUMNS.len()) .p-0 {
                div id=(row_id) data-detail-url=(detail_url) {}
            }
        }
    })
}

/// Render one row of the per-request detail table: status, duration, client
/// IP, user and timestamp for a single logged request.
///
/// `duration` and `created` are produced by whichever run is looking at the
/// page — during a visual-baseline capture that is the capture run itself —
/// so each is wrapped in an element the baseline suite keys its masks on. The wrapper
/// sits inside the cell rather than on the `<td>` for the same reason as in
/// [`inbound_row`]: `components::data_table` owns the `<td>`.
fn detail_row(
    status_code: i64,
    duration: i64,
    client_ip: &str,
    user_id: &str,
    created: &str,
) -> Vec<Markup> {
    vec![
        Badge::new(status_code_badge_variant(status_code)).render(html! { (status_code) }),
        html! { span .text-muted { span data-volatile-metric { (duration) "ms" } } },
        html! { span .text-muted { (client_ip) } },
        html! {
            @if !user_id.is_empty() {
                span .text-muted { (user_id.get(..8).unwrap_or(user_id)) }
            }
        },
        html! {
            span .text-muted {
                time datetime=(created) { (created.get(..19).unwrap_or(created)) }
            }
        },
    ]
}

/// The two network tables' columns. Declared once each so the
/// `<td data-label>` the component stamps on every cell names the same column
/// its header does. The summary's first column is the chevron and has no
/// label; it keeps the 30px width the old `th .w-30` gave it.
const INBOUND_COLUMNS: [components::TableCol<'static>; 7] = [
    components::TableCol {
        label: "",
        width: Some("30px"),
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
        label: "Requests",
        width: None,
    },
    components::TableCol {
        label: "Avg Duration",
        width: None,
    },
    components::TableCol {
        label: "Errors",
        width: None,
    },
    components::TableCol {
        label: "Last Seen",
        width: None,
    },
];

const DETAIL_COLUMNS: [components::TableCol<'static>; 5] = [
    components::TableCol {
        label: "Status",
        width: None,
    },
    components::TableCol {
        label: "Duration",
        width: None,
    },
    components::TableCol {
        label: "IP",
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

    let rows: Vec<Vec<Markup>> = display_rows
        .iter()
        .map(|row| {
            detail_row(
                row.status_code,
                row.duration_ms,
                row.client_ip.as_str(),
                row.user_id.as_str(),
                row.created_at.as_str(),
            )
        })
        .collect();

    let markup = html! {
        (components::data_table::<fn(usize) -> Option<String>>(
            &DETAIL_COLUMNS,
            rows,
            None,
            html! { p .text-center .text-muted { "No requests logged for this path" } },
        ))
        @if has_more {
            @let next_offset = offset + i64::from(limit);
            div .text-center .p-2 {
                button .btn .btn--secondary .btn--sm
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

    /// Render one summary row the way the page does: through the shared
    /// component, against the columns it declares.
    fn rendered_inbound_row(
        method: &str,
        path: &str,
        cnt: i64,
        avg_ms: i64,
        errors: i64,
        last_seen: &str,
    ) -> String {
        inbound_row(method, path, cnt, avg_ms, errors, last_seen)
            .render(&INBOUND_COLUMNS, None)
            .into_string()
    }

    #[test]
    fn inbound_row_has_no_js_string_xss_sink() {
        // Attacker-controlled request path crafted to break out of the old
        // `onclick="toggleDetail('…')"` JS-string literal.
        let html = rendered_inbound_row(
            "GET",
            "'); alert(document.cookie); //",
            1,
            2,
            0,
            "2026-01-01T00:00:00Z",
        );

        // The JS-string sink is gone entirely.
        assert!(
            !html.contains("onclick"),
            "must not emit an onclick JS-string sink: {html}"
        );
        // Replaced by a maud-escaped data-* attribute the delegated handler
        // reads. It sits on the detail pane, not on the `<tr>` — the shared
        // component owns the row element and takes no caller attributes.
        assert!(
            html.contains("data-detail-url="),
            "detail pane must carry data-detail-url: {html}"
        );
        assert!(
            html.contains(r#"class="data-table__row expand-row""#),
            "the row must keep the class the delegated handler selects on: {html}"
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

    /// The visual-baseline suite screenshots this page, and the values in
    /// these two tables are produced by the run that takes the screenshot:
    /// `avg_ms` and `duration_ms` are latencies measured during it, and
    /// `last_seen` / `created_at` are wall-clock stamps of requests the
    /// baseline run itself made. Nothing pinned them, so they drift on every
    /// capture; the suite's 1% pixel tolerance absorbed the drift, which made
    /// this latent fragility rather than a live flake.
    ///
    /// The mask hooks go INSIDE the cell, never on the `<td>`.
    /// `components::data_table` renders the `<td>` itself and accepts only
    /// each cell's inner markup, so an attribute on the `<td>` would have been
    /// dropped when this table migrated onto the shared component. Inner
    /// markup is carried through verbatim, and both hooks survived that
    /// migration — which is what these two tests now assert against the
    /// component's own output.
    #[test]
    fn inbound_row_marks_the_values_the_baseline_run_itself_produces() {
        let html = rendered_inbound_row("GET", "/b/admin/", 4, 37, 0, "2026-01-01T00:00:00Z");

        assert!(
            html.contains("<span data-volatile-metric>37ms</span>"),
            "the per-route average must carry the mask hook inside the cell: {html}"
        );
        assert!(
            html.contains("<time datetime=\"2026-01-01T00:00:00Z\">2026-01-01T00:00:00</time>"),
            "the last-seen stamp must be a <time>, which the suite's existing \
             `[data-relative-time], .relative-time, time` mask already matches: {html}"
        );
    }

    /// Same contract for the lazily-loaded per-request detail table. It is
    /// collapsed until an operator clicks a row, so it is not inside the
    /// captured region today — but it is the same page module rendering the
    /// same two kinds of per-run value.
    #[test]
    fn detail_row_marks_the_values_the_baseline_run_itself_produces() {
        let html =
            components::TableRow::new(detail_row(200, 12, "127.0.0.1", "", "2026-01-01T00:00:00Z"))
                .render(&DETAIL_COLUMNS, None)
                .into_string();

        assert!(
            html.contains("<span data-volatile-metric>12ms</span>"),
            "the per-request duration must carry the mask hook inside the cell: {html}"
        );
        assert!(
            html.contains("<time datetime=\"2026-01-01T00:00:00Z\">2026-01-01T00:00:00</time>"),
            "the per-request stamp must be a <time>: {html}"
        );
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
