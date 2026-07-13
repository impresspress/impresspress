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
