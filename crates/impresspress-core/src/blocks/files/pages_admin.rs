//! SSR admin pages for the files block.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use super::repo;
use crate::{
    ui::{self, components, icons, shell::Crumb},
    util::{format_bytes, RecordExt},
};

/// Tabs navigation across the storage-admin sub-pages
/// (Overview / Buckets / Shares / Quotas). `active` matches the
/// crumb label so the active tab can be highlighted.
///
/// Designed to slot into `list_page`'s `filters` arg (the same slot the
/// Users tabs use), so the tab strip lives inside `.page--list` and picks
/// up the page padding consistently.
pub(crate) fn admin_tabs(active: &str) -> Markup {
    let items: &[(&str, &str)] = &[
        ("Overview", "/b/storage/admin/"),
        ("Buckets", "/b/storage/admin/buckets"),
        ("Shares", "/b/storage/admin/shares"),
        ("Quotas", "/b/storage/admin/quotas"),
    ];
    html! {
        div .tabs {
            @for (label, href) in items {
                a class={ "tab" @if *label == active { " active" } } href=(href) { (label) }
            }
        }
    }
}

async fn files_page<'a>(
    ctx: &dyn Context,
    title: &'a str,
    crumb_label: &'a str,
    subtitle: Option<&'a str>,
    content: Markup,
    msg: &Message,
) -> OutputStream {
    files_page_with_action(ctx, title, crumb_label, subtitle, None, content, msg).await
}

/// Admin storage shell. Thin wrapper over [`ui::shell_page`] that fixes the
/// nav to Admin and keeps the storage pages' single-crumb shape; tabs ride in
/// each caller's `list_page` `filters` slot (matching `/b/admin/users`).
async fn files_page_with_action<'a>(
    ctx: &dyn Context,
    title: &'a str,
    crumb_label: &'a str,
    subtitle: Option<&'a str>,
    primary_action: Option<Markup>,
    content: Markup,
    msg: &Message,
) -> OutputStream {
    ui::shell_page(
        ctx,
        msg,
        ui::Shell {
            title,
            nav: ui::NavKind::Admin,
            crumbs: vec![Crumb {
                label: crumb_label,
                href: None,
            }],
            subtitle,
            primary_action,
        },
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct AdminStats {
    pub buckets: i64,
    pub files: i64,
    pub total_size_bytes: i64,
    pub shares: i64,
    pub quotas_count: i64,
}

/// Render the 4 stat cards (Buckets, Files, Total Size, Active Shares)
/// for the admin storage overview. Designed for the `list_page` template's
/// `filters` slot. Pure helper — no `Context` access.
pub fn render_admin_overview_stats(stats: &AdminStats) -> Markup {
    html! {
        div .stats-grid {
            (components::stat_card("Buckets", &stats.buckets.to_string(), icons::folder()))
            (components::stat_card("Files", &stats.files.to_string(), icons::file_text()))
            (components::stat_card("Total Size", &format_bytes(stats.total_size_bytes), icons::hard_drive()))
            (components::stat_card("Active Shares", &stats.shares.to_string(), icons::globe()))
        }
    }
}

/// Render the "create your first bucket" CTA for the empty admin storage
/// overview. Renders empty markup once at least one bucket exists — this is
/// a first-run nudge, not a permanent overview fixture. Links to the
/// Buckets tab, which owns the actual "+ New bucket" trigger (the modal +
/// `files-browser.js` bootstrap script) — no duplicate modal wiring here.
pub fn render_admin_overview_empty_cta(bucket_count: i64) -> Markup {
    if bucket_count > 0 {
        return html! {};
    }
    components::empty_state(
        icons::folder(),
        "Create your first bucket",
        "Buckets hold the files uploaded through Storage. Create one to get started.",
        Some(html! {
            a .btn .btn--primary .btn--md href="/b/storage/admin/buckets" { "+ New bucket" }
        }),
    )
}

/// Render the optional "X user(s) with custom quotas" hint card.
/// Returns an empty markup when `quotas_count == 0`. Pure helper.
pub fn render_admin_overview_quotas_hint(quotas_count: i64) -> Markup {
    if quotas_count <= 0 {
        return html! {};
    }
    html! {
        div .card style="padding:1rem" {
            p .text-muted style="font-size:0.875rem" {
                (quotas_count) " user(s) with custom quotas configured."
            }
        }
    }
}

pub async fn overview(ctx: &dyn Context, msg: &Message) -> OutputStream {
    use crate::ui::templates::{list_page, PageHeader};

    let stats = load_admin_stats(ctx).await;

    // Tabs go in the `filters` slot (their padding gutter matches
    // /b/admin/users); stats live in the body. Keeping them in separate
    // slots prevents `.page-filters` (display:flex) from putting tabs
    // and the stats-grid side-by-side at wide viewports.
    let body = list_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        Some(admin_tabs("Overview")),
        html! {
            (render_admin_overview_stats(&stats))
            (render_admin_overview_empty_cta(stats.buckets))
            (render_admin_overview_quotas_hint(stats.quotas_count))
        },
        None,
    );

    files_page(
        ctx,
        "Storage",
        "Overview",
        Some("File storage statistics"),
        body,
        msg,
    )
    .await
}

async fn load_admin_stats(ctx: &dyn Context) -> AdminStats {
    let buckets = repo::buckets::count_all(ctx).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e.message, "admin overview: bucket count failed");
        0
    });

    let files = repo::objects::count_completed(ctx)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e.message, "admin overview: files count failed");
            0
        });

    let shares = repo::shares::count_all(ctx).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e.message, "admin overview: shares count failed");
        0
    });

    let quotas_count = repo::quota::count_all(ctx).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e.message, "admin overview: quotas count failed");
        0
    });

    let total_size_bytes = repo::objects::sum_size_completed(ctx)
        .await
        .map(|s| s as i64)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e.message, "admin overview: total size sum failed");
            0
        });

    AdminStats {
        buckets,
        files,
        total_size_bytes,
        shares,
        quotas_count,
    }
}

// ---------------------------------------------------------------------------
// Buckets
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct AdminBucketRow {
    pub name: String,
    pub owner_short: String,
    pub public: bool,
    pub created_at_short: String,
}

/// Render the admin Buckets table (or empty state).
pub fn render_admin_buckets_table(rows: &[AdminBucketRow]) -> Markup {
    if rows.is_empty() {
        return html! {
            div .empty-state { p { "No buckets" } }
        };
    }
    html! {
        table .data-table {
            thead { tr {
                th { "Name" }
                th { "Owner" }
                th { "Public" }
                th { "Created" }
            } }
            tbody {
                @for r in rows {
                    tr data-bucket=(r.name) {
                        td data-label="Name" .font-medium { (r.name) }
                        td data-label="Owner" .text-muted .text-sm { (r.owner_short) }
                        td data-label="Public" {
                            (components::status_badge(if r.public { "public" } else { "private" }))
                        }
                        td data-label="Created" .text-muted .text-sm { (r.created_at_short) }
                    }
                }
            }
        }
    }
}

pub async fn buckets(ctx: &dyn Context, msg: &Message) -> OutputStream {
    use crate::ui::templates::{list_page, PageHeader};

    let rows: Vec<AdminBucketRow> = match repo::buckets::list_recent(ctx, 100).await {
        Ok(list) => list
            .records
            .into_iter()
            .map(|r| AdminBucketRow {
                name: r.str_field("name").to_string(),
                owner_short: r
                    .str_field("created_by")
                    .get(..8)
                    .unwrap_or("—")
                    .to_string(),
                public: r.str_field("public") == "true",
                created_at_short: r
                    .str_field("created_at")
                    .get(..10)
                    .unwrap_or("")
                    .to_string(),
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e.message, "admin bucket list failed");
            Vec::new()
        }
    };

    // Admin can create buckets the same way users do — re-use the
    // native <dialog> modal + JS from `pages_user`. The bootstrap
    // script with empty bucket/prefix is needed for the JS to wire
    // the "+ New bucket" trigger; without it the JS bails on init.
    let js_url = crate::ui::assets::files_browser_js_url();
    let body = list_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        Some(admin_tabs("Buckets")),
        html! {
            (render_admin_buckets_table(&rows))
            (super::pages_user::render_new_bucket_modal())
            script type="application/json" id="files-browser-bootstrap" {
                "{}"
            }
            script src=(js_url) defer {}
        },
        None,
    );

    files_page_with_action(
        ctx,
        "Buckets",
        "Buckets",
        Some("All storage buckets"),
        Some(crate::ui::components::button(
            crate::ui::components::BtnVariant::Primary,
            crate::ui::components::CtrlSize::Sm,
            "+ New bucket",
            maud::PreEscaped(r#"type="button" data-action="open-new-bucket""#.to_string()),
        )),
        body,
        msg,
    )
    .await
}

// ---------------------------------------------------------------------------
// Shares
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct AdminShareRow {
    pub token_short: String,
    pub bucket: String,
    pub key: String,
    pub access_count: i64,
    pub max_access_count: Option<i64>,
    pub expires_short: Option<String>,
    pub owner_short: String,
}

/// Render the admin Shares table (or empty state). Token displayed as
/// short prefix in a `<code>` block; access count includes optional
/// "/ N" divisor when a max is set; "Never" renders for unset expires.
pub fn render_admin_shares_table(rows: &[AdminShareRow]) -> Markup {
    if rows.is_empty() {
        return html! {
            div .empty-state { p { "No active shares" } }
        };
    }
    html! {
        table .data-table {
            thead { tr {
                th { "Token" }
                th { "Bucket" }
                th { "File" }
                th { "Access Count" }
                th { "Expires" }
                th { "Created By" }
            } }
            tbody {
                @for r in rows {
                    tr data-share-token=(r.token_short) {
                        td data-label="Token" .text-sm { code { (r.token_short) "..." } }
                        td data-label="Bucket" .font-medium { (r.bucket) }
                        td data-label="File" .text-sm { (r.key) }
                        td data-label="Access Count" .text-sm {
                            (r.access_count)
                            @if let Some(max) = r.max_access_count {
                                @if max > 0 { " / " (max) }
                            }
                        }
                        td data-label="Expires" .text-muted .text-sm {
                            @if let Some(exp) = &r.expires_short { (exp) } @else { "Never" }
                        }
                        td data-label="Created By" .text-muted .text-sm { (r.owner_short) }
                    }
                }
            }
        }
    }
}

pub async fn shares(ctx: &dyn Context, msg: &Message) -> OutputStream {
    use crate::ui::templates::{list_page, PageHeader};

    let rows: Vec<AdminShareRow> = match repo::shares::list_recent(ctx, 100, 0).await {
        Ok(list) => list
            .records
            .into_iter()
            .map(|r| {
                let max_str = r.str_field("max_access_count");
                let max = if max_str.is_empty() {
                    None
                } else {
                    max_str.parse::<i64>().ok().filter(|n| *n > 0)
                };
                let exp_str = r.str_field("expires_at");
                let expires_short = if exp_str.is_empty() {
                    None
                } else {
                    Some(exp_str.get(..10).unwrap_or(exp_str).to_string())
                };
                AdminShareRow {
                    token_short: r.str_field("token").get(..12).unwrap_or("—").to_string(),
                    bucket: r.str_field("bucket").to_string(),
                    key: r.str_field("key").to_string(),
                    access_count: r.i64_field("access_count"),
                    max_access_count: max,
                    expires_short,
                    owner_short: r
                        .str_field("created_by")
                        .get(..8)
                        .unwrap_or("—")
                        .to_string(),
                }
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e.message, "admin shares list failed");
            Vec::new()
        }
    };

    let body = list_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        Some(admin_tabs("Shares")),
        render_admin_shares_table(&rows),
        None,
    );

    files_page(
        ctx,
        "Shares",
        "Shares",
        Some("Public file share links"),
        body,
        msg,
    )
    .await
}

// ---------------------------------------------------------------------------
// Quotas
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct AdminQuotaRow {
    pub user_short: String,
    pub max_storage_bytes: i64,
    pub max_file_size_bytes: i64,
    pub max_files_per_bucket: i64,
}

/// Render the admin Storage Quotas table (or empty state). Bytes
/// columns humanize via `format_bytes`; user_id is truncated to the
/// first 8 chars in the loader. Pure helper.
pub fn render_admin_quotas_table(rows: &[AdminQuotaRow]) -> Markup {
    if rows.is_empty() {
        return html! {
            div .empty-state {
                p { "No custom quotas. Default: 1 GB storage, 100 MB file size, 10,000 files per bucket." }
            }
        };
    }
    html! {
        table .data-table {
            thead { tr {
                th { "User" }
                th { "Max Storage" }
                th { "Max File Size" }
                th { "Max Files/Bucket" }
            } }
            tbody {
                @for r in rows {
                    tr {
                        td data-label="User" .text-sm { (r.user_short) }
                        td data-label="Max Storage" .text-sm { (format_bytes(r.max_storage_bytes)) }
                        td data-label="Max File Size" .text-sm { (format_bytes(r.max_file_size_bytes)) }
                        td data-label="Max Files/Bucket" .text-sm { (r.max_files_per_bucket) }
                    }
                }
            }
        }
    }
}

pub async fn quotas(ctx: &dyn Context, msg: &Message) -> OutputStream {
    use crate::ui::templates::{list_page, PageHeader};

    let rows: Vec<AdminQuotaRow> = match repo::quota::list_recent(ctx, 100).await {
        Ok(list) => list
            .records
            .into_iter()
            .map(|r| AdminQuotaRow {
                user_short: r.str_field("user_id").get(..8).unwrap_or("—").to_string(),
                max_storage_bytes: r.i64_field("max_storage_bytes"),
                max_file_size_bytes: r.i64_field("max_file_size_bytes"),
                max_files_per_bucket: r.i64_field("max_files_per_bucket"),
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e.message, "admin quotas list failed");
            Vec::new()
        }
    };

    let body = list_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        Some(admin_tabs("Quotas")),
        render_admin_quotas_table(&rows),
        None,
    );

    files_page(
        ctx,
        "Quotas",
        "Quotas",
        Some("Per-user storage limits"),
        body,
        msg,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_admin_overview_stats_renders_four_stat_cards() {
        let stats = AdminStats {
            buckets: 3,
            files: 42,
            total_size_bytes: 2_500_000_000,
            shares: 5,
            quotas_count: 1,
        };
        let html = render_admin_overview_stats(&stats).into_string();
        assert!(html.contains(">3<"), "buckets count missing: {html}");
        assert!(html.contains(">42<"), "files count missing: {html}");
        assert!(html.contains(">5<"), "shares count missing: {html}");
        // total_size_bytes 2.5 GB → "2.3 GB" via format_bytes (or close).
        assert!(html.contains("GB"), "size humanization missing: {html}");
    }

    #[test]
    fn render_admin_overview_empty_cta_shown_when_zero_buckets() {
        let html = render_admin_overview_empty_cta(0).into_string();
        assert!(
            html.contains("Create your first bucket"),
            "cta title missing: {html}"
        );
        assert!(
            html.contains(r#"href="/b/storage/admin/buckets""#),
            "cta should link to the Buckets tab (the real create trigger): {html}"
        );
        assert!(html.contains("New bucket"), "cta label missing: {html}");
    }

    #[test]
    fn render_admin_overview_empty_cta_hidden_when_buckets_exist() {
        let html = render_admin_overview_empty_cta(3).into_string();
        assert!(
            html.trim().is_empty(),
            "cta should be hidden once a bucket exists: {html}"
        );
    }

    #[tokio::test]
    async fn overview_page_shows_create_bucket_cta_when_empty() {
        use crate::test_support::{admin_msg, output_html, TestContext};

        let ctx = TestContext::with_files().await;
        let msg = admin_msg("retrieve", "/b/storage/admin/");
        let html = output_html(overview(&ctx, &msg).await).await;

        assert!(
            html.contains("Create your first bucket"),
            "empty-state CTA missing from the live overview render: {html}"
        );
        assert!(
            html.contains(r#"href="/b/storage/admin/buckets""#),
            "CTA should link to the Buckets tab: {html}"
        );
    }

    #[tokio::test]
    async fn overview_page_hides_create_bucket_cta_once_a_bucket_exists() {
        use crate::test_support::{admin_msg, output_html, TestContext};

        let ctx = TestContext::with_files().await;
        let mut row: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();
        row.insert("name".into(), serde_json::json!("photos"));
        row.insert("created_by".into(), serde_json::json!("admin_1"));
        repo::buckets::seed(&ctx, row).await.expect("seed bucket");

        let msg = admin_msg("retrieve", "/b/storage/admin/");
        let html = output_html(overview(&ctx, &msg).await).await;

        assert!(
            !html.contains("Create your first bucket"),
            "CTA should be gone once a bucket exists: {html}"
        );
    }

    #[test]
    fn render_admin_overview_quotas_hint_when_present() {
        let html = render_admin_overview_quotas_hint(3).into_string();
        assert!(
            html.contains("3 user(s) with custom quotas"),
            "quotas hint missing: {html}"
        );
    }

    #[test]
    fn render_admin_overview_quotas_hint_empty_when_zero() {
        let html = render_admin_overview_quotas_hint(0).into_string();
        // Empty markup or no visible "with custom quotas" copy.
        assert!(
            !html.contains("with custom quotas"),
            "should be empty when zero: {html}"
        );
    }

    #[test]
    fn render_admin_buckets_table_empty_state() {
        let html = render_admin_buckets_table(&[]).into_string();
        assert!(html.contains("No buckets"), "missing empty hint: {html}");
    }

    #[test]
    fn render_admin_buckets_table_renders_rows() {
        let rows = vec![
            AdminBucketRow {
                name: "photos".into(),
                owner_short: "admin_1".into(),
                public: true,
                created_at_short: "2026-05-06".into(),
            },
            AdminBucketRow {
                name: "docs".into(),
                owner_short: "user_42".into(),
                public: false,
                created_at_short: "2026-05-05".into(),
            },
        ];
        let html = render_admin_buckets_table(&rows).into_string();
        assert!(html.contains(">photos<"), "name missing: {html}");
        assert!(html.contains(">docs<"));
        assert!(html.contains("admin_1"));
        // status_badge renders class names containing "public" / "private".
        assert!(html.contains("public"));
        assert!(html.contains("private"));
        assert!(html.contains("2026-05-06"));
    }

    #[test]
    fn render_admin_shares_table_empty_state() {
        let html = render_admin_shares_table(&[]).into_string();
        assert!(html.contains("No active shares"), "missing empty: {html}");
    }

    #[test]
    fn render_admin_shares_table_renders_rows() {
        let rows = vec![AdminShareRow {
            token_short: "tok12345abc1".into(),
            bucket: "photos".into(),
            key: "a.png".into(),
            access_count: 4,
            max_access_count: Some(10),
            expires_short: Some("2026-06-06".into()),
            owner_short: "admin_1".into(),
        }];
        let html = render_admin_shares_table(&rows).into_string();
        assert!(html.contains("tok12345abc1"));
        assert!(html.contains(">photos<"));
        assert!(html.contains(">a.png<"));
        // access_count and max rendered together as "4 / 10"
        assert!(
            html.contains("4 / 10"),
            "access count + max missing: {html}"
        );
        // max_access_count rendered as "/ 10"
        assert!(html.contains("/ 10"));
        assert!(html.contains("2026-06-06"));
        assert!(html.contains("admin_1"));
    }

    #[test]
    fn render_admin_shares_table_no_expires_renders_never() {
        let rows = vec![AdminShareRow {
            token_short: "abc".into(),
            bucket: "b".into(),
            key: "k".into(),
            access_count: 0,
            max_access_count: None,
            expires_short: None,
            owner_short: "u".into(),
        }];
        let html = render_admin_shares_table(&rows).into_string();
        assert!(
            html.contains("Never"),
            "missing 'Never' for null expires: {html}"
        );
        // No "/ N" segment when max_access_count is None.
        assert!(
            !html.contains("/ "),
            "should not show max divisor when None: {html}"
        );
    }

    #[test]
    fn render_admin_quotas_table_empty_state() {
        let html = render_admin_quotas_table(&[]).into_string();
        assert!(
            html.contains("No custom quotas"),
            "missing empty hint: {html}"
        );
        // Default values surfaced in the empty state copy.
        assert!(html.contains("1 GB"), "missing 1 GB default copy: {html}");
    }

    #[test]
    fn render_admin_quotas_table_renders_rows() {
        let rows = vec![AdminQuotaRow {
            user_short: "user_1".into(),
            max_storage_bytes: 5_000_000_000,
            max_file_size_bytes: 100_000_000,
            max_files_per_bucket: 1000,
        }];
        let html = render_admin_quotas_table(&rows).into_string();
        assert!(html.contains("user_1"), "user missing: {html}");
        // 5 GB ≈ "4.7 GB" via format_bytes humanization.
        assert!(html.contains("GB"), "GB unit missing: {html}");
        // 100 MB → "95.4 MB".
        assert!(html.contains("MB"), "MB unit missing: {html}");
        // max_files_per_bucket as integer in its own cell.
        assert!(
            html.contains(">1000<"),
            "files-per-bucket count missing: {html}"
        );
    }
}
