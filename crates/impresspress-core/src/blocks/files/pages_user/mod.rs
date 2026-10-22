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
//! Callers reach a page through the domain module that owns it
//! (`pages_user::buckets::bucket_list_page`, …). There is no flat re-export
//! layer: it would have to name every item whether or not anything consumes
//! it, and the names with no consumer would be invisible.

pub(crate) mod buckets;
pub(crate) mod cloudstorage;
pub(crate) mod objects;

use maud::{html, Markup, PreEscaped};

/// Render the bootstrap JSON in a script tag, escaping `<` through
/// [`crate::ui::script_json`] so a `</script>` sequence cannot terminate the
/// JSON-typed script element early. That helper is where the reasoning lives;
/// this used to spell the same `replace` out by hand.
///
/// Shared by [`objects::object_list_page`] (real bucket + prefix bootstrap)
/// and [`cloudstorage::cloudstorage_page`] (JS-bundle load only, called
/// with empty bucket/prefix).
fn render_bootstrap_script(bucket: &str, current_prefix: &str) -> Markup {
    let bootstrap_json = crate::ui::script_json(&serde_json::json!({
        "bucket": bucket,
        "currentPrefix": current_prefix,
    }));
    let js_url = crate::blocks::files::assets::files_browser_js_url();
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
