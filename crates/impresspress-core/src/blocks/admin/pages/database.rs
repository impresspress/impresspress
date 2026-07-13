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
use wafer_run::{context::Context, Message, OutputStream};

use super::{admin_page, crumb};
use crate::{
    blocks::admin::database::{
        introspect_columns, introspect_table_summaries, validate_readonly_query, TableSummary,
    },
    ui::{
        html_response, icons,
        shell::Topbar,
        templates::{list_page, PageHeader},
        SiteConfig, UserInfo,
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
    html! {
        span .badge .badge-info .text-xs title="Database backend" {
            (label) " · " (table_count) " tables"
        }
    }
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
                    autocomplete="off"
                    oninput="(function(e){var q=e.target.value.toLowerCase();var visible=0;document.querySelectorAll('[data-db-table]').forEach(function(li){var n=li.getAttribute('data-db-table');var show=n.indexOf(q)>=0;li.style.display=show?'':'none';if(show)visible++;});document.querySelectorAll('[data-db-group]').forEach(function(g){var anyVisible=g.querySelector('[data-db-table]:not([style*=\"none\"])');g.style.display=anyVisible?'':'none';});var empty=document.getElementById('db-filter-empty');if(empty)empty.style.display=visible===0?'':'none';})(event)";
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
                    style="display:none" { "No tables match." }
            }
        }
    }
}

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

async fn schema_panel(ctx: &dyn Context, table: Option<&str>) -> Markup {
    let Some(name) = table else {
        return html! {
            div .empty-state {
                p { "Select a table on the left to view its schema." }
            }
        };
    };

    // The selected name is user input; an invalid identifier renders the same
    // empty state as a table the backend can't introspect. Columns + row count
    // come from the shared introspection routine used by the JSON API too.
    let (columns, row_count) = introspect_columns(ctx, name).await;

    html! {
        div .db-panel {
            header .db-panel__head {
                h3 { (name) }
                span .text-muted .text-sm { (row_count) " rows" }
            }
            @if columns.is_empty() {
                div .empty-state { p { "No columns introspected (table may be empty or backend doesn't support it)." } }
            } @else {
                div .table-container {
                    table .table {
                        thead {
                            tr {
                                th { "Column" }
                                th { "Type" }
                                th { "Not null" }
                                th { "PK" }
                                th { "Default" }
                            }
                        }
                        tbody {
                            @for c in &columns {
                                tr {
                                    td .font-medium { (c.name) }
                                    td .text-muted { (c.ty) }
                                    td { @if c.notnull { "✓" }  }
                                    td { @if c.pk { "✓" }  }
                                    td .text-muted .text-sm { (c.default_value.as_deref().unwrap_or("")) }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn right_pane(ctx: &dyn Context, selected: Option<&str>, tab: Tab) -> Markup {
    html! {
        section .db-pane .db-pane--right {
            (right_pane_tabs(selected, tab))
            div .db-panel-body {
                @match tab {
                    Tab::Schema => (schema_panel(ctx, selected).await),
                    Tab::Sql => (sql_panel(selected, None, None)),
                }
            }
        }
    }
}

fn sql_panel(selected: Option<&str>, query: Option<&str>, result: Option<Markup>) -> Markup {
    let initial = match (query, selected) {
        (Some(q), _) if !q.is_empty() => q.to_string(),
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
                textarea name="query" rows="6" .db-sql__input
                    spellcheck="false"
                    placeholder="SELECT … FROM …"
                { (initial) }
                div .db-sql__actions {
                    button .btn .btn-primary type="submit" { "Run" }
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

    html! {
        p .text-muted .text-sm { (rows.len()) " rows in " (duration_ms) "ms" }
        div .table-container {
            table .table {
                thead {
                    tr {
                        @for c in &columns { th { (c) } }
                    }
                }
                tbody {
                    @for r in rows {
                        tr {
                            @for c in &columns {
                                td .text-sm {
                                    @if let Some(v) = r.data.get(c) {
                                        (format_cell(v))
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
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
    let config = SiteConfig::load(ctx).await;
    let user = UserInfo::from_message(msg);

    let tables = introspect_table_summaries(ctx).await;
    let backend = crate::db_backend(ctx).await;
    let selected = msg.query("table");
    let tab = Tab::from_query(msg.query("tab"));

    let body = list_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        None,
        html! {
            div .db-layout {
                (left_pane(&tables, if selected.is_empty() { None } else { Some(selected) }, tab))
                (right_pane(ctx, if selected.is_empty() { None } else { Some(selected) }, tab).await)
            }
        },
        None,
    );

    admin_page(
        "Database",
        &config,
        "/b/admin/database",
        user.as_ref(),
        Topbar {
            crumbs: crumb("Database"),
            primary_action: Some(backend_badge(backend, tables.len())),
            subtitle: Some("Browse tables, view schema, run read-only SQL"),
            show_palette: true,
        },
        body,
        msg,
    )
}

pub async fn handle_database_query(
    ctx: &dyn Context,
    _msg: &Message,
    input: wafer_run::InputStream,
) -> OutputStream {
    let raw = input.collect_to_bytes().await;
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

#[cfg(test)]
mod tests {
    use super::*;

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
