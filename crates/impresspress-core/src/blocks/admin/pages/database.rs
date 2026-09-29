//! Database admin page — table browser + schema viewer + SQL editor.
//!
//! Layout: two-pane.
//! * Left pane (~32%): table list with row counts + filter input.
//! * Right pane: tabs (Schema / SQL editor). Schema is the default tab.
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
    blocks::admin::database::{
        introspect_columns, introspect_table_summaries, validate_readonly_query, IntrospectError,
        TableSummary,
    },
    ui::{
        components::{self, Badge, BadgeVariant},
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

fn backend_badge(backend: wafer_sql_utils::Backend, table_count: usize) -> Markup {
    let label = match backend {
        wafer_sql_utils::Backend::Sqlite => "SQLite",
        wafer_sql_utils::Backend::Postgres => "PostgreSQL",
    };
    Badge::new(BadgeVariant::Info)
        .classes("text-xs")
        .title("Database backend")
        .render(html! { (label) " · " (table_count) " tables" })
}

fn left_pane(tables: &[TableSummary], selected: Option<&str>, tab: Tab) -> Markup {
    // Group tables by their `org__block` prefix (first two `__`-separated
    // segments). Tables without `__` (e.g. legacy `variables`) go in their
    // own "Other" section. Each group renders as a small card with an
    // org/block heading, a count badge, and an always-visible table list —
    // no more collapsible `<details>` carets. The filter input below hides
    // matching rows without needing to expand/collapse anything.
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

    html! {
        aside .db-pane .db-pane--left {
            div .db-pane__head {
                input #db-filter type="text"
                    placeholder="Filter tables…"
                    aria-label="Filter tables"
                    autocomplete="off"
                    data-action="db-table-filter";
            }
            div .db-table-groups {
                @if tables.is_empty() {
                    div .db-table-list__empty .text-muted .text-sm { "No tables yet" }
                }
                @for (group_key, group_tables) in &groups {
                    @let (org, block) = group_label(group_key);
                    section .db-table-group data-db-group=(group_key) {
                        header .db-table-group__head {
                            span .db-table-group__icon { (icons::package()) }
                            div .db-table-group__title-wrap {
                                span .db-table-group__title { (block) }
                                span .db-table-group__org .text-muted { (org) }
                            }
                            span .db-table-group__count { (group_tables.len()) }
                        }
                        ul .db-table-group__list {
                            @for t in group_tables {
                                @let active = selected == Some(t.name.as_str());
                                @let encoded_name = pct_encode(&t.name);
                                @let leaf = t.name.rsplit("__").next().unwrap_or(&t.name);
                                li data-db-table=(t.name.to_lowercase()) {
                                    a .db-table-list__item .(if active { "is-active" } else { "" })
                                        aria-current=[active.then_some("page")]
                                        href={"/b/admin/database?table=" (encoded_name) "&tab=" (tab.as_query())}
                                        hx-get={"/b/admin/database?table=" (encoded_name) "&tab=" (tab.as_query())}
                                        hx-target="#content"
                                        hx-push-url="true"
                                    {
                                        span .db-table-list__name { (leaf) }
                                        span .db-table-list__count .text-muted .text-xs { (t.row_count) }
                                    }
                                }
                            }
                        }
                    }
                }
                @if !ungrouped.is_empty() {
                    section .db-table-group data-db-group="_other" {
                        header .db-table-group__head {
                            span .db-table-group__icon { (icons::database()) }
                            div .db-table-group__title-wrap {
                                span .db-table-group__title { "Other" }
                                span .db-table-group__org .text-muted { "no block prefix" }
                            }
                            span .db-table-group__count { (ungrouped.len()) }
                        }
                        ul .db-table-group__list {
                            @for t in &ungrouped {
                                @let active = selected == Some(t.name.as_str());
                                @let encoded_name = pct_encode(&t.name);
                                li data-db-table=(t.name.to_lowercase()) {
                                    a .db-table-list__item .(if active { "is-active" } else { "" })
                                        aria-current=[active.then_some("page")]
                                        href={"/b/admin/database?table=" (encoded_name) "&tab=" (tab.as_query())}
                                        hx-get={"/b/admin/database?table=" (encoded_name) "&tab=" (tab.as_query())}
                                        hx-target="#content"
                                        hx-push-url="true"
                                    {
                                        span .db-table-list__name { (t.name) }
                                        span .db-table-list__count .text-muted .text-xs { (t.row_count) }
                                    }
                                }
                            }
                        }
                    }
                }
                div #db-filter-empty .db-table-list__empty .text-muted .text-sm
                    hidden { "No tables match." }
            }
            script { (maud::PreEscaped(TABLE_FILTER_JS)) }
        }
    }
}

/// The table-list filter, which used to be a 478-character minified `oninput`
/// attribute: the longest LITERAL handler in the tree, though not the longest
/// handler — `blocks/userportal/pages/security.rs:73` built a ~521-character
/// one with `format!`. (The specification's figure of 430 was a miscount and
/// is corrected here rather than copied.) Wherever it sat it was unreadable,
/// unlintable and untestable. It hides `[data-db-table]` rows that do not match,
/// collapses a `[data-db-group]` whose rows are all hidden, and reveals
/// `#db-filter-empty` when nothing matches at all.
const TABLE_FILTER_JS: &str = r#"
(function () {
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
      group.hidden = !group.querySelector('[data-db-table]:not([hidden])');
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
            icon: None,
        },
        NavTab {
            active: tab == Tab::Sql,
            href: &sql_href,
            label: "SQL editor",
            icon: None,
        },
    ])
}

/// The schema panel for the selected table, or `Err` when a read failed —
/// the page then answers an error page rather than an empty schema and a
/// `0` count.
async fn schema_panel(ctx: &dyn Context, table: Option<&str>) -> Result<Markup, WaferError> {
    let Some(name) = table else {
        return Ok(html! {
            div .empty-state {
                p { "Select a table on the left to view its schema." }
            }
        });
    };

    // The selected name is user input: a name the backend cannot quote, or
    // one it has no table for, is said so in the panel. Columns + row count
    // come from the shared introspection routine used by the JSON API too.
    let (columns, row_count) = match introspect_columns(ctx, name).await {
        Ok(schema) => schema,
        Err(IntrospectError::InvalidName) => {
            return Ok(html! {
                div .empty-state { p { "\"" (name) "\" is not a valid table name." } }
            })
        }
        Err(IntrospectError::NoSuchTable) => {
            return Ok(html! {
                div .empty-state { p { "There is no table named \"" (name) "\"." } }
            })
        }
        Err(IntrospectError::Read(e)) => return Err(e),
    };

    let rows: Vec<Vec<Markup>> = columns
        .iter()
        .map(|c| {
            vec![
                html! { span .font-medium { (c.name) } },
                html! { span .text-muted { (c.ty) } },
                html! { @if c.notnull { span aria-label="Yes" { (icons::check()) } } },
                html! { @if c.pk { span aria-label="Yes" { (icons::check()) } } },
                html! { span .text-muted { (c.default_value.as_deref().unwrap_or("")) } },
            ]
        })
        .collect();

    Ok(html! {
        div .db-panel {
            header .db-panel__head {
                h3 { (name) }
                span .text-muted .text-sm { (row_count) " rows" }
            }
            (components::data_table::<fn(usize) -> Option<String>>(
                &SCHEMA_COLUMNS,
                rows,
                None,
                html! {},
            ))
        }
    })
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
    Ok(html! {
        section .db-pane .db-pane--right {
            (right_pane_tabs(selected, tab))
            div .db-panel-body { (panel) }
        }
    })
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
        div .db-panel {
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
                    button .btn .btn--primary type="submit" { "Run" }
                    span .text-muted .text-sm { "Read-only: SELECT, PRAGMA, EXPLAIN, WITH" }
                }
            }
            div #db-sql-results .db-sql-results {
                @if let Some(r) = result { (r) } @else {
                    p .text-muted .text-sm { "Run a query to see results." }
                }
            }
        }
    }
}

fn render_sql_results(rows: &[db::Record], duration_ms: u128) -> Markup {
    if rows.is_empty() {
        return html! {
            p .text-muted .text-sm { "0 rows in " (duration_ms) "ms" }
        };
    }

    // Stable column ordering: union of keys, in first-row order then any new
    // keys appended. A HashSet keeps membership lookup O(1) so the overall
    // pass is O(rows × cols) instead of O(rows × cols²).
    let mut columns: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in rows {
        for k in r.data.keys() {
            if seen.insert(k.clone()) {
                columns.push(k.clone());
            }
        }
    }

    // The result grid's columns are the query's, so they are built per render
    // rather than declared as a const the way the fixed tables are.
    let cols: Vec<components::TableCol<'_>> = columns
        .iter()
        .map(|c| components::TableCol {
            label: c.as_str(),
            width: None,
        })
        .collect();
    let cells: Vec<Vec<Markup>> = rows
        .iter()
        .map(|r| {
            columns
                .iter()
                .map(|c| html! { @if let Some(v) = r.data.get(c) { (format_cell(v)) } })
                .collect()
        })
        .collect();

    html! {
        p .text-muted .text-sm { (rows.len()) " rows in " (duration_ms) "ms" }
        (components::data_table::<fn(usize) -> Option<String>>(&cols, cells, None, html! {}))
    }
}

fn format_cell(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "".to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn render_sql_error(msg: &str) -> Markup {
    html! {
        div .login-error { (msg) }
    }
}

pub async fn database_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let backend = match crate::db_backend(ctx).await {
        Ok(backend) => backend,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "database page: backend read failed")
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
            return crate::blocks::crud::db_error_page(
                msg,
                e,
                "admin database page: introspection read failed",
            )
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
            primary_action: Some(backend_badge(backend, tables.len())),
            subtitle: Some("Browse tables, view schema, run read-only SQL"),
            show_palette: true,
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
    components::TableCol {
        label: "Column",
        width: None,
    },
    components::TableCol {
        label: "Type",
        width: None,
    },
    components::TableCol {
        label: "Not null",
        width: None,
    },
    components::TableCol {
        label: "PK",
        width: None,
    },
    components::TableCol {
        label: "Default",
        width: None,
    },
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
            html.contains("There is no table named &quot;no_such_table&quot;."),
            "{html}"
        );
        assert!(!html.contains("0 rows"), "{html}");
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
