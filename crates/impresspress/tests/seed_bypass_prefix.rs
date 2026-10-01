//! The service worker's seed bypass prefix and the dev block's seed URL root
//! are one value, spelled in two crates.
//!
//! `impresspress-bundle` is native bundling tooling that deliberately depends
//! on no impresspress crate (the wasm32 runtime never compiles it), so it
//! restates the prefix rather than importing it. This crate depends on both
//! and is therefore the only place the two spellings can be compared — which
//! is what turns "restated" into something other than "free to drift".
//!
//! A drift here is silent and total: the service worker would intercept
//! `/seed/…`, answer it from the published site, and every fresh instance
//! would boot with no seed and no error.

#[test]
fn the_bundler_bypasses_exactly_the_prefix_the_seed_importer_fetches_from() {
    assert_eq!(
        impresspress_bundle::bundle::SEED_BYPASS_PREFIX,
        impresspress_core::blocks::dev::seed::ROOT,
    );
    // And the manifest the service worker probes is under it, so bypassing the
    // prefix is enough to reach the whole bundle.
    assert!(impresspress_core::blocks::dev::seed::MANIFEST_URL
        .starts_with(impresspress_bundle::bundle::SEED_BYPASS_PREFIX));
}

/// `asset-manifest.json`'s `bypass` is written by `impresspress-bundle` and
/// read by the dev block, in two crates that share no type. This crate sees
/// both, so it checks the round trip: what the bundler writes for the
/// dev-sandbox deployment's configuration is what the sandbox reads back,
/// rule for rule — and that `/manifest.json` (the likeliest collision, a PWA
/// manifest an agent writes) is one of them.
#[test]
fn the_sandbox_reads_back_exactly_the_bypass_rules_the_bundler_writes() {
    let dist = tempfile::tempdir().unwrap();
    std::fs::write(dist.path().join("app_bg.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::write(dist.path().join("app.js"), b"fetch('app_bg.wasm');").unwrap();
    std::fs::write(dist.path().join("sw.js.tmpl"), b"if (__BYPASS__) {}").unwrap();
    let app = impresspress_bundle::bundle::AppConfig {
        dev_enabled: true,
        extra_bypass_prefix: vec!["/__impresspress_dev/compiler/".to_string()],
        ..Default::default()
    };
    impresspress_bundle::bundle::run(dist.path(), dist.path(), app).unwrap();

    let written = std::fs::read(dist.path().join("asset-manifest.json")).unwrap();
    let produced: impresspress_bundle::bundle::manifest::AssetManifest =
        serde_json::from_slice(&written).unwrap();
    let read = impresspress_core::blocks::dev::BypassRules::from_asset_manifest(&written).unwrap();

    assert_eq!(read.exact, produced.bypass.exact);
    assert_eq!(read.prefixes, produced.bypass.prefixes);
    assert!(
        read.exact.contains(&"/manifest.json".to_string()),
        "{read:?}"
    );
    assert!(read
        .prefixes
        .contains(&impresspress_core::blocks::dev::seed::ROOT.to_string()));
    assert!(read.refuse_shadowed("site/manifest.json").is_err());
    assert!(read.refuse_shadowed("site/vendor/bootstrap/x.css").is_ok());
}
