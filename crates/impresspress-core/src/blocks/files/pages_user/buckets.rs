//! Bucket-list domain: the `/b/storage/` page showing a user's buckets
//! with live object counts, and the "New bucket" creation modal.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{crud, files::repo},
    db_read::CappedList,
    ui::{
        self,
        components::{
            self, Action, ActionTarget, BadgeVariant, DataTable, HeaderAction, TableCol, TableRow,
        },
        icons,
        templates::list_page,
    },
    util::url_path_encode,
};

/// Aggregated bucket info as shown in the user-facing table:
/// name, public flag, created-at ISO string, and live object count.
///
/// A render-side projection of [`repo::buckets::BucketRow`] — it holds no
/// decoding of its own, only the object count, which comes from a second
/// query rather than from the bucket row.
#[derive(Clone, Debug)]
pub struct BucketRow {
    pub name: String,
    pub public: bool,
    pub created_at: String,
    pub object_count: i64,
}

impl From<(&repo::buckets::BucketRow, i64)> for BucketRow {
    fn from((row, object_count): (&repo::buckets::BucketRow, i64)) -> Self {
        Self {
            name: row.name.clone(),
            public: row.public,
            created_at: row.created_at.clone(),
            object_count,
        }
    }
}

/// A bucket's visibility as one badge, the same on every page that lists
/// buckets: "Public" in the warning tone — anyone with an object's URL can
/// read it, which is a risk to see at a glance — and "Private" neutral (the
/// default, nothing to call out).
pub(crate) fn visibility_badge(public: bool) -> Markup {
    if public {
        components::badge(BadgeVariant::Warning, "Public")
    } else {
        components::badge(BadgeVariant::Secondary, "Private")
    }
}

/// The "New bucket" trigger: one label and one target, which both of its
/// renderings take — the topbar's primary ([`new_bucket_action`]) and the
/// empty list's secondary ([`no_buckets_empty_state`]). It opens the modal
/// through chrome.js's `modal-open` verb.
const NEW_BUCKET: (&str, ActionTarget<'static>) =
    ("New bucket", ActionTarget::Modal(NEW_BUCKET_MODAL_ID));

/// The "New bucket" label, for the storage overview's call to action, which
/// leads to the bucket list (where the modal lives) rather than opening it.
pub(crate) const NEW_BUCKET_LABEL: &str = NEW_BUCKET.0;

/// The trigger as the topbar's primary action, on the user's bucket list and
/// administration's.
pub(crate) fn new_bucket_action() -> HeaderAction<'static> {
    let (label, target) = NEW_BUCKET;
    HeaderAction::new(label, target).icon(icons::plus())
}

/// The empty state a bucket list shows before any bucket exists. The header
/// already offers "New bucket" as the page's primary, so the empty state's
/// copy of it is a secondary (layout rule 9).
pub(crate) fn no_buckets_empty_state() -> Markup {
    let (label, target) = NEW_BUCKET;
    no_buckets_empty(Action::secondary(label, target).icon(icons::plus()))
}

/// "No buckets yet" with the caller's call to action: one wording for the
/// user's list, administration's, and the storage overview.
pub(crate) fn no_buckets_empty(action: Action<'_>) -> Markup {
    components::empty_state(
        icons::folder(),
        "No buckets yet",
        "Buckets hold uploaded files. Create one to start uploading.",
        Some(action.render()),
    )
}

const BUCKET_COLUMNS: [TableCol<'static>; 4] = [
    TableCol::new("Name").primary(),
    TableCol::new("Visibility"),
    TableCol::new("Created"),
    TableCol::new("Objects"),
];

/// Render the bucket-list table (or empty state).
pub fn render_buckets_table(rows: &[BucketRow]) -> Markup {
    DataTable::new(&BUCKET_COLUMNS)
        .rows(
            rows.iter()
                .map(|r| {
                    TableRow::new(vec![
                        html! { a href={"/b/storage/" (url_path_encode(&r.name)) "/"} { (r.name) } },
                        visibility_badge(r.public),
                        components::timestamp(&r.created_at),
                        html! { (r.object_count) },
                    ])
                })
                .collect(),
        )
        .empty(no_buckets_empty_state())
        .render()
}

/// The "New bucket" modal's element id, which its triggers name in
/// `data-modal-target`.
pub const NEW_BUCKET_MODAL_ID: &str = "new-bucket";

/// Render the "New bucket" modal: the shared `components::modal` dialog,
/// which chrome.js opens and closes. The form is wired by
/// `bucketCreateForm()` in `files-browser.js`: it intercepts submit, checks
/// the name, POSTs to `/b/storage/api/buckets`, and on success redirects to
/// `/b/storage/{name}/`; a refusal lands in the alert at the top of the form.
/// `method="dialog"` is what a submit does if that script never loaded:
/// it closes the modal rather than navigating.
pub fn render_new_bucket_modal() -> Markup {
    components::modal(
        NEW_BUCKET_MODAL_ID,
        "New bucket",
        html! {
            form method="dialog" {
                p .alert .alert--error #new-bucket-error role="alert" hidden {}
                div .form-group {
                    label .form-label .required for="new-bucket-name" { "Name" }
                    input .form-input
                        #new-bucket-name
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
                        placeholder="my-bucket"
                        aria-describedby="new-bucket-hint"
                        autofocus;
                    p .form-hint #new-bucket-hint {
                        "3–63 characters. Lowercase letters, digits, and hyphens. Must start and end with a letter or digit."
                    }
                }
                div .form-group {
                    label .form-checkbox {
                        input type="checkbox" name="public" value="1";
                        " Public (objects can be accessed by anonymous URL)"
                    }
                }
                (components::modal_footer(html! {
                    (components::modal_cancel())
                    (Action::primary("Create bucket", ActionTarget::Submit { form: None }).render())
                }))
            }
        },
    )
}

/// Load the calling user's buckets, decorated with live object counts.
///
/// `created_by` filtering (inside [`repo::buckets::list_owned_sorted`])
/// keeps users from seeing each other's buckets. Object counts come from
/// a single GROUP BY query on the objects table
/// ([`repo::objects::count_by_bucket`], one row per bucket) so we avoid
/// the previous N+1 count query per bucket.
///
/// Both reads propagate. Either one collapsing into an empty collection is
/// indistinguishable from the truth it is standing in for — "this account
/// has no buckets", and "this bucket holds no objects" — so the page it
/// feeds renders an error rather than that lie.
pub async fn list_buckets_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<CappedList<BucketRow>, wafer_run::WaferError> {
    use std::collections::HashMap;

    let owned = repo::buckets::list_owned_sorted(ctx, user_id).await?;
    let truncated = owned.truncated;

    // Restrict the GROUP BY to the buckets this user owns so the count
    // matches the previous per-bucket count semantics exactly (which
    // counted all objects in the bucket regardless of `uploaded_by`).
    let bucket_names: Vec<String> = owned.rows.iter().map(|r| r.name.clone()).collect();
    let counts_by_bucket: HashMap<String, i64> =
        repo::objects::count_by_bucket(ctx, &bucket_names).await?;

    Ok(CappedList {
        truncated,
        rows: owned
            .rows
            .iter()
            .map(|row| {
                let count = counts_by_bucket.get(&row.name).copied().unwrap_or(0);
                BucketRow::from((row, count))
            })
            .collect(),
    })
}

/// GET `/b/storage/` — bucket list for the calling user.
pub async fn bucket_list_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    // The handle() above us already enforces auth; this guard is defensive.
    if user_id.is_empty() {
        return ui::not_found_response(msg);
    }

    let rows = match list_buckets_for_user(ctx, &user_id).await {
        Ok(rows) => rows,
        Err(e) => return crud::db_error_page(msg, e, "bucket list page"),
    };

    // The table cell carries the modal markup + JS so it lives inside the
    // shelled response without needing a new template parameter.
    let js_url = crate::blocks::files::assets::files_browser_js_url();
    let table_with_modal = html! {
        @if rows.truncated {
            p .text-muted .text-sm { "Showing the first " (rows.rows.len()) " buckets." }
        }
        (render_buckets_table(&rows.rows))
        (render_new_bucket_modal())
        script src=(js_url) defer {}
    };

    let body = list_page(None, table_with_modal, None);

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::portal("Files", "Files")
            .subtitle("Your buckets and their object counts.")
            .primary_action(new_bucket_action()),
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

    /// The creation timestamp differs on every visual-baseline run, which
    /// masks dates by the `<time>` element alone.
    #[test]
    fn render_buckets_table_renders_the_created_timestamp_as_a_time_element() {
        let html = render_buckets_table(&[sample("photos", false, 0)]).into_string();
        assert!(
            html.contains(r#"<time class="datetime" datetime="2026-05-06T10:00:00.000Z" title="2026-05-06T10:00:00.000Z">2026-05-06 10:00 UTC</time>"#),
            "{html}"
        );
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
            body.contains("topbar__actions"),
            "topbar action slot missing: {body}"
        );
        assert!(
            body.contains(
                r#"<button class="btn btn--primary" type="button" data-action="modal-open" data-modal-target="new-bucket"><svg"#
            ),
            "new-bucket trigger missing: {body}"
        );
        assert!(
            body.contains("</svg>New bucket</button>"),
            "new-bucket button label missing: {body}"
        );
    }

    /// The header's "New bucket" is the page's one primary; the empty list
    /// offers the same trigger — same label, same modal — as a secondary.
    /// The modal's submit is the dialog's own primary.
    #[tokio::test]
    async fn an_empty_list_repeats_new_bucket_as_a_secondary_under_the_one_primary() {
        let ctx = TestContext::with_files().await;
        let body =
            output_html(bucket_list_page(&ctx, &admin_msg("retrieve", "/b/storage/")).await).await;
        let (page, dialogs) = crate::test_support::dom::visible_primaries(&body);
        assert_eq!(page.len(), 1, "one primary on the page: {page:?}");
        assert!(page[0].contains("New bucket"), "{page:?}");
        // The modal's submit; the shared confirm dialog shows none until it
        // asks.
        assert_eq!(
            dialogs.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![1, 0],
            "{dialogs:?}"
        );
        assert!(
            body.contains(concat!(
                r#"<div class="empty__action"><button class="btn btn--secondary" type="button" "#,
                r#"data-action="modal-open" data-modal-target="new-bucket"><svg"#
            )),
            "the empty state's trigger is a secondary opening the same modal: {body}"
        );
        assert!(body.contains("</svg>New bucket</button></div>"), "{body}");
        assert!(
            body.contains(
                r#"<button class="btn btn--primary" type="submit">Create bucket</button>"#
            ),
            "{body}"
        );
    }

    #[tokio::test]
    async fn bucket_list_page_renders_new_bucket_modal() {
        let ctx = TestContext::with_files().await;
        let msg = admin_msg("retrieve", "/b/storage/");
        let body = output_html(bucket_list_page(&ctx, &msg).await).await;

        // Modal markup is server-rendered next to the table.
        assert!(
            body.contains(r#"<dialog class="modal" id="new-bucket""#),
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

#[cfg(test)]
mod outage_tests {
    //! A read that FAILED is not "this user has no buckets".
    //!
    //! Both reads behind `/b/storage/` used to log a warning and substitute
    //! an empty collection, so an outage rendered "No buckets yet" — exactly
    //! what a brand-new account renders — and a bucket whose object count
    //! could not be read rendered `0`.

    use super::{super::test_helpers::seed_two_buckets, *};
    use crate::{
        blocks::files::repo,
        test_support::{admin_msg, output_http_status, FailingDbOpContext, TestContext},
    };

    #[tokio::test]
    async fn a_failing_bucket_list_renders_the_error_page_not_an_empty_one() {
        let ctx = TestContext::with_files().await.break_reads();
        let out = bucket_list_page(&ctx, &admin_msg("retrieve", "/b/storage/")).await;
        assert_eq!(
            output_http_status(out).await,
            500,
            "an unreadable bucket list must not render as an empty account"
        );
    }

    /// The object-count aggregate is the page's SECOND read, and its failure
    /// was just as invisible: every bucket rendered `0` objects.
    #[tokio::test]
    async fn a_failing_object_count_renders_the_error_page() {
        let ctx = TestContext::with_files().await;
        seed_two_buckets(&ctx, "admin_1").await;
        let failing = FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.aggregate", repo::objects::TABLE)],
        );

        let out = bucket_list_page(&failing, &admin_msg("retrieve", "/b/storage/")).await;
        assert_eq!(
            output_http_status(out).await,
            500,
            "an unreadable object count must not render as zero objects"
        );
    }

    /// The success path is untouched: the page still renders the buckets and
    /// their counts.
    #[tokio::test]
    async fn a_healthy_read_still_renders_the_buckets() {
        let ctx = TestContext::with_files().await;
        seed_two_buckets(&ctx, "admin_1").await;
        let rows = list_buckets_for_user(&ctx, "admin_1")
            .await
            .expect("a healthy read succeeds");
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(
            rows.rows
                .iter()
                .find(|r| r.name == "photos")
                .expect("photos bucket")
                .object_count,
            2
        );
    }
}
