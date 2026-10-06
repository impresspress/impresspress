//! SSR admin pages for the files block.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use super::{models::QuotaConfig, repo};
use crate::{
    ui::{
        self,
        components::{self, DataTable, TableCol, TableRow},
        icons,
    },
    util::format_bytes,
};

/// The storage-admin sections (Overview / Buckets / Shares / Quotas) as the
/// shared sub-nav the shell draws above the content card, the current one
/// marked `aria-current="page"`. `active` is the section's label.
pub(crate) fn admin_sections(active: &str) -> Markup {
    let items: [(&str, &str); 4] = [
        ("Overview", "/b/storage/admin/"),
        ("Buckets", "/b/storage/admin/buckets"),
        ("Shares", "/b/storage/admin/shares"),
        ("Quotas", "/b/storage/admin/quotas"),
    ];
    components::subnav(
        "Storage sections",
        items
            .iter()
            .map(|(label, href)| components::Tab {
                active: *label == active,
                href,
                label,
                icon: None,
            })
            .collect(),
    )
}

/// Admin storage shell: the admin nav, the page's one crumb, its subtitle and
/// actions, and the storage sections above the body.
async fn files_page(
    ctx: &dyn Context,
    msg: &Message,
    section: &str,
    subtitle: &str,
    actions: Vec<Markup>,
    content: Markup,
) -> OutputStream {
    let title = if section == "Overview" {
        "Storage"
    } else {
        section
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::admin(title, title)
            .subtitle(subtitle)
            .actions(actions),
        ui::PageBody::from(content).with_subnav(admin_sections(section)),
    )
    .await
}

/// A storage admin page whose read failed: drawn in the admin shell, with a
/// link back to the dashboard.
async fn error_page(
    ctx: &dyn Context,
    msg: &Message,
    error: wafer_run::WaferError,
    context: &str,
) -> OutputStream {
    ui::shell_error_page(
        ctx,
        msg,
        ui::Shell::admin("Storage", "Storage"),
        None,
        ui::BackLink::ADMIN_DASHBOARD,
        error,
        context,
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
            (components::stat_card("Buckets", &stats.buckets.to_string(), icons::folder(), None))
            (components::stat_card("Files", &stats.files.to_string(), icons::file_text(), None))
            (components::stat_card("Total Size", &format_bytes(stats.total_size_bytes), icons::hard_drive(), None))
            (components::stat_card("Active Shares", &stats.shares.to_string(), icons::globe(), None))
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
        "No buckets yet",
        "Buckets hold uploaded files. Create one to start uploading.",
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
        p .text-muted .text-sm {
            a href="/b/storage/admin/quotas" {
                (crate::util::format_count(quotas_count))
                @if quotas_count == 1 { " user has" } @else { " users have" }
                " a custom quota."
            }
        }
    }
}

pub async fn overview(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let stats = match load_admin_stats(ctx).await {
        Ok(stats) => stats,
        Err(e) => return error_page(ctx, msg, e, "storage admin overview: stats read").await,
    };

    let body = html! {
        (render_admin_overview_stats(&stats))
        (render_admin_overview_empty_cta(stats.buckets))
        (render_admin_overview_quotas_hint(stats.quotas_count))
    };

    files_page(
        ctx,
        msg,
        "Overview",
        "File storage statistics",
        Vec::new(),
        body,
    )
    .await
}

/// The five overview figures, or the failure that stopped us reading one.
///
/// Every one of these used to fall back to `0`, and the page reads the same
/// zeroes an untouched deployment produces — down to the "Create your first
/// bucket" call to action, which an operator would follow into a deployment
/// that already has buckets. The overview is a statement about the whole
/// deployment, so a figure that could not be read fails the page rather than
/// being published as a number.
async fn load_admin_stats(ctx: &dyn Context) -> Result<AdminStats, wafer_run::WaferError> {
    let buckets = repo::buckets::count_all(ctx).await?;
    let files = repo::objects::count_completed(ctx).await?;
    let shares = repo::shares::count_all(ctx).await?;
    let quotas_count = repo::quota::count_all(ctx).await?;
    let total_size_bytes = repo::objects::sum_size_completed(ctx).await? as i64;

    Ok(AdminStats {
        buckets,
        files,
        total_size_bytes,
        shares,
        quotas_count,
    })
}

// ---------------------------------------------------------------------------
// Column shaping
//
// The admin tables are narrow, so ids are cut to a prefix: a value SHORTER
// than the cut renders the fallback rather than the value, because `str::get`
// returns `None` for an out-of-range (or non-char-boundary) index. Dates are
// carried whole and drawn by `components::timestamp`.
// ---------------------------------------------------------------------------

/// A user id cut to its first 8 bytes; an em dash when it is shorter.
fn short_id(id: &str) -> String {
    id.get(..8).unwrap_or("—").to_string()
}

// ---------------------------------------------------------------------------
// Buckets
// ---------------------------------------------------------------------------

/// A render-side projection of [`repo::buckets::BucketRow`]: the owner id
/// truncated for the table's narrow column. It holds no decoding — `public`
/// is the row's `bool`, decoded once in the repo (B13).
#[derive(Clone, Debug)]
pub struct AdminBucketRow {
    pub name: String,
    pub owner_short: String,
    pub public: bool,
    pub created_at: String,
}

impl From<&repo::buckets::BucketRow> for AdminBucketRow {
    fn from(row: &repo::buckets::BucketRow) -> Self {
        Self {
            name: row.name.clone(),
            owner_short: short_id(&row.created_by),
            public: row.public,
            created_at: row.created_at.clone(),
        }
    }
}

const ADMIN_BUCKET_COLUMNS: [TableCol<'static>; 4] = [
    TableCol::new("Name").primary(),
    TableCol::new("Owner"),
    TableCol::new("Visibility"),
    TableCol::new("Created"),
];

/// Render the admin Buckets table (or empty state).
///
/// The owner id is monospaced, so a fixed-length value has a fixed width and
/// the columns do not re-flow as it changes, and it carries
/// `data-volatile-id`: the visual-baseline suite captures this table and masks
/// the cell holding it (the bootstrap admin's id differs on every run), as it
/// masks the `<time>` the Created date is.
pub fn render_admin_buckets_table(rows: &[AdminBucketRow]) -> Markup {
    DataTable::new(&ADMIN_BUCKET_COLUMNS)
        .rows(
            rows.iter()
                .map(|r| {
                    TableRow::new(vec![
                        html! { (r.name) },
                        html! { span .font-mono .text-sm data-volatile-id { (r.owner_short) } },
                        super::pages_user::buckets::visibility_badge(r.public),
                        components::timestamp(&r.created_at),
                    ])
                })
                .collect(),
        )
        .empty(super::pages_user::buckets::no_buckets_empty_state())
        .render()
}

pub async fn buckets(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let rows: Vec<AdminBucketRow> = match repo::buckets::list_recent(ctx, 100).await {
        Ok(page) => page.rows.iter().map(AdminBucketRow::from).collect(),
        Err(e) => return error_page(ctx, msg, e, "storage admin buckets").await,
    };

    // Admin can create buckets the same way users do — the same modal, and
    // the same `files-browser.js` handler for its form. No bootstrap carrier:
    // that names a bucket to drop uploads into, and this list has none.
    let js_url = crate::blocks::files::assets::files_browser_js_url();
    let body = html! {
        (render_admin_buckets_table(&rows))
        (super::pages_user::buckets::render_new_bucket_modal())
        script src=(js_url) defer {}
    };

    files_page(
        ctx,
        msg,
        "Buckets",
        "All storage buckets",
        vec![super::pages_user::buckets::new_bucket_button()],
        body,
    )
    .await
}

// ---------------------------------------------------------------------------
// Shares
// ---------------------------------------------------------------------------

/// A render-side projection of [`repo::shares::ShareRow`]: the token and the
/// owner id cut for the admin table's narrow columns. It holds no decoding —
/// `max_access_count` is the row's already-normalised `Option`.
#[derive(Clone, Debug)]
pub struct AdminShareRow {
    pub token_short: String,
    pub bucket: String,
    pub key: String,
    pub access_count: i64,
    pub max_access_count: Option<i64>,
    pub expires_at: Option<String>,
    pub owner_short: String,
}

impl From<&repo::shares::ShareRow> for AdminShareRow {
    fn from(row: &repo::shares::ShareRow) -> Self {
        Self {
            token_short: row.token.get(..12).unwrap_or("—").to_string(),
            bucket: row.bucket.clone(),
            key: row.key.clone(),
            access_count: row.access_count,
            max_access_count: row.max_access_count,
            expires_at: row.expires_at.clone(),
            owner_short: short_id(&row.created_by),
        }
    }
}

const ADMIN_SHARE_COLUMNS: [TableCol<'static>; 5] = [
    TableCol::new("File").primary(),
    TableCol::new("Token"),
    TableCol::new("Accesses"),
    TableCol::new("Expires"),
    TableCol::new("Created by"),
];

/// Render the admin Shares table (or empty state). The token is a short
/// prefix in a `<code>`; the access count carries its cap ("4 / 10") when one
/// is set; a share with no expiry says "Never".
pub fn render_admin_shares_table(rows: &[AdminShareRow]) -> Markup {
    DataTable::new(&ADMIN_SHARE_COLUMNS)
        .rows(
            rows.iter()
                .map(|r| {
                    TableRow::new(vec![
                        html! { (components::breakable_id(&format!("{}/{}", r.bucket, r.key))) },
                        html! { code { (r.token_short) "…" } },
                        html! {
                            (r.access_count)
                            @if let Some(max) = r.max_access_count.filter(|m| *m > 0) {
                                " / " (max)
                            }
                        },
                        match &r.expires_at {
                            Some(exp) => components::timestamp(exp),
                            None => html! { "Never" },
                        },
                        html! { span .font-mono .text-sm { (r.owner_short) } },
                    ])
                })
                .collect(),
        )
        .empty_state(
            "No share links",
            "Links people create to share a file appear here.",
            None,
        )
        .render()
}

pub async fn shares(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let rows: Vec<AdminShareRow> = match repo::shares::list_recent(ctx, 100, 0).await {
        Ok(page) => page.rows.iter().map(AdminShareRow::from).collect(),
        Err(e) => return error_page(ctx, msg, e, "storage admin shares").await,
    };

    files_page(
        ctx,
        msg,
        "Shares",
        "Public file share links",
        Vec::new(),
        render_admin_shares_table(&rows),
    )
    .await
}

// ---------------------------------------------------------------------------
// Quotas
// ---------------------------------------------------------------------------

/// A render-side projection of [`repo::quota::QuotaRow`]: the user id cut
/// for the table's narrow column, and the three caps it renders. It holds no
/// decoding — the caps come off the row's `QuotaConfig`, which is where the
/// per-field fallback to the block defaults happens.
///
/// The per-file cap this table shows is the one an upload is actually refused
/// on: [`repo::quota::QuotaRow::from_record`] has already clamped a stored
/// 100 MiB to the transport's request-body ceiling, so the table cannot
/// advertise a limit nobody can use.
#[derive(Clone, Debug)]
pub struct AdminQuotaRow {
    pub user_short: String,
    pub max_storage_bytes: i64,
    pub max_file_size_bytes: i64,
    pub max_files_per_bucket: i64,
}

impl From<&repo::quota::QuotaRow> for AdminQuotaRow {
    fn from(row: &repo::quota::QuotaRow) -> Self {
        Self {
            user_short: short_id(&row.user_id),
            max_storage_bytes: row.config.max_storage_bytes,
            max_file_size_bytes: row.config.max_file_size_bytes,
            max_files_per_bucket: row.config.max_files_per_bucket,
        }
    }
}

/// The note under the quotas table explaining why a per-file cap an admin
/// types may come back smaller.
///
/// Without it the clamp is a silent edit: an admin sets 100 MB, the API and
/// the table answer 10 MB, and nothing on the page accounts for the
/// difference. Rendered whatever the table holds, because the ceiling applies
/// to the defaults too.
fn transport_cap_note() -> Markup {
    let cap = format_bytes(crate::streaming::MAX_REQUEST_BODY_BYTES as i64);
    html! {
        p .text-muted .text-sm {
            "A request body is capped at " (cap) " by the transport, so a "
            "per-file quota above that is stored as written and enforced — and "
            "shown — as " (cap) "."
        }
    }
}

const ADMIN_QUOTA_COLUMNS: [TableCol<'static>; 4] = [
    TableCol::new("User").primary(),
    TableCol::new("Max storage"),
    TableCol::new("Max file size"),
    TableCol::new("Max files per bucket"),
];

/// Render the admin Storage Quotas table (or empty state). Bytes
/// columns humanize via `format_bytes`; user_id is truncated to the
/// first 8 chars in the loader. Pure helper.
pub fn render_admin_quotas_table(rows: &[AdminQuotaRow]) -> Markup {
    // Rendered from the defaults rather than written out, and from the
    // *effective* ones: a hand-typed "100 MB file size" is how this line
    // came to advertise a per-file cap no upload could reach.
    let defaults = QuotaConfig::effective_default();
    let empty_body = format!(
        "Every user gets the defaults: {} storage, {} per file, {} files per bucket.",
        format_bytes(defaults.max_storage_bytes),
        format_bytes(defaults.max_file_size_bytes),
        crate::util::format_count(defaults.max_files_per_bucket),
    );
    html! {
        (DataTable::new(&ADMIN_QUOTA_COLUMNS)
            .rows(
                rows.iter()
                    .map(|r| {
                        TableRow::new(vec![
                            html! { span .font-mono { (r.user_short) } },
                            html! { (format_bytes(r.max_storage_bytes)) },
                            html! { (format_bytes(r.max_file_size_bytes)) },
                            html! { (crate::util::format_count(r.max_files_per_bucket)) },
                        ])
                    })
                    .collect(),
            )
            .empty_state("No custom quotas", &empty_body, None)
            .render())
        (transport_cap_note())
    }
}

pub async fn quotas(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let rows: Vec<AdminQuotaRow> = match repo::quota::list_recent(ctx, 100).await {
        Ok(page) => page.rows.iter().map(AdminQuotaRow::from).collect(),
        Err(e) => return error_page(ctx, msg, e, "storage admin quotas").await,
    };

    files_page(
        ctx,
        msg,
        "Quotas",
        "Per-user storage limits",
        Vec::new(),
        render_admin_quotas_table(&rows),
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
        assert!(html.contains("No buckets yet"), "cta title missing: {html}");
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
            html.contains("No buckets yet"),
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
            !html.contains("No buckets yet"),
            "CTA should be gone once a bucket exists: {html}"
        );
    }

    #[test]
    fn render_admin_overview_quotas_hint_when_present() {
        let html = render_admin_overview_quotas_hint(3).into_string();
        assert!(
            html.contains("3 users have a custom quota."),
            "quotas hint missing: {html}"
        );
    }

    #[test]
    fn render_admin_overview_quotas_hint_empty_when_zero() {
        let html = render_admin_overview_quotas_hint(0).into_string();
        // Empty markup: no "custom quota" copy.
        assert!(
            !html.contains("custom quota"),
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
                created_at: "2026-05-06T10:00:00Z".into(),
            },
            AdminBucketRow {
                name: "docs".into(),
                owner_short: "user_42".into(),
                public: false,
                created_at: "2026-05-05T10:00:00Z".into(),
            },
        ];
        let html = render_admin_buckets_table(&rows).into_string();
        assert!(html.contains(">photos<"), "name missing: {html}");
        assert!(html.contains(">docs<"));
        assert!(html.contains("admin_1"));
        // One visibility badge, the same as the user's bucket list draws.
        assert!(html.contains(r#"<span class="badge badge-warning">Public</span>"#));
        assert!(html.contains(r#"<span class="badge badge-secondary">Private</span>"#));
        assert!(html.contains("2026-05-06 10:00 UTC"));
    }

    #[test]
    fn render_admin_buckets_table_marks_the_volatile_cells() {
        let rows = vec![AdminBucketRow {
            name: "photos".into(),
            owner_short: "019a2b3c".into(),
            public: false,
            created_at: "2026-09-25T10:00:00Z".into(),
        }];
        let html = render_admin_buckets_table(&rows).into_string();
        // The visual-baseline suite masks the cell holding the owner id by
        // `data-volatile-id`, and it is monospaced so its width is fixed.
        assert!(
            html.contains(r#"<td data-label="Owner"><span class="font-mono text-sm" data-volatile-id>019a2b3c</span></td>"#),
            "the Owner cell must carry the volatile-id marker: {html}"
        );
        // ...and dates by the `<time>` element.
        assert!(
            html.contains(r#"<time class="datetime" datetime="2026-09-25T10:00:00.000Z""#),
            "the Created date must be a <time>: {html}"
        );
    }

    #[test]
    fn the_buckets_table_names_its_rows_and_offers_a_new_bucket_when_empty() {
        let html = render_admin_buckets_table(&[AdminBucketRow {
            name: "photos".into(),
            owner_short: "u".into(),
            public: true,
            created_at: "2026-09-25T10:00:00Z".into(),
        }])
        .into_string();
        assert!(
            html.contains(r#"<td class="data-table__cell--primary" data-label="Name">photos</td>"#),
            "the bucket name is the card title: {html}"
        );
        let empty = render_admin_buckets_table(&[]).into_string();
        assert!(
            empty.contains(r#"data-modal-target="new-bucket""#),
            "the empty list opens the new-bucket modal: {empty}"
        );
        assert!(empty.contains(r#"<h2 class="empty__title">No buckets yet</h2>"#));
    }

    /// One capped, expiring share row, as the repo decodes it.
    fn sample_share_row() -> repo::shares::ShareRow {
        repo::shares::ShareRow::from_record(&wafer_core::clients::database::Record {
            id: "s1".to_string(),
            data: [
                ("token", serde_json::json!("tok12345abcdef-more")),
                ("bucket", serde_json::json!("photos")),
                ("key", serde_json::json!("a.png")),
                ("created_by", serde_json::json!("alice-1234-5678")),
                ("created_at", serde_json::json!("2026-05-06T10:00:00Z")),
                ("expires_at", serde_json::json!("2026-06-06T10:00:00Z")),
                ("access_count", serde_json::json!(4)),
                ("max_access_count", serde_json::json!(10)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        })
    }

    /// The admin share projection cuts the token to 12, keeps the expiry
    /// whole for `components::timestamp`, and reads nothing else. `max_access_count` comes straight off the row:
    /// the inline decoder it replaces read it with `str_field(..).parse()`,
    /// which is empty for the JSON number SQLite's `INTEGER` column returns,
    /// so a capped share rendered as uncapped.
    #[test]
    fn admin_share_projection_shapes_only_what_the_table_renders() {
        let row = sample_share_row();
        let projected = AdminShareRow::from(&row);
        assert_eq!(projected.token_short, "tok12345abcd");
        assert_eq!(projected.bucket, "photos");
        assert_eq!(projected.key, "a.png");
        assert_eq!(projected.access_count, 4);
        assert_eq!(projected.max_access_count, Some(10));
        assert_eq!(
            projected.expires_at.as_deref(),
            Some("2026-06-06T10:00:00Z")
        );
        assert_eq!(projected.owner_short, "alice-12");
    }

    #[test]
    fn render_admin_shares_table_empty_state() {
        let html = render_admin_shares_table(&[]).into_string();
        assert!(
            html.contains(r#"<h2 class="empty__title">No share links</h2>"#),
            "missing empty: {html}"
        );
    }

    #[test]
    fn render_admin_shares_table_renders_rows() {
        let rows = vec![AdminShareRow {
            token_short: "tok12345abc1".into(),
            bucket: "photos".into(),
            key: "a.png".into(),
            access_count: 4,
            max_access_count: Some(10),
            expires_at: Some("2026-06-06T10:00:00Z".into()),
            owner_short: "admin_1".into(),
        }];
        let html = render_admin_shares_table(&rows).into_string();
        assert!(html.contains("tok12345abc1"));
        assert!(
            html.contains("photos/<wbr>a<wbr>.png"),
            "file missing: {html}"
        );
        // access_count and max rendered together as "4 / 10"
        assert!(
            html.contains("4 / 10"),
            "access count + max missing: {html}"
        );
        // max_access_count rendered as "/ 10"
        assert!(html.contains("/ 10"));
        assert!(html.contains("2026-06-06 10:00 UTC"));
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
            expires_at: None,
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

    /// The admin quota projection cuts the user id to 8 and reads the three
    /// caps off the row's `QuotaConfig` — which is where a column that is
    /// absent falls back to the block default — then clamps the per-file cap
    /// to the transport's request-body ceiling, so the table shows the cap
    /// that is actually enforced.
    #[test]
    fn admin_quota_projection_shapes_only_what_the_table_renders() {
        let row = repo::quota::QuotaRow::from_record(&wafer_core::clients::database::Record {
            id: "q1".to_string(),
            data: [
                ("user_id", serde_json::json!("alice-1234-5678")),
                ("max_storage_bytes", serde_json::json!("5000000000")),
                ("max_file_size_bytes", serde_json::json!(100_000_000)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        });
        let projected = AdminQuotaRow::from(&row);
        assert_eq!(projected.user_short, "alice-12");
        assert_eq!(projected.max_storage_bytes, 5_000_000_000);
        assert_eq!(
            projected.max_file_size_bytes,
            crate::streaming::MAX_REQUEST_BODY_BYTES as i64,
            "a stored 100 MB per-file cap is shown as the transport ceiling that refuses the upload, not as a limit no request can reach",
        );
        assert_eq!(
            projected.max_files_per_bucket,
            crate::blocks::files::models::QuotaConfig::DEFAULT_MAX_FILES_PER_BUCKET,
            "a column with no override renders the default the block enforces"
        );
    }

    #[test]
    fn render_admin_quotas_table_empty_state() {
        let html = render_admin_quotas_table(&[]).into_string();
        assert!(
            html.contains("No custom quotas"),
            "missing empty hint: {html}"
        );
        // Default values surfaced in the empty state copy, rendered from the
        // defaults themselves.
        assert!(
            html.contains(&format_bytes(QuotaConfig::DEFAULT_MAX_STORAGE_BYTES)),
            "missing the default storage cap: {html}"
        );
        assert!(
            html.contains(&format_bytes(
                crate::streaming::MAX_REQUEST_BODY_BYTES as i64
            )),
            "the per-file copy must name the enforced ceiling, not the stored 100 MB: {html}"
        );
        assert!(
            !html.contains(&format_bytes(QuotaConfig::DEFAULT_MAX_FILE_SIZE_BYTES)),
            "a cap no upload can reach must not be advertised: {html}"
        );
        // Rendering the default rather than the old hand-typed prose lost its
        // thousands separator: "10000 files per bucket" reads as a different
        // number at a glance.
        assert!(
            html.contains("10,000 files per bucket"),
            "the file-count default is grouped: {html}"
        );
        // And the clamp is explained rather than left as two numbers that
        // merely differ.
        assert!(
            html.contains("capped at"),
            "the transport-cap note must render with the empty state: {html}"
        );
    }

    /// The note renders alongside a populated table too — an admin who set a
    /// 100 MB per-file cap and is shown 10 MB needs the explanation most.
    #[test]
    fn render_admin_quotas_table_explains_the_transport_cap_with_rows() {
        let rows = vec![AdminQuotaRow {
            user_short: "user_1".into(),
            max_storage_bytes: 5_000_000_000,
            max_file_size_bytes: crate::streaming::MAX_REQUEST_BODY_BYTES as i64,
            max_files_per_bucket: 1000,
        }];
        let html = render_admin_quotas_table(&rows).into_string();
        assert!(
            html.contains("capped at")
                && html.contains(&format_bytes(
                    crate::streaming::MAX_REQUEST_BODY_BYTES as i64
                )),
            "the note must name the enforced ceiling: {html}"
        );
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
            html.contains(">1,000<"),
            "files-per-bucket count missing: {html}"
        );
    }
}

#[cfg(test)]
mod b13_visibility_tests {
    use serde_json::json;
    use wafer_core::clients::database::Record;

    use super::*;
    use crate::test_support::{admin_msg, output_html, TestContext};

    /// One bucket row whose `public` column arrived in `shape`.
    fn row_with_public(shape: serde_json::Value) -> repo::buckets::BucketRow {
        repo::buckets::BucketRow::from_record(&Record {
            id: "b1".to_string(),
            data: [
                ("name".to_string(), json!("photos")),
                ("public".to_string(), shape),
                ("created_by".to_string(), json!("alice-1234-5678")),
                ("created_at".to_string(), json!("2026-05-06T10:00:00Z")),
            ]
            .into_iter()
            .collect(),
        })
    }

    /// The two projections, off one repo row, for every shape `public` can
    /// arrive in — including the Postgres `Bool` and TEXT `String` shapes the
    /// SQLite-backed page tests below cannot produce. Both projections read
    /// the row's already-decoded `bool`, so there is no second place for the
    /// two pages to drift apart again.
    #[test]
    fn both_bucket_projections_agree_for_every_shape_public_arrives_in() {
        for (shape, expected) in [
            (json!(1), true),
            (json!(true), true),
            (json!("true"), true),
            (json!(0), false),
            (json!(false), false),
            (json!("false"), false),
        ] {
            let row = row_with_public(shape.clone());
            let user = super::super::pages_user::buckets::BucketRow::from((&row, 3));
            let admin = AdminBucketRow::from(&row);
            assert_eq!(
                (user.public, admin.public),
                (expected, expected),
                "`public` as {shape} projected as user={} admin={}",
                user.public,
                admin.public
            );
        }
    }

    /// The projections shape their columns and read nothing else: the user
    /// table keeps the full timestamp and carries the object count from the
    /// second query, the admin table cuts the owner to 8 and keeps the date.
    #[test]
    fn the_bucket_projections_shape_only_what_the_table_renders() {
        let row = row_with_public(json!(1));
        let user = super::super::pages_user::buckets::BucketRow::from((&row, 7));
        assert_eq!(user.name, "photos");
        assert_eq!(user.created_at, "2026-05-06T10:00:00Z");
        assert_eq!(user.object_count, 7);

        let admin = AdminBucketRow::from(&row);
        assert_eq!(admin.name, "photos");
        assert_eq!(admin.owner_short, "alice-12");
        assert_eq!(admin.created_at, "2026-05-06T10:00:00Z");
    }

    /// Does the user-facing bucket table say this bucket is public?
    /// Both tables draw `pages_user::buckets::visibility_badge`: "Public" in
    /// `badge-warning`, "Private" in the neutral `badge-secondary`.
    fn user_page_says_public(html: &str) -> bool {
        assert!(
            html.contains(">photos<"),
            "the bucket is missing from the user page entirely: {html}"
        );
        html.contains("badge-warning")
    }

    /// Does the admin bucket table say this bucket is public?
    fn admin_page_says_public(html: &str) -> bool {
        assert!(
            html.contains(">photos<"),
            "the bucket is missing from the admin page entirely: {html}"
        );
        html.contains(">Public</span>")
    }

    /// B13. `public` is written as a JSON bool by every writer
    /// ([`repo::buckets::insert`] among them) into a column that is
    /// `INTEGER` on SQLite and `BOOLEAN` on Postgres. The user bucket page
    /// decoded it with `as_bool()` and the admin bucket page with
    /// `str_field("public") == "true"`, so between them they accepted a JSON
    /// bool and a JSON string and neither accepted the integer SQLite hands
    /// back — one bucket, one row, two pages, and up to three different
    /// answers depending on the backend.
    ///
    /// The fix is to decode it exactly once, in
    /// [`repo::buckets::BucketRow::from_record`], through
    /// `RecordExt::bool_field` (which accepts all three shapes). This test
    /// therefore asserts the *correct* answer on both pages, not merely that
    /// the two agree: on SQLite today they already agree — on "Private", for
    /// a bucket that was created public.
    #[tokio::test]
    async fn both_bucket_pages_report_a_public_bucket_as_public() {
        let ctx = TestContext::with_files().await;
        // `admin_msg`'s user id, so the owner-scoped user page shows it too.
        repo::buckets::insert(&ctx, "photos", true, "admin_1")
            .await
            .expect("seed public bucket");

        let user_html = output_html(
            super::super::pages_user::buckets::bucket_list_page(
                &ctx,
                &admin_msg("retrieve", "/b/storage/"),
            )
            .await,
        )
        .await;
        let admin_html =
            output_html(buckets(&ctx, &admin_msg("retrieve", "/b/storage/admin/buckets")).await)
                .await;

        let user = user_page_says_public(&user_html);
        let admin = admin_page_says_public(&admin_html);
        assert!(
            user && admin,
            "a bucket created public must read public on both pages; \
             user page said public={user}, admin page said public={admin}"
        );
    }

    /// The other half of the same door: a private bucket must read private on
    /// both pages. Guards the fix from over-correcting into "everything is
    /// public".
    #[tokio::test]
    async fn both_bucket_pages_report_a_private_bucket_as_private() {
        let ctx = TestContext::with_files().await;
        repo::buckets::insert(&ctx, "photos", false, "admin_1")
            .await
            .expect("seed private bucket");

        let user_html = output_html(
            super::super::pages_user::buckets::bucket_list_page(
                &ctx,
                &admin_msg("retrieve", "/b/storage/"),
            )
            .await,
        )
        .await;
        let admin_html =
            output_html(buckets(&ctx, &admin_msg("retrieve", "/b/storage/admin/buckets")).await)
                .await;

        let user = user_page_says_public(&user_html);
        let admin = admin_page_says_public(&admin_html);
        assert!(
            !user && !admin,
            "a bucket created private must read private on both pages; \
             user page said public={user}, admin page said public={admin}"
        );
    }
}

#[cfg(test)]
mod outage_tests {
    //! The four storage-admin pages during a database outage.
    //!
    //! Each one used to log a warning and render its empty state: an overview
    //! of five zeroes (with the "Create your first bucket" call to action an
    //! operator would follow into a deployment that already has buckets), and
    //! three tables saying there are no buckets, no shares and no quotas.

    use super::*;
    use crate::test_support::{admin_msg, output_http_status, TestContext};

    #[tokio::test]
    async fn a_failing_stats_read_renders_the_error_page_not_five_zeroes() {
        let ctx = TestContext::with_files().await.break_reads();
        let out = overview(&ctx, &admin_msg("retrieve", "/b/storage/admin/")).await;
        assert_eq!(output_http_status(out).await, 500);
    }

    #[tokio::test]
    async fn a_failing_bucket_list_renders_the_error_page() {
        let ctx = TestContext::with_files().await.break_reads();
        let out = buckets(&ctx, &admin_msg("retrieve", "/b/storage/admin/buckets")).await;
        assert_eq!(output_http_status(out).await, 500);
    }

    #[tokio::test]
    async fn a_failing_share_list_renders_the_error_page() {
        let ctx = TestContext::with_files().await.break_reads();
        let out = shares(&ctx, &admin_msg("retrieve", "/b/storage/admin/shares")).await;
        assert_eq!(output_http_status(out).await, 500);
    }

    #[tokio::test]
    async fn a_failing_quota_list_renders_the_error_page() {
        let ctx = TestContext::with_files().await.break_reads();
        let out = quotas(&ctx, &admin_msg("retrieve", "/b/storage/admin/quotas")).await;
        assert_eq!(output_http_status(out).await, 500);
    }
}
