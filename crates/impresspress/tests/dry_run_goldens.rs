//! Asserts the command construction for each flow under
//! `IMPRESSPRESS_CLI_DRY_RUN=1`. Subprocess invocations are printed as
//! `DRY_RUN step="..." cmd=... args=[...]` lines instead of being spawned,
//! letting the test runner verify behavior without `wafer` / `wasm-pack`
//! / `cargo` of the user's project being available.

use std::process::Command;

use tempfile::tempdir;

fn run_dry(args: &[&str], cwd: &std::path::Path) -> (String, std::process::ExitStatus) {
    let exe = env!("CARGO_BIN_EXE_impresspress");
    let out = Command::new(exe)
        .args(args)
        .env("IMPRESSPRESS_CLI_DRY_RUN", "1")
        .current_dir(cwd)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status,
    )
}

#[test]
fn sealed_native_build_dry_run() {
    let tmp = tempdir().unwrap();
    // Plant a `.git` so the mode walk-up stops here and treats this as
    // sealed (no Cargo.toml above).
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let (out, status) = run_dry(&["build", "--target", "native"], tmp.path());
    assert!(status.success(), "build exited non-zero, stdout:\n{out}");
    // Sealed × native build with no blocks/ and no frontend/ produces no
    // subprocess steps; only the "ready" line.
    assert!(
        out.contains("ready: run"),
        "expected ready line in stdout, got:\n{out}"
    );
}

#[test]
fn sealed_web_build_dry_run_with_blocks() {
    let tmp = tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let block = tmp.path().join("blocks/foo");
    std::fs::create_dir_all(&block).unwrap();
    std::fs::write(block.join("Cargo.toml"), "").unwrap();

    let (out, status) = run_dry(&["build", "--target", "web"], tmp.path());
    assert!(status.success(), "build exited non-zero, stdout:\n{out}");
    assert!(
        out.contains("step=\"wafer build blocks/foo\""),
        "expected wafer subprocess step in stdout, got:\n{out}"
    );
}
