use std::fs;

use impresspress::cli::helpers::wasm::{resolve_impresspress_web_wasm, WasmSource};
use tempfile::tempdir;

// All sub-cases share `IMPRESSPRESS_WEB_WASM` env state — combined into one
// test to run sequentially without a serial-test dep.
#[test]
fn wasm_resolution() {
    // 1. Bundled fallback.
    std::env::remove_var("IMPRESSPRESS_WEB_WASM");
    let resolved = resolve_impresspress_web_wasm().unwrap();
    assert!(!resolved.bytes.is_empty());
    assert_eq!(&resolved.bytes[0..4], &[0x00, 0x61, 0x73, 0x6D]);
    assert_eq!(resolved.source, WasmSource::Baked);

    // 2. Env override returns file contents.
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("override.wasm");
    fs::write(&path, b"\x00asm\x01\x00\x00\x00").unwrap();
    std::env::set_var("IMPRESSPRESS_WEB_WASM", &path);
    let resolved = resolve_impresspress_web_wasm().unwrap();
    assert_eq!(resolved.bytes.as_ref(), b"\x00asm\x01\x00\x00\x00");
    assert_eq!(resolved.source, WasmSource::WasmEnv(path));

    // 3. Env override to missing path errors.
    std::env::set_var("IMPRESSPRESS_WEB_WASM", "/nonexistent/path.wasm");
    let result = resolve_impresspress_web_wasm();
    assert!(result.is_err());

    std::env::remove_var("IMPRESSPRESS_WEB_WASM");
}
