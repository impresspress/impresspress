//! Stage static assets from a consumer's dist/ + content/ + public/ and an
//! optional explicitly configured release-assets directory into
//! `target/impresspress-cloudflare/assets/` for later R2 upload.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
// The release asset set's shape is shared with the runtime that reads it back
// — see `impresspress_core::release_inventory`'s module doc. Only the
// directory walk that turns staged files into entries is deploy-side, and it
// is `release_manifest_from_staged_dir` below.
pub use impresspress_core::release_inventory::{
    ReleaseAssetEntry, ReleaseManifest, RELEASES_ROOT, RELEASE_MANIFEST_SCHEMA_VERSION,
};
use impresspress_core::{routing::STATIC_PREFIX, ui::assets as ui_assets};

/// Source dirs (relative to repo_root) to mirror into out_dir/assets/.
const ASSET_DIRS: &[&str] = &["dist", "content", "public"];

/// Where a deployed worker should fetch UI assets (CSS/JS/fonts/logos) from.
///
/// With an R2 bucket configured, the worker serves them from its own origin
/// — no cross-org runtime dependency, and the `/b/static/` URL contract that
/// pages already emit stays unchanged. Without one there is nowhere to
/// publish, so fall back to the shared version-pinned CDN.
pub fn resolve_asset_base_url(has_r2: bool) -> String {
    if has_r2 {
        STATIC_PREFIX.to_string()
    } else {
        ui_assets::DEFAULT_CDN_BASE_TEMPLATE.to_string()
    }
}

/// The UI asset set (`impresspress_core::ui::assets::ASSETS`) as release
/// entries, keyed by hashed filename (e.g. `app-a1b2c3d4.css`) rather than
/// the source-relative `logical` name, so the published R2 objects are
/// immutable and match the URLs `ui::assets::url()` emits.
///
/// `ASSETS` is unconditional (`build.rs` lists every asset file on disk
/// regardless of Cargo features), but `ui_assets::bytes()` returns `None`
/// for a block-owned entry whose block the CLI wasn't built with — the block
/// declares those bytes itself (`blocks::llm::assets` owns
/// `marked.min.js`/`purify.min.js`/`llm-chat.js` and needs `block-llm`;
/// `blocks::files::assets` owns `files-browser.js` and needs `block-files`),
/// so a build without the block carries no bytes for them. A supported
/// combination, e.g.
/// `--no-default-features --features sqlite,embed-assets`. Such entries are
/// skipped rather than treated as an error, with a warning naming exactly
/// what was skipped so the omission is visible in the deploy log instead of
/// silently missing at runtime.
///
/// Note: this only fixes the CLI-build-without-`block-*` case. A CLI built
/// WITH a block feature but deployed against a *worker* built without the
/// matching feature (or vice versa) is a separate, unresolved seam — the
/// published set here is always the CLI's, not the worker's, so a mismatch
/// between the two independent builds can still under- or over-publish.
/// Left as follow-up; not attempted here.
///
/// Gated on `embed-assets` because it calls [`ui_assets::bytes`], which is
/// itself gated: a CLI built without `embed-assets` has no asset bytes
/// compiled in at all, so there is nothing here to enumerate. Callers that
/// need to publish UI assets to R2 (see `r2_upload_ui_assets` and its call
/// site in `embed_cloudflare::deploy`) branch on the same feature and fail
/// loudly instead of silently publishing nothing.
#[cfg(feature = "embed-assets")]
pub fn ui_asset_entries() -> Vec<ReleaseAssetEntry> {
    let mut skipped: Vec<&str> = Vec::new();
    let entries = ui_assets::ASSETS
        .iter()
        .filter_map(|e| {
            let Some(bytes) = ui_assets::bytes(e.logical) else {
                skipped.push(e.logical);
                return None;
            };
            Some(ReleaseAssetEntry {
                logical_key: e.filename.to_string(),
                size: bytes.len() as u64,
                sha256: impresspress_core::util::sha256_hex(bytes),
                content_type: e.content_type.to_string(),
            })
        })
        .collect();
    if !skipped.is_empty() {
        eprintln!(
            "warning: skipping {} UI asset(s) with no embedded bytes on this CLI build \
             (missing block feature(s) — see ui_asset_entries doc): {}",
            skipped.len(),
            skipped.join(", "),
        );
    }
    entries
}

/// Walk a staged assets directory into a [`ReleaseManifest`].
///
/// This is the only deploy-side half of the release manifest: turning files on
/// disk into [`ReleaseAssetEntry`] values. Everything downstream of the
/// entries — the asset-set digest, the immutable prefix, the key inventory,
/// the prepared-plan identity — belongs to `ReleaseManifest` itself and is
/// shared with the runtime that verifies it.
pub fn release_manifest_from_staged_dir(assets_root: &Path) -> Result<ReleaseManifest> {
    let mut paths = Vec::new();
    collect_files(assets_root, &mut paths)?;
    let keyed_paths = paths
        .into_iter()
        .map(|path| {
            let key = logical_key(assets_root, &path)?;
            if key == ".impresspress" || key.starts_with(".impresspress/") {
                anyhow::bail!(
                    "release asset logical key {key:?} uses reserved .impresspress namespace"
                );
            }
            if key == "manifest.json" || key == "keys.json" {
                anyhow::bail!(
                    "release asset logical key {key:?} collides with the release \
                     metadata object of the same name; rename the file"
                );
            }
            Ok((key, path))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut files = Vec::with_capacity(keyed_paths.len());
    for (key, path) in keyed_paths {
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read staged release asset {}", path.display()))?;
        files.push(ReleaseAssetEntry {
            logical_key: key,
            size: bytes.len() as u64,
            sha256: impresspress_core::util::sha256_hex(&bytes),
            content_type: mime_for_path(&path).to_string(),
        });
    }

    Ok(ReleaseManifest::from_entries(files)?)
}

#[derive(Debug, Default)]
pub struct StageReport {
    pub files_copied: usize,
    pub bytes_copied: u64,
    pub dirs_skipped: Vec<&'static str>,
    /// Logical keys held out by `release_assets_exclude`. Reported rather than
    /// counted so a deploy log names what it dropped — a silent exclusion of a
    /// file the runtime *did* need would otherwise look identical to shipping it.
    pub files_excluded: Vec<String>,
}

pub fn stage(
    repo_root: &Path,
    out_dir: &Path,
    release_assets_dir: Option<&Path>,
    release_assets_prefix: &Path,
    exclude: &[glob::Pattern],
) -> Result<StageReport> {
    let assets_dir = out_dir.join("assets");
    if assets_dir.exists() {
        std::fs::remove_dir_all(&assets_dir)
            .with_context(|| format!("clean {}", assets_dir.display()))?;
    }
    std::fs::create_dir_all(&assets_dir)
        .with_context(|| format!("create {}", assets_dir.display()))?;

    let mut report = StageReport::default();
    for &name in ASSET_DIRS {
        let src = repo_root.join(name);
        if !src.exists() {
            report.dirs_skipped.push(name);
            continue;
        }
        let dst = assets_dir.join(name);
        copy_dir_recursive(&src, &dst, name, exclude, &mut report)?;
    }
    if let Some(relative_source) = release_assets_dir {
        let src = repo_root.join(relative_source);
        if !src.is_dir() {
            anyhow::bail!(
                "configured R2 release-assets directory does not exist: {}",
                src.display()
            );
        }
        let dst = assets_dir.join(release_assets_prefix);
        let key_prefix = release_assets_prefix.to_string_lossy().replace('\\', "/");
        copy_dir_recursive(&src, &dst, &key_prefix, exclude, &mut report)?;
    }
    Ok(report)
}

/// `key_prefix` is the logical key of `src` relative to the staged assets root
/// — the same string the release manifest will use — so exclusion patterns are
/// written against the keys that actually spend the inventory budget.
fn copy_dir_recursive(
    src: &Path,
    dst: &Path,
    key_prefix: &str,
    exclude: &[glob::Pattern],
    report: &mut StageReport,
) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let key = if key_prefix.is_empty() {
            name.to_string()
        } else {
            format!("{key_prefix}/{name}")
        };
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&path, &dst_path, &key, exclude, report)?;
        } else if ft.is_file() {
            if exclude.iter().any(|pattern| pattern.matches(&key)) {
                report.files_excluded.push(key);
                continue;
            }
            if dst_path.exists() {
                anyhow::bail!(
                    "R2 release asset key collision at {} (source {})",
                    dst_path.display(),
                    path.display()
                );
            }
            let bytes = std::fs::copy(&path, &dst_path)
                .with_context(|| format!("copy {} -> {}", path.display(), dst_path.display()))?;
            report.files_copied += 1;
            report.bytes_copied += bytes;
        }
        // symlinks: silently skipped
    }
    Ok(())
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            collect_files(&path, files)?;
        } else if ft.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn logical_key(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("{} is outside {}", path.display(), root.display()))?;
    let key = relative.to_str().with_context(|| {
        format!(
            "release asset key is not valid UTF-8: {}",
            relative.display()
        )
    })?;
    Ok(key.replace('\\', "/"))
}

/// Returns the MIME type for a file extension. `octet-stream` for unknown.
pub fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The release asset key inventory is bound into a single Worker variable
    /// capped at 4 KB, so every staged file spends a shared budget. Editorial
    /// sidecars that no runtime reads must not spend it.
    #[test]
    fn configured_excludes_are_not_staged_as_release_assets() {
        let repo = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let guides = repo.path().join("content/guides");
        std::fs::create_dir_all(&guides).unwrap();
        std::fs::write(guides.join("wanaka.md"), b"guide").unwrap();
        std::fs::write(guides.join("wanaka.state.json"), b"{}").unwrap();
        let legal = repo.path().join("content/legal");
        std::fs::create_dir_all(&legal).unwrap();
        std::fs::write(legal.join("terms.md"), b"terms").unwrap();

        let excludes = vec![glob::Pattern::new("content/guides/**").unwrap()];
        let report = stage(repo.path(), out.path(), None, Path::new(""), &excludes).unwrap();

        // Only content/legal/terms.md survives.
        let staged = out.path().join("assets");
        assert_eq!(report.files_copied, 1);
        assert!(staged.join("content/legal/terms.md").exists());
        assert!(!staged.join("content/guides/wanaka.md").exists());
        assert!(!staged.join("content/guides/wanaka.state.json").exists());
        assert_eq!(report.files_excluded.len(), 2);
    }

    #[test]
    fn stages_explicit_release_assets_under_the_configured_r2_prefix() {
        let repo = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let source = repo.path().join("release/media");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("hero.webp"), b"image").unwrap();

        let report = stage(
            repo.path(),
            out.path(),
            Some(Path::new("release/media")),
            Path::new("gdsf/site/media"),
            &[],
        )
        .unwrap();

        assert_eq!(report.files_copied, 1);
        assert_eq!(report.bytes_copied, 5);
        assert_eq!(
            std::fs::read(out.path().join("assets/gdsf/site/media/hero.webp")).unwrap(),
            b"image"
        );
    }

    #[test]
    fn release_manifest_is_sorted_and_content_addressed() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(staged.path().join("site/media")).unwrap();
        std::fs::write(staged.path().join("site/media/z.webp"), b"z-image").unwrap();
        std::fs::write(staged.path().join("site/media/a.jpg"), b"a-image").unwrap();

        let first = release_manifest_from_staged_dir(staged.path()).unwrap();
        let second = release_manifest_from_staged_dir(staged.path()).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first
                .files
                .iter()
                .map(|entry| entry.logical_key.as_str())
                .collect::<Vec<_>>(),
            vec!["site/media/a.jpg", "site/media/z.webp"]
        );
        assert_eq!(
            first.immutable_prefix,
            format!("{RELEASES_ROOT}/{}", first.asset_set_sha256)
        );
        assert_eq!(
            first.manifest_key(),
            format!("{}/manifest.json", first.immutable_prefix)
        );
    }

    #[test]
    fn release_identity_changes_for_bytes_or_logical_key() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("hero.webp"), b"v1").unwrap();
        let v1 = release_manifest_from_staged_dir(staged.path()).unwrap();

        std::fs::write(staged.path().join("hero.webp"), b"v2").unwrap();
        let changed_bytes = release_manifest_from_staged_dir(staged.path()).unwrap();
        assert_ne!(v1.asset_set_sha256, changed_bytes.asset_set_sha256);

        std::fs::rename(
            staged.path().join("hero.webp"),
            staged.path().join("renamed.webp"),
        )
        .unwrap();
        let changed_key = release_manifest_from_staged_dir(staged.path()).unwrap();
        assert_ne!(changed_bytes.asset_set_sha256, changed_key.asset_set_sha256);
    }

    #[test]
    fn manifest_json_round_trips_with_identity() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("app.js"), b"console.log(1)").unwrap();
        let manifest = release_manifest_from_staged_dir(staged.path()).unwrap();

        let encoded = manifest.to_pretty_json().unwrap();
        let decoded: ReleaseManifest = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, manifest);
        assert_eq!(
            decoded.immutable_key("app.js"),
            format!("{}/app.js", decoded.immutable_prefix)
        );
        assert!(decoded.manifest_sha256().unwrap().starts_with("sha256:"));
        assert!(decoded
            .logical_keys_sha256()
            .unwrap()
            .starts_with("sha256:"));
        assert!(matches!(
            decoded.prepared_identity().unwrap(),
            impresspress_core::PreparedReleaseAssets::Present { .. }
        ));
    }

    #[test]
    fn manifest_rejects_reserved_deployer_namespace() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(staged.path().join(".impresspress")).unwrap();
        std::fs::write(staged.path().join(".impresspress/user.json"), b"data").unwrap();

        let err = release_manifest_from_staged_dir(staged.path()).unwrap_err();
        assert!(err.to_string().contains("reserved .impresspress namespace"));
    }

    #[test]
    fn keys_key_lives_beside_the_manifest() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("app.js"), b"app").unwrap();
        let release = release_manifest_from_staged_dir(staged.path()).unwrap();
        assert_eq!(
            release.keys_key(),
            format!("{}/keys.json", release.immutable_prefix)
        );
    }

    #[test]
    fn root_level_metadata_names_are_reserved_logical_keys() {
        for reserved in ["manifest.json", "keys.json"] {
            let staged = tempfile::tempdir().unwrap();
            std::fs::write(staged.path().join(reserved), b"x").unwrap();
            let err = release_manifest_from_staged_dir(staged.path()).unwrap_err();
            assert!(
                err.to_string().contains(reserved),
                "expected {reserved} rejection, got: {err}"
            );
        }
    }
}
