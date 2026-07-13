//! Deploy subprocess orchestration: atomic versioned `wrangler` deploys
//! (`versions upload` → `/_deploy/init` gate → `versions deploy` promote),
//! plus R2 asset upload. The version-upload/promote helpers inherit
//! stdio for interactive progress except `wrangler_versions_upload`,
//! which captures stdout to parse the version id and preview URL.

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};

use super::assets::mime_for_path;

/// Output of `wrangler versions upload` (unpromoted deployment).
pub struct VersionUpload {
    pub version_id: String,
    pub preview_url: String,
}

struct UploadAttempt {
    version_id: String,
    preview_url: Option<String>,
}

fn versions_upload_once(wrangler_toml: &Path) -> Result<UploadAttempt> {
    let output = Command::new("wrangler")
        .args(["versions", "upload", "--config"])
        .arg(wrangler_toml)
        .output()
        .context("run wrangler versions upload")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    print!("{stdout}");
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        bail!(
            "wrangler versions upload failed (exit {:?})",
            output.status.code()
        );
    }
    // wrangler prints lines like:
    //   Worker Version ID: 8e3c...-....
    //   Version Preview URL: https://<hash>-<worker>.<subdomain>.workers.dev
    let version_id = parse_labeled_line(&stdout, "Version ID:")
        .context("parse Version ID from wrangler versions upload output")?;
    Ok(UploadAttempt {
        version_id,
        preview_url: parse_labeled_line(&stdout, "Preview URL:"),
    })
}

/// Apply worker-level settings (routes, `preview_urls`, observability) from
/// the generated toml to the live worker without uploading code.
fn wrangler_triggers_deploy(wrangler_toml: &Path) -> Result<()> {
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
/// stdout (unlike the inherit-stdio helpers) to parse the version id and
/// preview URL.
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
            })
        }
        None => {
            eprintln!(
                "no Version Preview URL in upload output — first deploy since \
                 enabling preview_urls; applying worker settings via \
                 `wrangler triggers deploy` and retrying the upload once"
            );
            wrangler_triggers_deploy(wrangler_toml)?;
            versions_upload_once(wrangler_toml)?
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

/// Set a worker secret via `wrangler secret put <NAME> --config <toml>`,
/// piping the value on stdin (never as an argv arg, which would leak it into
/// the process table). Stdout/stderr inherit so wrangler's own confirmation
/// shows through. One-time provisioning helper behind `impresspress deploy secret`.
pub fn wrangler_secret_put(wrangler_toml: &Path, name: &str, value: &str) -> Result<()> {
    let mut child = Command::new("wrangler")
        .args(["secret", "put", name, "--config"])
        .arg(wrangler_toml)
        .stdin(Stdio::piped())
        .spawn()
        .context("spawn wrangler secret put")?;
    child
        .stdin
        .take()
        .context("wrangler secret put stdin unavailable")?
        .write_all(value.as_bytes())
        .context("write secret value to wrangler stdin")?;
    let status = child.wait().context("wait for wrangler secret put")?;
    if !status.success() {
        bail!(
            "wrangler secret put {name} failed (exit {:?})",
            status.code()
        );
    }
    Ok(())
}

/// Resolve a secret value for `impresspress deploy secret`: reuse a caller-provided
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

pub fn r2_upload_dir(bucket: &str, assets_root: &Path) -> Result<usize> {
    if !assets_root.is_dir() {
        return Ok(0);
    }
    let mut uploaded = 0;
    walk_files(assets_root, &mut |abs| {
        let rel = abs.strip_prefix(assets_root).unwrap_or(abs);
        let key = rel.to_string_lossy().replace('\\', "/");
        let mime = mime_for_path(abs);
        let status = Command::new("wrangler")
            .args(["r2", "object", "put", &format!("{bucket}/{key}"), "--file"])
            .arg(abs)
            .args(["--content-type", mime, "--remote"])
            .status()
            .context("run wrangler r2 object put")?;
        if !status.success() {
            bail!("upload {} failed (exit {:?})", key, status.code());
        }
        uploaded += 1;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(uploaded)
}

fn walk_files<F>(root: &Path, f: &mut F) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    for entry in std::fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_files(&path, f)?;
        } else if ft.is_file() {
            f(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_labeled_line, resolve_secret};

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
    fn upload_attempt_without_preview_url_is_detectable() {
        let out = "Total Upload: 4210 KiB\nWorker Version ID: abc-123\n";
        assert_eq!(
            parse_labeled_line(out, "Version ID:").as_deref(),
            Some("abc-123")
        );
        assert_eq!(parse_labeled_line(out, "Preview URL:"), None);
    }
}
