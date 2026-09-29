//! Build-time asset manifest.
//!
//! Hashes every static asset and assembles the CSS bundle so the crate has a
//! single source of truth for asset identity. This must happen at build time:
//! with `embed-assets` off the bytes are not in the binary, so nothing is left
//! to hash at runtime, yet the content-hashed URLs still have to resolve.

use std::{env, fs, path::PathBuf};

use sha2::{Digest, Sha256};

/// CSS bundle order. Explicit, never a glob — see CLAUDE.md, "no magic code".
///
/// Tokens first so custom properties are defined before use; layouts last so
/// they can override component defaults. Every entry must exist on disk —
/// this build.rs panics on a missing layer (see `main`'s `fs::read_to_string`
/// below), even for a layer that has no rules yet.
const CSS_ORDER: &[&str] = &[
    "styles/tokens.css",
    "styles/base.css",
    "styles/components/button.css",
    "styles/components/card.css",
    "styles/components/table.css",
    "styles/components/form.css",
    "styles/components/badge.css",
    "styles/components/modal.css",
    "styles/components/nav.css",
    "styles/components/toast.css",
    "styles/components/palette.css",
    "styles/components/stat.css",
    "styles/components/chart.css",
    "styles/components/auth.css",
    "styles/layouts/shell.css",
    "styles/layouts/page.css",
    "styles/layouts/auth-split.css",
];

/// Single-file assets: (path relative to `src/`, logical key, content type).
///
/// Paths are rooted at `src/`, not at `src/ui`, because an asset lives with
/// whoever owns it: the shared chrome's under `ui/assets/`, a block's under
/// `blocks/<block>/assets/`. The manifest is still one list, and still
/// unconditional — every file on disk gets a hash and a content-addressed
/// filename whatever the Cargo feature set is, because `/b/static/{filename}`
/// must resolve identically for an R2/CDN deployment that publishes from one
/// build and serves from another. Only the *bytes* are feature-gated, and
/// those are declared by the owning module (see `ui::assets::bytes` and
/// `blocks::static_asset_bytes`).
const FILE_ASSETS: &[(&str, &str, &str)] = &[
    (
        "ui/assets/htmx.min.js",
        "htmx.min.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "ui/assets/chrome.js",
        "chrome.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/llm/assets/marked.min.js",
        "marked.min.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/llm/assets/purify.min.js",
        "purify.min.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/llm/assets/llm-chat.js",
        "llm-chat.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/files/assets/files-browser.js",
        "files-browser.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/products/assets/products-wizard.js",
        "products-wizard.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/products/assets/products-manager.js",
        "products-manager.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/products/assets/products-catalog-admin.js",
        "products-catalog-admin.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/products/assets/products-seller-admin.js",
        "products-seller-admin.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/products/assets/products-stripe-setup.js",
        "products-stripe-setup.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/products/assets/products-commerce-portal.js",
        "products-commerce-portal.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "blocks/products/assets/products-order-detail.js",
        "products-order-detail.js",
        "application/javascript; charset=utf-8",
    ),
    (
        "ui/assets/fonts/itim-latin.woff2",
        "itim-latin.woff2",
        "font/woff2",
    ),
    (
        "ui/assets/fonts/itim-latin-ext.woff2",
        "itim-latin-ext.woff2",
        "font/woff2",
    ),
    (
        "ui/assets/impresspress-logo.png",
        "impresspress-logo.png",
        "image/png",
    ),
    (
        "ui/assets/impresspress-logo-2x.png",
        "impresspress-logo-2x.png",
        "image/png",
    ),
    ("ui/assets/favicon.ico", "favicon.ico", "image/x-icon"),
];

/// The WebMCP script is COMPOSED, not served raw.
///
/// `webmcp-core.js` is a fragment — the shared `buildRequest`/`toolOptions`
/// logic, with no IIFE and no `'use strict'`. `webmcp.js` is a tail written to
/// run inside that wrapper. Neither half is a valid script on its own, so the
/// only bytes that may ever reach a browser are the composed ones.
///
/// Composing here rather than at runtime is what keeps the manifest honest:
/// the hash in `/b/static/webmcp-{hash}.js` is the hash of the bytes actually
/// served. Hashing at runtime cannot work at all without `embed-assets` --
/// the bytes are not in the binary, so there is nothing left to hash, yet the
/// content-hashed URL still has to resolve.
const WEBMCP_CORE: &str = "ui/assets/webmcp-core.js";
const WEBMCP_TAIL: &str = "ui/assets/webmcp.js";

/// Wrap the shared core and a tail in one IIFE.
///
/// Byte-for-byte the composition `ui::assets::compose_webmcp_module` applies
/// for the sandbox's module variant, so both WebMCP scripts this crate serves
/// are wrapped identically and `buildRequest`/`toolOptions` behave the same in
/// each. A test in `ui::assets` pins the composed output's shape.
fn compose_webmcp(core: &str, tail: &str) -> String {
    format!("(function () {{\n  'use strict';\n{core}\n{tail}\n}})();\n")
}

fn short_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `foo.css` + `a1b2c3d4` -> `foo-a1b2c3d4.css`
fn hashed_name(logical: &str, hash: &str) -> String {
    match logical.split_once('.') {
        Some((stem, ext)) => format!("{stem}-{hash}.{ext}"),
        None => format!("{logical}-{hash}"),
    }
}

fn main() {
    let src_root = PathBuf::from("src");
    let ui = src_root.join("ui");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let mut entries: Vec<(String, String, String, String, usize)> = Vec::new();

    // --- single files -----------------------------------------------------
    let mut font_names = Vec::new();
    for (rel, logical, ct) in FILE_ASSETS {
        let path = src_root.join(rel);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes =
            fs::read(&path).unwrap_or_else(|e| panic!("missing asset {}: {e}", path.display()));
        let hash = short_hash(&bytes);
        let name = hashed_name(logical, &hash);
        if logical.ends_with(".woff2") {
            font_names.push((logical.to_string(), name.clone()));
        }
        entries.push((logical.to_string(), name, hash, ct.to_string(), bytes.len()));
    }

    // --- composed WebMCP script ------------------------------------------
    let core_path = src_root.join(WEBMCP_CORE);
    let tail_path = src_root.join(WEBMCP_TAIL);
    println!("cargo:rerun-if-changed={}", core_path.display());
    println!("cargo:rerun-if-changed={}", tail_path.display());
    let core = fs::read_to_string(&core_path)
        .unwrap_or_else(|e| panic!("missing asset {}: {e}", core_path.display()));
    let tail = fs::read_to_string(&tail_path)
        .unwrap_or_else(|e| panic!("missing asset {}: {e}", tail_path.display()));
    let webmcp = compose_webmcp(&core, &tail);
    let webmcp_hash = short_hash(webmcp.as_bytes());
    entries.push((
        "webmcp.js".into(),
        hashed_name("webmcp.js", &webmcp_hash),
        webmcp_hash,
        "application/javascript; charset=utf-8".into(),
        webmcp.len(),
    ));
    fs::write(out.join("webmcp.js"), &webmcp).expect("write webmcp.js");

    // --- CSS bundle -------------------------------------------------------
    // Font URLs are rewritten to bare hashed filenames, which resolve relative
    // to the stylesheet's own URL. That is what makes the same bytes correct
    // both at /b/static/ and at a CDN path, with no placeholder substitution.
    let mut css = String::new();
    for rel in CSS_ORDER {
        let path = ui.join(rel);
        println!("cargo:rerun-if-changed={}", path.display());
        let part = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing CSS layer {}: {e}", path.display()));
        css.push_str(&part);
        css.push('\n');
    }
    for (logical, name) in &font_names {
        css = css.replace(&format!("url('{logical}')"), &format!("url('{name}')"));
    }
    assert!(!css.contains("__ITIM"), "stale placeholder left in bundle");

    let css_hash = short_hash(css.as_bytes());
    let css_name = hashed_name("app.css", &css_hash);
    entries.push((
        "app.css".into(),
        css_name,
        css_hash,
        "text/css; charset=utf-8".into(),
        css.len(),
    ));
    fs::write(out.join("app.css"), &css).expect("write app.css");

    // --- manifest ---------------------------------------------------------
    let mut src = String::from(
        "pub struct AssetEntry {\n\
         \x20   pub logical: &'static str,\n\
         \x20   pub filename: &'static str,\n\
         \x20   pub hash: &'static str,\n\
         \x20   pub content_type: &'static str,\n\
         \x20   pub len: usize,\n\
         }\n\
         pub const ASSETS: &[AssetEntry] = &[\n",
    );
    for (logical, name, hash, ct, len) in &entries {
        src.push_str(&format!(
            "    AssetEntry {{ logical: {logical:?}, filename: {name:?}, hash: {hash:?}, content_type: {ct:?}, len: {len} }},\n"
        ));
    }
    src.push_str("];\n");
    fs::write(out.join("asset_manifest.rs"), src).expect("write manifest");
}
