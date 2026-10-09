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
//! # The sandbox's own `llms.txt`
//!
//! One published file is not in any generation's manifest. A sandbox seed
//! carries an `llms.txt` — what the sandbox tells a reader about itself —
//! and the static host serves it at `/llms.txt` to anyone the service worker
//! does not control. Once the worker does control the page, that path is the
//! runtime's like every other site path, and `wafer-run/web` answers a path
//! it has no file for with the site's `index.html`. So the text has to be IN
//! the published folder, and this module puts it there: a site that has no
//! `llms.txt` of its own is published with the sandbox's
//! ([`SandboxLlms`]), and a site that has one is published with its own.
//!
//! It is done here, on the published view, and not by seeding the file into
//! `site/`, because the file is the sandbox's and not the site's: it must not
//! be listed in the workspace, counted against it, or — the case that
//! matters — exported. An export is built from the generation's manifest
//! (`super::export`), which never names it. And it is not done by taking
//! `/llms.txt` away from the runtime (a service-worker bypass), because that
//! would shadow the `site/llms.txt` an agent may well write.
//!
//! The file joins both sides of a publish's diff, so the existing ordering
//! does the rest: writing `site/llms.txt` replaces the sandbox's text as a
//! changed file, and deleting it puts the sandbox's text back.
//!
//! The two sides are not derived the same way, and that is deliberate. What
//! this publish SHOULD leave at `llms.txt` is computed: the sandbox's text,
//! when one is recorded and the site leaves the name free. What an earlier
//! publish DID leave there is read, from the record this module keeps
//! (`seed_info.llms_published_sha256`, written at the end of every publish
//! that changes it). It is not inferred from "a text is recorded", because
//! that is false whenever the text arrived after the last publish — an
//! instance seeded before the sandbox had an `llms.txt` gets one from the
//! boot repair (`seed::repair_llms`), and a publisher that assumed it was
//! already in the folder would find it "unchanged" and never write it. The
//! record is written last, so a publish that fails part-way leaves it
//! saying what was true before; the worst that costs is one rewrite of an
//! identical file.
//!
//! # Why only changed files
//!
//! Every entry names a content-addressed blob, so "unchanged" is exact —
//! same path, same sha — and re-uploading the whole site on every keystroke
//! would make a one-line edit cost as much as the site is big.

use std::collections::BTreeMap;

use wafer_core::clients::storage;
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{blobs, contracts::SiteManifest, repo::seed_info, seed, workspace::FileEntry};

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
    let recorded = seed_info::llms(ctx).await?;
    let sandbox_llms = recorded.text.map(SandboxLlms::new);
    // What is in the folder: `prev`'s files, and the sandbox's `llms.txt` if
    // the record says an earlier publish left one — unless `prev`'s own
    // files hold that name, in which case that publish replaced or removed
    // it and failed before it could say so.
    let mut prev_by_path: BTreeMap<&str, &str> = prev
        .map(|manifest| manifest.files.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();
    if let Some(published) = recorded.published_sha256.as_deref() {
        if name_is_free(seed::LLMS_PATH, &prev_by_path) {
            prev_by_path.insert(seed::LLMS_PATH, published);
        }
    }
    let next_by_path = published_view(&next.files, sandbox_llms.as_ref());

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
        if *path == ENTRYPOINT || is_unchanged(&prev_by_path, path, *entry) {
            continue;
        }
        write(ctx, *entry).await?;
        touched.push(path.to_string());
    }

    // 3. The remaining files the new manifest no longer holds.
    for path in &rest {
        remove(ctx, path).await?;
        touched.push(path.to_string());
    }

    // 4. The entrypoint, last.
    if let Some(entry) = next_by_path.get(ENTRYPOINT) {
        if !is_unchanged(&prev_by_path, ENTRYPOINT, *entry) {
            write(ctx, *entry).await?;
            touched.push(ENTRYPOINT.to_string());
        }
    }

    // Last: say what is at `llms.txt` now, when that changed.
    let published_now = match next_by_path.get(seed::LLMS_PATH) {
        Some(Published::Sandbox(llms)) => Some(llms.entry.sha256.as_str()),
        _ => None,
    };
    if published_now != recorded.published_sha256.as_deref() {
        seed_info::set_llms_published(ctx, published_now).await?;
    }
    Ok(touched)
}

/// Put the sandbox's `llms.txt` where `recorded` and the active generation
/// say it should be, if it is not there.
///
/// The boot repair's second half (`seed::repair_llms`, which read `recorded`
/// and passes it on rather than have it read twice): a text recorded after
/// the last publish is in no folder yet. Returns whether anything was
/// written.
///
/// A record that names this very text settles it without another read: the
/// record is only ever set by a publish that wrote the file because the
/// site left the name free. Otherwise the active generation decides — its
/// site may hold the name itself — and republishing it over itself is a
/// diff of that one file.
pub async fn converge_sandbox_llms(
    ctx: &dyn Context,
    recorded: seed_info::LlmsRow,
) -> Result<bool, WaferError> {
    let Some(llms) = recorded.text.map(SandboxLlms::new) else {
        return Ok(false);
    };
    if recorded.published_sha256.as_deref() == Some(llms.entry.sha256.as_str()) {
        return Ok(false);
    }
    let Some((_, manifest)) = super::generation::active(ctx).await? else {
        return Ok(false);
    };
    let active = &manifest.site;
    let wanted = match published_view(&active.files, Some(&llms)).get(seed::LLMS_PATH) {
        Some(Published::Sandbox(llms)) => Some(llms.entry.sha256.as_str()),
        _ => None,
    };
    if wanted == recorded.published_sha256.as_deref() {
        return Ok(false);
    }
    Ok(!publish_site(ctx, Some(active), active).await?.is_empty())
}

/// Whether the published folder can hold a file at `path` beside `files`:
/// the name is a file or a directory and never both.
fn name_is_free<V>(path: &str, files: &BTreeMap<&str, V>) -> bool {
    !files.contains_key(path) && !collides_with_any(path, files)
}

/// Whether `removed` shares a name with any path the new manifest holds — one
/// being a directory prefix of the other, in either direction.
///
/// The two are never equal here: `removed` is a path the new manifest does not
/// have.
fn collides_with_any<V>(removed: &str, next_by_path: &BTreeMap<&str, V>) -> bool {
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

/// The sandbox's own `llms.txt`, as the seed import recorded it — see the
/// module docs.
struct SandboxLlms {
    /// What it is published as: [`seed::LLMS_PATH`], the hash of `bytes`.
    entry: FileEntry,
    bytes: Vec<u8>,
}

impl SandboxLlms {
    /// From the recorded text. There is none on an instance whose seed
    /// carried no sandbox block — an exported bundle, above all.
    fn new(text: String) -> Self {
        let bytes = text.into_bytes();
        Self {
            entry: FileEntry {
                path: seed::LLMS_PATH.to_string(),
                sha256: blobs::sha256_hex(&bytes),
                size: bytes.len() as u64,
                content_type: seed::llms_content_type().to_string(),
            },
            bytes,
        }
    }
}

/// One file of the published folder, and where its bytes are.
#[derive(Clone, Copy)]
enum Published<'a> {
    /// A file of the generation's site manifest; its bytes are the blob the
    /// entry names.
    Site(&'a FileEntry),
    /// The sandbox's `llms.txt`, which is in no manifest and no blob.
    Sandbox(&'a SandboxLlms),
}

impl Published<'_> {
    fn entry(&self) -> &FileEntry {
        match self {
            Self::Site(entry) => entry,
            Self::Sandbox(llms) => &llms.entry,
        }
    }
}

/// What publishing `files` puts in the folder: the files themselves, plus the
/// sandbox's `llms.txt` when the site leaves that name free.
///
/// "Free" is the published folder's own rule — a name is a file or a
/// directory and never both — so a site with `llms.txt/index.html` keeps the
/// name as well as a site with `llms.txt`.
fn published_view<'a>(
    files: &'a [FileEntry],
    sandbox_llms: Option<&'a SandboxLlms>,
) -> BTreeMap<&'a str, Published<'a>> {
    let mut view: BTreeMap<&str, Published> = files
        .iter()
        .map(|f| (f.path.as_str(), Published::Site(f)))
        .collect();
    if let Some(llms) = sandbox_llms {
        let path = llms.entry.path.as_str();
        if name_is_free(path, &view) {
            view.insert(path, Published::Sandbox(llms));
        }
    }
    view
}

/// Write one file into the published folder.
async fn write(ctx: &dyn Context, file: Published<'_>) -> Result<(), WaferError> {
    let entry = file.entry();
    let stored;
    let bytes = match file {
        Published::Site(entry) => {
            stored = blobs::get(ctx, &entry.sha256).await?;
            &stored
        }
        Published::Sandbox(llms) => &llms.bytes,
    };
    storage::put(ctx, SITE_FOLDER, &entry.path, bytes, &entry.content_type).await
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
fn is_unchanged(prev: &BTreeMap<&str, &str>, path: &str, file: Published<'_>) -> bool {
    prev.get(path)
        .is_some_and(|before| *before == file.entry().sha256)
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

    // -- the sandbox's own llms.txt ------------------------------------------

    const SANDBOX_LLMS: &str = "# The sandbox\n\nBuild a site here.\n";

    /// A dev context whose seed import recorded [`SANDBOX_LLMS`].
    async fn seeded_sandbox() -> TestContext {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        seed_info::write(
            &ctx,
            &seed_info::SeedInfo {
                template: "blank".to_string(),
                suggested_prompt: String::new(),
                guide_markdown: String::new(),
                llms_text: Some(SANDBOX_LLMS.to_string()),
            },
        )
        .await
        .expect("seed info");
        ctx
    }

    /// The case the whole mechanism is for: the worker controls the page, the
    /// site has no `llms.txt`, and `/llms.txt` must still be the sandbox's
    /// text rather than the SPA fallback's `index.html`.
    #[tokio::test]
    async fn a_site_without_llms_txt_is_published_with_the_sandboxes() {
        let ctx = seeded_sandbox().await;
        let next = SiteManifest {
            files: vec![entry(&ctx, "index.html", b"<h1>hi</h1>").await],
        };
        let touched = publish_site(&ctx, None, &next).await.expect("publish");
        assert_eq!(touched, ["llms.txt", "index.html"]);
        assert_eq!(
            published(&ctx, "llms.txt").await.as_deref(),
            Some(SANDBOX_LLMS.as_bytes())
        );

        // And it is not rewritten by a publish that does not concern it.
        let before = site_ops(&ctx).len();
        let again = SiteManifest {
            files: vec![entry(&ctx, "index.html", b"<h1>two</h1>").await],
        };
        publish_site(&ctx, Some(&next), &again)
            .await
            .expect("republish");
        assert_eq!(
            site_ops(&ctx)[before..],
            ["put wafer-run/web/site/index.html"]
        );
    }

    /// An instance with no recorded text — an exported bundle's, whose seed
    /// has no sandbox block — publishes its site and nothing else.
    #[tokio::test]
    async fn an_instance_with_no_sandbox_llms_publishes_only_its_site() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let next = SiteManifest {
            files: vec![entry(&ctx, "index.html", b"<h1>hi</h1>").await],
        };
        let touched = publish_site(&ctx, None, &next).await.expect("publish");
        assert_eq!(touched, ["index.html"]);
        assert!(published(&ctx, "llms.txt").await.is_none());
    }

    /// The site's own file wins the moment it exists, and the sandbox's comes
    /// back the moment it is gone.
    #[tokio::test]
    async fn a_sites_own_llms_txt_replaces_the_sandboxes_and_deleting_it_restores_it() {
        let ctx = seeded_sandbox().await;
        let bare = SiteManifest {
            files: vec![entry(&ctx, "index.html", b"<h1>hi</h1>").await],
        };
        publish_site(&ctx, None, &bare).await.expect("publish");

        let own = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"<h1>hi</h1>").await,
                entry(&ctx, "llms.txt", b"# My shop\n").await,
            ],
        };
        let touched = publish_site(&ctx, Some(&bare), &own)
            .await
            .expect("the site's own");
        assert_eq!(touched, ["llms.txt"]);
        assert_eq!(
            published(&ctx, "llms.txt").await.as_deref(),
            Some(&b"# My shop\n"[..])
        );

        let touched = publish_site(&ctx, Some(&own), &bare)
            .await
            .expect("deleted again");
        assert_eq!(touched, ["llms.txt"]);
        assert_eq!(
            published(&ctx, "llms.txt").await.as_deref(),
            Some(SANDBOX_LLMS.as_bytes())
        );
    }

    /// A site that uses the NAME as a directory keeps it: the sandbox's file
    /// is withdrawn before the children land, like any file-to-directory
    /// change, and is not written over them.
    #[tokio::test]
    async fn a_site_directory_named_llms_txt_displaces_the_sandboxes_file() {
        let ctx = seeded_sandbox().await;
        let bare = SiteManifest {
            files: vec![entry(&ctx, "index.html", b"one").await],
        };
        publish_site(&ctx, None, &bare).await.expect("publish");
        let before = site_ops(&ctx).len();

        let nested = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"one").await,
                entry(&ctx, "llms.txt/index.html", b"nested").await,
            ],
        };
        publish_site(&ctx, Some(&bare), &nested)
            .await
            .expect("republish");
        assert_eq!(
            site_ops(&ctx)[before..],
            [
                "delete wafer-run/web/site/llms.txt",
                "put wafer-run/web/site/llms.txt/index.html",
            ]
        );
    }

    /// A text recorded AFTER the site was published — the boot repair's case
    /// (`tests/dev_seed.rs` drives the repair itself) — is written by the next
    /// publish, although no site file changed: what is
    /// in the folder is read from the record, not assumed from the row.
    #[tokio::test]
    async fn a_text_recorded_after_the_last_publish_is_written_by_the_next_one() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let site = SiteManifest {
            files: vec![entry(&ctx, "index.html", b"<h1>hi</h1>").await],
        };
        publish_site(&ctx, None, &site).await.expect("publish");
        assert!(published(&ctx, "llms.txt").await.is_none());

        seed_info::write(
            &ctx,
            &seed_info::SeedInfo {
                template: "blank".to_string(),
                suggested_prompt: String::new(),
                guide_markdown: String::new(),
                llms_text: None,
            },
        )
        .await
        .expect("a row from before the column");
        let republish = || publish_site(&ctx, Some(&site), &site);
        assert!(republish().await.expect("nothing yet").is_empty());
        seed_info::record_llms_text(&ctx, SANDBOX_LLMS)
            .await
            .expect("record");

        assert_eq!(republish().await.expect("republish"), ["llms.txt"]);
        assert_eq!(
            published(&ctx, "llms.txt").await.as_deref(),
            Some(SANDBOX_LLMS.as_bytes())
        );
        // And once it is there, republishing again writes nothing.
        let before = site_ops(&ctx).len();
        assert!(republish().await.expect("again").is_empty());
        assert_eq!(site_ops(&ctx).len(), before);
    }

    /// The record follows the folder: set when the sandbox's text is
    /// written, cleared when the site's own file takes the name.
    #[tokio::test]
    async fn the_record_says_whether_the_sandboxes_text_is_in_the_folder() {
        let ctx = seeded_sandbox().await;
        let bare = SiteManifest {
            files: vec![entry(&ctx, "index.html", b"<h1>hi</h1>").await],
        };
        publish_site(&ctx, None, &bare).await.expect("publish");
        assert_eq!(
            seed_info::llms(&ctx).await.expect("row").published_sha256,
            Some(blobs::sha256_hex(SANDBOX_LLMS.as_bytes()))
        );
        let own = SiteManifest {
            files: vec![
                entry(&ctx, "index.html", b"<h1>hi</h1>").await,
                entry(&ctx, "llms.txt", b"# My shop\n").await,
            ],
        };
        publish_site(&ctx, Some(&bare), &own).await.expect("own");
        assert_eq!(
            seed_info::llms(&ctx).await.expect("row").published_sha256,
            None
        );
        // …and republishing a site with its own file touches nothing.
        assert!(publish_site(&ctx, Some(&own), &own)
            .await
            .expect("republish")
            .is_empty());
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
