use std::fs;

use impresspress::cli::helpers::frontend::find_frontend_dir;
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
