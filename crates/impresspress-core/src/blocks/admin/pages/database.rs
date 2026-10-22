//! Database admin page — table browser + schema viewer + SQL editor.
//!
//! Layout: two sibling cards on the page background.
//! * "Tables" (~32%): the table list, grouped by block, with row counts and a
//!   filter input.
//! * The workbench: tabs (Schema / SQL editor). Schema is the default tab.
//!
//! Backend status badge in the page header.
//!
//! Reuses `wafer_sql_utils::introspect` for table listing/columns and
//! the shared `validate_readonly_query` helper for the SQL editor.

use maud::{html, Markup};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream, WaferError};

use super::{admin_page, crumb};
use crate::{
    blocks::admin::{
        contracts::result_columns,
        database::{
            introspect_columns, introspect_table_summaries, validate_readonly_query,
            IntrospectError, TableSummary,
        },
    },
    ui::{
        components::{
            self, empty_note, Action, ActionTarget, Badge, BadgeVariant, Card, HeaderStatus,
        },
        html_response, icons,
        shell::Topbar,
        templates::list_page,
    },
    util::{now_millis, parse_form_body, url_path_encode as pct_encode},
};

/// Tab the right pane is showing.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Schema,
    Sql,
}

impl Tab {
    fn from_query(q: &str) -> Self {
        match q {
            "sql" => Tab::Sql,
            _ => Tab::Schema,
        }
    }
    fn as_query(self) -> &'static str {
        match self {
            Tab::Schema => "schema",
            Tab::Sql => "sql",
        }
    }
}

fn backend_badge(backend: wafer_sql_utils::Backend, table_count: usize) -> HeaderStatus {
    let label = match backend {
        wafer_sql_utils::Backend::Sqlite => "SQLite",
        wafer_sql_utils::Backend::Postgres => "PostgreSQL",
    };
    // Neutral: a fact about the deployment, not a state that is good or bad.
    HeaderStatus::badge(
        Badge::new(BadgeVariant::Secondary).title("Database backend"),
        &format!("{label} · {table_count} tables"),
    )
}

fn left_pane(tables: &[TableSummary], selected: Option<&str>, tab: Tab) -> Markup {
    // Group tables by their `org__block` prefix (first two `__`-separated
    // segments). Tables without `__` (e.g. legacy `variables`) go in their
    // own "Other" group. Each group is a collapsible row (a `<details>`,
    // open by default) holding the block's name, its org and its table
    // count; the filter input above hides rows that do not match and the
    // groups left with none.
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<&TableSummary>> = BTreeMap::new();
    let mut ungrouped: Vec<&TableSummary> = Vec::new();
    for t in tables {
        let parts: Vec<&str> = t.name.splitn(3, "__").collect();
        if parts.len() == 3 {
            let group_key = format!("{}__{}", parts[0], parts[1]);
            groups.entry(group_key).or_default().push(t);
        } else {
            ungrouped.push(t);
        }
    }

    // Below 900px the list sits above the panel (table.css stacks the two
    // cards there), 62 tables tall on a full deployment, so once there is a
    // panel to read — a selected table's schema, or the SQL editor — it
    // starts collapsed behind its summary (`TABLE_FILTER_JS` closes it);
    // with nothing selected the list is what the visitor came for and stays
    // open.
    let collapse_on_phone = selected.is_some() || tab == Tab::Sql;
    // A plain card whose body is the `<details>`: its summary is the card's
    // heading, so the card folds on a phone by its own title (the
    // collapsible settings section's shape, `ui::settings_form`).
    Card::plain(html! {
        details .db-tables open data-db-tables data-db-collapse-narrow[collapse_on_phone] {
            summary .db-tables__summary {
                h2 .section-header__title { "Tables" }
                span .db-count { (tables.len()) }
            }
            (components::FilterInput {
                action: "db-table-filter",
                label: "Filter tables",
                placeholder: "Filter tables…",
                controls: "db-table-groups",
            }
            .render())
            div #db-table-groups .db-table-groups {
                @if tables.is_empty() {
                    (empty_note("No tables yet.", None))
                }
                @for (group_key, group_tables) in &groups {
                    @let (org, block) = group_label(group_key);
                    (table_group(group_key, &block, &org, group_tables, selected, tab, true))
                }
                @if !ungrouped.is_empty() {
                    (table_group("_other", "Other", "no block prefix", &ungrouped, selected, tab, false))
                }
                // `empty_note` takes no attributes; the wrapper carries the
                // id and the `hidden` the filter toggles.
                div #db-filter-empty hidden { (empty_note("No tables match.", None)) }
            }
            script { (maud::PreEscaped(TABLE_FILTER_JS)) }
        }
    })
    .render()
}

/// One block's tables: a collapsible row (`<details>`, open) with the
/// block's name, its org and its table count, over the list of its tables.
/// `leaf_names` shows each table by its last `__` segment, which is unique
/// within an `org__block` group; the ungrouped tables show their whole name.
fn table_group(
    key: &str,
    title: &str,
    org: &str,
    tables: &[&TableSummary],
    selected: Option<&str>,
    tab: Tab,
    leaf_names: bool,
) -> Markup {
    html! {
        details .db-table-group open data-db-group=(key) {
            // The name and the org wrap as whole words onto a second line
            // when the column is narrow; the count keeps its own column.
            summary .db-table-group__head {
                span .db-table-group__name {
                    span .db-table-group__title { (title) }
                    " "
                    span .db-table-group__org { (org) }
                }
                span .db-count { (tables.len()) }
            }
            ul .db-table-group__list {
                @for t in tables {
                    @let active = selected == Some(t.name.as_str());
                    @let href = format!("/b/admin/database?table={}&tab={}", pct_encode(&t.name), tab.as_query());
                    @let name = if leaf_names { t.name.rsplit("__").next().unwrap_or(&t.name) } else { t.name.as_str() };
                    li data-db-table=(t.name.to_lowercase()) {
                        a .db-table-list__item .is-active[active]
                            aria-current=[active.then_some("page")]
                            href=(href)
                            hx-get=(href)
                            hx-target="#content"
                            hx-push-url="true"
                        {
                            span .db-table-list__name { (name) }
                            span .db-table-list__count { (t.row_count) }
                        }
                    }
                }
            }
        }
    }
}

/// The table-list filter, which used to be a 478-character minified `oninput`
/// attribute: the longest LITERAL handler in the tree, though not the longest
/// handler — `blocks/userportal/pages/security.rs:73` built a ~521-character
/// one with `format!`. (The specification's figure of 430 was a miscount and
/// is corrected here rather than copied.) Wherever it sat it was unreadable,
/// unlintable and untestable. It hides `[data-db-table]` rows that do not match,
/// hides a `[data-db-group]` whose rows are all hidden, opens a folded group
/// that holds a match, and reveals `#db-filter-empty` when nothing matches at
/// all.
///
/// It also closes the table list where the cards stack (below 900px, the
/// breakpoint `.db-layout` in table.css stacks at) when the page drew it
/// `data-db-collapse-narrow` (see [`left_pane`]); that part runs on every
/// render, the listener only once.
const TABLE_FILTER_JS: &str = r#"
(function () {
  if (window.matchMedia('(max-width: 900px)').matches) {
    document.querySelectorAll('[data-db-collapse-narrow]').forEach(function (d) { d.open = false; });
  }
  if (window.__dbTableFilterInit) return;
  window.__dbTableFilterInit = true;
  document.addEventListener('input', function (e) {
    var el = e.target;
    if (!(el instanceof Element)) return;
    if (el.getAttribute('data-action') !== 'db-table-filter') return;
    var query = el.value.toLowerCase();
    var visible = 0;
    document.querySelectorAll('[data-db-table]').forEach(function (row) {
      var show = (row.getAttribute('data-db-table') || '').indexOf(query) >= 0;
      row.hidden = !show;
      if (show) visible++;
    });
    document.querySelectorAll('[data-db-group]').forEach(function (group) {
      var matches = !!group.querySelector('[data-db-table]:not([hidden])');
      group.hidden = !matches;
      // A group folded by hand still shows what the filter found in it.
      if (query && matches) group.open = true;
    });
    var empty = document.getElementById('db-filter-empty');
    if (empty) empty.hidden = visible !== 0;
  });
})();
"#;

/// Split an `org__block` group key into a display-friendly `(org, block)`
/// pair. `impresspress__admin` → `("impresspress", "admin")`. Leaves the value
/// alone if it doesn't have a `__` (shouldn't happen given the caller's
/// split, but keeps this helper defensive for tests).
fn group_label(group_key: &str) -> (String, String) {
    let (org_raw, block) = match group_key.split_once("__") {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (String::new(), group_key.to_string()),
    };
    // DB-safe identifiers use `_` where the block name's `org/block` form
    // uses `-` (so `impresspress/admin` lands as `impresspress__admin`).
    // Reverse that for the org-label display only — block names are valid
    // ident segments and don't need translation here.
    let org_display = org_raw.replace('_', "-");
    (org_display, block)
}

fn right_pane_tabs(selected: Option<&str>, tab: Tab) -> Markup {
    use crate::ui::components::{tab_navigation, Tab as NavTab};
    let table_qs = selected
        .map(|t| format!("&table={}", pct_encode(t)))
        .unwrap_or_default();
    let schema_href = format!("/b/admin/database?tab=schema{table_qs}");
    let sql_href = format!("/b/admin/database?tab=sql{table_qs}");
    tab_navigation(vec![
        NavTab {
            active: tab == Tab::Schema,
            href: &schema_href,
            label: "Schema",
        },
        NavTab {
            active: tab == Tab::Sql,
            href: &sql_href,
            label: "SQL editor",
        },
    ])
}

/// The schema panel for the selected table, or `Err` when a read failed —
/// the page then answers an error page rather than an empty schema and a
/// `0` count.
async fn schema_panel(ctx: &dyn Context, table: Option<&str>) -> Result<Markup, WaferError> {
    let Some(name) = table else {
        return Ok(empty_note("Select a table to view its columns.", None));
    };

    // The selected name is user input: a name the backend cannot quote, or
    // one it has no table for, is said so in the panel. Columns + row count
    // come from the shared introspection routine used by the JSON API too.
    let (columns, row_count) = match introspect_columns(ctx, name).await {
        Ok(schema) => schema,
        Err(IntrospectError::InvalidName) => {
            return Ok(empty_note(
                &format!("\"{name}\" is not a valid table name."),
                None,
            ))
        }
        Err(IntrospectError::NoSuchTable) => {
            return Ok(empty_note(
                &format!("There is no table named \"{name}\"."),
                None,
            ))
        }
        Err(IntrospectError::Read(e)) => return Err(e),
    };

    let rows: Vec<Vec<Markup>> = columns
        .iter()
        .map(|c| {
            vec![
                html! { span .font-medium { (c.name) } },
                html! { span .text-muted { (c.ty) } },
                yes_mark(c.notnull),
                yes_mark(c.pk),
                html! { span .text-muted { (c.default_value.as_deref().unwrap_or("")) } },
            ]
        })
        .collect();

    Ok(html! {
        (components::section_header(
            name,
            Some(html! { span .text-muted .text-sm .tabular-nums { (rows_label(row_count)) } }),
        ))
        (components::data_table::<fn(usize) -> Option<String>>(
            &SCHEMA_COLUMNS,
            rows,
            None,
            html! {},
        ))
    })
}

/// A schema flag's cell: a check for yes, read out as "Yes" (an `aria-label`
/// on a `<span>` is not announced — a span has no role to name), and nothing
/// for no.
fn yes_mark(yes: bool) -> Markup {
    html! {
        @if yes {
            span aria-hidden="true" { (icons::check()) }
            span .sr-only { "Yes" }
        }
    }
}

/// `"1 row"` / `"3 rows"`.
fn rows_label(n: i64) -> String {
    if n == 1 {
        format!("{n} row")
    } else {
        format!("{n} rows")
    }
}

async fn right_pane(
    ctx: &dyn Context,
    selected: Option<&str>,
    tab: Tab,
) -> Result<Markup, WaferError> {
    let panel = match tab {
        Tab::Schema => schema_panel(ctx, selected).await?,
        Tab::Sql => sql_panel(selected, None, None),
    };
    // Flush: the schema grid and the query results are tables, whose frame
    // the card absorbs (layout rule 2); everything else in the panel takes
    // the card's inset back (`.db-workbench`, table.css).
    Ok(Card::plain(html! {
        div .db-workbench {
            (right_pane_tabs(selected, tab))
            (panel)
        }
    })
    .flush()
    .render())
}

fn sql_panel(selected: Option<&str>, query: Option<&str>, result: Option<Markup>) -> Markup {
    // A table the validator refuses is not prefilled with a query that cannot
    // run: the panel says why and where to go instead, rather than handing an
    // operator a Run button whose only outcome is a 403. Same text the API
    // returns, from the same entry — the page cannot describe the rule
    // differently from the rule.
    let refused = selected.and_then(crate::secret_tables::secret_table_named_in);
    let initial = match (query, selected) {
        (Some(q), _) if !q.is_empty() => q.to_string(),
        _ if refused.is_some() => "SELECT 1;".to_string(),
        (_, Some(t)) => format!("SELECT * FROM {t} LIMIT 100;"),
        _ => "SELECT 1;".to_string(),
    };
    html! {
        form .db-sql
            hx-post="/b/admin/database/query"
            hx-target="#db-sql-results"
            hx-swap="innerHTML"
        {
            @if let Some(t) = selected {
                input type="hidden" name="table" value=(t);
            }
            @if let Some(entry) = refused {
                p .text-muted .text-sm { (entry.refusal()) }
            }
            textarea name="query" rows="6" .db-sql__input
                spellcheck="false"
                placeholder="SELECT … FROM …"
            { (initial) }
            div .db-sql__actions {
                // The page's one primary.
                (Action::primary("Run", ActionTarget::Submit { form: None }).render())
                span .text-muted .text-sm { "Read-only: SELECT, PRAGMA, EXPLAIN, WITH" }
            }
        }
        div #db-sql-results .db-sql-results {
            @if let Some(r) = result { (r) } @else {
                (empty_note("Run a query to see results.", None))
            }
        }
    }
}

fn render_sql_results(rows: &[db::Record], duration_ms: u128) -> Markup {
    if rows.is_empty() {
        return html! {
            p .text-muted .text-sm role="status" { "0 rows in " (duration_ms) "ms" }
        };
    }

    // The grid's columns are the query's, in the order it returned them, so
    // they are built per render rather than declared as a const the way the
    // fixed tables are.
    let columns = result_columns(rows);
    let cols: Vec<components::TableCol<'_>> = columns
        .iter()
        .map(|c| components::TableCol::new(c.as_str()))
        .collect();
    let cells: Vec<components::TableRow> = rows
        .iter()
        .map(|r| {
            components::TableRow::new(
                columns
                    .iter()
                    .map(|c| html! { @if let Some(v) = r.data.get(c) { (format_cell(v)) } })
                    .collect(),
            )
        })
        .collect();

    // A query's columns are arbitrary and can be many: the result stays a
    // grid at every width, in a labelled, focusable horizontal scroller,
    // rather than collapsing each row into a card.
    html! {
        p .text-muted .text-sm role="status" { (rows_label(i64::try_from(rows.len()).unwrap_or(i64::MAX))) " in " (duration_ms) "ms" }
        (components::DataTable::new(&cols).rows(cells).scroll("Query results").render())
    }
}

/// One result value, as the query returned it: the explorer shows what is
/// stored, so a date-time string is not reformatted the way the curated
/// tables' `components::timestamp` does.
fn format_cell(v: &serde_json::Value) -> Markup {
    match v {
        serde_json::Value::Null => html! {},
        serde_json::Value::String(s) => html! { (s) },
        other => html! { (other.to_string()) },
    }
}

/// A refused or failed query, as the shared error alert: `role="alert"`, so
/// the message the Run button's swap inserts is announced, not only painted.
fn render_sql_error(msg: &str) -> Markup {
    components::alert_message(components::AlertVariant::Error, msg)
}

pub async fn database_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let backend = match crate::db_backend(ctx).await {
        Ok(backend) => backend,
        Err(e) => {
            return super::admin_error_page(
                ctx,
                msg,
                "Database",
                e,
                "database page: backend read failed",
            )
            .await
        }
    };
    let selected = msg.query("table");
    let selected = (!selected.is_empty()).then_some(selected);
    let tab = Tab::from_query(msg.query("tab"));

    // A failed listing, count or column read is an error page (403 for a WRAP
    // denial, 429 for a quota, else 500), not an empty database.
    let read = async {
        let tables = introspect_table_summaries(ctx).await?;
        let right = right_pane(ctx, selected, tab).await?;
        Ok::<_, WaferError>((tables, right))
    };
    let (tables, right) = match read.await {
        Ok(read) => read,
        Err(e) => {
            return super::admin_error_page(
                ctx,
                msg,
                "Database",
                e,
                "admin database page: introspection read failed",
            )
            .await
        }
    };

    let body = list_page(
        None,
        html! {
            div .db-layout {
                (left_pane(&tables, selected, tab))
                (right)
            }
        },
        None,
    );

    admin_page(
        ctx,
        msg,
        "Database",
        Topbar {
            crumbs: crumb("Database"),
            subtitle: Some("Browse tables, view schema, run read-only SQL"),
            status: vec![backend_badge(backend, tables.len())],
            ..Topbar::default()
        },
        body,
    )
    .await
}

pub async fn handle_database_query(
    ctx: &dyn Context,
    _msg: &Message,
    input: wafer_run::InputStream,
) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let form = parse_form_body(&raw);
    let query = form.get("query").cloned().unwrap_or_default();

    if let Err(err) = validate_readonly_query(&query) {
        return html_response(render_sql_error(err.message()));
    }

    // `std::time::Instant::now()` panics on wasm32-unknown-unknown (no system
    // clock). `now_millis()` uses chrono which is wasm-safe.
    let started_ms = now_millis();
    let result = db::query_raw(ctx, &query, &[]).await;
    let elapsed = (now_millis() - started_ms) as u128;

    let fragment = match result {
        Ok(rows) => render_sql_results(&rows, elapsed),
        Err(e) => render_sql_error(&format!("Query error: {e}")),
    };
    html_response(fragment)
}

/// The schema panel's columns. Declared once so the `<td data-label>` the
/// component stamps on every cell names the same column the header does.
const SCHEMA_COLUMNS: [components::TableCol<'static>; 5] = [
    components::TableCol::new("Column").primary(),
    components::TableCol::new("Type"),
    components::TableCol::new("Not null"),
    components::TableCol::new("PK"),
    components::TableCol::new("Default").optional(),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::admin::test_support::browser_request,
        test_support::{admin_msg, TestContext},
    };

    /// The visual-baseline suite masks the table-list rows of the two log
    /// tables every request writes to, by the suffix of their
    /// `data-db-table` name, because their row counts follow the suite's own
    /// traffic. Rename either table (or the attribute) and the selector
    /// silently stops matching: the capture then fails on whatever ran
    /// before it, not on the page. This reads the selectors out of the spec
    /// and requires each to match a row this page renders.
    #[tokio::test]
    async fn the_log_table_rows_carry_the_names_the_visual_mask_keys_on() {
        const SPEC: &str =
            include_str!("../../../../../impresspress-web/tests/e2e/visual-baseline.spec.ts");
        const PREFIX: &str = r#"li[data-db-table$=""#;
        let suffixes: Vec<&str> = SPEC
            .match_indices(PREFIX)
            .map(|(at, _)| {
                let rest = &SPEC[at + PREFIX.len()..];
                &rest[..rest.find('"').expect("a closed selector")]
            })
            .collect();
        assert_eq!(
            suffixes,
            ["__request_logs", "__storage_access_logs"],
            "visual-baseline.spec.ts no longer masks the two log tables' rows"
        );

        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let parts = browser_request(&ctx, admin_msg("retrieve", "/b/admin/database")).await;
        assert_eq!(parts.status, 200);
        let html = String::from_utf8(parts.body).expect("UTF-8 body");
        for suffix in suffixes {
            let rows = html
                .match_indices(r#"<li data-db-table=""#)
                .filter(|(at, _)| {
                    let name = &html[at + r#"<li data-db-table=""#.len()..];
                    name[..name.find('"').unwrap_or(0)].ends_with(suffix)
                })
                .count();
            assert_eq!(
                rows, 1,
                "the mask `li[data-db-table$=\"{suffix}\"]` must match exactly one row: {html}"
            );
        }
    }

    /// A failed table listing is a 500, not a database with no tables.
    #[tokio::test]
    async fn a_failed_introspection_is_a_500_not_an_empty_database() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();

        let parts = browser_request(&ctx, admin_msg("retrieve", "/b/admin/database")).await;

        assert_eq!(parts.status, 500);
        let html = String::from_utf8(parts.body).expect("UTF-8 body");
        assert!(!html.contains("db-layout"), "{html}");
    }

    /// A selected name the backend has no table for says so, rather than an
    /// empty schema with "0 rows" — and it is the visitor's typo, not a 500.
    #[tokio::test]
    async fn an_unknown_table_says_so() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let mut msg = admin_msg("retrieve", "/b/admin/database");
        msg.set_meta("req.query.table", "no_such_table");

        let parts = browser_request(&ctx, msg).await;

        assert_eq!(parts.status, 200);
        let html = String::from_utf8(parts.body).expect("UTF-8 body");
        assert!(
            html.contains(
                r#"<p class="empty-note">There is no table named &quot;no_such_table&quot;.</p>"#
            ),
            "{html}"
        );
        assert!(!html.contains("0 rows"), "{html}");
    }

    /// With no table selected, and for a name the backend cannot quote, the
    /// schema panel says so in one line: the panel already sits in a pane,
    /// so it is an `empty_note`, not a second framed surface.
    #[tokio::test]
    async fn the_schema_panel_says_what_to_do_in_one_line() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);

        let parts = browser_request(&ctx, admin_msg("retrieve", "/b/admin/database")).await;
        assert_eq!(parts.status, 200);
        let html = String::from_utf8(parts.body).expect("UTF-8 body");
        assert!(
            html.contains(r#"<p class="empty-note">Select a table to view its columns.</p>"#),
            "{html}"
        );
        assert!(!html.contains(r#"class="empty""#), "{html}");

        let mut msg = admin_msg("retrieve", "/b/admin/database");
        msg.set_meta("req.query.table", "bad name;");
        let parts = browser_request(&ctx, msg).await;
        assert_eq!(parts.status, 200);
        let html = String::from_utf8(parts.body).expect("UTF-8 body");
        assert!(
            html.contains(
                r#"<p class="empty-note">&quot;bad name;&quot; is not a valid table name.</p>"#
            ),
            "{html}"
        );
    }

    /// Control: a real table still shows its columns and count.
    #[tokio::test]
    async fn a_real_table_shows_its_schema() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let mut msg = admin_msg("retrieve", "/b/admin/database");
        msg.set_meta("req.query.table", crate::blocks::admin::ROLES_TABLE);

        let parts = browser_request(&ctx, msg).await;

        assert_eq!(parts.status, 200);
        let html = String::from_utf8(parts.body).expect("UTF-8 body");
        assert!(html.contains(" rows<"), "{html}");
        assert!(html.contains(">name<"), "{html}");
    }

    /// The database page over `ctx` for `query`.
    async fn database_html(ctx: &TestContext, query: &[(&str, &str)]) -> String {
        let mut msg = admin_msg("retrieve", "/b/admin/database");
        for (name, value) in query {
            msg.set_meta(format!("req.query.{name}"), *value);
        }
        let parts = browser_request(ctx, msg).await;
        let html = String::from_utf8(parts.body).expect("UTF-8 body");
        assert_eq!(parts.status, 200, "{html}");
        html
    }

    /// The backend badge is a neutral fact about the deployment, at the
    /// badge's own size.
    #[tokio::test]
    async fn the_backend_badge_is_neutral() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let html = database_html(&ctx, &[]).await;
        assert!(
            html.contains(
                r#"<span class="badge badge-secondary" title="Database backend">SQLite · "#
            ),
            "{html}"
        );
    }

    /// "Tables" and the workbench are two sibling cards on the page
    /// background. The workbench is flush, so the schema grid and the query
    /// results are its one card-level surface (rule 2), and the SQL tab's
    /// Run is the page's one primary.
    #[tokio::test]
    async fn the_tables_and_the_workbench_are_sibling_cards() {
        use crate::test_support::dom;

        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        for query in [
            vec![("table", crate::blocks::admin::ROLES_TABLE)],
            vec![("tab", "sql")],
        ] {
            let html = database_html(&ctx, &query).await;
            let doc = scraper::Html::parse_document(&html);
            let cards: Vec<String> = doc
                .select(&scraper::Selector::parse(".db-layout > .card").unwrap())
                .map(|card| card.value().attr("class").unwrap_or("").to_string())
                .collect();
            assert_eq!(cards, ["card", "card card--flush"], "{query:?}: {html}");
            assert!(
                dom::card_level_inside_card(&html).is_empty(),
                "{query:?}: {:?}",
                dom::card_level_inside_card(&html)
            );
            assert_eq!(
                dom::heading_outline(&html).first(),
                Some(&(1, "Database".to_string())),
                "{query:?}"
            );
            assert!(
                dom::heading_outline(&html).contains(&(2, "Tables".to_string())),
                "{query:?}: the Tables card is headed by its fold's summary: {html}"
            );
            let (primaries, _) = dom::visible_primaries(&html);
            let expected: &[&str] = if query[0].0 == "tab" {
                &[r#"button.btn.btn--primary "Run""#]
            } else {
                &[]
            };
            assert_eq!(primaries, expected, "{query:?}");
        }
    }

    /// Each block's tables are one open fold headed by the block, its org and
    /// its count, with no frame of its own; a table's row count is plain
    /// running text, not the old 12px utility.
    #[tokio::test]
    async fn each_block_is_an_open_fold_with_its_name_org_and_count() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let html = database_html(&ctx, &[]).await;
        assert!(
            html.contains(concat!(
                r#"<details class="db-table-group" open data-db-group="impresspress__admin">"#,
                r#"<summary class="db-table-group__head"><span class="db-table-group__name">"#,
                r#"<span class="db-table-group__title">admin</span> "#,
                r#"<span class="db-table-group__org">impresspress</span></span><span class="db-count">"#,
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="db-table-list__count">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div id="db-filter-empty" hidden><p class="empty-note">No tables match.</p></div>"#),
            "{html}"
        );
        // The filter is the shared in-page filter box (`FilterInput`): a
        // `search` landmark naming the list it narrows.
        assert!(
            html.contains(r#"<div class="search-input" role="search"><span class="search-input-icon">"#)
                && html.contains(r#"data-action="db-table-filter" placeholder="Filter tables…" aria-label="Filter tables" aria-controls="db-table-groups""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="db-table-groups" id="db-table-groups">"#),
            "{html}"
        );
    }

    /// The list folds where the two cards stack: the media query in
    /// `TABLE_FILTER_JS` is the one `table.css` stacks `.db-layout` under.
    /// The breakpoint is written in both (CSS cannot read a Rust constant),
    /// so this keeps them one value.
    #[test]
    fn the_list_folds_at_the_width_the_cards_stack() {
        const CSS: &str = include_str!("../../../ui/styles/components/table.css");
        let stack = CSS
            .find(".db-layout { grid-template-columns: minmax(0, 1fr); }")
            .expect("the stacking rule");
        let media = &CSS[CSS[..stack].rfind("@media ").expect("its media query") + 7..stack];
        let media = media[..media.find('{').expect("the query's block")].trim();
        assert_eq!(media, "(max-width: 900px)");
        assert!(
            TABLE_FILTER_JS.contains(&format!("window.matchMedia('{media}')")),
            "TABLE_FILTER_JS folds the list at another width than {media}"
        );
    }

    #[test]
    fn group_label_translates_underscores_to_dashes_in_org() {
        // `org__block` keys come from splitting table names like
        // `impresspress__admin__users` on `__`. The org segment uses `_` as
        // its separator; the display form uses `-`, matching how blocks
        // are referenced everywhere else (e.g. "impresspress/admin").
        let (org, block) = group_label("impresspress__admin");
        assert_eq!(org, "impresspress");
        assert_eq!(block, "admin");
    }

    #[test]
    fn group_label_handles_single_segment_keys() {
        // Defensive: caller currently never passes a single-segment key,
        // but if it ever does we put the whole thing into `block` rather
        // than panicking.
        let (org, block) = group_label("variables");
        assert_eq!(org, "");
        assert_eq!(block, "variables");
    }
}
