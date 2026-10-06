use maud::{html, Markup};
use wafer_block::db::{ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message};

use crate::{
    blocks::{
        admin::STORAGE_ACCESS_LOGS_TABLE as STORAGE_ACCESS_LOGS,
        files::{repo::objects::object_key_of_blob, FILES_BLOCK_ID},
    },
    ui::components::{self, badge, BadgeVariant},
    util::RecordExt,
};

/// What a person reads for a logged storage path: the part under the calling
/// block's own namespace (the storage handler resolves a plain folder there,
/// so `impresspress/files/photos/…` is that block's `photos/…`), and for the
/// files block the object key rather than the per-upload blob key it stores
/// the bytes under (`photos/{claim}~a.png` reads `photos/a.png`).
fn display_path(source_block: &str, path: &str) -> String {
    let own = path
        .strip_prefix(source_block)
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(path);
    if source_block == FILES_BLOCK_ID {
        object_key_of_blob(own)
    } else {
        own.to_string()
    }
}

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
                "duration_ms".into(),
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
        .enumerate()
        .map(|(i, log)| {
            let source = log.str_field("source_block");
            let path = log.str_field("path");
            let status = log.str_field("status");
            let full_id = format!("storage-log-path-{i}");
            vec![
                html! {
                    @if !source.is_empty() {
                        (badge(BadgeVariant::Info, source))
                    }
                },
                html! { span .font-mono { (log.str_field("operation")) } },
                html! {
                    @if !path.is_empty() {
                        span .font-mono title=(path) { (components::breakable_id(&display_path(source, path))) }
                        " "
                        // The whole stored path, for a search elsewhere: copied
                        // from the hidden element (chrome.js `copy-text`).
                        span #(full_id) hidden { (path) }
                        button .btn .btn--ghost .btn--sm type="button"
                            data-action="copy-text" data-copy-source=(full_id)
                            aria-label=(format!("Copy the full path {path}"))
                        { "Copy" }
                    }
                },
                html! {
                    @if status.starts_with("BLOCKED") {
                        (badge(BadgeVariant::Danger, status))
                    } @else if status.starts_with("ERROR") {
                        (badge(BadgeVariant::Warning, status))
                    } @else {
                        span .text-muted { (status) }
                    }
                },
                html! {
                    @if let Some(ms) = log.data.get("duration_ms").and_then(serde_json::Value::as_i64) {
                        span .text-muted .tabular-nums { (ms) "ms" }
                    }
                },
                html! { span .text-muted { (components::timestamp(log.str_field("created_at"))) } },
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
const STORAGE_LOG_COLUMNS: [components::TableCol<'static>; 6] = [
    components::TableCol::new("Block"),
    components::TableCol::new("Operation"),
    components::TableCol::new("Path").primary(),
    components::TableCol::new("Status"),
    components::TableCol::new("Duration").optional(),
    components::TableCol::new("Time"),
];

#[cfg(test)]
mod tests {
    use super::display_path;

    /// A path reads under its block's own namespace, and a files blob key as
    /// the object it stores; anything else stands as written.
    #[test]
    fn a_storage_path_reads_as_the_object_it_names() {
        assert_eq!(
            display_path(
                "impresspress/files",
                "impresspress/files/photos/22d8ce89-8a0f-47ac-b57a-19b31dd5f104~a.png"
            ),
            "photos/a.png"
        );
        assert_eq!(
            display_path("wafer-run/web", "wafer-run/web/site/index.html"),
            "site/index.html"
        );
        assert_eq!(
            display_path("impresspress/files", "@acme/x/y~z"),
            "@acme/x/y~z",
            "another block's namespace is shown whole"
        );
        assert_eq!(
            display_path("wafer-run/web", "wafer-run/web"),
            "wafer-run/web"
        );
    }
}
