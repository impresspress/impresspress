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

/// The sandbox's configuration with no staged title is not built with a
/// title from somewhere else: the build fails and names the file.
#[tokio::test]
async fn the_dev_sandbox_configuration_does_not_build_without_its_seeds_title() {
    let sandbox =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/dev-sandbox");
    let tmp = tempdir().unwrap();
    for file in ["impresspress.toml", "boot-notice.html"] {
        fs::copy(sandbox.join(file), tmp.path().join(file)).unwrap();
    }
    let err = sealed_web::build(tmp.path(), false)
        .await
        .expect_err("a sandbox build with no staged title must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("title_file") && msg.contains("boot-title.txt"),
        "{msg}"
    );
}

/// A build with no configuration is an ordinary app: no `llms.txt`, and a
/// boot page with nothing between its heading and its status line.
#[tokio::test]
async fn a_plain_build_has_no_llms_txt_and_no_boot_notice() {
    let tmp = tempdir().unwrap();
    sealed_web::build(tmp.path(), false).await.unwrap();
    let dist = tmp.path().join("dist");
    assert!(!dist.join("llms.txt").exists());
    let index = fs::read_to_string(dist.join("index.html")).unwrap();
    let after_heading = &index[index.find("</h1>").expect("the heading") + "</h1>".len()..];
    let before_status = &after_heading[..after_heading
        .find(r#"<p id="status">"#)
        .expect("status line")];
    assert_eq!(before_status.trim(), "", "{index}");
    assert!(!index.contains("llms.txt"), "{index}");
}
