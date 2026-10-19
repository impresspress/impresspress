//! The generated Worker entry module (`helpers::cloudflare::entry`), run in
//! Node against worker-build's own shim.
//!
//! `support/cloudflare_entry/worker-build-0.7.5-shim.js` is
//! `worker-build-0.7.5/src/js/shim.js` verbatim (Apache-2.0, Cloudflare; the
//! licence is `support/cloudflare_entry/LICENSE-worker-build`, the provenance
//! `support/cloudflare_entry/README.md`), the version
//! `build::WORKER_BUILD_VERSION` pins. The test fills its two
//! placeholders the way worker-build 0.7.5's `generate_handlers` does for this
//! Worker's two exports, and points its imports at stand-ins: `cloudflare:workers`
//! at a `WorkerEntrypoint` that stores `ctx`/`env`, and `./index.js` at a module
//! emulating the Rust side (`support/cloudflare_entry/index.js`). The shim's
//! own logic — the class and instance proxies, `checkReinitialize`, the
//! `RuntimeError` catch — is the real one.
//!
//! What it pins:
//! - with the trace var off, a Worker entered through `entry.mjs` behaves
//!   exactly like one entered through the shim, a trap and the reset after it
//!   included, and nothing creates the shared state;
//! - with it on, every request gets one `ip-entry` line naming the instance
//!   that serves it: the request after the reset already carries the new wasm
//!   id and the next generation. Its `wbi` field is `-`: worker-build 0.7.5's
//!   shim keeps its instance counter module-local, and nothing creates
//!   worker 0.8's `__worker_init_state` global. When that global is present,
//!   the line carries its counter instead (`wbi=1`);
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

/// The `$HANDLERS` worker-build 0.7.5 generates for a Worker exporting
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

/// A consumer repo laid out as `impresspress build` leaves it: the shim at
/// `build/worker/shim.mjs`, the entry module in
/// `target/impresspress-cloudflare/`. Returns the entry module's directory.
fn stage(repo: &Path) -> std::path::PathBuf {
    assert_eq!(WORKER_BUILD_VERSION, "0.7.5", "re-vendor the shim fixture");
    let worker = repo.join("build/worker");
    fs::create_dir_all(&worker).unwrap();
    let shim = fs::read_to_string(Path::new(SUPPORT).join("worker-build-0.7.5-shim.js")).unwrap();
    let shim = replace_once(&shim, "$HANDLERS", HANDLERS);
    // worker-build's non-unwind build, which is this workspace's (`panic = "abort"`).
    let shim = replace_once(&shim, "$PANIC_CRITICAL_ERROR", "criticalError = true;");
    let shim = replace_once(&shim, "\"cloudflare:workers\"", "\"./workers-stub.mjs\"");
    fs::write(worker.join("shim.mjs"), shim).unwrap();
    for file in ["index.js", "workers-stub.mjs"] {
        fs::copy(Path::new(SUPPORT).join(file), worker.join(file)).unwrap();
    }

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

    let through_shim = serve(&out, "../../build/worker/shim.mjs", "off");
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
            "ip-entry <isolate> wasm-1 1 1 1024 wbi=-",
            "response 200 https://example.test/a sameEnv=true",
            "state seq=1",
            "ip-entry <isolate> wasm-1 1 2 1024 wbi=-",
            "Critical RuntimeError: unreachable",
            "threw RuntimeError",
            "state seq=2",
            // The reset runs in the construct trap, before the override:
            // the next ip-entry already names the new instance.
            "Reinitializing Wasm application",
            "ip-entry <isolate> wasm-2 2 3 1024 wbi=-",
            "response 200 https://example.test/c sameEnv=true",
            "state seq=3",
            "ip-entry <isolate> wasm-2 2 4 1024 wbi=-",
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
            "ip-entry <isolate> wasm-1 1 1 1024 wbi=-",
            "response 200 https://example.test/a sameEnv=true",
            "state seq=1",
            // The `finally` still logs, with dashes for what the probe would
            // have said; the error then reaches the shim's catch.
            "ip-entry <isolate> - - 2 - wbi=-",
            "Critical RuntimeError: memory access out of bounds",
            "threw RuntimeError",
            "state seq=2",
            "Reinitializing Wasm application",
            "ip-entry <isolate> wasm-2 2 3 1024 wbi=-",
            "response 200 https://example.test/c sameEnv=true",
            "state seq=3",
            "ip-entry <isolate> wasm-2 2 4 1024 wbi=-",
            "response 200 https://example.test/d sameEnv=true",
            "state seq=4",
        ]
    );
}

/// With worker 0.8's `__worker_init_state` present, the counter the probe
/// reads reaches the line through the entry module as `wbi=<n>`. The 0.7.5
/// shim never moves that global, so it stays at the preset 1 across the reset.
#[test]
fn traced_with_worker_0_8s_init_state_the_line_carries_its_instance_counter() {
    let repo = tempfile::tempdir().unwrap();
    let out = stage(repo.path());

    let lines = serve(&out, "./entry.mjs", "on-0.8");

    let entries: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("ip-entry "))
        .collect();
    assert_eq!(entries.len(), 4, "one ip-entry per request: {lines:?}");
    for line in entries {
        assert!(line.ends_with(" 1024 wbi=1"), "{line}");
    }
}
