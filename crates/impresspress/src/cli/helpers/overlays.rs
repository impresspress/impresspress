//! Asset-overlay logic shared by the four flow quadrants. Reads
//! `[[assets.overlay]]` entries from `impresspress.toml` and copies each
//! `from` (relative to repo root) to `to` (relative to the dist dir). An
//! entry may name either a file or a directory — a directory is copied
//! recursively, which is what the dev-sandbox example uses to overlay its
//! whole `seed/` tree onto `dist/seed/` in one entry. `[sandbox]
//! export_shell` is laid down the same way, at the bundler's
//! `EXPORT_SHELL_ROOT` ([`apply_export_shell`]).

use std::path::Path;

use anyhow::{anyhow, Result};

use crate::cli::config::Config;

pub fn apply_overlays(cfg: &Config, repo_root: &Path, dist_dir: &Path) -> Result<()> {
    for overlay in &cfg.assets.overlay {
        let src = repo_root.join(&overlay.from);
        let dst = dist_dir.join(&overlay.to);
        if src.is_dir() {
            copy_dir_all(&src, &dst)?;
        } else {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| anyhow!("create dir {parent:?}: {e}"))?;
            }
            std::fs::copy(&src, &dst).map_err(|e| anyhow!("overlay {src:?} → {dst:?}: {e}"))?;
        }
    }
    Ok(())
}

/// Lay `[sandbox] export_shell` down at
/// [`EXPORT_SHELL_ROOT`](impresspress_bundle::bundle::EXPORT_SHELL_ROOT) in
/// `dist_dir` — the address the bundler rendered into this bundle's `sw.js` —
/// when the configuration names one. A no-op otherwise.
///
/// Called by the web flows after `impresspress_bundle::bundle::run`, like
/// [`apply_overlays`], and for the same reason: the bundler lists the
/// sandbox's OWN files in its `asset-manifest.json`, and the export shell is
/// not one of them.
///
/// The directory must be a finished bundle: its `asset-manifest.json` parses
/// as the bundler's [`AssetManifest`](impresspress_bundle::bundle::manifest::AssetManifest)
/// — `bypass` and `sha256` included, which that type requires — and every
/// file it lists is there with the digest it lists. A path that names the shell's configuration directory instead of
/// its `dist/`, or a shell that was never built, fails the build here rather
/// than shipping a sandbox whose export has nothing to copy. And no
/// `[[assets.overlay]]` may write under the root: its files would be mixed
/// into the shell.
pub fn apply_export_shell(cfg: &Config, repo_root: &Path, dist_dir: &Path) -> Result<()> {
    let Some(shell) = &cfg.sandbox.export_shell else {
        return Ok(());
    };
    let src = repo_root.join(shell);
    let manifest_path = src.join("asset-manifest.json");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        anyhow!("[sandbox] export_shell {src:?} is not a built bundle ({manifest_path:?}: {e})")
    })?;
    let manifest: impresspress_bundle::bundle::manifest::AssetManifest =
        serde_json::from_str(&text).map_err(|e| {
            anyhow!(
                "[sandbox] export_shell {src:?}: asset-manifest.json is not the bundler's \
                 manifest ({e}); rebuild the shell with a current impresspress"
            )
        })?;
    if manifest.files.is_empty() {
        return Err(anyhow!(
            "[sandbox] export_shell {src:?}: asset-manifest.json lists no files; rebuild the \
             shell with a current impresspress"
        ));
    }
    // The files the export will fetch are the ones the manifest describes,
    // digests and all: a shell edited, or half-copied, after it was built
    // fails here rather than in every export of the deployment.
    manifest.verify(&src).map_err(|e| {
        anyhow!("[sandbox] export_shell {src:?} is not the bundle its manifest describes: {e:#}")
    })?;
    let root = impresspress_bundle::bundle::EXPORT_SHELL_ROOT.trim_matches('/');
    if let Some(overlay) = cfg.assets.overlay.iter().find(|overlay| {
        let to = overlay.to.trim_start_matches('/');
        to == root || to.starts_with(&format!("{root}/"))
    }) {
        return Err(anyhow!(
            "[[assets.overlay]] writes to {:?}, inside the export shell's /{root}/ — the \
             shell is [sandbox] export_shell, laid down whole",
            overlay.to
        ));
    }
    let dst = dist_dir.join(root);
    // The flow that bundles in place (`embed × web`) keeps its output
    // directory between builds: a previous build's shell would otherwise be
    // mixed with this one's, its old content-hashed files and all.
    if dst.exists() {
        std::fs::remove_dir_all(&dst).map_err(|e| anyhow!("remove the old {dst:?}: {e}"))?;
    }
    copy_dir_all(&src, &dst)
}

/// Recursively copy every entry under `src` into `dst`, creating directories
/// as needed. The directory half of [`apply_overlays`] — a plain `fs::copy`
/// only ever handles the file-to-file case.
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).map_err(|e| anyhow!("create dir {dst:?}: {e}"))?;
    for entry in std::fs::read_dir(src).map_err(|e| anyhow!("read dir {src:?}: {e}"))? {
        let entry = entry.map_err(|e| anyhow!("read dir entry under {src:?}: {e}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| anyhow!("overlay {src_path:?} → {dst_path:?}: {e}"))?;
        }
    }
    Ok(())
}

pub use crate::cli::config::OverlayEntry as Overlay;

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A minimal `Config` whose `[[assets.overlay]]` entries are exactly
    /// `entries`.
    fn overlay_cfg(entries: &[(&str, &str)]) -> Config {
        let mut toml =
            String::from("[app]\nname = \"x\"\ntitle = \"X\"\nboot_redirect = \"/\"\n\n[assets]\n");
        for (from, to) in entries {
            toml.push_str("[[assets.overlay]]\n");
            toml.push_str(&format!("from = \"{from}\"\n"));
            toml.push_str(&format!("to = \"{to}\"\n"));
        }
        crate::cli::config::parse(&toml).unwrap()
    }

    #[test]
    fn directory_overlay_copies_recursively() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("seed/a")).unwrap();
        fs::write(root.join("seed/a/b.txt"), b"hi").unwrap();

        let cfg = overlay_cfg(&[("seed", "seed")]);
        let dist = root.join("dist");
        fs::create_dir_all(&dist).unwrap();
        apply_overlays(&cfg, root, &dist).unwrap();

        assert_eq!(fs::read_to_string(dist.join("seed/a/b.txt")).unwrap(), "hi");
    }

    /// A site bundle, rendered by the real bundler into `dir` — what
    /// `examples/dev-sandbox/build.sh` builds `export-shell/dist/` as.
    fn built_site_shell(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        impresspress_bundle::assets::write_to(dir).unwrap();
        impresspress_bundle::bundle::run(
            dir,
            dir,
            impresspress_bundle::bundle::AppConfig {
                sandbox: impresspress_bundle::bundle::SandboxShell::Site,
                ..Default::default()
            },
        )
        .unwrap();
    }

    fn workspace_cfg(extra: &str) -> Config {
        crate::cli::config::parse(&format!(
            "[app]\nname = \"x\"\ntitle = \"X\"\nboot_redirect = \"/\"\n{extra}\n\
             [sandbox]\nmode = \"workspace\"\nexport_shell = \"shell/dist\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn the_export_shell_is_laid_down_whole_at_the_bundlers_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        built_site_shell(&root.join("shell/dist"));
        let dist = root.join("dist");
        fs::create_dir_all(&dist).unwrap();

        apply_export_shell(&workspace_cfg(""), root, &dist).unwrap();

        let laid = dist.join("__impresspress_dev/export-shell");
        assert_eq!(
            format!("/{}/", "__impresspress_dev/export-shell"),
            impresspress_bundle::bundle::EXPORT_SHELL_ROOT
        );
        let listed =
            impresspress_bundle::bundle::manifest::list_dist_files(&root.join("shell/dist"))
                .unwrap();
        assert_eq!(
            impresspress_bundle::bundle::manifest::list_dist_files(&laid).unwrap(),
            listed
        );
        for file in &listed {
            assert_eq!(
                fs::read(laid.join(file)).unwrap(),
                fs::read(root.join("shell/dist").join(file)).unwrap(),
                "{file}"
            );
        }

        // Laid down again over a previous build's shell, nothing of the old
        // one is left.
        fs::write(laid.join("stale-0123abcd.js"), b"old").unwrap();
        apply_export_shell(&workspace_cfg(""), root, &dist).unwrap();
        assert!(!laid.join("stale-0123abcd.js").exists());
    }

    #[test]
    fn a_configuration_with_no_export_shell_lays_none_down() {
        let tmp = tempfile::tempdir().unwrap();
        let dist = tmp.path().join("dist");
        fs::create_dir_all(&dist).unwrap();
        apply_export_shell(&overlay_cfg(&[]), tmp.path(), &dist).unwrap();
        assert!(!dist.join("__impresspress_dev").exists());
    }

    /// A directory that is not a finished bundle is refused, naming it: never
    /// built, its manifest without the `bypass` or `sha256` the bundler's type
    /// requires, a listed file whose bytes are not the ones it lists, or a
    /// listed file that is not there.
    #[test]
    fn an_export_shell_that_is_not_a_finished_bundle_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dist = root.join("dist");
        fs::create_dir_all(&dist).unwrap();
        let cfg = workspace_cfg("");

        let err = apply_export_shell(&cfg, root, &dist)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("export_shell") && err.contains("shell/dist"),
            "{err}"
        );

        let shell = root.join("shell/dist");
        built_site_shell(&shell);
        let manifest_path = shell.join("asset-manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();

        let mut no_bypass = manifest.clone();
        no_bypass.as_object_mut().unwrap().remove("bypass");
        fs::write(&manifest_path, no_bypass.to_string()).unwrap();
        let err = apply_export_shell(&cfg, root, &dist)
            .unwrap_err()
            .to_string();
        assert!(err.contains("bypass") && err.contains("rebuild"), "{err}");

        let mut no_digests = manifest.clone();
        no_digests.as_object_mut().unwrap().remove("sha256");
        fs::write(&manifest_path, no_digests.to_string()).unwrap();
        let err = apply_export_shell(&cfg, root, &dist)
            .unwrap_err()
            .to_string();
        assert!(err.contains("sha256") && err.contains("rebuild"), "{err}");

        fs::write(&manifest_path, manifest.to_string()).unwrap();
        let loader = fs::read(shell.join("loader.js")).unwrap();
        fs::write(shell.join("loader.js"), b"// edited after the build").unwrap();
        let err = apply_export_shell(&cfg, root, &dist)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("loader.js") && err.contains("SHA-256"),
            "{err}"
        );

        fs::remove_file(shell.join("loader.js")).unwrap();
        let err = apply_export_shell(&cfg, root, &dist)
            .unwrap_err()
            .to_string();
        assert!(err.contains("loader.js"), "{err}");
        assert!(!dist.join("__impresspress_dev").exists());
        fs::write(shell.join("loader.js"), loader).unwrap();
        apply_export_shell(&cfg, root, &dist).expect("the shell as it was built");
    }

    #[test]
    fn an_overlay_into_the_export_shell_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        built_site_shell(&root.join("shell/dist"));
        let dist = root.join("dist");
        fs::create_dir_all(&dist).unwrap();
        let cfg = workspace_cfg(
            "[[assets.overlay]]\nfrom = \"x.js\"\nto = \"__impresspress_dev/export-shell/x.js\"\n",
        );
        let err = apply_export_shell(&cfg, root, &dist)
            .unwrap_err()
            .to_string();
        assert!(err.contains("export-shell/x.js"), "{err}");
    }

    #[test]
    fn file_overlay_still_works() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("index.html"), b"<h1>hi</h1>").unwrap();

        let cfg = overlay_cfg(&[("index.html", "index.html")]);
        let dist = root.join("dist");
        fs::create_dir_all(&dist).unwrap();
        apply_overlays(&cfg, root, &dist).unwrap();

        assert_eq!(
            fs::read_to_string(dist.join("index.html")).unwrap(),
            "<h1>hi</h1>"
        );
    }
}
