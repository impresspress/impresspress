//! Embed × Cloudflare flow: cross-compile a consumer crate to wasm32,
//! generate wrangler.toml + stage assets, optionally deploy via wrangler.

use std::path::Path;

use anyhow::{bail, Result};

use crate::cli::helpers::cloudflare::{
    assets, build as cf_build, deploy as cf_deploy, env, first_create, password_hasher,
    profile_check, wrangler,
};

pub async fn build(repo_root: &Path, release: bool) -> Result<()> {
    let cfg = env::load(repo_root)?;

    // Inspect [profile.release] before we kick off the long cargo build.
    // Warns only — doesn't block — but surfaces the most common cause of
    // missing the Workers 1-second startup-CPU budget, which `wrangler
    // deploy` enforces at deploy-validation time (error code 10021), not
    // as a per-request runtime failure.
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
    let hasher_path = wrangler::generate_password_hasher(&cfg, &out_dir)?;
    println!("-> {}", hasher_path.display());

    cf_build::run(repo_root, release).await?;

    // The password-hasher Worker the main Worker's crypto service calls: its
    // own crate, staged against the consumer's `impresspress-password`.
    let hasher_wasm = password_hasher::build(repo_root).await?;
    if let Ok(meta) = std::fs::metadata(&hasher_wasm) {
        println!(
            "-> password-hasher Worker {} ({:.1} KB)",
            hasher_wasm.display(),
            meta.len() as f64 / 1024.0
        );
    }

    // Post-build: measure the produced WASM. Warns if it's likely to
    // exceed the Workers startup-CPU cap on cold-start.
    if release {
        let wasm_path = repo_root.join("build/index_bg.wasm");
        profile_check::check_wasm_size(&wasm_path)?;
    }

    let report = assets::stage(
        repo_root,
        &out_dir,
        cfg.r2.release_assets_dir.as_deref(),
        &cfg.r2.release_assets_prefix,
        &cfg.r2.release_assets_exclude,
    )?;
    println!(
        "-> staged {} files ({:.1} KB) into {}/assets/",
        report.files_copied,
        report.bytes_copied as f64 / 1024.0,
        out_dir.display(),
    );
    if !report.dirs_skipped.is_empty() {
        println!("  (skipped missing dirs: {:?})", report.dirs_skipped);
    }
    if !report.files_excluded.is_empty() {
        println!(
            "  ({} files held out of the release set by \
             cloudflare.r2.release_assets_exclude)",
            report.files_excluded.len(),
        );
    }

    println!();
    println!("Next step: impresspress deploy --target cloudflare");
    Ok(())
}

pub async fn serve(repo_root: &Path, release: bool, port: Option<u16>) -> Result<()> {
    build(repo_root, release).await?;

    let out_dir = repo_root.join("target/impresspress-cloudflare");
    let wrangler_toml = out_dir.join("wrangler.toml");
    let hasher_toml = out_dir.join(wrangler::PASSWORD_HASHER_CONFIG_FILE);

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
    // Both Workers in one session, the main one first (it serves the port):
    // the main Worker's Durable Object binding names the hasher's script,
    // which `wrangler dev` resolves to the second config.
    dev.args(["dev", "--config"])
        .arg(&wrangler_toml)
        .arg("--config")
        .arg(&hasher_toml);
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
    let token_key = impresspress_core::config_vars::DEPLOY_TOKEN_KEY;

    // Before the build: does the main Worker exist yet? A site's first deploy
    // creates it (step 0a below); any later one needs the operator's deploy
    // token, and is refused here, not after minutes of building, without it.
    // An existing Worker missing either deploy secret (a first deploy that
    // stopped part-way) is given it here.
    let wrangler_cli = first_create::Wrangler::default();
    let placeholder_toml =
        first_create::generate_placeholder(&cfg, &repo_root.join(first_create::FIRST_CREATE_DIR))?;
    let worker_secrets =
        first_create::resolve_worker_secrets(|name| std::env::var(name).ok(), random_secret_bytes)?;
    let main_worker = first_create::plan_main_worker(
        &wrangler_cli,
        &placeholder_toml,
        &cfg.worker_name,
        &worker_secrets,
        std::env::var(token_key).ok(),
    )?;

    build(repo_root, release).await?;

    let out_dir = repo_root.join("target/impresspress-cloudflare");
    // `build()` emits a developer config with a Wrangler build hook for
    // `wrangler dev`. A deploy consumes the artifact already produced by
    // that build through a second, upload-only config. This prevents
    // either `versions upload` from compiling the Rust worker a second time.
    let assets_root = out_dir.join("assets");
    let release_assets = assets::release_manifest_from_staged_dir(&assets_root)?;
    let wasm_path = repo_root.join("build/index_bg.wasm");
    let wasm_sha256 = cf_deploy::artifact_sha256(&wasm_path)?;
    let application_identity =
        crate::cli::helpers::cloudflare::prepared::ApplicationArtifactIdentity::from_repo(
            repo_root,
            cfg.worker_name.clone(),
            &wasm_path,
        )?;
    let mut deployment_gate =
        crate::cli::helpers::cloudflare::prepared::TwoStageDeploymentGate::new();
    let prepared_release_assets = release_assets.prepared_identity()?;

    // 0. The password-hasher Worker first. Every version of the main Worker
    //    this deploy uploads — the candidate `/_deploy/prepare` runs in, the
    //    final one verified below — hashes through whichever hasher is
    //    deployed, so the one this build pairs with has to be live before
    //    any of them runs. The live main Worker keeps working meanwhile:
    //    the hasher answers the previous release's protocol version too.
    let hasher_toml = out_dir.join(wrangler::PASSWORD_HASHER_CONFIG_FILE);
    cf_deploy::wrangler_deploy_password_hasher(&hasher_toml)?;
    deployment_gate.password_hasher_deployed()?;
    println!(
        "-> deployed password-hasher Worker {}",
        cfg.password_hasher.worker_name
    );

    // 0a. The main Worker. `wrangler versions upload` cannot create one, so
    //     a site's first deploy creates it here as a placeholder that answers
    //     503, with the deploy token and JWT secret its `/_deploy/*`
    //     endpoints need; the final version promoted below replaces it.
    let deploy_token = match main_worker {
        first_create::MainWorkerPlan::Existing { deploy_token } => deploy_token,
        first_create::MainWorkerPlan::Create => {
            let deploy_token = first_create::create_main_worker(
                &wrangler_cli,
                &placeholder_toml,
                &cfg.worker_name,
                &worker_secrets,
            )?;
            println!(
                "-> created Worker {} (a placeholder answering 503 until this deploy promotes)",
                cfg.worker_name
            );
            deploy_token
        }
    };
    deployment_gate.main_worker_ready()?;

    // 0b. A site that set its pepper before hashing moved still has the keys
    //     on the main Worker, which reads them no longer. Uploading on would
    //     lock out peppered accounts and write unpeppered hashes, so the
    //     deploy stops here until they are on the hasher (secret names only).
    let leftovers = password_hasher::check_pepper_placement(
        &cf_deploy::wrangler_secret_names(&out_dir.join("wrangler.toml"))?,
        &cf_deploy::wrangler_secret_names(&hasher_toml)?,
        &cfg.worker_name,
        &cfg.password_hasher.worker_name,
    )?;
    deployment_gate.pepper_placement_checked()?;
    for name in leftovers {
        eprintln!(
            "-> note: {name} is on the password-hasher Worker and still on {} too, which no \
             longer reads it; delete it there: npx wrangler secret delete {name} --name {}",
            cfg.worker_name, cfg.worker_name
        );
    }

    let candidate_toml = wrangler::generate_candidate_upload(
        &cfg,
        repo_root,
        &out_dir,
        &release_assets,
        &application_identity,
    )?;
    println!(
        "-> upload artifact {} ({})",
        wasm_path.display(),
        application_identity.application_build_sha256
    );
    println!(
        "-> release assets {} files (sha256 {}, prefix {})",
        release_assets.files.len(),
        release_assets.asset_set_sha256,
        release_assets.immutable_prefix
    );

    // 1. Upload the dynamic, unpromoted preparation candidate. It has no
    //    packaged plan and is never eligible for promotion.
    let candidate = cf_deploy::wrangler_versions_upload(&candidate_toml)?;
    cf_deploy::verify_artifact_sha256(&wasm_path, &wasm_sha256)?;
    application_identity.verify_repo(repo_root, &wasm_path)?;
    deployment_gate.candidate_uploaded(&candidate.version_id, &wasm_sha256)?;
    println!(
        "-> uploaded preparation candidate {} (preview {})",
        candidate.version_id, candidate.preview_url
    );
    cf_deploy::smoke_preview_lockdown(&candidate.preview_url).await?;
    println!("-> preparation candidate preview lockdown verified");
    // Cloudflare's own reported size for the exact bundle it received —
    // prefer this over the pre-upload `.wasm` byte heuristic in
    // `profile_check` when deciding whether size is actually a problem.
    profile_check::report_upload_size(candidate.upload_size);

    // 2. Populate and byte-verify immutable external R2 release assets before
    //    candidate initialization. Manifests bind them to this Worker version.
    //    Mutable logical keys are deliberately left untouched so the active
    //    legacy Worker and legacy rollbacks continue to see their old assets.
    let release_upload = cf_deploy::r2_upload_release(
        &cfg.r2.bucket_name,
        &assets_root,
        &release_assets,
        &candidate.version_id,
        &wasm_sha256,
    )?;
    println!(
        "-> uploaded {} immutable R2 files and {} metadata objects; \
         verified {} objects (deployment record {})",
        release_upload.immutable_files_uploaded,
        release_upload.metadata_objects_uploaded,
        release_upload.objects_verified,
        release_upload.deployment_record_key,
    );
    deployment_gate.assets_verified()?;

    // Publish impresspress's own UI asset set (CSS/JS/fonts/logos) to R2 at
    // their hashed filenames. Independent of this app's release-asset gate
    // above: these are the shared static assets a lean (no `embed-assets`)
    // Cloudflare build drops at compile time, and `IMPRESSPRESS_ASSET_BASE_URL`
    // (set in every generated wrangler config via
    // `assets::resolve_asset_base_url`, see `wrangler::base_toml`) points
    // `/b/static/` at wherever they land here — served back out by
    // `impresspress_cloudflare::run_with_config`'s R2 read-through
    // (`static_asset_target` / `serve_static_asset_from_r2`), not by the
    // (compiled-out) embedded byte path.
    if !cfg.r2.bucket_name.is_empty() {
        #[cfg(feature = "embed-assets")]
        {
            let ui_asset_upload = cf_deploy::r2_upload_ui_assets(&cfg.r2.bucket_name)?;
            println!(
                "-> published {} UI asset object(s) to R2",
                ui_asset_upload.uploaded
            );
        }
        // An R2 bucket is configured, so the deployed worker expects to read
        // its UI assets (CSS/JS/fonts/logos) back from R2 at
        // `/b/static/{filename}`. Without `embed-assets`, this CLI binary
        // has no asset bytes compiled in to publish there — proceeding would
        // silently leave the bucket empty and every `/b/static/` request on
        // the live site 404ing. Fail the deploy loudly instead, before any
        // candidate is promoted, rather than let this ship broken.
        #[cfg(not(feature = "embed-assets"))]
        bail!(
            "cloudflare.r2.bucket_name is configured, but this `impresspress` CLI binary was \
             built without the `embed-assets` feature, so it has no UI asset bytes to publish \
             to R2. Rebuild the CLI with default features (or explicitly with \
             `--features embed-assets`) before running `deploy cloudflare` against an \
             R2-backed target."
        );
    }

    // 3. One authenticated request owns every mutation: migrate, seed, reload
    //    final structural state, and export the strict immutable plan.
    let prepared_response =
        cf_deploy::call_deploy_prepare(&candidate.preview_url, &deploy_token).await?;
    prepared_response
        .plan
        .verify_compatibility(
            &application_identity.application_id,
            &application_identity.application_build_sha256,
            &application_identity.dependency_lock,
            &prepared_release_assets,
        )
        .map_err(|error| anyhow::anyhow!("prepared candidate identity mismatch: {error}"))?;
    deployment_gate.prepared()?;
    println!(
        "-> prepared plan {} ({} blocks, {} routes)",
        prepared_response.plan.plan_hash,
        prepared_response.plan.structure.application_blocks.len(),
        prepared_response.plan.structure.routes.len()
    );

    // 4. Package deterministic JS/Text modules around the already-built Wasm
    //    and upload the final candidate. No Rust/Wasm build occurs here.
    let prepared_module = crate::cli::helpers::cloudflare::prepared::stage_prepared_module(
        &out_dir,
        &prepared_response.plan,
    )?;
    let final_toml = wrangler::generate_final_upload(
        &cfg,
        repo_root,
        &out_dir,
        &release_assets,
        &application_identity,
        &prepared_module,
    )?;
    let final_upload = cf_deploy::wrangler_versions_upload(&final_toml)?;
    cf_deploy::verify_artifact_sha256(&wasm_path, &wasm_sha256)?;
    application_identity.verify_repo(repo_root, &wasm_path)?;
    crate::cli::helpers::cloudflare::prepared::verify_prepared_module(&prepared_module)?;
    deployment_gate.final_uploaded(&final_upload.version_id, &wasm_sha256)?;
    println!(
        "-> uploaded final candidate {} (preview {}, same Wasm sha256 {})",
        final_upload.version_id, final_upload.preview_url, wasm_sha256
    );
    profile_check::report_upload_size(final_upload.upload_size);

    let final_record = cf_deploy::r2_upload_final_deployment_record(
        &cfg.r2.bucket_name,
        &release_assets,
        &final_upload.version_id,
        &wasm_sha256,
        &prepared_response.plan.plan_hash,
    )?;
    println!("-> verified final deployment record {final_record}");

    // 5. The final version must prove it parsed/applied the packaged plan and
    //    can read an immutable representative asset, then serve a normal
    //    route. Neither request reruns migrations or seeds.
    cf_deploy::call_deploy_verify(
        &final_upload.preview_url,
        &deploy_token,
        &prepared_response.plan,
        &release_assets,
    )
    .await?;
    cf_deploy::smoke_preview_lockdown(&final_upload.preview_url).await?;
    cf_deploy::smoke_authenticated_get(&final_upload.preview_url, &deploy_token, "/health").await?;
    cf_deploy::smoke_authenticated_concurrency(
        &final_upload.preview_url,
        &deploy_token,
        &cfg.deploy_smoke_paths,
        cf_deploy::KV_PROPAGATION_SETTLE,
    )
    .await?;
    deployment_gate.final_verified()?;
    println!(
        "-> final candidate plan, release asset, preview lockdown, /health, and \
         mixed 160-request P32 workload verified"
    );

    // 6. Only the prepared second upload is eligible for promotion.
    deployment_gate.authorize_promotion()?;
    cf_deploy::wrangler_versions_promote(&final_upload.version_id, &final_upload.wrangler_toml)?;
    println!("-> promoted {}", final_upload.version_id);

    // 7. Worker-level settings last. Neither `wrangler versions upload` nor
    //    `wrangler versions deploy` applies them — they are not part of a
    //    Worker version — so the `[triggers] crons` schedule reaches the live
    //    Worker only here. After promotion, deliberately: the schedule points
    //    at whatever code is serving, and until this line runs that is still
    //    the previous version.
    let triggers_toml = wrangler::generate_triggers(&cfg, repo_root, &out_dir)?;
    cf_deploy::wrangler_triggers_deploy(&triggers_toml)?;
    println!(
        "-> applied worker-level settings ({})",
        if cfg.crons.is_empty() {
            "no cron schedule".to_string()
        } else {
            format!("crons {}", cfg.crons.join(", "))
        }
    );

    println!();
    println!("deploy complete");
    Ok(())
}

/// `impresspress deploy secret`: set the worker secrets the deploy funnel
/// needs (`IMPRESSPRESS_DEPLOY_TOKEN` + the auth JWT secret) via `wrangler
/// secret put`. Each value is taken from the same-named env var when set,
/// otherwise a fresh 32-byte hex token is generated — for the JWT secret that
/// signs every user out. `impresspress deploy` sets both itself when it
/// creates a site's Worker; this is for setting them again. Requires the
/// generated `wrangler.toml` (run `impresspress build --target cloudflare`
/// first).
pub async fn deploy_secret(repo_root: &Path) -> Result<()> {
    let out_dir = repo_root.join("target/impresspress-cloudflare");
    let wrangler_toml = out_dir.join("wrangler.toml");
    if !wrangler_toml.exists() {
        bail!(
            "wrangler.toml not found at {}. Run `impresspress build --target cloudflare` first.",
            wrangler_toml.display()
        );
    }

    let secrets =
        first_create::resolve_worker_secrets(|name| std::env::var(name).ok(), random_secret_bytes)?;
    first_create::put_worker_secrets(&first_create::Wrangler::default(), &wrangler_toml, &secrets)
}

/// 32 random bytes for a generated worker secret (64 hex characters).
fn random_secret_bytes() -> Result<[u8; 32]> {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
    Ok(buf)
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
