use std::{fs, path::PathBuf};

use impresspress_bundle::bundle::{run, AppConfig};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/bundle_fixtures/pkg-in")
}

fn default_app() -> AppConfig {
    AppConfig {
        app_name: None,
        app_title: None,
        boot_redirect: None,
        extra_bypass_prefix: Vec::new(),
        extra_bypass_exact: Vec::new(),
        opfs_wipe_on_recovery: false,
    }
}

#[test]
fn exact_bypass_renders_into_sw() {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir(&fixture_path(), tmp.path());

    let app = AppConfig {
        extra_bypass_exact: vec!["/".to_string(), "/index.html".to_string()],
        ..default_app()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    assert!(
        sw.contains("url.pathname === '/'"),
        "sw.js missing exact '/' bypass = {sw}"
    );
    assert!(
        sw.contains("url.pathname === '/index.html'"),
        "sw.js missing exact '/index.html' bypass"
    );
    assert!(
        !sw.contains("__EXTRA_BYPASS_EXACT__"),
        "placeholder not substituted"
    );
}

#[test]
fn exact_bypass_empty_leaves_sw_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir(&fixture_path(), tmp.path());
    run(tmp.path(), tmp.path(), default_app()).expect("bundler ok");
    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    assert!(!sw.contains("__EXTRA_BYPASS_EXACT__"));
    assert!(!sw.contains("=== '/'"));
}

#[test]
fn end_to_end_renames_rewrites_and_templates() {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir(&fixture_path(), tmp.path());

    run(tmp.path(), tmp.path(), default_app()).expect("bundler ok");

    let manifest_body = fs::read_to_string(tmp.path().join("asset-manifest.json")).unwrap();
    assert!(manifest_body.contains("\"buildId\""));
    assert!(manifest_body.contains("\"app.js\""));
    assert!(manifest_body.contains("\"app_bg.wasm\""));

    let entries: Vec<String> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        entries
            .iter()
            .any(|n| n.starts_with("app-") && n.ends_with(".js")),
        "missing hashed JS in {entries:?}"
    );
    assert!(entries
        .iter()
        .any(|n| n.starts_with("app_bg-") && n.ends_with(".wasm")));
    assert!(!entries.iter().any(|n| n == "app.js"));
    assert!(!entries.iter().any(|n| n == "app_bg.wasm"));

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    assert!(sw.contains("from '/app-"), "sw.js = {sw}");
    assert!(!sw.contains("__WASM_JS__"));
    assert!(!sw.contains("__BUILD_ID__"));

    let glue_name = entries
        .iter()
        .find(|n| n.starts_with("app-") && n.ends_with(".js"))
        .unwrap();
    let glue = fs::read_to_string(tmp.path().join(glue_name)).unwrap();
    assert!(glue.contains("app_bg-"), "glue = {glue}");
    assert!(!glue.contains("'app_bg.wasm'"));
}

#[test]
fn deterministic_across_runs() {
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();
    copy_dir(&fixture_path(), tmp1.path());
    copy_dir(&fixture_path(), tmp2.path());
    impresspress_bundle::bundle::run(tmp1.path(), tmp1.path(), default_app()).unwrap();
    impresspress_bundle::bundle::run(tmp2.path(), tmp2.path(), default_app()).unwrap();

    let m1 = fs::read_to_string(tmp1.path().join("asset-manifest.json")).unwrap();
    let m2 = fs::read_to_string(tmp2.path().join("asset-manifest.json")).unwrap();
    let v1: serde_json::Value = serde_json::from_str(&m1).unwrap();
    let v2: serde_json::Value = serde_json::from_str(&m2).unwrap();
    assert_eq!(v1.get("assets"), v2.get("assets"));
}

#[test]
fn empty_exact_leaves_production_sw_bypass_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir(&fixture_path(), tmp.path());

    // Overwrite the fixture stub with the real production template.
    let prod_tmpl = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/sw.js.tmpl"));
    fs::write(tmp.path().join("sw.js.tmpl"), prod_tmpl).unwrap();

    // default_app has both extra_bypass_prefix and extra_bypass_exact empty.
    run(tmp.path(), tmp.path(), default_app()).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();

    // Placeholders must be fully substituted.
    assert!(
        !sw.contains("__EXTRA_BYPASS_EXACT__"),
        "placeholder not substituted in sw.js"
    );
    assert!(
        !sw.contains("__EXTRA_BYPASS__"),
        "placeholder not substituted in sw.js"
    );
    // No injected exact-match bypass clause (the bundler emits " || url.pathname === '<path>'"
    // for each entry; the template itself uses "=== '" for its own static pathname checks but
    // never prefixed with "|| url.pathname").
    assert!(
        !sw.contains("|| url.pathname === '"),
        "unexpected exact-match bypass clause injected into sw.js"
    );
    // The bypass expression must close exactly as in the pre-change form.
    assert!(
        sw.contains("startsWith('/sql-')) {"),
        "production bypass closing token changed; sw.js = {sw}"
    );
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    for entry in fs::read_dir(src).unwrap() {
        let e = entry.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            fs::create_dir_all(&to).unwrap();
            copy_dir(&e.path(), &to);
        } else {
            fs::copy(e.path(), to).unwrap();
        }
    }
}
