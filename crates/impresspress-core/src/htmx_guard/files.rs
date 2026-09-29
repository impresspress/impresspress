//! `impresspress/files`: the storage and cloud-storage pages.
//!
//! Every page it serves drives its mutations from `files-browser.js` with
//! `fetch`, not htmx, so its pages carry no mutating htmx control today and
//! `must_fire` is empty. The pages are still rendered, so a control added to
//! one of them is fired from then on.

use std::{collections::HashMap, sync::Arc};

use serde_json::json;
use wafer_run::{Block, Message};

use super::{Entry, Exempt};
use crate::{
    blocks::files::{repo, FilesBlock},
    test_support::{
        admin_msg,
        htmx::{Fixture, Page, Site},
        InMemoryStorageService, TestContext,
    },
};

/// The files block's own download routes: they answer with the object's
/// bytes under its stored content type, not a page.
const DOWNLOAD: Exempt = Exempt::NotAPage("file download (the object's bytes)");

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/files",
        fixture: Some(|| Box::pin(fixture())),
        exempt: &[
            ("/b/storage/api/buckets/{name}/objects/{key...}", DOWNLOAD),
            (
                "/b/storage/direct/{token}",
                Exempt::NotAPage("a share link's download (the object's bytes)"),
            ),
        ],
        // No mutating htmx control on any files page; see the module doc.
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &[],
    }
}

/// The signed-in admin (`admin_1`) owns everything seeded, so the user pages
/// and the admin pages render the same rows.
fn caller(action: &str, path: &str) -> Message {
    admin_msg(action, path)
}

/// Two buckets, two objects (one under a folder), one share and one quota
/// override, all owned by `admin_1`: enough for every list, the bucket page
/// and the nested folder page to render a row rather than an empty state.
async fn fixture() -> Fixture {
    let mut ctx = TestContext::with_files().await;
    ctx.register_block(
        "wafer-run/storage",
        crate::blocks::storage::create(Arc::new(InMemoryStorageService::new())),
    );

    for (name, public) in [("photos", true), ("docs", false)] {
        let row: HashMap<String, serde_json::Value> = HashMap::from([
            ("name".into(), json!(name)),
            ("public".into(), json!(public)),
            ("created_by".into(), json!("admin_1")),
            ("created_at".into(), json!(crate::util::now_rfc3339())),
        ]);
        repo::buckets::seed(&ctx, row).await.expect("seed bucket");
    }
    for key in ["a.png", "nested/b.png"] {
        let row: HashMap<String, serde_json::Value> = HashMap::from([
            ("bucket".into(), json!("photos")),
            ("key".into(), json!(key)),
            ("size".into(), json!(1024)),
            ("uploaded_by".into(), json!("admin_1")),
        ]);
        repo::objects::seed(&ctx, row).await.expect("seed object");
        // The object's bytes, through the storage block the way the upload
        // route stores them, so a download answers them.
        wafer_core::clients::storage::put(&ctx, "photos", key, b"png", "image/png")
            .await
            .expect("store the object's bytes");
    }
    let share: HashMap<String, serde_json::Value> = HashMap::from([
        ("token".into(), json!("tok123abc")),
        ("bucket".into(), json!("photos")),
        ("key".into(), json!("a.png")),
        ("created_by".into(), json!("admin_1")),
        ("created_at".into(), json!(crate::util::now_rfc3339())),
        ("access_count".into(), json!(0)),
        (
            "expires_at".into(),
            json!((chrono::Utc::now() + chrono::Duration::days(365)).to_rfc3339()),
        ),
    ]);
    repo::shares::seed(&ctx, share).await.expect("seed share");
    let quota: HashMap<String, serde_json::Value> = HashMap::from([
        ("user_id".into(), json!("admin_1")),
        ("max_storage_bytes".into(), json!(1_073_741_824i64)),
    ]);
    repo::quota::seed(&ctx, quota).await.expect("seed quota");

    Fixture {
        ctx,
        site: Site(vec![Arc::new(FilesBlock::new()) as Arc<dyn Block>]),
        caller,
        pages: vec![
            Page::at("/b/storage/admin"),
            Page::at("/b/storage/admin/buckets"),
            Page::at("/b/storage/admin/shares"),
            Page::at("/b/storage/admin/quotas"),
            Page::at("/b/storage/"),
            Page::at("/b/cloudstorage/"),
            Page::at("/b/storage/photos/"),
            Page::at("/b/storage/photos/nested/"),
        ],
        probes: vec![
            (
                "/b/storage/api/buckets/{name}/objects",
                "/b/storage/api/buckets/photos/objects".to_string(),
            ),
            (
                "/b/storage/api/buckets/{name}/objects/{key...}",
                "/b/storage/api/buckets/photos/objects/nested/b.png".to_string(),
            ),
            (
                "/b/storage/api/search",
                "/b/storage/api/search?q=png".to_string(),
            ),
            (
                "/b/storage/direct/{token}",
                "/b/storage/direct/tok123abc".to_string(),
            ),
        ],
        operator_input: &[],
    }
}
