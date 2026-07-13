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

        template::render_to_file(&src, &out, &vars).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();

        assert!(body.contains("RECOVERY_DONE_KEY"), "missing loop-guard key");
        assert!(
            body.contains("renderRecoveryStuckUI"),
            "missing stuck-UI fallback fn"
        );
        assert!(
            body.contains("Reset local data and reload"),
            "missing manual reset button label"
        );
        assert!(body.contains("const OPFS_WIPE_ON_RECOVERY = false;"));
    }
}
