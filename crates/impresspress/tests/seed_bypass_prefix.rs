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
//!
//! The boot notice's two markers are the same kind of pair: the bundler
//! renders them, the sandbox's export finds the notice by them.

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

/// The rules a worker hands `initialize({ bypass })` are rendered by
/// `impresspress-bundle` and read by the dev block, in two crates that share
/// no type. This crate sees both, so it checks the round trip: what the
/// bundler renders for the dev-sandbox deployment's configuration is what the
/// sandbox reads back, rule for rule — and that `/manifest.json` (the
/// likeliest collision, a PWA manifest an agent writes) is one of them.
#[test]
fn the_sandbox_reads_back_exactly_the_bypass_rules_the_worker_hands_it() {
    let dist = tempfile::tempdir().unwrap();
    std::fs::write(dist.path().join("app_bg.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::write(dist.path().join("app.js"), b"fetch('app_bg.wasm');").unwrap();
    std::fs::write(
        dist.path().join("sw.js.tmpl"),
        b"const BYPASS_RULES = __BYPASS_RULES__;\nif (__BYPASS_CONDITION__) {}\n",
    )
    .unwrap();
    let app = impresspress_bundle::bundle::AppConfig {
        dev_enabled: true,
        extra_bypass_prefix: vec!["/__impresspress_dev/compiler/".to_string()],
        ..Default::default()
    };
    let rendered = impresspress_bundle::bundle::BypassRules::for_bundle("/app", &app);
    impresspress_bundle::bundle::run(dist.path(), dist.path(), app).unwrap();

    let sw = std::fs::read_to_string(dist.path().join("sw.js")).unwrap();
    let data = sw
        .strip_prefix("const BYPASS_RULES = ")
        .and_then(|rest| rest.split_once(";\n"))
        .map(|(data, _)| data)
        .expect("sw.js declares BYPASS_RULES");
    let read: impresspress_core::blocks::dev::BypassRules = serde_json::from_str(data).unwrap();

    assert_eq!(read.exact, rendered.exact);
    assert_eq!(read.prefixes, rendered.prefixes);
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

/// The markers the bundler renders a boot notice between are the ones the
/// export removes it by. A drift here would not fail anything by itself — the
/// export leaves a page with neither marker alone — it would ship the
/// sandbox's "build a website here" text as the boot page of every exported
/// site.
#[test]
fn the_export_removes_the_boot_notice_by_the_markers_the_bundler_renders() {
    assert_eq!(
        impresspress_bundle::bundle::BOOT_NOTICE_START,
        impresspress_core::blocks::dev::export::BOOT_NOTICE_START,
    );
    assert_eq!(
        impresspress_bundle::bundle::BOOT_NOTICE_END,
        impresspress_core::blocks::dev::export::BOOT_NOTICE_END,
    );
    // The same for the wrapper around the deployment's title, which the
    // export replaces with the exported site's name.
    assert_eq!(
        impresspress_bundle::bundle::APP_TITLE_OPEN,
        impresspress_core::blocks::dev::export::APP_TITLE_OPEN,
    );
    assert_eq!(
        impresspress_bundle::bundle::APP_TITLE_CLOSE,
        impresspress_core::blocks::dev::export::APP_TITLE_CLOSE,
    );
}
