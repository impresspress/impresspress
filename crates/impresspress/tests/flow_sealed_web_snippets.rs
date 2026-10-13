//! Sealed × web must ship all THREE parts of a `wasm-pack --target web`
//! output.
//!
//! The JS glue's first statement is
//! `import { … } from './snippets/<crate>-<hash>/js/bridge.js'`, so a `dist/`
//! without that tree cannot load its own module: `sw.js` self-destructs and
//! the app serves its boot shell forever, with no error anywhere. Nothing
//! about the flow's output *looks* wrong when this regresses, which is why it
//! is a test rather than a comment.
//!
//! `IMPRESSPRESS_WEB_PKG_DIR` is process-global, so both halves live in ONE
//! `#[tokio::test]` that runs them in sequence. Two `#[test]`s would race
//! under the parallel harness — the override-case's `set_var` would be
//! observed by the baked-default case and vice versa, and the failure would
//! be a flake that reproduces on some machines and not others. A separate
//! integration-test binary keeps `flow_sealed_web.rs`'s own test out of the
//! same process; this keeps these two out of each other's.

mod support;

use std::fs;

use impresspress::cli::{flows::sealed_web, helpers::wasm::PKG_DIR_ENV};
use impresspress_bundle::bundle::{sandbox_mode::fixture::runtime_declaring, SandboxShell};
use support::fake_web_pkg;
use tempfile::tempdir;

/// Both scenarios, in sequence. See the module docs for why they are one
/// test: `IMPRESSPRESS_WEB_PKG_DIR` is process-global.
#[tokio::test]
async fn sealed_web_ships_snippets_from_an_override_and_from_the_bake() {
    a_pkg_dir_override_ships_its_snippets_beside_the_wasm_and_glue().await;
    the_baked_default_also_ships_snippets().await;
}

async fn a_pkg_dir_override_ships_its_snippets_beside_the_wasm_and_glue() {
    let pkg = tempdir().unwrap();
    fake_web_pkg(pkg.path(), SandboxShell::None);
    let app = tempdir().unwrap();

    // Removed on every path, including the failing one: the next scenario
    // asserts on the *baked* tree and would otherwise read this fixture's.
    std::env::set_var(PKG_DIR_ENV, pkg.path());
    let built = sealed_web::build(app.path(), false).await;
    std::env::remove_var(PKG_DIR_ENV);
    built.unwrap();

    let dist = app.path().join("dist");

    // The snippet lands at the exact path the glue's import names — the whole
    // point: the relative path inside `snippets/` is what wasm-bindgen wrote
    // into the import, so it has to survive the copy verbatim.
    let snippet = dist.join("snippets/fake-browser-0123456789abcdef/js/bridge.js");
    assert!(
        snippet.is_file(),
        "expected {} to exist; dist held {:?}",
        snippet.display(),
        fs::read_dir(&dist)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        fs::read_to_string(&snippet).unwrap(),
        "export function ping() {}\n",
        "the snippet's bytes must be the override's, not the baked build's"
    );

    // And the pair came from that same directory, so the three parts cannot be
    // from three different builds. The bundler content-hashes the pair and
    // deletes the unhashed copies, so they are found by extension rather than
    // by name.
    let one = |ext: &str| -> std::path::PathBuf {
        let mut found: Vec<_> = fs::read_dir(&dist)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some(ext))
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with("impresspress_web"))
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "expected one impresspress_web*.{ext} in dist"
        );
        found.remove(0)
    };
    assert_eq!(
        fs::read(one("wasm")).unwrap(),
        runtime_declaring(SandboxShell::None)
    );
    assert!(
        fs::read_to_string(one("js"))
            .unwrap()
            .contains("./snippets/fake-browser-0123456789abcdef/js/bridge.js"),
        "the bundler must not rewrite the snippets import out of the glue"
    );
}

/// Without the override, the flow writes the tree `build.rs` baked — which is
/// what makes the *default* sealed bundle work, not just an overridden one.
async fn the_baked_default_also_ships_snippets() {
    let app = tempdir().unwrap();
    std::env::remove_var(PKG_DIR_ENV);
    sealed_web::build(app.path(), false).await.unwrap();

    let snippets = app.path().join("dist/snippets");
    assert!(
        snippets.is_dir(),
        "the baked impresspress-web build has inline JS modules, so `dist/snippets/` must exist"
    );
    let bridges: Vec<_> = fs::read_dir(&snippets)
        .unwrap()
        .flatten()
        .map(|e| e.path().join("js/bridge.js"))
        .filter(|p| p.is_file())
        .collect();
    assert!(
        !bridges.is_empty(),
        "expected at least one <crate>-<hash>/js/bridge.js under {}",
        snippets.display()
    );
}
