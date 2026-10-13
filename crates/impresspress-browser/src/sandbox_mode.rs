//! The runtime's sandbox mode, declared to what loads it.
//!
//! The mode itself is `impresspress_core::sandbox_mode::SandboxMode::COMPILED`,
//! decided by `impresspress-core`'s `site-runtime`/`block-dev` features. It
//! is declared here, in the crate every browser cdylib links (each one
//! stores its Wafer through `store_wafer`), so that every such cdylib
//! carries it — `impresspress-web`'s three builds and an
//! application's own cdylib bundled through the embed flow alike — in two
//! forms:
//!
//! * [`compiled_sandbox_mode`], a `#[wasm_bindgen]` export the bundle's
//!   `sw.js` imports and calls;
//! * the `impresspress.sandbox_mode` custom section, holding the same
//!   spelling as raw bytes, for tooling that reads the wasm without running
//!   it.
//!
//! A consumer has nothing to call or re-export. wasm-bindgen emits the
//! export as an `#[export_name]` symbol of this crate (its JS shim description
//! goes into this crate's `__wasm_bindgen_unstable` section, which wasm-ld
//! concatenates with the consumer's), and rustc exports every upstream
//! crate's C-level symbol from a cdylib, so the export and its glue reach the
//! consumer's wasm and JS whether or not the consumer names it. CI's
//! `examples/minimal-browser` bundle step checks exactly that on a cdylib
//! that does not mention it.

use impresspress_core::sandbox_mode::SandboxMode;
use wasm_bindgen::prelude::*;

/// The mode's spelling ([`SandboxMode::as_str`] of [`SandboxMode::COMPILED`]),
/// the one value both declarations below carry.
const SPELLING: &str = SandboxMode::COMPILED.as_str();

/// Which sandbox this runtime was compiled with — `'none'`, `'site'` or
/// `'workspace'` (`SandboxMode::COMPILED`, spelled by `SandboxMode::as_str`).
///
/// `sw.js` imports this and calls it after `init()` and before
/// `initialize()`, and compares it with the mode the bundle was built for
/// (`const SANDBOX`, rendered from `[sandbox] mode`). A mismatch is a bundle
/// assembled from the wrong runtime; the worker stops there with stage
/// `build` — before anything has opened the database, so no recovery erases
/// data for it — rather than boot a workspace's `sw.js` over a site build,
/// or a site's over the workspace.
///
/// The import is static, so a runtime without this export would fail to link
/// the worker module before any of `sw.js` ran. That is why it lives in this
/// crate and not in any one runtime: every browser cdylib links this crate,
/// and so every one exports it.
///
/// It answers with [`SANDBOX_MODE_SECTION`]'s bytes rather than with
/// [`SPELLING`] directly. That read is what ties the custom section to the
/// export in every build: the object file that holds the section is the one
/// that defines the static, and the export refers to the static, so wasm-ld
/// links that object whenever it links the export. Without the reference, a
/// build without LTO (a `--dev` build) keeps the section only if some other
/// symbol of the same codegen unit happens to be needed.
#[wasm_bindgen]
pub fn compiled_sandbox_mode() -> String {
    // The bytes are a copy of a `&str`, so they are always UTF-8; the lossy
    // conversion never replaces anything.
    String::from_utf8_lossy(&SANDBOX_MODE_SECTION).into_owned()
}

/// The mode's spelling as a static: on wasm32, the `impresspress.sandbox_mode`
/// custom section — [`SPELLING`]'s bytes, no length prefix and no
/// terminator.
///
/// A custom section because it is part of the shipped bytes — it cannot be
/// separated from the wasm it describes — and any wasm parser reads it
/// without instantiating the module. Why it survives into the shipped wasm:
///
/// * on wasm32, rustc records a `#[link_section]` static's bytes as
///   `wasm.custom_sections` metadata, which LLVM writes out as a custom
///   section of the object file that defines the static (the static itself
///   is still an ordinary global, read by [`compiled_sandbox_mode`]). Fat
///   LTO (`lto = true`, the release profile) merges that metadata,
///   concatenating entries of the same name;
/// * without LTO, wasm-ld links that object because
///   [`compiled_sandbox_mode`], which every cdylib exports, refers to the
///   static;
/// * wasm-ld copies every custom section of the objects it links into the
///   output, and `strip = true` (`--strip-all`) removes only the `name` and
///   debug sections;
/// * wasm-bindgen's walrus rewrite keeps sections it does not know as raw
///   bytes (it removes only its own `__wasm_bindgen_unstable`);
/// * wasm-opt carries unknown custom sections through, and the arguments
///   every runtime here is optimised with (`-Oz --all-features`) do not ask
///   it to strip any.
///
/// CI checks the result on real bundles of all three modes, release and
/// `--dev` (`scripts/check-bundle-runtime.mjs`).
///
/// The section is wasm32 only: on a native target the name is not a valid
/// object-file section (Mach-O wants `segment,section`), and only a wasm has
/// anywhere to carry it. Natively this is a plain static.
#[cfg_attr(target_arch = "wasm32", link_section = "impresspress.sandbox_mode")]
static SANDBOX_MODE_SECTION: [u8; SPELLING.len()] = {
    // `SPELLING`'s bytes as an array of exactly its length: a `static` needs
    // a sized, owned initialiser, and `as_bytes` is a borrowed slice.
    let src = SPELLING.as_bytes();
    let mut out = [0u8; SPELLING.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = src[i];
        i += 1;
    }
    out
};
