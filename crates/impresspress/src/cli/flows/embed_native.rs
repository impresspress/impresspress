//! Embed × native: cargo build the user's bin crate, then exec it.

use std::path::Path;

use anyhow::{anyhow, bail, Result};

use crate::cli::{
    cmd, config,
    helpers::{blocks, frontend},
};

pub async fn build(repo_root: &Path, release: bool) -> Result<()> {
    // A malformed `impresspress.toml` fails the build here, before any work;
    // only an absent file means "no manifest_path override".
    let cfg = config::find_and_load(repo_root)?;

    // 1. wafer build per block.
    blocks::build_all(repo_root).await?;

    // 2. Frontend copy (if any).
    frontend::stage_native_site(repo_root).map_err(|e| anyhow!("copy frontend: {e}"))?;

    // 3. cargo build the user's bin crate.
    let mut cargo = tokio::process::Command::new("cargo");
    cargo.arg("build");
    if release {
        cargo.arg("--release");
    }
    if let Some(mp) = cfg
        .as_ref()
        .and_then(|(c, _)| c.impresspress.manifest_path.as_ref())
    {
        cargo.arg("--manifest-path").arg(mp);
    }
    cargo.current_dir(repo_root);
    cmd::run("cargo build", cargo).await?;

    let profile = if release { "release" } else { "debug" };
    println!("built embed × native ({profile})");
    Ok(())
}

pub async fn serve(repo_root: &Path, release: bool, port: Option<u16>) -> Result<()> {
    // The server is the app's own binary, which reads its listen address
    // itself (`IMPRESSPRESS_LISTEN` when it boots through
    // `impresspress_server::run`): nothing this CLI hands it is guaranteed to
    // reach its listener, so the flag is refused rather than ignored.
    if let Some(port) = port {
        bail!(
            "--port {port} is not supported for an embedded native app: its own binary \
             chooses its listen address; set IMPRESSPRESS_LISTEN in its .env instead"
        );
    }
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
    // Embed flow exec's the user's bin as a subprocess; it applies its
    // database's pending migrations itself, at boot.
    //
    // Use `tokio::process::Command` because the child is long-running
    // (it's the actual impresspress server) and blocking on `wait` from a
    // sync `std::process::Command` would freeze the tokio worker thread
    // this async fn is parked on.
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.current_dir(repo_root);
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
