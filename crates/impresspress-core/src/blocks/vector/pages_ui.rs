//! User-facing UI pages for the impresspress/vector block.
//!
//! Pure render helpers live alongside async handlers; helpers are
//! unit-tested directly without `Context`.

use maud::{html, Markup};
use wafer_core::clients::vector as vclient;
use wafer_run::{context::Context, Message, OutputStream};

use super::service::{display_index_name, vector_backend_available, IndexRow};
use crate::ui::{
    self,
    shell::Crumb,
    templates::{detail_page, list_page, DetailHero, DetailMeta, PageHeader},
};

/// htmx-friendly success render for `POST /b/vector/api/indexes` — re-loads
/// the index list so the modal swap shows the new row.
pub async fn render_index_list_fragment(ctx: &dyn Context) -> Result<Markup, String> {
    let rows = super::service::list_index_rows(ctx)
        .await
        .map_err(|e| e.to_string())?;
    Ok(html! {
        div #vector-index-list { (render_index_list_table(&rows)) }
        (render_create_index_modal())
    })
}

/// Modal markup for creating a vector index. Always shipped pre-rendered
/// next to the index list; opening it is a `openModal('create-vector-index')`
/// onclick on the topbar action button.
pub fn render_create_index_modal() -> Markup {
    crate::ui::components::modal(
        "create-vector-index",
        "Create vector index",
        html! {
            form hx-post="/b/vector/api/indexes" hx-target="#vector-index-list" hx-swap="outerHTML" {
                div .form-group {
                    label .form-label .required for="vec-name" { "Name" }
                    input .form-input type="text" #vec-name name="name" placeholder="e.g. docs" required;
                }
                div .form-group {
                    label .form-label for="vec-model" { "Embedding model" }
                    input .form-input type="text" #vec-model name="model" placeholder="(default — leave blank)";
                }
                div .form-group {
                    label .form-label .checkbox-inline {
                        input type="checkbox" name="keyword_search" value="on";
                        " Enable keyword (full-text) search alongside vectors"
                    }
                }
                div .form-actions {
                    button .btn .btn-secondary type="button" onclick="closeModal('create-vector-index')" { "Cancel" }
                    button .btn .btn-primary type="submit" { "Create" }
                }
            }
        },
    )
}

pub fn render_index_list_table(rows: &[IndexRow]) -> Markup {
    if rows.is_empty() {
        return html! {
            div .empty-state {
                p { "No vector indexes yet." }
            }
        };
    }

    html! {
        table .data-table {
            thead { tr {
                th { "Name" }
                th { "Model" }
                th { "Dimensions" }
                th { "Vectors" }
                th { "Keyword search" }
            } }
            tbody {
                @for r in rows {
                    @let display = display_index_name(&r.name);
                    tr data-index-name=(display) {
                        td data-label="Name" { (display) }
                        td data-label="Model" { (r.model) }
                        td data-label="Dimensions" { (r.dimensions) }
                        td data-label="Vectors" { (r.vector_count) }
                        td data-label="Keyword search" {
                            @if r.keyword_search {
                                span .badge.badge-success { "Yes" }
                            } @else {
                                span .badge { "No" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Render the body sections for an index detail page: a Stats section
/// (counts/model/dimensions/keyword toggle) and a Schema section showing
/// the underlying storage table name plus introspected columns. Pure
/// helper so the markup can be unit-tested without spinning a `Context`.
pub fn render_index_detail_sections(
    row: &IndexRow,
    schema_cols: &[(String, String)],
) -> Vec<Markup> {
    let stats = html! {
        section .section {
            h3 { "Stats" }
            dl .kv-list {
                dt { "Vector count" } dd { (row.vector_count) }
                dt { "Dimensions" }   dd { (row.dimensions) }
                dt { "Model" }        dd { (row.model) }
                dt { "Keyword search" }
                dd {
                    @if row.keyword_search {
                        span .badge.badge-success { "Yes" }
                    } @else {
                        span .badge { "No" }
                    }
                }
            }
        }
    };

    let schema = html! {
        section .section {
            h3 { "Schema" }
            p { "Storage table: " code { (row.name) } }
            @if !schema_cols.is_empty() {
                table .data-table {
                    thead { tr { th { "Column" } th { "Type" } } }
                    tbody {
                        @for (n, t) in schema_cols {
                            tr {
                                td data-label="Column" { (n) }
                                td data-label="Type" { (t) }
                            }
                        }
                    }
                }
            }
        }
    };

    vec![stats, schema]
}

/// GET `/b/vector/` — admin-facing index listing.
///
/// Reads the per-index metadata registry (`impresspress__vector__registry`)
/// and decorates each row with a live vector count from the underlying
/// `_meta` table. A failure to load the registry (e.g. fresh DB where the
/// table doesn't exist yet) falls through to the empty state — the
/// `wafer-block-sqlite` service returns an empty list rather than erroring
/// for unknown collections, so this only logs when something more serious
/// happens.
pub async fn index_list_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let rows = match super::service::list_index_rows(ctx).await {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!(error = %e, "vector index list failed");
            Vec::new()
        }
    };

    // Native impresspress doesn't ship a `wafer-run/vector` backend block —
    // creating/upserting against an index would 500 with "block not found".
    // Detect the missing backend at page render so we can hide the Create
    // button and surface an actionable callout instead of letting the user
    // hit a generic htmx error. Single-sourced with the JSON API's own
    // backend-availability gate (`pages.rs`) via `service::vector_backend_available`.
    let backend_available = vector_backend_available(ctx);

    let body = list_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        None,
        html! {
            @if !backend_available {
                div .callout .callout--warning style="margin-bottom: var(--spacing-md); padding: var(--spacing-md); background: #fff8e1; border: 1px solid #f0d78c; border-radius: var(--radius-md); color: #92400e" {
                    strong { "Vector backend not available" }
                    p style="margin: 4px 0 0; font-size: 13px" {
                        "The "
                        code style="font-size: 12px; padding: 1px 4px; background: rgba(0,0,0,0.05); border-radius: 3px" { "wafer-run/vector" }
                        " block isn't registered in this build, so indexes can't be created or queried here. Use the browser-WASM build (with "
                        code style="font-size: 12px; padding: 1px 4px; background: rgba(0,0,0,0.05); border-radius: 3px" { "impresspress-web" }
                        ") or wire a vector service via your runtime config."
                    }
                }
            }
            div #vector-index-list { (render_index_list_table(&rows)) }
            @if backend_available {
                (render_create_index_modal())
            }
        },
        None,
    );

    let primary_action = if backend_available {
        Some(crate::ui::components::button(
            crate::ui::components::BtnVariant::Primary,
            crate::ui::components::CtrlSize::Sm,
            "+ Create index",
            maud::PreEscaped(
                r#"type="button" onclick="openModal('create-vector-index')""#.to_string(),
            ),
        ))
    } else {
        None
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell {
            title: "Vector indexes",
            nav: ui::NavKind::Admin,
            crumbs: vec![Crumb {
                label: "Vector indexes",
                href: None,
            }],
            subtitle: Some("Per-index counts, model, dimensions"),
            primary_action,
        },
        body,
    )
    .await
}

/// GET `/b/vector/{name}/` — admin-facing single-index detail page.
///
/// Validates the user-facing index name, resolves it to its prefixed
/// storage name, and looks up the registry row + meta-table count via
/// `service::get_index_row`. Schema columns are introspected from the
/// `_meta` table — empty on a fresh DB, in which case the helper omits
/// the schema table. Any 404 path (invalid name, missing row) goes to
/// `ui::not_found_response`.
pub async fn index_detail_page(ctx: &dyn Context, msg: &Message, name: &str) -> OutputStream {
    if super::service::validate_index_name(name).is_err() {
        return ui::not_found_response(msg);
    }
    let storage_name = super::service::prefixed_index_name(name);

    let row = match super::service::get_index_row(ctx, &storage_name).await {
        Ok(Some(r)) => r,
        Ok(None) => return ui::not_found_response(msg),
        Err(e) => {
            tracing::warn!(error = %e, "vector index lookup failed");
            return ui::not_found_response(msg);
        }
    };

    // Real column state of the meta table via the typed `vector.describe_index`
    // op (WRAP-authorized on the index). Lenient like before: any error — or a
    // missing index (`exists: false`) — renders an empty schema section.
    let schema_cols: Vec<(String, String)> = vclient::describe_index(ctx, &storage_name)
        .await
        .map(|d| {
            d.columns
                .into_iter()
                .map(|c| (c.name, c.sql_type))
                .collect()
        })
        .unwrap_or_default();

    let display = display_index_name(&row.name);
    let subtitle = format!(
        "{} · {} dimensions",
        if row.model.is_empty() {
            "(no model)"
        } else {
            row.model.as_str()
        },
        row.dimensions
    );

    let sections = render_index_detail_sections(&row, &schema_cols);
    let body = detail_page(
        DetailHero {
            icon: None,
            title: display,
            subtitle: Some(&subtitle),
            badges: Vec::new(),
            action_menu: None,
        },
        sections,
        Vec::<DetailMeta<'_>>::new(),
    );

    ui::shell_page(
        ctx,
        msg,
        ui::Shell {
            title: display,
            nav: ui::NavKind::Admin,
            crumbs: vec![
                Crumb {
                    label: "Vector indexes",
                    href: Some("/b/vector/"),
                },
                Crumb {
                    label: display,
                    href: None,
                },
            ],
            subtitle: None,
            primary_action: None,
        },
        body,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_index(name: &str, model: &str, dims: u32, count: u64, kw: bool) -> IndexRow {
        IndexRow {
            name: name.into(),
            model: model.into(),
            dimensions: dims,
            vector_count: count,
            keyword_search: kw,
        }
    }

    #[test]
    fn render_index_list_table_renders_rows_and_empty_state() {
        let empty = render_index_list_table(&[]).into_string();
        assert!(
            empty.contains("No vector indexes yet"),
            "missing empty hint: {empty}"
        );

        let rows = vec![sample_index("docs", "fastembed", 384, 1234, true)];
        let html = render_index_list_table(&rows).into_string();
        assert!(html.contains("docs"), "missing index name");
        assert!(html.contains("fastembed"), "missing model");
        assert!(html.contains("384"), "missing dimensions");
        assert!(html.contains("1234"), "missing count");
    }

    #[test]
    fn render_index_list_table_strips_storage_prefix() {
        let row = sample_index("impresspress__vector__docs", "fastembed", 384, 0, false);
        let html = render_index_list_table(&[row]).into_string();
        assert!(html.contains(">docs<"), "prefix not stripped: {html}");
        assert!(!html.contains("impresspress__vector__"), "raw prefix leaked");
    }

    fn join(sections: &[Markup]) -> String {
        sections.iter().map(|m| m.clone().into_string()).collect()
    }

    #[test]
    fn render_index_detail_sections_includes_count_and_model() {
        let row = IndexRow {
            name: "impresspress__vector__docs".into(),
            model: "fastembed".into(),
            dimensions: 384,
            vector_count: 42,
            keyword_search: true,
        };
        let schema_cols = vec![
            ("id".to_string(), "TEXT".to_string()),
            ("vector".to_string(), "BLOB".to_string()),
        ];
        let html = join(&render_index_detail_sections(&row, &schema_cols));

        assert!(html.contains("42"), "missing vector count");
        assert!(html.contains("fastembed"), "missing model");
        assert!(html.contains("384"), "missing dimensions");
        assert!(
            html.contains("impresspress__vector__docs"),
            "missing storage table name"
        );
        assert!(html.contains("vector"), "missing column name");
        assert!(html.contains("BLOB"), "missing column type");
    }

    #[test]
    fn render_index_detail_sections_keyword_badge_off() {
        let row = IndexRow {
            name: "x".into(),
            model: "fastembed".into(),
            dimensions: 16,
            vector_count: 0,
            keyword_search: false,
        };
        let html = join(&render_index_detail_sections(&row, &[]));
        assert!(html.contains("No"), "keyword badge missing for kw=false");
    }
}

#[cfg(test)]
mod integration_tests {
    use std::{collections::HashMap, sync::Arc};

    use serde_json::json;
    use wafer_block::{
        core_types::{LifecycleEvent, WaferError},
        types::BlockInfo,
        Block,
    };
    use wafer_core::clients::database as db;
    use wafer_run::{ErrorCode, InputStream};

    use super::*;
    use crate::test_support::{admin_msg, output_html, TestContext};

    /// Minimal `wafer-run/vector` stand-in answering `vector.describe_index`
    /// with the columns of the `_meta` fixture table and `vector.count` by
    /// counting that same fixture table. The TestContext's database service
    /// owns a private in-memory connection, so a real `SqliteVecService`
    /// can't see fixture tables; the real describe/count behavior is covered
    /// upstream by wafer-run's sqlite behavior tests and handler
    /// authorization suites. This fake keeps the page-rendering assertions
    /// meaningful: the rendered count still traces back to the seeded row.
    struct FakeVectorBlock;

    #[wafer_block::wafer_async_trait]
    impl Block for FakeVectorBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("wafer-run/vector", "0.0.1", "vector@v1", "test fake")
        }

        async fn handle(
            &self,
            ctx: &dyn Context,
            msg: Message,
            input: InputStream,
        ) -> OutputStream {
            match msg.kind.as_str() {
                "vector.count" => {
                    let body = input.collect_to_bytes().await;
                    let req: wafer_block::wire::vector::CountRequest =
                        wafer_block::codec::decode(&body).expect("decode count request");
                    let count = db::count(ctx, &format!("{}_meta", req.index), &[])
                        .await
                        .expect("count fixture _meta table") as u64;
                    let resp = wafer_block::wire::vector::CountResponse { count };
                    OutputStream::respond(
                        wafer_block::codec::encode(&resp).expect("encode count response"),
                    )
                }
                "vector.describe_index" => {
                    let resp = wafer_block::wire::vector::DescribeIndexResponse {
                        exists: true,
                        columns: [
                            ("id", "TEXT"),
                            ("vector_id", "TEXT"),
                            ("created_at", "TEXT"),
                            ("updated_at", "TEXT"),
                        ]
                        .into_iter()
                        .map(|(name, sql_type)| wafer_block::wire::vector::ColumnInfo {
                            name: name.into(),
                            sql_type: sql_type.into(),
                        })
                        .collect(),
                        keyword_search: true,
                    };
                    OutputStream::respond(
                        wafer_block::codec::encode(&resp).expect("encode describe response"),
                    )
                }
                other => OutputStream::error(WaferError::new(
                    ErrorCode::Unimplemented,
                    format!("FakeVectorBlock has no handler for '{other}'"),
                )),
            }
        }

        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _e: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// Seed one row in the vector registry plus the matching `_meta` table
    /// so the listing has both a registry entry and a vector count to show.
    async fn seed_docs_index(ctx: &TestContext) {
        // Registry row.
        let mut registry_row: HashMap<String, serde_json::Value> = HashMap::new();
        registry_row.insert("prefixed_name".into(), json!("impresspress__vector__docs"));
        registry_row.insert("model".into(), json!("fastembed"));
        registry_row.insert("dimensions".into(), json!(384));
        registry_row.insert("keyword_search".into(), json!(1));
        db::create(ctx, "impresspress__vector__registry", registry_row)
            .await
            .expect("seed registry row");

        // Per-index `_meta` table — created on demand in production by the
        // upstream `wafer-run/vector` runtime block via `vclient::create_index`
        // (see vector/migrations/mod.rs header). The runtime no longer
        // auto-creates tables on first insert, so the test materialises it
        // explicitly with the columns `FakeVectorBlock`'s count/describe
        // arms expect.
        db::exec_raw(
            ctx,
            "CREATE TABLE IF NOT EXISTS impresspress__vector__docs_meta (id TEXT PRIMARY KEY, vector_id TEXT, created_at TEXT, updated_at TEXT)",
            &[],
        )
        .await
        .expect("create _meta table");
        let mut meta_row: HashMap<String, serde_json::Value> = HashMap::new();
        meta_row.insert("vector_id".into(), json!("v1"));
        db::create(ctx, "impresspress__vector__docs_meta", meta_row)
            .await
            .expect("seed meta row");
    }

    #[tokio::test]
    async fn index_list_page_renders_admin_view() {
        let mut ctx = TestContext::with_vector().await;
        // Counts now flow through the vector service (`vclient::count`),
        // so the list page needs the backend block registered to see a
        // non-zero count.
        ctx.register_block("wafer-run/vector", Arc::new(FakeVectorBlock));
        seed_docs_index(&ctx).await;

        let msg = admin_msg("retrieve", "/b/vector/");
        let resp = index_list_page(&ctx, &msg).await;
        let body = output_html(resp).await;

        assert!(
            body.contains("Vector indexes"),
            "missing page header: {body}"
        );
        assert!(body.contains(">docs<"), "seeded row missing: {body}");
        assert!(body.contains("fastembed"), "missing model column: {body}");
        // The count cell is rendered as `<td data-label="Vectors">1</td>`.
        // Asserting the exact substring proves the count traces through
        // `vclient::count` back to the seeded row (a broken index name or
        // an unregistered backend would silently render 0).
        assert!(
            body.contains(r#"data-label="Vectors">1<"#),
            "vector count cell should show 1, got: {body}"
        );
    }

    #[tokio::test]
    async fn index_list_page_count_degrades_to_zero_without_backend() {
        // No `wafer-run/vector` block registered: `vclient::count` fails
        // with NotFound and `map_index_row` must degrade the count to 0
        // (same fallback the API stats route uses) instead of erroring
        // the whole listing.
        let ctx = TestContext::with_vector().await;
        seed_docs_index(&ctx).await;

        let msg = admin_msg("retrieve", "/b/vector/");
        let resp = index_list_page(&ctx, &msg).await;
        let body = output_html(resp).await;

        assert!(body.contains(">docs<"), "seeded row missing: {body}");
        assert!(
            body.contains(r#"data-label="Vectors">0<"#),
            "vector count cell should degrade to 0, got: {body}"
        );
    }

    #[tokio::test]
    async fn index_list_page_renders_empty_state_on_fresh_db() {
        // No registry table at all: handler must fall through cleanly to
        // the "No vector indexes yet" empty state, not error out.
        let ctx = TestContext::with_vector().await;

        let msg = admin_msg("retrieve", "/b/vector/");
        let resp = index_list_page(&ctx, &msg).await;
        let body = output_html(resp).await;

        assert!(
            body.contains("Vector indexes"),
            "missing page header: {body}"
        );
        assert!(
            body.contains("No vector indexes yet"),
            "missing empty state copy: {body}"
        );
    }

    #[tokio::test]
    async fn index_detail_page_404_for_missing() {
        let ctx = TestContext::with_vector().await;
        let mut msg = admin_msg("retrieve", "/b/vector/missing/");
        // Browser-style request → not_found_response returns the styled
        // 404 page; without text/html it returns the JSON err path which
        // panics through `output_html`.
        msg.set_meta("http.header.accept", "text/html");
        let out = index_detail_page(&ctx, &msg, "missing").await;
        let body = output_html(out).await;
        assert!(
            body.contains("Not found") || body.contains("404"),
            "expected 404 status_page: {body}"
        );
    }

    #[tokio::test]
    async fn index_detail_page_404_for_invalid_name() {
        // Names with disallowed characters never reach the database —
        // they're rejected at the route boundary by validate_index_name.
        let ctx = TestContext::with_vector().await;
        let mut msg = admin_msg("retrieve", "/b/vector/bad-name/");
        msg.set_meta("http.header.accept", "text/html");
        let out = index_detail_page(&ctx, &msg, "bad-name").await;
        let body = output_html(out).await;
        assert!(
            body.contains("Not found") || body.contains("404"),
            "expected 404 status_page: {body}"
        );
    }

    #[tokio::test]
    async fn index_detail_page_happy_path() {
        let mut ctx = TestContext::with_vector().await;
        ctx.register_block("wafer-run/vector", Arc::new(FakeVectorBlock));
        seed_docs_index(&ctx).await;

        let msg = admin_msg("retrieve", "/b/vector/docs/");
        let out = index_detail_page(&ctx, &msg, "docs").await;
        let body = output_html(out).await;

        assert!(body.contains("Vector count"), "missing stats label: {body}");
        // seed_docs_index inserts one row in the _meta table.
        assert!(
            body.contains(">1<"),
            "vector count cell should show 1, got: {body}"
        );
        assert!(body.contains("docs"), "missing display name");
        assert!(body.contains("fastembed"), "missing model");
        assert!(
            body.contains("impresspress__vector__docs"),
            "missing storage table name in schema section: {body}"
        );
        assert!(
            body.contains("vector_id"),
            "schema column should render: {body}"
        );
    }
}
