use std::{path::Path, process::Command};

/// Derives a build identifier for a `pkg/` directory.
///
/// Uses `git rev-parse --short=8 HEAD` when available; appends `-dirty` if
/// the working tree has uncommitted changes. Falls back to a SHA-256-8 of
/// the concatenated asset SHAs if `git` is unavailable or fails.
pub fn build_id(repo_dir: &Path, asset_hashes: &[&str]) -> String {
    if let Some(sha) = git_short_sha(repo_dir) {
        let suffix = if git_is_dirty(repo_dir) { "-dirty" } else { "" };
        return format!("{sha}{suffix}");
    }
    let joined: String = asset_hashes.join("");
    crate::bundle::hash::short_hash(joined.as_bytes())
}

fn git_short_sha(dir: &Path) -> Option<String> {
    // Only treat `dir` as a git repo root if it contains a `.git` entry
    // directly. This prevents git from walking up to an outer repo when
    // `dir` is a temp directory created by tests.
    if !dir.join(".git").exists() {
        return None;
    }
    let out = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn git_is_dirty(dir: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_when_not_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let id = build_id(tmp.path(), &["aaaa", "bbbb"]);
        // SHA-256("aaaabbbb") first 8 hex chars
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fallback_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let a = build_id(tmp.path(), &["x", "y"]);
        let b = build_id(tmp.path(), &["x", "y"]);
        assert_eq!(a, b);
    }
}
