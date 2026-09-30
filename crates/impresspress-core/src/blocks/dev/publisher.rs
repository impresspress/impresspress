//! The site publisher: turning a generation's site manifest into the files
//! `wafer-run/web` serves.
//!
//! # Why the order matters
//!
//! The published folder is read by browsers *while* it is being written —
//! there is no atomic swap of an object store — so a publish is only as safe
//! as its ordering. `index.html` is the entrypoint: it is what names the
//! stylesheet, the script and the images of the version it belongs to. If it
//! were written first, every request landing between that write and the last
//! asset write would get the new document referencing assets that are still
//! the old ones (or, for a newly added asset, are not there at all).
//!
//! So the publisher writes in four passes: the deletions that a write in this
//! same publish would otherwise collide with, every changed non-entrypoint
//! file, the remaining deletions, then `index.html`. A reader in the middle of
//! the window sees the *previous* document with assets that are already the
//! new ones — which is only a stale page, and only until the entrypoint lands.
//!
//! Deletions sit before the entrypoint for the same reason and are *not*
//! reordered with it: a file the new manifest dropped is a file the new
//! `index.html` does not reference.
//!
//! # Why some deletions jump the queue
//!
//! The published folder is hierarchical: `blog` is a file or a directory and
//! never both. Within one workspace that clash cannot arise —
//! `workspace::Workspace::path_collision` refuses it on the way in — but
//! *across* generations it can, because the two paths never coexist:
//!
//! ```text
//! generation 3   site/blog/index.html      `blog` is a directory
//! generation 4   site/blog                 `blog` is a file
//! ```
//!
//! Publishing generation 4 in path order would write the file `blog` while the
//! directory `blog` is still there, and the backend answers with a type
//! mismatch. The publish fails, the generation is marked `Failed`, the
//! published manifest never advances — and the next publish attempts exactly
//! the same pair, forever.
//!
//! A colliding deletion therefore runs before the write it collides with. This
//! is the *only* reordering, and it is safe on the reader argument above:
//! those two paths are the same name, so the old file was already unreachable
//! the moment the new one was meant to exist.
//!
//! The backend must also drop a directory the deletions have emptied
//! (`bridge.js::storageDelete` prunes upward), or the emptied `blog` would
//! still be occupying the name.
//!
//! # Why only changed files
//!
//! Every entry names a content-addressed blob, so "unchanged" is exact —
//! same path, same sha — and re-uploading the whole site on every keystroke
//! would make a one-line edit cost as much as the site is big.

use std::collections::BTreeMap;

use wafer_core::clients::storage;
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{blobs, contracts::SiteManifest, workspace::FileEntry};

/// The cross-block folder the published site lives in.
///
/// `wafer-run/web` owns it; the dev block reaches it under the one WRAP grant
/// [`super::wrap_grants`] hands the runtime.
pub const SITE_FOLDER: &str = "@wafer-run/web/site";

/// The document a site is entered through, and the last file a publish writes.
pub const ENTRYPOINT: &str = "index.html";

/// Publish `next`, given the manifest that is currently published.
///
/// `prev` is `None` for the first publish, which therefore writes everything.
///
/// Returns every path it wrote or removed, in the order it touched them — the
/// site-relative spelling the published folder uses (`index.html`, not
/// `site/index.html`). That is what an activation announces to the page
/// (design §2.6), and an unchanged file is absent from it by construction.
pub async fn publish_site(
    ctx: &dyn Context,
    prev: Option<&SiteManifest>,
    next: &SiteManifest,
) -> Result<Vec<String>, WaferError> {
    let prev_by_path = by_path(prev.map(|m| m.files.as_slice()).unwrap_or_default());
    let next_by_path = by_path(&next.files);

    let (colliding, rest): (Vec<&str>, Vec<&str>) = prev_by_path
        .keys()
        .filter(|path| !next_by_path.contains_key(*path))
        .copied()
        .partition(|path| collides_with_any(path, &next_by_path));

    let mut touched = Vec::new();

    // 1. Deletions that stand in the way of a write below.
    for path in &colliding {
        remove(ctx, path).await?;
        touched.push(path.to_string());
    }

    // 2. Every changed non-entrypoint file.
    for (path, entry) in &next_by_path {
        if *path == ENTRYPOINT || is_unchanged(&prev_by_path, path, entry) {
            continue;
        }
        write(ctx, entry).await?;
        touched.push(path.to_string());
    }

    // 3. The remaining files the new manifest no longer holds.
    for path in &rest {
        remove(ctx, path).await?;
        touched.push(path.to_string());
    }

    // 4. The entrypoint, last.
    if let Some(entry) = next_by_path.get(ENTRYPOINT) {
        if !is_unchanged(&prev_by_path, ENTRYPOINT, entry) {
            write(ctx, entry).await?;
            touched.push(ENTRYPOINT.to_string());
        }
    }
    Ok(touched)
}

/// Whether `removed` shares a name with any path the new manifest holds — one
/// being a directory prefix of the other, in either direction.
///
/// The two are never equal here: `removed` is a path the new manifest does not
/// have.
fn collides_with_any(removed: &str, next_by_path: &BTreeMap<&str, &FileEntry>) -> bool {
    // A path being written that lives under `removed/`: `removed` is a file
    // in the old manifest and a directory in the new one.
    let as_dir = format!("{removed}/");
    if next_by_path
        .range(as_dir.as_str()..)
        .next()
        .is_some_and(|(path, _)| path.starts_with(&as_dir))
    {
        return true;
    }
    // A path being written that `removed` lives under: `removed` is a
    // directory in the old manifest and a file in the new one.
    let mut cursor = removed;
    while let Some((parent, _)) = cursor.rsplit_once('/') {
        if next_by_path.contains_key(parent) {
            return true;
        }
        cursor = parent;
    }
    false
}

/// Write one manifest entry into the published folder, reading its bytes from
/// the blob the entry names.
async fn write(ctx: &dyn Context, entry: &FileEntry) -> Result<(), WaferError> {
    let bytes = blobs::get(ctx, &entry.sha256).await?;
    storage::put(ctx, SITE_FOLDER, &entry.path, &bytes, &entry.content_type).await
}

/// Remove one path from the published folder.
///
/// A path that is already gone is the outcome asked for: an interrupted
/// publish may have deleted it, and converging on that state must not fail.
async fn remove(ctx: &dyn Context, path: &str) -> Result<(), WaferError> {
    match storage::delete(ctx, SITE_FOLDER, path).await {
        Err(e) if e.code == ErrorCode::NotFound => Ok(()),
        other => other,
    }
}

/// Whether `entry` is already published at `path` with the same content.
fn is_unchanged(prev: &BTreeMap<&str, &FileEntry>, path: &str, entry: &FileEntry) -> bool {
    prev.get(path)
        .is_some_and(|before| before.sha256 == entry.sha256)
}

fn by_path(files: &[FileEntry]) -> BTreeMap<&str, &FileEntry> {
    files.iter().map(|f| (f.path.as_str(), f)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{blocks::dev::test_support::FakeControl, test_support::TestContext};

    /// Store `content` as a blob and describe it as a manifest entry.
    async fn entry(ctx: &TestContext, path: &str, content: &[u8]) -> FileEntry {
        let (sha, _stored) = blobs::put(ctx, content).await.expect("put blob");
        FileEntry {
            path: path.to_string(),
            sha256: sha,
            size: content.len() as u64,
            content_type: crate::blocks::dev::paths::content_type_for(path).to_string(),
        }
    }

    async fn published(ctx: &TestContext, key: &str) -> Option<Vec<u8>> {
        ctx.storage_get("wafer-run/web", "site", key).await.ok()
    }

    /// Only the `@wafer-run/web/site` writes, in order, without the blob
    /// reads and workspace writes the fixture also records.
    fn site_ops(ctx: &TestContext) -> Vec<String> {
        ctx.storage_ops()
            .into_iter()
            .filter(|op| op.contains("wafer-run/web/site/"))
            .collect()
    }

    #[tokio::test]
    async fn the_first_publish_writes_every_file() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let next = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"<h1>hi</h1>").await,
                entry(&ctx, "a.css", b"a{}").await,
            ],
        };
        publish_site(&ctx, None, &next).await.expect("publish");
        assert_eq!(
            published(&ctx, "index.html").await.as_deref(),
            Some(&b"<h1>hi</h1>"[..])
        );
        assert_eq!(published(&ctx, "a.css").await.as_deref(), Some(&b"a{}"[..]));
    }

    /// The ordering contract, and the reason it is a separate assertion from
    /// "the files are there": the final state is identical whichever order
    /// the publisher used.
    #[tokio::test]
    async fn the_entrypoint_is_written_after_everything_else() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        // `a.css` sorts before `index.html`, `z.css` after — so a publisher
        // that simply iterated the manifest in path order would write
        // `index.html` before `z.css` and fail this.
        let next = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"<h1>hi</h1>").await,
                entry(&ctx, "a.css", b"a{}").await,
                entry(&ctx, "z.css", b"z{}").await,
            ],
        };
        let touched = publish_site(&ctx, None, &next).await.expect("publish");
        // What it reports touching is what it touched, in the same order.
        assert_eq!(touched, ["a.css", "z.css", "index.html"]);
        assert_eq!(
            site_ops(&ctx),
            vec![
                "put wafer-run/web/site/a.css",
                "put wafer-run/web/site/z.css",
                "put wafer-run/web/site/index.html",
            ]
        );
    }

    /// A deletion is a change to what the previous `index.html` referenced, so
    /// it lands before the new entrypoint, not after it.
    #[tokio::test]
    async fn deletions_land_before_the_new_entrypoint() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let prev = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"one").await,
                entry(&ctx, "gone.css", b"g{}").await,
            ],
        };
        publish_site(&ctx, None, &prev).await.expect("publish");
        let next = SiteManifest {
            files: vec![entry(&ctx, "index.html", b"two").await],
        };
        let touched = publish_site(&ctx, Some(&prev), &next)
            .await
            .expect("republish");

        // A removed path is reported as well as a written one.
        assert_eq!(touched, ["gone.css", "index.html"]);
        assert_eq!(
            site_ops(&ctx)[2..],
            [
                "delete wafer-run/web/site/gone.css",
                "put wafer-run/web/site/index.html",
            ]
        );
        assert!(published(&ctx, "gone.css").await.is_none());
        assert_eq!(
            published(&ctx, "index.html").await.as_deref(),
            Some(&b"two"[..])
        );
    }

    /// Re-publishing an unchanged manifest must touch nothing: the whole
    /// point of content addressing is that "same sha" is exact.
    #[tokio::test]
    async fn an_unchanged_file_is_not_rewritten() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let prev = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"one").await,
                entry(&ctx, "a.css", b"a{}").await,
            ],
        };
        publish_site(&ctx, None, &prev).await.expect("publish");
        let before = site_ops(&ctx).len();

        // Same content at `a.css`, new content at the entrypoint.
        let next = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"two").await,
                entry(&ctx, "a.css", b"a{}").await,
            ],
        };
        let touched = publish_site(&ctx, Some(&prev), &next)
            .await
            .expect("republish");
        assert_eq!(
            site_ops(&ctx)[before..],
            ["put wafer-run/web/site/index.html"]
        );
        assert_eq!(touched, ["index.html"], "an unchanged file is not reported");
    }

    /// A path that was a directory in the previous generation and is a file in
    /// this one: the deletion under it has to land before the write, or the
    /// backend refuses the write and the publisher wedges for good.
    #[tokio::test]
    async fn a_deletion_that_frees_a_name_for_a_write_lands_first() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let prev = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"one").await,
                entry(&ctx, "blog/post.html", b"p").await,
            ],
        };
        publish_site(&ctx, None, &prev).await.expect("publish");
        let before = site_ops(&ctx).len();

        // `blog` stops being a directory and becomes a file.
        let next = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"two").await,
                entry(&ctx, "blog", b"b").await,
            ],
        };
        publish_site(&ctx, Some(&prev), &next)
            .await
            .expect("republish");
        assert_eq!(
            site_ops(&ctx)[before..],
            [
                "delete wafer-run/web/site/blog/post.html",
                "put wafer-run/web/site/blog",
                "put wafer-run/web/site/index.html",
            ]
        );
    }

    /// The reverse direction: a file becomes a directory.
    #[tokio::test]
    async fn a_file_that_becomes_a_directory_is_deleted_before_the_children_land() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let prev = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"one").await,
                entry(&ctx, "blog", b"b").await,
            ],
        };
        publish_site(&ctx, None, &prev).await.expect("publish");
        let before = site_ops(&ctx).len();

        let next = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"two").await,
                entry(&ctx, "blog/post.html", b"p").await,
            ],
        };
        publish_site(&ctx, Some(&prev), &next)
            .await
            .expect("republish");
        assert_eq!(
            site_ops(&ctx)[before..],
            [
                "delete wafer-run/web/site/blog",
                "put wafer-run/web/site/blog/post.html",
                "put wafer-run/web/site/index.html",
            ]
        );
    }

    /// A deletion that collides with nothing keeps its place AFTER the writes
    /// — that ordering is what keeps a reader in the publish window on a stale
    /// page rather than a broken one.
    #[tokio::test]
    async fn an_unrelated_deletion_still_lands_after_the_writes() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let prev = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"one").await,
                entry(&ctx, "gone.css", b"g{}").await,
            ],
        };
        publish_site(&ctx, None, &prev).await.expect("publish");
        let before = site_ops(&ctx).len();

        let next = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"two").await,
                entry(&ctx, "new.css", b"n{}").await,
            ],
        };
        publish_site(&ctx, Some(&prev), &next)
            .await
            .expect("republish");
        assert_eq!(
            site_ops(&ctx)[before..],
            [
                "put wafer-run/web/site/new.css",
                "delete wafer-run/web/site/gone.css",
                "put wafer-run/web/site/index.html",
            ]
        );
    }

    /// Converging after an interrupted publish must not fail on a file the
    /// interrupted run had already removed.
    #[tokio::test]
    async fn removing_a_file_that_is_already_gone_succeeds() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let prev = SiteManifest {
            files: vec![entry(&ctx, "gone.css", b"g{}").await],
        };
        // `prev` was never actually published, so the delete pass finds
        // nothing — exactly the state a half-finished publish leaves.
        publish_site(&ctx, Some(&prev), &SiteManifest::default())
            .await
            .expect("publish");
    }

    /// A manifest naming a blob that is not stored is corruption, and must
    /// surface rather than publish a partial site quietly.
    #[tokio::test]
    async fn a_missing_blob_fails_the_publish() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let next = SiteManifest {
            files: vec![FileEntry {
                path: "a.css".to_string(),
                sha256: blobs::sha256_hex(b"never stored"),
                size: 3,
                content_type: "text/css; charset=utf-8".to_string(),
            }],
        };
        assert_eq!(
            publish_site(&ctx, None, &next)
                .await
                .expect_err("must fail")
                .code,
            ErrorCode::NotFound
        );
    }
}
