//! Sealed × native: prebuilt server bin already on PATH (this CLI binary).
//! `build` runs block discovery + frontend asset prep into the runtime's
//! storage path. `serve` does `build` then boots the in-process server
//! using cwd's .env via `impresspress_server::run`; `--port` replaces the
//! port of its `IMPRESSPRESS_LISTEN` address.

use std::path::Path;

use anyhow::Result;

use crate::cli::{
    config,
    helpers::{blocks, frontend, overlays},
};

pub async fn build(repo_root: &Path, _release: bool) -> Result<()> {
    // A malformed `impresspress.toml` fails the build here, before any work;
    // only an absent file means "no overlays".
    let cfg = config::find_and_load(repo_root)?;

    // 1. wafer build per block.
    blocks::build_all(repo_root).await?;

    // 2. Frontend copy.
    frontend::stage_native_site(repo_root).map_err(|e| anyhow::anyhow!("copy frontend: {e}"))?;

    // 3. Optional overlays from impresspress.toml.
    if let Some((cfg, root)) = cfg {
        let dst = frontend::native_site_dir(&root);
        overlays::apply_overlays(&cfg, &root, &dst)?;
    }

    println!("ready: run `impresspress serve`");
    Ok(())
}

pub async fn serve(repo_root: &Path, release: bool, port: Option<u16>) -> Result<()> {
    build(repo_root, release).await?;
    impresspress_server::run(
        repo_root,
        impresspress_server::IMPRESSPRESS_LISTENER_FLOW,
        impresspress_server::AppHooks::none(),
        port,
    )
    .await
}
