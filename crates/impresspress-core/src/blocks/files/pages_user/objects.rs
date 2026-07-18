//! Object/folder browsing domain: `/b/storage/{bucket}/[{prefix}/]` — lists
//! objects and synthesizes folder navigation from key prefixes.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::files::repo,
    ui::{
        self,
        shell::Crumb,
        templates::{list_page, PageHeader},
    },
    util::{format_bytes, format_timestamp, url_path_encode, RecordExt},
};

/// Object as the user sees it (key, size, modified timestamp).
#[derive(Clone, Debug)]
pub struct ObjectRow {
    pub key: String,
    pub size: i64,
    pub modified: String,
}

/// Result of grouping a flat object list by a current-prefix folder view.
pub struct FolderListing<'a> {
    pub folders: Vec<String>,
    pub files: Vec<&'a ObjectRow>,
}

/// Synthesize a folder/file split for the rows whose key starts with
/// `current_prefix`. Folder names are deduped while preserving first-seen
/// order. Files are objects with no further `/` after `current_prefix`.
///
/// Pure function; safe to unit-test without `Context`.
pub fn group_objects_by_prefix<'a>(
    objs: &'a [ObjectRow],
    current_prefix: &str,
) -> FolderListing<'a> {
    let mut folders: Vec<String> = Vec::new();
    let mut files: Vec<&ObjectRow> = Vec::new();

    for obj in objs {
        let Some(rest) = obj.key.strip_prefix(current_prefix) else {
            continue;
        };
        match rest.find('/') {
            Some(idx) => {
                let folder = &rest[..idx];
                if !folder.is_empty() && !folders.iter().any(|f| f == folder) {
                    folders.push(folder.to_string());
                }
            }
            None => {
                if !rest.is_empty() {
                    files.push(obj);
                }
            }
        }
    }

    FolderListing { folders, files }
}

/// URL-encode a prefix (folder path) by splitting on '/', encoding each segment,
/// and rejoining with '/'. Preserves the trailing slash if present.
fn url_encode_prefix(prefix: &str) -> String {
    if prefix.is_empty() {
        return String::new();
    }
    // Split on '/', encode each segment, rejoin.
    let trimmed = prefix.trim_end_matches('/');
    let parts: Vec<String> = trimmed.split('/').map(url_path_encode).collect();
    if parts.is_empty() {
        return String::new();
    }
    parts.join("/") + "/"
}

/// Folder/file table for `/b/storage/{bucket}/...` views.
///
/// Folder rows link into `/b/storage/{bucket}/{prefix}{folder}/`.
/// File rows show the filename portion (after the `current_prefix`),
/// link to the download route, and carry a `data-action-menu` kebab
/// trigger that the JS asset wires up to Share / Delete / Copy-link.
pub fn render_objects_table(
    bucket: &str,
    current_prefix: &str,
    listing: &FolderListing<'_>,
) -> Markup {
    if listing.folders.is_empty() && listing.files.is_empty() {
        return html! {
            div .empty-state {
                p { "This folder is empty — drag files here to upload." }
            }
        };
    }

    html! {
        table .data-table {
            thead { tr {
                th { input type="checkbox" .bulk-select-all data-bulk-toggle; }
                th { "Name" }
                th { "Size" }
                th { "Modified" }
                th {} // kebab column
            } }
            tbody {
                @for folder in &listing.folders {
                    tr .row--folder {
                        td {} // bulk-select disabled on folders
                        td data-label="Name" {
                            a href={"/b/storage/" (url_path_encode(bucket)) "/" (url_encode_prefix(current_prefix)) (url_path_encode(folder)) "/"} {
                                "📁 " (folder)
                            }
                        }
                        td data-label="Size" { "—" }
                        td data-label="Modified" { "—" }
                        td {}
                    }
                }
                @for f in &listing.files {
                    @let filename = f.key.strip_prefix(current_prefix).unwrap_or(&f.key);
                    @let download_href = format!(
                        "/b/storage/api/buckets/{}/objects/{}",
                        url_path_encode(bucket),
                        f.key.split('/').map(url_path_encode).collect::<Vec<_>>().join("/"),
                    );
                    tr data-object-key=(f.key) {
                        td { input type="checkbox" .bulk-select data-key=(f.key); }
                        td data-label="Name" {
                            a href=(download_href) { (filename) }
                        }
                        td data-label="Size" { (format_bytes(f.size)) }
                        // Wrap the timestamp in <time> so the visual-baseline
                        // mask `[data-relative-time], .relative-time, time`
                        // catches it. The visible text is humanized to
                        // minute precision; the `datetime` attr keeps the
                        // full raw timestamp as the machine-readable form.
                        td data-label="Modified" { time datetime=(f.modified) { (format_timestamp(&f.modified)) } }
                        td {
                            button .kebab-trigger
                                type="button"
                                data-action-menu
                                data-bucket=(bucket)
                                data-key=(f.key)
                                aria-label={"Actions for " (filename)}
                            { "⋯" }
                        }
                    }
                }
            }
        }
    }
}

/// Render breadcrumb crumbs for the page body (below the topbar).
///
/// This is distinct from the shell `Topbar { crumbs: vec![Crumb {...}] }`
/// system: the topbar shows the page-level chrome ("Files > {bucket}"),
/// and this in-body breadcrumb shows the current folder path within the
/// bucket. The bucket and each prefix segment except the last are
/// clickable; the last segment is plain text. Returned `Markup` is a
/// `<nav class="breadcrumbs">` block.
pub fn render_breadcrumbs(bucket: &str, current_prefix: &str) -> Markup {
    let segments: Vec<&str> = current_prefix
        .trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let last_idx = segments.len();
    let encoded_bucket = url_path_encode(bucket);

    html! {
        nav .breadcrumbs aria-label="Folder" {
            a href="/b/storage/" { "Files" }
            span .breadcrumbs__sep { " / " }
            @if segments.is_empty() {
                span { (bucket) }
            } @else {
                a href={"/b/storage/" (encoded_bucket) "/"} { (bucket) }
                @for (i, seg) in segments.iter().enumerate() {
                    span .breadcrumbs__sep { " / " }
                    @if i + 1 == last_idx {
                        span { (seg) }
                    } @else {
                        @let cumulative: String = segments[..=i].iter().map(|s| url_path_encode(s)).collect::<Vec<_>>().join("/");
                        a href={"/b/storage/" (encoded_bucket) "/" (cumulative) "/"} { (seg) }
                    }
                }
            }
        }
    }
}

async fn list_objects_in_bucket(ctx: &dyn Context, bucket: &str) -> Vec<ObjectRow> {
    match repo::objects::list_for_bucket(ctx, bucket, 1000).await {
        Ok(rl) => rl
            .records
            .into_iter()
            .map(|r| ObjectRow {
                key: r
                    .data
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                size: r.i64_field("size"),
                modified: r
                    .data
                    .get("uploaded_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, bucket = %bucket, "object list failed");
            Vec::new()
        }
    }
}

/// GET `/b/storage/{bucket}/[{prefix}/]` — object listing with synthesized
/// folder navigation. 404s if the bucket doesn't exist for this user
/// (cross-user isolation enforced by the `created_by` filter on lookup).
pub async fn object_list_page(
    ctx: &dyn Context,
    msg: &Message,
    bucket: &str,
    current_prefix: &str,
) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return crate::ui::not_found_response(msg);
    }
    // SSR portal is strictly owner-scoped (no admin bypass) — see the
    // `bucket_owned_by` doc comment for the admin-policy split vs the JSON API.
    if !crate::blocks::files::storage::bucket_owned_by(ctx, &user_id, bucket).await {
        return crate::ui::not_found_response(msg);
    }

    let all_objects = list_objects_in_bucket(ctx, bucket).await;
    let listing = group_objects_by_prefix(&all_objects, current_prefix);

    let title = if current_prefix.is_empty() {
        bucket.to_string()
    } else {
        format!("{bucket} / {}", current_prefix.trim_end_matches('/'))
    };

    let table = render_objects_table(bucket, current_prefix, &listing);
    let table_with_js = html! {
        // Hidden file input that the topbar Upload button triggers via
        // [data-action="open-upload"]. Multi-select so users can pick
        // many files at once. Same upload endpoint as drag-drop.
        input #file-upload-input type="file" multiple style="display: none";
        (table)
        (super::render_bootstrap_script(bucket, current_prefix))
    };

    let body = list_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        Some(render_breadcrumbs(bucket, current_prefix)),
        table_with_js,
        None,
    );

    let upload_btn = crate::ui::components::button(
        crate::ui::components::BtnVariant::Primary,
        crate::ui::components::CtrlSize::Sm,
        "+ Upload",
        maud::PreEscaped(r#"type="button" data-action="open-upload""#.to_string()),
    );
    ui::shell_page(
        ctx,
        msg,
        ui::Shell {
            title: &title,
            nav: ui::NavKind::Portal,
            crumbs: vec![
                Crumb {
                    label: "Files",
                    href: Some("/b/storage/"),
                },
                Crumb {
                    label: bucket,
                    href: None,
                },
            ],
            subtitle: Some("Drag files here to upload, or use the Upload button."),
            primary_action: Some(upload_btn),
        },
        body,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_objects_by_prefix_empty() {
        let g = group_objects_by_prefix(&[], "");
        assert!(g.folders.is_empty());
        assert!(g.files.is_empty());
    }

    #[test]
    fn group_objects_by_prefix_root_files_only() {
        let objs = vec![
            ObjectRow {
                key: "a.png".into(),
                size: 1,
                modified: "2026-05-06T10:00:00Z".into(),
            },
            ObjectRow {
                key: "b.txt".into(),
                size: 2,
                modified: "2026-05-06T11:00:00Z".into(),
            },
        ];
        let g = group_objects_by_prefix(&objs, "");
        assert!(g.folders.is_empty());
        assert_eq!(g.files.len(), 2);
        assert_eq!(g.files[0].key, "a.png");
    }

    #[test]
    fn group_objects_by_prefix_synthesizes_folder() {
        let objs = vec![
            ObjectRow {
                key: "a.png".into(),
                size: 1,
                modified: "x".into(),
            },
            ObjectRow {
                key: "nested/b.png".into(),
                size: 2,
                modified: "x".into(),
            },
            ObjectRow {
                key: "nested/c.png".into(),
                size: 3,
                modified: "x".into(),
            },
        ];
        let g = group_objects_by_prefix(&objs, "");
        assert_eq!(g.folders, vec!["nested".to_string()]);
        assert_eq!(g.files.len(), 1);
        assert_eq!(g.files[0].key, "a.png");
    }

    #[test]
    fn group_objects_by_prefix_filters_by_current_prefix() {
        let objs = vec![
            ObjectRow {
                key: "a.png".into(),
                size: 1,
                modified: "x".into(),
            },
            ObjectRow {
                key: "nested/b.png".into(),
                size: 2,
                modified: "x".into(),
            },
            ObjectRow {
                key: "nested/sub/c.png".into(),
                size: 3,
                modified: "x".into(),
            },
        ];
        let g = group_objects_by_prefix(&objs, "nested/");
        assert_eq!(g.folders, vec!["sub".to_string()]);
        assert_eq!(g.files.len(), 1);
        assert_eq!(g.files[0].key, "nested/b.png");
    }

    #[test]
    fn group_objects_by_prefix_dedups_folder_names() {
        let objs = vec![
            ObjectRow {
                key: "x/a".into(),
                size: 0,
                modified: "x".into(),
            },
            ObjectRow {
                key: "x/b".into(),
                size: 0,
                modified: "x".into(),
            },
        ];
        let g = group_objects_by_prefix(&objs, "");
        assert_eq!(g.folders, vec!["x".to_string()]);
    }

    #[test]
    fn render_objects_table_empty_state() {
        let listing = FolderListing {
            folders: Vec::new(),
            files: Vec::new(),
        };
        let html = render_objects_table("photos", "", &listing).into_string();
        assert!(
            html.contains("This folder is empty"),
            "missing empty hint: {html}"
        );
    }

    #[test]
    fn render_objects_table_with_files_and_folders() {
        let f1 = ObjectRow {
            key: "a.png".into(),
            size: 1024,
            modified: "2026-05-06T10:00:00Z".into(),
        };
        let listing = FolderListing {
            folders: vec!["nested".into()],
            files: vec![&f1],
        };
        let html = render_objects_table("photos", "", &listing).into_string();
        // folder row with "📁" icon + link into the prefix
        assert!(html.contains("nested"), "folder name missing: {html}");
        assert!(
            html.contains(r#"href="/b/storage/photos/nested/""#),
            "folder href wrong: {html}"
        );
        // file row: filename portion only, no leading prefix
        assert!(html.contains(">a.png<"), "filename missing: {html}");
        assert!(html.contains("1.0 KB"), "humanized size missing: {html}");
        // kebab menu trigger
        assert!(html.contains(r#"data-action-menu"#), "kebab missing");
        assert!(
            html.contains(r#"data-bucket="photos""#),
            "kebab data-bucket missing/wrong: {html}"
        );
        assert!(
            html.contains(r#"data-key="a.png""#),
            "kebab data-key missing/wrong: {html}"
        );
    }

    /// SIZE renders via `format_bytes` (not the raw byte count) and the
    /// MODIFIED cell's visible text is humanized while the `<time>` element's
    /// `datetime` attribute keeps the full raw timestamp.
    #[test]
    fn render_objects_table_humanizes_size_and_modified() {
        let f1 = ObjectRow {
            key: "index.html".into(),
            size: 105,
            modified: "2026-07-11T19:13:45.123456789+00:00".into(),
        };
        let listing = FolderListing {
            folders: Vec::new(),
            files: vec![&f1],
        };
        let html = render_objects_table("site-assets", "", &listing).into_string();

        // Size: humanized, not the bare number cell.
        assert!(html.contains(">105 B<"), "size not humanized: {html}");

        // Modified: full raw timestamp preserved in the datetime attribute...
        assert!(
            html.contains(r#"datetime="2026-07-11T19:13:45.123456789+00:00""#),
            "datetime attr must keep the full timestamp: {html}"
        );
        // ...while the visible text is the humanized form, not the raw string.
        assert!(
            html.contains(">2026-07-11 19:13<"),
            "visible modified text not humanized: {html}"
        );
        assert!(
            !html.contains(">2026-07-11T19:13:45.123456789+00:00<"),
            "raw timestamp must not be the visible text: {html}"
        );
    }

    #[test]
    fn render_objects_table_filename_strips_prefix() {
        let f1 = ObjectRow {
            key: "nested/sub/c.png".into(),
            size: 0,
            modified: "x".into(),
        };
        let listing = FolderListing {
            folders: Vec::new(),
            files: vec![&f1],
        };
        let html = render_objects_table("photos", "nested/sub/", &listing).into_string();
        // The file row label is just the filename portion.
        assert!(html.contains(">c.png<"), "filename portion missing: {html}");
        // The download link still uses the full key.
        assert!(
            html.contains(r#"href="/b/storage/api/buckets/photos/objects/nested/sub/c.png""#),
            "download href wrong: {html}"
        );
    }

    #[test]
    fn render_objects_table_url_encodes_key_with_spaces() {
        let f1 = ObjectRow {
            key: "report Q2.pdf".into(),
            size: 0,
            modified: "x".into(),
        };
        let listing = FolderListing {
            folders: Vec::new(),
            files: vec![&f1],
        };
        let html = render_objects_table("photos", "", &listing).into_string();
        assert!(
            html.contains(r#"href="/b/storage/api/buckets/photos/objects/report%20Q2.pdf""#),
            "download href not URL-encoded: {html}"
        );
        // Display text remains the raw filename (HTML-escaped by maud).
        assert!(
            html.contains(">report Q2.pdf<"),
            "filename text wrong: {html}"
        );
    }

    #[test]
    fn render_objects_table_url_encodes_prefix_with_spaces() {
        let f1 = ObjectRow {
            key: "my files/sub/c.png".into(),
            size: 0,
            modified: "x".into(),
        };
        let listing = FolderListing {
            folders: vec!["sub".into()],
            files: vec![&f1],
        };
        let html = render_objects_table("photos", "my files/", &listing).into_string();
        // Folder href should encode the prefix's space.
        assert!(
            html.contains(r#"href="/b/storage/photos/my%20files/sub/""#),
            "folder href should URL-encode prefix space: {html}"
        );
    }

    #[test]
    fn render_breadcrumbs_root_only() {
        let html = render_breadcrumbs("photos", "").into_string();
        // bucket name visible, no extra crumbs.
        assert!(html.contains("photos"));
        assert!(!html.contains("nested"));
    }

    #[test]
    fn render_breadcrumbs_includes_each_segment() {
        let html = render_breadcrumbs("photos", "nested/sub/").into_string();
        // Each crumb except the last has a clickable link;
        // the last segment is non-link text.
        assert!(html.contains("photos"));
        assert!(html.contains(r#"href="/b/storage/photos/nested/""#));
        assert!(html.contains(">sub<"));
        // Last segment ("sub") must NOT be a link.
        assert!(
            !html.contains(r#"href="/b/storage/photos/nested/sub/""#),
            "last segment should be plain text, not a link: {html}"
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::{super::test_helpers::seed_two_buckets, *};
    use crate::test_support::{admin_msg, output_html, TestContext};

    #[tokio::test]
    async fn object_list_page_root_renders_files_and_folders() {
        let ctx = TestContext::with_files().await;
        seed_two_buckets(&ctx, "admin_1").await;

        let msg = admin_msg("retrieve", "/b/storage/photos/");
        let resp = object_list_page(&ctx, &msg, "photos", "").await;
        let body = output_html(resp).await;

        assert!(body.contains(">a.png<"), "root file missing: {body}");
        assert!(
            body.contains("📁 nested"),
            "synthesized folder missing: {body}"
        );
        // Breadcrumb has only the bucket segment, no prefix segments.
        assert!(
            body.contains(r#"href="/b/storage/""#),
            "Files crumb link missing: {body}"
        );
    }

    #[tokio::test]
    async fn object_list_page_with_prefix_strips_filename() {
        let ctx = TestContext::with_files().await;
        seed_two_buckets(&ctx, "admin_1").await;

        let msg = admin_msg("retrieve", "/b/storage/photos/nested/");
        let resp = object_list_page(&ctx, &msg, "photos", "nested/").await;
        let body = output_html(resp).await;

        // Filename portion of "nested/b.png" is just "b.png".
        assert!(body.contains(">b.png<"), "filename missing: {body}");
        assert!(!body.contains(">nested/b.png<"), "raw key leaked: {body}");
    }

    #[tokio::test]
    async fn object_list_page_404_for_unknown_bucket() {
        let ctx = TestContext::with_files().await;
        let mut msg = admin_msg("retrieve", "/b/storage/missing/");
        msg.set_meta("http.header.accept", "text/html");
        let resp = object_list_page(&ctx, &msg, "missing", "").await;
        let body = output_html(resp).await;
        assert!(
            body.contains("Not found") || body.contains("404"),
            "expected 404: {body}"
        );
    }

    #[tokio::test]
    async fn object_list_page_404_for_other_users_bucket() {
        // Cross-user isolation: a bucket owned by another user must 404,
        // not render its contents. The request is made as an ADMIN, which
        // pins the documented policy split: the SSR portal routes through the
        // shared `storage::bucket_owned_by` predicate and deliberately does
        // NOT grant the admin bypass that the JSON API's
        // `is_bucket_access_denied` does — so even an admin sees a 404 here.
        let ctx = TestContext::with_files().await;
        let mut row: HashMap<String, serde_json::Value> = HashMap::new();
        row.insert("name".into(), json!("secrets"));
        row.insert("created_by".into(), json!("other_user"));
        repo::buckets::seed(&ctx, row).await.expect("seed");

        let mut msg = admin_msg("retrieve", "/b/storage/secrets/");
        msg.set_meta("http.header.accept", "text/html");
        let resp = object_list_page(&ctx, &msg, "secrets", "").await;
        let body = output_html(resp).await;
        assert!(
            body.contains("Not found") || body.contains("404"),
            "expected 404 for cross-user bucket (admin, no SSR bypass): {body}"
        );
    }

    #[tokio::test]
    async fn object_list_page_renders_empty_state_for_empty_bucket() {
        let ctx = TestContext::with_files().await;
        // seed_two_buckets seeds `docs` with no objects.
        seed_two_buckets(&ctx, "admin_1").await;

        let msg = admin_msg("retrieve", "/b/storage/docs/");
        let resp = object_list_page(&ctx, &msg, "docs", "").await;
        let body = output_html(resp).await;

        assert!(
            body.contains("This folder is empty"),
            "expected empty-state copy: {body}"
        );
    }

    #[tokio::test]
    async fn object_list_page_includes_files_browser_js() {
        let ctx = TestContext::with_files().await;
        seed_two_buckets(&ctx, "admin_1").await;

        let msg = admin_msg("retrieve", "/b/storage/photos/");
        let resp = object_list_page(&ctx, &msg, "photos", "").await;
        let body = output_html(resp).await;

        assert!(
            body.contains(r#"id="files-browser-bootstrap""#),
            "bootstrap carrier missing: {body}"
        );
        assert!(
            body.contains(r#""bucket":"photos""#),
            "bootstrap bucket missing: {body}"
        );
        assert!(
            body.contains(r#""currentPrefix":"""#) || body.contains(r#""currentPrefix": """#),
            "bootstrap currentPrefix missing: {body}"
        );
        assert!(
            body.contains("/b/static/files-browser-"),
            "files-browser.js script tag missing: {body}"
        );
    }

    #[tokio::test]
    async fn object_list_page_shows_actual_size_from_text_columns() {
        // SQLite TEXT columns store integers as strings (see MEMORY.md
        // wafer-wrap-table-naming). The renderer must coerce both shapes.
        let ctx = TestContext::with_files().await;
        let mut bucket: HashMap<String, serde_json::Value> = HashMap::new();
        bucket.insert("name".into(), json!("photos"));
        bucket.insert("created_by".into(), json!("admin_1"));
        repo::buckets::seed(&ctx, bucket)
            .await
            .expect("seed bucket");

        let mut obj: HashMap<String, serde_json::Value> = HashMap::new();
        obj.insert("bucket".into(), json!("photos"));
        obj.insert("key".into(), json!("a.png"));
        // Note: json!(2048) is a JSON number, but the SQLite backend will
        // round-trip it as a string. The fallback in list_objects_in_bucket
        // must accept both shapes.
        obj.insert("size".into(), json!(2048));
        obj.insert("uploaded_by".into(), json!("admin_1"));
        repo::objects::seed(&ctx, obj).await.expect("seed obj");

        let msg = admin_msg("retrieve", "/b/storage/photos/");
        let body = output_html(object_list_page(&ctx, &msg, "photos", "").await).await;
        // The Size column should show the humanized 2048 ("2.0 KB"), not the
        // "0 B" a failed TEXT-column coercion would produce.
        assert!(
            body.contains(r#"data-label="Size">2.0 KB<"#),
            "size cell should be 2.0 KB (2048 via TEXT fallback): {body}"
        );
    }

    #[tokio::test]
    async fn object_list_page_escapes_script_close_in_bootstrap() {
        // A bucket name containing `</script>` would prematurely close the
        // <script type="application/json"> bootstrap carrier. The render
        // path must escape `<` so that no `</script>` appears in the JSON.
        let ctx = TestContext::with_files().await;
        let mut bucket: HashMap<String, serde_json::Value> = HashMap::new();
        bucket.insert("name".into(), json!("foo</script>bar"));
        bucket.insert("created_by".into(), json!("admin_1"));
        repo::buckets::seed(&ctx, bucket).await.expect("seed");

        let msg = admin_msg("retrieve", "/b/storage/foo</script>bar/");
        let body = output_html(object_list_page(&ctx, &msg, "foo</script>bar", "").await).await;

        // The dangerous substring must NOT appear in the rendered HTML.
        // The escaped form `</script>` is the safe representation.
        assert!(
            !body.contains("</script>foo") && !body.contains("foo</script>bar\""),
            "bootstrap is broken by unescaped </script>: {body}"
        );
        // The escaped form should appear (defensive — proves the escape ran).
        assert!(
            body.contains("\\u003c/script\\u003e") || body.contains("\\u003c/script>"),
            "expected escaped </script> sequence in bootstrap: {body}"
        );
    }
}
