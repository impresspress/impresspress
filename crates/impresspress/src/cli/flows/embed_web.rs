//! Embed × web: today's impresspress-cli flow, with the asset writer + bundler
//! (`impresspress-bundle`) as a direct library call instead of a cargo subprocess.

use std::path::Path;

use anyhow::Result;
use tokio::process::Command;

use crate::cli::{
    cmd, config,
    helpers::{blocks, http_server, overlays},
};

pub async fn build(repo_root: &Path, release: bool) -> Result<()> {
    let (cfg, _) = config::find_and_load_required(repo_root)?;

    // 1. wafer build per block.
    blocks::build_all(repo_root).await?;

    // 2. wasm-pack the user's cdylib.
    let mut wp = Command::new("wasm-pack");
    let mut args = vec!["build".to_string(), "--target".into(), "web".into()];
    args.push(if release {
        "--release".into()
    } else {
        "--dev".into()
    });
    args.push("--out-dir".into());
    args.push(cfg.wasm.out_dir.clone());
    wp.args(&args).current_dir(repo_root);
    cmd::run("wasm-pack build", wp).await?;

    // 3. Bundle: write static assets + content-hash + render templates.
    let dist_dir = repo_root.join(&cfg.wasm.out_dir);
    let app = cfg.bundle_app(repo_root)?;
    impresspress_bundle::assets::write_to(&dist_dir)?;
    impresspress_bundle::bundle::run(&dist_dir, repo_root, app)?;
    // `release` no longer flips the bundle path — every build is hashed so
    // dev iterations don't get pinned to stale browser-cached modules.
    let _ = release;

    // 4. Apply overlays.
    overlays::apply_overlays(&cfg, repo_root, &dist_dir)?;

    let profile = if release { "release" } else { "dev" };
    println!("built {} ({profile}) → {}", cfg.app.name, cfg.wasm.out_dir);
    Ok(())
}

pub async fn serve(repo_root: &Path, release: bool, port: Option<u16>) -> Result<()> {
    // Web serve runs a static-file server over the wasm bundle; the wasm
    // runtime applies its own pending migrations when the bundle loads.
    build(repo_root, release).await?;
    let port = port.unwrap_or(8080);
    let cfg = config::find_and_load_required(repo_root)?.0;
    let dist = repo_root.join(&cfg.wasm.out_dir);
    eprintln!("serving {} on http://127.0.0.1:{port}", dist.display());
    http_server::serve_static(&dist, port).await
}
