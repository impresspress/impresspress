//! Embed × native: cargo build the user's bin crate, then exec it.

use std::path::Path;

use anyhow::{anyhow, bail, Result};

use crate::cli::{
    cmd, config,
    helpers::{blocks, frontend},
};

const RUNTIME_SITE_REL: &str = "data/storage/wafer-run/web/site";

pub async fn build(repo_root: &Path, release: bool) -> Result<()> {
    // 1. wafer build per block.
    blocks::build_all(repo_root).await?;

    // 2. Frontend copy (if any).
    if let Some(fe) = frontend::find_frontend_dir(repo_root) {
        let dst = repo_root.join(RUNTIME_SITE_REL);
        frontend::copy_tree(&fe, &dst).map_err(|e| anyhow!("copy frontend: {e}"))?;
    }

    // 3. cargo build the user's bin crate.
    let mut cargo = tokio::process::Command::new("cargo");
    cargo.arg("build");
    if release {
        cargo.arg("--release");
    }
    if let Ok((cfg, _)) = config::find_and_load(repo_root) {
        if let Some(mp) = &cfg.impresspress.manifest_path {
            cargo.arg("--manifest-path").arg(mp);
        }
    }
    cargo.current_dir(repo_root);
    cmd::run("cargo build", cargo).await?;

    let profile = if release { "release" } else { "debug" };
    println!("built embed × native ({profile})");
    Ok(())
}

pub async fn serve(
    repo_root: &Path,
    release: bool,
    _port: Option<u16>,
    run_migrations: bool,
) -> Result<()> {
    build(repo_root, release).await?;

    // Locate target/<profile>/<bin-name>. Read Cargo.toml to find the bin.
    let cargo_toml = repo_root.join("Cargo.toml");
    let text = std::fs::read_to_string(&cargo_toml)?;
    let parsed: toml::Value = toml::from_str(&text)?;
    let bin_name = parsed
        .get("bin")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|b| b.get("name"))
        .and_then(|n| n.as_str())
        .map(String::from)
        .or_else(|| {
            parsed
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .map(String::from)
        })
        .ok_or_else(|| anyhow!("could not determine bin name from Cargo.toml"))?;

    let profile = if release { "release" } else { "debug" };
    let bin = repo_root.join("target").join(profile).join(&bin_name);
    if !bin.is_file() {
        return Err(anyhow!("expected binary at {bin:?} after cargo build"));
    }
    // Embed flow exec's the user's bin as a subprocess. Pass the
    // run-migrations flag via the child's env (scoped to that child),
    // rather than mutating the CLI's own process env via `set_var` (unsafe
    // in Rust 2024, and would leak into any other child the CLI spawns).
    //
    // Use `tokio::process::Command` because the child is long-running
    // (it's the actual impresspress server) and blocking on `wait` from a
    // sync `std::process::Command` would freeze the tokio worker thread
    // this async fn is parked on.
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.current_dir(repo_root);
    if run_migrations {
        cmd.env(impresspress_core::migration_helper::RUN_MIGRATIONS_KEY, "1");
    }
    let mut child = cmd.spawn()?;
    let status = child.wait().await?;
    if !status.success() {
        // Propagate the child's exit code via `Result` rather than calling
        // `std::process::exit` directly, which would bypass tokio runtime
        // drop and skip any in-flight async cleanup.
        bail!(
            "embedded impresspress binary exited with status {:?}",
            status.code()
        );
    }
    Ok(())
}
