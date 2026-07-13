pub mod build_id;
pub mod hash;
pub mod manifest;
pub mod rename;
pub mod template;

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};

/// Consumer-supplied configuration that controls how templates are rendered.
/// All fields are optional; sensible defaults are derived from the discovered
/// wasm-pack output pair when omitted.
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
    let manifest = manifest::AssetManifest {
        build_id: build_id.clone(),
        assets: manifest_assets,
    };
    manifest.write(&pkg_dir.join("asset-manifest.json"))?;

    // 5. Render templates.
    let base_name = pair.as_ref().map(|(b, _, _)| b.as_str()).unwrap_or("app");
    let vars = build_template_vars(
        build_id,
        wasm_js_val,
        wasm_bin_val,
        wasm_js_prefix_val,
        base_name,
        &app,
    );
    render_if_exists(pkg_dir, "sw.js.tmpl", "sw.js", &vars)?;
    render_if_exists(pkg_dir, "loader.js.tmpl", "loader.js", &vars)?;
    render_if_exists(pkg_dir, "index.html.tmpl", "index.html", &vars)?;

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
            let _ = std::fs::remove_file(entry.path());
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
    let extra_bypass = if app.extra_bypass_prefix.is_empty() {
        String::new()
    } else {
        let mut out = String::new();
        for prefix in &app.extra_bypass_prefix {
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
    std::fs::remove_file(&src).ok();
    Ok(())
}
