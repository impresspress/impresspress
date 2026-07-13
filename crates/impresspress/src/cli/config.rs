use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub app: AppConfig,
    #[serde(default)]
    pub assets: AssetsConfig,
    #[serde(default)]
    pub wasm: WasmConfig,
    #[serde(default)]
    pub impresspress: ImpresspressConfig,
}

/// Points at the impresspress workspace when the consumer repo isn't part of it.
///
/// For repos that ARE inside the impresspress workspace (e.g. `impresspress-web` at
/// `crates/impresspress-web/`) this stays at the default — cargo resolves the
/// impresspress crates from the enclosing workspace. For external consumers
/// (e.g. gizza-ai that path-depends on impresspress from a sibling directory)
/// set `manifest_path = "../impresspress"` so the CLI passes
/// `--manifest-path ../impresspress/Cargo.toml` to the `cargo build` that
/// compiles the native binary (see `embed_native`).
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ImpresspressConfig {
    /// Path (absolute or relative to `impresspress.toml`) to a directory that
    /// contains the impresspress workspace `Cargo.toml`, or to the `Cargo.toml`
    /// file itself. `None` → no `--manifest-path` flag passed.
    #[serde(default)]
    pub manifest_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub name: String,
    pub title: String,
    pub boot_redirect: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AssetsConfig {
    #[serde(default)]
    pub extra_bypass_prefix: Vec<String>,
    #[serde(default)]
    pub extra_bypass_exact: Vec<String>,
    #[serde(default)]
    pub overlay: Vec<OverlayEntry>,
    /// Whether `loader.js`'s recovery path wipes OPFS when the SW
    /// self-destructs. Defaults to **false** — apps that store user data
    /// in OPFS shouldn't lose it on a transient init failure. Set to
    /// `true` for throwaway-data deployments like `demo.impresspress.dev`
    /// where a stale-schema loop should self-resolve without manual
    /// `chrome://settings/siteData` cleanup. See
    /// `crates/impresspress-bundle/assets/loader.js.tmpl` for the runtime
    /// behavior this controls.
    #[serde(default)]
    pub opfs_wipe_on_recovery: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayEntry {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WasmConfig {
    #[serde(default = "default_out_dir")]
    pub out_dir: String,
}

fn default_out_dir() -> String {
    "pkg".to_string()
}

impl Default for WasmConfig {
    fn default() -> Self {
        Self {
            out_dir: default_out_dir(),
        }
    }
}

pub fn parse(toml_text: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(toml_text)
}

use std::path::{Path, PathBuf};

/// Walk up from `start` looking for `impresspress.toml`; parse and return
/// `(config, repo_root)` where `repo_root` is the directory that contains
/// the file.
pub fn find_and_load(start: &Path) -> anyhow::Result<(Config, PathBuf)> {
    let start = start
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("canonicalize {start:?}: {e}"))?;
    let mut cur: &Path = &start;
    loop {
        let candidate = cur.join("impresspress.toml");
        if candidate.is_file() {
            let text = std::fs::read_to_string(&candidate)
                .map_err(|e| anyhow::anyhow!("read {candidate:?}: {e}"))?;
            let cfg = parse(&text).map_err(|e| anyhow::anyhow!("parse {candidate:?}: {e}"))?;
            return Ok((cfg, cur.to_path_buf()));
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => {
                return Err(anyhow::anyhow!(
                    "no impresspress.toml found in {start:?} or any parent directory"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let input = r#"
[app]
name = "impresspress-web"
title = "Impresspress"
boot_redirect = "/b/system/"
"#;
        let cfg = parse(input).unwrap();
        assert_eq!(cfg.app.name, "impresspress-web");
        assert_eq!(cfg.app.title, "Impresspress");
        assert_eq!(cfg.app.boot_redirect, "/b/system/");
        assert_eq!(cfg.assets.extra_bypass_prefix, Vec::<String>::new());
        assert!(cfg.assets.overlay.is_empty());
        assert_eq!(cfg.wasm.out_dir, "pkg");
    }

    #[test]
    fn parse_full_config() {
        let input = r#"
[app]
name = "gizza-ai"
title = "Gizza AI"
boot_redirect = "/"

[assets]
extra_bypass_prefix = ["/gizza-app.js", "/gizza.css"]

[[assets.overlay]]
from = "site/index.html"
to = "index.html"

[[assets.overlay]]
from = "site/gizza-app.js"
to = "gizza-app.js"

[wasm]
out_dir = "dist"
"#;
        let cfg = parse(input).unwrap();
        assert_eq!(cfg.app.name, "gizza-ai");
        assert_eq!(
            cfg.assets.extra_bypass_prefix,
            vec!["/gizza-app.js".to_string(), "/gizza.css".to_string()]
        );
        assert_eq!(cfg.assets.overlay.len(), 2);
        assert_eq!(cfg.assets.overlay[0].from, "site/index.html");
        assert_eq!(cfg.assets.overlay[0].to, "index.html");
        assert_eq!(cfg.wasm.out_dir, "dist");
    }

    #[test]
    fn reject_unknown_field_in_app() {
        let input = r#"
[app]
name = "x"
title = "y"
boot_redirect = "/"
color = "red"
"#;
        let err = parse(input).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("color"),
            "expected error to mention 'color', got: {msg}"
        );
    }

    #[test]
    fn reject_missing_app() {
        let input = r#"
[assets]
extra_bypass_prefix = []
"#;
        let err = parse(input).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("app"),
            "expected error to mention 'app', got: {msg}"
        );
    }

    #[test]
    fn find_config_walks_up() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("impresspress.toml"),
            r#"
[app]
name = "x"
title = "y"
boot_redirect = "/"
"#,
        )
        .unwrap();
        let nested = root.join("sub/dir");
        fs::create_dir_all(&nested).unwrap();

        let (cfg, repo_root) = find_and_load(&nested).unwrap();
        assert_eq!(cfg.app.name, "x");
        assert_eq!(
            repo_root.canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
    }

    #[test]
    fn find_config_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let err = find_and_load(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("impresspress.toml"));
        assert!(err.contains("no"));
    }

    #[test]
    fn parses_extra_bypass_exact() {
        let toml = r#"
[app]
name = "x"
title = "y"
boot_redirect = "/"

[assets]
extra_bypass_exact = ["/", "/index.html"]
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            cfg.assets.extra_bypass_exact,
            vec!["/".to_string(), "/index.html".to_string()]
        );
    }
}
