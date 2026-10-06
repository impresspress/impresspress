//! Quota + share-links domain: the `/b/cloudstorage/` page showing a
//! user's active public share links alongside their storage quota card.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::files::repo,
    db_read::CappedList,
    ui::{
        self,
        components::{self, DataTable, TableCol, TableRow},
        icons,
    },
    util::format_bytes,
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

/// The share of `limit` that `used` is, in whole percent (0 when there is no
/// limit to measure against).
fn quota_pct(used: i64, limit: i64) -> i64 {
    if limit <= 0 {
        return 0;
    }
    ((used.max(0) as f64 / limit as f64) * 100.0).round() as i64
}

/// The percentage as it reads: a usage that rounds to 0% but is not nothing
/// says "<1%", so a few files never read as an empty account.
fn quota_pct_text(used: i64, limit: i64) -> String {
    match quota_pct(used, limit) {
        0 if used > 0 && limit > 0 => "<1%".to_string(),
        pct => format!("{pct}%"),
    }
}

/// The storage quota: "8.3 KB of 1.0 GB used · <1%" over a meter. The meter
/// is `role="meter"` with its value in bytes and the same sentence as its
/// value text, so a screen reader hears what the eye reads; any use at all
/// fills a visible sliver (the stylesheet's minimum width), and at 90% or
/// more the bar and the text turn to the warning colour and say so.
pub fn render_quota_card(q: &QuotaInfo) -> Markup {
    let pct = quota_pct(q.used_bytes, q.limit_bytes);
    let warn = pct >= 90;
    let used = format_bytes(q.used_bytes.max(0));
    let limit = format_bytes(q.limit_bytes);
    let pct_text = quota_pct_text(q.used_bytes, q.limit_bytes);
    let summary = format!("{used} of {limit} used · {pct_text}");
    html! {
        section {
            (components::section_header("Storage", None))
            p .quota__summary {
                (summary)
                @if warn { span .quota__warning { " · Almost full" } }
            }
            div class={ "quota-bar" @if warn { " quota-bar--warning" } }
                role="meter"
                aria-label="Storage used"
                aria-valuemin="0"
                aria-valuemax=(q.limit_bytes.max(0))
                aria-valuenow=(q.used_bytes.clamp(0, q.limit_bytes.max(0)))
                aria-valuetext=(summary)
            {
                @if q.used_bytes > 0 {
                    div .quota-bar__fill style={"--fill-pct:" (pct.min(100)) "%"} {}
                }
            }
        }
    }
}

const SHARE_COLUMNS: [TableCol<'static>; 6] = [
    TableCol::new("File").primary(),
    TableCol::new("Token"),
    TableCol::new("Created"),
    TableCol::new("Expires"),
    TableCol::new("Accesses"),
    TableCol::new("Actions").actions(),
];

pub fn render_shares_table(rows: &[ShareRow]) -> Markup {
    DataTable::new(&SHARE_COLUMNS)
        .rows(
            rows.iter()
                .map(|r| {
                    let file = format!("{}/{}", r.bucket, r.key);
                    TableRow::new(vec![
                        html! { (components::breakable_id(&file)) },
                        html! { code .share-token { (r.token) } },
                        components::timestamp(&r.created_at),
                        match &r.expires_at {
                            Some(exp) => components::timestamp(exp),
                            None => html! { "Never" },
                        },
                        html! { (r.access_count) },
                        // `data-share-id`, not the token: the button's only
                        // action is `DELETE /b/cloudstorage/shares/{id}`,
                        // which is keyed on the row id.
                        html! {
                            button .btn .btn--ghost-danger .btn--icon
                                type="button"
                                data-action="revoke-share"
                                data-share-id=(r.id)
                                data-file=(file)
                                aria-label={"Revoke the share link for " (file)}
                            { (icons::trash()) }
                        },
                    ])
                })
                .collect(),
        )
        .empty(components::empty_state(
            icons::link(),
            "No share links yet",
            "Share a file from its menu in Files to create a link.",
            Some(html! { a .btn .btn--secondary .btn--md href="/b/storage/" { "Go to Files" } }),
        ))
        .render()
}

/// The dialog that confirms revoking a share link: "Revoke the link to
/// photos/a.png? Anyone who has it loses access." (`files-browser.js` asks
/// the question).
fn render_revoke_confirm_modal() -> Markup {
    super::render_confirm_modal("revoke-confirm", "Revoke share link", "Revoke")
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

    let body = html! {
        div .page-sections {
            (render_quota_card(&quota))
            // `#share-listing` is what `files-browser.js` re-fetches and swaps
            // after a revoke, so the page shows the links that remain without
            // a reload wiping the outcome it reports.
            section #share-listing {
                (components::section_header("Share links", None))
                @if shares.truncated {
                    p .text-muted .text-sm { "Showing the first " (shares.rows.len()) " share links." }
                }
                (render_shares_table(&shares.rows))
            }
        }
        (render_revoke_confirm_modal())
        (super::render_bootstrap_script("", ""))
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::portal("Shares", "Shares")
            .subtitle("Public links you've created and your storage quota."),
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
        assert!(
            html.contains("97.7 KB of 976.6 KB used · 10%"),
            "the usage reads in bytes units: {html}"
        );
        assert!(
            html.contains(r#"role="meter""#),
            "the bar is a meter: {html}"
        );
        assert!(
            html.contains(r#"aria-valuenow="100000""#)
                && html.contains(r#"aria-valuemax="1000000""#),
            "the meter's value is in bytes: {html}"
        );
        assert!(
            !html.contains("quota-bar--warning"),
            "should not be warning class"
        );
    }

    /// A few kilobytes against a gigabyte round to 0%; the card says "<1%"
    /// and still draws the fill (the stylesheet gives it a minimum width), so
    /// an account with files never reads as empty.
    #[test]
    fn render_quota_card_shows_a_little_use_as_some() {
        let q = QuotaInfo {
            used_bytes: 8_460,
            limit_bytes: 1_073_741_824,
        };
        let html = render_quota_card(&q).into_string();
        assert!(html.contains("8.3 KB of 1.0 GB used · &lt;1%"), "{html}");
        assert!(html.contains("quota-bar__fill"), "{html}");
        assert!(
            !html.contains(" bytes"),
            "no raw byte counts on the page: {html}"
        );
        assert!(
            html.contains(r#"<h2 class="section-header__title">Storage</h2>"#),
            "the section is headed by the shared h2: {html}"
        );
    }

    /// Revoking asks through the block's one confirm dialog, Cancel focused.
    #[test]
    fn revoking_confirms_in_the_shared_dialog() {
        let html = render_revoke_confirm_modal().into_string();
        assert!(
            html.contains(r#"<dialog class="modal" id="revoke-confirm""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<p id="revoke-confirm-question"></p>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"data-action="modal-close" autofocus>Cancel</button>"#),
            "{html}"
        );
        assert!(html.contains(r#"data-confirm>Revoke</button>"#), "{html}");
    }

    #[test]
    fn render_quota_card_with_nothing_used_draws_no_fill() {
        let q = QuotaInfo {
            used_bytes: 0,
            limit_bytes: 1_073_741_824,
        };
        let html = render_quota_card(&q).into_string();
        assert!(html.contains("0 B of 1.0 GB used · 0%"), "{html}");
        assert!(!html.contains("quota-bar__fill"), "{html}");
    }

    #[test]
    fn render_quota_card_near_quota() {
        let q = QuotaInfo {
            used_bytes: 950_000,
            limit_bytes: 1_000_000,
        };
        let html = render_quota_card(&q).into_string();
        assert!(
            html.contains("quota-bar--warning") && html.contains("Almost full"),
            "should mark near-quota in colour and in words: {html}"
        );
    }

    #[test]
    fn render_shares_table_empty() {
        let html = render_shares_table(&[]).into_string();
        assert!(html.contains(r#"<h2 class="empty__title">No share links yet</h2>"#));
        assert!(
            html.contains(r#"href="/b/storage/""#),
            "the way to share a file: {html}"
        );
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
        assert!(html.contains("photos/<wbr>a<wbr>.png"));
        assert!(html.contains(">4<"), "access count missing");
        assert!(
            html.contains(r#"aria-label="Revoke the share link for photos/a.png""#),
            "the revoke button is named after its file: {html}"
        );
        assert!(
            html.contains("2026-06-06 10:00 UTC"),
            "expiry via timestamp: {html}"
        );
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
            body.contains("1.0 KB of 2.0 KB used · 50%"),
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

        // The value the revoke button hands `revokeShare`, read off the page.
        let attr = format!("{}=\"", revoke_id_attribute());
        let at = body.find(&attr).unwrap_or_else(|| {
            panic!("the shares table renders no `{attr}` for the revoke button to read: {body}")
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
