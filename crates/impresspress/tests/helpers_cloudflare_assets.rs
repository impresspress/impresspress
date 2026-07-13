use std::{fs, path::Path};

use impresspress::cli::helpers::cloudflare::assets::{mime_for_path, stage};
use tempfile::tempdir;

#[test]
fn mime_for_path_covers_common_extensions() {
    assert_eq!(
        mime_for_path(Path::new("a.html")),
        "text/html; charset=utf-8"
    );
    assert_eq!(mime_for_path(Path::new("x.WASM")), "application/wasm");
    assert_eq!(
        mime_for_path(Path::new("y.unknown")),
        "application/octet-stream"
    );
    assert_eq!(
        mime_for_path(Path::new("noext")),
        "application/octet-stream"
    );
}

#[test]
fn stage_copies_dist_and_content_skips_missing_public() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("dist/sub")).unwrap();
    fs::write(root.join("dist/index.html"), "hello").unwrap();
    fs::write(root.join("dist/sub/app.js"), "console.log(1);").unwrap();
    fs::create_dir_all(root.join("content")).unwrap();
    fs::write(root.join("content/page.md"), "# hi").unwrap();
    // public/ intentionally missing

    let out = root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    let report = stage(root, &out).unwrap();
    assert_eq!(report.files_copied, 3);
    assert!(
        report.dirs_skipped.contains(&"public"),
        "expected 'public' in skipped dirs: {:?}",
        report.dirs_skipped
    );

    assert!(out.join("assets/dist/index.html").is_file());
    assert!(out.join("assets/dist/sub/app.js").is_file());
    assert!(out.join("assets/content/page.md").is_file());
}

#[test]
fn stage_returns_zero_files_when_no_dirs_present() {
    let tmp = tempdir().unwrap();
    let out = tmp.path().join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();
    let report = stage(tmp.path(), &out).unwrap();
    assert_eq!(report.files_copied, 0);
    assert_eq!(report.dirs_skipped.len(), 3);
}
