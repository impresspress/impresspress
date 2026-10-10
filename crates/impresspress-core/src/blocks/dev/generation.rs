//! The generation manifest — a frozen projection of the workspace, and the
//! thing the ledger stores, hashes and diffs.
//!
//! A generation *is* the workspace's `site/` entries plus the active block set
//! at one instant (design §11.3). The manifest is never edited: every change
//! produces a new one, and rolling back republishes an old one's contents
//! under a new id.
//!
//! # Canonical JSON
//!
//! Everything stored is canonical JSON — object keys sorted, no whitespace —
//! because `manifest_sha256` is a hash over exactly those bytes. A manifest
//! serialized straight from a struct is *deterministic* (serde emits fields in
//! declaration order) but not *canonical*, and the two stop agreeing the
//! moment a field is reordered in the source. [`canonical_text`] therefore
//! routes every value through [`serde_json::Value`] and rebuilds each object
//! from a [`BTreeMap`], so the bytes depend on the data alone.
//!
//! [`super::repo::json_text`] normalizes the same way on the way back out, so
//! a round trip through any backend returns the bytes the hash was taken over.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{
    blobs::sha256_hex,
    contracts::{GenerationSummary, SiteManifest},
    control::DynamicBlockSpec,
    repo::{self, generations::GenerationRow, runtime_state::RuntimeState},
    workspace::FileEntry,
};

/// Manifest schema version (design §11.3). Stamped on every manifest this
/// build writes; a stored manifest that declares a different one is a manifest
/// from another build of the block.
pub const SCHEMA_VERSION: u32 = 1;

/// One generation's complete manifest.
///
/// The two halves are stored in separate columns (`site_manifest_json`,
/// `block_manifest_json`) because the ledger indexes and reads them
/// separately; the whole manifest exists as one value because that is what
/// `manifest_sha256` covers and what the generations API publishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationManifest {
    /// Manifest schema version. `1` for every manifest this build writes.
    pub schema_version: u32,
    /// The generation this manifest describes.
    pub generation_id: String,
    /// The generation this one was derived from, or null for the first.
    pub parent_id: Option<String>,
    /// The files the generation publishes.
    pub site: SiteManifest,
    /// The blocks the generation's runtime is built from.
    pub blocks: Vec<DynamicBlockSpec>,
}

impl GenerationManifest {
    /// A manifest that has not been staged yet: it describes a desired state,
    /// but has no place in the ledger's history until one is assigned.
    ///
    /// `generation_id` and `parent_id` are deliberately not caller-supplied.
    /// The id is minted when the row is inserted and the parent is whatever
    /// was active *at that moment* — which is not the same as what was active
    /// when the caller built the manifest, because an earlier activation may
    /// still have been in flight. Letting a caller pass them in is how a
    /// coalesced request ends up recording the wrong parent.
    pub fn staged(site: SiteManifest, blocks: Vec<DynamicBlockSpec>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            generation_id: String::new(),
            parent_id: None,
            site,
            blocks,
        }
    }

    /// Place the manifest in the ledger's history.
    ///
    /// Called once, immediately before the row is inserted, so the hash the
    /// row stores covers the identity the row has.
    pub fn identify(&mut self, generation_id: String, parent_id: Option<String>) {
        self.generation_id = generation_id;
        self.parent_id = parent_id;
    }
}

// ---------------------------------------------------------------------------
// Canonical JSON
// ---------------------------------------------------------------------------

/// `value` as canonical JSON: object keys sorted at every depth, no
/// whitespace.
///
/// Used for the manifest itself and for each half as stored, so there is one
/// definition of "canonical" rather than one per column.
pub fn canonical_text<T: Serialize + ?Sized>(value: &T) -> Result<String, WaferError> {
    let value = serde_json::to_value(value).map_err(|e| {
        WaferError::new(
            ErrorCode::Internal,
            format!("serializing a generation manifest: {e}"),
        )
    })?;
    Ok(canonicalize(value).to_string())
}

/// The canonical bytes of a whole manifest — what [`manifest_sha256`] hashes.
pub fn canonical_json(manifest: &GenerationManifest) -> Result<Vec<u8>, WaferError> {
    canonical_text(manifest).map(String::into_bytes)
}

/// SHA-256 of [`canonical_json`], hex-encoded.
pub fn manifest_sha256(manifest: &GenerationManifest) -> Result<String, WaferError> {
    Ok(sha256_hex(&canonical_json(manifest)?))
}

/// Rebuild every object in `value` from a [`BTreeMap`], recursively.
///
/// `serde_json`'s `Map` is already `BTreeMap`-backed in this graph, so this is
/// a no-op *today* — and that is exactly why it is written out. Enabling
/// `serde_json/preserve_order` anywhere in the workspace (a feature any
/// dependency can turn on, for the whole graph) would silently switch `Map` to
/// an insertion-ordered `IndexMap`, and every stored `manifest_sha256` would
/// stop matching the manifest it was taken over. Re-inserting in sorted order
/// keeps the bytes a function of the data under either backing map.
///
/// `pub(super)` because [`super::repo::json_text`] canonicalizes the same
/// bytes on the way back out of the database. "Canonical JSON" is one contract
/// — the one `manifest_sha256` is taken over — so it has one implementation;
/// two of them differing in robustness is a hash check that passes on the way
/// in and fails on the way out.
pub(super) fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalize).collect()),
        scalar => scalar,
    }
}

// ---------------------------------------------------------------------------
// Diffs
// ---------------------------------------------------------------------------

/// What changed between two generations.
///
/// Paths are site paths (relative to the site root); blocks are block names.
/// A "changed" block is one whose whole [`DynamicBlockSpec`] differs — not
/// merely its artifact — because a route or capability change needs the same
/// runtime rebuild an artifact change does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationDiff {
    /// Site paths the generation adds.
    pub added_paths: Vec<String>,
    /// Site paths whose content hash changed.
    pub changed_paths: Vec<String>,
    /// Site paths the generation drops.
    pub removed_paths: Vec<String>,
    /// Blocks the generation adds.
    pub added_blocks: Vec<String>,
    /// Blocks whose spec changed.
    pub changed_blocks: Vec<String>,
    /// Blocks the generation drops.
    pub removed_blocks: Vec<String>,
}

impl GenerationDiff {
    /// Whether the block set differs, and so whether the runtime has to be
    /// rebuilt.
    pub fn blocks_changed(&self) -> bool {
        !self.added_blocks.is_empty()
            || !self.changed_blocks.is_empty()
            || !self.removed_blocks.is_empty()
    }
}

/// Compare two generations. `prev` is `None` for the first generation, which
/// therefore adds everything it holds.
pub fn diff(prev: Option<&GenerationManifest>, next: &GenerationManifest) -> GenerationDiff {
    let prev_files = by_path(prev.map(|m| m.site.files.as_slice()).unwrap_or_default());
    let next_files = by_path(&next.site.files);
    let prev_blocks = by_name(prev.map(|m| m.blocks.as_slice()).unwrap_or_default());
    let next_blocks = by_name(&next.blocks);

    let mut diff = GenerationDiff::default();
    for (path, entry) in &next_files {
        match prev_files.get(path) {
            None => diff.added_paths.push((*path).to_string()),
            Some(before) if before.sha256 != entry.sha256 => {
                diff.changed_paths.push((*path).to_string());
            }
            Some(_) => {}
        }
    }
    for path in prev_files.keys() {
        if !next_files.contains_key(path) {
            diff.removed_paths.push((*path).to_string());
        }
    }
    for (name, spec) in &next_blocks {
        match prev_blocks.get(name) {
            None => diff.added_blocks.push((*name).to_string()),
            Some(before) if before != spec => diff.changed_blocks.push((*name).to_string()),
            Some(_) => {}
        }
    }
    for name in prev_blocks.keys() {
        if !next_blocks.contains_key(name) {
            diff.removed_blocks.push((*name).to_string());
        }
    }
    diff
}

/// Whether activating `next` over `prev` changes the block set, and so needs a
/// runtime rebuild rather than a site-only republish (design §7.2).
///
/// Derived from [`diff`] rather than computed separately: two definitions of
/// "the block set changed" is one definition too many, and the one that
/// decides whether to rebuild is the one that must not drift.
pub fn block_set_changed(prev: Option<&GenerationManifest>, next: &GenerationManifest) -> bool {
    diff(prev, next).blocks_changed()
}

fn by_path(files: &[FileEntry]) -> BTreeMap<&str, &FileEntry> {
    files.iter().map(|f| (f.path.as_str(), f)).collect()
}

fn by_name(blocks: &[DynamicBlockSpec]) -> BTreeMap<&str, &DynamicBlockSpec> {
    blocks.iter().map(|b| (b.name.as_str(), b)).collect()
}

/// Every distinct blob a manifest's site half names, deduplicated.
pub fn site_blob_shas(manifest: &GenerationManifest) -> BTreeSet<&str> {
    manifest
        .site
        .files
        .iter()
        .map(|entry| entry.sha256.as_str())
        .collect()
}

// ---------------------------------------------------------------------------
// Ledger rows ↔ manifests
// ---------------------------------------------------------------------------

/// Rebuild the manifest a ledger row stores.
///
/// The row holds the two halves plus the identity, so for a row this version
/// wrote the reconstruction is exact: [`manifest_sha256`] of the result
/// equals the row's stored hash.
///
/// A row an earlier version wrote can still carry a `content_type` on each
/// site entry, and its stored hash covers that key. The key is dropped on the
/// way in (see [`super::stored_types`]), so for such a row the result denotes
/// the same files and blocks but hashes differently from what the row
/// stores. The boot upgrade rewrites every such row with a hash recomputed
/// over the manifest without the key; until then, and for as long as the
/// active row is held back because republishing its site failed, the two
/// disagree.
pub fn from_row(row: &GenerationRow) -> Result<GenerationManifest, WaferError> {
    Ok(GenerationManifest {
        schema_version: SCHEMA_VERSION,
        generation_id: row.id.clone(),
        parent_id: row.parent_id.clone(),
        site: manifest_field(&row.id, "site_manifest_json", &row.site_manifest_json)?,
        blocks: manifest_field(&row.id, "block_manifest_json", &row.block_manifest_json)?,
    })
}

/// Project a row and its manifest into the summary every API publishes.
///
/// `manifest` is `None` for a row whose manifest columns do not parse
/// ([`from_row`] refused them): the ledger still lists it, with its counts
/// unknown, rather than one unreadable row failing the whole listing.
pub fn summarize(row: &GenerationRow, manifest: Option<&GenerationManifest>) -> GenerationSummary {
    GenerationSummary {
        id: row.id.clone(),
        parent_id: row.parent_id.clone(),
        cause: row.cause,
        status: row.status,
        created_at: row.created_at.clone(),
        activated_at: row.activated_at.clone(),
        failure_message: row.failure_message.clone(),
        site_files: manifest.map(|m| m.site.files.len() as u32),
        blocks: manifest.map(|m| m.blocks.len() as u32),
    }
}

/// Load one generation and its manifest.
pub async fn load(
    ctx: &dyn Context,
    id: &str,
) -> Result<(GenerationRow, GenerationManifest), WaferError> {
    let row = repo::generations::get(ctx, id).await?;
    let manifest = from_row(&row)?;
    Ok((row, manifest))
}

/// The generation the journal says is live, with its manifest, or `None` on a
/// fresh instance.
///
/// The journal — not the `status` column — is what says which generation is
/// serving; the column records the row's own lifecycle.
pub async fn active(
    ctx: &dyn Context,
) -> Result<Option<(GenerationRow, GenerationManifest)>, WaferError> {
    let state = repo::runtime_state::read(ctx).await?;
    active_from(ctx, &state).await
}

/// [`active`] for a caller that has already read the journal.
///
/// The status endpoint is polled every ~300 ms while a tool call is in flight
/// and needs the phase as well as the active generation; reading the
/// single-row journal twice per poll to answer one question is the duplication
/// this exists to avoid.
pub async fn active_from(
    ctx: &dyn Context,
    state: &RuntimeState,
) -> Result<Option<(GenerationRow, GenerationManifest)>, WaferError> {
    match state.active_generation_id.as_deref() {
        Some(id) => load(ctx, id).await.map(Some),
        None => Ok(None),
    }
}

/// Decode one stored manifest column.
///
/// A column that does not parse is an error, never a default: reporting "no
/// blocks" for a generation that has some would tell the page to drop tool
/// registrations that are still live, and reporting "no files" would publish
/// an empty site.
fn manifest_field<T: serde::de::DeserializeOwned>(
    generation_id: &str,
    column: &str,
    json: &str,
) -> Result<T, WaferError> {
    serde_json::from_str(json).map_err(|e| {
        WaferError::new(
            ErrorCode::Internal,
            format!("generation {generation_id}: {column} did not parse: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::dev::{
        control::{DynamicRoute, RouteAccessKind},
        WAFER_GUEST_VERSION,
    };

    fn file(path: &str, sha: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            sha256: sha.to_string(),
            size: 3,
        }
    }

    fn spec(name: &str, artifact: &str) -> DynamicBlockSpec {
        DynamicBlockSpec {
            name: name.to_string(),
            artifact_sha256: artifact.to_string(),
            routes: vec![DynamicRoute {
                prefix: format!("/b/{name}/"),
                access: RouteAccessKind::Public,
            }],
            capabilities: wafer_block::BlockCapabilities::default(),
            wafer_guest_version: WAFER_GUEST_VERSION,
        }
    }

    fn manifest(files: Vec<FileEntry>, blocks: Vec<DynamicBlockSpec>) -> GenerationManifest {
        let mut manifest = GenerationManifest::staged(SiteManifest { files }, blocks);
        manifest.identify("g1".to_string(), None);
        manifest
    }

    #[test]
    fn a_staged_manifest_carries_no_identity_until_it_is_given_one() {
        let mut staged = GenerationManifest::staged(SiteManifest::default(), Vec::new());
        assert_eq!(staged.schema_version, SCHEMA_VERSION);
        assert_eq!(staged.generation_id, "");
        assert_eq!(staged.parent_id, None);
        staged.identify("g2".to_string(), Some("g1".to_string()));
        assert_eq!(staged.generation_id, "g2");
        assert_eq!(staged.parent_id.as_deref(), Some("g1"));
    }

    /// Sorted keys at every depth, no whitespace — the form design §11.3
    /// mandates and `manifest_sha256` is taken over.
    #[test]
    fn canonical_json_sorts_every_object_and_emits_no_whitespace() {
        let manifest = manifest(vec![file("index.html", "aa")], Vec::new());
        let text = String::from_utf8(canonical_json(&manifest).expect("canonical")).expect("utf8");
        assert_eq!(
            text,
            r#"{"blocks":[],"generation_id":"g1","parent_id":null,"schema_version":1,"site":{"files":[{"path":"index.html","sha256":"aa","size":3}]}}"#
        );
        // No whitespace *between* tokens — the only space in the output is
        // inside the `text/html; charset=utf-8` value itself.
        assert!(!text.contains('\n'));
        assert!(!text.contains(": "));
        assert!(!text.contains(", "));
    }

    /// The recursion is the point: a nested object arriving key-shuffled must
    /// come out sorted, or a hash taken over it would depend on how the value
    /// happened to be built.
    #[test]
    fn canonicalize_sorts_nested_objects_and_leaves_arrays_in_order() {
        let messy = serde_json::json!({
            "b": [{"y": 1, "x": {"n": 2, "m": 3}}, {"a": 1}],
            "a": 1,
        });
        assert_eq!(
            canonicalize(messy).to_string(),
            r#"{"a":1,"b":[{"x":{"m":3,"n":2},"y":1},{"a":1}]}"#
        );
    }

    /// The hash is a function of the manifest's content, not of its identity's
    /// spelling: two manifests differing only in id must hash differently, and
    /// the same manifest must hash the same every time.
    #[test]
    fn the_hash_covers_the_whole_manifest_including_its_identity() {
        let one = manifest(vec![file("index.html", "aa")], Vec::new());
        let same = manifest(vec![file("index.html", "aa")], Vec::new());
        assert_eq!(
            manifest_sha256(&one).expect("hash"),
            manifest_sha256(&same).expect("hash")
        );

        let mut other = same;
        other.identify("g2".to_string(), None);
        assert_ne!(
            manifest_sha256(&one).expect("hash"),
            manifest_sha256(&other).expect("hash")
        );

        let content_changed = manifest(vec![file("index.html", "bb")], Vec::new());
        assert_ne!(
            manifest_sha256(&one).expect("hash"),
            manifest_sha256(&content_changed).expect("hash")
        );
        assert_eq!(manifest_sha256(&one).expect("hash").len(), 64);
    }

    #[test]
    fn the_first_generation_adds_everything_it_holds() {
        let next = manifest(vec![file("index.html", "aa")], vec![spec("site/x", "bb")]);
        let diff = diff(None, &next);
        assert_eq!(diff.added_paths, vec!["index.html"]);
        assert_eq!(diff.added_blocks, vec!["site/x"]);
        assert!(diff.changed_paths.is_empty() && diff.removed_paths.is_empty());
        assert!(block_set_changed(None, &next));
    }

    #[test]
    fn a_diff_separates_added_changed_and_removed_paths() {
        let prev = manifest(
            vec![file("a.css", "aa"), file("gone.css", "cc")],
            Vec::new(),
        );
        let next = manifest(
            vec![file("a.css", "aa2"), file("new.css", "dd")],
            Vec::new(),
        );
        let diff = diff(Some(&prev), &next);
        assert_eq!(diff.changed_paths, vec!["a.css"]);
        assert_eq!(diff.added_paths, vec!["new.css"]);
        assert_eq!(diff.removed_paths, vec!["gone.css"]);
        // A site-only change must not ask for a runtime rebuild.
        assert!(!diff.blocks_changed());
        assert!(!block_set_changed(Some(&prev), &next));
    }

    /// The rebuild decision has to see more than the artifact hash: a route or
    /// capability change is a different runtime even with identical bytes.
    #[test]
    fn a_block_whose_routes_changed_counts_as_changed() {
        let prev = manifest(Vec::new(), vec![spec("site/x", "bb")]);
        let mut rerouted = spec("site/x", "bb");
        rerouted.routes[0].access = RouteAccessKind::Admin;
        let next = manifest(Vec::new(), vec![rerouted]);
        assert_eq!(diff(Some(&prev), &next).changed_blocks, vec!["site/x"]);
        assert!(block_set_changed(Some(&prev), &next));

        // Identical specs are not a change.
        let same = manifest(Vec::new(), vec![spec("site/x", "bb")]);
        assert!(!block_set_changed(Some(&prev), &same));
    }

    #[test]
    fn a_removed_block_changes_the_block_set() {
        let prev = manifest(Vec::new(), vec![spec("site/x", "bb")]);
        let next = manifest(Vec::new(), Vec::new());
        assert_eq!(diff(Some(&prev), &next).removed_blocks, vec!["site/x"]);
        assert!(block_set_changed(Some(&prev), &next));
    }

    /// A ledger row for `manifest`, its halves and hash written as activation
    /// writes them.
    fn row_for(manifest: &GenerationManifest) -> GenerationRow {
        GenerationRow {
            id: manifest.generation_id.clone(),
            parent_id: manifest.parent_id.clone(),
            status: repo::generations::GenerationStatus::Active,
            cause: repo::generations::GenerationCause::SiteWrite,
            site_manifest_json: canonical_text(&manifest.site).expect("site"),
            block_manifest_json: canonical_text(&manifest.blocks).expect("blocks"),
            manifest_sha256: manifest_sha256(manifest).expect("hash"),
            created_at: "2026-10-10T00:00:00Z".to_string(),
            activated_at: None,
            failure_message: None,
        }
    }

    #[test]
    fn from_row_reproduces_the_stored_hash_of_a_row_this_version_wrote() {
        let manifest = manifest(vec![file("index.html", "aa")], vec![spec("site/x", "bb")]);
        let row = row_for(&manifest);
        let rebuilt = from_row(&row).expect("parses");
        assert_eq!(rebuilt, manifest);
        assert_eq!(
            manifest_sha256(&rebuilt).expect("hash"),
            row.manifest_sha256
        );
    }

    #[test]
    fn from_row_drops_a_legacy_content_type_the_stored_hash_still_covers() {
        let manifest = manifest(vec![file("index.html", "aa")], Vec::new());

        // What an earlier version stored: the same canonical manifest with a
        // `content_type` on each site entry, hashed with it.
        let mut legacy = serde_json::to_value(&manifest).expect("value");
        for entry in legacy["site"]["files"].as_array_mut().expect("files") {
            entry["content_type"] = Value::from("text/html");
        }
        let mut row = row_for(&manifest);
        row.site_manifest_json = canonicalize(legacy["site"].clone()).to_string();
        row.manifest_sha256 = sha256_hex(canonicalize(legacy).to_string().as_bytes());

        let rebuilt = from_row(&row).expect("a legacy row still parses");
        assert_eq!(rebuilt, manifest, "the key is dropped on the way in");
        assert_ne!(
            manifest_sha256(&rebuilt).expect("hash"),
            row.manifest_sha256,
            "until the upgrade rewrites the row, its stored hash covers the key"
        );
    }

    #[test]
    fn site_blob_shas_deduplicates_paths_that_share_content() {
        let manifest = manifest(
            vec![
                file("a.html", "aa"),
                file("b.html", "aa"),
                file("c.html", "bb"),
            ],
            Vec::new(),
        );
        assert_eq!(
            site_blob_shas(&manifest).into_iter().collect::<Vec<_>>(),
            vec!["aa", "bb"]
        );
    }
}
