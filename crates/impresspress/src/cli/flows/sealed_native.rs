//! Sealed × native: prebuilt server bin already on PATH (this CLI binary).
//! `build` runs block discovery + frontend asset prep into the runtime's
//! storage path. `serve` does `build` then boots the in-process server
//! using cwd's .env via `cli::server::run`.

use std::path::Path;

use anyhow::Result;

use crate::cli::{
    config,
    helpers::{blocks, frontend, overlays},
};

const RUNTIME_SITE_REL: &str = "data/storage/wafer-run/web/site";

pub async fn build(repo_root: &Path, _release: bool) -> Result<()> {
    // 1. wafer build per block.
    blocks::build_all(repo_root).await?;

    // 2. Frontend copy.
    if let Some(fe) = frontend::find_frontend_dir(repo_root) {
        let dst = repo_root.join(RUNTIME_SITE_REL);
        frontend::copy_tree(&fe, &dst).map_err(|e| anyhow::anyhow!("copy frontend: {e}"))?;
    }

    // 3. Optional overlays from impresspress.toml.
    if let Ok((cfg, root)) = config::find_and_load(repo_root) {
        let dst = root.join(RUNTIME_SITE_REL);
        overlays::apply_overlays(&cfg, &root, &dst)?;
    }

    println!("ready: run `impresspress serve`");
    Ok(())
}

pub async fn serve(
    repo_root: &Path,
    release: bool,
    _port: Option<u16>,
    run_migrations: bool,
) -> Result<()> {
    build(repo_root, release).await?;
    crate::cli::server::run(repo_root, run_migrations).await
}
