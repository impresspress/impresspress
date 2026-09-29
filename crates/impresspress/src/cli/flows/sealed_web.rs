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
        Some((c, _)) => impresspress_bundle::bundle::AppConfig {
            app_name: Some(c.app.name.clone()),
            app_title: Some(c.app.title.clone()),
            boot_redirect: Some(c.app.boot_redirect.clone()),
            extra_bypass_prefix: c.assets.extra_bypass_prefix.clone(),
            extra_bypass_exact: c.assets.extra_bypass_exact.clone(),
            opfs_wipe_on_recovery: c.assets.opfs_wipe_on_recovery,
            dev_enabled: c.dev.enabled,
        },
        // No `impresspress.toml` — every knob, the sandbox included, stays
        // at its default.
        None => impresspress_bundle::bundle::AppConfig::default(),
    };

    impresspress_bundle::assets::write_to(&dist)?;
    impresspress_bundle::bundle::run(&dist, repo_root, app)?;

    // 5. Apply overlays from impresspress.toml if present.
    if let Some((cfg, root)) = cfg {
        overlays::apply_overlays(&cfg, &root, &dist)?;
    }

    let profile = if release { "release" } else { "dev" };
    println!("built sealed × web ({profile}) → dist/");
    Ok(())
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

pub async fn serve(
    repo_root: &Path,
    release: bool,
    port: Option<u16>,
    _run_migrations: bool,
) -> Result<()> {
    // Web serve runs a static-file server over the wasm bundle; the
    // wasm itself owns its own runtime-side migration state. The flag is
    // accepted for CLI-symmetry but has nothing to do at this layer.
    build(repo_root, release).await?;
    let port = port.unwrap_or(8080);
    let dist = repo_root.join("dist");
    eprintln!("serving {} on http://127.0.0.1:{port}", dist.display());
    http_server::serve_static(&dist, port).await
}
