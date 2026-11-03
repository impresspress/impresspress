//! The deploy half of the Miniflare D1-outage harness
//! (`scripts/miniflare-outage/`): `impresspress deploy --target cloudflare`'s
//! build and packaging, run through the CLI's own code, with the two things
//! that need a Cloudflare account — `wrangler versions upload` and the R2
//! bucket — replaced by files the harness hands to Miniflare.
//!
//! ```text
//! miniflare_deploy candidate <fixture-dir> <state-dir> <candidate-version-id>
//! miniflare_deploy final     <fixture-dir> <state-dir> <prepare-response.json>
//! miniflare_deploy smoke     <fixture-dir> <worker-url> <deploy-token>
//! ```
//!
//! `candidate` is `deploy`'s steps up to the candidate's upload: the build
//! (`wrangler::generate`, which stages the entry module, then worker-build
//! and the asset staging, as `impresspress build --target cloudflare` runs
//! them, minus the password-hasher Worker the harness does not run), the
//! release manifest, the application identity and the candidate config
//! (`wrangler::generate_candidate_upload`). It uploads the release and the UI
//! assets with the deploy's own upload and verification into
//! `<state-dir>/r2/`, and writes `<state-dir>/candidate.json`.
//!
//! `final` is `deploy`'s steps after `/_deploy/prepare`: it decodes the
//! response the harness saved (`parse_prepare_response`), checks the plan
//! against the identity (`verify_compatibility`), packages it
//! (`stage_prepared_module`) and writes the final config
//! (`wrangler::generate_final_upload`), then `<state-dir>/final.json`.
//!
//! `smoke` is `deploy`'s pre-promotion smoke against the running final
//! version, in the order `impresspress deploy` runs it
//! (`cli/flows/embed_cloudflare.rs`): the authenticated `/health` GET
//! (`smoke_authenticated_get`), then the 160-request concurrency smoke over
//! the config's `deploy_smoke_paths` (`smoke_authenticated_concurrency`). The
//! first failure is the error, as it is the deploy's: the flow promotes only
//! after both pass. Not run: `call_deploy_verify` (it probes the
//! password-hasher Worker, which the harness does not run), the preview
//! lockdown check (a `*.workers.dev` preview's, meaningless on a loopback
//! URL), the KV propagation settle before the burst (Miniflare has one KV
//! copy) and the memory report (Cloudflare's analytics).
//!
//! `CLOUDFLARE_ACCOUNT_ID` and `IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID`, which
//! the config loader requires, are placeholders the harness sets.
//! `worker-build` must already be on `PATH` at the CLI's pinned version: the
//! harness checks it before running this, because the build installs it
//! globally otherwise.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use impresspress::cli::helpers::cloudflare::{
    assets, build as cf_build,
    deploy::{self as cf_deploy, R2ObjectClient},
    env,
    prepared::{self, ApplicationArtifactIdentity},
    wrangler,
};
use serde_json::json;

/// Where the CLI stages a build, relative to the consumer crate.
const OUT_DIR: &str = "target/impresspress-cloudflare";

/// An R2 bucket as a directory: each object a file at its key, and its
/// content type in `content-types.json` beside them.
struct DirectoryR2 {
    root: PathBuf,
    content_types: BTreeMap<String, String>,
}

impl DirectoryR2 {
    fn new(root: PathBuf) -> Result<Self> {
        if root.exists() {
            std::fs::remove_dir_all(&root).with_context(|| format!("clean {}", root.display()))?;
        }
        std::fs::create_dir_all(root.join("objects"))?;
        Ok(Self {
            root,
            content_types: BTreeMap::new(),
        })
    }

    fn object_path(&self, key: &str) -> PathBuf {
        self.root.join("objects").join(key)
    }

    fn put(&mut self, key: &str, bytes: &[u8], content_type: &str) -> Result<()> {
        let path = self.object_path(key);
        std::fs::create_dir_all(path.parent().context("an object key has a parent")?)?;
        std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        self.content_types
            .insert(key.to_string(), content_type.to_string());
        Ok(())
    }

    fn finish(self) -> Result<()> {
        let index = self.root.join("content-types.json");
        std::fs::write(&index, serde_json::to_vec_pretty(&self.content_types)?)
            .with_context(|| format!("write {}", index.display()))
    }
}

impl R2ObjectClient for DirectoryR2 {
    fn put_file(&mut self, key: &str, path: &Path, content_type: &str) -> Result<()> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        self.put(key, &bytes, content_type)
    }

    fn put_bytes(&mut self, key: &str, bytes: &[u8], content_type: &str) -> Result<()> {
        self.put(key, bytes, content_type)
    }

    fn get_bytes(&mut self, key: &str) -> Result<Vec<u8>> {
        let path = self.object_path(key);
        std::fs::read(&path).with_context(|| format!("read {}", path.display()))
    }
}

async fn candidate(fixture: &Path, state: &Path, version_id: &str) -> Result<()> {
    let cfg = env::load(fixture)?;
    let out_dir = fixture.join(OUT_DIR);
    if out_dir.exists() {
        std::fs::remove_dir_all(&out_dir)?;
    }
    std::fs::create_dir_all(&out_dir)?;

    // `impresspress build --target cloudflare`'s steps, in its order.
    wrangler::generate(&cfg, fixture, &out_dir)?;
    cf_build::run(fixture, true).await?;
    assets::stage(
        fixture,
        &out_dir,
        cfg.r2.release_assets_dir.as_deref(),
        &cfg.r2.release_assets_prefix,
        &cfg.r2.release_assets_exclude,
    )?;

    // `impresspress deploy --target cloudflare`'s, up to the candidate upload.
    let assets_root = out_dir.join("assets");
    let release = assets::release_manifest_from_staged_dir(&assets_root)?;
    let wasm_path = fixture.join("build/index_bg.wasm");
    let wasm_sha256 = cf_deploy::artifact_sha256(&wasm_path)?;
    let identity =
        ApplicationArtifactIdentity::from_repo(fixture, cfg.worker_name.clone(), &wasm_path)?;
    let config = wrangler::generate_candidate_upload(&cfg, fixture, &out_dir, &release, &identity)?;

    let mut r2 = DirectoryR2::new(state.join("r2"))?;
    cf_deploy::r2_upload_release_with_client(
        &mut r2,
        &assets_root,
        &release,
        version_id,
        &wasm_sha256,
        &[],
    )?;
    cf_deploy::r2_upload_ui_assets_with_client(&mut r2, &[])?;
    r2.finish()?;

    write_json(
        &state.join("candidate.json"),
        &json!({ "config": config, "r2": state.join("r2") }),
    )
}

fn finalize(fixture: &Path, state: &Path, prepare_response: &Path) -> Result<()> {
    let cfg = env::load(fixture)?;
    let out_dir = fixture.join(OUT_DIR);
    let release = assets::release_manifest_from_staged_dir(&out_dir.join("assets"))?;
    let wasm_path = fixture.join("build/index_bg.wasm");
    let identity =
        ApplicationArtifactIdentity::from_repo(fixture, cfg.worker_name.clone(), &wasm_path)?;
    identity.verify_repo(fixture, &wasm_path)?;

    let bytes = std::fs::read(prepare_response)
        .with_context(|| format!("read {}", prepare_response.display()))?;
    let response = prepared::parse_prepare_response(&bytes)?;
    response
        .plan
        .verify_compatibility(
            &identity.application_id,
            &identity.application_build_sha256,
            &identity.dependency_lock,
            &release.prepared_identity()?,
        )
        .map_err(|error| anyhow::anyhow!("prepared candidate identity mismatch: {error}"))?;
    let module = prepared::stage_prepared_module(&out_dir, &response.plan)?;
    let config =
        wrangler::generate_final_upload(&cfg, fixture, &out_dir, &release, &identity, &module)?;
    prepared::verify_prepared_module(&module)?;

    write_json(
        &state.join("final.json"),
        &json!({ "config": config, "plan_hash": response.plan.plan_hash }),
    )
}

async fn smoke(fixture: &Path, worker_url: &str, token: &str) -> Result<()> {
    let cfg = env::load(fixture)?;
    cf_deploy::smoke_authenticated_get(worker_url, token, "/health").await?;
    cf_deploy::smoke_authenticated_concurrency(
        worker_url,
        token,
        &cfg.deploy_smoke_paths,
        std::time::Duration::ZERO,
    )
    .await
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write {}", path.display()))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = |s: &String| -> Result<PathBuf> {
        std::fs::canonicalize(s).with_context(|| format!("resolve {s}"))
    };
    match args.as_slice() {
        [cmd, fixture, state, version_id] if cmd == "candidate" => {
            std::fs::create_dir_all(state)?;
            candidate(&path(fixture)?, &path(state)?, version_id).await
        }
        [cmd, fixture, state, response] if cmd == "final" => {
            finalize(&path(fixture)?, &path(state)?, &path(response)?)
        }
        [cmd, fixture, worker_url, token] if cmd == "smoke" => {
            smoke(&path(fixture)?, worker_url, token).await
        }
        _ => bail!(
            "usage: miniflare_deploy candidate <fixture-dir> <state-dir> <candidate-version-id>\n\
             \x20      miniflare_deploy final <fixture-dir> <state-dir> <prepare-response.json>\n\
             \x20      miniflare_deploy smoke <fixture-dir> <worker-url> <deploy-token>"
        ),
    }
}
