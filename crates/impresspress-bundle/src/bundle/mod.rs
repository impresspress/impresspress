pub mod build_id;
pub mod bypass;
pub mod hash;
pub mod manifest;
pub mod rename;
pub mod sandbox_mode;
pub mod shell_path;
pub mod template;

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use self::{
    bypass::{BypassRules, SEED_BYPASS_PREFIX},
    shell_path::{InvalidShellPath, ShellPath},
};

/// What `index.html.tmpl` wraps each place it shows [`AppConfig::app_title`]
/// in the page body in, so the title can be replaced by exact text. The
/// third place is `<title>` itself, which needs no wrapper.
///
/// For the development sandbox's export: the title is the export shell's
/// (`[app] title` of the bundle the sandbox deployment carries at
/// [`EXPORT_SHELL_ROOT`]), and the export ships that page as the boot shell
/// of a site that has a name of its own. The export restates the pair;
/// `crates/impresspress/tests/seed_bypass_prefix.rs` compares the spellings.
pub const APP_TITLE_OPEN: &str = "<span data-app-title>";
pub const APP_TITLE_CLOSE: &str = "</span>";

/// Where a development sandbox's deployment carries its EXPORT SHELL: a
/// complete bundle built with [`SandboxShell::Site`] — around the site
/// runtime's own wasm, with no workspace compiled in — which the sandbox's
/// export ships, file for file as its `asset-manifest.json` lists them, as
/// the shell of the site it hands the user. The `impresspress` CLI lays the
/// shell down here (`[sandbox] export_shell`), and `sw.js.tmpl`'s `__EXPORT_SHELL__`
/// renders this value into the one constant the worker hands
/// `initialize({ exportShell })` — one value, so the directory the files are
/// in and the address the runtime is told cannot disagree.
///
/// Deliberately NOT a bypass rule: the shell's files are for the worker's own
/// `fetch` to read, and a service worker never intercepts its own requests.
/// A page this worker controls that navigates here is answered by the
/// runtime; an uncontrolled load (first visit, hard reload) gets the static
/// file — the shell's own `index.html`.
pub const EXPORT_SHELL_ROOT: &str = "/__impresspress_dev/export-shell/";

/// Which half of the browser sandbox a bundle's service worker boots: `sw.js.tmpl`'s
/// `__SANDBOX__` placeholder renders [`Self::as_str`] into ONE constant,
/// `const SANDBOX = '…';`, which the check before `initialize()` and the
/// isolation-header passthrough both read.
///
/// The spelling is the same everywhere it appears: `[sandbox] mode = "site"`
/// in `impresspress.toml` (no `[sandbox]` table is [`Self::None`]), this
/// enum, and `const SANDBOX = 'site';` in the shipped `sw.js`.
///
/// The service worker states what it was BUILT for; the runtime wasm states
/// what it was COMPILED as — `impresspress-core`'s `SandboxMode`, decided by
/// its features (neither, `site-runtime` for [`Self::Site`], `block-dev` for
/// [`Self::Workspace`]; `impresspress-web`'s `browser-site-runtime` and
/// `browser-devtools` forward to them) and answered by
/// `compiled_sandbox_mode()`, which every runtime exports because
/// `impresspress-browser`, the crate every browser cdylib links, defines it;
/// the same crate writes the spelling into the wasm's
/// [`sandbox_mode::SANDBOX_MODE_SECTION`] custom section.
///
/// The two are compared twice. [`run`] reads the section of the wasm it is
/// bundling and refuses a mismatch before it touches the directory, so the
/// CLI never produces a bundle assembled from the wrong runtime. The worker
/// compares them again after loading the module and before `initialize()`,
/// for a bundle that did not come out of [`run`] whole — files hand-copied
/// between deployments, or a runtime swapped on the host afterwards: a
/// mismatch stops it there with stage `build`, which erases nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxShell {
    /// No sandbox: a plain bundle. The service worker neither bypasses
    /// [`SEED_BYPASS_PREFIX`] nor adds the isolation headers.
    #[default]
    None,
    /// The sandbox's runtime half alone — a site that ships a `seed/` and
    /// imports it on a cold boot (an exported sandbox). The service worker
    /// bypasses [`SEED_BYPASS_PREFIX`] and adds no isolation headers: a site
    /// wants its pages free to embed a third-party iframe.
    Site,
    /// The development sandbox itself: the runtime half plus `/b/dev`. The
    /// service worker bypasses [`SEED_BYPASS_PREFIX`] and answers every
    /// bypassed same-origin request itself, adding the cross-origin-isolation
    /// headers the in-browser compiler's worker needs.
    Workspace,
}

impl SandboxShell {
    /// The mode as `sw.js` spells it: `'none'`, `'site'` or `'workspace'` —
    /// the same words `[sandbox] mode` takes.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Site => "site",
            Self::Workspace => "workspace",
        }
    }

    /// The mode `spelling` names — exactly one of [`Self::as_str`]'s words,
    /// nothing else.
    pub fn from_spelling(spelling: &str) -> Option<Self> {
        [Self::None, Self::Site, Self::Workspace]
            .into_iter()
            .find(|mode| mode.as_str() == spelling)
    }

    /// Whether this bundle ships the sandbox's runtime half, which imports
    /// the seed served under [`SEED_BYPASS_PREFIX`] on a cold boot.
    pub const fn runs_the_site_runtime(self) -> bool {
        matches!(self, Self::Site | Self::Workspace)
    }
}

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
    /// The boot shell's own URL path — [`ShellPath`], whose module docs give
    /// what a valid one is and the static host's contract: the host must
    /// answer this path with the shell document. **Default: `/`.** Rendered
    /// into both `sw.js.tmpl` and `loader.js.tmpl` as `SHELL_URL`, and
    /// recorded in `asset-manifest.json` ([`manifest::AssetManifest::shell_path`]).
    /// Driven by `[app] shell_path` in `impresspress.toml`.
    ///
    /// [`run`] refuses a shell path any of the bundle's bypass rules matches:
    /// the worker would leave the shell's own address to the static host, so
    /// the runtime could never answer it — and a boot URL that is the shell's
    /// address would send the shell back to itself.
    pub shell_path: ShellPath,
    /// Additional URL path prefixes that the Service Worker's fetch handler
    /// should bypass (let the origin serve directly). Each entry joins the
    /// bundle's [`BypassRules`] as a prefix rule, rendered into `sw.js` as a
    /// `url.pathname.startsWith(<prefix>)` clause and into the rules
    /// `initialize()` hands the runtime.
    pub extra_bypass_prefix: Vec<String>,
    /// Additional exact URL paths the Service Worker's fetch handler should
    /// bypass. Unlike `extra_bypass_prefix` (a prefix rule), each entry is an
    /// exact rule, rendered as `url.pathname === <path>` — needed for `/` and
    /// `/index.html`, which cannot be expressed as a prefix. Listing `/` (or
    /// any other path the shell would stand at) needs [`Self::shell_path`]
    /// elsewhere: [`run`] refuses a shell path a bypass rule matches.
    pub extra_bypass_exact: Vec<String>,
    /// Whether loader.js's recovery path should wipe OPFS when the Service
    /// Worker self-destructs. **Default: false** — for production apps that
    /// store user data in OPFS (chat history, generated assets, settings),
    /// wiping on a self-destruct loop is silent data loss. The demo opts in
    /// via `impresspress build --target web --opfs-wipe-on-recovery` so the
    /// stale-schema migration scenario self-resolves without manual user
    /// action; other apps surface the error to the user instead and let
    /// them choose whether to clear data. Even when true, only a failure
    /// of the runtime's `initialize()` on the data it read is recovered from
    /// by wiping: a module that could not be loaded, a database that could
    /// not be read at all, a request the runtime died on and a boot that
    /// merely runs out of time all keep the data (`erasesFor` in
    /// `loader.js.tmpl`).
    pub opfs_wipe_on_recovery: bool,
    /// Which half of the browser sandbox this bundle's service worker boots
    /// — [`SandboxShell`]. **Default: [`SandboxShell::None`].** Driven by
    /// `[sandbox] mode` in `impresspress.toml`.
    pub sandbox: SandboxShell,
    /// Whether this bundle's deployment carries an export shell at
    /// [`EXPORT_SHELL_ROOT`]. **Default: false.** Only a
    /// [`SandboxShell::Workspace`] bundle may: [`run`] refuses it for any other
    /// mode, because the export shell is what the WORKSPACE's export copies.
    /// When set, `sw.js` hands `initialize({ exportShell })` that root; when
    /// not, `null`. Driven by `[sandbox] export_shell` in `impresspress.toml`,
    /// whose directory the `impresspress` CLI copies there after `run`
    /// returns — `run` itself writes no file for it.
    ///
    /// `run` does NOT require it of a workspace bundle. It could only require
    /// the flag, never the files — those are the CLI's to lay down — so the
    /// rule that a workspace carries a shell lives where both are known: the
    /// CLI's `[sandbox]` parser refuses a workspace without `export_shell`. A
    /// workspace bundle built some other way without one still boots, and its
    /// export refuses, naming the missing shell — never falls back to the
    /// sandbox's own runtime.
    pub export_shell: bool,
    /// An HTML fragment the boot shell (`index.html`) shows under its title.
    /// **Default: none.**
    ///
    /// The boot shell is the only document the static host serves, and until
    /// the service worker is installed it is the whole of what a visitor — or
    /// a reader that runs no JavaScript at all — gets: a title and
    /// "Loading...". An app whose first visitor needs to be told something
    /// before the runtime exists says it here. The fragment is the app's own
    /// markup, rendered verbatim.
    pub boot_notice_html: Option<String>,
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
    // Before anything in `pkg_dir` is touched: a configuration the bundle
    // cannot be built with, or a runtime it cannot be built around, leaves
    // the directory as it was.
    if app.export_shell && app.sandbox != SandboxShell::Workspace {
        anyhow::bail!(
            "an export shell is for a development sandbox's export to ship, but this \
             bundle's sandbox mode is '{}', not 'workspace'",
            app.sandbox.as_str()
        );
    }

    // --- Discovery -----------------------------------------------------------
    // Ahead of the cleanup below, which cannot remove what this finds: a
    // previous build's hashed copies are `{base}_bg-<hash>.wasm`, and this
    // looks for `_bg.wasm`.
    let pair = discover_wasm_pair(pkg_dir)?;
    match &pair {
        // The runtime must have been compiled as the mode `sw.js` is about to
        // be rendered for — see `SandboxShell`. A refusal is typed
        // (`RuntimeModeRefusal`), so a caller can add where its runtime came
        // from.
        Some((_, _, wasm)) => {
            let path = pkg_dir.join(wasm);
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            sandbox_mode::check(&path, &bytes, app.sandbox)?;
        }
        // Template-only: no runtime here to disagree with the mode.
        None => eprintln!(
            "warning: no *_bg.wasm found in {}; skipping asset hashing",
            pkg_dir.display()
        ),
    }

    // --- Bypass rules --------------------------------------------------------
    // Computed ONCE, here: `sw.js`'s fetch condition, the rules it hands the
    // runtime in `initialize()` and the manifest's `bypass` are all rendered
    // from this one value — see `bypass`'s module docs for why there must not
    // be a second list. Here rather than beside the templates because the
    // shell path is checked against it, and a refusal leaves `pkg_dir` as it
    // was. The glue's prefix is the discovered base name, which the hashing
    // below keeps (`{base}-<hash>.js`, `{base}_bg-<hash>.wasm`).
    let wasm_js_prefix = match &pair {
        Some((base, _, _)) => format!("/{base}"),
        // Template-only: a harmless fallback so the rules still render.
        None => "/app".to_string(),
    };
    let bypass = BypassRules::for_bundle(&wasm_js_prefix, &app);
    if let Some(rule) = bypass.rule_matching(app.shell_path.as_str()) {
        anyhow::bail!(
            "shell_path {:?} is on this bundle's service-worker bypass list ({rule}): the \
             worker would hand the boot shell's own address to the static host, so the \
             runtime could never answer it, and a boot that ends there would load the shell \
             again; take the path off the bypass list or give the shell another path",
            app.shell_path.as_str()
        );
    }

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

    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    let mut renamed: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();

    // --- Derive template vars from discovery + AppConfig ---------------------
    let (wasm_js_val, wasm_bin_val) = if let Some((_, js, wasm)) = &pair {
        // 1. Hash + rename the wasm, then write its hashed name into the
        //    glue, and only THEN hash + rename the glue.
        //
        //    The order is the point. The glue names the wasm
        //    (`'{base}_bg.wasm'` → `'{base}_bg-<hash>.wasm'`, exactly one
        //    such reference), so its bytes depend on the wasm's hash. Hashed
        //    before that rewrite, two builds that differed only in the wasm
        //    shipped a glue file with the SAME name and different contents —
        //    and, wherever the build id is a commit sha rather than a hash of
        //    these, a byte-identical `sw.js`: a browser comparing worker
        //    scripts saw no new deployment, and an old worker restarted
        //    later asked for a wasm the deploy had deleted. Hashed after, the
        //    glue's name follows the wasm's, and `sw.js` — which imports the
        //    glue by name — follows the glue's.
        let hash_and_rename = |filename: &String| -> Result<(String, std::path::PathBuf)> {
            let src = pkg_dir.join(filename);
            let bytes =
                std::fs::read(&src).with_context(|| format!("reading {}", src.display()))?;
            let h = hash::short_hash(&bytes);
            let new_path = rename::rename_with_hash(&src, &h)?;
            Ok((h, new_path))
        };
        let (wasm_hash, wasm_path) = hash_and_rename(wasm)?;
        let new_wasm_name = wasm_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        rename::rewrite_literal(
            &pkg_dir.join(js),
            &format!("'{wasm}'"),
            &format!("'{new_wasm_name}'"),
        )?;
        let (js_hash, js_path) = hash_and_rename(js)?;
        hashes.insert(wasm.clone(), wasm_hash);
        renamed.insert(wasm.clone(), wasm_path);
        hashes.insert(js.clone(), js_hash);
        renamed.insert(js.clone(), js_path);

        let hashed_js_name = renamed
            .get(js)
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let hashed_wasm_name = new_wasm_name;

        (format!("/{hashed_js_name}"), format!("/{hashed_wasm_name}"))
    } else {
        // No pair found — provide harmless fallbacks so template vars resolve.
        ("/app.js".to_string(), "/app_bg.wasm".to_string())
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
    //
    //    `sw.js`'s fetch condition and the rules it hands the runtime in
    //    `initialize()` are both rendered from the one `bypass` value above.
    let base_name = pair.as_ref().map(|(b, _, _)| b.as_str()).unwrap_or("app");
    let vars = build_template_vars(
        build_id.clone(),
        wasm_js_val,
        wasm_bin_val,
        &bypass,
        base_name,
        &app,
    );
    render_if_exists(pkg_dir, "sw.js.tmpl", "sw.js", &vars)?;
    render_if_exists(pkg_dir, "loader.js.tmpl", "loader.js", &vars)?;
    render_if_exists(pkg_dir, "index.html.tmpl", "index.html", &vars)?;

    // 6. The manifest, last: `assets` (the two logical names templates
    //    reference), `files` (the whole shell, for a runtime that needs
    //    to enumerate the static files of a bundle — see
    //    `AssetManifest::files`), `bypass` (the same rules value `sw.js`
    //    was rendered from in step 5 — see `AssetManifest::bypass`) and
    //    `sha256` (each listed file's digest, taken after every file is
    //    final — see `AssetManifest::sha256`).
    //
    //    `asset-manifest.json` names itself in `files`. That is deliberate
    //    and costs nothing: the listing is of NAMES, taken before the file
    //    is written, and the name is fixed — so a consumer copying every
    //    listed file gets the manifest too, which is what a faithful copy of
    //    the shell means. Nothing the CLI adds afterwards is listed either —
    //    `impresspress`'s `apply_overlays`, which lays the sandbox's `seed/`
    //    and compiler tree down after this returns, and `apply_export_shell`,
    //    which lays the export shell down at `EXPORT_SHELL_ROOT`: neither is
    //    there yet when the listing is taken.
    //
    //    `run` is NOT the only writer into this directory, though: `embed ×
    //    web` bundles in place in wasm-pack's own `--out-dir`, so the npm
    //    metadata wasm-pack wrote is already sitting beside the assets.
    //    `list_dist_files` holds those back — see `is_package_metadata`.
    let mut files = manifest::list_dist_files(pkg_dir)?;
    let manifest_name = manifest::ASSET_MANIFEST.to_string();
    if !files.contains(&manifest_name) {
        files.push(manifest_name);
        files.sort();
    }
    let sha256 = manifest::digest_files(pkg_dir, &files)?;
    let manifest = manifest::AssetManifest {
        build_id,
        assets: manifest_assets,
        files,
        bypass,
        shell_path: app.shell_path,
        sha256,
    };
    manifest.write(&pkg_dir.join(manifest::ASSET_MANIFEST))?;

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
    bypass: &BypassRules,
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

    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    vars.insert("BUILD_ID".to_string(), build_id);
    vars.insert("WASM_JS".to_string(), wasm_js);
    vars.insert("WASM_BIN".to_string(), wasm_bin);
    // The fetch handler's whole bypass condition, and the same rules as the
    // data `initialize()` hands the runtime — both from the one value.
    vars.insert("BYPASS_CONDITION".to_string(), bypass.render_condition());
    vars.insert("BYPASS_RULES".to_string(), bypass.render_data());
    // The page engines, from their one list (`assets::PAGE_ENGINES`): as the
    // JSON array `sw.js` hands `initialize({ pageEngines })`, and as the boot
    // shell's module script tags.
    let engines: Vec<String> = crate::assets::page_engine_scripts().collect();
    vars.insert(
        "PAGE_ENGINES".to_string(),
        serde_json::to_string(&engines).expect("a list of strings serializes"),
    );
    vars.insert(
        "PAGE_ENGINE_TAGS".to_string(),
        engines
            .iter()
            .map(|src| format!(r#"<script type="module" src="{src}"></script>"#))
            .collect::<Vec<_>>()
            .join("\n    "),
    );
    vars.insert("APP_NAME".to_string(), app_name);
    // `index.html` is the only template that shows the title, and it shows
    // it as text: in `<title>` and between `APP_TITLE_OPEN`/`_CLOSE`.
    // Escaped, so a title is never markup — and can never contain the
    // closing tag whoever replaces it looks for.
    vars.insert("APP_TITLE".to_string(), html_text(&app_title));
    vars.insert("BOOT_REDIRECT".to_string(), boot_redirect);
    // Rendered as a JavaScript value, `const SHELL_URL = __SHELL_URL__;`: the
    // path as a JSON string, so no character a valid path may carry (`'`
    // among them) can end the literal early.
    vars.insert(
        "SHELL_URL".to_string(),
        serde_json::to_string(app.shell_path.as_str()).expect("a string serializes"),
    );
    vars.insert(
        "BOOT_NOTICE".to_string(),
        app.boot_notice_html.clone().unwrap_or_default(),
    );
    vars.insert(
        "OPFS_WIPE_ON_RECOVERY".to_string(),
        if app.opfs_wipe_on_recovery {
            "true".to_string()
        } else {
            "false".to_string()
        },
    );
    // Rendered between the quotes of `const SANDBOX = '__SANDBOX__';` — one
    // of three fixed words, none of which needs escaping.
    vars.insert("SANDBOX".to_string(), app.sandbox.as_str().to_string());
    // Rendered as a JavaScript value, `const EXPORT_SHELL = __EXPORT_SHELL__;`:
    // the root as a JSON string, or `null` for a bundle that carries none.
    vars.insert(
        "EXPORT_SHELL".to_string(),
        if app.export_shell {
            serde_json::to_string(EXPORT_SHELL_ROOT).expect("a string serializes")
        } else {
            "null".to_string()
        },
    );
    vars
}

/// `text` as HTML text content.
fn html_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
