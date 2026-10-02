//! Static assets shipped with the framework crate, exposed as a typed
//! `Asset` slice plus a `write_to(dir)` convenience.

use std::path::Path;

pub struct Asset {
    /// Path relative to the target directory, using forward slashes. E.g.
    /// `"sw.js.tmpl"` or `"vendor/sql-wasm.wasm"`.
    pub path: &'static str,
    pub bytes: &'static [u8],
}

pub fn static_assets() -> &'static [Asset] {
    ASSETS
}

/// The directory the shell's own third-party files ship under.
pub const VENDOR_DIR: &str = "vendor/";

/// The shell's own files under [`VENDOR_DIR`] (`vendor/sql-wasm-esm.js`,
/// `vendor/sql-wasm.wasm`), as asset paths.
///
/// The service worker bypasses exactly these, not the whole `/vendor/`
/// prefix: the directory is a common one for a site's own files, and a
/// prefix bypass would hand every one of them to the static host instead of
/// the runtime that serves the site. This list is the one source of truth for
/// those exact rules in `bundle::BypassRules::for_bundle`.
///
/// The one other place that names these files is their loader,
/// `crates/impresspress-browser/js/bridge.js`, which requests
/// `/vendor/sql-wasm-esm.js` and `/vendor/sql-wasm.wasm` by literal path: a
/// rename here must change it too, or the runtime's database cannot load.
pub fn vendor_files() -> impl Iterator<Item = &'static str> {
    static_assets()
        .iter()
        .map(|asset| asset.path)
        .filter(|path| path.starts_with(VENDOR_DIR))
}

pub fn write_to(dir: &Path) -> std::io::Result<()> {
    for asset in static_assets() {
        let out = dir.join(asset.path);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, asset.bytes)?;
    }
    Ok(())
}

const ASSETS: &[Asset] = &[
    Asset {
        path: "sw.js.tmpl",
        bytes: include_bytes!("../assets/sw.js.tmpl"),
    },
    Asset {
        path: "loader.js.tmpl",
        bytes: include_bytes!("../assets/loader.js.tmpl"),
    },
    Asset {
        path: "index.html.tmpl",
        bytes: include_bytes!("../assets/index.html.tmpl"),
    },
    Asset {
        path: "vendor/sql-wasm-esm.js",
        bytes: include_bytes!("../assets/vendor/sql-wasm-esm.js"),
    },
    Asset {
        path: "vendor/sql-wasm.wasm",
        bytes: include_bytes!("../assets/vendor/sql-wasm.wasm"),
    },
    Asset {
        path: "webllm-engine.js",
        bytes: include_bytes!("../assets/webllm-engine.js"),
    },
    Asset {
        path: "embed-engine.js",
        bytes: include_bytes!("../assets/embed-engine.js"),
    },
    Asset {
        path: "t2i-engine.js",
        bytes: include_bytes!("../assets/t2i-engine.js"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_assets_is_non_empty_and_has_expected_paths() {
        let paths: Vec<&str> = static_assets().iter().map(|a| a.path).collect();
        assert!(paths.contains(&"sw.js.tmpl"));
        assert!(paths.contains(&"loader.js.tmpl"));
        assert!(paths.contains(&"index.html.tmpl"));
        assert!(paths.contains(&"vendor/sql-wasm-esm.js"));
        assert!(paths.contains(&"vendor/sql-wasm.wasm"));
        assert!(paths.contains(&"webllm-engine.js"));
        assert!(paths.contains(&"embed-engine.js"));
        assert!(paths.contains(&"t2i-engine.js"));
    }

    /// `bridge.js` loads sql.js by literal path; every `/vendor/` path it
    /// requests must be one the shell ships (and the service worker bypasses),
    /// so renaming a vendored file in only one of the two places fails here.
    #[test]
    fn every_vendor_path_bridge_js_requests_is_a_shipped_vendor_file() {
        let bridge_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../impresspress-browser/js/bridge.js"
        );
        let bridge =
            std::fs::read_to_string(bridge_path).unwrap_or_else(|e| panic!("{bridge_path}: {e}"));
        let shipped: Vec<String> = vendor_files().map(|path| format!("/{path}")).collect();

        let mut requested = Vec::new();
        for quote in ['\'', '"'] {
            let open = format!("{quote}/{VENDOR_DIR}");
            let mut rest = bridge.as_str();
            while let Some(start) = rest.find(&open) {
                let literal = &rest[start + 1..];
                let end = literal
                    .find(quote)
                    .unwrap_or_else(|| panic!("unterminated literal in {bridge_path}"));
                requested.push(literal[..end].to_string());
                rest = &literal[end + 1..];
            }
        }

        assert!(
            !requested.is_empty(),
            "{bridge_path} names no /vendor/ path"
        );
        for path in &requested {
            assert!(
                shipped.contains(path),
                "{bridge_path} requests {path}, which the shell does not ship; shipped: {shipped:?}"
            );
        }
    }

    #[test]
    fn every_asset_has_non_empty_bytes() {
        for asset in static_assets() {
            assert!(
                !asset.bytes.is_empty(),
                "asset {:?} has empty bytes",
                asset.path
            );
        }
    }

    #[test]
    fn write_to_writes_all_files_with_correct_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        write_to(tmp.path()).unwrap();
        for asset in static_assets() {
            let got = std::fs::read(tmp.path().join(asset.path)).unwrap();
            assert_eq!(got, asset.bytes, "mismatched bytes for {:?}", asset.path);
        }
    }

    // Regression: the loader's recovery path must include a loop-guard that
    // stops the `self-destruct → wipe → ?_freshen reload → self-destruct`
    // cycle that traps production builds (OPFS_WIPE_ON_RECOVERY=false) when
    // initialize() keeps failing after a wipe — and must surface a manual
    // reset UI instead. Render the actual template and assert the loop-guard
    // wiring is present.
    #[test]
    fn loader_template_renders_with_recovery_loop_guard() {
        use std::collections::BTreeMap;

        use crate::bundle::template;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("loader.js.tmpl");
        let out = tmp.path().join("loader.js");
        std::fs::write(&src, include_bytes!("../assets/loader.js.tmpl")).unwrap();
        let mut vars = BTreeMap::new();
        vars.insert("APP_NAME".into(), "demo-app".into());
        vars.insert("BOOT_REDIRECT".into(), "/".into());
        vars.insert("OPFS_WIPE_ON_RECOVERY".into(), "false".into());
        vars.insert("SHELL_URL".into(), crate::bundle::SHELL_URL.into());

        template::render_to_file(&src, &out, &vars).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();

        assert!(body.contains("RECOVERY_DONE_KEY"), "missing loop-guard key");
        assert!(
            body.contains("renderStoppedUI"),
            "missing stopped-UI fallback fn"
        );
        assert!(
            body.contains("label: 'Reset local data and reload'"),
            "missing manual reset button label"
        );
        assert!(body.contains("const OPFS_WIPE_ON_RECOVERY = false;"));
        assert!(body.contains("const SHELL_URL = '/';"));
        assert!(
            body.contains("const BOOT_PROBE_TIMEOUT_MS = 60_000;"),
            "missing readiness-probe timeout"
        );
        assert!(
            body.contains("signal: controller.signal"),
            "readiness fetch is not abortable"
        );
        // The stopped UI says what stopped the runtime — as text, never as
        // markup — and offers the retry beside the reset.
        assert!(
            body.contains("said: stoppedText(failure.cause),"),
            "the stopped UI does not show the cause"
        );
        assert!(
            body.contains(
                "document.getElementById('impresspress-stopped-cause').textContent = said;"
            ),
            "the cause is not set as text"
        );
        // A recovery's OPFS wipe has one gate, and it asks for the one stage
        // that can mean the stored data is unusable. A module that failed to
        // load, a request the runtime died on and a probe that ran out of
        // time do not pass it. (`tests/sw/loader_recovery.test.mjs` drives
        // every road into it.)
        assert!(
            body.contains("return OPFS_WIPE_ON_RECOVERY && failure.stage === STAGE_INITIALIZE;"),
            "the wipe is not gated on an initialize() failure"
        );
        // The automatic recovery runs from exactly one place — a cause the
        // worker reported — and a timeout never reaches it.
        assert_eq!(
            body.matches("await recoverBrowserState(").count(),
            1,
            "something other than a reported cause runs the automatic recovery"
        );
        assert!(
            !body.contains("local data is incompatible"),
            "the stuck UI still guesses at a cause instead of showing it"
        );
    }
}
