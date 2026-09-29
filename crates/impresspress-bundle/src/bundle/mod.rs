pub mod build_id;
pub mod hash;
pub mod manifest;
pub mod rename;
pub mod template;

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};

/// URL prefix the development sandbox's seed bundle is served under, added to
/// the service worker's bypass list whenever [`AppConfig::dev_enabled`].
///
/// The same value as `impresspress_core::blocks::dev::seed::ROOT`. It is
/// restated rather than imported because this crate deliberately depends on no
/// impresspress crate — it is native bundling tooling that the wasm32 runtime
/// never compiles — and pulling `impresspress-core` in for one string would
/// invert that.
pub const SEED_BYPASS_PREFIX: &str = "/seed/";

/// Consumer-supplied configuration that controls how templates are rendered.
/// All fields are optional; sensible defaults are derived from the discovered
/// wasm-pack output pair when omitted.
#[derive(Default)]
pub struct AppConfig {
    /// Log prefix shown in sw.js / loader.js console messages
    /// (e.g. `"impresspress-web"`). Defaults to the discovered base name.
    pub app_name: Option<String>,
    /// Title rendered into `<title>` and `<h1>` in index.html.
    /// Defaults to the discovered base name with underscores replaced by
    /// spaces.
    pub app_title: Option<String>,
    /// URL the loader navigates to after the Service Worker activates.
    /// Defaults to `"/"`.
    pub boot_redirect: Option<String>,
    /// Additional URL path prefixes that the Service Worker's fetch handler
    /// should bypass (let the origin serve directly). Each entry is
    /// appended to the default bypass list in `sw.js.tmpl` as a
    /// `url.pathname.startsWith(<prefix>)` clause via the `__EXTRA_BYPASS__`
    /// placeholder.
    pub extra_bypass_prefix: Vec<String>,
    /// Additional exact URL paths the Service Worker's fetch handler should
    /// bypass. Unlike `extra_bypass_prefix` (rendered as `startsWith`), each
    /// entry renders as `url.pathname === <path>` via the
    /// `__EXTRA_BYPASS_EXACT__` placeholder — needed for `/` and
    /// `/index.html`, which cannot be expressed as a prefix.
    pub extra_bypass_exact: Vec<String>,
    /// Whether loader.js's recovery path should wipe OPFS when the Service
    /// Worker self-destructs. **Default: false** — for production apps that
    /// store user data in OPFS (chat history, generated assets, settings),
    /// wiping on a self-destruct loop is silent data loss. The demo opts in
    /// via `impresspress build --target web --opfs-wipe-on-recovery` so the
    /// stale-schema migration scenario self-resolves without manual user
    /// action; other apps surface the error to the user instead and let
    /// them choose whether to clear data.
    pub opfs_wipe_on_recovery: bool,
    /// Whether the Service Worker boots the runtime with the browser
    /// development sandbox on: `sw.js.tmpl`'s `__DEV_ENABLED__` placeholder
    /// renders one constant, `const DEV_ENABLED = true;`, which both
    /// `initialize({ dev: DEV_ENABLED })` and the isolation-header
    /// passthrough read. **Default: false.**
    /// Driven by `[dev] enabled` in `impresspress.toml`; the runtime still
    /// needs to have been compiled with `impresspress-web/browser-devtools`
    /// for the flag to register anything.
    pub dev_enabled: bool,
}

/// Discover the wasm-pack output pair (`{base}.js` + `{base}_bg.wasm`) in
/// `pkg_dir` by scanning for files that end with `_bg.wasm`.
///
/// Returns `Some((base, js_filename, wasm_filename))` or `None` when nothing
/// is found (caller may still run in template-only mode).
///
/// Errors when more than one `_bg.wasm` file is found (ambiguous).
fn discover_wasm_pair(pkg_dir: &Path) -> Result<Option<(String, String, String)>> {
    let mut candidates: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(pkg_dir)
        .with_context(|| format!("reading pkg dir {}", pkg_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with("_bg.wasm") {
            // strip the `_bg.wasm` suffix to get the base name
            let base = name[..name.len() - "_bg.wasm".len()].to_string();
            candidates.push(base);
        }
    }
    match candidates.len() {
        0 => Ok(None),
        1 => {
            let base = candidates.remove(0);
            let js = format!("{base}.js");
            let wasm = format!("{base}_bg.wasm");
            Ok(Some((base, js, wasm)))
        }
        n => anyhow::bail!(
            "found {n} *_bg.wasm files in {}; expected at most one (wasm-pack emits one pair per crate)",
            pkg_dir.display()
        ),
    }
}

/// Convert a wasm-pack base name to a human-readable title:
/// underscores → spaces, each word title-cased.
fn base_to_title(base: &str) -> String {
    base.split('_')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn run(pkg_dir: &Path, repo_dir: &Path, app: AppConfig) -> Result<()> {
    // --- Cleanup -------------------------------------------------------------
    // Dev rebuilds re-run wasm-pack, which writes the un-hashed pair (e.g.
    // `gizza_ai.js`, `gizza_ai_bg.wasm`) and then hits the same hashing path
    // we're about to run again. Without cleanup, hashed copies from prior
    // builds (`gizza_ai-abc123.js`, `gizza_ai_bg-def456.wasm`) accumulate in
    // `pkg/`. Worse, a stale Service Worker registered against an old hash
    // can keep finding the file via the SW bypass list and serve outdated
    // bytes long after the user thought it was gone. Sweep them up so each
    // build leaves a clean directory.
    remove_previously_hashed(pkg_dir)?;

    // --- Discovery -----------------------------------------------------------
    let pair = discover_wasm_pair(pkg_dir)?;
    if pair.is_none() {
        eprintln!(
            "warning: no *_bg.wasm found in {}; skipping asset hashing",
            pkg_dir.display()
        );
    }

    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    let mut renamed: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();

    // --- Derive template vars from discovery + AppConfig ---------------------
    let (wasm_js_val, wasm_bin_val, wasm_js_prefix_val) = if let Some((base, js, wasm)) = &pair {
        // 1. Hash + rename the discovered pair.
        for filename in &[js, wasm] {
            let src = pkg_dir.join(filename);
            let bytes =
                std::fs::read(&src).with_context(|| format!("reading {}", src.display()))?;
            let h = hash::short_hash(&bytes);
            let new_path = rename::rename_with_hash(&src, &h)?;
            hashes.insert((*filename).clone(), h);
            renamed.insert((*filename).clone(), new_path);
        }

        // 2. Rewrite the cross-reference inside the glue JS:
        //    `'{base}_bg.wasm'` → `'{base}_bg-<hash>.wasm'`.
        let js_renamed = renamed.get(js).unwrap();
        let old_literal = format!("'{wasm}'");
        let new_wasm_name = renamed
            .get(wasm)
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let new_literal = format!("'{new_wasm_name}'");
        // wasm-bindgen glue has exactly one such reference.
        rename::rewrite_literal(js_renamed, &old_literal, &new_literal)?;

        let hashed_js_name = renamed
            .get(js)
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let hashed_wasm_name = new_wasm_name;

        (
            format!("/{hashed_js_name}"),
            format!("/{hashed_wasm_name}"),
            format!("/{base}"),
        )
    } else {
        // No pair found — provide harmless fallbacks so template vars resolve.
        (
            "/app.js".to_string(),
            "/app_bg.wasm".to_string(),
            "/app".to_string(),
        )
    };

    // 3. Build ID.
    let asset_hashes_ordered: Vec<&str> = hashes.values().map(|h| h.as_str()).collect();
    let build_id = build_id::build_id(repo_dir, &asset_hashes_ordered);

    // 4. Manifest (only the discovered pair goes in).
    let mut manifest_assets = BTreeMap::new();
    if let Some((_, js, wasm)) = &pair {
        manifest_assets.insert(
            js.clone(),
            format!(
                "/{}",
                renamed
                    .get(js)
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
            ),
        );
        manifest_assets.insert(
            wasm.clone(),
            format!(
                "/{}",
                renamed
                    .get(wasm)
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
            ),
        );
    }
    // 5. Render templates. BEFORE the manifest is written, because the
    //    manifest now enumerates the directory and the rendered `sw.js` /
    //    `loader.js` / `index.html` have to be in it — a listing taken first
    //    would name three `*.tmpl` files that no longer exist and omit the
    //    three real ones.
    let base_name = pair.as_ref().map(|(b, _, _)| b.as_str()).unwrap_or("app");
    let vars = build_template_vars(
        build_id.clone(),
        wasm_js_val,
        wasm_bin_val,
        wasm_js_prefix_val,
        base_name,
        &app,
    );
    render_if_exists(pkg_dir, "sw.js.tmpl", "sw.js", &vars)?;
    render_if_exists(pkg_dir, "loader.js.tmpl", "loader.js", &vars)?;
    render_if_exists(pkg_dir, "index.html.tmpl", "index.html", &vars)?;

    // 6. The manifest, last: `assets` (the two logical names templates
    //    reference) plus `files` (the whole shell, for a runtime that needs
    //    to enumerate the static files it was shipped inside of — see
    //    `AssetManifest::files`).
    //
    //    `asset-manifest.json` names itself in `files`. That is deliberate
    //    and costs nothing: the listing is of NAMES, taken before the file
    //    is written, and the name is fixed — so a consumer copying every
    //    listed file gets the manifest too, which is what a faithful copy of
    //    the shell means. Nothing an overlay adds afterwards
    //    (`impresspress`'s `apply_overlays`, which lays the sandbox's `seed/`
    //    and compiler tree down after this returns) is listed either — it is
    //    not there yet when the listing is taken.
    //
    //    `run` is NOT the only writer into this directory, though: `embed ×
    //    web` bundles in place in wasm-pack's own `--out-dir`, so the npm
    //    metadata wasm-pack wrote is already sitting beside the assets.
    //    `list_dist_files` holds those back — see `is_package_metadata`.
    let mut files = manifest::list_dist_files(pkg_dir)?;
    let manifest_name = "asset-manifest.json".to_string();
    if !files.contains(&manifest_name) {
        files.push(manifest_name);
        files.sort();
    }
    let manifest = manifest::AssetManifest {
        build_id,
        assets: manifest_assets,
        files,
    };
    manifest.write(&pkg_dir.join("asset-manifest.json"))?;

    Ok(())
}

/// Sweep up `{base}-{8-hex}.js` and `{base}_bg-{8-hex}.wasm` files in
/// `pkg_dir` left behind by previous bundle runs. Cheap (a single `read_dir`
/// scan) and safe — the regex matches only the exact short-hash format we
/// emit, so user files like `app-1.js` or `app_bg-final.wasm` are untouched.
fn remove_previously_hashed(pkg_dir: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(pkg_dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if is_hashed_artifact(&name) {
            // Best-effort, but not silent. A stale hashed artifact that
            // survives is served alongside the new one and never expires
            // (these filenames are immutable-cached), so an operator wants to
            // know which file did not go.
            if let Err(error) = std::fs::remove_file(entry.path()) {
                eprintln!(
                    "warning: could not remove stale bundle artifact {}: {error}",
                    entry.path().display()
                );
            }
        }
    }
    Ok(())
}

/// Match `*-XXXXXXXX.js` or `*_bg-XXXXXXXX.wasm` where X is hex.
fn is_hashed_artifact(name: &str) -> bool {
    if let Some(stem) = name.strip_suffix(".js") {
        return ends_with_short_hash(stem, "-");
    }
    if let Some(stem) = name.strip_suffix(".wasm") {
        return ends_with_short_hash(stem, "_bg-");
    }
    false
}

fn ends_with_short_hash(stem: &str, sep: &str) -> bool {
    let Some(idx) = stem.rfind(sep) else {
        return false;
    };
    let suffix = &stem[idx + sep.len()..];
    suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_hexdigit())
}

/// Build the complete template variable map from the resolved values and
/// consumer-supplied `AppConfig` overrides.
fn build_template_vars(
    build_id: String,
    wasm_js: String,
    wasm_bin: String,
    wasm_js_prefix: String,
    base_name: &str,
    app: &AppConfig,
) -> BTreeMap<String, String> {
    let app_name = app
        .app_name
        .clone()
        .unwrap_or_else(|| base_name.to_string());
    let app_title = app
        .app_title
        .clone()
        .unwrap_or_else(|| base_to_title(base_name));
    let boot_redirect = app.boot_redirect.clone().unwrap_or_else(|| "/".to_string());

    // Render extra bypass prefixes into a chain of OR'd startsWith calls that
    // slot into `sw.js.tmpl` via the `__EXTRA_BYPASS__` placeholder. When the
    // list is empty we expand to the empty string, leaving the existing
    // bypass expression intact. Each entry is quoted and escaped defensively
    // against single quotes in the path.
    // The development sandbox's seed bundle is served by the static host, not
    // by the runtime: on a cold boot the service worker fetches
    // `/seed/manifest.json` and imports generation 0 from it, and a page
    // asking for the same files must reach the host too. A runtime that
    // intercepted the prefix would answer from the published site — which,
    // on the boot that needs the seed, is empty. Added here rather than by
    // every consumer, so "the sandbox is on" is the only thing an app has to
    // say. See `impresspress_core::blocks::dev::seed::ROOT`, which this
    // crate cannot import (it depends on nothing of impresspress's).
    let mut prefixes: Vec<&str> = app.extra_bypass_prefix.iter().map(String::as_str).collect();
    if app.dev_enabled && !prefixes.contains(&SEED_BYPASS_PREFIX) {
        prefixes.push(SEED_BYPASS_PREFIX);
    }
    let extra_bypass = if prefixes.is_empty() {
        String::new()
    } else {
        let mut out = String::new();
        for prefix in prefixes {
            let escaped = prefix.replace('\\', "\\\\").replace('\'', "\\'");
            out.push_str(" || url.pathname.startsWith('");
            out.push_str(&escaped);
            out.push_str("')");
        }
        out
    };

    let extra_bypass_exact = if app.extra_bypass_exact.is_empty() {
        String::new()
    } else {
        let mut out = String::new();
        for path in &app.extra_bypass_exact {
            let escaped = path.replace('\\', "\\\\").replace('\'', "\\'");
            out.push_str(" || url.pathname === '");
            out.push_str(&escaped);
            out.push('\'');
        }
        out
    };

    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    vars.insert("BUILD_ID".to_string(), build_id);
    vars.insert("WASM_JS".to_string(), wasm_js);
    vars.insert("WASM_BIN".to_string(), wasm_bin);
    vars.insert("WASM_JS_PREFIX".to_string(), wasm_js_prefix);
    vars.insert("APP_NAME".to_string(), app_name);
    vars.insert("APP_TITLE".to_string(), app_title);
    vars.insert("BOOT_REDIRECT".to_string(), boot_redirect);
    vars.insert("EXTRA_BYPASS".to_string(), extra_bypass);
    vars.insert("EXTRA_BYPASS_EXACT".to_string(), extra_bypass_exact);
    vars.insert(
        "OPFS_WIPE_ON_RECOVERY".to_string(),
        if app.opfs_wipe_on_recovery {
            "true".to_string()
        } else {
            "false".to_string()
        },
    );
    // Rendered into `initialize({ dev: __DEV_ENABLED__ })` — a JS boolean
    // literal, not a string, so the runtime reads it back as `Some(bool)`.
    vars.insert(
        "DEV_ENABLED".to_string(),
        if app.dev_enabled {
            "true".to_string()
        } else {
            "false".to_string()
        },
    );
    vars
}

fn render_if_exists(
    pkg_dir: &Path,
    src_name: &str,
    out_name: &str,
    vars: &BTreeMap<String, String>,
) -> Result<()> {
    let src = pkg_dir.join(src_name);
    if !src.exists() {
        return Ok(());
    }
    template::render_to_file(&src, &pkg_dir.join(out_name), vars)?;
    // The rendered output is written; failing to delete the template it came
    // from leaves an unrendered `{{VAR}}` file in the published bundle. Not
    // fatal — the rendered file is the one anything loads — but it must not
    // vanish from the build log.
    if let Err(error) = std::fs::remove_file(&src) {
        eprintln!(
            "warning: rendered {out_name} but could not remove its template {}: {error}",
            src.display()
        );
    }
    Ok(())
}
