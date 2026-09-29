//! Deploy subprocess orchestration: atomic versioned `wrangler` deploys
//! (dynamic candidate → verified R2 assets → `/_deploy/prepare` → same-Wasm
//! prepared candidate → mutation-free verify/health → promote). The
//! version-upload/promote helpers inherit
//! stdio for interactive progress except `wrangler_versions_upload`,
//! which captures stdout to parse the version id and preview URL.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};
#[cfg(feature = "embed-assets")]
use impresspress_core::ui::assets as ui_assets;
use serde::{Deserialize, Serialize};

use super::{
    assets::ReleaseManifest,
    prepared::{
        parse_prepare_response, parse_verify_response, PrepareResponse, VerifyResponse,
        PREPARE_ENDPOINT, VERIFY_ENDPOINT,
    },
    profile_check::UploadSize,
};

const DEPLOYMENTS_ROOT: &str = ".impresspress/deployments/v1";
/// How long to let the prepared plan's config generation propagate before the
/// concurrency burst. Chosen below `call_deploy_verify`'s own ~61 s retry
/// tolerance for the same lag, and paid once per deploy.
pub const KV_PROPAGATION_SETTLE: std::time::Duration = std::time::Duration::from_secs(30);
const CONCURRENT_SMOKE_TOTAL_REQUESTS: usize = 160;
const CONCURRENT_SMOKE_MAX_IN_FLIGHT: usize = 32;
const CONCURRENT_SMOKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_REPORTED_SMOKE_FAILURES: usize = 8;

/// Output of `wrangler versions upload` (unpromoted deployment).
pub struct VersionUpload {
    pub version_id: String,
    pub preview_url: String,
    /// Exact Wrangler configuration accepted for this upload. On Cloudflare's
    /// Free plan this can be a generated compatibility copy with the
    /// unsupported explicit `cpu_ms = 10` field omitted; the plan still
    /// enforces that same 10 ms request budget.
    pub wrangler_toml: PathBuf,
    /// Cloudflare-reported upload size, when `wrangler` printed one. See
    /// [`super::profile_check::report_upload_size`].
    pub upload_size: Option<UploadSize>,
}

struct UploadAttempt {
    version_id: String,
    preview_url: Option<String>,
    wrangler_toml: PathBuf,
    upload_size: Option<UploadSize>,
}

const FREE_PLAN_CPU_LIMIT_ERROR: &str = "CPU limits are not supported for the Free plan";
const FREE_PLAN_CPU_LIMIT_ERROR_CODE: &str = "100328";

/// SHA-256 identity of the Wasm artifact staged by `worker-build`.
///
/// The value is safe to print and persist in deploy logs. It binds the
/// pre-upload size/profile checks to the exact bytes that the upload-only
/// Wrangler config references.
pub fn artifact_sha256(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read worker artifact {}", path.display()))?;
    Ok(impresspress_core::util::sha256_hex(&bytes))
}

/// Fail if a staged artifact changed while Wrangler uploaded the version.
pub fn verify_artifact_sha256(path: &Path, expected: &str) -> Result<()> {
    let actual = artifact_sha256(path)?;
    if actual != expected {
        bail!(
            "worker artifact changed during upload: expected sha256 {expected}, \
             found {actual} at {}",
            path.display()
        );
    }
    Ok(())
}

fn versions_upload_once(wrangler_toml: &Path) -> Result<UploadAttempt> {
    let run = |config: &Path| {
        Command::new("wrangler")
            .args(["versions", "upload", "--config"])
            .arg(config)
            .output()
            .context("run wrangler versions upload")
    };
    let print_output = |output: &std::process::Output| {
        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    };

    let mut selected_toml = wrangler_toml.to_path_buf();
    let mut output = run(&selected_toml)?;
    print_output(&output);

    if !output.status.success() {
        let diagnostics = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(compatible_toml) =
            free_plan_ten_ms_compatible_config(wrangler_toml, &diagnostics)?
        {
            eprintln!(
                "Cloudflare confirmed its Free plan and rejected the explicit \
                 cpu_ms = 10 field; retrying with {}. The Free plan continues \
                 to enforce the same 10 ms per-request CPU budget.",
                compatible_toml.display()
            );
            selected_toml = compatible_toml;
            output = run(&selected_toml)?;
            print_output(&output);
        }
    }

    if !output.status.success() {
        bail!(
            "wrangler versions upload failed (exit {:?})",
            output.status.code()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // wrangler prints lines like:
    //   Total Upload: 4210.12 KiB / gzip: 1234.56 KiB
    //   Worker Version ID: 8e3c...-....
    //   Version Preview URL: https://<hash>-<worker>.<subdomain>.workers.dev
    let version_id = parse_labeled_line(&stdout, "Version ID:")
        .context("parse Version ID from wrangler versions upload output")?;
    Ok(UploadAttempt {
        version_id,
        preview_url: parse_labeled_line(&stdout, "Preview URL:"),
        wrangler_toml: selected_toml,
        upload_size: super::profile_check::parse_upload_size(&stdout),
    })
}

/// Cloudflare's Free plan enforces a fixed 10 ms request CPU budget but
/// rejects an explicit `limits.cpu_ms` field with API error 100328. Generate
/// an upload-only config without that unsupported field only when both facts
/// are proven by the response and the source config pins exactly 10 ms.
///
/// Any other value, plan response, or malformed config fails closed through
/// the original upload error.
fn free_plan_ten_ms_compatible_config(
    wrangler_toml: &Path,
    diagnostics: &str,
) -> Result<Option<PathBuf>> {
    if !diagnostics.contains(FREE_PLAN_CPU_LIMIT_ERROR)
        || !diagnostics.contains(FREE_PLAN_CPU_LIMIT_ERROR_CODE)
    {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(wrangler_toml)
        .with_context(|| format!("read {}", wrangler_toml.display()))?;
    let mut value: toml::Value =
        toml::from_str(&raw).with_context(|| format!("parse {}", wrangler_toml.display()))?;
    let root = value
        .as_table_mut()
        .context("generated Wrangler configuration must be a TOML table")?;
    let remove_empty_limits = {
        let Some(limits) = root.get_mut("limits").and_then(toml::Value::as_table_mut) else {
            return Ok(None);
        };
        if limits.get("cpu_ms").and_then(toml::Value::as_integer) != Some(10) {
            return Ok(None);
        }
        limits.remove("cpu_ms");
        limits.is_empty()
    };
    if remove_empty_limits {
        root.remove("limits");
    }

    let stem = wrangler_toml
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("wrangler");
    let compatible_toml = wrangler_toml.with_file_name(format!("{stem}.free-plan-10ms.toml"));
    let body = toml::to_string_pretty(&value).context("serialize Free-plan Wrangler config")?;
    std::fs::write(
        &compatible_toml,
        format!(
            "# Generated by ImPressPress after Cloudflare API error 100328.\n\
             # The Free plan enforces the configured 10 ms request CPU budget but\n\
             # rejects an explicit limits.cpu_ms field.\n\n{body}"
        ),
    )
    .with_context(|| format!("write {}", compatible_toml.display()))?;
    Ok(Some(compatible_toml))
}

/// Apply worker-level settings from the generated toml to the live worker
/// without uploading code: routes and custom domains, the `workers.dev` /
/// `preview_urls` subdomain state, cron schedules, queue consumers, and
/// workflow bindings.
///
/// These are exactly the settings `wrangler versions upload` and `wrangler
/// versions deploy` do **not** touch — versioned settings travel with a Worker
/// version, worker-level ones do not — which is why the upload closes with
/// "Changes to triggers (routes, custom domains, cron schedules, etc) must be
/// applied with the command `wrangler triggers deploy`". `impresspress deploy`
/// is those two commands, so without this call the `[triggers] crons` an
/// operator configured would never reach the live Worker.
pub fn wrangler_triggers_deploy(wrangler_toml: &Path) -> Result<()> {
    let status = Command::new("wrangler")
        .args(["triggers", "deploy", "--config"])
        .arg(wrangler_toml)
        .status()
        .context("run wrangler triggers deploy")?;
    if !status.success() {
        bail!("wrangler triggers deploy failed (exit {:?})", status.code());
    }
    Ok(())
}

/// Upload a new worker version WITHOUT routing traffic to it. Captures
/// stdout (unlike the inherit-stdio helpers) to parse the version id,
/// preview URL, and (if present) wrangler's own reported upload size.
///
/// First-enable recovery: `preview_urls = true` in the toml is a
/// worker-level setting; the first upload after enabling it prints no
/// preview URL until a one-time `wrangler triggers deploy` applies it.
/// Detect that, apply settings, and retry the upload once.
pub fn wrangler_versions_upload(wrangler_toml: &Path) -> Result<VersionUpload> {
    let attempt = versions_upload_once(wrangler_toml)?;
    let attempt = match attempt.preview_url {
        Some(preview_url) => {
            return Ok(VersionUpload {
                version_id: attempt.version_id,
                preview_url,
                wrangler_toml: attempt.wrangler_toml,
                upload_size: attempt.upload_size,
            })
        }
        None => {
            eprintln!(
                "no Version Preview URL in upload output — first deploy since \
                 enabling preview_urls; applying worker settings via \
                 `wrangler triggers deploy` and retrying the upload once"
            );
            wrangler_triggers_deploy(&attempt.wrangler_toml)?;
            versions_upload_once(&attempt.wrangler_toml)?
        }
    };
    let preview_url = attempt.preview_url.with_context(|| {
        format!(
            "still no Version Preview URL after applying settings; run \
             `wrangler triggers deploy --config {}` manually, then retry the deploy",
            wrangler_toml.display()
        )
    })?;
    Ok(VersionUpload {
        version_id: attempt.version_id,
        preview_url,
        wrangler_toml: attempt.wrangler_toml,
        upload_size: attempt.upload_size,
    })
}

fn parse_labeled_line(stdout: &str, label: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|l| l.split(label).nth(1))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Promote an uploaded version to 100% of traffic.
pub fn wrangler_versions_promote(version_id: &str, wrangler_toml: &Path) -> Result<()> {
    let status = Command::new("wrangler")
        .args([
            "versions",
            "deploy",
            &format!("{version_id}@100%"),
            "--yes",
            "--config",
        ])
        .arg(wrangler_toml)
        .status()
        .context("run wrangler versions deploy")?;
    if !status.success() {
        bail!("wrangler versions deploy failed (exit {:?})", status.code());
    }
    Ok(())
}

/// POST /_deploy/init on the preview URL. Returns (ok, report_body).
///
/// Generous timeout: the funnel legitimately runs migrations + seeds for
/// every block in one request. Without one, a hung worker blocks the
/// deploy forever (safe direction — pre-promote — but needs an operator).
pub async fn call_deploy_init(preview_url: &str, token: &str) -> Result<(bool, String)> {
    const DEPLOY_INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
    let url = format!("{}/_deploy/init", preview_url.trim_end_matches('/'));
    let resp = reqwest::Client::builder()
        .timeout(DEPLOY_INIT_TIMEOUT)
        .build()
        .context("build deploy-init http client")?
        .post(&url)
        .header("x-deploy-token", token)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let ok = resp.status().is_success();
    let body = resp
        .text()
        .await
        .with_context(|| format!("read /_deploy/init response body from {url}"))?;
    Ok((ok, body))
}

/// Run the one mutation-bearing deployment request against the dynamic first
/// candidate. The endpoint performs init, reloads structural state, and emits
/// the strict prepared plan in one request-local operation.
pub async fn call_deploy_prepare(preview_url: &str, token: &str) -> Result<PrepareResponse> {
    const PREPARE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
    let url = format!("{}{PREPARE_ENDPOINT}", preview_url.trim_end_matches('/'));
    let response = reqwest::Client::builder()
        .timeout(PREPARE_TIMEOUT)
        .build()
        .context("build deploy-prepare http client")?
        .post(&url)
        .header("x-deploy-token", token)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("read {PREPARE_ENDPOINT} response from {url}"))?;
    if !status.is_success() {
        bail!(
            "{PREPARE_ENDPOINT} failed with {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    parse_prepare_response(&bytes)
}

/// Verify the packaged Text module, plan identities, hydrated structure, and
/// one representative immutable release asset without rerunning mutations.
pub async fn call_deploy_verify(
    preview_url: &str,
    token: &str,
    expected_plan: &impresspress_core::PreparedRuntimePlan,
    release: &ReleaseManifest,
) -> Result<VerifyResponse> {
    const VERIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
    // The preparation candidate writes the exact structural generation to
    // Workers KV immediately before exporting the plan. A final-version
    // preview can land in another colo before that KV write has propagated.
    // Retry only server-side verification failures; malformed auth/routes
    // remain immediate hard failures and persistent mismatches never promote.
    const VERIFY_RETRY_SECS: &[u64] = &[1, 2, 4, 8, 16, 30];
    let url = format!("{}{VERIFY_ENDPOINT}", preview_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(VERIFY_TIMEOUT)
        .build()
        .context("build deploy-verify http client")?;
    let representative = release.files.first().map(|entry| {
        (
            entry.logical_key.as_str(),
            release.immutable_key(&entry.logical_key),
            entry.sha256.as_str(),
        )
    });
    // The attempt budget IS the delay list: one attempt, then one more for
    // each delay it yields — seven attempts across the six delays above. The
    // first `next()` that comes back empty is the terminal failure, which is
    // why this loop needs neither an attempt counter nor an unreachable tail.
    let mut backoff = VERIFY_RETRY_SECS.iter().copied();
    loop {
        let response = client
            .post(&url)
            .header("x-deploy-token", token)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("read {VERIFY_ENDPOINT} response from {url}"))?;
        if status.is_success() {
            return parse_verify_response(
                &bytes,
                expected_plan,
                representative
                    .as_ref()
                    .map(|(logical, immutable, hash)| (*logical, immutable.as_str(), *hash)),
            );
        }
        let retry_in = if status.is_server_error() {
            backoff.next()
        } else {
            None
        };
        let Some(delay) = retry_in else {
            bail!(
                "{VERIFY_ENDPOINT} failed with {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        };
        eprintln!(
            "-> {VERIFY_ENDPOINT} returned {status}; retrying in {delay}s for KV propagation"
        );
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
    }
}

/// Exercise an ordinary final-candidate route through preview-host lockdown.
/// The request-current deploy token is accepted only for this pre-promotion
/// smoke path; wrong/missing credentials remain a 404 at the runtime layer.
pub async fn smoke_authenticated_get(preview_url: &str, token: &str, path: &str) -> Result<()> {
    if !path.starts_with('/') {
        bail!("smoke path must start with '/': {path:?}");
    }
    const SMOKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
    let url = format!("{}{path}", preview_url.trim_end_matches('/'));
    let response = reqwest::Client::builder()
        .timeout(SMOKE_TIMEOUT)
        .build()
        .context("build deploy smoke http client")?
        .get(&url)
        .header("x-deploy-token", token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !response.status().is_success() {
        bail!(
            "final candidate smoke GET {path} failed with {}",
            response.status()
        );
    }
    Ok(())
}

/// Exercise overlapping ordinary requests against the authenticated final
/// preview before it is eligible for promotion.
///
/// A single `/health` request cannot detect request-concurrency failures in
/// lazily initialized application blocks. This gate launches a bounded burst
/// through the existing deploy-token preview bypass and requires every direct
/// response to be 2xx. Unique query values prevent intermediary caching from
/// collapsing the requests.
/// Wait for the structural config generation written by `/_deploy/prepare` to
/// propagate across Cloudflare colos before the concurrency burst starts.
///
/// `call_deploy_verify` already tolerates this lag (`VERIFY_RETRY_SECS`), but
/// it stops at the FIRST colo that answers — while the 160-request burst is
/// spread across many. An isolate that lands in a colo whose KV copy is still
/// stale fails `prepared_generation_matches`, abandons the packaged plan, and
/// pays a full dynamic runtime build inside its own request budget. On the
/// free plan that build does not fit in 10 ms of CPU, so it surfaces as a 500
/// ("Worker exceeded resource limits", or "Too many subrequests" when its
/// D1/KV/R2 calls also pass the 1,000 internal-service subrequests an
/// invocation may make) rather than the cheap 503 the runtime used to return.
///
/// This is a plain WAIT, deliberately not a warm-up: it issues no requests to
/// the smoke paths, so the burst still measures genuinely cold isolates and
/// the gate keeps its capacity-testing value. It only removes a false failure
/// mode that has nothing to do with the candidate's capacity.
///
/// The caller supplies the duration (`KV_PROPAGATION_SETTLE` for a real
/// deploy) so tests against a local mock server can pass `Duration::ZERO`
/// instead of sleeping through a propagation lag that does not exist there.
async fn settle_for_kv_propagation(settle: std::time::Duration) {
    if settle.is_zero() {
        return;
    }
    eprintln!(
        "-> settling {}s for structural-generation KV propagation before the concurrency smoke",
        settle.as_secs()
    );
    tokio::time::sleep(settle).await;
}

pub async fn smoke_authenticated_concurrency(
    preview_url: &str,
    token: &str,
    paths: &[String],
    settle: std::time::Duration,
) -> Result<()> {
    if paths.is_empty() {
        bail!("concurrency smoke paths must contain at least one path");
    }
    settle_for_kv_propagation(settle).await;
    let preview_url = preview_url.trim_end_matches('/');
    let base_urls = paths
        .iter()
        .map(|path| {
            reqwest::Url::parse(&format!("{preview_url}{path}"))
                .with_context(|| format!("parse concurrency smoke URL for path {path:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let client = reqwest::Client::builder()
        .timeout(CONCURRENT_SMOKE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build concurrent deploy smoke http client")?;
    let mut requests = tokio::task::JoinSet::new();
    let mut next_request_id = 0;

    // Seed a full P32 window. Each completion below admits exactly one more
    // request until all 160 have run, so the hard in-flight ceiling cannot be
    // exceeded while the workload remains continuously pressured.
    while next_request_id < CONCURRENT_SMOKE_MAX_IN_FLIGHT {
        spawn_concurrent_smoke_request(
            &mut requests,
            &client,
            token,
            &base_urls,
            paths,
            next_request_id,
        );
        next_request_id += 1;
    }

    let mut failures = Vec::new();
    while let Some(joined) = requests.join_next().await {
        match joined {
            Ok((_, _, Ok(()))) => {}
            Ok((request_id, path, Err(detail))) => {
                failures.push(format!("#{request_id} GET {path}: {detail}"));
            }
            Err(error) => failures.push(format!("request task failed: {error}")),
        }
        if next_request_id < CONCURRENT_SMOKE_TOTAL_REQUESTS {
            spawn_concurrent_smoke_request(
                &mut requests,
                &client,
                token,
                &base_urls,
                paths,
                next_request_id,
            );
            next_request_id += 1;
        }
    }
    if failures.is_empty() {
        return Ok(());
    }

    failures.sort();
    let omitted = failures.len().saturating_sub(MAX_REPORTED_SMOKE_FAILURES);
    failures.truncate(MAX_REPORTED_SMOKE_FAILURES);
    let mut summary = failures.join("; ");
    if omitted > 0 {
        summary.push_str(&format!("; {omitted} more failure(s) omitted"));
    }
    bail!(
        "final candidate mixed concurrency smoke failed: {}/{} requests failed: {summary}",
        omitted + failures.len(),
        CONCURRENT_SMOKE_TOTAL_REQUESTS
    )
}

fn spawn_concurrent_smoke_request(
    requests: &mut tokio::task::JoinSet<(usize, String, std::result::Result<(), String>)>,
    client: &reqwest::Client,
    token: &str,
    base_urls: &[reqwest::Url],
    paths: &[String],
    request_id: usize,
) {
    let client = client.clone();
    let token = token.to_owned();
    let path_index = request_id % paths.len();
    let path = paths[path_index].clone();
    let mut url = base_urls[path_index].clone();
    url.query_pairs_mut()
        .append_pair("__impresspress_smoke", &request_id.to_string());
    requests.spawn(async move {
        let result = concurrent_smoke_request(&client, url, &token).await;
        (request_id, path, result)
    });
}

async fn concurrent_smoke_request(
    client: &reqwest::Client,
    url: reqwest::Url,
    token: &str,
) -> std::result::Result<(), String> {
    let mut response = client
        .get(url)
        .header("x-deploy-token", token)
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }

    let body = bounded_response_summary(&mut response, token)
        .await
        .unwrap_or_else(|error| format!("<body read failed: {error}>"));
    Err(format!("{status}, body {body:?}"))
}

async fn bounded_response_summary(
    response: &mut reqwest::Response,
    token: &str,
) -> reqwest::Result<String> {
    const MAX_BODY_BYTES: usize = 512;
    let mut body = Vec::new();
    while body.len() < MAX_BODY_BYTES {
        let Some(chunk) = response.chunk().await? else {
            break;
        };
        let remaining = MAX_BODY_BYTES - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }

    let body = String::from_utf8_lossy(&body);
    let redacted = if !token.is_empty() && body.contains(token) {
        body.replace(token, "[REDACTED]")
    } else {
        body.into_owned()
    };
    let body = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    if body.is_empty() {
        Ok("<empty>".to_string())
    } else {
        Ok(body)
    }
}

/// Prove that an unpromoted `*.workers.dev` preview does not expose the
/// consumer's homepage through either GET or HEAD. This specifically guards
/// app-level static fast paths that can otherwise run before ImpressPress's
/// deploy-token lockdown.
pub async fn smoke_preview_lockdown(preview_url: &str) -> Result<()> {
    const SMOKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
    let url = format!("{}/", preview_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(SMOKE_TIMEOUT)
        .build()
        .context("build preview-lockdown http client")?;
    for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
        let response = client
            .request(method.clone(), &url)
            .send()
            .await
            .with_context(|| format!("{method} {url}"))?;
        if response.status() != reqwest::StatusCode::NOT_FOUND {
            bail!(
                "unpromoted preview {method} / returned {}; expected 404 lockdown",
                response.status()
            );
        }
    }
    Ok(())
}

/// Deploy the password-hasher Worker with plain `wrangler deploy`: it uploads
/// and activates in one step, and applies the config's Durable Object
/// migration, which `wrangler versions upload` cannot carry. The hasher has no
/// preview to verify first — Cloudflare generates none for a Worker that
/// implements a Durable Object — so what keeps a deploy safe is the protocol's
/// compatibility rule (`impresspress_password::protocol`): the hasher answers
/// the live main Worker's requests as well as the new one's.
pub fn wrangler_deploy_password_hasher(wrangler_toml: &Path) -> Result<()> {
    let status = Command::new("wrangler")
        .args(["deploy", "--config"])
        .arg(wrangler_toml)
        .status()
        .context("run wrangler deploy for the password-hasher Worker")?;
    if !status.success() {
        bail!(
            "wrangler deploy of the password-hasher Worker failed (exit {:?})",
            status.code()
        );
    }
    Ok(())
}

/// The NAMES of a Worker's secrets, via `wrangler secret list --format json`
/// against `wrangler_toml`'s Worker. Wrangler prints names and types only;
/// no value is ever read.
pub fn wrangler_secret_names(wrangler_toml: &Path) -> Result<Vec<String>> {
    let output = Command::new("wrangler")
        .args(["secret", "list", "--format", "json", "--config"])
        .arg(wrangler_toml)
        .output()
        .context("run wrangler secret list")?;
    if !output.status.success() {
        bail!(
            "wrangler secret list --config {} failed (exit {:?}): {}",
            wrangler_toml.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    super::password_hasher::parse_secret_names(&String::from_utf8_lossy(&output.stdout))
}

/// Resolve a worker secret's value (see `first_create::resolve_worker_secrets`): reuse a caller-provided
/// value (from the same-named env var) when present and non-empty, otherwise
/// generate one by hex-encoding `random_bytes`. Returns `(value, generated)`
/// where `generated` is `true` when the value was freshly minted (so the CLI
/// can print an export reminder for the deploy token). Pure — the impure env
/// lookup and randomness are supplied by the caller so this stays host-testable.
pub fn resolve_secret(from_env: Option<String>, random_bytes: &[u8]) -> (String, bool) {
    match from_env {
        Some(v) if !v.is_empty() => (v, false),
        _ => (impresspress_core::util::hex_encode(random_bytes), true),
    }
}

/// Immutable record joining a Cloudflare Worker version to the exact Wasm and
/// R2 release asset set it was validated against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeploymentReleaseRecord {
    pub schema_version: u32,
    pub worker_version_id: String,
    pub wasm_sha256: String,
    pub asset_set_sha256: String,
    pub immutable_prefix: String,
    pub release_manifest_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_plan_hash: Option<String>,
    /// Whether this deployment wrote mutable logical asset keys.
    ///
    /// Rollback-safe deploys always set this to `false`. The field remains in
    /// the schema so stored records explicitly describe historical behavior.
    pub logical_key_compatibility_projection: bool,
}

impl DeploymentReleaseRecord {
    pub fn key(&self) -> String {
        format!("{DEPLOYMENTS_ROOT}/{}.json", self.worker_version_id)
    }

    fn to_pretty_json(&self) -> Result<Vec<u8>> {
        let mut bytes =
            serde_json::to_vec_pretty(self).context("serialize deployment release record")?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct R2ReleaseUploadReport {
    pub immutable_files_uploaded: usize,
    pub metadata_objects_uploaded: usize,
    pub objects_verified: usize,
    pub deployment_record_key: String,
}

/// Report for [`r2_upload_ui_assets`].
#[cfg(feature = "embed-assets")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiAssetUploadReport {
    pub uploaded: usize,
}

/// Publish the embedded UI asset set (CSS/JS/fonts/logos) to R2 at their
/// hashed filenames, as plain bucket-root keys.
///
/// Unlike [`r2_upload_release`], these objects are deliberately NOT wrapped
/// under a shared content-addressed `immutable_prefix`: each object's own
/// filename already carries its content hash (baked in by `build.rs`), so
/// every object is already immutable and content-addressed on its own — and
/// a plain root-level key is exactly what `impresspress_cloudflare::run_with_config`
/// reads back to resolve `/b/static/{filename}` on a lean (no `embed-assets`)
/// worker once `IMPRESSPRESS_ASSET_BASE_URL` points at `/b/static/` (see
/// [`super::assets::resolve_asset_base_url`]).
///
/// Gated on `embed-assets`: publishing the CLI's UI asset bytes requires the
/// CLI itself to have those bytes compiled in. This is independent of the
/// *worker* build, which is intentionally built lean (without
/// `embed-assets`) and instead reads these objects back from R2 at runtime.
/// See the call site in `cli::flows::embed_cloudflare::deploy` for what
/// happens when the CLI is built without this feature.
#[cfg(feature = "embed-assets")]
pub fn r2_upload_ui_assets(bucket: &str) -> Result<UiAssetUploadReport> {
    let mut client = WranglerR2Client { bucket };
    r2_upload_ui_assets_with_client(&mut client, OBJECT_RETRY_SECS)
}

#[cfg(feature = "embed-assets")]
fn r2_upload_ui_assets_with_client<C: R2ObjectClient>(
    client: &mut C,
    retry_delays: &[u64],
) -> Result<UiAssetUploadReport> {
    let entries = super::assets::ui_asset_entries();
    for entry in &entries {
        let bytes = ui_assets::ASSETS
            .iter()
            .find(|e| e.filename == entry.logical_key)
            .and_then(|e| ui_assets::bytes(e.logical))
            .with_context(|| format!("no embedded bytes for UI asset {}", entry.logical_key))?;
        with_object_retries(
            &entry.logical_key,
            retry_delays,
            |client| {
                client.put_bytes(&entry.logical_key, bytes, &entry.content_type)?;
                verify_remote_sha256(client, &entry.logical_key, &entry.sha256)
            },
            client,
        )?;
    }
    Ok(UiAssetUploadReport {
        uploaded: entries.len(),
    })
}

/// Upload and byte-verify a deterministic release bundle before candidate
/// initialization.
///
/// Files first land under the content-addressed immutable prefix. The release
/// manifest and per-Worker-version deployment record are immutable metadata.
/// The deployer never writes mutable logical keys, lists the bucket, deletes
/// objects, or treats unrelated business/user data as a release artifact.
/// Existing legacy logical objects therefore remain unchanged until a
/// version-aware Worker is promoted.
pub fn r2_upload_release(
    bucket: &str,
    assets_root: &Path,
    release: &ReleaseManifest,
    worker_version_id: &str,
    wasm_sha256: &str,
) -> Result<R2ReleaseUploadReport> {
    let mut client = WranglerR2Client { bucket };
    r2_upload_release_with_client(
        &mut client,
        assets_root,
        release,
        worker_version_id,
        wasm_sha256,
        OBJECT_RETRY_SECS,
    )
}

/// Backoff schedule for one release object's put+verify pair. Three retries
/// is sized for the observed failure mode — a lone spurious `fetch failed`
/// from one `wrangler` invocation among ~900 — not for an actual outage,
/// which should still abort the deploy promptly.
const OBJECT_RETRY_SECS: &[u64] = &[2, 5, 15];

/// Write and verify the final Worker-version record after the second upload.
/// This is separate from [`r2_upload_release`] so immutable files and their
/// release manifest are not redundantly uploaded twice.
pub fn r2_upload_final_deployment_record(
    bucket: &str,
    release: &ReleaseManifest,
    worker_version_id: &str,
    wasm_sha256: &str,
    prepared_plan_hash: &str,
) -> Result<String> {
    let mut client = WranglerR2Client { bucket };
    // Retries matter most here: this record is written AFTER promotion, so a
    // lone spurious wrangler failure would fail a deploy whose Worker is
    // already live.
    let mut record_key = None;
    with_object_retries(
        "final deployment record",
        OBJECT_RETRY_SECS,
        |client| {
            record_key = Some(upload_deployment_record_with_client(
                client,
                release,
                worker_version_id,
                wasm_sha256,
                Some(prepared_plan_hash),
            )?);
            Ok(())
        },
        &mut client,
    )?;
    Ok(record_key.expect("with_object_retries returned Ok without running the record upload"))
}

trait R2ObjectClient {
    fn put_file(&mut self, key: &str, path: &Path, content_type: &str) -> Result<()>;
    fn put_bytes(&mut self, key: &str, bytes: &[u8], content_type: &str) -> Result<()>;
    fn get_bytes(&mut self, key: &str) -> Result<Vec<u8>>;
}

struct WranglerR2Client<'a> {
    bucket: &'a str,
}

impl WranglerR2Client<'_> {
    fn object_path(&self, key: &str) -> String {
        format!("{}/{key}", self.bucket)
    }
}

impl R2ObjectClient for WranglerR2Client<'_> {
    fn put_file(&mut self, key: &str, path: &Path, content_type: &str) -> Result<()> {
        let status = Command::new("wrangler")
            .args(["r2", "object", "put", &self.object_path(key), "--file"])
            .arg(path)
            .args(["--content-type", content_type, "--remote"])
            .status()
            .context("run wrangler r2 object put")?;
        if !status.success() {
            bail!("upload {key} failed (exit {:?})", status.code());
        }
        Ok(())
    }

    fn put_bytes(&mut self, key: &str, bytes: &[u8], content_type: &str) -> Result<()> {
        let mut child = Command::new("wrangler")
            .args([
                "r2",
                "object",
                "put",
                &self.object_path(key),
                "--pipe",
                "--content-type",
                content_type,
                "--remote",
            ])
            .stdin(Stdio::piped())
            .spawn()
            .context("spawn wrangler r2 object put")?;
        child
            .stdin
            .take()
            .context("wrangler r2 object put stdin unavailable")?
            .write_all(bytes)
            .with_context(|| format!("write {key} to wrangler stdin"))?;
        let status = child.wait().context("wait for wrangler r2 object put")?;
        if !status.success() {
            bail!("upload {key} failed (exit {:?})", status.code());
        }
        Ok(())
    }

    fn get_bytes(&mut self, key: &str) -> Result<Vec<u8>> {
        let output = Command::new("wrangler")
            .args([
                "r2",
                "object",
                "get",
                &self.object_path(key),
                "--pipe",
                "--remote",
            ])
            .output()
            .context("run wrangler r2 object get")?;
        if !output.status.success() {
            bail!(
                "verify {key} failed (exit {:?}): {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
    }
}

/// Run one object's put+verify, retrying transient failures with backoff.
///
/// A release upload shells out to `wrangler` twice per object, hundreds of
/// times back to back; at that volume the occasional spurious `fetch failed`
/// from a single invocation is a certainty, not a possibility, and it aborted
/// two otherwise-healthy production deploys before this existed. Re-running
/// the WHOLE pair is deliberate: the verify may have failed because the put
/// itself only half-happened, and both operations are idempotent
/// (digest-pinned immutable keys). Persistent failures still abort — the
/// budget is small and the last error is returned unchanged.
fn with_object_retries<C: R2ObjectClient>(
    key: &str,
    retry_delays: &[u64],
    mut op: impl FnMut(&mut C) -> Result<()>,
    client: &mut C,
) -> Result<()> {
    for (attempt, delay) in retry_delays.iter().enumerate() {
        match op(client) {
            Ok(()) => return Ok(()),
            Err(error) => {
                eprintln!(
                    "-> release object {key} attempt {} failed ({error:#}); retrying in {delay}s",
                    attempt + 1,
                );
                std::thread::sleep(std::time::Duration::from_secs(*delay));
            }
        }
    }
    op(client)
}

fn r2_upload_release_with_client<C: R2ObjectClient>(
    client: &mut C,
    assets_root: &Path,
    release: &ReleaseManifest,
    worker_version_id: &str,
    wasm_sha256: &str,
    retry_delays: &[u64],
) -> Result<R2ReleaseUploadReport> {
    validate_worker_version_id(worker_version_id)?;
    verify_local_release(assets_root, release)?;

    let mut objects_verified = 0;
    for entry in &release.files {
        let path = assets_root.join(&entry.logical_key);
        let immutable_key = release.immutable_key(&entry.logical_key);
        with_object_retries(
            &immutable_key,
            retry_delays,
            |client| {
                client.put_file(&immutable_key, &path, &entry.content_type)?;
                verify_remote_sha256(client, &immutable_key, &entry.sha256)
            },
            client,
        )?;
        objects_verified += 1;
    }

    let manifest_bytes = release.to_pretty_json()?;
    let manifest_key = release.manifest_key();
    with_object_retries(
        &manifest_key,
        retry_delays,
        |client| {
            client.put_bytes(
                &manifest_key,
                &manifest_bytes,
                "application/json; charset=utf-8",
            )?;
            verify_remote_bytes(client, &manifest_key, &manifest_bytes)
        },
        client,
    )?;
    objects_verified += 1;

    let keys_bytes = release.logical_keys_json()?.into_bytes();
    let keys_key = release.keys_key();
    with_object_retries(
        &keys_key,
        retry_delays,
        |client| {
            client.put_bytes(&keys_key, &keys_bytes, "application/json; charset=utf-8")?;
            verify_remote_bytes(client, &keys_key, &keys_bytes)
        },
        client,
    )?;
    objects_verified += 1;

    let mut deployment_key = None;
    with_object_retries(
        "deployment record",
        retry_delays,
        |client| {
            deployment_key = Some(upload_deployment_record_with_client(
                client,
                release,
                worker_version_id,
                wasm_sha256,
                None,
            )?);
            Ok(())
        },
        client,
    )?;
    let deployment_key =
        deployment_key.expect("with_object_retries returned Ok without running the record upload");
    objects_verified += 1;

    Ok(R2ReleaseUploadReport {
        immutable_files_uploaded: release.files.len(),
        metadata_objects_uploaded: 3,
        objects_verified,
        deployment_record_key: deployment_key,
    })
}

fn upload_deployment_record_with_client<C: R2ObjectClient>(
    client: &mut C,
    release: &ReleaseManifest,
    worker_version_id: &str,
    wasm_sha256: &str,
    prepared_plan_hash: Option<&str>,
) -> Result<String> {
    validate_worker_version_id(worker_version_id)?;
    let deployment = DeploymentReleaseRecord {
        schema_version: 1,
        worker_version_id: worker_version_id.to_string(),
        wasm_sha256: wasm_sha256.to_string(),
        asset_set_sha256: release.asset_set_sha256.clone(),
        immutable_prefix: release.immutable_prefix.clone(),
        release_manifest_key: release.manifest_key(),
        prepared_plan_hash: prepared_plan_hash.map(str::to_string),
        logical_key_compatibility_projection: false,
    };
    let deployment_bytes = deployment.to_pretty_json()?;
    let deployment_key = deployment.key();
    client.put_bytes(
        &deployment_key,
        &deployment_bytes,
        "application/json; charset=utf-8",
    )?;
    verify_remote_bytes(client, &deployment_key, &deployment_bytes)?;
    Ok(deployment_key)
}

fn validate_worker_version_id(version_id: &str) -> Result<()> {
    if version_id.is_empty()
        || !version_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        bail!("invalid Cloudflare Worker version id {version_id:?}");
    }
    Ok(())
}

fn verify_local_release(assets_root: &Path, release: &ReleaseManifest) -> Result<()> {
    for entry in &release.files {
        let path = assets_root.join(&entry.logical_key);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read staged release asset {}", path.display()))?;
        let actual = impresspress_core::util::sha256_hex(&bytes);
        if actual != entry.sha256 || bytes.len() as u64 != entry.size {
            bail!(
                "staged release asset changed after manifest generation: {}",
                entry.logical_key
            );
        }
    }
    Ok(())
}

fn verify_remote_sha256<C: R2ObjectClient>(
    client: &mut C,
    key: &str,
    expected_sha256: &str,
) -> Result<()> {
    let bytes = client.get_bytes(key)?;
    let actual = impresspress_core::util::sha256_hex(&bytes);
    if actual != expected_sha256 {
        bail!(
            "R2 object verification failed for {key}: expected sha256 \
             {expected_sha256}, found {actual}"
        );
    }
    Ok(())
}

fn verify_remote_bytes<C: R2ObjectClient>(
    client: &mut C,
    key: &str,
    expected: &[u8],
) -> Result<()> {
    let expected_sha256 = impresspress_core::util::sha256_hex(expected);
    verify_remote_sha256(client, key, &expected_sha256)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::Path,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    use anyhow::Result;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::{Barrier, Mutex},
        task::JoinSet,
    };

    #[cfg(feature = "embed-assets")]
    use super::r2_upload_ui_assets_with_client;
    use super::{
        artifact_sha256, free_plan_ten_ms_compatible_config, parse_labeled_line,
        r2_upload_release_with_client, resolve_secret, smoke_authenticated_concurrency,
        upload_deployment_record_with_client, verify_artifact_sha256, DeploymentReleaseRecord,
        R2ObjectClient, CONCURRENT_SMOKE_MAX_IN_FLIGHT, CONCURRENT_SMOKE_TOTAL_REQUESTS,
        FREE_PLAN_CPU_LIMIT_ERROR, FREE_PLAN_CPU_LIMIT_ERROR_CODE,
    };
    use crate::cli::helpers::cloudflare::assets::release_manifest_from_staged_dir;
    #[cfg(feature = "embed-assets")]
    use crate::cli::helpers::cloudflare::assets::ui_asset_entries;

    fn configured_smoke_paths() -> Vec<String> {
        [
            "/",
            "/catalog/",
            "/catalog/example/",
            "/browse/",
            "/collections/sample/",
        ]
        .map(str::to_string)
        .to_vec()
    }

    #[derive(Clone)]
    struct MockFailure {
        status: &'static str,
        body: String,
        extra_headers: &'static str,
    }

    struct MockWorkload {
        requests: Vec<String>,
        max_active: usize,
    }

    async fn spawn_concurrent_smoke_server(
        failure: Option<MockFailure>,
    ) -> (String, tokio::task::JoinHandle<MockWorkload>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let first_overlap = Arc::new(Barrier::new(2));
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            for connection_id in 0..CONCURRENT_SMOKE_TOTAL_REQUESTS {
                let (mut socket, _) = listener.accept().await.unwrap();
                let first_overlap = first_overlap.clone();
                let recorded = recorded.clone();
                let active = active.clone();
                let max_active = max_active.clone();
                let failure = failure.clone();
                connections.spawn(async move {
                    let mut head = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        let read = socket.read(&mut chunk).await.unwrap();
                        assert!(read > 0, "client closed before request headers completed");
                        head.extend_from_slice(&chunk[..read]);
                        if head.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                        assert!(head.len() < 64 * 1024, "request head exceeded test cap");
                    }
                    recorded
                        .lock()
                        .await
                        .push(String::from_utf8_lossy(&head).into_owned());

                    let current_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(current_active, Ordering::SeqCst);
                    // Force the first two requests to overlap while allowing
                    // the rolling P32 client window to advance normally.
                    if connection_id < 2 {
                        first_overlap.wait().await;
                    }
                    let response = if connection_id == 0 { failure } else { None };
                    let (status, body, extra_headers) = response
                        .map(|failure| (failure.status, failure.body, failure.extra_headers))
                        .unwrap_or(("200 OK", "ok".to_string(), ""));
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\n\
                         Content-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",
                        body.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
            while let Some(connection) = connections.join_next().await {
                connection.unwrap();
            }
            MockWorkload {
                requests: Arc::try_unwrap(recorded).unwrap().into_inner(),
                max_active: max_active.load(Ordering::SeqCst),
            }
        });
        (format!("http://{address}"), server)
    }

    #[derive(Default)]
    struct MemoryR2 {
        objects: BTreeMap<String, Vec<u8>>,
        put_keys: Vec<String>,
        corrupt_on_get: Option<String>,
    }

    impl R2ObjectClient for MemoryR2 {
        fn put_file(&mut self, key: &str, path: &Path, _content_type: &str) -> Result<()> {
            self.put_keys.push(key.to_string());
            self.objects.insert(key.to_string(), std::fs::read(path)?);
            Ok(())
        }

        fn put_bytes(&mut self, key: &str, bytes: &[u8], _content_type: &str) -> Result<()> {
            self.put_keys.push(key.to_string());
            self.objects.insert(key.to_string(), bytes.to_vec());
            Ok(())
        }

        fn get_bytes(&mut self, key: &str) -> Result<Vec<u8>> {
            if self.corrupt_on_get.as_deref() == Some(key) {
                return Ok(b"corrupt".to_vec());
            }
            self.objects
                .get(key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing mock object {key}"))
        }
    }

    #[tokio::test]
    async fn concurrent_smoke_matches_mixed_p32_acceptance_workload() {
        let token = "test-deploy-token";
        let paths = configured_smoke_paths();
        let (preview_url, server) = spawn_concurrent_smoke_server(None).await;

        tokio::time::timeout(
            Duration::from_secs(5),
            smoke_authenticated_concurrency(&preview_url, token, &paths, Duration::ZERO),
        )
        .await
        .expect("concurrent smoke should not hang")
        .unwrap();
        let workload = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("test server should finish")
            .unwrap();

        assert_eq!(workload.requests.len(), CONCURRENT_SMOKE_TOTAL_REQUESTS);
        assert!(workload.max_active >= 2, "workload did not overlap");
        assert!(
            workload.max_active <= CONCURRENT_SMOKE_MAX_IN_FLIGHT,
            "observed {} requests in flight",
            workload.max_active
        );
        let mut targets = BTreeSet::new();
        let mut path_counts = BTreeMap::<String, usize>::new();
        for request in workload.requests {
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("request target");
            assert!(
                targets.insert(target.to_string()),
                "duplicate target {target}"
            );
            let (path, query) = target.split_once('?').expect("smoke query string");
            let request_id = query
                .strip_prefix("__impresspress_smoke=")
                .expect("unique smoke query parameter")
                .parse::<usize>()
                .expect("numeric smoke request id");
            assert!(request_id < CONCURRENT_SMOKE_TOTAL_REQUESTS);
            *path_counts.entry(path.to_string()).or_default() += 1;
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains(&format!("\r\nx-deploy-token: {token}\r\n")),
                "deploy token header missing"
            );
        }
        assert_eq!(targets.len(), CONCURRENT_SMOKE_TOTAL_REQUESTS);
        let expected_per_path = CONCURRENT_SMOKE_TOTAL_REQUESTS / paths.len();
        for path in &paths {
            assert_eq!(
                path_counts.get(path.as_str()).copied(),
                Some(expected_per_path),
                "wrong distribution for {path}"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_smoke_fails_closed_with_redacted_bounded_diagnostic() {
        let token = "top-secret-deploy-token";
        let paths = configured_smoke_paths();
        let failure = MockFailure {
            status: "500 Internal Server Error",
            body: format!("lazy init exploded while handling {token}"),
            extra_headers: "",
        };
        let (preview_url, server) = spawn_concurrent_smoke_server(Some(failure)).await;

        let error = tokio::time::timeout(
            Duration::from_secs(5),
            smoke_authenticated_concurrency(&preview_url, token, &paths, Duration::ZERO),
        )
        .await
        .expect("concurrent smoke should not hang")
        .unwrap_err()
        .to_string();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("test server should finish")
            .unwrap();

        assert!(error.contains("1/160 requests failed"), "{error}");
        assert!(error.contains("500 Internal Server Error"), "{error}");
        assert!(error.contains("lazy init exploded"), "{error}");
        assert!(error.contains("[REDACTED]"), "{error}");
        assert!(!error.contains(token), "deploy token leaked: {error}");
    }

    #[tokio::test]
    async fn concurrent_smoke_rejects_redirects_as_non_2xx() {
        let paths = configured_smoke_paths();
        let failure = MockFailure {
            status: "302 Found",
            body: "do not promote".to_string(),
            extra_headers: "Location: /redirected\r\n",
        };
        let (preview_url, server) = spawn_concurrent_smoke_server(Some(failure)).await;

        let error = tokio::time::timeout(
            Duration::from_secs(5),
            smoke_authenticated_concurrency(&preview_url, "token", &paths, Duration::ZERO),
        )
        .await
        .expect("concurrent smoke should not hang")
        .unwrap_err()
        .to_string();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("test server should finish")
            .unwrap();

        assert!(error.contains("302 Found"), "{error}");
        assert!(error.contains("do not promote"), "{error}");
    }

    #[tokio::test]
    async fn concurrent_smoke_rejects_empty_path_slice() {
        let error =
            smoke_authenticated_concurrency("http://127.0.0.1:1", "token", &[], Duration::ZERO)
                .await
                .unwrap_err();
        assert!(error.to_string().contains("at least one path"));
    }

    #[test]
    fn resolve_secret_prefers_non_empty_env() {
        let (value, generated) = resolve_secret(Some("from-env".to_string()), &[0xab, 0xcd]);
        assert_eq!(value, "from-env");
        assert!(!generated);
    }

    #[test]
    fn resolve_secret_generates_when_env_absent() {
        let (value, generated) = resolve_secret(None, &[0x00, 0xff, 0x0a, 0xbc]);
        // Hex-encoded random bytes, lowercase zero-padded.
        assert_eq!(value, "00ff0abc");
        assert!(generated);
    }

    #[test]
    fn resolve_secret_generates_when_env_empty() {
        // An empty env var is treated as unset — generate instead.
        let (value, generated) = resolve_secret(Some(String::new()), &[0x01, 0x02]);
        assert_eq!(value, "0102");
        assert!(generated);
    }

    #[test]
    fn parses_wrangler_version_lines() {
        let out = "Total Upload: 4210 KiB\nWorker Version ID: abc-123\nVersion Preview URL: https://x-y.z.workers.dev\n";
        assert_eq!(
            parse_labeled_line(out, "Version ID:").as_deref(),
            Some("abc-123")
        );
        assert_eq!(
            parse_labeled_line(out, "Preview URL:").as_deref(),
            Some("https://x-y.z.workers.dev")
        );
    }

    #[test]
    fn missing_label_returns_none() {
        assert_eq!(parse_labeled_line("no labels here", "Version ID:"), None);
    }

    #[test]
    fn free_plan_compat_omits_only_exact_ten_ms_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wrangler-upload.toml");
        std::fs::write(
            &source,
            "account_id = \"acct-test\"\n[limits]\ncpu_ms = 10\n",
        )
        .unwrap();
        let diagnostics =
            format!("{FREE_PLAN_CPU_LIMIT_ERROR} [code: {FREE_PLAN_CPU_LIMIT_ERROR_CODE}]");

        let compatible = free_plan_ten_ms_compatible_config(&source, &diagnostics)
            .unwrap()
            .expect("exact Free-plan 10 ms response should generate a compatible config");
        let body = std::fs::read_to_string(compatible).unwrap();

        assert!(body.contains("account_id = \"acct-test\""));
        assert!(!body.contains("cpu_ms ="));
        assert!(!body.contains("[limits]"));
    }

    #[test]
    fn free_plan_compat_fails_closed_for_other_budget_or_error() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wrangler-upload.toml");
        let diagnostics =
            format!("{FREE_PLAN_CPU_LIMIT_ERROR} [code: {FREE_PLAN_CPU_LIMIT_ERROR_CODE}]");

        std::fs::write(&source, "[limits]\ncpu_ms = 11\n").unwrap();
        assert!(free_plan_ten_ms_compatible_config(&source, &diagnostics)
            .unwrap()
            .is_none());

        std::fs::write(&source, "[limits]\ncpu_ms = 10\n").unwrap();
        assert!(
            free_plan_ten_ms_compatible_config(&source, "authentication error 10000")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn upload_attempt_without_preview_url_is_detectable() {
        let out = "Total Upload: 4210 KiB\nWorker Version ID: abc-123\n";
        assert_eq!(
            parse_labeled_line(out, "Version ID:").as_deref(),
            Some("abc-123")
        );
        assert_eq!(parse_labeled_line(out, "Preview URL:"), None);
    }

    #[test]
    fn artifact_digest_detects_changed_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = tmp.path().join("index_bg.wasm");
        std::fs::write(&artifact, b"wasm-v1").unwrap();

        let expected = artifact_sha256(&artifact).unwrap();
        verify_artifact_sha256(&artifact, &expected).unwrap();

        std::fs::write(&artifact, b"wasm-v2").unwrap();
        let err = verify_artifact_sha256(&artifact, &expected).unwrap_err();
        assert!(
            err.to_string().contains("changed during upload"),
            "unexpected error: {err}"
        );
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn ui_asset_upload_publishes_every_entry_at_its_plain_hashed_filename() {
        let mut r2 = MemoryR2::default();

        let report = r2_upload_ui_assets_with_client(&mut r2, &[]).unwrap();

        let entries = ui_asset_entries();
        assert_eq!(report.uploaded, entries.len());
        assert!(!entries.is_empty());
        for entry in &entries {
            // Plain bucket-root key — NOT wrapped under the release
            // manifest's shared `.impresspress/releases/v1/{hash}/` prefix.
            // Each object's own filename already carries its content hash.
            assert_eq!(r2.objects[&entry.logical_key].len(), entry.size as usize);
            assert_eq!(
                impresspress_core::util::sha256_hex(&r2.objects[&entry.logical_key]),
                entry.sha256
            );
            assert!(!entry.logical_key.contains('/'), "expected a flat key");
        }
    }

    #[test]
    fn release_upload_writes_only_immutable_objects_and_preserves_logical_keys() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(staged.path().join("site/media")).unwrap();
        std::fs::write(staged.path().join("site/media/hero.webp"), b"hero").unwrap();
        std::fs::write(staged.path().join("site/app.js"), b"app").unwrap();
        let release = release_manifest_from_staged_dir(staged.path()).unwrap();
        let mut r2 = MemoryR2::default();
        r2.objects
            .insert("site/media/hero.webp".into(), b"legacy-hero".to_vec());
        r2.objects
            .insert("site/app.js".into(), b"legacy-app".to_vec());
        r2.objects
            .insert("users/avatar.png".into(), b"business-data".to_vec());

        let report = r2_upload_release_with_client(
            &mut r2,
            staged.path(),
            &release,
            "abc-123",
            "wasm-sha",
            &[],
        )
        .unwrap();

        assert_eq!(report.immutable_files_uploaded, 2);
        assert_eq!(report.metadata_objects_uploaded, 3);
        assert_eq!(report.objects_verified, 5);
        assert_eq!(r2.objects["site/media/hero.webp"], b"legacy-hero");
        assert_eq!(r2.objects["site/app.js"], b"legacy-app");
        assert_eq!(r2.objects["users/avatar.png"], b"business-data");
        assert!(!r2.put_keys.iter().any(|key| key == "site/media/hero.webp"));
        assert!(!r2.put_keys.iter().any(|key| key == "site/app.js"));
        for entry in &release.files {
            let expected = std::fs::read(staged.path().join(&entry.logical_key)).unwrap();
            assert_eq!(
                r2.objects[&release.immutable_key(&entry.logical_key)],
                expected
            );
        }

        let manifest_bytes = release.to_pretty_json().unwrap();
        assert_eq!(r2.objects[&release.manifest_key()], manifest_bytes);
        // keys.json is byte-identical to the Worker-pinned inventory encoding
        assert_eq!(
            r2.objects[&release.keys_key()],
            release.logical_keys_json().unwrap().into_bytes()
        );
        let deployment: DeploymentReleaseRecord =
            serde_json::from_slice(&r2.objects[&report.deployment_record_key]).unwrap();
        assert_eq!(deployment.worker_version_id, "abc-123");
        assert_eq!(deployment.wasm_sha256, "wasm-sha");
        assert_eq!(deployment.asset_set_sha256, release.asset_set_sha256);
        assert!(!deployment.logical_key_compatibility_projection);
        assert!(deployment.prepared_plan_hash.is_none());
    }

    #[test]
    fn final_deployment_record_binds_second_worker_to_plan_without_reuploading_assets() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("hero.webp"), b"hero").unwrap();
        let release = release_manifest_from_staged_dir(staged.path()).unwrap();
        let mut r2 = MemoryR2::default();
        let plan_hash = format!("sha256:{}", "c".repeat(64));

        let key = upload_deployment_record_with_client(
            &mut r2,
            &release,
            "final-456",
            "wasm-sha",
            Some(&plan_hash),
        )
        .unwrap();

        assert_eq!(
            r2.objects.len(),
            1,
            "only the final record should be written"
        );
        let record: DeploymentReleaseRecord = serde_json::from_slice(&r2.objects[&key]).unwrap();
        assert_eq!(record.worker_version_id, "final-456");
        assert_eq!(
            record.prepared_plan_hash.as_deref(),
            Some(plan_hash.as_str())
        );
        assert_eq!(record.asset_set_sha256, release.asset_set_sha256);
        assert!(!record.logical_key_compatibility_projection);
    }

    #[test]
    fn release_upload_preflights_local_bytes_before_remote_mutation() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("hero.webp"), b"v1").unwrap();
        let release = release_manifest_from_staged_dir(staged.path()).unwrap();
        std::fs::write(staged.path().join("hero.webp"), b"v2").unwrap();
        let mut r2 = MemoryR2::default();

        let err = r2_upload_release_with_client(
            &mut r2,
            staged.path(),
            &release,
            "abc-123",
            "wasm-sha",
            &[],
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("changed after manifest generation"));
        assert!(r2.objects.is_empty());
    }

    #[test]
    fn release_upload_aborts_on_remote_byte_mismatch() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("hero.webp"), b"hero").unwrap();
        let release = release_manifest_from_staged_dir(staged.path()).unwrap();
        let immutable_key = release.immutable_key("hero.webp");
        let mut r2 = MemoryR2 {
            corrupt_on_get: Some(immutable_key),
            ..Default::default()
        };

        let err = r2_upload_release_with_client(
            &mut r2,
            staged.path(),
            &release,
            "abc-123",
            "wasm-sha",
            &[],
        )
        .unwrap_err();

        assert!(err.to_string().contains("verification failed"));
        assert!(!r2.objects.contains_key("hero.webp"));
    }

    /// One spurious get failure must not abort the release: the put+verify
    /// pair re-runs and the second attempt succeeds. This is the exact shape
    /// of the wrangler `fetch failed` that aborted two production deploys.
    struct FlakyOnceR2 {
        inner: MemoryR2,
        fail_get_once_for: Option<String>,
    }

    impl R2ObjectClient for FlakyOnceR2 {
        fn put_file(&mut self, key: &str, path: &Path, content_type: &str) -> Result<()> {
            self.inner.put_file(key, path, content_type)
        }

        fn put_bytes(&mut self, key: &str, bytes: &[u8], content_type: &str) -> Result<()> {
            self.inner.put_bytes(key, bytes, content_type)
        }

        fn get_bytes(&mut self, key: &str) -> Result<Vec<u8>> {
            if self.fail_get_once_for.as_deref() == Some(key) {
                self.fail_get_once_for = None;
                anyhow::bail!("verify {key} failed (exit Some(1)): fetch failed");
            }
            self.inner.get_bytes(key)
        }
    }

    #[test]
    fn release_upload_retries_a_transient_verify_failure() {
        let staged = tempfile::tempdir().unwrap();
        std::fs::write(staged.path().join("hero.webp"), b"hero").unwrap();
        let release = release_manifest_from_staged_dir(staged.path()).unwrap();
        let immutable_key = release.immutable_key("hero.webp");
        let mut r2 = FlakyOnceR2 {
            inner: MemoryR2::default(),
            fail_get_once_for: Some(immutable_key.clone()),
        };

        let report = r2_upload_release_with_client(
            &mut r2,
            staged.path(),
            &release,
            "abc-123",
            "wasm-sha",
            &[0],
        )
        .unwrap();

        // hero.webp, the release manifest, and the release-metadata objects
        // the upload writes after it (keys inventory + latest pointer).
        assert_eq!(report.objects_verified, 4);
        // The pair re-ran in full: the object was PUT twice, then verified.
        assert_eq!(
            r2.inner
                .put_keys
                .iter()
                .filter(|key| **key == immutable_key)
                .count(),
            2
        );
    }
}
