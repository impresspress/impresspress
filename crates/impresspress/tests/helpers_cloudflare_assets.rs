use std::{fs, path::Path};

#[cfg(feature = "embed-assets")]
use impresspress::cli::helpers::cloudflare::assets::ui_asset_entries;
use impresspress::cli::helpers::cloudflare::assets::{
    release_manifest_from_staged_dir, resolve_asset_base_url, stage,
};
use tempfile::tempdir;

/// A release asset is uploaded, and served, with the type its manifest entry
/// records: the one table every server path uses, so text declares UTF-8.
#[test]
fn release_assets_carry_their_type_and_a_utf8_charset() {
    let tmp = tempdir().unwrap();
    let staged = tmp.path();
    for name in [
        "llms.txt",
        "README.md",
        "feed.xml",
        "data.csv",
        "index.html",
        "x.WASM",
        "noext",
    ] {
        fs::write(staged.join(name), "caf\u{e9}\n").unwrap();
    }
    let release = release_manifest_from_staged_dir(staged).unwrap();
    for (key, expected) in [
        ("llms.txt", "text/plain; charset=utf-8"),
        ("README.md", "text/markdown; charset=utf-8"),
        ("feed.xml", "application/xml; charset=utf-8"),
        ("data.csv", "text/csv; charset=utf-8"),
        ("index.html", "text/html; charset=utf-8"),
        ("x.WASM", "application/wasm"),
        ("noext", "application/octet-stream"),
    ] {
        let entry = release
            .files
            .iter()
            .find(|e| e.logical_key == key)
            .unwrap_or_else(|| panic!("{key} is in the release manifest"));
        assert_eq!(entry.content_type, expected, "{key}");
    }
}

#[test]
fn stage_copies_dist_and_content_skips_missing_public() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("dist/sub")).unwrap();
    fs::write(root.join("dist/index.html"), "hello").unwrap();
    fs::write(root.join("dist/sub/app.js"), "console.log(1);").unwrap();
    fs::create_dir_all(root.join("content")).unwrap();
    fs::write(root.join("content/page.md"), "# hi").unwrap();
    // public/ intentionally missing

    let out = root.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();

    let report = stage(root, &out, None, Path::new(""), &[]).unwrap();
    assert_eq!(report.files_copied, 3);
    assert!(
        report.dirs_skipped.contains(&"public"),
        "expected 'public' in skipped dirs: {:?}",
        report.dirs_skipped
    );

    assert!(out.join("assets/dist/index.html").is_file());
    assert!(out.join("assets/dist/sub/app.js").is_file());
    assert!(out.join("assets/content/page.md").is_file());
}

#[test]
fn stage_returns_zero_files_when_no_dirs_present() {
    let tmp = tempdir().unwrap();
    let out = tmp.path().join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();
    let report = stage(tmp.path(), &out, None, Path::new(""), &[]).unwrap();
    assert_eq!(report.files_copied, 0);
    assert_eq!(report.dirs_skipped.len(), 3);
}

#[test]
fn asset_base_url_prefers_own_origin_when_r2_is_configured() {
    // R2 present: the worker streams from its own bucket, so the URL contract
    // stays same-origin and no cross-origin font CORS is involved.
    assert_eq!(resolve_asset_base_url(true), "/b/static/");
}

#[test]
fn asset_base_url_falls_back_to_versioned_cdn_without_r2() {
    let b = resolve_asset_base_url(false);
    assert!(
        b.starts_with("https://cdn.impresspress.org/ui/v"),
        "got {b}"
    );
    assert!(b.ends_with('/'));
}

#[cfg(feature = "embed-assets")]
#[test]
fn ui_asset_entries_carry_hashed_keys_and_content_types() {
    let entries = ui_asset_entries();
    assert!(entries
        .iter()
        .any(|e| e.logical_key.starts_with("app-") && e.logical_key.ends_with(".css")));
    assert!(entries.iter().all(|e| !e.content_type.is_empty()));
    assert!(entries.iter().all(|e| e.sha256.len() == 64));
}

// Regression for a real repro: `impresspress deploy cloudflare` built with
// `--no-default-features --features sqlite,embed-assets` (a supported
// combination — the worker just doesn't run block-llm/block-files) used to
// panic in `ui_asset_entries()` claiming "publishing requires an
// embed-assets-enabled CLI build" — misleading, since embed-assets *is* on;
// the manifest just also lists block-llm/block-files-gated logical assets
// this feature set has no bytes for. Only meaningful (and only compiled)
// under that exact feature combination; run with:
// `cargo test -p impresspress --no-default-features --features sqlite,embed-assets`.
#[cfg(all(
    feature = "embed-assets",
    not(feature = "block-llm"),
    not(feature = "block-files"),
    not(feature = "block-products")
))]
#[test]
fn ui_asset_entries_skips_block_gated_assets_instead_of_panicking() {
    let entries = ui_asset_entries();
    // The base set (app.css, htmx, chrome.js, webmcp, fonts, logos, favicon)
    // still publishes; exactly the 11 block-gated logical assets are absent
    // from the manifest's bytes on this feature set and must be skipped, not
    // panic the whole publish. They are `marked.min.js`, `purify.min.js` and
    // `llm-chat.js` (block-llm), `files-browser.js` (block-files) and the
    // seven `products-*.js` bundles (block-products).
    assert!(!entries.is_empty());
    assert_eq!(
        entries.len(),
        impresspress_core::ui::assets::ASSETS.len() - 11,
        "expected exactly the 11 block-gated assets to be skipped"
    );
}
