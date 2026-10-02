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
    #[serde(default)]
    pub dev: DevConfig,
}

/// `[dev]` — the browser development sandbox (`impresspress/dev` block,
/// `/b/dev`, dynamic guest blocks). Off by default; also requires the
/// consumer crate to be built with `impresspress-web/browser-devtools`. A
/// bundle built without the feature accepts `enabled = true` and ignores it —
/// the block cannot exist in a binary that never compiled it.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DevConfig {
    #[serde(default)]
    pub enabled: bool,
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
#[serde(try_from = "RawAppConfig")]
pub struct AppConfig {
    pub name: String,
    pub title: AppTitle,
    pub boot_redirect: String,
    /// An HTML fragment file (relative to the directory holding
    /// `impresspress.toml`, like an overlay's `from`) that the boot shell
    /// shows under its title — what a first visitor, or a reader that runs no
    /// JavaScript, is told before the runtime exists. See
    /// `impresspress_bundle::bundle::AppConfig::boot_notice_html`.
    #[serde(default)]
    pub boot_notice: Option<String>,
}

/// Where the app's title, as the boot shell shows it, comes from. `[app]`
/// gives exactly one of the two keys; a file with both or neither does not
/// parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppTitle {
    /// `title = "…"`: written in the configuration.
    Inline(String),
    /// `title_file = "<file>"`: a text file (relative to the directory
    /// holding `impresspress.toml`, like an overlay's `from`) whose one line
    /// is the title — for a build whose title is decided by an earlier build
    /// step. One trailing newline is not part of the title; an empty file or
    /// a second line is refused.
    File(String),
}

/// `[app]` as it is written, before the two title keys become one value.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAppConfig {
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    title_file: Option<String>,
    boot_redirect: String,
    #[serde(default)]
    boot_notice: Option<String>,
}

impl TryFrom<RawAppConfig> for AppConfig {
    type Error = &'static str;

    fn try_from(raw: RawAppConfig) -> Result<Self, Self::Error> {
        let title = match (raw.title, raw.title_file) {
            (Some(title), None) => AppTitle::Inline(title),
            (None, Some(file)) => AppTitle::File(file),
            (Some(_), Some(_)) => {
                return Err("[app] sets both `title` and `title_file`; give exactly one")
            }
            (None, None) => {
                return Err(
                    "[app] needs a `title` (or a `title_file` naming the file that holds it)",
                )
            }
        };
        Ok(Self {
            name: raw.name,
            title,
            boot_redirect: raw.boot_redirect,
            boot_notice: raw.boot_notice,
        })
    }
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
    /// `true` for throwaway-data deployments like `demo.impresspress.org`
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

impl Config {
    /// What this configuration asks the bundler for — the one place the two
    /// web flows (`sealed × web`, `embed × web`) turn `impresspress.toml` into
    /// an `impresspress_bundle` `AppConfig`. `repo_root` is the directory the
    /// file was found in; `[app] boot_notice` is read relative to it.
    pub fn bundle_app(
        &self,
        repo_root: &Path,
    ) -> anyhow::Result<impresspress_bundle::bundle::AppConfig> {
        let boot_notice_html = match &self.app.boot_notice {
            Some(path) => {
                let file = repo_root.join(path);
                Some(
                    std::fs::read_to_string(&file)
                        .map_err(|e| anyhow::anyhow!("read [app] boot_notice {file:?}: {e}"))?,
                )
            }
            None => None,
        };
        Ok(impresspress_bundle::bundle::AppConfig {
            app_name: Some(self.app.name.clone()),
            app_title: Some(self.app_title(repo_root)?),
            boot_redirect: Some(self.app.boot_redirect.clone()),
            extra_bypass_prefix: self.assets.extra_bypass_prefix.clone(),
            extra_bypass_exact: self.assets.extra_bypass_exact.clone(),
            opfs_wipe_on_recovery: self.assets.opfs_wipe_on_recovery,
            dev_enabled: self.dev.enabled,
            boot_notice_html,
        })
    }
}

impl Config {
    /// The app's title: `[app] title`, or the one line of `[app] title_file`
    /// (read relative to `repo_root`, the directory the configuration was
    /// found in).
    pub fn app_title(&self, repo_root: &Path) -> anyhow::Result<String> {
        let path = match &self.app.title {
            AppTitle::Inline(title) => return Ok(title.clone()),
            AppTitle::File(path) => path,
        };
        let file = repo_root.join(path);
        let text = std::fs::read_to_string(&file)
            .map_err(|e| anyhow::anyhow!("read [app] title_file {file:?}: {e}"))?;
        let title = text
            .strip_suffix('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .unwrap_or(&text);
        if title.trim().is_empty() {
            anyhow::bail!("[app] title_file {file:?} is empty; it holds the app's title");
        }
        if title.contains(['\n', '\r']) {
            anyhow::bail!("[app] title_file {file:?} has more than one line; it holds the app's title and nothing else");
        }
        Ok(title.to_string())
    }
}

/// Walk up from `start` looking for `impresspress.toml`; parse and return
/// `(config, repo_root)` where `repo_root` is the directory that contains
/// the file.
///
/// `Ok(None)` means no `impresspress.toml` exists in `start` or any parent —
/// the only outcome a caller may treat as "no config". An entry that exists
/// but cannot be read or parsed (a dangling symlink included) is an `Err`:
/// falling back to defaults, or to a parent's file, would silently drop every
/// setting the operator wrote.
pub fn find_and_load(start: &Path) -> anyhow::Result<Option<(Config, PathBuf)>> {
    let start = start
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("canonicalize {start:?}: {e}"))?;
    let mut cur: &Path = &start;
    loop {
        let candidate = cur.join("impresspress.toml");
        // `symlink_metadata`, not `is_file`: only "no entry at this path"
        // moves the search up. A dangling symlink, a directory or an I/O
        // error is this directory's config failing to load, and must not
        // hand the build to a parent directory's `impresspress.toml`.
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => {
                let text = std::fs::read_to_string(&candidate)
                    .map_err(|e| anyhow::anyhow!("read {candidate:?}: {e}"))?;
                let cfg = parse(&text).map_err(|e| anyhow::anyhow!("parse {candidate:?}: {e}"))?;
                return Ok(Some((cfg, cur.to_path_buf())));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(anyhow::anyhow!("stat {candidate:?}: {e}")),
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return Ok(None),
        }
    }
}

/// [`find_and_load`] for flows that cannot run without an `impresspress.toml`:
/// a missing file is an error too.
pub fn find_and_load_required(start: &Path) -> anyhow::Result<(Config, PathBuf)> {
    find_and_load(start)?.ok_or_else(|| {
        anyhow::anyhow!("no impresspress.toml found in {start:?} or any parent directory")
    })
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
        assert_eq!(cfg.app.title, AppTitle::Inline("Impresspress".to_string()));
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

    /// The title is written in the configuration or read from a file an
    /// earlier build step wrote — one or the other, and always one.
    #[test]
    fn the_title_is_given_once_inline_or_as_a_file() {
        let app = |lines: &str| format!("[app]\nname = \"x\"\nboot_redirect = \"/\"\n{lines}");
        let tmp = tempfile::tempdir().unwrap();

        let inline = parse(&app("title = \"Inline\"\n")).unwrap();
        assert_eq!(inline.app_title(tmp.path()).unwrap(), "Inline");

        let from_file = parse(&app("title_file = \"t/title.txt\"\n")).unwrap();
        std::fs::create_dir(tmp.path().join("t")).unwrap();
        // One trailing newline (either spelling) is the file's, not the title's.
        for (written, title) in [
            ("From a file\n", "From a file"),
            ("From a file\r\n", "From a file"),
            ("No newline", "No newline"),
            ("  kept as written \n", "  kept as written "),
        ] {
            std::fs::write(tmp.path().join("t/title.txt"), written).unwrap();
            assert_eq!(from_file.app_title(tmp.path()).unwrap(), title);
            assert_eq!(
                from_file
                    .bundle_app(tmp.path())
                    .unwrap()
                    .app_title
                    .as_deref(),
                Some(title)
            );
        }
        for bad in ["", "\n", "   \n", "one\ntwo\n", "one\n\n"] {
            std::fs::write(tmp.path().join("t/title.txt"), bad).unwrap();
            let err = from_file.app_title(tmp.path()).unwrap_err().to_string();
            assert!(err.contains("title_file"), "{bad:?}: {err}");
        }
        std::fs::remove_file(tmp.path().join("t/title.txt")).unwrap();
        let err = from_file.app_title(tmp.path()).unwrap_err().to_string();
        assert!(
            err.contains("title_file") && err.contains("title.txt"),
            "{err}"
        );

        let both = parse(&app("title = \"A\"\ntitle_file = \"b\"\n"))
            .unwrap_err()
            .to_string();
        assert!(both.contains("both"), "{both}");
        let neither = parse(&app("")).unwrap_err().to_string();
        assert!(neither.contains("title"), "{neither}");
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

        let (cfg, repo_root) = find_and_load(&nested).unwrap().unwrap();
        assert_eq!(cfg.app.name, "x");
        assert_eq!(
            repo_root.canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
    }

    #[test]
    fn find_config_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(find_and_load(tmp.path()).unwrap().is_none());
        let err = find_and_load_required(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("impresspress.toml"));
        assert!(err.contains("no"));
    }

    /// A dangling `impresspress.toml` symlink is this directory's config
    /// failing to load, not an absent file: the search must not walk up and
    /// pick the parent's valid config instead.
    #[cfg(unix)]
    #[test]
    fn find_config_dangling_symlink_is_an_error_not_a_walk_up() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("impresspress.toml"),
            "[app]\nname = \"parent\"\ntitle = \"P\"\nboot_redirect = \"/\"\n",
        )
        .unwrap();
        let child = tmp.path().join("child");
        std::fs::create_dir(&child).unwrap();
        std::os::unix::fs::symlink(child.join("missing.toml"), child.join("impresspress.toml"))
            .unwrap();

        let err = find_and_load(&child).unwrap_err().to_string();
        assert!(err.contains("impresspress.toml"), "{err}");
    }

    #[test]
    fn find_config_malformed_is_an_error_not_absent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("impresspress.toml"), "[app\n").unwrap();
        let err = find_and_load(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("parse"), "{err}");
    }

    #[test]
    fn dev_table_parses_and_defaults_off() {
        let cfg = parse(
            r#"
[app]
name = "x"
title = "X"
boot_redirect = "/"

[dev]
enabled = true
"#,
        )
        .unwrap();
        assert!(cfg.dev.enabled);

        // The sandbox is opt-in: a config that never mentions `[dev]` must
        // leave it off rather than inheriting whatever the last build used.
        let cfg = parse(
            r#"
[app]
name = "x"
title = "X"
boot_redirect = "/"
"#,
        )
        .unwrap();
        assert!(!cfg.dev.enabled);
    }

    #[test]
    fn reject_unknown_field_in_dev() {
        let err = parse(
            r#"
[app]
name = "x"
title = "X"
boot_redirect = "/"

[dev]
enabled = true
sandbox = "yes"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("sandbox"), "expected 'sandbox', got: {err}");
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
