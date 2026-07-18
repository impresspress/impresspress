//! Quota + share-links domain: the `/b/cloudstorage/` page showing a
//! user's active public share links alongside their storage quota card.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::files::repo,
    ui::{
        self,
        shell::Crumb,
        templates::{list_page, PageHeader},
    },
    util::RecordExt,
};

#[derive(Clone, Debug)]
pub struct QuotaInfo {
    pub used_bytes: i64,
    pub limit_bytes: i64,
}

#[derive(Clone, Debug)]
pub struct ShareRow {
    pub token: String,
    pub bucket: String,
    pub key: String,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub access_count: i64,
}

fn quota_pct(used: i64, limit: i64) -> i64 {
    if limit <= 0 {
        return 0;
    }
    ((used.max(0) as f64 / limit as f64) * 100.0).round() as i64
}

pub fn render_quota_card(q: &QuotaInfo) -> Markup {
    let pct = quota_pct(q.used_bytes, q.limit_bytes);
    let warn = pct >= 90;
    html! {
        div class={ "quota-card" @if warn { " quota-warning" } } {
            h3 { "Storage quota" }
            p {
                (q.used_bytes) " / " (q.limit_bytes) " bytes"
                " · " (pct) "%"
            }
            div .quota-bar { div .quota-bar__fill style={"width: " (pct) "%"} {} }
        }
    }
}

pub fn render_shares_table(rows: &[ShareRow]) -> Markup {
    if rows.is_empty() {
        return html! {
            div .empty-state { p { "No active shares yet." } }
        };
    }
    html! {
        table .data-table {
            thead { tr {
                th { "Token" }
                th { "Source" }
                th { "Created" }
                th { "Expires" }
                th { "Accesses" }
                th {}
            } }
            tbody {
                @for r in rows {
                    tr data-share-token=(r.token) {
                        td data-label="Token" { code { (r.token) } }
                        td data-label="Source" { (r.bucket) "/" (r.key) }
                        td data-label="Created" { (r.created_at) }
                        td data-label="Expires" {
                            @if let Some(exp) = &r.expires_at { (exp) } @else { "—" }
                        }
                        td data-label="Accesses" { (r.access_count) }
                        td {
                            button .kebab-trigger
                                type="button"
                                data-action-menu
                                data-token=(r.token)
                                aria-label={"Actions for share " (r.token)}
                            { "⋯" }
                        }
                    }
                }
            }
        }
    }
}

async fn list_shares_for_user(ctx: &dyn Context, user_id: &str) -> Vec<ShareRow> {
    match repo::shares::list_all_for_user(ctx, user_id).await {
        Ok(records) => records
            .into_iter()
            .map(|r| ShareRow {
                token: r
                    .data
                    .get("token")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                bucket: r
                    .data
                    .get("bucket")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                key: r
                    .data
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                created_at: r
                    .data
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                expires_at: r
                    .data
                    .get("expires_at")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                access_count: r.i64_field("access_count"),
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "shares list failed");
            Vec::new()
        }
    }
}

/// GET `/b/cloudstorage/` — share list with quota card.
pub async fn cloudstorage_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return crate::ui::not_found_response(msg);
    }

    let shares = list_shares_for_user(ctx, &user_id).await;
    // Same quota source as upload enforcement (`quota::check_quota`), so
    // the card can never disagree with what the API enforces.
    let quota = QuotaInfo {
        used_bytes: crate::blocks::files::quota::get_used_bytes(ctx, &user_id).await,
        limit_bytes: crate::blocks::files::quota::get_user_quota(ctx, &user_id)
            .await
            .max_storage_bytes,
    };

    let shares_with_js = html! {
        (render_shares_table(&shares))
        (super::render_bootstrap_script("", ""))
    };

    let body = list_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        Some(render_quota_card(&quota)),
        shares_with_js,
        None,
    );

    ui::shell_page(
        ctx,
        msg,
        ui::Shell {
            title: "Shares",
            nav: ui::NavKind::Portal,
            crumbs: vec![Crumb {
                label: "Shares",
                href: None,
            }],
            subtitle: Some("Public links you've created and your storage quota."),
            primary_action: None,
        },
        body,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_quota_card_under_quota() {
        let q = QuotaInfo {
            used_bytes: 100_000,
            limit_bytes: 1_000_000,
        };
        let html = render_quota_card(&q).into_string();
        assert!(html.contains("100"), "used count missing");
        assert!(
            html.contains("10%") || html.contains("10 %"),
            "percent missing"
        );
        assert!(
            !html.contains("quota-warning"),
            "should not be warning class"
        );
    }

    #[test]
    fn render_quota_card_near_quota() {
        let q = QuotaInfo {
            used_bytes: 950_000,
            limit_bytes: 1_000_000,
        };
        let html = render_quota_card(&q).into_string();
        assert!(
            html.contains("quota-warning"),
            "should mark near-quota: {html}"
        );
    }

    #[test]
    fn render_shares_table_empty() {
        let html = render_shares_table(&[]).into_string();
        assert!(html.contains("No active shares"));
    }

    #[test]
    fn render_shares_table_with_rows() {
        let rows = vec![ShareRow {
            token: "abc12345".into(),
            bucket: "photos".into(),
            key: "a.png".into(),
            created_at: "2026-05-06T10:00:00Z".into(),
            expires_at: Some("2026-06-06T10:00:00Z".into()),
            access_count: 4,
        }];
        let html = render_shares_table(&rows).into_string();
        assert!(html.contains("abc12345"));
        assert!(html.contains("photos"));
        assert!(html.contains("a.png"));
        assert!(html.contains(">4<"), "access count missing");
    }
}

#[cfg(test)]
mod integration_tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::test_support::{admin_msg, output_html, TestContext};

    #[tokio::test]
    async fn cloudstorage_page_renders_shares_and_quota() {
        let ctx = TestContext::with_files().await;

        // Seed a share + a quota row owned by admin_1.
        let mut share: HashMap<String, serde_json::Value> = HashMap::new();
        share.insert("token".into(), json!("tok123abc"));
        share.insert("bucket".into(), json!("photos"));
        share.insert("key".into(), json!("a.png"));
        share.insert("created_by".into(), json!("admin_1"));
        share.insert("access_count".into(), json!(2));
        repo::shares::seed(&ctx, share).await.expect("seed share");

        let mut quota: HashMap<String, serde_json::Value> = HashMap::new();
        quota.insert("user_id".into(), json!("admin_1"));
        quota.insert("max_storage_bytes".into(), json!(1_073_741_824i64));
        repo::quota::seed(&ctx, quota).await.expect("seed quota");

        let msg = admin_msg("retrieve", "/b/cloudstorage/");
        let resp = cloudstorage_page(&ctx, &msg).await;
        let body = output_html(resp).await;

        assert!(body.contains("Shares"));
        assert!(body.contains("tok123abc"));
        assert!(body.contains("photos"));
        assert!(body.contains("a.png"));
        assert!(body.contains(">2<"), "access count cell: {body}");
    }

    /// Regression: the quota card used to be fed by a page-local
    /// `load_quota_info` copy of the quota logic. It now reads the same
    /// `quota::get_user_quota` + `quota::get_used_bytes` the upload
    /// enforcement uses, so an override row must show up on the page.
    #[tokio::test]
    async fn cloudstorage_page_quota_card_reflects_override_and_usage() {
        let ctx = TestContext::with_files().await;

        let mut quota: HashMap<String, serde_json::Value> = HashMap::new();
        quota.insert("user_id".into(), json!("admin_1"));
        quota.insert("max_storage_bytes".into(), json!(2048));
        repo::quota::seed(&ctx, quota).await.expect("seed quota");

        let mut obj: HashMap<String, serde_json::Value> = HashMap::new();
        obj.insert("bucket".into(), json!("photos"));
        obj.insert("key".into(), json!("a.png"));
        obj.insert("size".into(), json!(1024));
        obj.insert("uploaded_by".into(), json!("admin_1"));
        repo::objects::seed(&ctx, obj).await.expect("seed obj");

        let msg = admin_msg("retrieve", "/b/cloudstorage/");
        let body = output_html(cloudstorage_page(&ctx, &msg).await).await;
        assert!(
            body.contains("1024 / 2048 bytes"),
            "quota card must show summed usage against the override limit: {body}"
        );
    }

    #[tokio::test]
    async fn cloudstorage_page_hides_other_users_shares() {
        let ctx = TestContext::with_files().await;
        // Seed admin_1's share.
        let mut mine: HashMap<String, serde_json::Value> = HashMap::new();
        mine.insert("token".into(), json!("mine"));
        mine.insert("bucket".into(), json!("photos"));
        mine.insert("key".into(), json!("a.png"));
        mine.insert("created_by".into(), json!("admin_1"));
        repo::shares::seed(&ctx, mine).await.expect("seed mine");
        // Seed another user's share.
        let mut theirs: HashMap<String, serde_json::Value> = HashMap::new();
        theirs.insert("token".into(), json!("theirs"));
        theirs.insert("bucket".into(), json!("secrets"));
        theirs.insert("key".into(), json!("k"));
        theirs.insert("created_by".into(), json!("other_user"));
        repo::shares::seed(&ctx, theirs).await.expect("seed theirs");

        let msg = admin_msg("retrieve", "/b/cloudstorage/");
        let body = output_html(cloudstorage_page(&ctx, &msg).await).await;
        assert!(body.contains("mine"), "own share missing: {body}");
        assert!(!body.contains("theirs"), "other-user share leaked: {body}");
    }

    #[tokio::test]
    async fn cloudstorage_page_includes_files_browser_js() {
        let ctx = TestContext::with_files().await;

        let msg = admin_msg("retrieve", "/b/cloudstorage/");
        let resp = cloudstorage_page(&ctx, &msg).await;
        let body = output_html(resp).await;

        assert!(
            body.contains(r#"id="files-browser-bootstrap""#),
            "bootstrap carrier missing: {body}"
        );
        assert!(
            body.contains("/b/static/files-browser-"),
            "files-browser.js script tag missing: {body}"
        );
    }
}
