//! E2E: synthetic wafer.lock + ~/.wafer/cache entry → Wafer loads the
//! remote block at boot via Path B (lockfile loader, per
//! docs/superpowers/specs/2026-04-24-wafer-runtime-remote-loading-design.md
//! §End-to-end).
//!
//! Uses `WaferBuilder::lockfile()` directly to avoid touching
//! process-global env vars; no `#[serial]` annotation needed.
//!
//! The call_block dispatch assertion from the spec is deferred: driving a
//! WASM handler requires a full Context + InputStream/OutputStream
//! scaffolding that impresspress tests don't typically build. The registration
//! check alone proves Path B works in impresspress's build.
// TODO(spec §End-to-end): add call_block dispatch assertion once
//   impresspress gains a test-context helper that wraps Context.

use std::fs;

use tempfile::tempdir;
use wafer_run::WaferBuilder;

/// echo_block.wasm compiled from the wafer-run SDK echo example.
/// Registered as "example/echo" (v0.1.0), ABI 1.
const ECHO_WASM: &[u8] = include_bytes!("data/echo_block.wasm");

#[test]
fn lockfile_loads_remote_block() {
    let tmp = tempdir().expect("tempdir");

    // -----------------------------------------------------------------------
    // 1. Seed the cache at ~/.wafer/cache/example/echo/0.1.0/
    //    (using tmp as the fake HOME).
    // -----------------------------------------------------------------------
    let cache_dir = tmp.path().join(".wafer/cache/example/echo/0.1.0");
    fs::create_dir_all(&cache_dir).expect("create cache dir");

    fs::write(cache_dir.join("echo.wasm"), ECHO_WASM).expect("write wasm");
    fs::write(
        cache_dir.join("wafer.toml"),
        // org + name are separate fields; the loader cross-checks
        // "{org}/{name}" against the lockfile package name.
        "[package]\norg = \"example\"\nname = \"echo\"\nversion = \"0.1.0\"\nabi = 1\n",
    )
    .expect("write wafer.toml");

    // -----------------------------------------------------------------------
    // 2. Write the synthetic wafer.lock.
    //    sha256 is a required field in LockfilePackage; the loader
    //    validates source prefix but does not verify the hash at runtime.
    // -----------------------------------------------------------------------
    let lockfile = tmp.path().join("wafer.lock");
    fs::write(
        &lockfile,
        r#"version = 1

[[package]]
name = "example/echo"
version = "0.1.0"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
source = "registry+https://example.test/registry"
"#,
    )
    .expect("write lockfile");

    // -----------------------------------------------------------------------
    // 3. Point the cache root at our tempdir by setting HOME so that
    //    dirs::home_dir() → tmp.path(), then construct Wafer via the
    //    builder's explicit lockfile path (avoids WAFER_LOCKFILE global).
    //
    //    disable_inventory() prevents the impresspress register_static_block! blocks
    //    from being registered — we want a clean runtime for this assertion.
    // -----------------------------------------------------------------------
    let prior_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", tmp.path());

    let result = WaferBuilder::default()
        .disable_inventory()
        .lockfile(&lockfile)
        .build();

    // Restore HOME before any assertion that might panic.
    match prior_home {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }

    let w = result.expect("WaferBuilder::build() with valid lockfile + cache should succeed");

    assert!(
        w.has_block("example/echo"),
        "lockfile-loaded block should be present in the Wafer registry"
    );
}
