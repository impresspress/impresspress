//! Sealed × web: assemble a static dist/ from the bundled impresspress-web
//! wasm + the user's frontend overlays + any blocks/.

use std::path::Path;

use anyhow::{anyhow, Result};

use crate::cli::{
    config,
    helpers::{blocks, http_server, overlays, wasm},
};

pub async fn build(repo_root: &Path, release: bool) -> Result<()> {
    // A malformed `impresspress.toml` fails the build here, before `dist/` is
    // wiped; only an absent file means "every knob at its default".
    let cfg = config::find_and_load(repo_root)?;

    // 1. Discover and build user blocks (if any).
    blocks::build_all(repo_root).await?;

    // 2. Prepare dist directory.
    let dist = repo_root.join("dist");
    if dist.exists() {
        std::fs::remove_dir_all(&dist).map_err(|e| anyhow!("clean dist/: {e}"))?;
    }
    std::fs::create_dir_all(&dist).map_err(|e| anyhow!("create dist/: {e}"))?;

    // 3. Resolve and write the impresspress-web wasm and JS glue.
    let wasm_bytes = wasm::resolve_impresspress_web_wasm()?;
    let wasm_path = dist.join("impresspress_web_bg.wasm");
    std::fs::write(&wasm_path, &*wasm_bytes).map_err(|e| anyhow!("write {wasm_path:?}: {e}"))?;

    let js_bytes = wasm::resolve_impresspress_web_js()?;
    let js_path = dist.join("impresspress_web.js");
    std::fs::write(&js_path, &*js_bytes).map_err(|e| anyhow!("write {js_path:?}: {e}"))?;

    // 3b. The inline-JS snippets the glue imports from. The THIRD part of a
    //     wasm-pack output, and not optional: `impresspress_web.js` opens with
    //     `import { … } from './snippets/<crate>-<hash>/js/bridge.js'`, so a
    //     dist without the tree cannot load its own module — the service
    //     worker self-destructs and the app serves its boot shell forever.
    //     `sw.js` has always had `/snippets/` on its bypass list; until this
    //     landed there was simply nothing there to bypass to.
    write_snippets(&dist, &wasm::resolve_impresspress_web_snippets()?)?;

    // 4. Run the bundler — content-hash assets + render templates.
    //    This calls impresspress_bundle::bundle::run, which writes the
    //    static shell (index.html, sw.js, loader.js) into dist/.
    let app = match cfg.as_ref() {
        Some((c, root)) => c.bundle_app(root)?,
        // No `impresspress.toml` — every knob, the sandbox included, stays
        // at its default.
        None => impresspress_bundle::bundle::AppConfig::default(),
    };

    impresspress_bundle::assets::write_to(&dist)?;
    // The bundler refuses a runtime compiled for another sandbox mode than
    // `[sandbox] mode`; it cannot know where this flow's runtime came from,
    // so the flow says how to get the right one.
    impresspress_bundle::bundle::run(&dist, repo_root, app).map_err(|e| {
        match e.downcast::<impresspress_bundle::bundle::sandbox_mode::RuntimeModeRefusal>() {
            Ok(refusal) => anyhow!("{refusal}\n{}", runtime_hint(refusal.configured)),
            Err(e) => e,
        }
    })?;

    // 5. Apply overlays from impresspress.toml if present, then the export
    //    shell a workspace names (`[sandbox] export_shell`) — after the
    //    bundler has listed the bundle's own files, so neither is listed.
    if let Some((cfg, root)) = cfg {
        overlays::apply_overlays(&cfg, &root, &dist)?;
        overlays::apply_export_shell(&cfg, &root, &dist)?;
    }

    let profile = if release { "release" } else { "dev" };
    println!("built sealed × web ({profile}) → dist/");
    Ok(())
}

/// Where the sealed web flow's runtime comes from, and how to give it one
/// compiled as `mode`: the flow bundles `impresspress-web` — the build baked
/// into this CLI (`crates/impresspress-web/pkg`, a plain build, when the CLI
/// was compiled), unless `IMPRESSPRESS_WEB_WASM` or the wasm-pack output
/// [`wasm::PKG_DIR_ENV`] names another.
fn runtime_hint(mode: impresspress_bundle::bundle::SandboxShell) -> String {
    use impresspress_bundle::bundle::SandboxShell;
    let features = match mode {
        SandboxShell::None => "",
        SandboxShell::Site => " --features browser-site-runtime",
        SandboxShell::Workspace => " --features browser-devtools",
    };
    let pkg_dir = wasm::PKG_DIR_ENV;
    let build = |out_dir: &str| {
        format!("wasm-pack build --target web --release --out-dir {out_dir} -- --locked{features}")
    };
    let baked = match mode {
        SandboxShell::None => format!(
            " (or rebuild the baked one: `{}` in crates/impresspress-web, then reinstall the \
             CLI, whose build bakes that directory)",
            build("pkg")
        ),
        SandboxShell::Site | SandboxShell::Workspace => String::new(),
    };
    format!(
        "This flow bundles impresspress-web's runtime: the build baked into this CLI, unless \
         IMPRESSPRESS_WEB_WASM or {pkg_dir} (a wasm-pack output) names another. For \
         '{mode}', build one with `{build}` in crates/impresspress-web and set {pkg_dir} to \
         that directory{baked}.",
        mode = mode.as_str(),
        build = build("<dir>"),
    )
}

/// Write `snippets` under `dist/snippets/`, creating the per-crate
/// directories wasm-bindgen named.
///
/// Every relative path is checked to be exactly that — relative, with no `..`
/// segment. The list comes from a directory the operator pointed at, so this
/// is not the difference between safe and unsafe by itself; it is the
/// difference between a typo landing in `dist/` and a typo landing anywhere
/// on the filesystem.
fn write_snippets(
    dist: &Path,
    snippets: &[(String, std::borrow::Cow<'static, [u8]>)],
) -> Result<()> {
    for (rel, bytes) in snippets {
        let rel_path = Path::new(rel);
        if rel_path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(anyhow!("snippet path {rel:?} is not a plain relative path"));
        }
        let dst = dist.join("snippets").join(rel_path);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(&dst, bytes).map_err(|e| anyhow!("write {}: {e}", dst.display()))?;
    }
    Ok(())
}

pub async fn serve(repo_root: &Path, release: bool, port: Option<u16>) -> Result<()> {
    // Web serve runs a static-file server over the wasm bundle; the wasm
    // runtime applies its own pending migrations when the bundle loads.
    build(repo_root, release).await?;
    let port = port.unwrap_or(8080);
    let dist = repo_root.join("dist");
    eprintln!("serving {} on http://127.0.0.1:{port}", dist.display());
    http_server::serve_static(&dist, port).await
}
