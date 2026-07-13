# Default: full build (wasm + native).
default: build

# Build impresspress-web wasm via wasm-pack (gets wasm-opt automatically),
# then the impresspress CLI binary which include_bytes!s the wasm.
build:
    cd crates/impresspress-web && RUSTFLAGS="-C target-feature=+simd128" wasm-pack build --target web --release --out-dir pkg
    cargo build -p impresspress --release

# Build the CLI in debug profile. Wasm stays release-built (no point
# shipping a debug wasm — it's data baked into the binary).
build-debug:
    cd crates/impresspress-web && RUSTFLAGS="-C target-feature=+simd128" wasm-pack build --target web --release --out-dir pkg
    cargo build -p impresspress

# Run the workspace test suite.
test:
    cargo test --workspace

# Run only the unit tests (no integration tests that need a wasm rebuild).
test-unit:
    cargo test --workspace --lib

# Clean all build artifacts.
clean:
    cargo clean
    rm -rf crates/impresspress-web/pkg

# Pull bundled font binaries from impresspress/site-kit. Run when site-kit
# ships a font update, then commit the changed binaries.
sync-fonts:
    ./scripts/sync-fonts.sh
