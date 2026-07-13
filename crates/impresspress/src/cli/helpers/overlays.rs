//! Asset-overlay logic shared by the four flow quadrants. Reads
//! `[[assets.overlay]]` entries from `impresspress.toml` and copies each
//! `from` (relative to repo root) to `to` (relative to the dist dir).

use std::path::Path;

use anyhow::{anyhow, Result};

use crate::cli::config::Config;

pub fn apply_overlays(cfg: &Config, repo_root: &Path, dist_dir: &Path) -> Result<()> {
    for overlay in &cfg.assets.overlay {
        let src = repo_root.join(&overlay.from);
        let dst = dist_dir.join(&overlay.to);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| anyhow!("create dir {parent:?}: {e}"))?;
        }
        std::fs::copy(&src, &dst).map_err(|e| anyhow!("overlay {src:?} → {dst:?}: {e}"))?;
    }
    Ok(())
}

pub use crate::cli::config::OverlayEntry as Overlay;
