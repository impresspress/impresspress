//! Cross-compile the consumer crate via `worker-build`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

/// Exact `worker-build` version this CLI provisions and drives.
///
/// Pinned to an exact version (not a semver range) so [`ensure_worker_build_installed`]
/// can skip `cargo install` outright once the right binary is already on
/// `PATH`, and so every build path installs the identical toolchain: this
/// helper (used by `impresspress build --target cloudflare` locally) and the
/// `[build] command` embedded in the developer `wrangler.toml` by
/// [`super::wrangler::base_toml`]. Production deploys use a separate
/// upload-only config with that hook removed, so the artifact produced here
/// is not rebuilt by `wrangler versions upload`.
///
/// worker-build checks the consumer's `worker` crate: 0.8.x refuses
/// `worker < 0.8`. Bump together with the `worker` crate version pin in
/// `Cargo.toml` (the embedded command in `wrangler::base_toml` reads this
/// constant), and re-vendor the shim fixture under
/// `tests/support/cloudflare_entry/`, which `tests/cloudflare_entry_module.rs`
/// checks against this version.
pub const WORKER_BUILD_VERSION: &str = "0.8.7";

/// Ensure `worker-build` is installed at exactly [`WORKER_BUILD_VERSION`].
///
/// Checks the already-installed binary's own `--version` output first and
/// returns immediately when it matches — skipping `cargo install` entirely.
/// The previous implementation ran `cargo install worker-build --version
/// ^0.7` unconditionally on every build; even when the right version was
/// already present, that command still resolves the semver range against
/// the crates.io index (a network round trip) before deciding there's
/// nothing to do. A plain `worker-build --version` check avoids that.
///
/// # Errors
///
/// Returns an error if the install subprocess fails to spawn or exits
/// non-zero.
pub async fn ensure_worker_build_installed() -> Result<()> {
    if installed_version_matches(WORKER_BUILD_VERSION).await {
        return Ok(());
    }

    let version_req = format!("={WORKER_BUILD_VERSION}");
    let install = Command::new("cargo")
        .args([
            "install",
            "worker-build",
            "--version",
            &version_req,
            "--quiet",
        ])
        .status()
        .await
        .with_context(|| format!("run `cargo install worker-build --version {version_req}`"))?;
    if !install.success() {
        bail!(
            "cargo install worker-build --version {version_req} failed (exit {:?})",
            install.code()
        );
    }
    Ok(())
}

/// `true` if `worker-build --version` is already on `PATH` and reports
/// exactly `expected`. Any failure to run it (not installed, no permission,
/// unexpected output) is treated as "doesn't match" rather than an error —
/// the caller falls back to `cargo install`, which is the same recovery a
/// missing/wrong binary needed before this check existed.
async fn installed_version_matches(expected: &str) -> bool {
    let Ok(output) = Command::new("worker-build").arg("--version").output().await else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).trim() == expected
}

/// The `strip` setting worker-build's cargo build runs with, as the
/// environment override of the consumer's `[profile.release]`.
///
/// worker-build 0.8 has wasm-bindgen generate catch wrappers
/// (`--force-enable-abort-handler`), and wasm-bindgen 0.2.129 generates them
/// only in externref mode, which it switches on only when the module's
/// `target_features` section lists `reference-types`. `strip = true` (which
/// the size-first profile sets, and which the browser build keeps) has the
/// linker drop that section, so the build fails with "externref table
/// required for catch wrappers" (cloudflare/workers-rs#1014). Stripping only
/// debug info keeps the section, and with it the function-names section;
/// [`strip_name_section`] removes that afterwards. Drop this override, and
/// that step, once a wasm-bindgen release reads reference types without the
/// section (wasm-bindgen/wasm-bindgen#5382, or #5205).
pub const WORKER_BUILD_STRIP_ENV: (&str, &str) = ("CARGO_PROFILE_RELEASE_STRIP", "debuginfo");

/// `worker-build` with the arguments and environment every Worker build of
/// this CLI uses, run in `crate_dir`.
pub fn worker_build_command(crate_dir: &Path) -> Command {
    let mut cmd = Command::new("worker-build");
    cmd.current_dir(crate_dir)
        .env(WORKER_BUILD_STRIP_ENV.0, WORKER_BUILD_STRIP_ENV.1)
        .args(["--no-default-features", "--features", "target-cloudflare"]);
    cmd
}

/// Cross-compile the consumer crate to `wasm32-unknown-unknown` via
/// `worker-build`, then drop the function-names section from its output.
///
/// Uses `tokio::process::Command` because the worker-build subprocess is
/// long-running (full wasm32 cargo build of the consumer crate plus the
/// wasm-bindgen post-processing step); blocking on `status()` from a
/// `std::process::Command` would freeze the tokio worker thread.
///
/// # Errors
///
/// Returns an error if `worker-build` cannot be installed, fails to spawn,
/// or exits non-zero, or if its output module cannot be rewritten.
pub async fn run(repo_root: &Path, _release: bool) -> Result<()> {
    ensure_worker_build_installed().await?;

    let status = worker_build_command(repo_root)
        .status()
        .await
        .context("run worker-build")?;
    if !status.success() {
        bail!("worker-build failed (exit {:?})", status.code());
    }
    strip_name_section(&repo_root.join("build/index_bg.wasm"))
}

/// Remove the function-names custom section (`name`) from the wasm module at
/// `path`, in place, leaving every other section as it was.
///
/// The section is there because [`WORKER_BUILD_STRIP_ENV`] keeps it through
/// the link, and worker-build's `wasm-opt` step keeps names on purpose
/// (`--debuginfo`). Measured on `examples/webmcp-demo` (2026-10-27) it is a
/// quarter of the module and 11% of the gzipped upload, and nothing at run
/// time reads it.
///
/// # Errors
///
/// Returns an error if the file cannot be read or written, or is not a wasm
/// module this function can walk.
pub fn strip_name_section(path: &Path) -> Result<()> {
    let module = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let stripped = without_custom_section(&module, "name")
        .with_context(|| format!("walk the sections of {}", path.display()))?;
    std::fs::write(path, stripped).with_context(|| format!("write {}", path.display()))
}

/// The wasm magic number and binary version 1, which open every module.
const WASM_HEADER: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

/// `module` without its custom sections called `name`, every other byte kept.
fn without_custom_section(module: &[u8], name: &str) -> Result<Vec<u8>> {
    if module.get(..WASM_HEADER.len()) != Some(&WASM_HEADER[..]) {
        bail!("not a version-1 wasm module");
    }
    let mut out = Vec::with_capacity(module.len());
    out.extend_from_slice(&WASM_HEADER);
    let mut at = WASM_HEADER.len();
    while at < module.len() {
        let start = at;
        let id = module[at];
        at += 1;
        let size = read_u32_leb128(module, &mut at)? as usize;
        let payload_start = at;
        let end = payload_start
            .checked_add(size)
            .filter(|&end| end <= module.len())
            .with_context(|| format!("section at byte {start} runs past the end"))?;
        let skip = id == 0 && {
            let name_len = read_u32_leb128(module, &mut at)? as usize;
            let name_end = at
                .checked_add(name_len)
                .filter(|&name_end| name_end <= end)
                .with_context(|| format!("custom section name at byte {start} overruns it"))?;
            &module[at..name_end] == name.as_bytes()
        };
        if !skip {
            out.extend_from_slice(&module[start..end]);
        }
        at = end;
    }
    Ok(out)
}

/// Read an unsigned LEB128 `u32` at `*at`, advancing past it.
fn read_u32_leb128(bytes: &[u8], at: &mut usize) -> Result<u32> {
    let mut value: u32 = 0;
    for shift in (0..35).step_by(7) {
        let byte = *bytes.get(*at).context("LEB128 runs past the end")?;
        *at += 1;
        // The fifth byte holds bits 28-31: only its low four bits fit.
        if shift == 28 && byte & 0x70 != 0 {
            bail!("LEB128 overflows u32");
        }
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("LEB128 longer than 5 bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A section: its id, its size and the payload.
    fn section(id: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![id];
        let mut size = payload.len();
        loop {
            let byte = (size & 0x7f) as u8;
            size >>= 7;
            if size == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out.extend_from_slice(payload);
        out
    }

    fn custom(name: &str, body: &[u8]) -> Vec<u8> {
        let mut payload = vec![name.len() as u8];
        payload.extend_from_slice(name.as_bytes());
        payload.extend_from_slice(body);
        section(0, &payload)
    }

    fn module(sections: &[Vec<u8>]) -> Vec<u8> {
        let mut out = WASM_HEADER.to_vec();
        for s in sections {
            out.extend_from_slice(s);
        }
        out
    }

    #[test]
    fn only_the_name_section_is_removed() {
        // A type section, a 200-byte name section (a two-byte size), and the
        // two custom sections wasm-bindgen reads.
        let types = section(1, &[0x01, 0x60, 0x00, 0x00]);
        let features = custom("target_features", b"\x01+\x0freference-types");
        let producers = custom("producers", b"\x00");
        let names = custom("name", &[0x42; 200]);
        let input = module(&[types.clone(), names, features.clone(), producers.clone()]);

        let output = without_custom_section(&input, "name").unwrap();

        assert_eq!(output, module(&[types, features, producers]));
    }

    #[test]
    fn a_module_without_a_name_section_is_unchanged() {
        let input = module(&[section(1, &[0x00]), custom("names", b"x")]);
        assert_eq!(without_custom_section(&input, "name").unwrap(), input);
    }

    #[test]
    fn a_truncated_or_foreign_file_is_refused() {
        let mut truncated = module(&[custom("name", &[0; 10])]);
        truncated.pop();
        assert!(without_custom_section(&truncated, "name").is_err());
        assert!(without_custom_section(b"\0asm\x0d\0\x01\0", "name").is_err());
        let overlong_name = module(&[section(0, &[0x05, b'n'])]);
        assert!(without_custom_section(&overlong_name, "name").is_err());
    }

    #[test]
    fn leb128_reads_multi_byte_values_and_refuses_overflow() {
        let mut at = 0;
        assert_eq!(
            read_u32_leb128(&[0xe5, 0x8e, 0x26], &mut at).unwrap(),
            624_485
        );
        assert_eq!(at, 3);
        let mut at = 0;
        let max = [0xff, 0xff, 0xff, 0xff, 0x0f];
        assert_eq!(read_u32_leb128(&max, &mut at).unwrap(), u32::MAX);
        assert!(read_u32_leb128(&[0xff, 0xff, 0xff, 0xff, 0x1f], &mut 0).is_err());
        assert!(read_u32_leb128(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00], &mut 0).is_err());
    }

    #[test]
    fn every_worker_build_runs_with_the_strip_override() {
        let cmd = worker_build_command(Path::new("/tmp/crate"));
        let cmd = cmd.as_std();
        assert_eq!(cmd.get_program(), "worker-build");
        assert!(cmd.get_envs().any(|(key, value)| {
            key == WORKER_BUILD_STRIP_ENV.0 && value == Some(WORKER_BUILD_STRIP_ENV.1.as_ref())
        }));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            ["--no-default-features", "--features", "target-cloudflare"]
        );
    }
}
