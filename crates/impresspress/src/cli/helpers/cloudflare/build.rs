//! Cross-compile the consumer crate via `worker-build`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

/// Exact `worker-build` version this CLI provisions and drives.
///
/// Pinned to an exact version (not a semver range) so [`ensure_worker_build_installed`]
/// can skip `cargo install` outright once the right binary is already on
/// `PATH`, and so every build path installs the identical toolchain: this
/// helper (used by `impresspress build --target cloudflare` locally) and the
/// `[build] command` embedded in generated `wrangler.toml` by
/// [`super::wrangler::base_toml`] (run by Cloudflare's own build step during
/// `wrangler deploy`/`versions upload`).
///
/// Pin reason: worker-build 0.8.x rejects `worker < 0.8` (hard version
/// check) and changed its output layout from `build/worker/shim.mjs` to
/// `build/index.js`. Bump together with the `worker` crate version pin in
/// `Cargo.toml` and the embedded command in `wrangler::base_toml`.
pub const WORKER_BUILD_VERSION: &str = "0.7.5";

/// Ensure `worker-build` is installed at exactly [`WORKER_BUILD_VERSION`].
///
/// Checks the already-installed binary's own `--version` output first and
/// returns immediately when it matches — skipping `cargo install` entirely.
/// The previous implementation ran `cargo install worker-build --version
/// ^0.7` unconditionally on every build; even when the right version was
/// already present, that command still resolves the semver range against
/// the crates.io index (a network round trip) before deciding there's
/// nothing to do. A plain `worker-build --version` check avoids that.
///
/// # Errors
///
/// Returns an error if the install subprocess fails to spawn or exits
/// non-zero.
pub async fn ensure_worker_build_installed() -> Result<()> {
    if installed_version_matches(WORKER_BUILD_VERSION).await {
        return Ok(());
    }

    let version_req = format!("={WORKER_BUILD_VERSION}");
    let install = Command::new("cargo")
        .args([
            "install",
            "worker-build",
            "--version",
            &version_req,
            "--quiet",
        ])
        .status()
        .await
        .with_context(|| format!("run `cargo install worker-build --version {version_req}`"))?;
    if !install.success() {
        bail!(
            "cargo install worker-build --version {version_req} failed (exit {:?})",
            install.code()
        );
    }
    Ok(())
}

/// `true` if `worker-build --version` is already on `PATH` and reports
/// exactly `expected`. Any failure to run it (not installed, no permission,
/// unexpected output) is treated as "doesn't match" rather than an error —
/// the caller falls back to `cargo install`, which is the same recovery a
/// missing/wrong binary needed before this check existed.
async fn installed_version_matches(expected: &str) -> bool {
    let Ok(output) = Command::new("worker-build").arg("--version").output().await else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).trim() == expected
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
