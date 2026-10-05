use maud::{html, Markup};
use wafer_block::db::{ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message};

use crate::{
    blocks::admin::STORAGE_ACCESS_LOGS_TABLE as STORAGE_ACCESS_LOGS,
    ui::components::{self, badge, BadgeVariant},
};

/// The storage access log: the Logs page's "Storage access" tab
/// ([`super::logs_page`]). A log belongs with the other logs, and a second
/// "Storage" in the sidebar would name two things (the Files admin is
/// "Storage"); `/b/admin/storage` redirects to the tab.
///
/// `Err` when the read failed; the Logs page answers it, never an empty log.
pub(super) async fn storage_logs_tab(
    ctx: &dyn Context,
    _msg: &Message,
) -> Result<Markup, wafer_run::WaferError> {
    let logs = db::list(
        ctx,
        STORAGE_ACCESS_LOGS,
        &ListOptions {
            columns: Some(vec![
                "source_block".into(),
                "operation".into(),
                "path".into(),
                "status".into(),
                "created_at".into(),
            ]),
            sort: vec![SortField {
                field: "created_at".into(),
                desc: true,
            }],
            limit: Some(100),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?
    .records;

    let rows: Vec<Vec<Markup>> = logs
        .iter()
        .map(|log| {
            let field = |name: &str| {
                log.data
                    .get(name)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let source = field("source_block");
            let op = field("operation");
            let path = field("path");
            let status = field("status");
            let created = field("created_at");
            vec![
                html! {
                    @if !source.is_empty() {
                        (badge(BadgeVariant::Info, &source))
                    }
                },
                html! { span .font-mono { (op) } },
                html! { span .font-mono { (components::breakable_id(&path)) } },
                html! {
                    @if status.starts_with("BLOCKED") {
                        (badge(BadgeVariant::Danger, &status))
                    } @else if status.starts_with("ERROR") {
                        (badge(BadgeVariant::Warning, &status))
                    } @else {
                        span .text-muted { (status) }
                    }
                },
                html! { span .text-muted { (components::timestamp(&created)) } },
            ]
        })
        .collect();

    Ok(html! {
        p .text-muted .mb-4 {
            "Recent storage access by blocks. Each block is isolated to "
            code { "/storage/{block-name}/" }
            "."
        }

        (components::data_table::<fn(usize) -> Option<String>>(
            &STORAGE_LOG_COLUMNS,
            rows,
            None,
            html! { p .text-center .text-muted { "No storage access logs yet." } },
        ))
    })
}

/// The access-log table's columns. Declared once so the `<td data-label>` the
/// component stamps on every cell names the same column the header does.
const STORAGE_LOG_COLUMNS: [components::TableCol<'static>; 5] = [
    components::TableCol::new("Block"),
    components::TableCol::new("Operation"),
    components::TableCol::new("Path").primary(),
    components::TableCol::new("Status"),
    components::TableCol::new("Time"),
];
