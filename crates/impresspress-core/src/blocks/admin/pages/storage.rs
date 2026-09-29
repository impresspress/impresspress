use maud::{html, Markup};
use wafer_block::db::{ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream};

use super::{admin_page, crumb};
use crate::{
    blocks::admin::STORAGE_ACCESS_LOGS_TABLE as STORAGE_ACCESS_LOGS,
    ui::{
        components::{self, badge, BadgeVariant},
        icons,
        shell::Topbar,
        templates::list_page,
    },
};

pub async fn storage_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let refresh_action = html! {
        button .btn .btn--secondary .btn--sm
            hx-get="/b/admin/storage"
            hx-target="#content"
        { (icons::refresh_cw()) " Refresh" }
    };

    // "No storage access logs yet." is what a deployment whose blocks have
    // never touched storage renders; an unreadable log must not borrow it.
    let logs_tab = match storage_logs_tab(ctx, msg).await {
        Ok(markup) => markup,
        Err(e) => {
            return crate::blocks::crud::db_error_page(
                msg,
                e,
                "admin storage page: access-log read failed",
            )
        }
    };

    let tabs_and_body = html! {
        (components::tab_navigation(vec![components::Tab {
            active: true,
            href: "/b/admin/storage",
            label: "Access Logs",
            icon: Some(icons::eye()),
        }]))

        div #storage-tab-content {
            (logs_tab)
        }
    };

    let body = list_page(None, tabs_and_body, None);

    admin_page(
        ctx,
        msg,
        "Storage",
        Topbar {
            crumbs: crumb("Storage"),
            primary_action: Some(refresh_action),
            subtitle: Some("Per-block storage isolation and access logs"),
            show_palette: true,
        },
        body,
    )
    .await
}

async fn storage_logs_tab(
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
                html! { span .font-mono { (path) } },
                html! {
                    @if status.starts_with("BLOCKED") {
                        (badge(BadgeVariant::Danger, &status))
                    } @else if status.starts_with("ERROR") {
                        (badge(BadgeVariant::Warning, &status))
                    } @else {
                        span .text-muted { (status) }
                    }
                },
                html! { span .text-muted { (created.get(..19).unwrap_or(&created)) } },
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
    components::TableCol {
        label: "Block",
        width: None,
    },
    components::TableCol {
        label: "Operation",
        width: None,
    },
    components::TableCol {
        label: "Path",
        width: None,
    },
    components::TableCol {
        label: "Status",
        width: None,
    },
    components::TableCol {
        label: "Time",
        width: None,
    },
];

#[cfg(test)]
mod outage_tests {
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
        let msg = admin_msg("retrieve", "/b/admin/storage");
        assert_eq!(
            output_http_status(storage_page(&ctx, &msg).await).await,
            500
        );
    }
}
