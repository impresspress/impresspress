use std::fs;

use impresspress::cli::flows::sealed_native;
use tempfile::tempdir;

#[tokio::test]
async fn build_in_empty_dir_succeeds_with_no_work() {
    let tmp = tempdir().unwrap();
    sealed_native::build(tmp.path(), false).await.unwrap();
}

#[tokio::test]
async fn build_copies_frontend_to_data_storage_site() {
    let tmp = tempdir().unwrap();
    let fe = tmp.path().join("frontend/build");
    fs::create_dir_all(&fe).unwrap();
    fs::write(fe.join("index.html"), "placeholder-frontend-asset").unwrap();

    sealed_native::build(tmp.path(), false).await.unwrap();

    let copied = tmp
        .path()
        .join("data/storage/wafer-run/web/site/index.html");
    assert!(
        copied.is_file(),
        "expected frontend file copied to {copied:?}"
    );
}

/// A present-but-malformed `impresspress.toml` is refused, not treated as
/// "no config" (which would skip the operator's overlays without a word).
#[tokio::test]
async fn build_refuses_malformed_impresspress_toml() {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("impresspress.toml"), "[app\n").unwrap();

    let err = sealed_native::build(tmp.path(), false)
        .await
        .expect_err("a malformed impresspress.toml must fail the build");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("parse") && msg.contains("impresspress.toml"),
        "{msg}"
    );
}
