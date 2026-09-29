//! The workspace manifest — which paths exist and which blob each one names.
//!
//! One JSON object at `workspace.json` in the block's own storage namespace.
//! It is the *editable* state: the sandbox's files API reads and writes it,
//! and a generation is a frozen projection of it (design §11.3), never the
//! other way round.
//!
//! The manifest holds no content. Every entry names a blob by sha, so
//! replacing a file rewrites one small JSON document rather than moving bytes,
//! and two paths holding the same content cost one copy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use wafer_core::clients::storage;
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::paths::content_type_for;

/// Storage key the manifest lives under, in the block's own namespace
/// (folder `""`, which `wafer-run/storage` resolves to `impresspress/dev`).
pub const KEY: &str = "workspace.json";

/// Storage folder the manifest lives in: the block's namespace root.
pub const FOLDER: &str = "";

/// Content type the manifest is stored under.
const MANIFEST_CONTENT_TYPE: &str = "application/json";

/// The `site/` area's path prefix, including its separator.
pub const SITE_PREFIX: &str = "site/";

/// The `blocks/` area's path prefix, including its separator.
pub const BLOCKS_PREFIX: &str = "blocks/";

/// One file in the workspace: where it is, which blob holds it, how big it is
/// and what it is served as.
///
/// The same type is what a generation's site manifest is made of — a
/// generation *is* the workspace's `site/` entries, frozen — so there is one
/// definition rather than a wire type and a stored type that have to be kept
/// in step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// Where the file lives. Workspace-relative (`site/index.html`) in the
    /// files API; relative to its area's root (`index.html`) in a
    /// generation's site manifest and in a block's source listing.
    pub path: String,
    /// SHA-256 of the file's content-addressed blob, hex-encoded.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
    /// Content type the file is served with.
    pub content_type: String,
}

/// Every file the workspace holds, plus what its blob store has cost.
///
/// `files` is a `BTreeMap` for two reasons that are both load-bearing:
/// iteration is in path order, so every projection below is sorted without
/// sorting; and serialization is key-ordered, so the stored manifest is
/// byte-for-byte deterministic for a given workspace.
///
/// # Why the blob totals live here
///
/// `files` describes what is *reachable*; it says nothing about what is
/// *stored*. Content is never edited in place, so overwriting one 512 KiB
/// page two hundred times leaves two hundred blobs behind — 100 MB of
/// storage that `files` still reports as one 512 KiB entry. The quota that
/// matters (design §6.6, "≤ 64 MiB of blobs per workspace") is on the store,
/// so the store's size is tracked here rather than recomputed from `files`,
/// which cannot see the difference.
///
/// Both counters have exactly two writers. Between collections,
/// [`Workspace::record_blob_stored`] charges each blob a write adds to the
/// store; each collection then sets them to what the store actually holds
/// ([`Workspace::reset_blob_totals`], from `super::gc`'s listing). The charge
/// is what keeps the quota honest between collections, and the reset is what
/// keeps it honest across a charge that was lost — a blob stored by a write
/// whose manifest save then failed — which no write-side bookkeeping can see.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Entries by path. The key and [`FileEntry::path`] are always equal —
    /// [`Workspace::insert`] is the only thing that writes either.
    #[serde(default)]
    pub files: BTreeMap<String, FileEntry>,
    /// Total bytes of blobs in this workspace's store, including blobs no
    /// `files` entry names any more. This is what
    /// [`super::paths::MAX_WORKSPACE_BYTES`] bounds.
    #[serde(default)]
    pub blob_bytes: u64,
    /// How many blobs those bytes are spread over.
    #[serde(default)]
    pub blob_count: u32,
}

impl Workspace {
    /// Record `sha256`/`size` at `path`, deriving the content type from the
    /// path, and return the entry as stored.
    ///
    /// The single writer of `files`, which is what keeps the map key and
    /// [`FileEntry::path`] from drifting apart.
    pub fn insert(&mut self, path: &str, sha256: String, size: u64) -> FileEntry {
        let entry = FileEntry {
            path: path.to_string(),
            sha256,
            size,
            content_type: content_type_for(path).to_string(),
        };
        self.files.insert(path.to_string(), entry.clone());
        entry
    }

    /// The entry at `path`, if any.
    pub fn get(&self, path: &str) -> Option<&FileEntry> {
        self.files.get(path)
    }

    /// Drop the entry at `path`, returning it when it was there.
    ///
    /// The blob it named is deliberately left alone — an older generation may
    /// still reference it (see [`super::blobs`]).
    pub fn remove(&mut self, path: &str) -> Option<FileEntry> {
        self.files.remove(path)
    }

    /// Total size of the files the workspace can currently reach, in bytes.
    ///
    /// NOT the quota: superseded and deleted content is still stored and still
    /// costs, and only [`Self::blob_bytes`] counts it. This is the "how big is
    /// my site" number, not the "how much have I used" one.
    pub fn total_bytes(&self) -> u64 {
        self.files.values().map(|entry| entry.size).sum()
    }

    /// Whether any entry names the blob `sha`.
    ///
    /// A referenced blob is necessarily still stored — the collector only
    /// reclaims unreachable ones — so this answers "is this content already
    /// paid for?" without a storage round trip.
    pub fn references(&self, sha: &str) -> bool {
        self.files.values().any(|entry| entry.sha256 == sha)
    }

    /// Charge the workspace for a blob of `bytes` that was just written.
    ///
    /// Call this only when the store actually grew — a deduplicated write
    /// ([`super::blobs::Stored::Deduplicated`]) stored nothing and must not be
    /// charged, or two paths holding one asset would count it twice.
    pub fn record_blob_stored(&mut self, bytes: u64) {
        self.blob_bytes = self.blob_bytes.saturating_add(bytes);
        self.blob_count = self.blob_count.saturating_add(1);
    }

    /// Set both counters to what the blob store holds, and say whether that
    /// changed them.
    ///
    /// For `super::gc` only, which counts `bytes` and `count` off its own
    /// listing of the store after it has deleted what it collects. A reset,
    /// never a subtraction of what was freed: a blob the write path failed to
    /// charge is freed like any other, and subtracting its size from a total
    /// that never included it would leave the workspace under-counted for
    /// good.
    pub fn reset_blob_totals(&mut self, bytes: u64, count: u32) -> bool {
        let changed = self.blob_bytes != bytes || self.blob_count != count;
        self.blob_bytes = bytes;
        self.blob_count = count;
        changed
    }

    /// The existing path that stops `path` from being stored, if any.
    ///
    /// # Why a set of paths can be invalid when every path in it is valid
    ///
    /// [`super::paths::validate_path`] judges one path on its own, which is
    /// all it can do — but the workspace is written to a *hierarchical* store,
    /// where a name is a file or a directory and never both. Two individually
    /// legal paths can therefore disagree about one name:
    ///
    /// ```text
    /// site/blog/index.html   `blog` is a directory
    /// site/blog              `blog` is a file
    /// ```
    ///
    /// Whichever is written second fails inside the storage backend — OPFS
    /// answers `getFileHandle(…, {create: true})` on a directory (and
    /// `getDirectoryHandle` on a file) with `TypeMismatchError`, and a native
    /// filesystem answers `IsADirectory`. That failure arrives at *publish*
    /// time, long after `workspace.json` accepted both paths, and it recurs on
    /// every later publish: the generation is marked `Failed`, so the
    /// published manifest never advances and the next publish attempts the
    /// same pair of writes. The sandbox wedges permanently, with a message
    /// that names a storage type mismatch rather than the two files that
    /// caused it.
    ///
    /// This is the same reasoning that puts the [`super::paths::META_SUFFIX`]
    /// rule in `validate_path` rather than in the backend, applied to the one
    /// shape a single path cannot reveal. Refusing it on the way in is what
    /// makes it a 400 naming both paths instead.
    ///
    /// Returns the offending existing path, in either direction: `path` is a
    /// directory prefix of something stored, or something stored is a
    /// directory prefix of `path`.
    pub fn path_collision(&self, path: &str) -> Option<&str> {
        // `path` used as a directory by an existing entry. `files` is ordered,
        // so the first key at or after `path/` is the only candidate.
        let as_dir = format!("{path}/");
        if let Some((stored, _)) = self.files.range(as_dir.clone()..).next() {
            if stored.starts_with(&as_dir) {
                return Some(stored.as_str());
            }
        }
        // An existing entry used as a directory by `path`. Bounded by path
        // depth, which `validate_path` has already capped.
        let mut cursor = path;
        while let Some((parent, _)) = cursor.rsplit_once('/') {
            if let Some(entry) = self.files.get(parent) {
                return Some(entry.path.as_str());
            }
            cursor = parent;
        }
        None
    }

    /// Every distinct block name the workspace defines sources for, sorted.
    pub fn block_names(&self) -> Vec<String> {
        self.files
            .keys()
            .filter_map(|path| block_name_of(path))
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<String>>()
            .into_iter()
            .collect()
    }
}

/// The `site/` entries, with the prefix stripped, in path order.
///
/// This is the shape a generation's site manifest and the site publisher both
/// want: `site/index.html` in the workspace is `index.html` under the
/// published site root.
pub fn site_manifest(ws: &Workspace) -> Vec<FileEntry> {
    entries_under(ws, SITE_PREFIX)
}

/// The `blocks/<name>/` entries, with the prefix stripped, in path order.
///
/// Stripped for the same reason [`site_manifest`] is: the consumer is a
/// compiler that wants a crate rooted at `Cargo.toml` / `src/lib.rs`, not a
/// tree nested two directories deep. The workspace path is recoverable as
/// `blocks/{name}/{entry.path}`.
pub fn block_sources(ws: &Workspace, name: &str) -> Vec<FileEntry> {
    entries_under(ws, &format!("{BLOCKS_PREFIX}{name}/"))
}

/// The entries whose path starts with `prefix`, with `prefix` removed.
fn entries_under(ws: &Workspace, prefix: &str) -> Vec<FileEntry> {
    ws.files
        .iter()
        .filter_map(|(path, entry)| {
            let relative = path.strip_prefix(prefix)?;
            Some(FileEntry {
                path: relative.to_string(),
                ..entry.clone()
            })
        })
        .collect()
}

/// The block name a `blocks/<name>/…` path belongs to.
fn block_name_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(BLOCKS_PREFIX)?;
    let (name, remainder) = rest.split_once('/')?;
    // A bare `blocks/<name>/` never reaches the workspace (validate_path
    // refuses an area root), but the projection must not invent a block from
    // one if it somehow did.
    (!name.is_empty() && !remainder.is_empty()).then_some(name)
}

/// Read the workspace manifest. A missing manifest is an empty workspace —
/// that is what a fresh instance looks like.
///
/// A manifest that is present but does not parse is an error, never an empty
/// workspace: answering "no files" for a workspace that has some would let the
/// next write save a manifest that had dropped every existing entry.
pub async fn load(ctx: &dyn Context) -> Result<Workspace, WaferError> {
    let bytes = match storage::get(ctx, FOLDER, KEY).await {
        Ok((bytes, _info)) => bytes,
        Err(e) if e.code == ErrorCode::NotFound => return Ok(Workspace::default()),
        Err(e) => return Err(e),
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        WaferError::new(
            ErrorCode::Internal,
            format!("dev workspace manifest ({KEY}) did not parse: {e}"),
        )
    })
}

/// Write the workspace manifest.
///
/// Serialized compact, with the file map in path order (it is a
/// [`BTreeMap`]), so the bytes are deterministic for a given workspace. Note
/// this is *deterministic*, not *canonical*: serde emits a struct's fields in
/// declaration order, not sorted, so `workspace.json` is not the sorted-key
/// form design §11.3 requires of a **generation** manifest. That is fine
/// because this file is never hashed — but a generation manifest must be
/// built through `serde_json::Value` (whose maps sort) rather than serialized
/// straight from a struct, as `dev_status`'s fixture already does.
///
/// Compact rather than pretty because this is rewritten on every single file
/// write.
pub async fn save(ctx: &dyn Context, ws: &Workspace) -> Result<(), WaferError> {
    let bytes = serde_json::to_vec(ws).map_err(|e| {
        WaferError::new(
            ErrorCode::Internal,
            format!("serializing the dev workspace manifest: {e}"),
        )
    })?;
    storage::put(ctx, FOLDER, KEY, &bytes, MANIFEST_CONTENT_TYPE).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{blocks::dev::test_support::FakeControl, test_support::TestContext};

    fn populated() -> Workspace {
        let mut ws = Workspace::default();
        ws.insert("site/z.css", "z".to_string(), 3);
        ws.insert("site/a.css", "a".to_string(), 5);
        ws.insert("site/nested/b.html", "b".to_string(), 7);
        ws.insert("blocks/hello/src/lib.rs", "l".to_string(), 11);
        ws.insert("blocks/hello/Cargo.toml", "c".to_string(), 13);
        ws.insert("blocks/other/src/lib.rs", "o".to_string(), 17);
        ws
    }

    #[test]
    fn insert_derives_the_content_type_and_keys_by_path() {
        let mut ws = Workspace::default();
        let entry = ws.insert("site/index.html", "abc".to_string(), 11);
        assert_eq!(entry.content_type, "text/html; charset=utf-8");
        assert_eq!(ws.get("site/index.html"), Some(&entry));
        // The map key and the entry's own path can never disagree.
        for (key, entry) in &ws.files {
            assert_eq!(key, &entry.path);
        }
    }

    #[test]
    fn site_manifest_strips_the_prefix_and_comes_out_sorted() {
        let ws = populated();
        let files = site_manifest(&ws);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.css", "nested/b.html", "z.css"]);
        // Everything else on the entry survives the projection.
        assert_eq!(files[0].sha256, "a");
        assert_eq!(files[0].size, 5);
        assert_eq!(files[0].content_type, "text/css; charset=utf-8");
    }

    #[test]
    fn block_sources_are_scoped_to_one_block_and_rooted_at_its_crate() {
        let ws = populated();
        let paths: Vec<String> = block_sources(&ws, "hello")
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(paths, vec!["Cargo.toml", "src/lib.rs"]);
        assert!(block_sources(&ws, "missing").is_empty());
        // A name that is a prefix of another must not pull in its files.
        assert!(block_sources(&ws, "hell").is_empty());
    }

    #[test]
    fn totals_and_block_names_are_derived_from_the_entries() {
        let ws = populated();
        assert_eq!(ws.total_bytes(), 3 + 5 + 7 + 11 + 13 + 17);
        assert_eq!(ws.block_names(), vec!["hello", "other"]);
        assert!(Workspace::default().block_names().is_empty());
        assert_eq!(Workspace::default().total_bytes(), 0);
    }

    #[test]
    fn remove_drops_the_entry_and_reports_what_it_dropped() {
        let mut ws = populated();
        let removed = ws.remove("site/a.css").expect("entry was there");
        assert_eq!(removed.sha256, "a");
        assert!(ws.get("site/a.css").is_none());
        assert!(ws.remove("site/a.css").is_none());
    }

    /// The stored bytes are compact and path-ordered, so a given workspace
    /// always serializes identically.
    #[test]
    fn the_manifest_serializes_deterministically() {
        let mut ws = Workspace::default();
        ws.insert("site/z.css", "z".to_string(), 1);
        ws.insert("site/a.css", "a".to_string(), 2);
        ws.record_blob_stored(1);
        ws.record_blob_stored(2);
        let json = serde_json::to_string(&ws).expect("serialize");
        assert_eq!(
            json,
            r#"{"files":{"site/a.css":{"path":"site/a.css","sha256":"a","size":2,"content_type":"text/css; charset=utf-8"},"site/z.css":{"path":"site/z.css","sha256":"z","size":1,"content_type":"text/css; charset=utf-8"}},"blob_bytes":3,"blob_count":2}"#
        );
        assert!(!json.contains('\n'));
    }

    /// A manifest written before the blob counters existed must still load —
    /// it describes a real workspace whose files are still reachable.
    #[test]
    fn a_manifest_without_the_blob_counters_loads_with_them_at_zero() {
        let ws: Workspace = serde_json::from_str(
            r#"{"files":{"site/a.css":{"path":"site/a.css","sha256":"a","size":2,"content_type":"text/css; charset=utf-8"}}}"#,
        )
        .expect("deserialize");
        assert_eq!(ws.files.len(), 1);
        assert_eq!(ws.blob_bytes, 0);
        assert_eq!(ws.blob_count, 0);
    }

    /// The two totals answer different questions, and the quota uses the
    /// second: superseded content is unreachable but still stored.
    #[test]
    fn stored_blob_bytes_outlive_the_entries_that_named_them() {
        let mut ws = Workspace::default();
        ws.insert("site/a.css", "one".to_string(), 100);
        ws.record_blob_stored(100);
        // Overwrite: a new blob, the old one still stored.
        ws.insert("site/a.css", "two".to_string(), 100);
        ws.record_blob_stored(100);
        assert_eq!(ws.total_bytes(), 100);
        assert_eq!(ws.blob_bytes, 200);
        assert_eq!(ws.blob_count, 2);

        // Deleting the entry frees nothing; only the collector does.
        ws.remove("site/a.css");
        assert_eq!(ws.total_bytes(), 0);
        assert_eq!(ws.blob_bytes, 200);

        // The collector freed the first blob and counted what is left.
        assert!(ws.reset_blob_totals(100, 1));
        assert_eq!(ws.blob_bytes, 100);
        assert_eq!(ws.blob_count, 1);
    }

    /// A reset that matches the counters reports no change, which is what
    /// spares the collector a `workspace.json` write on every activation.
    #[test]
    fn a_reset_to_the_same_totals_changes_nothing() {
        let mut ws = Workspace::default();
        ws.record_blob_stored(10);
        assert!(!ws.reset_blob_totals(10, 1));
        assert!(ws.reset_blob_totals(10, 2), "the count alone is a change");
        assert_eq!((ws.blob_bytes, ws.blob_count), (10, 2));
    }

    /// Both directions of the file/directory clash, and the near misses that
    /// must NOT be refused (a shared prefix that stops mid-segment).
    #[test]
    fn path_collision_reports_the_entry_that_claims_the_name() {
        let mut ws = Workspace::default();
        ws.insert("site/blog/index.html", "a".to_string(), 1);
        ws.insert("site/style.css", "b".to_string(), 1);

        // `site/blog` is a directory here, so it cannot also be a file.
        assert_eq!(ws.path_collision("site/blog"), Some("site/blog/index.html"));
        // `site` too, however deep the existing entry is.
        assert_eq!(ws.path_collision("site"), Some("site/blog/index.html"));
        // The reverse: `site/style.css` is a file, so nothing lives under it.
        assert_eq!(
            ws.path_collision("site/style.css/extra"),
            Some("site/style.css")
        );
        assert_eq!(
            ws.path_collision("site/style.css/a/b"),
            Some("site/style.css")
        );

        // A path that merely shares a textual prefix is fine — the clash is
        // about whole segments, which is why the check appends the separator.
        assert!(ws.path_collision("site/blogroll").is_none());
        assert!(ws.path_collision("site/style.css.map").is_none());
        // A sibling, and the entry itself (an overwrite is not a collision).
        assert!(ws.path_collision("site/blog/about.html").is_none());
        assert!(ws.path_collision("site/blog/index.html").is_none());
        assert!(Workspace::default()
            .path_collision("site/anything")
            .is_none());
    }

    #[test]
    fn references_reports_whether_any_entry_names_a_blob() {
        let mut ws = Workspace::default();
        ws.insert("site/a.html", "shared".to_string(), 1);
        ws.insert("site/b.html", "shared".to_string(), 1);
        assert!(ws.references("shared"));
        assert!(!ws.references("other"));
        ws.remove("site/a.html");
        assert!(ws.references("shared"), "the other path still names it");
        ws.remove("site/b.html");
        assert!(!ws.references("shared"));
    }

    #[tokio::test]
    async fn a_missing_manifest_loads_as_an_empty_workspace() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        assert_eq!(load(&ctx).await.expect("load"), Workspace::default());
    }

    #[tokio::test]
    async fn the_manifest_round_trips_through_storage() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let ws = populated();
        save(&ctx, &ws).await.expect("save");
        assert_eq!(load(&ctx).await.expect("load"), ws);
    }

    /// A corrupt manifest must not read as "no files": the next write would
    /// then save a manifest that had silently dropped every entry.
    #[tokio::test]
    async fn a_manifest_that_does_not_parse_is_an_error_not_an_empty_workspace() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        storage::put(&ctx, FOLDER, KEY, b"{ not json", MANIFEST_CONTENT_TYPE)
            .await
            .expect("put");
        assert_eq!(
            load(&ctx).await.expect_err("load must fail").code,
            ErrorCode::Internal
        );
    }
}
