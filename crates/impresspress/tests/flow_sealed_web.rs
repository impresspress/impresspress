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

/// The dev sandbox's own configuration, built the way `examples/dev-sandbox/
/// build.sh` builds it: its `impresspress.toml` and boot notice, over a staged
/// `seed/` and a compiler tree.
///
/// What a reader with no service worker gets from that deployment is two
/// static files, and both are this configuration's doing rather than the
/// bundler's: `/llms.txt` (an overlay of the staged seed's) and a boot page
/// that says what the sandbox is (`[app] boot_notice`). A plain build — the
/// test above — has neither.
#[tokio::test]
async fn the_dev_sandbox_configuration_emits_a_static_llms_txt_and_a_readable_boot_page() {
    let sandbox =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/dev-sandbox");
    let tmp = tempdir().unwrap();
    for file in ["impresspress.toml", "boot-notice.html"] {
        fs::copy(sandbox.join(file), tmp.path().join(file)).unwrap();
    }
    // What build.sh stages and builds before the CLI runs.
    let llms = "# ImpressPress build sandbox\n\nBuild a website here.\n";
    fs::create_dir_all(tmp.path().join("seed/site")).unwrap();
    fs::write(tmp.path().join("seed/manifest.json"), "{}").unwrap();
    fs::write(tmp.path().join("seed/llms.txt"), llms).unwrap();
    fs::create_dir_all(tmp.path().join("compiler/dist")).unwrap();
    fs::write(tmp.path().join("compiler/dist/manifest.json"), "{}").unwrap();

    sealed_web::build(tmp.path(), false).await.unwrap();
    let dist = tmp.path().join("dist");

    // The static copy, and the one the seed importer reads.
    assert_eq!(fs::read_to_string(dist.join("llms.txt")).unwrap(), llms);
    assert_eq!(
        fs::read_to_string(dist.join("seed/llms.txt")).unwrap(),
        llms
    );

    // The boot page carries the sandbox's notice, verbatim, in its HTML.
    let index = fs::read_to_string(dist.join("index.html")).unwrap();
    let notice = fs::read_to_string(sandbox.join("boot-notice.html")).unwrap();
    assert!(index.contains(&notice), "{index}");
    for needle in [
        r#"href="/llms.txt""#,
        r#"href="/b/dev/enter""#,
        "<noscript>",
    ] {
        assert!(
            index.contains(needle),
            "the boot page lacks {needle}: {index}"
        );
    }

    // `/llms.txt` stays the runtime's once the worker controls the page: it
    // is not a bypass rule, so a site's own `site/llms.txt` is never shadowed
    // by the static file above. And it is not part of the shell an export
    // copies.
    let sw = fs::read_to_string(dist.join("sw.js")).unwrap();
    assert!(!sw.contains("llms.txt"), "sw.js names llms.txt");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dist.join("asset-manifest.json")).unwrap())
            .unwrap();
    assert!(
        !manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str() == Some("llms.txt")),
        "{manifest}"
    );
}

/// A build with no configuration is an ordinary app: no `llms.txt`, and a
/// boot page with nothing between the notice markers.
#[tokio::test]
async fn a_plain_build_has_no_llms_txt_and_no_boot_notice() {
    let tmp = tempdir().unwrap();
    sealed_web::build(tmp.path(), false).await.unwrap();
    let dist = tmp.path().join("dist");
    assert!(!dist.join("llms.txt").exists());
    let index = fs::read_to_string(dist.join("index.html")).unwrap();
    assert!(
        index.contains("<!--boot-notice--><!--/boot-notice-->"),
        "{index}"
    );
    assert!(!index.contains("llms.txt"), "{index}");
}
