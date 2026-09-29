//! Quota + share-links domain: the `/b/cloudstorage/` page showing a
//! user's active public share links alongside their storage quota card.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::files::repo,
    db_read::CappedList,
    ui::{self, shell::Crumb, templates::list_page},
};

#[derive(Clone, Debug)]
pub struct QuotaInfo {
    pub used_bytes: i64,
    pub limit_bytes: i64,
}

/// A share as the owner sees it on `/b/cloudstorage/`.
///
/// A render-side projection of [`repo::shares::ShareRow`] — the columns the
/// table renders, with no decoding of its own.
#[derive(Clone, Debug)]
pub struct ShareRow {
    /// The share row's primary key — what `DELETE /b/cloudstorage/shares/{id}`
    /// is keyed on, and therefore what the revoke button has to carry. The
    /// token is the public credential, not the resource's name.
    pub id: String,
    pub token: String,
    pub bucket: String,
    pub key: String,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub access_count: i64,
}

impl From<&repo::shares::ShareRow> for ShareRow {
    fn from(row: &repo::shares::ShareRow) -> Self {
        Self {
            id: row.id.clone(),
            token: row.token.clone(),
            bucket: row.bucket.clone(),
            key: row.key.clone(),
            created_at: row.created_at.clone(),
            expires_at: row.expires_at.clone(),
            access_count: row.access_count,
        }
    }
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
            div .quota-bar { div .quota-bar__fill style={"--fill-pct:" (pct) "%"} {} }
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
                    tr data-share-id=(r.id) {
                        td data-label="Token" { code { (r.token) } }
                        td data-label="Source" { (r.bucket) "/" (r.key) }
                        td data-label="Created" { (r.created_at) }
                        td data-label="Expires" {
                            @if let Some(exp) = &r.expires_at { (exp) } @else { "—" }
                        }
                        td data-label="Accesses" { (r.access_count) }
                        td {
                            // `data-share-id`, not the token: the revoke
                            // button's only action is
                            // `DELETE /b/cloudstorage/shares/{id}`, which is
                            // keyed on the row id. It also doubles as the
                            // marker that tells `files-browser.js`'s kebab
                            // this is the shares table.
                            button .kebab-trigger
                                type="button"
                                data-action-menu
                                data-share-id=(r.id)
                                aria-label={"Actions for share " (r.token)}
                            { "⋯" }
                        }
                    }
                }
            }
        }
    }
}

/// The user's share links, or the failure that stopped us reading them.
///
/// The quota card on the same page already refuses to render a figure it
/// could not read; an empty share table is the same lie in table form ("you
/// have shared nothing"), so it fails the page the same way.
async fn list_shares_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<CappedList<ShareRow>, wafer_run::WaferError> {
    Ok(repo::shares::list_all_for_user(ctx, user_id)
        .await?
        .map(|row| ShareRow::from(&row)))
}

/// GET `/b/cloudstorage/` — share list with quota card.
pub async fn cloudstorage_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return crate::ui::not_found_response(msg);
    }

    let shares = match list_shares_for_user(ctx, &user_id).await {
        Ok(rows) => rows,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "cloud storage page: share list")
        }
    };
    // Same quota source as upload enforcement (`repo::objects::reserve_upload`
    // sums the same rows, and caps them at the same `max_storage_bytes`), so
    // the card can never disagree with what the API enforces.
    //
    // A quota card showing "0 B used" during an outage misleads; the page
    // fails like the API does, and on the first read that fails.
    let used_bytes = match crate::blocks::files::quota::get_used_bytes(ctx, &user_id).await {
        Ok(used_bytes) => used_bytes,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "cloud storage page: usage lookup")
        }
    };
    let limit = match crate::blocks::files::quota::get_user_quota(ctx, &user_id).await {
        Ok(limit) => limit,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "cloud storage page: quota lookup")
        }
    };
    let quota = QuotaInfo {
        used_bytes,
        limit_bytes: limit.max_storage_bytes,
    };

    let shares_with_js = html! {
        @if shares.truncated {
            p .text-muted .text-sm { "Showing the first " (shares.rows.len()) " share links." }
        }
        (render_shares_table(&shares.rows))
        (super::render_bootstrap_script("", ""))
    };

    let body = list_page(Some(render_quota_card(&quota)), shares_with_js, None);

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

    /// The user share projection renders the full token and the full
    /// timestamps — nothing is cut on this page — and reads no field itself.
    #[test]
    fn user_share_projection_carries_the_row_through_uncut() {
        let row = repo::shares::ShareRow::from_record(&wafer_core::clients::database::Record {
            id: "s1".to_string(),
            data: [
                ("token", serde_json::json!("tok12345abcdef-more")),
                ("bucket", serde_json::json!("photos")),
                ("key", serde_json::json!("a.png")),
                ("created_at", serde_json::json!("2026-05-06T10:00:00Z")),
                ("expires_at", serde_json::json!("2026-06-06T10:00:00Z")),
                ("access_count", serde_json::json!("4")),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        });
        let projected = ShareRow::from(&row);
        assert_eq!(projected.token, "tok12345abcdef-more");
        assert_eq!(projected.bucket, "photos");
        assert_eq!(projected.key, "a.png");
        assert_eq!(projected.created_at, "2026-05-06T10:00:00Z");
        assert_eq!(
            projected.expires_at.as_deref(),
            Some("2026-06-06T10:00:00Z")
        );
        assert_eq!(projected.access_count, 4);
    }

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
            id: "s1".into(),
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

    /// The revoke button must carry what the revoke route is keyed on.
    ///
    /// Nothing here is re-typed from either side: the attribute comes from
    /// the `dataset` key `files-browser.js` reads, its value is taken out of
    /// the rendered page, and the URL is the one the bundle builds from it.
    /// That value then goes through the block's real route table into the
    /// real handler — so a page rendering the share TOKEN where the route
    /// wants the row id fails here, as a revoke that 404s in the browser.
    #[tokio::test]
    async fn the_revoke_button_carries_what_the_delete_route_is_keyed_on() {
        use crate::{
            blocks::files::{
                cloud,
                test_support::{revoke_id_attribute, revoke_url, routed},
            },
            test_support::{auth_msg, output_json},
        };

        let ctx = TestContext::with_files().await;
        let mut share: HashMap<String, serde_json::Value> = HashMap::new();
        share.insert("token".into(), json!("tok123abc"));
        share.insert("bucket".into(), json!("photos"));
        share.insert("key".into(), json!("a.png"));
        share.insert("created_by".into(), json!("admin_1"));
        let seeded = repo::shares::seed(&ctx, share).await.expect("seed share");

        let body =
            output_html(cloudstorage_page(&ctx, &admin_msg("retrieve", "/b/cloudstorage/")).await)
                .await;

        // The value the kebab hands `revokeShare`, read off the page.
        let attr = format!("{}=\"", revoke_id_attribute());
        let at = body.find(&attr).unwrap_or_else(|| {
            panic!("the shares table renders no `{attr}` for the kebab to read: {body}")
        }) + attr.len();
        let revoke_key = &body[at..at + body[at..].find('"').expect("attribute value ends")];

        let msg = routed(auth_msg("delete", &revoke_url(revoke_key), "admin_1"));
        let out = cloud::handle_delete_share(&ctx, &msg).await;

        assert_eq!(
            output_json(out).await["deleted"],
            json!(true),
            "the value the revoke button carries must address the share on the delete route"
        );
        assert!(
            repo::shares::find_by_id(&ctx, &seeded.id).await.is_err(),
            "the share must be gone after the button's request"
        );
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

#[cfg(test)]
mod outage_tests {
    //! A share listing that FAILED is not "no share links".
    //!
    //! The quota card beside it already fails the page (Phase 2); the share
    //! table did not, so an outage rendered a page that said the user had
    //! shared nothing.

    use super::*;
    use crate::{
        blocks::files::repo,
        test_support::{admin_msg, output_http_status, FailingDbOpContext, TestContext},
    };

    #[tokio::test]
    async fn a_failing_share_list_renders_the_error_page_not_an_empty_one() {
        let ctx = TestContext::with_files().await;
        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.list", repo::shares::TABLE)]);

        let out = cloudstorage_page(&failing, &admin_msg("retrieve", "/b/cloudstorage/")).await;
        assert_eq!(
            output_http_status(out).await,
            500,
            "an unreadable share list must not render as no shares"
        );
    }
}
