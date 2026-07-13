//! Cross-compile the consumer crate via `worker-build`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

/// Install `worker-build` pinned to `^0.7`. We always run this — `cargo
/// install` is a no-op when the requested version is already present, and
/// will downgrade if a newer (incompatible) version was installed.
///
/// Pin reason: worker-build 0.8.x rejects `worker < 0.8` and changed its
/// output layout. Matches the version pin in `wrangler::base_toml`.
///
/// # Errors
///
/// Returns an error if the install subprocess fails to spawn or exits
/// non-zero.
pub async fn ensure_worker_build_installed() -> Result<()> {
    let install = Command::new("cargo")
        .args(["install", "worker-build", "--version", "^0.7", "--quiet"])
        .status()
        .await
        .context("run `cargo install worker-build --version ^0.7`")?;
    if !install.success() {
        bail!(
            "cargo install worker-build --version ^0.7 failed (exit {:?})",
            install.code()
        );
    }
    Ok(())
}

/// Cross-compile the consumer crate to `wasm32-unknown-unknown` via
/// `worker-build`.
///
/// Uses `tokio::process::Command` because the worker-build subprocess is
/// long-running (full wasm32 cargo build of the consumer crate plus the
/// wasm-bindgen post-processing step); blocking on `status()` from a
/// `std::process::Command` would freeze the tokio worker thread.
///
/// # Errors
///
/// Returns an error if `worker-build` cannot be installed, fails to spawn,
/// or exits non-zero.
pub async fn run(repo_root: &Path, release: bool) -> Result<()> {
    ensure_worker_build_installed().await?;

    let mut cmd = Command::new("worker-build");
    cmd.current_dir(repo_root)
        .args(["--no-default-features", "--features", "target-cloudflare"]);
    if release {
        cmd.arg("--release");
    }
    let status = cmd.status().await.context("run worker-build")?;
    if !status.success() {
        bail!("worker-build failed (exit {:?})", status.code());
    }
    Ok(())
}
