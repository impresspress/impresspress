//! Object/folder browsing domain: `/b/storage/{bucket}/[{prefix}/]` — lists
//! objects and synthesizes folder navigation from key prefixes.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::files::repo,
    ui::{
        self,
        components::{self, DataTable, TableCol, TableRow},
        icons,
        shell::Crumb,
        templates::list_page,
    },
    util::{format_bytes, url_path_encode},
};

/// Object as the user sees it (key, size, modified timestamp).
///
/// A render-side projection of [`repo::objects::ObjectRow`]; the browser
/// renders the upload instant as "modified" and needs nothing else from the
/// row.
#[derive(Clone, Debug)]
pub struct ObjectRow {
    pub key: String,
    pub size: i64,
    pub modified: String,
}

impl From<&repo::objects::ObjectRow> for ObjectRow {
    fn from(row: &repo::objects::ObjectRow) -> Self {
        Self {
            key: row.key.clone(),
            size: row.size,
            modified: row.uploaded_at.clone(),
        }
    }
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

const OBJECT_COLUMNS: [TableCol<'static>; 5] = [
    TableCol::new("Select").select(),
    TableCol::new("Name").primary(),
    TableCol::new("Size"),
    TableCol::new("Modified"),
    TableCol::new("Actions").actions(),
];

/// The "+ Upload" trigger: it opens the hidden file picker
/// (`files-browser.js` binds every `[data-action="open-upload"]`). The topbar
/// action and the empty folder's call to action both render it.
pub(crate) fn upload_button() -> Markup {
    components::button(
        components::BtnVariant::Primary,
        components::CtrlSize::Md,
        "+ Upload",
        maud::PreEscaped(r#"type="button" data-action="open-upload""#.to_string()),
    )
}

/// A file row's "more actions" trigger: the shared 44px icon button, named
/// after the file. `files-browser.js` opens the row's menu (Share / Copy link
/// / Delete) from it and drives the menu from the keyboard.
fn row_menu_trigger(bucket: &str, key: &str, filename: &str) -> Markup {
    html! {
        button .btn .btn--ghost .btn--icon
            type="button"
            data-action-menu
            data-bucket=(bucket)
            data-key=(key)
            aria-haspopup="menu"
            aria-expanded="false"
            aria-label={"Actions for " (filename)}
        { (icons::more_horizontal()) }
    }
}

/// The bar above a folder's files: "Select all files" and, once anything is
/// selected, how many and the bulk delete. A bar rather than a checkbox in the
/// table header, because below 720px the table is cards and has no header.
/// Rendered only when the folder holds files (folders cannot be selected).
fn render_bulk_bar() -> Markup {
    html! {
        div .bulk-bar {
            label .form-checkbox {
                input type="checkbox" data-bulk-toggle;
                "Select all files"
            }
            div .bulk-bar__selection #bulk-action-bar hidden {
                span .bulk-bar__count aria-live="polite" data-bulk-count {}
                button .btn .btn--ghost-danger .btn--sm type="button" data-bulk-delete {
                    (icons::trash()) "Delete selected"
                }
            }
        }
    }
}

/// Folder/file table for `/b/storage/{bucket}/...` views.
///
/// Folder rows link into `/b/storage/{bucket}/{prefix}{folder}/`. File rows
/// show the filename portion (after the `current_prefix`), link to the
/// download route, carry a checkbox named after the file for bulk actions and
/// a "more actions" menu trigger. A folder has neither, nor a size or date:
/// those cells are empty, so a card drops them.
pub fn render_objects_table(
    bucket: &str,
    current_prefix: &str,
    listing: &FolderListing<'_>,
) -> Markup {
    let folder_rows = listing.folders.iter().map(|folder| {
        TableRow::new(vec![
            html! {},
            html! {
                a .row--folder__link href={"/b/storage/" (url_path_encode(bucket)) "/" (url_encode_prefix(current_prefix)) (url_path_encode(folder)) "/"} {
                    span aria-hidden="true" { (icons::folder()) }
                    (folder)
                }
            },
            html! {},
            html! {},
            html! {},
        ])
    });
    let file_rows = listing.files.iter().map(|f| {
        let filename = f.key.strip_prefix(current_prefix).unwrap_or(&f.key);
        let download_href = format!(
            "/b/storage/api/buckets/{}/objects/{}",
            url_path_encode(bucket),
            f.key
                .split('/')
                .map(url_path_encode)
                .collect::<Vec<_>>()
                .join("/"),
        );
        TableRow::new(vec![
            // The label is the checkbox's 44px hit area and its name.
            html! {
                label .form-checkbox {
                    input type="checkbox" .bulk-select data-key=(f.key);
                    span .sr-only { "Select " (filename) }
                }
            },
            html! { a href=(download_href) { (filename) } },
            html! { (format_bytes(f.size)) },
            components::timestamp(&f.modified),
            row_menu_trigger(bucket, &f.key, filename),
        ])
    });
    html! {
        @if !listing.files.is_empty() { (render_bulk_bar()) }
        (DataTable::new(&OBJECT_COLUMNS)
            .rows(folder_rows.chain(file_rows).collect())
            .empty(components::empty_state(
                icons::upload(),
                "This folder is empty",
                "Drag files here, or upload them from your device.",
                Some(upload_button()),
            ))
            .render())
    }
}

/// The "Create share link" modal a row's menu opens: the shared
/// `components::modal` dialog. `files-browser.js` (`shareModal`) writes the
/// object it is about into `#share-object` and opens it through chrome.js's
/// `openModal` event; its form handler POSTs `/b/cloudstorage/shares`.
///
/// The expiry values are HOURS — the unit that endpoint takes in
/// `expires_in_hours`; the labels are the days the user thinks in. There is
/// no "never": a share link is a bearer credential, so every one of them
/// ends. The longest option is the deployment's default ceiling
/// (`IMPRESSPRESS__FILES__MAX_SHARE_EXPIRY_HOURS`), which the server applies
/// to a request that names no expiry at all. The `cloud` tests read these
/// options back out of this markup and hold them to the endpoint.
pub(crate) fn render_share_modal() -> Markup {
    components::modal(
        "share-link",
        "Create share link",
        html! {
            form method="dialog" {
                p .text-sm .text-muted .mb-3 { "Object: " code #share-object {} }
                div .form-group {
                    label .form-label for="share-expires" { "Expires in" }
                    select .form-input #share-expires name="expires" autofocus {
                        option value="24" { "1 day" }
                        option value="168" selected { "7 days" }
                        option value="720" { "30 days" }
                        option value="8760" { "365 days" }
                    }
                }
                div .form-group {
                    label .form-label for="share-max" { "Max accesses" }
                    input .form-input #share-max name="max" type="number" min="0"
                        placeholder="Unlimited" aria-describedby="share-max-hint";
                    p .form-hint #share-max-hint { "Leave empty for no limit." }
                }
                (components::modal_footer(html! {
                    (components::modal_cancel())
                    button .btn .btn--primary .btn--block type="submit" { "Create link" }
                }))
            }
        },
    )
}

/// The folder path inside a bucket, drawn in the page body below the topbar
/// when the page is a folder rather than the bucket's root: the bucket, then
/// each folder, each one a link except the current one
/// (`aria-current="page"`).
///
/// The topbar carries "Files ›" and the bucket as the page title; this trail
/// continues it into the bucket with the same separator, so the two never
/// repeat each other. At the bucket's root it renders nothing — the topbar
/// already says where the page is.
pub fn render_breadcrumbs(bucket: &str, current_prefix: &str) -> Markup {
    let segments: Vec<&str> = current_prefix
        .trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        return html! {};
    }
    let last_idx = segments.len();
    let encoded_bucket = url_path_encode(bucket);

    html! {
        nav .breadcrumbs aria-label="Folder" {
            ol {
                li { a href={"/b/storage/" (encoded_bucket) "/"} { (bucket) } }
                @for (i, seg) in segments.iter().enumerate() {
                    li {
                        @if i + 1 == last_idx {
                            span aria-current="page" { (seg) }
                        } @else {
                            @let cumulative: String = segments[..=i].iter().map(|s| url_path_encode(s)).collect::<Vec<_>>().join("/");
                            a href={"/b/storage/" (encoded_bucket) "/" (cumulative) "/"} { (seg) }
                        }
                    }
                }
            }
        }
    }
}

/// The bucket's objects, or the failure that stopped us reading them.
///
/// An empty vector means the bucket is empty; it must never also mean the
/// listing failed, because the page renders the two identically ("No files
/// yet") and the folder navigation below is synthesized from these keys.
async fn list_objects_in_bucket(
    ctx: &dyn Context,
    bucket: &str,
) -> Result<Vec<ObjectRow>, wafer_run::WaferError> {
    let page = repo::objects::list_for_bucket(ctx, bucket, 1000).await?;
    Ok(page.rows.iter().map(ObjectRow::from).collect())
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
    match crate::blocks::files::storage::bucket_owned_by(ctx, &user_id, bucket).await {
        Ok(true) => {}
        Ok(false) => return crate::ui::not_found_response(msg),
        Err(e) => return crate::blocks::crud::db_error_page(msg, e, "object list page: ownership"),
    }

    let all_objects = match list_objects_in_bucket(ctx, bucket).await {
        Ok(rows) => rows,
        Err(e) => return crate::blocks::crud::db_error_page(msg, e, "object list page"),
    };
    let listing = group_objects_by_prefix(&all_objects, current_prefix);

    let document_title = if current_prefix.is_empty() {
        bucket.to_string()
    } else {
        format!("{bucket} / {}", current_prefix.trim_end_matches('/'))
    };

    let table = render_objects_table(bucket, current_prefix, &listing);
    let body = html! {
        (render_breadcrumbs(bucket, current_prefix))
        // Hidden file input that every "+ Upload" trigger opens. Multi-select
        // so users can pick many files at once. Same upload endpoint as
        // drag-drop.
        input #file-upload-input type="file" multiple hidden;
        (table)
        (render_share_modal())
        (super::render_bootstrap_script(bucket, current_prefix))
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::portal(&document_title, bucket)
            .trail(vec![
                Crumb {
                    label: "Files",
                    href: Some("/b/storage/"),
                },
                Crumb {
                    label: bucket,
                    href: None,
                },
            ])
            .subtitle("Drag files here to upload, or use the Upload button.")
            .actions(vec![upload_button()]),
        list_page(None, body, None),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The projection takes the three columns the browser renders and reads
    /// no field itself — the "modified" column is the row's `uploaded_at`.
    #[test]
    fn projects_the_three_columns_the_browser_renders() {
        let row = repo::objects::ObjectRow::from_record(&wafer_core::clients::database::Record {
            id: "o1".to_string(),
            data: [
                ("key".to_string(), serde_json::json!("nested/a.png")),
                ("size".to_string(), serde_json::json!("2048")),
                (
                    "status".to_string(),
                    serde_json::json!(crate::blocks::files::contracts::ObjectStatus::Complete),
                ),
                (
                    "uploaded_at".to_string(),
                    serde_json::json!("2026-05-06T10:00:00Z"),
                ),
            ]
            .into_iter()
            .collect(),
        })
        .expect("the fixture row decodes");
        let projected = ObjectRow::from(&row);
        assert_eq!(projected.key, "nested/a.png");
        assert_eq!(projected.size, 2048);
        assert_eq!(projected.modified, "2026-05-06T10:00:00Z");
    }

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
        // folder row with folder icon (icons::folder()) + link into the prefix
        assert!(html.contains("nested"), "folder name missing: {html}");
        assert!(
            html.contains(r#"href="/b/storage/photos/nested/""#),
            "folder href wrong: {html}"
        );
        // file row: filename portion only, no leading prefix
        assert!(html.contains(">a.png<"), "filename missing: {html}");
        assert!(html.contains("1.0 KB"), "humanized size missing: {html}");
        // the row's menu trigger
        assert!(
            html.contains(r#"data-action-menu"#),
            "row menu trigger missing"
        );
        assert!(
            html.contains(r#"data-bucket="photos""#),
            "menu data-bucket missing/wrong: {html}"
        );
        assert!(
            html.contains(r#"data-key="a.png""#),
            "menu data-key missing/wrong: {html}"
        );
    }

    /// SIZE renders via `format_bytes` (not the raw byte count) and the
    /// MODIFIED cell's visible text is humanized while the `<time>` element's
    /// `datetime` attribute carries the instant as a valid HTML date-time
    /// (UTC, milliseconds — HTML allows at most three fraction digits).
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

        // Modified: the instant in the datetime attribute...
        assert!(
            html.contains(r#"datetime="2026-07-11T19:13:45.123Z""#),
            "datetime attr must carry the instant to the millisecond: {html}"
        );
        // ...while the visible text is the humanized form, not the raw string.
        assert!(
            html.contains(">2026-07-11 19:13 UTC<"),
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

    /// Every control a file row carries is named after the file: the
    /// selection checkbox, and the 44px icon button that opens its menu.
    /// Folder rows carry neither, and their empty cells are left empty so a
    /// card drops them.
    #[test]
    fn a_file_rows_controls_are_named_after_the_file() {
        let f1 = ObjectRow {
            key: "nested/report.pdf".into(),
            size: 10,
            modified: "2026-05-06T10:00:00Z".into(),
        };
        let listing = FolderListing {
            folders: vec!["deeper".into()],
            files: vec![&f1],
        };
        let html = render_objects_table("photos", "nested/", &listing).into_string();
        assert!(
            html.contains(r#"<label class="form-checkbox"><input class="bulk-select" type="checkbox" data-key="nested/report.pdf"><span class="sr-only">Select report.pdf</span></label>"#),
            "the row checkbox is labelled: {html}"
        );
        assert!(
            html.contains(r#"<button class="btn btn--ghost btn--icon" type="button" data-action-menu data-bucket="photos" data-key="nested/report.pdf" aria-haspopup="menu" aria-expanded="false" aria-label="Actions for report.pdf"><svg"#),
            "the menu trigger is the shared icon button with an SVG and a name: {html}"
        );
        assert!(!html.contains('⋯'), "no text glyph for an icon: {html}");
        assert!(
            html.contains(r#"<label class="form-checkbox"><input type="checkbox" data-bulk-toggle>Select all files</label>"#),
            "select-all is a visible, labelled control: {html}"
        );
        // The select and actions columns are headed for screen readers only.
        assert!(
            html.contains(r#"<th><span class="sr-only">Select</span></th>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<th><span class="sr-only">Actions</span></th>"#),
            "{html}"
        );
        // The folder row's control, size and date cells are empty cells.
        assert_eq!(
            html.matches("data-table__cell--empty").count(),
            4,
            "the folder row's four empty cells: {html}"
        );
    }

    /// An empty folder offers the upload it is waiting for, and no
    /// "Select all" over nothing.
    #[test]
    fn an_empty_folder_offers_an_upload() {
        let listing = FolderListing {
            folders: Vec::new(),
            files: Vec::new(),
        };
        let html = render_objects_table("photos", "", &listing).into_string();
        assert!(
            html.contains(r#"<h2 class="empty__title">This folder is empty</h2>"#),
            "{html}"
        );
        assert!(html.contains(r#"data-action="open-upload""#), "{html}");
        assert!(!html.contains("data-bulk-toggle"), "{html}");
    }

    #[test]
    fn render_breadcrumbs_is_empty_at_the_bucket_root() {
        // The topbar's "Files › photos" already says where the page is.
        assert!(render_breadcrumbs("photos", "").into_string().is_empty());
    }

    #[test]
    fn render_breadcrumbs_includes_each_segment() {
        let html = render_breadcrumbs("photos", "nested/sub/").into_string();
        // Each crumb except the last has a clickable link;
        // the last segment is non-link text.
        assert!(html.contains("photos"));
        assert!(html.contains(r#"href="/b/storage/photos/nested/""#));
        assert!(html.contains(">sub<"));
        assert!(
            html.contains(r#"<span aria-current="page">sub</span>"#),
            "the current folder is marked: {html}"
        );
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
            body.contains("row--folder__link") && body.contains("nested"),
            "synthesized folder missing: {body}"
        );
        // Folder rows render distinctly via the Lucide folder icon (no
        // longer a raw emoji character) -- pin the icon's SVG path so a
        // future regression to plain text (or a different icon) shows up
        // here.
        assert!(
            body.contains(r#"M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9"#),
            "folder icon svg missing: {body}"
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
        // `require_bucket_access` does — so even an admin sees a 404 here.
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

#[cfg(test)]
mod outage_tests {
    //! An object listing that FAILED is not an empty bucket.

    use super::*;
    use crate::{
        blocks::files::repo,
        test_support::{admin_msg, output_http_status, FailingDbOpContext, TestContext},
    };

    /// The ownership check reads the *buckets* table and must still pass, so
    /// the fault is scoped to the objects listing: this is the "bucket found,
    /// listing failed" shape, which used to render "No files yet".
    #[tokio::test]
    async fn a_failing_object_list_renders_the_error_page_not_an_empty_bucket() {
        let ctx = TestContext::with_files().await;
        super::super::test_helpers::seed_two_buckets(&ctx, "admin_1").await;
        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.list", repo::objects::TABLE)]);

        let out = object_list_page(
            &failing,
            &admin_msg("retrieve", "/b/storage/photos/"),
            "photos",
            "",
        )
        .await;
        assert_eq!(
            output_http_status(out).await,
            500,
            "an unreadable object list must not render as an empty bucket"
        );
    }
}
