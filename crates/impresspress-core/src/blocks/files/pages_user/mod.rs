//! User-facing UI pages for the impresspress/files block.
//!
//! Pure render helpers live alongside async handlers; helpers are
//! unit-tested directly without `Context`.
//!
//! Split by domain responsibility:
//! - [`buckets`] — the `/b/storage/` bucket-list page + "New bucket" modal.
//! - [`objects`] — `/b/storage/{bucket}/[{prefix}/]` object/folder browsing.
//! - [`cloudstorage`] — the `/b/cloudstorage/` share-list + quota page.
//!
//! Every item that was previously `pub` at `pages_user::*` is re-exported
//! here so external callers (`blocks/files/mod.rs`, `pages_admin.rs`) keep
//! using the same paths unchanged.

mod buckets;
mod cloudstorage;
mod objects;

// Only `bucket_list_page`, `object_list_page`, `cloudstorage_page`, and
// `render_new_bucket_modal` currently cross the `pages_user::` boundary
// (see `blocks/files/mod.rs` route dispatch + `pages_admin.rs`'s modal
// reuse). The rest were `pub` before this domain split too, just never
// consumed outside the (then single) file; re-exporting keeps every
// pre-split `pages_user::*` path reachable, so `unused_imports` fires on
// the ones with no current external caller.
#[allow(unused_imports)]
pub use buckets::{
    bucket_list_page, list_buckets_for_user, render_buckets_table, render_new_bucket_modal,
    BucketRow,
};
#[allow(unused_imports)]
pub use cloudstorage::{
    cloudstorage_page, render_quota_card, render_shares_table, QuotaInfo, ShareRow,
};
use maud::{html, Markup, PreEscaped};
#[allow(unused_imports)]
pub use objects::{
    group_objects_by_prefix, object_list_page, render_breadcrumbs, render_objects_table,
    FolderListing, ObjectRow,
};

/// Render the bootstrap JSON in a script tag, escaping `<` to prevent
/// `</script>` sequences from terminating the JSON-typed script element.
/// The escaped `<` (`<`) is valid JSON and decodes back to `<` when
/// the browser reads it via `JSON.parse`.
///
/// Shared by [`objects::object_list_page`] (real bucket + prefix bootstrap)
/// and [`cloudstorage::cloudstorage_page`] (JS-bundle load only, called
/// with empty bucket/prefix).
fn render_bootstrap_script(bucket: &str, current_prefix: &str) -> Markup {
    let bootstrap = serde_json::json!({
        "bucket": bucket,
        "currentPrefix": current_prefix,
    });
    let bootstrap_json = serde_json::to_string(&bootstrap)
        .unwrap_or_else(|_| "{}".to_string())
        .replace('<', "\\u003c");
    let js_url = crate::ui::assets::files_browser_js_url();
    html! {
        script type="application/json" id="files-browser-bootstrap" {
            (PreEscaped(bootstrap_json))
        }
        script src=(js_url) defer {}
    }
}

/// Test-only fixtures shared by more than one domain's integration tests
/// (the classic two-bucket fixture seeds both buckets and objects, so it's
/// used by both `buckets::integration_tests` and `objects::integration_tests`).
#[cfg(test)]
mod test_helpers {
    use std::collections::HashMap;

    use serde_json::json;

    use crate::{blocks::files::repo, test_support::TestContext};

    /// Seed two buckets + two objects in `photos`, none in `docs`.
    pub(super) async fn seed_two_buckets(ctx: &TestContext, owner: &str) {
        for (name, public) in [("photos", true), ("docs", false)] {
            let mut row: HashMap<String, serde_json::Value> = HashMap::new();
            row.insert("name".into(), json!(name));
            row.insert("public".into(), json!(public));
            row.insert("created_by".into(), json!(owner));
            repo::buckets::seed(ctx, row).await.expect("seed bucket");
        }
        for key in ["a.png", "nested/b.png"] {
            let mut row: HashMap<String, serde_json::Value> = HashMap::new();
            row.insert("bucket".into(), json!("photos"));
            row.insert("key".into(), json!(key));
            row.insert("size".into(), json!(1024));
            row.insert("uploaded_by".into(), json!(owner));
            repo::objects::seed(ctx, row).await.expect("seed object");
        }
    }
}
