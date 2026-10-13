//! Resolves the impresspress-web wasm, JS glue and inline-JS snippets for the
//! sealed × web flow.
//!
//! # Why three things and not two
//!
//! A `wasm-pack --target web` output is a triple: `<name>_bg.wasm`,
//! `<name>.js`, and the `snippets/<crate>-<hash>/…` tree the glue imports
//! from (wasm-bindgen puts every `#[wasm_bindgen(module = "/js/…")]` file
//! there). The glue's first statement is an `import` from that tree, so a
//! bundle missing it fails to load its module at all — `sw.js` self-destructs
//! and the app serves its boot shell forever. Every resolver here therefore
//! has a snippets counterpart, and [`resolve_impresspress_web_snippets`] is
//! not optional for a caller that writes the other two.
//!
//! # The overrides
//!
//! * [`PKG_DIR_ENV`] — a whole wasm-pack output directory. The one to use:
//!   the three parts cannot disagree about which build they came from.
//! * `IMPRESSPRESS_WEB_WASM` / `IMPRESSPRESS_WEB_JS` — one file each. Kept
//!   because they predate the directory override and because substituting a
//!   single artifact is occasionally what you want; they take precedence over
//!   [`PKG_DIR_ENV`] for the file they name, and they say nothing about
//!   snippets (which then come from the directory override, or from the bake).
//! * Otherwise the bytes baked into this binary by `build.rs`.

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};

/// Environment variable naming a `wasm-pack --target web --out-dir <dir>`
/// output directory to bundle instead of the baked-in build.
pub const PKG_DIR_ENV: &str = "IMPRESSPRESS_WEB_PKG_DIR";

/// A resolved [`PKG_DIR_ENV`] directory and the artifact stem wasm-pack
/// declared for it.
struct PkgDir {
    dir: PathBuf,
    /// `impresspress_web`, from which `<stem>_bg.wasm` and `<stem>.js` follow.
    stem: String,
}

/// Read [`PKG_DIR_ENV`], if it is set.
///
/// The stem comes from the directory's own `package.json` (`main`, or
/// `module` for a bundler-target output) rather than from a glob over `*.js`:
/// a pkg directory that has been post-processed by
/// `impresspress_bundle::bundle::run` holds hashed copies beside the
/// originals, and guessing between them is exactly the implicit mapping this
/// repo bans. wasm-pack writes the field; this reads it.
fn pkg_dir() -> Result<Option<PkgDir>> {
    let Ok(raw) = std::env::var(PKG_DIR_ENV) else {
        return Ok(None);
    };
    let dir = PathBuf::from(&raw);
    if !dir.is_dir() {
        return Err(anyhow!(
            "{PKG_DIR_ENV} points at {raw:?} but that is not a directory"
        ));
    }
    let manifest_path = dir.join("package.json");
    let manifest = std::fs::read_to_string(&manifest_path).with_context(|| {
        format!(
            "{PKG_DIR_ENV} points at {raw:?}, which has no readable package.json — it must be a \
             `wasm-pack --target web --out-dir <dir>` output"
        )
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    let entry = manifest
        .get("main")
        .or_else(|| manifest.get("module"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow!(
                "{} declares neither `main` nor `module`, so the JS glue cannot be identified",
                manifest_path.display()
            )
        })?;
    let stem = entry
        .strip_suffix(".js")
        .ok_or_else(|| {
            anyhow!(
                "{} declares `{entry}`, which is not a .js file",
                manifest_path.display()
            )
        })?
        .to_string();
    Ok(Some(PkgDir { dir, stem }))
}

fn read(path: &Path, what: &str) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| anyhow!("read {what} {}: {e}", path.display()))
}

/// Environment variable naming one wasm file to bundle instead of the
/// baked-in build — ahead of [`PKG_DIR_ENV`] for the wasm.
pub const WASM_ENV: &str = "IMPRESSPRESS_WEB_WASM";

/// Where [`resolve_impresspress_web_wasm`] took the wasm from — so a caller
/// that refuses it can name the file the operator supplied, not its own copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmSource {
    /// The file [`WASM_ENV`] names.
    WasmEnv(PathBuf),
    /// `<stem>_bg.wasm` in the directory [`PKG_DIR_ENV`] names.
    PkgDir(PathBuf),
    /// The `include_bytes!` bake of `crates/impresspress-web/pkg`.
    Baked,
}

/// The resolved wasm and where it came from.
pub struct ResolvedWasm {
    pub bytes: Cow<'static, [u8]>,
    pub source: WasmSource,
}

/// Resolution order:
/// 1. [`WASM_ENV`] (must point at an existing file)
/// 2. [`PKG_DIR_ENV`]`/<stem>_bg.wasm`
/// 3. the `include_bytes!` bake (always available)
pub fn resolve_impresspress_web_wasm() -> Result<ResolvedWasm> {
    if let Ok(p) = std::env::var(WASM_ENV) {
        let path = PathBuf::from(&p);
        if !path.is_file() {
            return Err(anyhow!(
                "{WASM_ENV} points at {p:?} but the file does not exist"
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {p:?}: {e}"))?;
        return Ok(ResolvedWasm {
            bytes: Cow::Owned(bytes),
            source: WasmSource::WasmEnv(path),
        });
    }
    if let Some(pkg) = pkg_dir()? {
        let path = pkg.dir.join(format!("{}_bg.wasm", pkg.stem));
        return Ok(ResolvedWasm {
            bytes: Cow::Owned(read(&path, "the wasm named by")?),
            source: WasmSource::PkgDir(path),
        });
    }
    Ok(ResolvedWasm {
        bytes: Cow::Borrowed(crate::IMPRESSPRESS_WEB_WASM),
        source: WasmSource::Baked,
    })
}

/// Resolution order:
/// 1. `IMPRESSPRESS_WEB_JS` (must point at an existing file)
/// 2. [`PKG_DIR_ENV`]`/<stem>.js`
/// 3. the `include_bytes!` bake (always available)
pub fn resolve_impresspress_web_js() -> Result<Cow<'static, [u8]>> {
    if let Ok(p) = std::env::var("IMPRESSPRESS_WEB_JS") {
        let path = PathBuf::from(&p);
        if !path.is_file() {
            return Err(anyhow!(
                "IMPRESSPRESS_WEB_JS points at {p:?} but the file does not exist"
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {p:?}: {e}"))?;
        return Ok(Cow::Owned(bytes));
    }
    if let Some(pkg) = pkg_dir()? {
        let path = pkg.dir.join(format!("{}.js", pkg.stem));
        return Ok(Cow::Owned(read(&path, "the JS glue named by")?));
    }
    Ok(Cow::Borrowed(crate::IMPRESSPRESS_WEB_JS))
}

/// Every inline-JS snippet, as `(path relative to `snippets/`, contents)`.
///
/// Resolution order:
/// 1. [`PKG_DIR_ENV`]`/snippets/**` — the whole tree, recursively
/// 2. the `include_bytes!` bake
///
/// There is deliberately no single-file override: a snippet is identified by
/// its wasm-bindgen-assigned `<crate>-<hash>` directory, so naming one in
/// isolation could only ever be a guess at which build it belongs to.
///
/// An override directory with no `snippets/` — or an empty one — is an error,
/// not an empty list. `impresspress-browser` declares its bridge with
/// `#[wasm_bindgen(module = "/js/bridge.js")]`, so every wasm-pack output this
/// override can legitimately point at has that tree; one without it is stale
/// or half-written, and a `dist/` built from it cannot load its own module.
/// `build.rs` applies the same rule to the baked list.
pub fn resolve_impresspress_web_snippets() -> Result<Vec<(String, Cow<'static, [u8]>)>> {
    if let Some(pkg) = pkg_dir()? {
        let root = pkg.dir.join("snippets");
        let mut out = Vec::new();
        if root.is_dir() {
            collect_snippets(&root, &root, &mut out)?;
        }
        // Same rule as `build.rs`'s bake, at the override that bypasses it: the
        // JS glue's first statement imports from `./snippets/…`, so a `pkg/`
        // without that tree produces a `dist/` that cannot load its own module.
        // An override pointing at a stale or half-written directory is the one
        // way to reach this, and failing here names it.
        if out.is_empty() {
            return Err(anyhow!(
                "{} has no inline-JS snippets under `snippets/`; the JS glue imports from there, \
                 so a bundle built from this directory cannot load its own module. Re-run the \
                 wasm-pack build for `impresspress-web`, or unset the pkg-dir override to use \
                 the snippets baked into this binary.",
                pkg.dir.display()
            ));
        }
        // Sorted so a bundle's contents do not depend on directory iteration
        // order — the same reason `build.rs` sorts the baked list.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        return Ok(out);
    }
    Ok(crate::IMPRESSPRESS_WEB_SNIPPETS
        .iter()
        .map(|(path, bytes)| ((*path).to_string(), Cow::Borrowed(*bytes)))
        .collect())
}

fn collect_snippets(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Cow<'static, [u8]>)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| anyhow!("read dir {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| anyhow!("read dir entry under {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_snippets(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| anyhow!("{} is not under {}: {e}", path.display(), root.display()))?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, Cow::Owned(read(&path, "snippet")?)));
        }
    }
    Ok(())
}
