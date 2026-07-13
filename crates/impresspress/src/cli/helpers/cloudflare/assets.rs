//! Stage static assets from a consumer's dist/ + content/ + public/ into
//! `target/impresspress-cloudflare/assets/` for later R2 upload.

use std::path::Path;

use anyhow::{Context, Result};

/// Source dirs (relative to repo_root) to mirror into out_dir/assets/.
const ASSET_DIRS: &[&str] = &["dist", "content", "public"];

#[derive(Debug, Default)]
pub struct StageReport {
    pub files_copied: usize,
    pub bytes_copied: u64,
    pub dirs_skipped: Vec<&'static str>,
}

pub fn stage(repo_root: &Path, out_dir: &Path) -> Result<StageReport> {
    let assets_dir = out_dir.join("assets");
    if assets_dir.exists() {
        std::fs::remove_dir_all(&assets_dir)
            .with_context(|| format!("clean {}", assets_dir.display()))?;
    }
    std::fs::create_dir_all(&assets_dir)
        .with_context(|| format!("create {}", assets_dir.display()))?;

    let mut report = StageReport::default();
    for &name in ASSET_DIRS {
        let src = repo_root.join(name);
        if !src.exists() {
            report.dirs_skipped.push(name);
            continue;
        }
        let dst = assets_dir.join(name);
        copy_dir_recursive(&src, &dst, &mut report)?;
    }
    Ok(report)
}

fn copy_dir_recursive(src: &Path, dst: &Path, report: &mut StageReport) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&path, &dst_path, report)?;
        } else if ft.is_file() {
            let bytes = std::fs::copy(&path, &dst_path)
                .with_context(|| format!("copy {} -> {}", path.display(), dst_path.display()))?;
            report.files_copied += 1;
            report.bytes_copied += bytes;
        }
        // symlinks: silently skipped
    }
    Ok(())
}

/// Returns the MIME type for a file extension. `octet-stream` for unknown.
pub fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}
