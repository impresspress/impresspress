use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::BypassRules;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetManifest {
    #[serde(rename = "buildId")]
    pub build_id: String,
    /// Logical asset name (as referenced from templates) → `/`-prefixed hashed URL.
    pub assets: BTreeMap<String, String>,
    /// Every file the bundler left in the dist directory, relative to it,
    /// forward-slash-separated and sorted.
    ///
    /// `assets` names only the two files a template has to *reference* by a
    /// logical name (the wasm-pack pair). This is the whole shell — the
    /// rendered `index.html`/`loader.js`/`sw.js`, the wasm-bindgen
    /// `snippets/` tree, `vendor/`, everything — and it exists because a
    /// running runtime has no other way to enumerate the static files of a
    /// bundle. The development sandbox's export (`impresspress-core`'s
    /// `blocks::dev::export`) copies exactly the list of its EXPORT SHELL —
    /// the site bundle a sandbox deployment carries at
    /// [`EXPORT_SHELL_ROOT`](super::EXPORT_SHELL_ROOT) (`[sandbox]
    /// export_shell`) — into the bundle it hands the user, so a file the
    /// bundler produced but this list omitted would be a file missing from
    /// every exported site, with no error anywhere.
    ///
    /// A sandbox that carries an export shell has two such lists: its own,
    /// which never names the shell's files — the shell is laid down after
    /// this listing is taken — and the shell's, in the shell's own
    /// `asset-manifest.json`, naming exactly the files the shell was built
    /// with. Only the shell's is copied anywhere.
    ///
    /// `#[serde(default)]` so a manifest without the field still
    /// deserializes as an empty list, which every reader refuses by name
    /// ("lists no files") rather than treating as a bundle with nothing in
    /// it.
    #[serde(default)]
    pub files: Vec<String>,
    /// The bypass rules this bundle's `sw.js` was rendered with — the one
    /// [`BypassRules`] value `run` computes, which also renders the fetch
    /// handler's condition and the `BYPASS_RULES` the worker hands
    /// `initialize()` (`bundle::bypass`'s module docs: there is no second
    /// list). Written here so a reader that holds only the bundle's files,
    /// not its running worker, can learn which paths that worker leaves to
    /// the static host without parsing JavaScript.
    ///
    /// No `#[serde(default)]`: empty rules would read as "this worker
    /// bypasses nothing", which is a claim about the worker, not the absence
    /// of one. A manifest from a bundler that predates the field does not
    /// parse as this type.
    pub bypass: BypassRules,
    /// The boot shell's own URL path this bundle's `sw.js` and `loader.js`
    /// were rendered with ([`super::AppConfig::shell_path`]) — the path the
    /// static host serving this bundle must answer with the shell document.
    /// Written here so a reader that holds only the bundle's files can learn
    /// that contract: the `impresspress` CLI refuses an export shell whose
    /// shell is not at `/`, because a development sandbox's export is
    /// served by any static file server, and those answer the root with
    /// `index.html` and have no other route to the shell.
    ///
    /// No `#[serde(default)]`, like [`Self::bypass`]: a manifest from a
    /// bundler that predates the field does not parse as this type.
    #[serde(rename = "shellPath")]
    pub shell_path: super::ShellPath,
    /// The SHA-256 (lowercase hex) of every file in [`Self::files`] as
    /// [`run`](super::run) wrote it, by the same relative path — except
    /// `asset-manifest.json` itself, which cannot carry its own digest.
    ///
    /// For a reader that fetches the listed files from a static host and has
    /// to know it got the bundle's bytes: the development sandbox's export,
    /// reading its export shell. A static host can answer a file it does not
    /// have with something else and a `200` — Cloudflare's
    /// `not_found_handling = "single-page-application"` serves the root
    /// `index.html` for any missing path — so a shell missing one listed file
    /// would otherwise be exported with HTML under the wasm's name. A digest
    /// catches that and every other wrong body (a stale cache, a partial
    /// upload), where checking the response's type would catch only the
    /// first.
    ///
    /// The bytes `run` wrote: a file the `impresspress` CLI lays over a
    /// listed one afterwards (`[[assets.overlay]]`) replaces the file and not
    /// its digest. The export shell is laid down whole, and the CLI refuses
    /// an overlay under its root, so what it lists is what it ships.
    ///
    /// No `#[serde(default)]`, like [`Self::bypass`]: a manifest from a
    /// bundler that predates the field cannot be verified, and does not
    /// parse as this type.
    pub sha256: BTreeMap<String, String>,
}

/// The name [`run`](super::run) writes the manifest under — the one listed
/// file [`AssetManifest::sha256`] has no digest for.
pub const ASSET_MANIFEST: &str = "asset-manifest.json";

/// [`AssetManifest::sha256`] for `files` under `dir`: every listed file but
/// [`ASSET_MANIFEST`], hashed as it is on disk.
pub fn digest_files(dir: &Path, files: &[String]) -> Result<BTreeMap<String, String>> {
    use sha2::{Digest, Sha256};
    let mut digests = BTreeMap::new();
    for file in files.iter().filter(|file| *file != ASSET_MANIFEST) {
        let path = dir.join(file);
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        digests.insert(file.clone(), hex::encode(Sha256::digest(&bytes)));
    }
    Ok(digests)
}

/// Every file under `dir`, relative to it, `/`-separated and sorted, skipping
/// unrendered `*.tmpl` templates and wasm-pack's npm-package metadata.
///
/// A `.tmpl` in the output directory is a template `run` had no renderer for
/// (`render_if_exists` deletes the ones it renders): it is bundler input, not
/// a file any browser should ever be served, and shipping one into an export
/// would ship an un-substituted `__WASM_JS__` alongside the real script.
///
/// [`is_package_metadata`] holds back the other files that are in the
/// directory without being part of the shell, for the same reason. The
/// `embed × web` flow bundles IN PLACE in wasm-pack's own `--out-dir`
/// (`flows::embed_web::build`), so `package.json`, the `*.d.ts` declarations,
/// `.gitignore` and the copied `README.md` sit beside the real assets by the
/// time this listing is taken. They are how npm consumes a wasm-pack package,
/// not how a browser boots one — and because `files` is exactly what the
/// development sandbox's export copies, anything listed here is a file every
/// exported site carries. (The `sealed × web` flow assembles a fresh `dist/`
/// and never has them, which is why this only shows up on one path.)
pub fn list_dist_files(dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    collect_files(dir, "", &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("listing dist dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading an entry of {}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", entry.path().display()))?;
        if file_type.is_dir() {
            collect_files(&entry.path(), &relative, out)?;
        } else if !name.ends_with(".tmpl") && !is_package_metadata(&name) {
            out.push(relative);
        }
    }
    Ok(())
}

/// Whether `name` is wasm-pack's npm-package metadata rather than part of the
/// shell a browser is served — see [`list_dist_files`].
///
/// Matched by exact name (or the `.d.ts` suffix), never by extension: a
/// `manifest.json` or a `vendor/*.json` the bundler really does ship is
/// untouched, and so is anything an overlay lays down later (overlays run
/// after the listing is taken, so they were never in it).
fn is_package_metadata(name: &str) -> bool {
    matches!(name, "package.json" | ".gitignore" | "README.md") || name.ends_with(".d.ts")
}

impl AssetManifest {
    /// Check that `dir` holds exactly what this manifest states: every listed
    /// file there, each but [`ASSET_MANIFEST`] with the digest
    /// [`Self::sha256`] gives it, and no digest for a file that is not
    /// listed. The error names the first file that is not.
    pub fn verify(&self, dir: &Path) -> Result<()> {
        if let Some(unlisted) = self.sha256.keys().find(|file| !self.files.contains(file)) {
            anyhow::bail!("{ASSET_MANIFEST} has a digest for {unlisted:?}, which it does not list");
        }
        let found = digest_files(dir, &self.files)?;
        for (file, digest) in &found {
            match self.sha256.get(file) {
                None => anyhow::bail!("{ASSET_MANIFEST} lists {file:?} with no digest"),
                Some(listed) if listed != digest => anyhow::bail!(
                    "{file:?} is not the file {ASSET_MANIFEST} lists: its SHA-256 is {digest}, \
                     not {listed}"
                ),
                Some(_) => {}
            }
        }
        Ok(())
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let body = serde_json::to_string_pretty(self).context("serialising asset manifest")?;
        std::fs::write(path, body).with_context(|| format!("writing {ASSET_MANIFEST}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_expected_json_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("asset-manifest.json");
        let mut assets = BTreeMap::new();
        assets.insert(
            "impresspress_web.js".into(),
            "/impresspress_web-a1b2c3d4.js".into(),
        );
        let m = AssetManifest {
            build_id: "a1b2c3d4".into(),
            assets,
            files: vec!["index.html".into(), "vendor/sql-wasm.wasm".into()],
            bypass: BypassRules {
                exact: vec!["/sw.js".into()],
                prefixes: vec!["/seed/".into()],
            },
            shell_path: crate::bundle::ShellPath::parse("/chat").unwrap(),
            sha256: BTreeMap::from([
                ("index.html".into(), "aa".repeat(32)),
                ("vendor/sql-wasm.wasm".into(), "bb".repeat(32)),
            ]),
        };
        m.write(&path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"buildId\": \"a1b2c3d4\""));
        assert!(contents.contains("\"impresspress_web.js\": \"/impresspress_web-a1b2c3d4.js\""));
        // The shell listing round-trips as a plain array of relative paths —
        // the export reads it back verbatim and fetches each one.
        let parsed: AssetManifest = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed.files, vec!["index.html", "vendor/sql-wasm.wasm"]);
        // The rules round-trip as the same `{exact, prefixes}` object
        // `BypassRules::render_data` writes into `sw.js`.
        assert_eq!(parsed.bypass, m.bypass);
        // The digests round-trip as a plain `{path: hex}` object.
        assert_eq!(parsed.sha256, m.sha256);
        // The shell path as the plain string the templates are rendered with.
        assert!(contents.contains("\"shellPath\": \"/chat\""), "{contents}");
        assert_eq!(parsed.shell_path, m.shell_path);
        let raw: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(
            raw["bypass"],
            serde_json::from_str::<serde_json::Value>(&m.bypass.render_data()).unwrap()
        );
    }

    /// A manifest without `files` still parses; the field is simply empty.
    /// The export turns that into an explicit refusal rather than an empty
    /// shell.
    #[test]
    fn a_manifest_without_files_still_parses() {
        let parsed: AssetManifest = serde_json::from_str(
            r#"{"buildId": "x", "assets": {}, "bypass": {"exact": [], "prefixes": []}, "shellPath": "/", "sha256": {}}"#,
        )
        .unwrap();
        assert!(parsed.files.is_empty());
    }

    /// One without `bypass` does not: empty rules would be a claim that the
    /// worker bypasses nothing, not the absence of a claim.
    #[test]
    fn a_manifest_without_bypass_does_not_parse() {
        let error = serde_json::from_str::<AssetManifest>(
            r#"{"buildId": "x", "assets": {}, "files": ["sw.js"], "shellPath": "/", "sha256": {}}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("bypass"), "{error}");
    }

    /// Nor one without `sha256`: a reader that verifies what it fetched
    /// cannot verify against nothing.
    #[test]
    fn a_manifest_without_digests_does_not_parse() {
        let error = serde_json::from_str::<AssetManifest>(
            r#"{"buildId": "x", "assets": {}, "files": ["sw.js"], "bypass": {"exact": [], "prefixes": []}, "shellPath": "/"}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("sha256"), "{error}");
    }

    /// Nor one without `shellPath`: a reader holding only the files would
    /// not know where the host has to serve the shell.
    #[test]
    fn a_manifest_without_a_shell_path_does_not_parse() {
        let error = serde_json::from_str::<AssetManifest>(
            r#"{"buildId": "x", "assets": {}, "files": ["sw.js"], "bypass": {"exact": [], "prefixes": []}, "sha256": {}}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("shellPath"), "{error}");
    }

    /// And one whose shell path is not a valid one is refused as such.
    #[test]
    fn a_manifest_with_an_invalid_shell_path_does_not_parse() {
        let error = serde_json::from_str::<AssetManifest>(
            r#"{"buildId": "x", "assets": {}, "files": ["sw.js"], "bypass": {"exact": [], "prefixes": []}, "shellPath": "chat", "sha256": {}}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("shell_path \"chat\""), "{error}");
    }

    /// Every listed file is hashed as it is on disk, except the manifest,
    /// which cannot hold its own digest.
    #[test]
    fn digest_files_hashes_every_listed_file_but_the_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("vendor")).unwrap();
        std::fs::write(tmp.path().join("index.html"), "hello").unwrap();
        std::fs::write(tmp.path().join("vendor/a.wasm"), "").unwrap();
        let files = vec![
            ASSET_MANIFEST.to_string(),
            "index.html".to_string(),
            "vendor/a.wasm".to_string(),
        ];
        let digests = digest_files(tmp.path(), &files).unwrap();
        assert_eq!(
            digests,
            BTreeMap::from([
                (
                    "index.html".to_string(),
                    // SHA-256("hello")
                    "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_string()
                ),
                (
                    "vendor/a.wasm".to_string(),
                    // SHA-256("")
                    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string()
                ),
            ])
        );
        // A listed file that is not there is an error, not a skipped entry.
        assert!(digest_files(tmp.path(), &["gone.js".to_string()]).is_err());
    }

    /// Every file under the directory, relative and sorted, with unrendered
    /// templates left out — `run` deletes the ones it renders, so a `.tmpl`
    /// that survives is bundler input no browser should be served.
    #[test]
    fn list_dist_files_walks_recursively_sorted_without_templates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("snippets/inner")).unwrap();
        std::fs::create_dir_all(root.join("vendor")).unwrap();
        std::fs::write(root.join("sw.js"), "x").unwrap();
        std::fs::write(root.join("index.html"), "x").unwrap();
        std::fs::write(root.join("index.html.tmpl"), "x").unwrap();
        std::fs::write(root.join("vendor/sql-wasm.wasm"), "x").unwrap();
        std::fs::write(root.join("snippets/inner/glue.js"), "x").unwrap();

        assert_eq!(
            list_dist_files(root).unwrap(),
            vec![
                "index.html",
                "snippets/inner/glue.js",
                "sw.js",
                "vendor/sql-wasm.wasm",
            ]
        );
    }

    /// `embed × web` bundles in place in wasm-pack's `--out-dir`, so the
    /// package metadata is right there when the listing is taken — and every
    /// name in this list is copied into every exported site.
    #[test]
    fn wasm_pack_package_metadata_is_not_part_of_the_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("app.js"), "x").unwrap();
        std::fs::write(root.join("app_bg.wasm"), "x").unwrap();
        std::fs::write(root.join("manifest.json"), "x").unwrap();
        for metadata in [
            "package.json",
            ".gitignore",
            "README.md",
            "app.d.ts",
            "app_bg.wasm.d.ts",
        ] {
            std::fs::write(root.join(metadata), "x").unwrap();
        }

        assert_eq!(
            list_dist_files(root).unwrap(),
            // `manifest.json` is the web app manifest the bundler ships, and
            // stays: the rule is by name, not by extension.
            vec!["app.js", "app_bg.wasm", "manifest.json"]
        );
    }

    #[test]
    fn ordering_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("m.json");
        let mut assets = BTreeMap::new();
        assets.insert("z.wasm".into(), "/z.wasm".into());
        assets.insert("a.js".into(), "/a.js".into());
        let m = AssetManifest {
            build_id: "x".into(),
            assets,
            files: Vec::new(),
            bypass: BypassRules::default(),
            shell_path: crate::bundle::ShellPath::default(),
            sha256: BTreeMap::new(),
        };
        m.write(&path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let a_pos = body.find("\"a.js\"").unwrap();
        let z_pos = body.find("\"z.wasm\"").unwrap();
        assert!(a_pos < z_pos);
    }
}
