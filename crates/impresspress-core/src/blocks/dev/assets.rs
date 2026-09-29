//! The `/b/dev` page's own static assets — its script, its stylesheet and the
//! compiler adapter that script imports.
//!
//! They live with the block rather than in [`crate::ui::assets`] because they
//! are the sandbox's, not the admin chrome's: a build without `block-dev`
//! must not carry them, and a page other than `/b/dev` has no use for any of
//! them. That is also why they are served from `/b/dev/static/*` at the
//! block's own `Admin` tier instead of the public, content-hashed
//! `/b/static/*` bundle.
//!
//! Only the first half of that generalises, and two blocks deliberately took
//! only the first half: `blocks::llm::assets` and `blocks::files::assets`
//! (named in prose, not as intra-doc links, because neither module exists on
//! a build without its own `block-*` feature) declare their own files the way
//! this module does, but keep serving them from the shared, content-hashed
//! `/b/static/*` manifest. Their assets are public page assets rather than
//! `Admin`-tier ones, and the manifest is what lets an `embed-assets`-off
//! build stream them from object storage instead of compiling them in. So
//! "assets live with their block" is the rule; "and the block serves them
//! itself" is this module's own consequence of being `Admin`-gated, not part
//! of it. Both of those module headers say the same thing from their side.

use std::sync::{LazyLock, OnceLock};

/// The workspace page's script, authored as a TAIL: no IIFE, no
/// `'use strict'`, exactly like `ui/assets/webmcp.js`. It is only ever served
/// through [`dev_js`], which composes it with the shared `webmcp-core.js`
/// fragment — reading this file on its own, a top-level `return` or a call to
/// `toolOptions` looks unbound; it is not, because the composition below is
/// the only form in which the bytes exist on the wire.
const DEV_JS_TAIL: &str = include_str!("assets/dev.js");

/// The module imports [`dev_js`] emits ahead of the IIFE.
///
/// `import` declarations may only stand at a module's top level, so they
/// cannot be written inside the tail — the tail is a function body once the
/// wrapper closes around it. Keeping them here also keeps the fact that
/// `/b/dev/static/dev.js` is a MODULE in one place: this constant, the
/// [`crate::ui::assets::compose_webmcp_module`] call below, and the
/// `type="module"` on the page's script tag are the three halves of that
/// single decision, and a test in `tests/dev_page.rs` pins them together.
///
/// A static import rather than a `import()` at first use: the adapter is part
/// of this page's contract, not an optional extra, so a build that shipped a
/// broken one should fail when the workspace loads — where the failure is
/// visible and attributable — rather than the first time somebody presses
/// Compile.
const DEV_JS_IMPORTS: &str =
    "import { BrowserRustCompiler } from '/b/dev/static/compiler-adapter.js';";

/// The workspace page's stylesheet.
const DEV_CSS: &str = include_str!("assets/dev.css");

/// The page half of the compiler protocol: a standalone ES module exporting
/// `BrowserRustCompiler`.
///
/// Served as-is — it is not a WebMCP tail and composes with nothing. It is a
/// module because the worker it creates is a module worker and because a
/// class sealed inside `dev.js`'s IIFE could not be driven by a test; see the
/// file's own header for the rest of the reasoning.
const COMPILER_ADAPTER_JS: &str = include_str!("assets/compiler-adapter.js");

/// The composed `/b/dev/static/dev.js`: the module imports, then
/// `webmcp-core.js` plus [`DEV_JS_TAIL`] inside one IIFE.
///
/// Composed through [`crate::ui::assets::compose_webmcp_module`] — the module
/// sibling of the function `webmcp_js()` uses — so both WebMCP scripts this
/// crate serves are wrapped identically inside the IIFE and
/// `buildRequest`/`toolOptions` behave the same in each. Cached in a
/// `LazyLock` so repeat requests don't re-format the string.
pub fn dev_js() -> &'static str {
    static SCRIPT: LazyLock<String> =
        LazyLock::new(|| crate::ui::assets::compose_webmcp_module(DEV_JS_IMPORTS, DEV_JS_TAIL));
    &SCRIPT
}

/// The `/b/dev/static/dev.css` stylesheet.
pub fn dev_css() -> &'static str {
    DEV_CSS
}

/// Short content hash of [`dev_js`], for `page.rs::asset`'s `ETag`, so a
/// rebuilt binary with different script bytes invalidates a repeat visitor's
/// cached copy instead of serving a `304` for content that changed.
///
/// Hashed here rather than read off a manifest entry because these assets are
/// deliberately block-local: they are served from `/b/dev/static/*` at this
/// block's own `Admin` tier, never from the public `/b/static/` bundle, so
/// there is no manifest entry to read. `ui::assets::short_hash` is the same
/// projection `build.rs` applies to manifest assets, so a hash means the same
/// thing on both sides.
pub fn dev_js_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| crate::ui::assets::short_hash(dev_js().as_bytes()))
}

/// Short content hash of [`dev_css`], for `page.rs::asset`'s `ETag`.
pub fn dev_css_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| crate::ui::assets::short_hash(dev_css().as_bytes()))
}

/// The `/b/dev/static/compiler-adapter.js` module.
pub fn compiler_adapter_js() -> &'static str {
    COMPILER_ADAPTER_JS
}

/// Short content hash of [`compiler_adapter_js`], for `page.rs::asset`'s
/// `ETag`.
pub fn compiler_adapter_js_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| crate::ui::assets::short_hash(compiler_adapter_js().as_bytes()))
}
