//! Bucket-list domain: the `/b/storage/` page showing a user's buckets
//! with live object counts, and the "+ New bucket" creation modal.

use maud::{html, Markup, PreEscaped};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::files::repo,
    ui::{
        self,
        components::{button, BtnVariant, CtrlSize},
        shell::Crumb,
        templates::{list_page, PageHeader},
    },
    util::url_path_encode,
};

/// Aggregated bucket info as shown in the user-facing table:
/// name, public flag, created-at ISO string, and live object count.
#[derive(Clone, Debug)]
pub struct BucketRow {
    pub name: String,
    pub public: bool,
    pub created_at: String,
    pub object_count: i64,
}

/// Render the bucket-list table (or empty state).
pub fn render_buckets_table(rows: &[BucketRow]) -> Markup {
    if rows.is_empty() {
        return html! {
            div .empty-state {
                p { "No buckets yet — create one to upload files." }
            }
        };
    }
    html! {
        table .data-table {
            thead { tr {
                th { "Name" }
                th { "Visibility" }
                th { "Created" }
                th { "Objects" }
            } }
            tbody {
                @for r in rows {
                    tr data-bucket=(r.name) {
                        td data-label="Name" { a href={"/b/storage/" (url_path_encode(&r.name)) "/"} { (r.name) } }
                        td data-label="Visibility" {
                            @if r.public {
                                span .badge.badge-success { "Public" }
                            } @else {
                                span .badge { "Private" }
                            }
                        }
                        td data-label="Created" { (r.created_at) }
                        td data-label="Objects" { (r.object_count) }
                    }
                }
            }
        }
    }
}

/// Render the "+ New bucket" `<dialog>` modal. The form is wired by the
/// `bucketCreateModal()` handler in `files-browser.js`: it intercepts
/// submit, POSTs to `/b/storage/api/buckets`, and on success redirects to
/// `/b/storage/{name}/`. Markup is rendered server-side so the page works
/// even before the JS bundle finishes loading (the trigger is a no-op
/// without JS — accepted v1 trade-off; the JSON API is still callable).
pub fn render_new_bucket_modal() -> Markup {
    html! {
        dialog #new-bucket-modal .modal.modal--bucket-create {
            form method="dialog" {
                h3 { "New bucket" }
                p .modal-error role="alert" hidden {}
                label {
                    span { "Name" }
                    input
                        type="text"
                        name="name"
                        required
                        minlength=(crate::blocks::files::storage::BUCKET_NAME_MIN_LEN)
                        maxlength=(crate::blocks::files::storage::BUCKET_NAME_MAX_LEN)
                        // S3-compatible: lowercase letters, digits, hyphens;
                        // start/end alnum. Single source of truth shared with
                        // the server-side `is_valid_bucket_name` check.
                        pattern=(crate::blocks::files::storage::BUCKET_NAME_PATTERN)
                        autocomplete="off"
                        spellcheck="false"
                        placeholder="my-bucket";
                }
                small .form-hint {
                    "3–63 characters. Lowercase letters, digits, and hyphens. Must start and end with a letter or digit."
                }
                label .checkbox-label {
                    input type="checkbox" name="public" value="1";
                    span { "Public (objects can be accessed by anonymous URL)" }
                }
                div .modal-actions {
                    button type="button" data-action="cancel" .btn.btn--ghost.btn--md { "Cancel" }
                    button type="submit" data-action="create" .btn.btn--primary.btn--md { "Create bucket" }
                }
            }
        }
    }
}

/// Load the calling user's buckets, decorated with live object counts.
///
/// `created_by` filtering (inside [`repo::buckets::list_owned_sorted`])
/// keeps users from seeing each other's buckets. Object counts come from
/// a single GROUP BY query on the objects table
/// ([`repo::objects::count_by_bucket`], one row per bucket) so we avoid
/// the previous N+1 count query per bucket.
pub async fn list_buckets_for_user(ctx: &dyn Context, user_id: &str) -> Vec<BucketRow> {
    use std::collections::HashMap;

    let recs = match repo::buckets::list_owned_sorted(ctx, user_id).await {
        Ok(records) => records,
        Err(e) => {
            tracing::warn!(error = %e, "files bucket list failed");
            Vec::new()
        }
    };

    // Restrict the GROUP BY to the buckets this user owns so the count
    // matches the previous per-bucket count semantics exactly (which
    // counted all objects in the bucket regardless of `uploaded_by`).
    let bucket_names: Vec<String> = recs
        .iter()
        .filter_map(|r| r.data.get("name").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect();
    let counts_by_bucket: HashMap<String, i64> =
        match repo::objects::count_by_bucket(ctx, &bucket_names).await {
            Ok(counts) => counts,
            Err(e) => {
                tracing::warn!(error = %e, "files bucket object counts failed");
                HashMap::new()
            }
        };

    let mut rows: Vec<BucketRow> = Vec::with_capacity(recs.len());
    for r in recs {
        let name = r
            .data
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let public = r
            .data
            .get("public")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let created_at = r
            .data
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let object_count = counts_by_bucket.get(&name).copied().unwrap_or(0);

        rows.push(BucketRow {
            name,
            public,
            created_at,
            object_count,
        });
    }
    rows
}

/// GET `/b/storage/` — bucket list for the calling user.
pub async fn bucket_list_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    // The handle() above us already enforces auth; this guard is defensive.
    if user_id.is_empty() {
        return ui::not_found_response(msg);
    }

    let rows = list_buckets_for_user(ctx, &user_id).await;

    let new_bucket_btn = button(
        BtnVariant::Primary,
        CtrlSize::Md,
        "+ New bucket",
        PreEscaped(r#"type="button" data-action="open-new-bucket""#.to_string()),
    );

    // The table cell carries the modal markup + JS so it lives inside the
    // shelled response without needing a new template parameter.
    let js_url = crate::ui::assets::files_browser_js_url();
    let table_with_modal = html! {
        (render_buckets_table(&rows))
        (render_new_bucket_modal())
        script src=(js_url) defer {}
    };

    let body = list_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        None,
        table_with_modal,
        None,
    );

    ui::shell_page(
        ctx,
        msg,
        ui::Shell {
            title: "Files",
            nav: ui::NavKind::Portal,
            crumbs: vec![Crumb {
                label: "Files",
                href: None,
            }],
            subtitle: Some("Your buckets and their object counts."),
            primary_action: Some(new_bucket_btn),
        },
        body,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, public: bool, count: i64) -> BucketRow {
        BucketRow {
            name: name.into(),
            public,
            created_at: "2026-05-06T10:00:00Z".into(),
            object_count: count,
        }
    }

    #[test]
    fn render_buckets_table_empty_state() {
        let html = render_buckets_table(&[]).into_string();
        assert!(
            html.contains("No buckets yet"),
            "missing empty hint: {html}"
        );
    }

    #[test]
    fn render_buckets_table_renders_rows() {
        let rows = vec![sample("photos", true, 12), sample("docs", false, 0)];
        let html = render_buckets_table(&rows).into_string();
        assert!(html.contains(">photos<"));
        assert!(html.contains(">docs<"));
        assert!(html.contains("Public"));
        assert!(html.contains("Private"));
        assert!(html.contains(">12<"));
        assert!(html.contains(r#"href="/b/storage/photos/""#));
    }

    #[test]
    fn render_buckets_table_escapes_special_chars_in_bucket_name() {
        // Maud auto-escapes both the text content and the href attribute
        // value, so a bucket name with `&` should render as `a&amp;b` in
        // both places. This guards against a future refactor that bypasses
        // maud's escaping (e.g. PreEscaped).
        let rows = vec![sample("a&b", false, 0)];
        let html = render_buckets_table(&rows).into_string();
        assert!(
            html.contains("a&amp;b"),
            "name should be HTML-escaped: {html}"
        );
        assert!(
            !html.contains(">a&b<") && !html.contains(r#"href="/b/storage/a&b/""#),
            "raw `&` leaked into HTML: {html}"
        );
    }

    #[test]
    fn render_buckets_table_url_encodes_bucket_name_in_href() {
        let rows = vec![sample("my files", false, 0)];
        let html = render_buckets_table(&rows).into_string();
        assert!(
            html.contains(r#"href="/b/storage/my%20files/""#),
            "bucket href should URL-encode space: {html}"
        );
        // Display text remains raw (HTML-escaped by maud).
        assert!(html.contains(">my files<"), "display text wrong: {html}");
    }
}

#[cfg(test)]
mod integration_tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::{super::test_helpers::seed_two_buckets, *};
    use crate::test_support::{admin_msg, output_html, TestContext};

    #[tokio::test]
    async fn bucket_list_page_renders_user_buckets() {
        let ctx = TestContext::with_files().await;
        let owner = "admin_1"; // admin_msg's default user_id
        seed_two_buckets(&ctx, owner).await;

        let msg = admin_msg("retrieve", "/b/storage/");
        let resp = bucket_list_page(&ctx, &msg).await;
        let body = output_html(resp).await;

        assert!(body.contains("Files"), "missing page header: {body}");
        assert!(body.contains(">photos<"), "missing bucket: {body}");
        assert!(body.contains(">docs<"), "missing bucket: {body}");
        assert!(
            body.contains(r#"data-label="Objects">2<"#),
            "photos should show 2 objects: {body}"
        );
        assert!(
            body.contains(r#"data-label="Objects">0<"#),
            "docs should show 0 objects: {body}"
        );
    }

    #[tokio::test]
    async fn bucket_list_page_empty_state_for_fresh_user() {
        let ctx = TestContext::with_files().await;

        let msg = admin_msg("retrieve", "/b/storage/");
        let resp = bucket_list_page(&ctx, &msg).await;
        let body = output_html(resp).await;

        assert!(body.contains("Files"), "missing page header");
        assert!(body.contains("No buckets yet"), "missing empty state");
    }

    #[tokio::test]
    async fn bucket_list_page_hides_other_users_buckets() {
        let ctx = TestContext::with_files().await;
        // Seed admin_1's buckets.
        seed_two_buckets(&ctx, "admin_1").await;
        // Seed one bucket for a different user.
        let mut row: HashMap<String, serde_json::Value> = HashMap::new();
        row.insert("name".into(), json!("secrets"));
        row.insert("created_by".into(), json!("other_user"));
        repo::buckets::seed(&ctx, row)
            .await
            .expect("seed cross-user bucket");

        let msg = admin_msg("retrieve", "/b/storage/"); // user_id = "admin_1"
        let body = output_html(bucket_list_page(&ctx, &msg).await).await;
        assert!(
            !body.contains(">secrets<"),
            "cross-user bucket leaked: {body}"
        );
    }

    #[tokio::test]
    async fn bucket_list_page_renders_new_bucket_button() {
        let ctx = TestContext::with_files().await;
        let msg = admin_msg("retrieve", "/b/storage/");
        let body = output_html(bucket_list_page(&ctx, &msg).await).await;

        // Primary-action lives in the Topbar slot now (see ui(pages) commit
        // that moved page-header content into the topbar).
        assert!(
            body.contains("topbar__action"),
            "topbar action slot missing: {body}"
        );
        assert!(
            body.contains("+ New bucket"),
            "new-bucket button label missing: {body}"
        );
        assert!(
            body.contains(r#"data-action="open-new-bucket""#),
            "new-bucket trigger attribute missing: {body}"
        );
    }

    #[tokio::test]
    async fn bucket_list_page_renders_new_bucket_modal() {
        let ctx = TestContext::with_files().await;
        let msg = admin_msg("retrieve", "/b/storage/");
        let body = output_html(bucket_list_page(&ctx, &msg).await).await;

        // Modal markup is server-rendered next to the table.
        assert!(
            body.contains(r#"id="new-bucket-modal""#),
            "modal element missing: {body}"
        );
        assert!(
            body.contains(r#"name="name""#),
            "name input missing: {body}"
        );
        assert!(
            body.contains(r#"name="public""#),
            "public toggle missing: {body}"
        );
        assert!(
            body.contains("Create bucket"),
            "submit button missing: {body}"
        );
        // JS bundle is included with the cache-busting hash URL.
        assert!(
            body.contains("/b/static/files-browser-"),
            "files-browser.js script tag missing: {body}"
        );
    }

    #[test]
    fn render_new_bucket_modal_validates_name_pattern_client_side() {
        // The pattern is used by the browser for native validation; assert
        // it's present so we don't regress accidentally to no client-side
        // validation. Server-side validation lives in storage.rs.
        let html = render_new_bucket_modal().into_string();
        assert!(
            html.contains(r#"pattern="[a-z0-9]([a-z0-9-]*[a-z0-9])?""#),
            "client-side pattern attribute missing: {html}"
        );
        assert!(
            html.contains(r#"minlength="3""#) && html.contains(r#"maxlength="63""#),
            "length constraints missing: {html}"
        );
    }
}
