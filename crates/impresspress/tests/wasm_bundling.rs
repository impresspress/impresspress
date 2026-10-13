//! Verifies the bundled impresspress-web wasm is non-empty and starts with
//! the wasm magic bytes (0x00 0x61 0x73 0x6D — "\0asm").

#[test]
fn bundled_wasm_is_present_and_valid() {
    let bytes = impresspress::IMPRESSPRESS_WEB_WASM;
    assert!(!bytes.is_empty(), "bundled wasm must be non-empty");
    assert_eq!(
        &bytes[0..4],
        &[0x00, 0x61, 0x73, 0x6D],
        "bundled bytes must start with wasm magic"
    );
}

/// The baked wasm is the real `crates/impresspress-web/pkg` build — in CI the
/// `build-wasm` artifact, through wasm-bindgen and `wasm-opt -Oz` — and it
/// declares its sandbox mode where the bundler reads it: one
/// `impresspress.sandbox_mode` custom section, written by
/// `impresspress-browser` under the name `impresspress-bundle` spells
/// separately. A runtime built with no sandbox feature is `none`. This is
/// what holds the two spellings of the section's name together, and the
/// section to surviving the release pipeline: if either drifts, every
/// `impresspress build --target web` refuses its runtime, and this says why.
#[test]
fn bundled_wasm_declares_no_sandbox() {
    assert_eq!(
        impresspress_bundle::bundle::sandbox_mode::compiled_sandbox_mode(
            impresspress::IMPRESSPRESS_WEB_WASM
        ),
        Ok(impresspress_bundle::bundle::SandboxShell::None)
    );
}
