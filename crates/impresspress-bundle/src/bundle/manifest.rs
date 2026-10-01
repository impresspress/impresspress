use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    /// `snippets/` tree, `vendor/`, everything — and it exists because the
    /// running runtime has no other way to enumerate the static files it was
    /// shipped inside of. The development sandbox's export
    /// (`impresspress-core`'s `blocks::dev::export`) copies exactly this list
    /// into the bundle it hands the user, so a file the bundler produced but
    /// this list omitted would be a file missing from every exported site,
    /// with no error anywhere.
    ///
    /// `#[serde(default)]` so a manifest written by an older bundler still
    /// deserializes — the field is then empty, which the export reports as
    /// "this deployment's shell cannot be listed" rather than silently
    /// exporting a site with no runtime in it.
    #[serde(default)]
    pub files: Vec<String>,
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
    pub fn write(&self, path: &Path) -> Result<()> {
        let body = serde_json::to_string_pretty(self).context("serialising asset manifest")?;
        std::fs::write(path, body).context("writing asset-manifest.json")?;
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
        };
        m.write(&path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"buildId\": \"a1b2c3d4\""));
        assert!(contents.contains("\"impresspress_web.js\": \"/impresspress_web-a1b2c3d4.js\""));
        // The shell listing round-trips as a plain array of relative paths —
        // the export reads it back verbatim and fetches each one.
        let parsed: AssetManifest = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed.files, vec!["index.html", "vendor/sql-wasm.wasm"]);
    }

    /// A manifest written by a bundler that predates `files` still parses;
    /// the field is simply empty. The export turns that into an explicit
    /// refusal rather than an empty shell.
    #[test]
    fn a_manifest_without_files_still_parses() {
        let parsed: AssetManifest =
            serde_json::from_str(r#"{"buildId": "x", "assets": {}}"#).unwrap();
        assert!(parsed.files.is_empty());
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
        };
        m.write(&path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let a_pos = body.find("\"a.js\"").unwrap();
        let z_pos = body.find("\"z.wasm\"").unwrap();
        assert!(a_pos < z_pos);
    }
}
