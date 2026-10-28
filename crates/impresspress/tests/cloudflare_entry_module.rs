//! The generated Worker entry module (`helpers::cloudflare::entry`), run in
//! Node against worker-build's own shim.
//!
//! `support/cloudflare_entry/worker-build-0.8.7-shim.js` is
//! `worker-build-0.8.7/src/js/shim.js` verbatim (Apache-2.0, Cloudflare; the
//! licence is `support/cloudflare_entry/LICENSE-worker-build`, the provenance
//! `support/cloudflare_entry/README.md`), the version
//! `build::WORKER_BUILD_VERSION` pins. The test fills its placeholder the way
//! worker-build 0.8.7's `generate_handlers` does for this Worker's two
//! exports, and points its imports at stand-ins: `cloudflare:workers` at a
//! `WorkerEntrypoint` that stores `ctx`/`env`, and the wasm-bindgen module at
//! one emulating the Rust side (`support/cloudflare_entry/index.js`), including
//! worker 0.8's `__worker_init_state`. The shim's own logic — the class and
//! instance proxies, `checkReinitialize`, the `RuntimeError` catch — is the
//! real one.
//!
//! What it pins:
//! - with the trace var off, a Worker entered through `entry.mjs` behaves
//!   exactly like one entered through the shim, a trap and the reset after it
//!   included, and nothing creates the shared state;
//! - with it on, every request gets one `ip-entry` line naming the instance
//!   that serves it: the request after the reset already carries the new wasm
//!   id, the next generation, and worker 0.8's instance counter (`wbi`) up by
//!   2, 1 from the reset's start function and 1 from the shim;
//! - a probe that traps still leaves a dashed `ip-entry` line, and its error
//!   takes the path of a trap in `fetch`: `Critical`, the throw, then the
//!   reset on the next request.

use std::{fs, path::Path, process::Command};

use impresspress::cli::helpers::cloudflare::{
    build::WORKER_BUILD_VERSION, entry::stage_entry_module,
};

const SUPPORT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/support/cloudflare_entry"
);

/// The `$HANDLERS` worker-build 0.8.7 generates for a Worker exporting
/// `fetch` and `__impresspressExecutorProbe`, without `RUN_TO_COMPLETION`
/// (`generate_handlers` in `worker-build/src/main.rs`).
const HANDLERS: &str = "Entrypoint.prototype.fetch = function fetch (arg) {
  return exports.fetch.call(this, arg, this.env, this.ctx);
}
Entrypoint.prototype.__impresspressExecutorProbe = exports.__impresspressExecutorProbe;
";

fn replace_once(source: &str, from: &str, to: &str) -> String {
    assert_eq!(
        source.matches(from).count(),
        1,
        "the pinned shim has changed shape: {from:?}"
    );
    source.replace(from, to)
}

/// The name the stand-in wasm-bindgen module is staged under. worker-build
/// bundles its shim and wasm-bindgen's `index.js` into one `build/index.js`
/// with esbuild; the test has no bundler, so the shim is written as
/// `build/index.js` and its `./index.js` import is pointed at this file.
const WASM_BINDGEN_MODULE: &str = "wasm-bindgen.js";

/// A consumer repo laid out as `impresspress build` leaves it: the shim at
/// `build/index.js`, the entry module in `target/impresspress-cloudflare/`.
/// Returns the entry module's directory.
fn stage(repo: &Path) -> std::path::PathBuf {
    assert_eq!(WORKER_BUILD_VERSION, "0.8.7", "re-vendor the shim fixture");
    let build = repo.join("build");
    fs::create_dir_all(&build).unwrap();
    let shim = fs::read_to_string(Path::new(SUPPORT).join("worker-build-0.8.7-shim.js")).unwrap();
    let shim = replace_once(&shim, "$HANDLERS", HANDLERS);
    let shim = replace_once(&shim, "\"cloudflare:workers\"", "\"./workers-stub.mjs\"");
    let shim = replace_once(
        &shim,
        "\"./index.js\"",
        &format!("\"./{WASM_BINDGEN_MODULE}\""),
    );
    fs::write(build.join("index.js"), shim).unwrap();
    fs::copy(
        Path::new(SUPPORT).join("index.js"),
        build.join(WASM_BINDGEN_MODULE),
    )
    .unwrap();
    fs::copy(
        Path::new(SUPPORT).join("workers-stub.mjs"),
        build.join("workers-stub.mjs"),
    )
    .unwrap();

    let out = repo.join("target/impresspress-cloudflare");
    fs::create_dir_all(&out).unwrap();
    stage_entry_module(&out).unwrap();
    fs::copy(
        Path::new(SUPPORT).join("driver.mjs"),
        out.join("driver.mjs"),
    )
    .unwrap();
    out
}

/// Run the driver against `module` (relative to the entry module's
/// directory) with the trace `mode`, in a fresh Node process, and return the
/// lines it recorded. Fails, rather than skips, without `node`.
fn serve(out: &Path, module: &str, mode: &str) -> Vec<String> {
    let output = Command::new("node")
        .current_dir(out)
        .args(["driver.mjs", module, mode])
        .output()
        .expect("`node` must be on PATH: the entry module is checked by running it");
    assert!(
        output.status.success(),
        "node driver.mjs {module} {mode} failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn untraced_the_entry_module_behaves_exactly_like_the_shim() {
    let repo = tempfile::tempdir().unwrap();
    let out = stage(repo.path());

    let through_shim = serve(&out, "../../build/index.js", "off");
    let through_entry = serve(&out, "./entry.mjs", "off");

    assert_eq!(through_entry, through_shim);
    // The scenario did exercise the shim's trap handling and reset.
    assert_eq!(
        through_shim,
        [
            "response 200 https://example.test/a sameEnv=true",
            "state absent",
            "Critical RuntimeError: unreachable",
            "threw RuntimeError",
            "state absent",
            "Reinitializing Wasm application",
            "response 200 https://example.test/c sameEnv=true",
            "state absent",
            "response 200 https://example.test/d sameEnv=true",
            "state absent",
        ]
    );
}

#[test]
fn traced_each_request_logs_the_instance_that_serves_it() {
    let repo = tempfile::tempdir().unwrap();
    let out = stage(repo.path());

    let lines = serve(&out, "./entry.mjs", "on");

    let isolate = lines[0]
        .strip_prefix("ip-entry ")
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_else(|| panic!("the first line is an ip-entry: {lines:?}"))
        .to_string();
    assert_eq!(isolate.len(), 32, "a random isolate id: {isolate}");
    let lines: Vec<String> = lines
        .iter()
        .map(|line| line.replace(&isolate, "<isolate>"))
        .collect();
    assert_eq!(
        lines,
        [
            "ip-entry <isolate> wasm-1 1 1 1024 wbi=1",
            "response 200 https://example.test/a sameEnv=true",
            "state seq=1",
            "ip-entry <isolate> wasm-1 1 2 1024 wbi=1",
            "Critical RuntimeError: unreachable",
            "threw RuntimeError",
            "state seq=2",
            // The reset runs in the construct trap, before the override:
            // the next ip-entry already names the new instance, and `wbi`
            // has moved by 2.
            "Reinitializing Wasm application",
            "ip-entry <isolate> wasm-2 2 3 1024 wbi=3",
            "response 200 https://example.test/c sameEnv=true",
            "state seq=3",
            "ip-entry <isolate> wasm-2 2 4 1024 wbi=3",
            "response 200 https://example.test/d sameEnv=true",
            "state seq=4",
        ]
    );
}

#[test]
fn a_probe_that_traps_leaves_a_dashed_line_and_resets_like_a_fetch_trap() {
    let repo = tempfile::tempdir().unwrap();
    let out = stage(repo.path());

    let lines = serve(&out, "./entry.mjs", "probe-trap");

    let isolate = lines[0]
        .strip_prefix("ip-entry ")
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_else(|| panic!("the first line is an ip-entry: {lines:?}"))
        .to_string();
    let lines: Vec<String> = lines
        .iter()
        .map(|line| line.replace(&isolate, "<isolate>"))
        .collect();
    assert_eq!(
        lines,
        [
            "ip-entry <isolate> wasm-1 1 1 1024 wbi=1",
            "response 200 https://example.test/a sameEnv=true",
            "state seq=1",
            // The `finally` still logs, with dashes for what the probe would
            // have said; the error then reaches the shim's catch.
            "ip-entry <isolate> - - 2 - wbi=-",
            "Critical RuntimeError: memory access out of bounds",
            "threw RuntimeError",
            "state seq=2",
            "Reinitializing Wasm application",
            "ip-entry <isolate> wasm-2 2 3 1024 wbi=3",
            "response 200 https://example.test/c sameEnv=true",
            "state seq=3",
            "ip-entry <isolate> wasm-2 2 4 1024 wbi=3",
            "response 200 https://example.test/d sameEnv=true",
            "state seq=4",
        ]
    );
}

/// The shim's reset path (path S) moves worker 0.8's instance counter by
/// exactly 2: `__wbg_reset_state` re-runs the start function, which adds 1,
/// and `checkReinitialize` adds its own 1. That +2 is how a trace tells the
/// shim's reset from wasm-bindgen's silent call-guard reset alone (+1).
#[test]
fn a_trap_moves_wbi_by_two_through_the_shims_reset() {
    let repo = tempfile::tempdir().unwrap();
    let out = stage(repo.path());

    let lines = serve(&out, "./entry.mjs", "on");

    let wbi: Vec<u32> = lines
        .iter()
        .filter(|line| line.starts_with("ip-entry "))
        .map(|line| {
            line.rsplit_once(" wbi=")
                .and_then(|(_, n)| n.parse().ok())
                .unwrap_or_else(|| panic!("no numeric wbi: {line}"))
        })
        .collect();
    let reset = lines
        .iter()
        .position(|line| line == "Reinitializing Wasm application")
        .unwrap_or_else(|| panic!("the trap was followed by the shim's reset: {lines:?}"));
    let entries_before_reset = lines[..reset]
        .iter()
        .filter(|line| line.starts_with("ip-entry "))
        .count();
    assert_eq!(entries_before_reset, 2, "{lines:?}");
    assert_eq!(wbi[1], wbi[0], "no reset between the first two requests");
    assert_eq!(wbi[2], wbi[1] + 2, "{lines:?}");
    assert_eq!(wbi[3], wbi[2], "no reset after the first");
}
