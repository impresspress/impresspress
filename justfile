# Default: full build (wasm + native).
default: build

# Build impresspress-web wasm via wasm-pack (gets wasm-opt automatically),
# then the impresspress CLI binary which include_bytes!s the wasm.
# `scripts/wasm-pack-locked.sh` refuses an out-of-date Cargo.lock before
# wasm-pack runs; `wasm-pack … -- --locked` would not (see that script).
build:
    RUSTFLAGS="-C target-feature=+simd128" scripts/wasm-pack-locked.sh crates/impresspress-web build --target web --release --out-dir pkg
    cargo build -p impresspress --release

# Build the CLI in debug profile. Wasm stays release-built (no point
# shipping a debug wasm — it's data baked into the binary).
build-debug:
    RUSTFLAGS="-C target-feature=+simd128" scripts/wasm-pack-locked.sh crates/impresspress-web build --target web --release --out-dir pkg
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

# The D1 read-cost golden (crates/impresspress-core/tests/d1_read_cost.rs):
# what a cold Cloudflare isolate sends D1, with the blocks a Worker compiles.
# `BLESS=1 just d1-cost` rewrites crates/impresspress-core/tests/d1_read_cost.golden.
d1-cost:
    cargo test -p impresspress-core --no-default-features --features test-support,block-files,block-messages,block-vector,block-llm,block-legalpages,block-tickets,block-userportal,block-products,block-signal --test d1_read_cost
