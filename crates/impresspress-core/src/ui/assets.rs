//! Embedded static assets — CSS and JS.
//!
//! Asset URLs include a content hash for cache busting:
//! `/b/static/app-{hash}.css` and `/b/static/htmx-{hash}.min.js`

use std::sync::OnceLock;

use crate::routing::STATIC_PREFIX;

include!(concat!(env!("OUT_DIR"), "/asset_manifest.rs"));

/// Look up a manifest entry by logical key. Panics on an unknown key: every
/// call site passes a literal that `build.rs` also knows, so a miss is a build
/// mismatch, not a runtime condition.
pub fn entry(logical: &str) -> &'static AssetEntry {
    ASSETS
        .iter()
        .find(|e| e.logical == logical)
        .unwrap_or_else(|| panic!("asset not in manifest: {logical}"))
}

/// Where assets are fetched from when no explicit base is configured.
/// Version-pinned so publishing a new release never breaks a deployment
/// still running an older one.
pub const DEFAULT_CDN_BASE_TEMPLATE: &str = concat!(
    "https://cdn.impresspress.org/ui/v",
    env!("CARGO_PKG_VERSION"),
    "/"
);

/// Worker var / process env var name resolved by [`base_url`]. Shared
/// single source of truth between the writer (the CLI's `wrangler.toml`
/// generator — `impresspress::cli::helpers::cloudflare::wrangler`) and every
/// reader, so the name can never drift out from under either side.
pub const ASSET_BASE_URL_VAR: &str = "IMPRESSPRESS_ASSET_BASE_URL";

/// Platform-pushed override for [`base_url`], for adapters that cannot rely
/// on `std::env` to see [`ASSET_BASE_URL_VAR`]. Cloudflare Workers stub
/// `std::env` to always-empty on `wasm32-unknown-unknown` — the only channel
/// that carries a Worker `[vars]` entry into Rust is `worker::Env::var`, which
/// requires a live per-request `Env` handle `base_url()` doesn't have. The
/// Cloudflare adapter (`impresspress-cloudflare::run_with_config`) reads the
/// var itself and calls this once per isolate, before dispatching any
/// request, so by the time a page render calls `base_url()` the value is
/// already resolved. Native targets never call this: `std::env::var` already
/// reads the real process environment there.
static BASE_URL_OVERRIDE: OnceLock<Option<String>> = OnceLock::new();

/// Register the platform override described on [`BASE_URL_OVERRIDE`]. Must
/// be called, if at all, before the first call to [`base_url`] — `base_url`
/// caches its resolved value forever after its first call. Idempotent: later
/// calls are silently ignored rather than panicking, because a Cloudflare
/// isolate can build the runtime more than once per isolate lifetime (e.g.
/// the `/_deploy/init` funnel always builds a fresh runtime); every call in
/// a given isolate reads the same fixed Worker var, so only the first one
/// needs to land.
pub fn set_base_url_override(value: Option<String>) {
    let _ = BASE_URL_OVERRIDE.set(value);
}

/// Resolve the asset base URL once.
///
/// 1. `IMPRESSPRESS_ASSET_BASE_URL` (infrastructure config — `IMPRESSPRESS_*`,
///    no `__`, never stored in the DB) wins outright. The deployer sets this
///    when assets live somewhere other than this origin. Read via
///    [`BASE_URL_OVERRIDE`] when a platform adapter pushed one in, otherwise
///    via `std::env::var` directly (the native target's real channel).
/// 2. Otherwise `/b/static/`, served by the system block — from memory when
///    `embed-assets` is on, streamed from R2 when it is off.
///
/// Whether R2 is configured is deploy-time knowledge, so the CLI writes that
/// decision into the env var rather than the runtime sniffing for a backend.
pub fn base_url() -> &'static str {
    static BASE: OnceLock<String> = OnceLock::new();
    BASE.get_or_init(|| {
        let from_override = BASE_URL_OVERRIDE.get().cloned().flatten();
        let resolved = from_override.or_else(|| std::env::var(ASSET_BASE_URL_VAR).ok());
        match resolved {
            Some(v) if !v.trim().is_empty() => {
                let v = v.trim().to_string();
                if v.ends_with('/') {
                    v
                } else {
                    format!("{v}/")
                }
            }
            _ => STATIC_PREFIX.to_string(),
        }
    })
}

/// Full URL for a logical asset key, e.g. `url("app.css")`.
pub fn url(logical: &str) -> String {
    format!("{}{}", base_url(), entry(logical).filename)
}

/// The single embed point for every asset's bytes.
///
/// Two halves, because assets have two owners. [`shared_bytes`] below carries
/// everything the shared chrome owns — one `include_*!` per file, exactly as
/// before. Everything else is a *block's* asset, and its bytes are declared
/// by that block ([`crate::blocks::static_asset_bytes`]), so this module names
/// no block and carries no `block-*` feature gate of its own: adding an asset
/// to the LLM block is a change inside `blocks/llm/`, not a new arm here.
///
/// The manifest stays one list either way (`build.rs` hashes every file on
/// disk regardless of features), so `url()` and `/b/static/{filename}` behave
/// identically whichever half owns the bytes.
#[cfg(feature = "embed-assets")]
pub fn bytes(logical: &str) -> Option<&'static [u8]> {
    shared_bytes(logical).or_else(|| crate::blocks::static_asset_bytes(logical))
}

/// Bytes for the assets the shared chrome itself owns.
///
/// Each arm is either the asset's own `include_str!`/`include_bytes!` literal
/// or (for `app.css`) delegates to `css()`, itself an `include_str!` of the
/// `build.rs`-assembled bundle — either way, exactly one `include_*!` per
/// source file, so asset content has one source-level truth.
#[cfg(feature = "embed-assets")]
fn shared_bytes(logical: &str) -> Option<&'static [u8]> {
    Some(match logical {
        "app.css" => css().as_bytes(),
        // htmx 2.x minified JS.
        "htmx.min.js" => include_str!("assets/htmx.min.js").as_bytes(),
        // The shared chrome's own behaviour — command palette, mobile
        // drawer, toasts, modals — in one file. See `chrome_js`.
        "chrome.js" => chrome_js().as_bytes(),
        // The COMPOSED WebMCP script, assembled by `build.rs` from
        // `webmcp-core.js` and `webmcp.js` — see `webmcp_js`.
        "webmcp.js" => webmcp_js().as_bytes(),
        // Itim font binaries, sourced from `impresspress/site-kit`'s
        // `/fonts/` mirror and committed here so every impresspress
        // deployment ships its own glyphs (no cross-origin runtime
        // dependency, no `https://impresspress.org/fonts/` 404).
        "itim-latin.woff2" => include_bytes!("assets/fonts/itim-latin.woff2"),
        "itim-latin-ext.woff2" => include_bytes!("assets/fonts/itim-latin-ext.woff2"),
        // Square Impresspress mark used as the sidebar/login icon. Bundled
        // locally so the admin renders correctly without internet (the
        // previous default pointed at `https://impresspress.org/images/logo.png`
        // which 404s offline). Real pixel art -- 32x32 art-pixels, 11 colours,
        // taken pixel-for-pixel from the published mark at
        // `https://impresspress.org/images/logo-32.webp`. Templates only ever
        // scale it by whole factors (`.pixel-art`), so its native size is part
        // of the contract (see the tests below).
        "impresspress-logo.png" => include_bytes!("assets/impresspress-logo.png"),
        // The same mark at 64x64 (`https://impresspress.org/images/favicon.webp`,
        // an exact 2x of the 32) -- served as the `2x` `srcset` candidate so a
        // high-DPI screen maps one source pixel to one device pixel. The art
        // grid stays 32 cells either way, so this carries no detail the 32
        // lacks; it spares the browser the scale step, nothing more.
        // There is no raster wordmark: brand text is rendered
        // as text next to the mark (see `templates::brand_lockup`) -- the
        // old `impresspress-logo-long.png` wordmark was dark-ink artwork
        // illegible on the navy chrome and has been removed outright; rows
        // still pointing at it are repaired by `seed_defaults` via
        // `is_stale_builtin_asset_url`.
        "impresspress-logo-2x.png" => include_bytes!("assets/impresspress-logo-2x.png"),
        // Impresspress favicon — bundled so every deployment ships its own
        // `<link rel="icon">` target without depending on a per-deployment
        // external URL or the implicit browser fallback to `/favicon.ico`
        // (which 404s by default).
        "favicon.ico" => include_bytes!("assets/favicon.ico"),
        _ => return None,
    })
}

/// The built-in brand accent, as a hex literal for surfaces that can't use
/// CSS variables (email inline styles). Must match `--primary-color` in
/// `styles/tokens.css` — the `brand_accent_matches_tokens_css` test pins the
/// two together so they can't drift.
///
/// Kept at this branch's own value (not origin/main's `#f0480f`) — this is
/// the redesign's chosen accent, already wired through `tokens.css` and the
/// value-based contrast guard (`text_or_background_in_primary_danger_family_
/// meets_wcag_aa`); main's value was mid-fix on a documented WCAG-AA
/// shortfall this branch already resolved differently.
pub const BRAND_ACCENT_HEX: &str = "#fd3534";

/// Square logo URL with content hash, e.g. `/b/static/impresspress-logo-a1b2c3d4.png`.
pub fn logo_icon_url() -> String {
    url("impresspress-logo.png")
}

/// 2x square logo URL with content hash, e.g. `/b/static/impresspress-logo-2x-a1b2c3d4.png`.
/// Retina candidate for [`templates::brand_icon`]'s `<picture>` `srcset` —
/// one device pixel per art-pixel instead of a nearest-neighbour blow-up of
/// the 32-cell mark.
pub fn logo_icon_2x_url() -> String {
    url("impresspress-logo-2x.png")
}

/// Favicon URL with content hash, e.g. `/b/static/favicon-a1b2c3d4.ico`.
pub fn favicon_url() -> String {
    url("favicon.ico")
}

/// True when `value` points at this deployment's own `/b/static/` route but
/// names a file this build does not serve.
///
/// Every built-in asset URL carries a content hash, so changing the artwork
/// changes the URL. Those URLs are also *seeded into the database* as config
/// defaults (`LOGO_ICON_URL`, `FAVICON_URL`, historically `LOGO_URL`), which
/// means an upgrade that touches an asset leaves every existing deployment
/// pointing at a hash that 404s — a broken image on every page showing the
/// brand. `seed_defaults` uses this to repair those rows back to the current
/// default.
///
/// The manifest is the oracle rather than a per-asset prefix list: a URL
/// under our own static route naming a file we do not serve is dead by
/// definition, whatever asset it once referred to. Scoped to `STATIC_PREFIX`,
/// so an operator's white-labelled URL — any absolute URL, or a CDN base,
/// which is version-pinned and therefore never goes stale — is never touched.
pub fn is_stale_builtin_asset_url(value: &str) -> bool {
    match value.strip_prefix(STATIC_PREFIX) {
        Some(filename) => !filename.is_empty() && !ASSETS.iter().any(|e| e.filename == filename),
        None => false,
    }
}

/// The assembled CSS bundle, built by `build.rs` from `CSS_ORDER`.
///
/// Gated on `embed-assets` like [`bytes`]: without it, this `include_str!`
/// would still compile the ~75 KB bundle into every binary that links this
/// crate (including a lean Cloudflare Worker), relying on the linker to
/// notice nothing calls it and garbage-collect it -- "very likely" GC'd is
/// exactly the gap a `#[cfg]` closes outright. See `bytes`'s doc for why
/// `embed-assets` is the feature that governs asset bytes at all.
#[cfg(feature = "embed-assets")]
pub fn css() -> &'static str {
    include_str!(concat!(env!("OUT_DIR"), "/app.css"))
}

/// CSS URL with content hash, e.g. `/b/static/app-a1b2c3d4.css`
pub fn css_url() -> String {
    url("app.css")
}

/// htmx JS URL with content hash, e.g. `/b/static/htmx-a1b2c3d4.min.js`
pub fn htmx_js_url() -> String {
    url("htmx.min.js")
}

/// WebMCP script URL with content hash, e.g. `/b/static/webmcp-a1b2c3d4.js`
pub fn webmcp_js_url() -> String {
    url("webmcp.js")
}

/// The composed WebMCP tool-registration script, served on every page.
///
/// Fetches the auth-filtered manifest at `/b/webmcp/manifest.json` and
/// registers each tool via `document.modelContext.registerTool` (no-ops on
/// browsers without WebMCP support). On a service-worker build the first
/// fetch waits for the worker to take control of the page (see
/// `assets/webmcp.js`); `window.__impresspressWebmcp.refresh()` re-fetches
/// the manifest and swaps out whatever this script previously registered.
///
/// `build.rs` composes it from `assets/webmcp-core.js` (the shared
/// `buildRequest`/`toolOptions` fragment) and `assets/webmcp.js` (the tail),
/// so this is one `include_str!` of a finished artifact rather than a runtime
/// `format!` — the same shape `css()` has, and for the same reason: the
/// manifest hash has to describe the bytes that are actually served, and
/// without `embed-assets` there are no runtime bytes to compose from.
///
/// UNGATED, unlike every other asset's bytes. This one is the deployment's
/// agent entry point: [`WEBMCP_JS_STABLE_PATH`] promises a URL that never
/// moves and always resolves, and a page that hardcodes it is by definition
/// one this pipeline does not render and cannot inject anything into. Making
/// those bytes conditional would make the promise conditional too — the
/// browser bundle and the sandbox's own generated shop both load exactly that
/// path, and a detached build would leave them with a script that never
/// arrives. Roughly 10 KB, against the ~326 KB the detachable set saves; the
/// content-hashed `/b/static/` copy stays detachable for rendered pages.
pub fn webmcp_js() -> &'static str {
    include_str!(concat!(env!("OUT_DIR"), "/webmcp.js"))
}

/// Short content hash of the composed WebMCP script.
///
/// Read straight off the manifest rather than hashed here, so the `ETag` at
/// [`WEBMCP_JS_STABLE_PATH`] and the hash embedded in [`webmcp_js_url`] are
/// the same string by construction and cannot drift. Ungated on purpose:
/// identity is manifest knowledge, so it still answers in a build that
/// carries no asset bytes at all.
pub fn webmcp_js_hash() -> &'static str {
    entry("webmcp.js").hash
}

/// Stable (non-content-hashed) path for the WebMCP script, served by
/// `pipeline.rs` at `GET /b/webmcp/webmcp.js` — right beside
/// `/b/webmcp/manifest.json`.
///
/// This is the path anything that is NOT server-rendered by this pipeline
/// hardcodes in a `<script>` tag: a page under `site/` (served by
/// `wafer-run/web`), an agent-written page, a user-built block. Those never
/// get `ui::layout`'s injection and have no way to discover the current
/// content hash, so they need one URL that never moves between deploys.
///
/// It resolves in every build configuration, by serving [`webmcp_js`]'s bytes
/// directly. A redirect to the content-hashed URL was tried instead, to keep
/// the bytes detachable, and it does not hold: the browser bundle serves this
/// pipeline's responses from inside a service worker, where a cross-route
/// redirect is one more thing that has to work before an agent's page gets
/// any tools at all. A path whose entire purpose is "hardcode me" must not
/// depend on the asset base being reachable.
pub const WEBMCP_JS_STABLE_PATH: &str = "/b/webmcp/webmcp.js";

/// The shared `buildRequest`/`toolOptions` fragment.
///
/// A fragment, not a script: no IIFE, no `'use strict'`. The classic script
/// is composed from it by `build.rs`; this runtime copy exists only for the
/// sandbox's module variant below, which cannot be a manifest asset because
/// it is served from the block's own `/b/dev/static/` tier. Gated with its
/// only caller so a build without the sandbox does not carry it.
#[cfg(feature = "block-dev")]
const WEBMCP_CORE: &str = include_str!("assets/webmcp-core.js");

/// Compose the shared core and a `tail` into an ES **module**.
///
/// `imports` is emitted verbatim ahead of the IIFE — the only place an
/// `import` declaration may stand, since it must be at a module's top level.
/// Everything after it is byte-identical to what `build.rs` produces for the
/// classic script: the same IIFE, the same `'use strict'`, the same core, the
/// same tail. The tail is therefore written once and reads the same whichever
/// wrapper it goes through; what changes is only that the bindings `imports`
/// introduces are in scope inside the closure.
///
/// Runtime rather than build-time because its caller's assets are block-local
/// by design (`blocks::dev::assets`): they are served from `/b/dev/static/*`
/// at the block's own `Admin` tier, never from the public content-hashed
/// manifest, and a build without `block-dev` must not carry them at all.
#[cfg(feature = "block-dev")]
pub(crate) fn compose_webmcp_module(imports: &'static str, tail: &'static str) -> String {
    format!("{imports}\n(function () {{\n  'use strict';\n{WEBMCP_CORE}\n{tail}\n}})();\n")
}

/// Short content hash (first 8 chars of hex SHA-256).
///
/// `pub(crate)` for `blocks::dev::assets`, which needs an `ETag` for its
/// block-local, stable-path assets and so has nothing in the manifest to read
/// a hash from. Manifest assets must NOT use this — their identity is
/// [`AssetEntry::hash`], fixed at build time. Same projection `build.rs`
/// applies, so a hash means the same thing on both sides of the build.
///
/// Gated with that caller: no manifest asset may use it, so a build without
/// the sandbox has nothing left to hash at runtime.
#[cfg(feature = "block-dev")]
pub(crate) fn short_hash(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(content);
    hash.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// The shared chrome's browser behaviour: command palette, mobile drawer,
/// toasts and modals, as one hashed `/b/static/chrome-{hash}.js`.
///
/// Was four Rust raw strings — `palette_js`, `drawer_js`, `toast_js`,
/// `modal_js` — inlined into the bottom of every rendered page, 196 lines
/// re-sent uncached on every request. They are now one file, concatenated in
/// exactly the order the page emitted them, loaded once and cached forever
/// (the filename carries a content hash, so a changed script is a changed
/// URL). See `assets/chrome.js`'s own header for the section order and for
/// why two of the four sections are deliberately not wrapped in an IIFE.
///
/// Gated on `embed-assets` like [`css`], and for the same reason: without it
/// these bytes are served from R2 or a CDN and must not be linked into the
/// binary at all.
#[cfg(feature = "embed-assets")]
pub fn chrome_js() -> &'static str {
    include_str!("assets/chrome.js")
}

/// Chrome JS URL with content hash, e.g. `/b/static/chrome-a1b2c3d4.js`.
pub fn chrome_js_url() -> String {
    url("chrome.js")
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "embed-assets")]
    #[test]
    fn toast_messages_are_rendered_as_text_not_html() {
        let js = super::chrome_js();
        assert!(
            !js.contains("innerHTML"),
            "toast content must not use an HTML sink"
        );
        assert!(js.contains("message.textContent"));
        assert!(js.contains("createElement(\"button\")"));
        assert!(js.contains("addEventListener(\"click\""));
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn brand_accent_matches_tokens_css() {
        // BRAND_ACCENT_HEX exists for CSS-var-less surfaces (emails). It must
        // stay byte-identical to the stylesheet's --primary-color default.
        assert!(
            super::css().contains(&format!("--primary-color: {}", super::BRAND_ACCENT_HEX)),
            "BRAND_ACCENT_HEX ({}) does not match --primary-color in tokens.css",
            super::BRAND_ACCENT_HEX
        );
    }

    /// PNG width/height from the IHDR chunk (big-endian u32 at bytes 16/20).
    #[cfg(feature = "embed-assets")]
    fn png_size(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[1..4], b"PNG", "not a PNG");
        let be = |i: usize| u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]);
        (be(16), be(20))
    }

    // The brand art is real pixel art (generated in the `site` repo's
    // `brand/` kit) and the templates scale it only by whole factors, so
    // the native sizes are part of the contract. Ported from origin/main
    // during the main merge -- 2026-09-02 -- adapted to call `bytes()`
    // (the manifest's single embed point) rather than main's dedicated
    // per-asset accessors, which this branch's detachable-assets refactor
    // superseded.
    #[cfg(feature = "embed-assets")]
    #[test]
    fn logo_icon_png_is_the_32_cell_mark() {
        assert_eq!(
            png_size(super::bytes("impresspress-logo.png").unwrap()),
            (32, 32)
        );
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn logo_icon_2x_png_is_the_64_cell_mark() {
        assert_eq!(
            png_size(super::bytes("impresspress-logo-2x.png").unwrap()),
            (64, 64)
        );
    }

    #[test]
    fn logo_icon_2x_url_has_content_hash() {
        let url = super::logo_icon_2x_url();
        assert!(url.starts_with("/b/static/impresspress-logo-2x-"), "{url}");
        assert!(url.ends_with(".png"));
        let hash = url
            .trim_start_matches("/b/static/impresspress-logo-2x-")
            .trim_end_matches(".png");
        assert_eq!(hash.len(), 8, "expected 8-char short hash, got: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn favicon_ico_frames_are_16_32_48_at_native_size() {
        let ico = super::bytes("favicon.ico").unwrap();
        assert_eq!(u16::from_le_bytes([ico[2], ico[3]]), 1, "ICO type");
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        let sizes: Vec<u8> = (0..count).map(|i| ico[6 + i * 16]).collect();
        assert_eq!(sizes, vec![16, 32, 48]);
        for (i, &size) in sizes.iter().enumerate() {
            let e = 6 + i * 16;
            let len =
                u32::from_le_bytes([ico[e + 8], ico[e + 9], ico[e + 10], ico[e + 11]]) as usize;
            let off =
                u32::from_le_bytes([ico[e + 12], ico[e + 13], ico[e + 14], ico[e + 15]]) as usize;
            let frame = &ico[off..off + len];
            assert_eq!(
                png_size(frame),
                (u32::from(size), u32::from(size)),
                "frame {i} is a 1:1 PNG"
            );
        }
    }

    #[test]
    fn stale_builtin_asset_url_matches_only_dead_local_assets() {
        // The URL this build actually serves is current, not stale.
        assert!(!super::is_stale_builtin_asset_url(&super::logo_icon_url()));
        assert!(!super::is_stale_builtin_asset_url(&super::favicon_url()));
        // A prior release's content hash under our own route is dead.
        assert!(super::is_stale_builtin_asset_url(
            "/b/static/impresspress-logo-5e884a3a.png"
        ));
        // The removed raster wordmark: route gone entirely.
        assert!(super::is_stale_builtin_asset_url(
            "/b/static/impresspress-logo-long-1f4c8ab2.png"
        ));
        // An operator's white-labelled artwork is their data.
        assert!(!super::is_stale_builtin_asset_url(
            "https://acme.example/wordmark.png"
        ));
        // Blank (the "no wordmark" default) and the bare route are not URLs
        // at a missing file.
        assert!(!super::is_stale_builtin_asset_url(""));
        assert!(!super::is_stale_builtin_asset_url("/b/static/"));
    }

    #[test]
    fn favicon_url_has_content_hash() {
        let url = super::favicon_url();
        assert!(url.starts_with("/b/static/favicon-"));
        assert!(url.ends_with(".ico"));
        let hash = url
            .trim_start_matches("/b/static/favicon-")
            .trim_end_matches(".ico");
        assert_eq!(hash.len(), 8, "expected 8-char short hash, got: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn tokens_include_new_scale() {
        let s = super::css();
        for tok in [
            "--text-base",
            "--text-2xl",
            "--space-12",
            "--surface-1",
            "--primary-button",
            "--focus-ring",
        ] {
            assert!(s.contains(tok), "missing token: {tok}");
        }
    }

    /// Relative luminance per WCAG 2.1.
    #[cfg(feature = "embed-assets")]
    fn luminance(hex: &str) -> f64 {
        let h = hex.trim_start_matches('#');
        let ch = |i| {
            let c = u8::from_str_radix(&h[i..i + 2], 16).unwrap() as f64 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(0) + 0.7152 * ch(2) + 0.0722 * ch(4)
    }

    #[cfg(feature = "embed-assets")]
    fn contrast(a: &str, b: &str) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Every value here is read from the LIVE token map rather than written
    /// as a frozen hex literal. It used to spell all four out -- and drifted:
    /// it asserted on `#64748b` while calling it "--text-muted" long after
    /// that token had moved to `#5d6b7f`, so the assertion still passed while
    /// describing a colour the stylesheet no longer used. A test naming a
    /// token must resolve that token, or it is a snapshot of a past build
    /// wearing the token's name.
    #[cfg(feature = "embed-assets")]
    #[test]
    fn brand_tokens_meet_wcag_aa() {
        let tokens = parse_root_tokens(super::css());
        let hex = |name: &str| -> String {
            let raw = tokens
                .get(name)
                .unwrap_or_else(|| panic!("token {name} missing from :root"));
            let rgba = resolve_color(raw, &tokens, 0)
                .unwrap_or_else(|| panic!("token {name} ({raw}) did not resolve to a colour"));
            hex_of(composite_over(rgba, (255, 255, 255)))
        };

        let primary_button = hex("--primary-button");
        let sidebar_bg = hex("--bg-sidebar");
        let sidebar_muted = hex("--sidebar-text-muted");
        let text_muted = hex("--text-muted");

        // White-on-red button surfaces carry normal-size text: 4.5:1 required.
        assert!(
            contrast(&primary_button, "#ffffff") >= 4.5,
            "--primary-button ({primary_button}) fails AA under white text: {:.2}:1",
            contrast(&primary_button, "#ffffff")
        );
        // Sidebar foregrounds on the navy slab.
        assert!(
            contrast("#ffffff", &sidebar_bg) >= 4.5,
            "white on --bg-sidebar ({sidebar_bg}) fails AA"
        );
        assert!(
            contrast(&sidebar_muted, &sidebar_bg) >= 4.5,
            "--sidebar-text-muted ({sidebar_muted}) on --bg-sidebar ({sidebar_bg}) fails AA: {:.2}:1",
            contrast(&sidebar_muted, &sidebar_bg)
        );
        // The regression this guards: the page-level muted token is too dark
        // for the navy slab (3.47:1 as of --text-muted #5d6b7f), which is why
        // --sidebar-text-muted exists as a separate, lighter token. If this
        // ever passes, the two have converged and one of them is redundant.
        assert!(
            contrast(&text_muted, &sidebar_bg) < 4.5,
            "--text-muted ({text_muted}) now clears AA on --bg-sidebar ({sidebar_bg}) -- \
             the separate --sidebar-text-muted token may be redundant"
        );
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn tokens_css_declares_the_new_palette() {
        let s = super::css();
        for (name, value) in [
            ("--primary-color", "#fd3534"),
            ("--primary-button", "#d92320"),
            ("--primary-hover", "#e02523"),
            ("--navy-900", "#02112a"),
            ("--navy-800", "#0a1122"),
            ("--navy-700", "#172136"),
            ("--sidebar-text-muted", "#94a3b8"),
        ] {
            assert!(
                s.contains(&format!("{name}: {value}")),
                "missing {name}: {value}"
            );
        }
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn ui_font_stack_is_system_and_itim_is_wordmark_only() {
        let s = super::css();
        assert!(
            s.contains("--font-ui: system-ui"),
            "UI face must be the system stack"
        );
        assert!(
            s.contains(".brand__wordmark"),
            "Itim must be scoped to the wordmark"
        );
        // The body must not name Itim any more.
        let body_rule = s
            .split("body {")
            .nth(1)
            .expect("body rule")
            .split('}')
            .next()
            .unwrap();
        assert!(
            !body_rule.contains("Itim"),
            "Itim must not be the body face"
        );
    }

    /// Strips every `/* ... */` block comment from `s`. Shared by
    /// `css_leaf_blocks` and `parse_root_tokens` below: a comment sitting
    /// between two declarations (nothing but the comment separates it from
    /// the declaration after it -- no `;`/`{`/`}` to split on) otherwise
    /// gets swallowed into that following declaration's segment when a
    /// caller splits on `;`, silently hiding the declaration. (Found
    /// auditing `.login-button` for Task 15: its `background:
    /// var(--primary-button)` sat right after such a comment and was
    /// invisible to `body.split(';').find(|d| d.starts_with("background:"))`
    /// as a result -- not a bug in that rule, a latent bug in this shared
    /// parsing step.) Also needed because a comment containing literal
    /// `{`/`}` characters in prose (several exist in this bundle, e.g.
    /// base.css's "`hidden` attribute" comment) would otherwise corrupt
    /// `css_leaf_blocks`'s brace-depth scan.
    #[cfg(feature = "embed-assets")]
    fn strip_css_comments(s: &str) -> String {
        let mut without_comments = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(start) = rest.find("/*") {
            without_comments.push_str(&rest[..start]);
            rest = match rest[start + 2..].find("*/") {
                Some(end) => &rest[start + 2 + end + 2..],
                None => "",
            };
        }
        without_comments.push_str(rest);
        without_comments
    }

    /// Finds every leaf declaration block (`selector { decl; decl; }`) in a
    /// CSS bundle, including ones nested inside `@media` (comments stripped
    /// first via `strip_css_comments` above). A block whose body still
    /// contains `{` after being popped off the brace stack is a container
    /// (e.g. the `@media` wrapper itself) and is skipped -- its children
    /// are captured on their own pop.
    #[cfg(feature = "embed-assets")]
    fn css_leaf_blocks(s: &str) -> Vec<(String, String)> {
        let without_comments = strip_css_comments(s);
        let s = without_comments.as_str();

        let chars: Vec<char> = s.chars().collect();
        let mut stack: Vec<usize> = Vec::new();
        let mut blocks = Vec::new();
        for (i, c) in chars.iter().enumerate() {
            match c {
                '{' => stack.push(i),
                '}' => {
                    let Some(open) = stack.pop() else { continue };
                    let body: String = chars[open + 1..i].iter().collect();
                    if body.contains('{') {
                        continue; // container, not a leaf -- e.g. @media
                    }
                    let prefix: String = chars[..open].iter().collect();
                    let selector = prefix.rsplit(['{', '}']).next().unwrap_or(&prefix).trim();
                    blocks.push((selector.to_string(), body));
                }
                _ => {}
            }
        }
        blocks
    }

    // ===== Value-based contrast resolution (Task 15 follow-up) =====
    //
    // Two guards touched this exact bug and each missed it from a
    // different angle: `brand_tokens_meet_wcag_aa` asserted token VALUES in
    // isolation, never checking which rules actually paired them (missed
    // every primary button failing AA, Task 7). Its replacement here
    // originally matched `background: var(--primary-color)` + `color:
    // white` (Task 12a), then grew a second assertion for the reverse
    // `color: var(--primary-color)` direction (Task 15) -- but both worked
    // by matching a token's literal `var(--name)` spelling in the CSS
    // source text. That is structurally a blocklist: `--accent-info:
    // #fd3534` was a byte-identical alias of `--primary-color` under a
    // different name, and evaded it completely, as did every bare hex
    // literal (`#ef4444` in `.form-error`) equal to a tracked token's
    // value. The next alias would have evaded it again.
    //
    // The functions below resolve a CSS color expression -- a token
    // reference (`var(--x)`), a `var(--x, fallback)` with its fallback, a
    // `color-mix(in srgb, c1 p1%, c2 p2%)`, or a literal hex/`white`/
    // `black`/`transparent` -- to actual RGBA by walking `tokens.css`'s
    // live `:root` values, so the single test below computes real WCAG
    // contrast instead of matching names.
    #[cfg(feature = "embed-assets")]
    type Rgba = (u8, u8, u8, u8);

    /// Parses the assembled bundle's `:root { ... }` custom-property
    /// declarations into a `name -> raw value` map, e.g. `"--primary-color"
    /// -> "#fd3534"`. `styles/tokens.css` is first in build.rs's
    /// `CSS_FILES` order and the only file with a `:root` block.
    #[cfg(feature = "embed-assets")]
    fn parse_root_tokens(s: &str) -> std::collections::HashMap<String, String> {
        let without_comments = strip_css_comments(s);
        let s = without_comments.as_str();
        let start = s.find(":root").expect(":root block missing from bundle");
        let open = s[start..].find('{').expect(":root has no body") + start;
        let mut depth = 0i32;
        let mut end = open;
        for (i, c) in s[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &s[open + 1..end];
        let mut map = std::collections::HashMap::new();
        for decl in body.split(';') {
            let decl = decl.trim();
            if let Some(rest) = decl.strip_prefix("--") {
                if let Some((name, val)) = rest.split_once(':') {
                    map.insert(format!("--{}", name.trim()), val.trim().to_string());
                }
            }
        }
        map
    }

    /// Splits a function-argument string on top-level commas (i.e. not
    /// inside a nested `(...)`) -- needed because both a `var(--x,
    /// var(--y))` fallback and a `color-mix(in srgb, c1 p1%, c2 p2%)`
    /// argument list can contain commas one level deeper than the ones
    /// that actually separate arguments.
    #[cfg(feature = "embed-assets")]
    fn split_top_level(s: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        let mut depth = 0i32;
        let mut start = 0usize;
        for (i, c) in s.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(s[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        parts.push(s[start..].trim());
        parts
    }

    #[cfg(feature = "embed-assets")]
    fn hex_byte(s: &str) -> Option<u8> {
        u8::from_str_radix(s, 16).ok()
    }

    /// Parses a `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa` literal.
    #[cfg(feature = "embed-assets")]
    fn parse_hex(h: &str) -> Option<Rgba> {
        let h = h.trim_start_matches('#');
        let double = |c: char| -> Option<u8> { hex_byte(&format!("{c}{c}")) };
        match h.len() {
            3 => {
                let mut cs = h.chars();
                Some((
                    double(cs.next()?)?,
                    double(cs.next()?)?,
                    double(cs.next()?)?,
                    255,
                ))
            }
            4 => {
                let mut cs = h.chars();
                Some((
                    double(cs.next()?)?,
                    double(cs.next()?)?,
                    double(cs.next()?)?,
                    double(cs.next()?)?,
                ))
            }
            6 => Some((
                hex_byte(&h[0..2])?,
                hex_byte(&h[2..4])?,
                hex_byte(&h[4..6])?,
                255,
            )),
            8 => Some((
                hex_byte(&h[0..2])?,
                hex_byte(&h[2..4])?,
                hex_byte(&h[4..6])?,
                hex_byte(&h[6..8])?,
            )),
            _ => None,
        }
    }

    /// Resolves a CSS color expression to RGBA, following `var()` chains
    /// (including a `var(--x, fallback)`'s fallback when `--x` isn't
    /// declared) and `color-mix(in srgb, c1 [p1%], c2 [p2%])`. Returns
    /// `None` for anything else this test doesn't need to understand --
    /// `currentColor`, `inherit`, gradients, `rgb()`/`rgba()` (none of
    /// which appear on a `color:`/`background:` declaration anywhere in
    /// this bundle today, checked by grep while writing this) -- callers
    /// treat `None` as "can't verify this rule" and skip it rather than
    /// assuming compliance.
    #[cfg(feature = "embed-assets")]
    fn resolve_color(
        value: &str,
        tokens: &std::collections::HashMap<String, String>,
        depth: u8,
    ) -> Option<Rgba> {
        if depth > 12 {
            return None;
        }
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        let lower = value.to_ascii_lowercase();
        match lower.as_str() {
            "white" => return Some((255, 255, 255, 255)),
            "black" => return Some((0, 0, 0, 255)),
            "transparent" => return Some((0, 0, 0, 0)),
            _ => {}
        }
        if let Some(hex) = value.strip_prefix('#') {
            return parse_hex(hex);
        }
        if lower.starts_with("var(") {
            let open = value.find('(')?;
            let close = value.rfind(')')?;
            let parts = split_top_level(&value[open + 1..close]);
            let name = parts.first()?.trim();
            if let Some(v) = tokens.get(name) {
                return resolve_color(v, tokens, depth + 1);
            }
            if parts.len() > 1 {
                return resolve_color(parts[1], tokens, depth + 1);
            }
            return None;
        }
        if lower.starts_with("color-mix(") {
            let open = value.find('(')?;
            let close = value.rfind(')')?;
            let parts = split_top_level(&value[open + 1..close]);
            if parts.len() < 3 {
                return None;
            }
            let split_pct = |p: &str| -> (String, Option<f64>) {
                match p.rsplit_once(' ') {
                    Some((color, pct)) if pct.ends_with('%') => {
                        match pct.trim_end_matches('%').parse::<f64>() {
                            Ok(v) => (color.trim().to_string(), Some(v)),
                            Err(_) => (p.to_string(), None),
                        }
                    }
                    _ => (p.to_string(), None),
                }
            };
            let (c1, p1) = split_pct(parts[1]);
            let (c2, p2) = split_pct(parts[2]);
            let (w1, w2) = match (p1, p2) {
                (None, None) => (50.0, 50.0),
                (Some(a), None) => (a, 100.0 - a),
                (None, Some(b)) => (100.0 - b, b),
                (Some(a), Some(b)) => (a, b),
            };
            let rgba1 = resolve_color(&c1, tokens, depth + 1)?;
            let rgba2 = resolve_color(&c2, tokens, depth + 1)?;
            let mix = |a: u8, b: u8| -> u8 {
                ((a as f64 * w1 / 100.0) + (b as f64 * w2 / 100.0)).round() as u8
            };
            return Some((
                mix(rgba1.0, rgba2.0),
                mix(rgba1.1, rgba2.1),
                mix(rgba1.2, rgba2.2),
                mix(rgba1.3, rgba2.3),
            ));
        }
        None
    }

    /// Alpha-composites `fg` over an opaque `base` -- e.g. a translucent
    /// tint like the old `--accent-info-bg`'s `#fd353419` over the page's
    /// white surface.
    #[cfg(feature = "embed-assets")]
    fn composite_over(fg: Rgba, base: (u8, u8, u8)) -> (u8, u8, u8) {
        let (r, g, b, a) = fg;
        if a == 255 {
            return (r, g, b);
        }
        if a == 0 {
            return base;
        }
        let af = a as f64 / 255.0;
        let mix = |f: u8, b: u8| -> u8 { (f as f64 * af + b as f64 * (1.0 - af)).round() as u8 };
        (mix(r, base.0), mix(g, base.1), mix(b, base.2))
    }

    #[cfg(feature = "embed-assets")]
    fn hex_of(rgb: (u8, u8, u8)) -> String {
        format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
    }

    /// Every `var(--token)` referenced anywhere in the bundle must resolve to
    /// a declaration in `:root`.
    ///
    /// An unresolvable `var()` does not fall back to something sensible -- the
    /// whole declaration becomes invalid at computed-value time and the
    /// property reverts to whatever won before it, which in this bundle is the
    /// `*, *::before, *::after { margin: 0; padding: 0 }` reset in base.css.
    /// The element silently renders with NO padding rather than the wrong
    /// padding, and nothing in the build or the test suite notices.
    ///
    /// This is not hypothetical. `--space-1` went missing from `:root` because
    /// a prose comment in tokens.css contained the two characters that close a
    /// CSS comment, mid-word, in the token name `--spacing-` + star + slash.
    /// CSS comments do not nest and have no escape, so the comment ended
    /// there; the parser read the rest of the sentence as a declaration and,
    /// recovering, discarded everything up to the next semicolon -- the end of
    /// `--space-1: 0.25rem;`. All 55 `var(--space-1)` uses then fell back to
    /// the reset, so `.btn--sm` (`padding: var(--space-1) var(--space-3)`) and
    /// `.topbar__palette` rendered with zero padding: the "Open" button on the
    /// Blocks page was 15px tall with its label touching the pill edge.
    ///
    /// Only fallback-less references count. `var(--border, #e2e8f0)` renders
    /// its fallback and is fine (though this bundle has a few of those whose
    /// fallback is byte-identical to an existing token, which is its own small
    /// smell -- they were replaced with the token itself).
    ///
    /// The failure mode is what makes this worth a guard: a missing token is
    /// invisible in the source (the declaration is right there in tokens.css),
    /// invisible to the CSS bundler (it is valid CSS -- just not the CSS
    /// anyone wrote), and invisible to every screenshot test that has not been
    /// re-baselined. Only resolving references against definitions catches it.
    #[cfg(feature = "embed-assets")]
    #[test]
    fn every_referenced_custom_property_is_defined_in_root() {
        let s = super::css();
        let tokens = parse_root_tokens(s);

        // Only references WITHOUT a fallback can break. `var(--x, #fff)` is
        // valid whether or not `--x` exists -- the fallback renders -- so it is
        // not a defect, just an undefined name. `var(--x)` with no fallback is
        // the one that drops its whole declaration.
        let mut referenced: std::collections::BTreeSet<String> = Default::default();
        let mut rest = s;
        while let Some(i) = rest.find("var(--") {
            rest = &rest[i + "var(".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            let after = rest[name.len()..].trim_start();
            if after.starts_with(',') {
                continue; // has a fallback -- renders fine
            }
            referenced.insert(name);
        }

        // Tokens a caller may legitimately supply per-deployment or per-render,
        // which therefore have no `:root` default. Each is set from Rust via an
        // inline custom property on the element that consumes it.
        const CALLER_SUPPLIED: &[&str] = &[
            // ui/components/table.rs sets this per-column on the <th>.
            "--col-width",
            // ui/components/chart.rs sets these per-datapoint: the line
            // chart's endpoint-dot offset, each y-axis label's offset and
            // each bar's height fraction.
            "--dot-y",
            "--tick-y",
            "--size",
            // ui/components/chart.rs sets the series colour on the wrapper.
            "--chart-color",
            // blocks/files/pages_user/cloudstorage.rs sets the quota bar width.
            "--fill-pct",
            // ui/templates.rs lets a deployment theme its public pages.
            "--public-page-bg",
        ];

        let missing: Vec<&String> = referenced
            .iter()
            .filter(|n| !tokens.contains_key(n.as_str()))
            .filter(|n| !CALLER_SUPPLIED.contains(&n.as_str()))
            .collect();

        assert!(
            missing.is_empty(),
            "these custom properties are referenced by the stylesheet but never \
             defined in :root, so every declaration using them is dropped and \
             the property falls back to the `*` reset (usually to 0): {missing:?}. \
             If one is deliberately supplied by a caller at render time, add it \
             to CALLER_SUPPLIED with a note saying who sets it."
        );

        // Sanity floor: the extractor must actually be finding references. If a
        // refactor changed `var()` spelling, the assertion above would pass
        // vacuously on an empty set.
        assert!(
            referenced.len() > 40,
            "only {} var() references found -- the extractor has probably \
             stopped matching, making the check above vacuous",
            referenced.len()
        );
    }

    /// Selectors this contrast guard does not hold to the 4.5:1 text floor,
    /// each with its own reason -- not a silent pass.
    #[cfg(feature = "embed-assets")]
    const CONTRAST_EXEMPT_SELECTORS: &[&str] = &[
        // `.db-table-group__icon` wraps `icons::package()`/`icons::database()`
        // (database.rs) -- an SVG icon, not text; `color` only feeds the
        // SVG's `currentColor`. WCAG's 3:1 non-text floor applies, and
        // #fd3534 (3.66:1 on white) already clears it.
        ".db-table-group__icon",
        // Disabled form controls. WCAG 2.x SC 1.4.3 explicitly exempts text
        // in "inactive user interface components" from the contrast minimum,
        // and the muted look is what communicates the disabled state. 4.39:1
        // (computed) -- deliberately just under, not an oversight.
        ".form-input:disabled",
        ".form-select:disabled",
        ".form-textarea:disabled",
    ];

    /// Text whose background comes from an ANCESTOR rule rather than its own.
    /// A single-rule scan cannot see the cascade, so without this table these
    /// selectors would either be skipped (unverified) or measured against a
    /// wrong assumed-white background and reported as false failures. Mapping
    /// each to the token its true ancestor actually sets means they get
    /// genuinely checked instead of waved through -- the navy panels are the
    /// only place in the bundle where text sits on a non-white surface set by
    /// a parent.
    #[cfg(feature = "embed-assets")]
    const ANCESTOR_BACKGROUNDS: &[(&str, &str)] = &[
        // `.sidebar`'s navy slab is painted by `.sidebar__nav` in
        // components/nav.css (`background: var(--bg-sidebar)`); every
        // `.sidebar__*` label, link and avatar caption renders on it.
        (".sidebar__", "--bg-sidebar"),
        // `.auth-split__brand` sets `background: var(--navy-900)` for the
        // login page's left-hand brand panel (layouts/auth-split.css).
        (".auth-split__", "--navy-900"),
    ];

    /// Asserts every text/background pair the CSS bundle declares meets the
    /// 4.5:1 WCAG AA floor for normal text, matching on RESOLVED COLOR VALUES
    /// rather than token names.
    ///
    /// Value-matching is what closes the alias hole that let
    /// `--accent-info: #fd3534` -- and bare literals like `#ef4444` --
    /// evade the two earlier name-matching guards (Tasks 12a and 15): a new
    /// token, or a re-literalized hex, cannot rename its way past this.
    ///
    /// This guard used to be scoped to the primary/danger (brand-red) token
    /// family only. That scoping is why it never saw the success and warning
    /// families: `--accent-success` (#10b981) as text was 2.54:1 on white and
    /// 2.31:1 on its own tint, and `.badge-warning` was 1.99:1 -- the worst
    /// pair in the bundle -- both structurally invisible to a family-filtered
    /// check, and both shipped. The narrow scope was a deliberate, documented
    /// deferral of "a separate, larger, unbudgeted audit"; that audit has now
    /// been done (every failing pair fixed, `--accent-success-text` added to
    /// match the `-text` siblings danger and warning already had, and
    /// `--text-secondary`/`--text-muted` given tint headroom), so the filter
    /// is gone and this now evaluates EVERY rule with a resolvable text color.
    /// Scoping a guard to the family whose bug prompted it is exactly how the
    /// next family's copy of that bug survives.
    ///
    /// Background resolution, in order: the rule's own `background`/
    /// `background-color`; else an `ANCESTOR_BACKGROUNDS` entry; else the
    /// page surface (white). Translucent tints are alpha-composited over the
    /// resolved background before measuring.
    ///
    /// What this still cannot see: a parent background paired with child text
    /// outside the `ANCESTOR_BACKGROUNDS` table; colors applied via inline
    /// `style` or JS; and values expressed through CSS functions the resolver
    /// does not parse. Such a rule is SKIPPED rather than assumed compliant --
    /// so the count below is asserted too, to catch a future refactor that
    /// silently drops rules out of coverage by making them unresolvable.
    // Depends on `css()`, so it needs the same `embed-assets` gate as every
    // other CSS-content test in this module -- a no-embed build has no CSS
    // bytes to check contrast on.
    #[cfg(feature = "embed-assets")]
    #[test]
    fn text_and_background_pairs_meet_wcag_aa() {
        let s = super::css();
        let tokens = parse_root_tokens(s);

        let exempt = |selector: &str| -> bool {
            selector
                .split(',')
                .map(str::trim)
                .any(|part| CONTRAST_EXEMPT_SELECTORS.contains(&part))
        };
        let ancestor_bg = |selector: &str| -> Option<Rgba> {
            ANCESTOR_BACKGROUNDS.iter().find_map(|(prefix, token)| {
                if selector
                    .split(',')
                    .map(str::trim)
                    .any(|part| part.starts_with(prefix))
                {
                    resolve_color(tokens.get(*token)?, &tokens, 0)
                } else {
                    None
                }
            })
        };

        let mut offenders: Vec<String> = Vec::new();
        let mut checked = 0usize;
        for (selector, body) in css_leaf_blocks(s) {
            if exempt(&selector) {
                continue;
            }
            let decls: Vec<&str> = body
                .split(';')
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .collect();
            let Some(color_decl) = decls.iter().find(|d| d.starts_with("color:")).copied() else {
                continue;
            };
            let Some(text_rgba) = resolve_color(&color_decl["color:".len()..], &tokens, 0) else {
                continue; // unresolvable -- can't verify, skip (documented above)
            };
            if text_rgba.3 == 0 {
                continue; // fully transparent text, not visible
            }

            // The surface this rule's own background (if any) paints onto:
            // an ancestor's background where we know it, else the page.
            let base_rgb = composite_over(
                ancestor_bg(&selector).unwrap_or((255, 255, 255, 255)),
                (255, 255, 255),
            );
            let bg_decl: Option<&str> = decls
                .iter()
                .find(|d| d.starts_with("background:") || d.starts_with("background-color:"))
                .copied();
            // A declared background composites OVER that base rather than
            // replacing it, so `background: transparent` (and any translucent
            // tint) correctly shows the ancestor through it. Compositing
            // against white unconditionally instead would have reported
            // `.sidebar__collapse-toggle` -- transparent over the navy slab --
            // as 2.56:1 white-background text, a false failure.
            let bg_rgb = match bg_decl {
                Some(d) => {
                    let val = d.split_once(':').map(|x| x.1).unwrap_or_default();
                    match resolve_color(val, &tokens, 0) {
                        Some(c) => composite_over(c, base_rgb),
                        None => continue, // declared but unresolvable -- can't verify
                    }
                }
                None => base_rgb,
            };

            let text_rgb = composite_over(text_rgba, bg_rgb);
            let ratio = contrast(&hex_of(text_rgb), &hex_of(bg_rgb));
            checked += 1;
            if ratio < 4.5 {
                offenders.push(format!(
                    "{}: {ratio:.2}:1 ({} text on {} background)",
                    selector.split(',').next().unwrap_or(&selector).trim(),
                    hex_of(text_rgb),
                    hex_of(bg_rgb)
                ));
            }
        }

        assert!(
            offenders.is_empty(),
            "text/background pairs failing 4.5:1 AA (computed): {offenders:#?}"
        );
        // Coverage floor: the bundle currently resolves ~200 such pairs. If a
        // refactor makes colors unresolvable to this parser (moving them into
        // an unparsed CSS function, say), rules would drop out of coverage
        // silently and the assert above would pass vacuously. Deliberately
        // loose -- this catches a collapse, not normal drift.
        assert!(
            checked > 120,
            "only {checked} text/background pairs were resolvable -- expected >120; \
             the resolver has probably stopped understanding a common value form, \
             which would make the contrast assertion above vacuous"
        );
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn chrome_js_carries_the_palette_section() {
        let js = super::chrome_js();
        assert!(js.contains("cmdk"));
        assert!(js.contains("Meta+K") || js.contains("metaKey"));
        assert!(js.contains("(function "));
        // Idempotent guard: the section must survive being evaluated twice.
        assert!(js.contains("__cmdkInit"));
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn chrome_js_drawer_section_handles_open_close_esc_and_navlink() {
        let js = super::chrome_js();
        assert!(js.contains("'drawer-open'"));
        assert!(js.contains("'drawer-close'"));
        assert!(js.contains("'Escape'"));
        assert!(js.contains(".sidebar a"));
        assert!(js.contains("data-drawer-open"));
        // Self-invoking + idempotent guard.
        assert!(js.contains("__drawerInit"));
    }

    /// The inverse of the pin PR #46 shipped. That one required `openModal`
    /// and `closeModal` to be top-level declarations, because pages reached
    /// them from `onclick` attribute strings and an IIFE would have made them
    /// silently dead. Those attributes are `data-action="modal-open"` /
    /// `"modal-close"` now, read by the delegated listener in the same
    /// section, and the htmx response-header channel covers the rest — so the
    /// helpers are internal, and re-exposing them would be a global with no
    /// caller. This is the same coverage pointed the other way, not coverage
    /// dropped: what it guards is that the modal section still handles every
    /// way a modal is opened or closed.
    #[cfg(feature = "embed-assets")]
    #[test]
    fn chrome_js_owns_the_modal_verbs_without_exposing_globals() {
        let js = super::chrome_js();
        assert!(
            js.contains("if (window.__modalInit) return;"),
            "the modal section must be a guarded IIFE"
        );
        // Matched on the name alone, at column zero, rather than on one exact
        // spelling: `function openModal (id) {` and `function openModal(id){`
        // are the same global, and pinning a single spelling would let a
        // re-globalised helper back in on a whitespace change. The assignment
        // forms are covered too, because `window.openModal = …` is just as
        // global as a declaration.
        for name in ["openModal", "closeModal"] {
            let declaration = format!("function {name}");
            assert!(
                !js.lines().any(|l| l.starts_with(&declaration)),
                "{name} must not be a top-level (global) function declaration"
            );
            let assignment = format!("window.{name}");
            assert!(
                !js.contains(&assignment),
                "{name} must not be published on `window` either"
            );
        }
        for verb in [
            "\"modal-open\"",
            "\"modal-close\"",
            "\"reveal-toggle\"",
            "\"copy-text\"",
            "\"mirror-value\"",
        ] {
            assert!(js.contains(verb), "chrome must handle the {verb} verb");
        }
        for hook in [
            ".modal-overlay[data-modal-dismiss]",
            "data-stop-propagation",
            "data-submit-on-enter",
        ] {
            assert!(js.contains(hook), "chrome must handle {hook}");
        }
        // Both directions of the htmx response-header channel: `closeModal`
        // was already there, `openModal` replaced the four auto-show scripts.
        for event in ["\"closeModal\"", "\"openModal\""] {
            assert!(
                js.contains(&format!("document.body.addEventListener({event}")),
                "chrome must listen for the {event} htmx trigger"
            );
        }
    }

    /// One asset, one hash, one `<script src>`: the four raw-string
    /// accessors this replaced are gone, and nothing may reintroduce an
    /// inline chrome script under a new name.
    #[test]
    fn chrome_js_url_has_content_hash() {
        let url = super::chrome_js_url();
        assert!(url.starts_with("/b/static/chrome-"), "{url}");
        assert!(url.ends_with(".js"));
        let hash = url
            .trim_start_matches("/b/static/chrome-")
            .trim_end_matches(".js");
        assert_eq!(hash.len(), 8, "expected 8-char short hash, got: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The shared asset module must not know any block's assets. `bytes()`
    /// resolves a block-owned key only by falling through to the block that
    /// declares it (`blocks::static_asset_bytes`), which is what removed the
    /// four `#[cfg(feature = "block-…")]` arms this module used to carry.
    #[cfg(feature = "embed-assets")]
    #[test]
    fn the_shared_half_owns_no_block_asset() {
        for logical in [
            "marked.min.js",
            "purify.min.js",
            "llm-chat.js",
            "files-browser.js",
        ] {
            assert!(
                super::shared_bytes(logical).is_none(),
                "{logical} is a block's asset; the shared module must not embed it"
            );
        }
    }

    #[test]
    fn manifest_covers_core_assets_with_hashed_filenames() {
        let by_logical = |k: &str| super::ASSETS.iter().find(|e| e.logical == k);
        for key in ["app.css", "htmx.min.js", "favicon.ico", "itim-latin.woff2"] {
            let e = by_logical(key).unwrap_or_else(|| panic!("manifest missing {key}"));
            assert!(e.len > 0, "{key} has zero length");
            // `app.css` -> `app-a1b2c3d4.css`: stem, dash, 8 hex chars, extension.
            let stem = key.split_once('.').unwrap().0;
            let rest = e
                .filename
                .strip_prefix(stem)
                .expect("filename keeps its stem");
            let hash = &rest[1..9];
            assert_eq!(hash.len(), 8);
            assert!(
                hash.chars().all(|c| c.is_ascii_hexdigit()),
                "{key}: {hash} not hex"
            );
        }
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn css_bundle_includes_all_layers_in_order() {
        let s = super::css();
        // Tokens must precede every consumer so custom properties are defined
        // before use; layouts come last so they can override component defaults.
        // This guard is what keeps Task 5's split honest — it must pass both
        // before and after the files are reorganised.
        let tokens = s.find("--primary-color").expect("tokens layer missing");
        let button = s.find(".btn").expect("button layer missing");
        let shell = s.find(".shell").expect("shell layout missing");
        assert!(tokens < button, "tokens must precede components");
        assert!(button < shell, "components must precede layouts");
        for marker in [
            ".card",
            ".data-table",
            ".badge",
            ".modal",
            ".toast",
            ".palette",
            ".stat-",
            ".charts-css",
            ".auth-split",
        ] {
            assert!(s.contains(marker), "missing layer marker: {marker}");
        }
    }

    #[test]
    fn base_url_defaults_to_static_prefix() {
        // No IMPRESSPRESS_ASSET_BASE_URL in the test environment.
        assert_eq!(super::base_url(), "/b/static/");
    }

    #[test]
    fn set_base_url_override_is_idempotent() {
        // Exercises `BASE_URL_OVERRIDE` in isolation — never calls
        // `base_url()` itself, whose own `OnceLock` is claimed by
        // `base_url_defaults_to_static_prefix` and must not be touched by
        // any other test in this process (tests share one process and
        // `base_url()`'s result is cached forever after its first call).
        super::set_base_url_override(Some("https://first.example/".to_string()));
        super::set_base_url_override(Some("https://second.example/".to_string()));
        assert_eq!(
            super::BASE_URL_OVERRIDE.get().cloned().flatten(),
            Some("https://first.example/".to_string()),
            "a later call must not clobber the first-set override"
        );
    }

    #[test]
    fn url_joins_base_and_hashed_filename() {
        let u = super::url("app.css");
        assert!(u.starts_with("/b/static/app-"), "unexpected url: {u}");
        assert!(u.ends_with(".css"), "unexpected url: {u}");
    }

    #[test]
    fn cdn_base_template_is_versioned_and_slash_terminated() {
        let t = super::DEFAULT_CDN_BASE_TEMPLATE;
        assert!(t.starts_with("https://cdn.impresspress.org/ui/v"));
        assert!(
            t.ends_with('/'),
            "base must end in / so joins are plain concatenation"
        );
        assert!(
            t.contains(env!("CARGO_PKG_VERSION")),
            "base must pin the crate version"
        );
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn embedded_bytes_agree_with_the_manifest() {
        // The manifest always lists every asset file on disk (build.rs panics
        // on a missing one), but the bytes behind a block-owned key are only
        // compiled in when that block is — under a lean build `marked.min.js`
        // and friends are simply absent. So: every asset that IS compiled in
        // must match its manifest length...
        for e in super::ASSETS {
            if let Some(b) = super::bytes(e.logical) {
                assert_eq!(
                    b.len(),
                    e.len,
                    "{} length disagrees with manifest",
                    e.logical
                );
            }
        }
        // ...and the assets that are never feature-gated must always be there.
        for logical in [
            "app.css",
            "htmx.min.js",
            "chrome.js",
            "webmcp.js",
            "favicon.ico",
            "itim-latin.woff2",
            "itim-latin-ext.woff2",
            "impresspress-logo.png",
            "impresspress-logo-2x.png",
        ] {
            assert!(
                super::bytes(logical).is_some(),
                "core asset missing: {logical}"
            );
        }
    }

    /// The "no-embed build" case the branch's spec called for and never got:
    /// assert the asset bytes are genuinely absent when `embed-assets` is
    /// off, not merely unreferenced-and-hopefully-linker-stripped.
    ///
    /// The strongest form of that guarantee is structural, not a runtime
    /// assertion: [`bytes`] and [`css`] are themselves `#[cfg(feature =
    /// "embed-assets")]`-gated (as of this fix), so under this cfg neither
    /// function -- nor the `include_bytes!`/`include_str!` literals inside
    /// them -- exists in the compiled crate at all; the compiler enforces
    /// it, the linker never has to. This test (only compiled under the
    /// opposite cfg from `embedded_bytes_agree_with_the_manifest` above)
    /// documents and exercises the other half of that contract: the
    /// manifest-driven URL surface -- what a lean build actually needs to
    /// keep working, e.g. to point at R2 or the CDN -- has no dependency on
    /// embedded bytes and still resolves every asset to a valid
    /// content-hashed URL.
    #[cfg(not(feature = "embed-assets"))]
    #[test]
    fn no_embed_build_has_no_asset_bytes_only_the_manifest() {
        assert!(
            !super::ASSETS.is_empty(),
            "build.rs still populates the manifest without embed-assets"
        );
        for e in super::ASSETS {
            // The filename carries its own content hash (baked in by
            // build.rs from the source file's bytes, at *build* time -- this
            // does not require `embed-assets`, which only controls whether
            // those bytes are additionally compiled into the *runtime*
            // binary). `url()` must still resolve it correctly.
            let u = super::url(e.logical);
            assert!(
                u.ends_with(e.filename),
                "{}: url {u} does not end in its own filename",
                e.logical
            );
        }
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn manifest_font_names_appear_in_css_bundle_as_relative_urls() {
        let css = super::css();
        for key in ["itim-latin.woff2", "itim-latin-ext.woff2"] {
            let e = super::ASSETS.iter().find(|e| e.logical == key).unwrap();
            assert!(
                css.contains(&format!("url('{}')", e.filename)),
                "{key} url not rewritten"
            );
        }
        assert!(!css.contains("__ITIM_LATIN_URL__"), "placeholder survived");
        assert!(
            !css.contains("/b/static/"),
            "font url must be relative, not absolute"
        );
    }

    /// The script served at `/b/static/webmcp-{hash}.js` is the COMPOSED one:
    /// the shared core fragment and the tail inside a single IIFE.
    ///
    /// `build.rs` does that composition, so this pins the artifact rather than
    /// a runtime function. Neither half is a valid script alone — the tail
    /// calls `toolOptions` and `buildRequest`, which only exist in the core —
    /// so serving either one raw would be a broken page, silently.
    #[test]
    fn webmcp_js_is_the_core_and_tail_composed_into_one_iife() {
        let js = super::webmcp_js();
        assert!(
            js.starts_with("(function () {\n  'use strict';"),
            "composed script must start with the IIFE: {js:.60}"
        );
        assert!(
            js.contains("function buildRequest"),
            "core fragment's buildRequest is missing"
        );
        assert!(
            js.contains("function toolOptions"),
            "core fragment's toolOptions is missing"
        );
        assert!(
            js.contains("__impresspressWebmcp"),
            "tail's window.__impresspressWebmcp is missing"
        );
        assert!(
            js.trim_end().ends_with("})();"),
            "composed script must end with the IIFE close"
        );
        assert_eq!(
            js.matches("'use strict';").count(),
            1,
            "one strict directive, not one per half"
        );
    }

    /// The manifest's hash for `webmcp.js` is the hash of the bytes actually
    /// served.
    ///
    /// This is the whole reason composition moved into `build.rs`. Composing
    /// at runtime and hashing the raw tail would advertise
    /// `/b/static/webmcp-{hash}.js` for content that hash never described, and
    /// a cache would then hold the wrong script under a URL that claims to be
    /// immutable.
    #[test]
    fn webmcp_manifest_hash_describes_the_composed_bytes() {
        let served = super::webmcp_js().as_bytes();
        let entry = super::entry("webmcp.js");
        assert_eq!(
            entry.len,
            served.len(),
            "manifest length does not match the served script"
        );
        assert!(
            super::webmcp_js_url().ends_with(&format!("webmcp-{}.js", entry.hash)),
            "url must embed the manifest hash: {}",
            super::webmcp_js_url()
        );
    }

    /// The module variant puts the imports first and leaves the IIFE alone.
    ///
    /// Composed here from a stub tail rather than through `dev_js()`, so this
    /// pins the FUNCTION's contract: whatever a caller passes as `imports`
    /// comes out ahead of an IIFE wrapped exactly the way `build.rs` wraps the
    /// classic script. `tests/dev_page.rs` pins the other end — that the
    /// `/b/dev` script really is composed this way and really is served as a
    /// module.
    #[test]
    #[cfg(feature = "block-dev")]
    fn compose_webmcp_module_puts_the_imports_before_an_unchanged_iife() {
        let module = super::compose_webmcp_module("import { X } from '/x.js';", "var used = X;");
        assert!(
            module.starts_with("import { X } from '/x.js';\n(function () {\n  'use strict';\n"),
            "imports must precede the IIFE: {module:.80}"
        );
        assert!(module.trim_end().ends_with("})();"));
        assert_eq!(
            module.matches("'use strict';").count(),
            1,
            "one strict directive, not one per half"
        );
        assert!(
            module.contains("function buildRequest") && module.contains("function toolOptions"),
            "the module variant must carry the same core fragment"
        );
    }
}
