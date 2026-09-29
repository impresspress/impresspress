//! Prepared-runtime deployment artifacts for Cloudflare's two-upload flow.
//!
//! The first candidate is the ordinary dynamic Worker. Its authenticated
//! `/_deploy/prepare` response is decoded into the strict core plan type. The
//! final candidate changes only JavaScript/Text modules and versioned vars:
//! the already-built Wasm is reused byte-for-byte.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use impresspress_core::{
    builder::{BootReport, DEPLOY_RESPONSE_SCHEMA_VERSION},
    PreparedRuntimePlan, PreparedRuntimePlanSummary, WaferLockIdentity,
};
use serde::{Deserialize, Serialize};

pub const PREPARE_ENDPOINT: &str = "/_deploy/prepare";
pub const VERIFY_ENDPOINT: &str = "/_deploy/verify";
pub const PREPARED_PLAN_GLOBAL: &str = "__IMPRESSPRESS_PREPARED_RUNTIME_PLAN";
pub const PREPARED_MODULE_DIR: &str = "prepared-runtime";
pub const PREPARED_PLAN_FILE: &str = "prepared-runtime-plan.prepared.json";
pub const PREPARED_SHIM_FILE: &str = "shim.mjs";
pub const PREPARED_TEXT_GLOB: &str = "**/*.prepared.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TwoStageState {
    Built,
    /// The password-hasher Worker is deployed. Before any version of the main
    /// Worker is uploaded: every version — the candidate `/_deploy/prepare`
    /// runs in, the final one verified before promotion — calls whichever
    /// hasher is deployed, so the one it was built against must already be
    /// there.
    PasswordHasherDeployed,
    /// The main Worker exists with its deploy secrets: already there, or just
    /// created as a placeholder (`first_create`). The pepper check lists its
    /// secrets and every upload targets it, so both need it to exist.
    MainWorkerReady,
    /// Neither Worker's secrets leave the pepper behind on the main Worker
    /// (`password_hasher::check_pepper_placement`).
    PepperPlacementChecked,
    CandidateUploaded,
    AssetsVerified,
    Prepared,
    FinalUploaded,
    FinalVerified,
}

/// Small fail-closed ordering guard shared by the production flow and tests.
/// It does not perform I/O; every transition is recorded only after its
/// corresponding operation succeeds.
#[derive(Debug)]
pub struct TwoStageDeploymentGate {
    state: TwoStageState,
    candidate_version: Option<String>,
    wasm_sha256: Option<String>,
}

impl TwoStageDeploymentGate {
    pub fn new() -> Self {
        Self {
            state: TwoStageState::Built,
            candidate_version: None,
            wasm_sha256: None,
        }
    }

    pub fn password_hasher_deployed(&mut self) -> Result<()> {
        self.advance(
            TwoStageState::Built,
            TwoStageState::PasswordHasherDeployed,
            "password-hasher deploy",
        )
    }

    pub fn main_worker_ready(&mut self) -> Result<()> {
        self.advance(
            TwoStageState::PasswordHasherDeployed,
            TwoStageState::MainWorkerReady,
            "main Worker creation check",
        )
    }

    pub fn pepper_placement_checked(&mut self) -> Result<()> {
        self.advance(
            TwoStageState::MainWorkerReady,
            TwoStageState::PepperPlacementChecked,
            "pepper placement check",
        )
    }

    pub fn candidate_uploaded(&mut self, version: &str, wasm_sha256: &str) -> Result<()> {
        self.expect(TwoStageState::PepperPlacementChecked, "candidate upload")?;
        self.candidate_version = Some(version.to_string());
        self.wasm_sha256 = Some(wasm_sha256.to_string());
        self.state = TwoStageState::CandidateUploaded;
        Ok(())
    }

    pub fn assets_verified(&mut self) -> Result<()> {
        self.advance(
            TwoStageState::CandidateUploaded,
            TwoStageState::AssetsVerified,
            "release asset verification",
        )
    }

    pub fn prepared(&mut self) -> Result<()> {
        self.advance(
            TwoStageState::AssetsVerified,
            TwoStageState::Prepared,
            "prepared plan export",
        )
    }

    pub fn final_uploaded(&mut self, version: &str, wasm_sha256: &str) -> Result<()> {
        self.expect(TwoStageState::Prepared, "final upload")?;
        if self.candidate_version.as_deref() == Some(version) {
            bail!("final upload unexpectedly reused the candidate Worker version id");
        }
        if self.wasm_sha256.as_deref() != Some(wasm_sha256) {
            bail!("final upload does not use the candidate's exact Wasm digest");
        }
        self.state = TwoStageState::FinalUploaded;
        Ok(())
    }

    pub fn final_verified(&mut self) -> Result<()> {
        self.advance(
            TwoStageState::FinalUploaded,
            TwoStageState::FinalVerified,
            "final verification",
        )
    }

    pub fn authorize_promotion(&self) -> Result<()> {
        self.expect(TwoStageState::FinalVerified, "promotion")
    }

    fn advance(
        &mut self,
        expected: TwoStageState,
        next: TwoStageState,
        operation: &str,
    ) -> Result<()> {
        self.expect(expected, operation)?;
        self.state = next;
        Ok(())
    }

    fn expect(&self, expected: TwoStageState, operation: &str) -> Result<()> {
        if self.state != expected {
            bail!(
                "cannot perform {operation} at two-stage deployment state {:?}; expected {:?}",
                self.state,
                expected
            );
        }
        Ok(())
    }
}

impl Default for TwoStageDeploymentGate {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationArtifactIdentity {
    pub application_id: String,
    /// Canonical core digest form: `sha256:<64 lowercase hex>`.
    pub application_build_sha256: String,
    pub dependency_lock: WaferLockIdentity,
}

impl ApplicationArtifactIdentity {
    pub fn from_repo(
        repo_root: &Path,
        application_id: impl Into<String>,
        wasm_path: &Path,
    ) -> Result<Self> {
        let application_id = application_id.into();
        if application_id.trim().is_empty() {
            bail!("prepared runtime application id must not be empty");
        }
        let wasm = std::fs::read(wasm_path)
            .with_context(|| format!("read application Wasm {}", wasm_path.display()))?;
        let raw = impresspress_core::util::sha256_hex(&wasm);
        let application_build_sha256 = canonical_sha256(&raw)?;
        let dependency_lock = dependency_lock_identity(repo_root)?;
        Ok(Self {
            application_id,
            application_build_sha256,
            dependency_lock,
        })
    }

    pub fn dependency_lock_json(&self) -> Result<String> {
        serde_json::to_string(&self.dependency_lock)
            .context("serialize wafer.lock deployment identity")
    }

    pub fn verify_repo(&self, repo_root: &Path, wasm_path: &Path) -> Result<()> {
        let current = Self::from_repo(repo_root, self.application_id.clone(), wasm_path)?;
        if &current != self {
            bail!("application Wasm or wafer.lock changed during the two-stage deployment");
        }
        Ok(())
    }
}

pub fn canonical_sha256(raw: &str) -> Result<String> {
    if raw.starts_with("sha256:") {
        bail!("expected raw SHA-256 hex, found already-prefixed digest");
    }
    if raw.len() != 64 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("expected exactly 64 hexadecimal SHA-256 characters");
    }
    Ok(format!("sha256:{}", raw.to_ascii_lowercase()))
}

fn dependency_lock_identity(repo_root: &Path) -> Result<WaferLockIdentity> {
    let path = repo_root.join("wafer.lock");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WaferLockIdentity::absent())
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let lockfile: wafer_block::lockfile::Lockfile =
        toml::from_str(std::str::from_utf8(&bytes).context("wafer.lock is not valid UTF-8")?)
            .with_context(|| format!("parse {}", path.display()))?;
    WaferLockIdentity::from_lockfile_bytes(&lockfile, &bytes)
        .with_context(|| format!("validate {}", path.display()))
}

/// The `/_deploy/prepare` envelope.
///
/// `deny_unknown_fields` stays here and is *not* inherited by
/// [`BootReport`]: this envelope is the document that carries
/// [`DEPLOY_RESPONSE_SCHEMA_VERSION`], and a field appearing beside a version
/// this CLI claims to understand means the Worker is speaking a contract this
/// CLI cannot see all of — while the plan it is about to package and promote
/// comes out of that same document. Its three members are all load-bearing;
/// the nested report's are not.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareResponse {
    pub schema_version: u32,
    /// The runtime's own report, deserialized as the very struct
    /// `impresspress_core::builder::boot` serialized — see [`BootReport`].
    pub init_report: BootReport,
    pub plan: PreparedRuntimePlan,
}

pub fn parse_prepare_response(bytes: &[u8]) -> Result<PrepareResponse> {
    let response: PrepareResponse =
        serde_json::from_slice(bytes).context("decode strict /_deploy/prepare response")?;
    response
        .plan
        .validate_deployable()
        .context("validate deployable prepared runtime plan")?;
    if response.schema_version != DEPLOY_RESPONSE_SCHEMA_VERSION {
        bail!(
            "unsupported /_deploy/prepare response schema {}; expected {}",
            response.schema_version,
            DEPLOY_RESPONSE_SCHEMA_VERSION
        );
    }
    if !response.init_report.ok
        || !response.init_report.sealed
        || !response.init_report.seed.ok
        || response.init_report.blocks.iter().any(|block| !block.ok)
    {
        bail!("/_deploy/prepare reported an unsuccessful initialization");
    }
    Ok(response)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedModule {
    pub directory: PathBuf,
    pub shim_path: PathBuf,
    pub plan_path: PathBuf,
    pub plan_hash: String,
    pub plan_module_sha256: String,
}

pub fn stage_prepared_module(out_dir: &Path, plan: &PreparedRuntimePlan) -> Result<PreparedModule> {
    plan.validate_deployable()
        .context("validate deployable plan before packaging")?;
    let directory = out_dir.join(PREPARED_MODULE_DIR);
    if directory.exists() {
        std::fs::remove_dir_all(&directory)
            .with_context(|| format!("clean {}", directory.display()))?;
    }
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("create {}", directory.display()))?;

    let plan_bytes = plan
        .to_json_pretty()
        .context("encode prepared plan module")?;
    let plan_path = directory.join(PREPARED_PLAN_FILE);
    std::fs::write(&plan_path, &plan_bytes)
        .with_context(|| format!("write {}", plan_path.display()))?;

    // This wrapper deliberately imports the original worker-build shim rather
    // than copying/rebuilding it. Static dependency evaluation completes
    // before requests can arrive; the assignment therefore exists before the
    // Rust fetch handler reads the global.
    let shim = format!(
        "// Generated by ImpressPress. Do not edit.\n\
         import preparedRuntimePlan from './{PREPARED_PLAN_FILE}';\n\
         globalThis.{PREPARED_PLAN_GLOBAL} = preparedRuntimePlan;\n\
         export * from '../../../build/worker/shim.mjs';\n\
         export {{ default }} from '../../../build/worker/shim.mjs';\n"
    );
    let shim_path = directory.join(PREPARED_SHIM_FILE);
    std::fs::write(&shim_path, shim.as_bytes())
        .with_context(|| format!("write {}", shim_path.display()))?;

    Ok(PreparedModule {
        directory,
        shim_path,
        plan_path,
        plan_hash: plan.plan_hash.clone(),
        plan_module_sha256: canonical_sha256(&impresspress_core::util::sha256_hex(&plan_bytes))?,
    })
}

pub fn verify_prepared_module(prepared: &PreparedModule) -> Result<()> {
    let plan_bytes = std::fs::read(&prepared.plan_path)
        .with_context(|| format!("read prepared plan module {}", prepared.plan_path.display()))?;
    let actual = canonical_sha256(&impresspress_core::util::sha256_hex(&plan_bytes))?;
    if actual != prepared.plan_module_sha256 {
        bail!(
            "prepared plan module changed during upload: expected {}, found {actual}",
            prepared.plan_module_sha256
        );
    }
    if !prepared.shim_path.is_file() {
        bail!("prepared runtime shim disappeared during upload");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifyResponse {
    pub schema_version: u32,
    pub ok: bool,
    pub summary: PreparedRuntimePlanSummary,
    pub release_asset_verified: bool,
    pub release_asset: Option<VerifiedReleaseAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedReleaseAsset {
    pub logical_key: String,
    pub immutable_key: String,
    pub sha256: String,
}

pub fn parse_verify_response(
    bytes: &[u8],
    expected_plan: &PreparedRuntimePlan,
    expected_release_asset: Option<(&str, &str, &str)>,
) -> Result<VerifyResponse> {
    let response: VerifyResponse =
        serde_json::from_slice(bytes).context("decode strict /_deploy/verify response")?;
    if response.schema_version != DEPLOY_RESPONSE_SCHEMA_VERSION {
        bail!(
            "unsupported /_deploy/verify response schema {}; expected {}",
            response.schema_version,
            DEPLOY_RESPONSE_SCHEMA_VERSION
        );
    }
    if !response.ok {
        bail!("/_deploy/verify reported failure");
    }
    let expected_summary = expected_plan.summary()?;
    if response.summary != expected_summary {
        bail!("/_deploy/verify summary does not match the packaged plan");
    }
    if !response.release_asset_verified {
        bail!("/_deploy/verify did not verify the representative release asset");
    }
    match (expected_release_asset, response.release_asset.as_ref()) {
        (None, None) => {}
        (Some((logical_key, immutable_key, sha256)), Some(actual))
            if actual.logical_key == logical_key
                && actual.immutable_key == immutable_key
                && actual.sha256 == sha256 => {}
        _ => bail!("/_deploy/verify representative release asset mismatch"),
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use impresspress_core::{
        builder::{BlockInitOutcome, StepOutcome},
        PreparedReleaseAssets, PreparedRuntimeStructure, PREPARED_RUNTIME_PLAN_SCHEMA_VERSION,
    };

    use super::*;

    fn plan() -> PreparedRuntimePlan {
        PreparedRuntimePlan::new_with_config_generation(
            "example",
            format!("sha256:{}", "a".repeat(64)),
            "1".repeat(32),
            WaferLockIdentity::absent(),
            PreparedReleaseAssets::absent(),
            PreparedRuntimeStructure {
                application_blocks: Vec::new(),
                routes: Vec::new(),
                built_in_route_count: 0,
                block_settings: Default::default(),
                block_configs: Default::default(),
                final_block_configs: Default::default(),
                wrap_grants: Vec::new(),
                deployment_wrap_grants: Vec::new(),
            },
        )
        .unwrap()
    }

    fn successful_prepare_json(plan: &PreparedRuntimePlan) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "init_report": {
                "sealed": true,
                "seed": { "ok": true },
                "blocks": [{ "block": "impresspress/admin", "ok": true }],
                "ok": true
            },
            "plan": plan
        }))
        .unwrap()
    }

    #[test]
    fn canonical_digest_rejects_missing_double_and_malformed_forms() {
        assert_eq!(
            canonical_sha256(&"A".repeat(64)).unwrap(),
            format!("sha256:{}", "a".repeat(64))
        );
        assert!(canonical_sha256(&format!("sha256:{}", "a".repeat(64))).is_err());
        assert!(canonical_sha256("abc").is_err());
        assert!(canonical_sha256(&"z".repeat(64)).is_err());
    }

    #[test]
    fn application_identity_distinguishes_absent_and_present_lockfile() {
        let repo = tempfile::tempdir().unwrap();
        let wasm = repo.path().join("index_bg.wasm");
        std::fs::write(&wasm, b"wasm").unwrap();
        let absent = ApplicationArtifactIdentity::from_repo(repo.path(), "app", &wasm).unwrap();
        assert_eq!(absent.dependency_lock, WaferLockIdentity::absent());
        absent.verify_repo(repo.path(), &wasm).unwrap();

        std::fs::write(repo.path().join("wafer.lock"), "version = 2\n").unwrap();
        assert!(absent.verify_repo(repo.path(), &wasm).is_err());
        let present = ApplicationArtifactIdentity::from_repo(repo.path(), "app", &wasm).unwrap();
        assert_ne!(present.dependency_lock, WaferLockIdentity::absent());
        assert!(present.dependency_lock_json().unwrap().contains("present"));
    }

    #[test]
    fn prepare_response_is_strict_and_requires_success() {
        let plan = plan();
        let parsed = parse_prepare_response(&successful_prepare_json(&plan)).unwrap();
        assert_eq!(parsed.plan.plan_hash, plan.plan_hash);
        assert_eq!(
            parsed.plan.schema_version,
            PREPARED_RUNTIME_PLAN_SCHEMA_VERSION
        );

        let mut unknown: serde_json::Value =
            serde_json::from_slice(&successful_prepare_json(&plan)).unwrap();
        unknown["unexpected"] = serde_json::json!(true);
        assert!(parse_prepare_response(&serde_json::to_vec(&unknown).unwrap()).is_err());

        let mut failed: serde_json::Value =
            serde_json::from_slice(&successful_prepare_json(&plan)).unwrap();
        failed["init_report"]["ok"] = serde_json::json!(false);
        assert!(parse_prepare_response(&serde_json::to_vec(&failed).unwrap()).is_err());

        let unbound = PreparedRuntimePlan::new(
            "example",
            format!("sha256:{}", "a".repeat(64)),
            WaferLockIdentity::absent(),
            PreparedReleaseAssets::absent(),
            PreparedRuntimeStructure {
                application_blocks: Vec::new(),
                routes: Vec::new(),
                built_in_route_count: 0,
                block_settings: Default::default(),
                block_configs: Default::default(),
                final_block_configs: Default::default(),
                wrap_grants: Vec::new(),
                deployment_wrap_grants: Vec::new(),
            },
        )
        .unwrap();
        assert!(parse_prepare_response(&successful_prepare_json(&unbound)).is_err());
    }

    /// The `/_deploy/prepare` init report is the runtime's own
    /// [`BootReport`], not a shape the CLI re-describes. A round trip through
    /// the producer's serializer and the consumer's parser is what makes a
    /// renamed field a compile error here instead of a decode failure against
    /// a Worker that is already live.
    #[test]
    fn the_runtimes_own_boot_report_round_trips_through_the_deploy_parser() {
        let plan = plan();
        let report = BootReport {
            sealed: true,
            seed: StepOutcome {
                ok: true,
                error: None,
            },
            blocks: vec![BlockInitOutcome {
                block: "impresspress/admin".into(),
                ok: true,
                error: None,
            }],
            ok: true,
        };
        let body = serde_json::to_vec(&serde_json::json!({
            "schema_version": DEPLOY_RESPONSE_SCHEMA_VERSION,
            "init_report": report,
            "plan": plan,
        }))
        .unwrap();

        let parsed = parse_prepare_response(&body).unwrap();
        assert_eq!(parsed.init_report, report);
    }

    /// A Worker that grows a step in its boot report must not break every CLI
    /// built before it. The report is a status document nested inside an
    /// envelope that already carries `schema_version`, so an added field
    /// cannot change the meaning of the four flags this parser gates the
    /// deploy on — refusing it would force a lockstep CLI upgrade for a purely
    /// additive change.
    #[test]
    fn an_added_boot_report_field_does_not_break_an_older_deploy_parser() {
        let plan = plan();
        let mut body: serde_json::Value =
            serde_json::from_slice(&successful_prepare_json(&plan)).unwrap();
        body["init_report"]["migrations"] = serde_json::json!({ "ok": true });

        let parsed = parse_prepare_response(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert!(parsed.init_report.ok);
    }

    #[test]
    fn stages_deterministic_text_module_and_wrapper_without_wasm() {
        let out = tempfile::tempdir().unwrap();
        let plan = plan();
        let first = stage_prepared_module(out.path(), &plan).unwrap();
        let first_plan = std::fs::read(&first.plan_path).unwrap();
        let first_shim = std::fs::read(&first.shim_path).unwrap();
        let second = stage_prepared_module(out.path(), &plan).unwrap();

        assert_eq!(std::fs::read(&second.plan_path).unwrap(), first_plan);
        assert_eq!(std::fs::read(&second.shim_path).unwrap(), first_shim);
        assert!(String::from_utf8(first_shim)
            .unwrap()
            .contains("globalThis.__IMPRESSPRESS_PREPARED_RUNTIME_PLAN"));
        assert!(!second.directory.join("index_bg.wasm").exists());
        verify_prepared_module(&second).unwrap();

        std::fs::write(&second.plan_path, b"changed").unwrap();
        assert!(verify_prepared_module(&second).is_err());
    }

    #[test]
    fn verify_response_checks_plan_and_build_identity() {
        let plan = plan();
        let body = serde_json::to_vec(&VerifyResponse {
            schema_version: 1,
            ok: true,
            summary: plan.summary().unwrap(),
            release_asset_verified: true,
            release_asset: None,
        })
        .unwrap();
        parse_verify_response(&body, &plan, None).unwrap();

        let mut wrong_plan = plan;
        wrong_plan.application.id = "other".into();
        assert!(parse_verify_response(&body, &wrong_plan, None).is_err());
    }

    #[test]
    fn verify_accepts_verified_empty_present_release_set_with_null_asset() {
        let staged = tempfile::tempdir().unwrap();
        let release = crate::cli::helpers::cloudflare::assets::release_manifest_from_staged_dir(
            staged.path(),
        )
        .unwrap();
        assert!(release.files.is_empty());
        let plan = PreparedRuntimePlan::new_with_config_generation(
            "example",
            format!("sha256:{}", "a".repeat(64)),
            "2".repeat(32),
            WaferLockIdentity::absent(),
            release.prepared_identity().unwrap(),
            PreparedRuntimeStructure {
                application_blocks: Vec::new(),
                routes: Vec::new(),
                built_in_route_count: 0,
                block_settings: Default::default(),
                block_configs: Default::default(),
                final_block_configs: Default::default(),
                wrap_grants: Vec::new(),
                deployment_wrap_grants: Vec::new(),
            },
        )
        .unwrap();
        let body = serde_json::to_vec(&VerifyResponse {
            schema_version: 1,
            ok: true,
            summary: plan.summary().unwrap(),
            release_asset_verified: true,
            release_asset: None,
        })
        .unwrap();

        parse_verify_response(&body, &plan, None).unwrap();
    }

    #[test]
    fn two_stage_gate_enforces_order_and_identical_wasm() {
        let mut gate = TwoStageDeploymentGate::new();
        assert!(gate.prepared().is_err());
        assert!(
            gate.candidate_uploaded("candidate-1", "wasm-a").is_err(),
            "no main Worker version before the password hasher it calls is deployed"
        );
        gate.password_hasher_deployed().unwrap();
        assert!(gate.password_hasher_deployed().is_err());
        assert!(
            gate.candidate_uploaded("candidate-1", "wasm-a").is_err(),
            "no main Worker version before the pepper's placement is checked"
        );
        assert!(gate.pepper_placement_checked().is_err());
        gate.main_worker_ready().unwrap();
        gate.pepper_placement_checked().unwrap();
        gate.candidate_uploaded("candidate-1", "wasm-a").unwrap();
        assert!(gate.final_uploaded("final-1", "wasm-a").is_err());
        gate.assets_verified().unwrap();
        gate.prepared().unwrap();
        assert!(gate.final_uploaded("final-1", "wasm-b").is_err());
        gate.final_uploaded("final-1", "wasm-a").unwrap();
        assert!(gate.authorize_promotion().is_err());
        gate.final_verified().unwrap();
        gate.authorize_promotion().unwrap();
    }
}
