use std::fs;

use impresspress::cli::helpers::frontend::{find_frontend_dir, native_site_dir, stage_native_site};
use tempfile::tempdir;

#[test]
fn returns_none_when_no_dirs_present() {
    let tmp = tempdir().unwrap();
    assert!(find_frontend_dir(tmp.path()).is_none());
}

#[test]
fn prefers_frontend_build_over_public() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("frontend/build")).unwrap();
    fs::create_dir_all(tmp.path().join("public")).unwrap();
    let found = find_frontend_dir(tmp.path()).unwrap();
    assert!(found.ends_with("frontend/build"));
}

#[test]
fn falls_back_to_public() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("public")).unwrap();
    let found = find_frontend_dir(tmp.path()).unwrap();
    assert!(found.ends_with("public"));
}

#[test]
fn native_site_lands_in_the_web_blocks_storage_folder() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("public/css")).unwrap();
    fs::write(tmp.path().join("public/css/app.css"), "body{}").unwrap();
    stage_native_site(tmp.path()).unwrap();
    let dst = native_site_dir(tmp.path());
    assert!(dst.ends_with("data/storage/wafer-run/web/site"));
    assert_eq!(
        fs::read_to_string(dst.join("css/app.css")).unwrap(),
        "body{}"
    );
}

#[test]
fn native_site_is_a_no_op_without_a_site() {
    let tmp = tempdir().unwrap();
    stage_native_site(tmp.path()).unwrap();
    assert!(!native_site_dir(tmp.path()).exists());
}
