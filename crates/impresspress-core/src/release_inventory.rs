//! The release asset set: where its objects live ([`RELEASES_ROOT`]), what
//! describes it ([`ReleaseManifest`]), and the key inventory a request-time
//! read resolves through ([`ReleaseInventory`]).
//!
//! One module for both ends on purpose. `impresspress deploy` writes the
//! manifest and the inventory beside the immutable objects it uploads; the
//! Cloudflare Worker re-reads both — the manifest during `/_deploy/verify`'s
//! deep verification, the inventory on every release read — and re-derives the
//! digests bound into the prepared plan from them. Every one of those
//! comparisons is between bytes the deployer produced and bytes the runtime
//! parsed, so the two sides cannot each own a copy of the shape: they used to,
//! and the copies differed in exactly the field (`deny_unknown_fields`) that
//! decides whether an unrecognised manifest fails closed.
//!
//! The one thing that stays on the deploy side is walking a staged directory
//! to produce [`ReleaseAssetEntry`] values — that is filesystem work with no
//! runtime counterpart. Everything downstream of the entries lives here.

use std::{collections::HashSet, sync::Arc};

use serde::{Deserialize, Serialize};
use wafer_core::interfaces::storage::service::{StorageError, StorageService};

use crate::isolate_cell::IsolateCell;

/// Reserved R2 namespace for immutable, deploy-managed release objects.
///
/// The deployer writes release objects only in this namespace (plus
/// per-Worker-version deployment records). It never lists, deletes, or
/// rewrites mutable logical business/user keys already present in the bucket,
/// and the runtime refuses any release prefix that does not start here.
pub const RELEASES_ROOT: &str = ".impresspress/releases/v1";

/// Schema version of the `manifest.json` object written under a release
/// prefix. Part of the asset-set digest, so a bump changes every prefix.
pub const RELEASE_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// One published release object: the logical key a page requests, and the
/// bytes' identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseAssetEntry {
    pub logical_key: String,
    pub size: u64,
    pub sha256: String,
    pub content_type: String,
}

/// Deterministic identity and inventory of one release asset set.
///
/// `asset_set_sha256` hashes the compact JSON encoding of `schema_version` and
/// the sorted `files` array. It deliberately excludes timestamps, repository
/// paths, and the derived immutable prefix, so equal bytes at equal logical
/// keys produce the same identity on every machine.
///
/// `deny_unknown_fields` here and on [`ReleaseAssetEntry`] is load-bearing on
/// the *reading* side: a manifest carrying a field this build does not know
/// about was written under a different release contract, and `/_deploy/verify`
/// must fail rather than silently verify a document it only half-understands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u32,
    pub asset_set_sha256: String,
    pub immutable_prefix: String,
    pub files: Vec<ReleaseAssetEntry>,
}

/// Exactly the bytes `asset_set_sha256` is taken over. Private, so the digest
/// has one definition rather than one per side of the deploy.
#[derive(Serialize)]
struct ManifestIdentity<'a> {
    schema_version: u32,
    files: &'a [ReleaseAssetEntry],
}

/// What can go wrong deriving a manifest's identity. A real error type rather
/// than a `String` because both ends propagate it into their own: the deployer
/// into `anyhow`, the Worker into `Box<dyn Error>`.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseManifestError {
    #[error("serialize {what}: {source}")]
    Serialize {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("construct prepared release-asset identity: {0}")]
    PreparedIdentity(#[from] crate::prepared_plan::PreparedPlanError),
}

impl ReleaseManifest {
    /// Build the manifest for a set of published entries, sorting them into
    /// the strictly-ascending logical-key order the digest is defined over and
    /// deriving the immutable prefix from that digest.
    ///
    /// Sorting here rather than at the caller is what makes the runtime's
    /// `logical_keys_strictly_sorted()` check meaningful: the invariant is
    /// established once, at construction, and re-checked on parse.
    pub fn from_entries(mut files: Vec<ReleaseAssetEntry>) -> Result<Self, ReleaseManifestError> {
        files.sort_by(|left, right| left.logical_key.cmp(&right.logical_key));
        let asset_set_sha256 = asset_set_digest(RELEASE_MANIFEST_SCHEMA_VERSION, &files)?;
        let immutable_prefix = format!("{RELEASES_ROOT}/{asset_set_sha256}");
        Ok(Self {
            schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
            asset_set_sha256,
            immutable_prefix,
            files,
        })
    }

    pub fn manifest_key(&self) -> String {
        format!("{}/manifest.json", self.immutable_prefix)
    }

    pub fn keys_key(&self) -> String {
        format!("{}/keys.json", self.immutable_prefix)
    }

    pub fn immutable_key(&self, logical_key: &str) -> String {
        format!("{}/{logical_key}", self.immutable_prefix)
    }

    /// The exact bytes written to `{prefix}/manifest.json`, digested by
    /// [`Self::manifest_sha256`] and re-read by `/_deploy/verify`.
    pub fn to_pretty_json(&self) -> Result<Vec<u8>, ReleaseManifestError> {
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(|source| ReleaseManifestError::Serialize {
                what: "release manifest",
                source,
            })?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Compact, sorted exact-key representation bound into the Worker version.
    /// The runtime uses exact membership to redirect release reads without
    /// touching business/user keys.
    pub fn logical_keys_json(&self) -> Result<String, ReleaseManifestError> {
        serde_json::to_string(&self.logical_keys()).map_err(|source| {
            ReleaseManifestError::Serialize {
                what: "release asset key set",
                source,
            }
        })
    }

    pub fn logical_keys(&self) -> Vec<&str> {
        self.files
            .iter()
            .map(|entry| entry.logical_key.as_str())
            .collect()
    }

    /// Whether `files` is in the strictly ascending logical-key order the
    /// asset-set digest is defined over. False for a duplicated key too, which
    /// is why this is not merely `is_sorted`.
    pub fn logical_keys_strictly_sorted(&self) -> bool {
        self.files
            .windows(2)
            .all(|pair| pair[0].logical_key < pair[1].logical_key)
    }

    /// Re-derive `asset_set_sha256` from the parsed entries. `/_deploy/verify`
    /// compares this against the stored field, so a manifest cannot vouch for
    /// itself.
    /// Re-derives from `self.schema_version`, not from
    /// [`RELEASE_MANIFEST_SCHEMA_VERSION`]: the whole point is to hash what
    /// was parsed. `/_deploy/verify` refuses a version mismatch before
    /// reaching here, so today the two are always equal — but this is a public
    /// method on a public type, and handed a manifest from a future contract
    /// it must derive that manifest's digest and disagree, not silently derive
    /// a digest for a document nobody wrote.
    pub fn recomputed_asset_set_sha256(&self) -> Result<String, ReleaseManifestError> {
        asset_set_digest(self.schema_version, &self.files)
    }

    pub fn canonical_asset_set_sha256(&self) -> String {
        format!("sha256:{}", self.asset_set_sha256)
    }

    pub fn manifest_sha256(&self) -> Result<String, ReleaseManifestError> {
        Ok(format!(
            "sha256:{}",
            crate::util::sha256_hex(&self.to_pretty_json()?)
        ))
    }

    pub fn logical_keys_sha256(&self) -> Result<String, ReleaseManifestError> {
        Ok(format!(
            "sha256:{}",
            crate::util::sha256_hex(self.logical_keys_json()?.as_bytes())
        ))
    }

    pub fn prepared_identity(&self) -> Result<crate::PreparedReleaseAssets, ReleaseManifestError> {
        crate::PreparedReleaseAssets::present(
            self.canonical_asset_set_sha256(),
            self.immutable_prefix.clone(),
            self.manifest_key(),
            self.manifest_sha256()?,
            self.logical_keys_sha256()?,
        )
        .map_err(ReleaseManifestError::PreparedIdentity)
    }
}

fn asset_set_digest(
    schema_version: u32,
    files: &[ReleaseAssetEntry],
) -> Result<String, ReleaseManifestError> {
    let canonical = serde_json::to_vec(&ManifestIdentity {
        schema_version,
        files,
    })
    .map_err(|source| ReleaseManifestError::Serialize {
        what: "release asset identity",
        source,
    })?;
    Ok(crate::util::sha256_hex(&canonical))
}

/// Checks if a logical key is normalized (no empty components, no `.` or `..`, no leading/trailing slash, no backslash).
/// Shared by both CLI and runtime inventory validation.
pub fn is_normalized_logical_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('/')
        && !key.ends_with('/')
        && !key.contains('\\')
        && key
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

/// Parsed release-key inventory fetched from `{prefix}/keys.json`.
/// Digest-checked against the Worker-version-pinned sha before parsing so a
/// tampered or truncated object fails closed, never downgrading a release
/// read to a mutable object at the same key.
pub struct ReleaseInventory {
    logical_keys: HashSet<String>,
    managed_prefixes: HashSet<String>,
}

impl ReleaseInventory {
    pub fn from_json_bytes(bytes: &[u8], expected_sha256: &str) -> Result<Self, String> {
        let actual = format!("sha256:{}", crate::util::sha256_hex(bytes));
        if expected_sha256 != actual {
            return Err(format!(
                "release asset key inventory digest mismatch: expected {expected_sha256}, got {actual}"
            ));
        }
        let keys: Vec<String> = serde_json::from_slice(bytes)
            .map_err(|e| format!("asset key inventory is not a JSON string array: {e}"))?;
        if keys.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("asset key inventory must be strictly sorted and unique".into());
        }
        let mut managed_prefixes = HashSet::new();
        for key in &keys {
            if !is_normalized_logical_key(key) {
                return Err(format!("asset key is not normalized: {key:?}"));
            }
            let mut end = 0;
            for component in key.split('/') {
                if end + component.len() == key.len() {
                    break; // final component is the object name, not a folder
                }
                end += component.len();
                managed_prefixes.insert(key[..end].to_string());
                end += 1; // the '/'
            }
        }
        Ok(Self {
            logical_keys: keys.into_iter().collect(),
            managed_prefixes,
        })
    }

    pub fn contains(&self, logical_key: &str) -> bool {
        self.logical_keys.contains(logical_key)
    }

    pub fn manages_folder(&self, folder: &str) -> bool {
        self.managed_prefixes.contains(folder)
    }

    pub fn logical_keys_sorted(&self) -> Vec<&str> {
        let mut keys: Vec<_> = self.logical_keys.iter().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }
}

thread_local! {
    /// Digest-keyed parsed inventory; survives across requests in a wasm
    /// isolate (thread-per-isolate) and per-thread in native tests. An
    /// `IsolateCell`, not a `RefCell`: a platform hard stop does not run
    /// destructors, and dropping the previous inventory inside a held borrow
    /// is exactly the wedge `isolate_cell` exists to prevent.
    static RELEASE_INVENTORY_CACHE: IsolateCell<(String, Arc<ReleaseInventory>)> =
        const { IsolateCell::new() };
}

/// Fetch the release key inventory object once per isolate and
/// digest-verify it. Any failure is a hard error — release reads must
/// never silently fall back to mutable objects.
pub async fn load_release_inventory(
    keys_folder: &str,
    keys_name: &str,
    expected_sha256: &str,
    storage: &dyn StorageService,
) -> Result<Arc<ReleaseInventory>, StorageError> {
    if let Some((key, inventory)) = RELEASE_INVENTORY_CACHE.with(IsolateCell::get) {
        if key == expected_sha256 {
            return Ok(inventory);
        }
    }
    let (bytes, _) = storage
        .get(keys_folder, keys_name)
        .await
        .map_err(|error| StorageError::Internal(format!("fetch release key inventory: {error}")))?;
    let inventory = Arc::new(
        ReleaseInventory::from_json_bytes(&bytes, expected_sha256)
            .map_err(StorageError::Internal)?,
    );
    RELEASE_INVENTORY_CACHE.with(|slot| {
        slot.set((expected_sha256.to_string(), inventory.clone()));
    });
    Ok(inventory)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory_json(keys: &[&str]) -> (Vec<u8>, String) {
        let bytes = serde_json::to_vec(keys).unwrap();
        let sha = format!("sha256:{}", crate::util::sha256_hex(&bytes));
        (bytes, sha)
    }

    fn entry(logical_key: &str, body: &[u8]) -> ReleaseAssetEntry {
        ReleaseAssetEntry {
            logical_key: logical_key.to_string(),
            size: body.len() as u64,
            sha256: crate::util::sha256_hex(body),
            content_type: "text/plain".into(),
        }
    }

    /// The immutable prefix is the one place the deploy-time writer and the
    /// request-time reader have to agree on a byte-for-byte string. They used
    /// to agree by both spelling `.impresspress/releases/v1` in their own
    /// crate.
    #[test]
    fn the_immutable_prefix_is_the_releases_root_and_the_asset_set_digest() {
        let manifest =
            ReleaseManifest::from_entries(vec![entry("public/app.css", b"body{}")]).unwrap();

        assert_eq!(
            manifest.immutable_prefix,
            format!("{RELEASES_ROOT}/{}", manifest.asset_set_sha256)
        );
        assert_eq!(
            manifest.immutable_key("public/app.css"),
            format!("{}/public/app.css", manifest.immutable_prefix)
        );
        assert_eq!(
            manifest.manifest_key(),
            format!("{}/manifest.json", manifest.immutable_prefix)
        );
    }

    /// What `/_deploy/verify` re-derives from an R2 object must be what the
    /// deployer computed from the local files, and the entries must come back
    /// in the strictly sorted order the digest was taken over.
    #[test]
    fn a_written_manifest_reparses_to_the_same_asset_set_identity() {
        let manifest = ReleaseManifest::from_entries(vec![
            entry("public/app.css", b"body{}"),
            entry("content/index.md", b"# hi"),
        ])
        .unwrap();

        let bytes = manifest.to_pretty_json().unwrap();
        let reparsed: ReleaseManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reparsed, manifest);
        assert_eq!(
            reparsed.recomputed_asset_set_sha256().unwrap(),
            manifest.asset_set_sha256
        );
        assert!(reparsed.logical_keys_strictly_sorted());
        assert_eq!(
            reparsed.logical_keys(),
            vec!["content/index.md", "public/app.css"]
        );
    }

    /// The recompute hashes the *parsed* schema version. A manifest written
    /// under a later contract must fail to vouch for itself here rather than
    /// be handed a digest derived under this build's version — which is what
    /// hashing the constant would produce, and which would look like agreement
    /// only because the verify endpoint happens to reject the version first.
    #[test]
    fn recomputing_follows_the_parsed_schema_version_not_this_builds_constant() {
        let manifest =
            ReleaseManifest::from_entries(vec![entry("public/app.css", b"body{}")]).unwrap();
        assert_eq!(
            manifest.recomputed_asset_set_sha256().unwrap(),
            manifest.asset_set_sha256
        );

        let future = ReleaseManifest {
            schema_version: RELEASE_MANIFEST_SCHEMA_VERSION + 1,
            ..manifest
        };
        assert_ne!(
            future.recomputed_asset_set_sha256().unwrap(),
            future.asset_set_sha256,
            "a manifest from a later contract must not be handed this \
             contract's digest"
        );
    }

    /// `deny_unknown_fields` came from the Worker's half of the twin and is
    /// load-bearing: a manifest carrying a field this build does not know was
    /// written under a different release contract, and verification must fail
    /// rather than ignore it. Merging the twins must not drop it.
    #[test]
    fn a_manifest_from_a_contract_this_build_does_not_know_is_refused() {
        let manifest =
            ReleaseManifest::from_entries(vec![entry("public/app.css", b"body{}")]).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&manifest.to_pretty_json().unwrap()).unwrap();

        value["compression"] = serde_json::json!("brotli");
        assert!(serde_json::from_value::<ReleaseManifest>(value.clone()).is_err());

        value.as_object_mut().unwrap().remove("compression");
        value["files"][0]["encoding"] = serde_json::json!("br");
        assert!(serde_json::from_value::<ReleaseManifest>(value).is_err());
    }

    #[test]
    fn inventory_parses_and_answers_membership_and_folders() {
        let (bytes, sha) = inventory_json(&["gdsf/site/media/hero.webp", "public/app.css"]);
        let inv = ReleaseInventory::from_json_bytes(&bytes, &sha).unwrap();
        assert!(inv.contains("gdsf/site/media/hero.webp"));
        assert!(!inv.contains("gdsf/site/media/upload.webp"));
        assert!(inv.manages_folder("gdsf/site"));
        assert!(inv.manages_folder("gdsf/site/media"));
        assert!(inv.manages_folder("public"));
        assert!(!inv.manages_folder("uploads"));
        assert!(!inv.manages_folder("gdsf/site/media/hero.webp")); // a key, not a folder
        assert_eq!(
            inv.logical_keys_sorted(),
            vec!["gdsf/site/media/hero.webp", "public/app.css"]
        );
    }

    #[test]
    fn inventory_rejects_digest_mismatch_unsorted_duplicates_and_bad_keys() {
        let (bytes, sha) = inventory_json(&["a/b.css"]);
        assert!(ReleaseInventory::from_json_bytes(&bytes, "sha256:0000").is_err());
        let mut tampered = bytes;
        tampered.push(b' ');
        assert!(ReleaseInventory::from_json_bytes(&tampered, &sha).is_err());

        let (unsorted, unsorted_sha) = inventory_json(&["b.css", "a.css"]);
        assert!(ReleaseInventory::from_json_bytes(&unsorted, &unsorted_sha).is_err());
        let (dup, dup_sha) = inventory_json(&["a.css", "a.css"]);
        assert!(ReleaseInventory::from_json_bytes(&dup, &dup_sha).is_err());
        let (bad, bad_sha) = inventory_json(&["../escape.css"]);
        assert!(ReleaseInventory::from_json_bytes(&bad, &bad_sha).is_err());
        let (not_array, na_sha) = {
            let bytes = br#"{"keys":[]}"#.to_vec();
            let sha = format!("sha256:{}", crate::util::sha256_hex(&bytes));
            (bytes, sha)
        };
        assert!(ReleaseInventory::from_json_bytes(&not_array, &na_sha).is_err());
    }

    struct FakeKeysStorage {
        bytes: Vec<u8>,
        gets: std::sync::atomic::AtomicUsize,
    }

    #[wafer_block::wafer_async_trait]
    impl StorageService for FakeKeysStorage {
        async fn put(
            &self,
            _folder: &str,
            _key: &str,
            _data: &[u8],
            _content_type: &str,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn put_streaming(
            &self,
            _folder: &str,
            _key: &str,
            _data: wafer_block::InputStream,
            _content_type: &str,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn get(
            &self,
            _folder: &str,
            key: &str,
        ) -> Result<
            (
                Vec<u8>,
                wafer_core::interfaces::storage::service::ObjectInfo,
            ),
            StorageError,
        > {
            assert_eq!(key, "keys.json");
            self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok((
                self.bytes.clone(),
                wafer_core::interfaces::storage::service::ObjectInfo {
                    key: key.to_string(),
                    size: self.bytes.len() as i64,
                    content_type: "application/json".to_string(),
                    last_modified: chrono::Utc::now(),
                },
            ))
        }

        async fn get_streaming(
            &self,
            _folder: &str,
            _key: &str,
        ) -> Result<
            (
                wafer_block::OutputStream,
                wafer_core::interfaces::storage::service::ObjectInfo,
            ),
            StorageError,
        > {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn delete(&self, _folder: &str, _key: &str) -> Result<(), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn list(
            &self,
            _folder: &str,
            _opts: &wafer_core::interfaces::storage::service::ListOptions,
        ) -> Result<wafer_core::interfaces::storage::service::ObjectList, StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn delete_folder(&self, _name: &str) -> Result<(), StorageError> {
            Err(StorageError::Internal("unsupported in test".into()))
        }

        async fn list_folders(
            &self,
        ) -> Result<Vec<wafer_core::interfaces::storage::service::FolderInfo>, StorageError>
        {
            Err(StorageError::Internal("unsupported in test".into()))
        }
    }

    #[test]
    fn inventory_loads_once_per_digest_and_fails_closed_on_mismatch() {
        futures::executor::block_on(async {
            let (bytes, sha) = inventory_json(&["public/app.css"]);
            let storage = FakeKeysStorage {
                bytes,
                gets: std::sync::atomic::AtomicUsize::new(0),
            };

            let first = load_release_inventory("some/prefix", "keys.json", &sha, &storage)
                .await
                .unwrap();
            assert!(first.contains("public/app.css"));
            let _second = load_release_inventory("some/prefix", "keys.json", &sha, &storage)
                .await
                .unwrap();
            assert_eq!(
                storage.gets.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "second load must hit the isolate cache"
            );

            let tampered = FakeKeysStorage {
                bytes: b"[\"public/evil.css\"]".to_vec(),
                gets: std::sync::atomic::AtomicUsize::new(0),
            };
            // Distinct digest so this case can't reuse the cache slot the
            // first assertions populated (or a slot any other test in this
            // thread might have populated).
            let other_sha = format!("sha256:{}", "ab".repeat(32));
            assert!(
                load_release_inventory("some/prefix", "keys.json", &other_sha, &tampered)
                    .await
                    .is_err()
            );
        });
    }
}
