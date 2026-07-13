//! Embed × Cloudflare flow: cross-compile a consumer crate to wasm32,
//! generate wrangler.toml + stage assets, optionally deploy via wrangler.

use std::path::Path;

use anyhow::{bail, Result};

use crate::cli::helpers::cloudflare::{
    assets, build as cf_build, deploy as cf_deploy, env, profile_check, wrangler,
};

pub async fn build(repo_root: &Path, release: bool) -> Result<()> {
    let cfg = env::load(repo_root)?;

    // Inspect [profile.release] before we kick off the long cargo build.
    // Warns only — doesn't block — but surfaces the most common cause of
    // the Workers 400ms startup-CPU 1102 cliff.
    if release {
        profile_check::check_release_profile(repo_root)?;
    }

    let out_dir = repo_root.join("target/impresspress-cloudflare");
    if out_dir.exists() {
        std::fs::remove_dir_all(&out_dir)?;
    }
    std::fs::create_dir_all(&out_dir)?;

    let wrangler_path = wrangler::generate(&cfg, repo_root, &out_dir)?;
    println!("-> {}", wrangler_path.display());

    cf_build::run(repo_root, release).await?;

    // Post-build: measure the produced WASM. Warns if it's likely to
    // exceed the Workers startup-CPU cap on cold-start.
    if release {
        let wasm_path = repo_root.join("build/index_bg.wasm");
        profile_check::check_wasm_size(&wasm_path)?;
    }

    let report = assets::stage(repo_root, &out_dir)?;
    println!(
        "-> staged {} files ({:.1} KB) into {}/assets/",
        report.files_copied,
        report.bytes_copied as f64 / 1024.0,
        out_dir.display(),
    );
    if !report.dirs_skipped.is_empty() {
        println!("  (skipped missing dirs: {:?})", report.dirs_skipped);
    }

    println!();
    println!("Next step: impresspress deploy --target cloudflare");
    Ok(())
}

pub async fn serve(repo_root: &Path, release: bool, port: Option<u16>) -> Result<()> {
    build(repo_root, release).await?;

    let out_dir = repo_root.join("target/impresspress-cloudflare");
    let wrangler_toml = out_dir.join("wrangler.toml");

    // Ephemeral deploy token for this serve session: lets us drive the same
    // /_deploy/init funnel a production deploy uses (migrations + seeds,
    // auto_generate vars included) against the local D1. `wrangler dev`
    // resolves `--var` bindings through `env.secret()`.
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
    let dev_token = impresspress_core::util::hex_encode(&buf);

    // `wrangler dev` is a long-running child: spawn (not status) so we can
    // POST the init funnel once it is up, then wait for it.
    let mut dev = tokio::process::Command::new("wrangler");
    dev.args(["dev", "--config"]).arg(&wrangler_toml);
    let local_port = port.unwrap_or(8787);
    dev.args(["--port", &local_port.to_string()]);
    dev.args([
        "--var",
        &format!(
            "{}:{dev_token}",
            impresspress_core::config_vars::DEPLOY_TOKEN_KEY
        ),
    ]);
    let mut child = dev.spawn()?;

    let local_url = format!("http://localhost:{local_port}");
    match wait_and_run_local_init(&mut child, &local_url, &dev_token).await {
        Ok((ok, report)) => {
            if ok {
                println!("-> local /_deploy/init ok (migrations + seeds applied)");
            } else {
                eprintln!("-> local /_deploy/init reported failures:\n{report}");
                eprintln!(
                    "-> retry manually: {}",
                    manual_seed_hint(&local_url, &dev_token)
                );
            }
        }
        // Child already exited: nothing is listening at `local_url`, so
        // there's nothing to "serve anyway" and no worker to receive a
        // manual POST. `child.wait()` below re-observes the same exit and
        // `bail!`s with the real status.
        Err(LocalInitError::ChildExited(msg)) => eprintln!("-> {msg}"),
        // Still probing when the retry budget ran out, but the child is
        // still alive — it may just be slow to start. Serving anyway and
        // suggesting a manual retry remains reasonable.
        Err(LocalInitError::Unreachable(e)) => eprintln!(
            "-> local /_deploy/init not reachable ({e}); serving anyway — {}",
            manual_seed_hint(&local_url, &dev_token)
        ),
    }

    let status = child.wait().await?;
    if !status.success() {
        bail!("wrangler dev failed (exit {:?})", status.code());
    }
    Ok(())
}

/// Shared "you can trigger deploy-init yourself" hint text, used by both
/// the funnel-failure branch (the funnel ran but reported failures) and
/// the unreachable-but-still-alive branch (probing timed out, but wrangler
/// is still up) so their wording can't drift apart. Do NOT use this for a
/// `LocalInitError::ChildExited` — nothing is listening at `local_url` to
/// receive the POST.
fn manual_seed_hint(local_url: &str, token: &str) -> String {
    format!("POST {local_url}/_deploy/init with header x-deploy-token: {token} to seed manually")
}

/// Why the deploy-init funnel didn't run against the local `wrangler dev`
/// worker. `serve()` needs this distinction to pick an accurate message: a
/// child that already exited isn't coming back to serve a manual POST,
/// whereas a child that's merely slow to start might still answer one.
#[derive(Debug)]
enum LocalInitError {
    /// `wrangler dev` exited before `/_deploy/init` became reachable — the
    /// funnel never ran and nothing is listening to retry against.
    ChildExited(String),
    /// The worker never became reachable within the retry budget, but the
    /// child is still alive.
    Unreachable(anyhow::Error),
}

impl std::fmt::Display for LocalInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalInitError::ChildExited(msg) => write!(f, "{msg}"),
            LocalInitError::Unreachable(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LocalInitError {}

impl From<std::io::Error> for LocalInitError {
    fn from(e: std::io::Error) -> Self {
        LocalInitError::Unreachable(anyhow::anyhow!(e))
    }
}

/// Poll until the local worker answers, then run the deploy-init funnel
/// against it. Bounded: ~60s of connect retries.
///
/// Checks `child` for an early exit before each retry sleep so a wrangler
/// crash surfaces as a distinct [`LocalInitError::ChildExited`] instead of
/// masquerading as 60s of generic "not reachable" connect failures — the
/// exit status is consumed on the first `Ok(Some(_))` from `try_wait()`, so
/// this is the only place that can observe it.
async fn wait_and_run_local_init(
    child: &mut tokio::process::Child,
    local_url: &str,
    token: &str,
) -> Result<(bool, String), LocalInitError> {
    const ATTEMPTS: u32 = 120;
    const INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
    let mut last_err = None;
    for _ in 0..ATTEMPTS {
        match cf_deploy::call_deploy_init(local_url, token).await {
            Ok(out) => return Ok(out),
            Err(e) => {
                last_err = Some(e);
                if let Some(status) = child.try_wait()? {
                    return Err(LocalInitError::ChildExited(format!(
                        "wrangler dev exited ({status}) before /_deploy/init became \
                         reachable — see wrangler output above"
                    )));
                }
                tokio::time::sleep(INTERVAL).await;
            }
        }
    }
    Err(LocalInitError::Unreachable(last_err.unwrap_or_else(|| {
        anyhow::anyhow!("wrangler dev never became reachable")
    })))
}

pub async fn deploy(repo_root: &Path, release: bool) -> Result<()> {
    let cfg = env::load(repo_root)?;
    let _ = env::require_api_token()?; // account_id already validated by load()
    let token_key = impresspress_core::config_vars::DEPLOY_TOKEN_KEY;
    let deploy_token = std::env::var(token_key).map_err(|_| {
        anyhow::anyhow!(
            "{token_key} is not set. Provision it with `impresspress deploy secret` \
             (or `wrangler secret put {token_key}`) and export it for deploys."
        )
    })?;

    build(repo_root, release).await?;

    let out_dir = repo_root.join("target/impresspress-cloudflare");
    let wrangler_toml = out_dir.join("wrangler.toml");

    // 1. Upload an unpromoted version (no traffic routed yet).
    let upload = cf_deploy::wrangler_versions_upload(&wrangler_toml)?;
    println!(
        "-> uploaded version {} (preview {})",
        upload.version_id, upload.preview_url
    );

    // 2. Run migrations + seeds through the new version's own code, against
    //    the shared production D1 (additive migrations keep the still-live
    //    old version safe). Abort pre-promote on failure.
    let (ok, report) = cf_deploy::call_deploy_init(&upload.preview_url, &deploy_token).await?;
    println!("{report}");
    if !ok {
        bail!(
            "deploy init failed — version {} NOT promoted",
            upload.version_id
        );
    }

    // 3. Promote.
    cf_deploy::wrangler_versions_promote(&upload.version_id, &wrangler_toml)?;
    println!("-> promoted {}", upload.version_id);

    let assets_root = out_dir.join("assets");
    let n = cf_deploy::r2_upload_dir(&cfg.r2.bucket_name, &assets_root)?;
    println!(
        "-> uploaded {} R2 objects to bucket {}",
        n, cfg.r2.bucket_name
    );

    println!();
    println!("deploy complete");
    Ok(())
}

/// `impresspress deploy secret`: provision the one-time-per-environment worker
/// secrets (`IMPRESSPRESS_DEPLOY_TOKEN` + the auth JWT secret) via
/// `wrangler secret put`. Each value is taken from the same-named env var when
/// set, otherwise a fresh 32-byte hex token is generated. Requires the
/// generated `wrangler.toml` (run `impresspress build --target cloudflare` first).
pub async fn deploy_secret(repo_root: &Path) -> Result<()> {
    let out_dir = repo_root.join("target/impresspress-cloudflare");
    let wrangler_toml = out_dir.join("wrangler.toml");
    if !wrangler_toml.exists() {
        bail!(
            "wrangler.toml not found at {}. Run `impresspress build --target cloudflare` first.",
            wrangler_toml.display()
        );
    }

    let deploy_token_key = impresspress_core::config_vars::DEPLOY_TOKEN_KEY;
    for name in [
        deploy_token_key,
        impresspress_core::blocks::auth::JWT_SECRET_KEY,
    ] {
        // 32 random bytes → 64 hex chars. getrandom is already a dependency
        // (used for variable seeding); no new crate for randomness.
        let mut buf = [0u8; 32];
        getrandom::getrandom(&mut buf).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
        let (value, generated) = cf_deploy::resolve_secret(std::env::var(name).ok(), &buf);

        cf_deploy::wrangler_secret_put(&wrangler_toml, name, &value)?;

        if generated {
            println!("-> generated and set worker secret {name}");
            if name == deploy_token_key {
                println!(
                    "   IMPORTANT: export this for future `impresspress deploy` runs:\n     \
                     export {name}={value}"
                );
            }
        } else {
            println!("-> set worker secret {name} (from env {name})");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child that exits immediately (no wrangler binary needed) must be
    /// detected between connect retries: `wait_and_run_local_init` should
    /// report "wrangler dev exited" rather than exhausting all ~60s of
    /// retries against a port nothing is listening on.
    #[tokio::test]
    async fn wait_and_run_local_init_detects_dead_child() {
        let mut child = tokio::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");

        // Port 1 is unassigned on loopback — connections are refused near-
        // instantly rather than timing out, so a dead child is what makes
        // the loop exit quickly instead of the 60s retry budget.
        let err = wait_and_run_local_init(&mut child, "http://127.0.0.1:1", "token")
            .await
            .expect_err("dead child must surface as an error, not a 60s hang");

        assert!(
            err.to_string().contains("wrangler dev exited"),
            "expected a wrangler-death error, got: {err}"
        );
    }

    /// The child-death case must be distinguishable from a generic
    /// "unreachable" timeout — `serve()` picks its message off the enum
    /// variant, not by pattern-matching the error text, so pin the variant
    /// directly.
    #[tokio::test]
    async fn wait_and_run_local_init_dead_child_yields_child_exited_variant() {
        let mut child = tokio::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");

        let err = wait_and_run_local_init(&mut child, "http://127.0.0.1:1", "token")
            .await
            .expect_err("dead child must surface as an error, not a 60s hang");

        assert!(
            matches!(err, LocalInitError::ChildExited(_)),
            "expected LocalInitError::ChildExited, got: {err:?}"
        );
    }
}
