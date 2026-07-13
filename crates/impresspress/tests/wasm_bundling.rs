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
