//! Fixtures shared by the sealed × web flow's test binaries.

use std::{fs, path::Path};

use impresspress_bundle::bundle::{sandbox_mode::fixture::runtime_declaring, SandboxShell};

/// A minimal `wasm-pack --target web --out-dir <dir>` output of a runtime
/// compiled as `mode`, for `IMPRESSPRESS_WEB_PKG_DIR`: the manifest that
/// names the glue, the pair itself — the wasm a module declaring `mode`, as
/// every real runtime does — and one inline-JS snippet under the
/// `<crate>-<hash>` directory wasm-bindgen assigns.
pub fn fake_web_pkg(dir: &Path, mode: SandboxShell) {
    fs::write(
        dir.join("package.json"),
        r#"{"name":"fake-web","type":"module","main":"fake_web.js","types":"fake_web.d.ts"}"#,
    )
    .unwrap();
    fs::write(dir.join("fake_web_bg.wasm"), runtime_declaring(mode)).unwrap();
    // The glue must carry the literal the bundler rewrites when it
    // content-hashes the wasm — `sealed_web` writes the pair under
    // impresspress's own names, so that is the literal to spell here.
    fs::write(
        dir.join("fake_web.js"),
        "import { ping } from './snippets/fake-browser-0123456789abcdef/js/bridge.js';\n\
         const url = new URL('impresspress_web_bg.wasm', import.meta.url);\n",
    )
    .unwrap();
    let snippet_dir = dir.join("snippets/fake-browser-0123456789abcdef/js");
    fs::create_dir_all(&snippet_dir).unwrap();
    fs::write(snippet_dir.join("bridge.js"), "export function ping() {}\n").unwrap();
}
