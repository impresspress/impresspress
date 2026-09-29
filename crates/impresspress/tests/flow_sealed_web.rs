use std::fs;

use impresspress::cli::flows::sealed_web;
use tempfile::tempdir;

#[tokio::test]
async fn build_emits_dist_with_wasm_and_index() {
    let tmp = tempdir().unwrap();
    sealed_web::build(tmp.path(), false).await.unwrap();

    let dist = tmp.path().join("dist");
    assert!(dist.is_dir(), "expected dist/ to be created");

    let wasm_files: Vec<_> = fs::read_dir(&dist)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("wasm"))
        .collect();
    assert!(
        !wasm_files.is_empty(),
        "expected at least one .wasm in dist/"
    );

    assert!(
        dist.join("index.html").is_file(),
        "expected dist/index.html"
    );
}

/// A present-but-malformed `impresspress.toml` is refused, not treated as
/// "no config" (which would bundle with default app name/title/redirect and
/// skip the operator's overlays without a word).
#[tokio::test]
async fn build_refuses_malformed_impresspress_toml() {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("impresspress.toml"), "[app\n").unwrap();

    let err = sealed_web::build(tmp.path(), false)
        .await
        .expect_err("a malformed impresspress.toml must fail the build");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("parse") && msg.contains("impresspress.toml"),
        "{msg}"
    );
}
