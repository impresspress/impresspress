//! Native web-bundle tooling for `impresspress build --target web` (and external
//! consumers like gizza-ai).
//!
//! This crate owns the static framework assets (`sw.js`/`loader.js`/
//! `index.html` templates + the sql.js vendor pair + engine shims) and the
//! build-time bundler that hashes the wasm-pack output pair, rewrites the glue
//! reference, writes the asset manifest, and renders the templates.
//!
//! Split out of `impresspress-browser` so that crate can be a pure wasm32 cdylib:
//! the bundler is `std::fs` host tooling that the native `impresspress` CLI runs,
//! and pulling it through `impresspress-browser` forced the native binary to
//! depend on the wasm-bindgen stack. `impresspress-browser` no longer compiles this
//! code on native.
//!
//! - [`assets`] — the shipped static assets + `write_to(dir)`.
//! - [`bundle`] — the hash/rename/manifest/template build step (`bundle::run`).

pub mod assets;
pub mod bundle;
