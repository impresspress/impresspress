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
