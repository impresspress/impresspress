//! Test doubles for the [`RuntimeControl`], [`ShellSource`] and
//! [`seed::SeedFetch`] seams, plus the request helpers every `dev_*.rs`
//! integration file needs to reach the block at all.
//!
//! Exposed under `test-support` (as well as `cfg(test)`) so the `tests/`
//! integration crates and downstream consumers can drive the dev block without
//! a real runtime behind it.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use wafer_run::{AuthLevel, BlockEndpoint, BlockInfo, OutputStream};

use super::{
    blobs,
    control::{DynamicBlockSpec, RuntimeControl, ShellSource, ValidationFailure, ValidationStage},
    paths, seed,
    seed::{SeedFetch, SeedFile},
};
use crate::test_support::{admin_msg, output_json, Session, TestContext};

// ---------------------------------------------------------------------------
// Reaching the block
// ---------------------------------------------------------------------------

/// A [`TestContext::with_dev`] fixture that can also sign people in — auth's
/// schema and [`TestContext::with_sign_in_added`] on top — for a test about
/// who may reach `/b/dev`, which sends real credentials through
/// [`TestContext::request`].
pub async fn dev_with_accounts(control: Arc<dyn RuntimeControl>) -> TestContext {
    TestContext::with_admin()
        .await
        .with_auth_added()
        .await
        .with_dev_added(control)
        .await
        .with_sign_in_added()
}

/// The password every account [`signed_in_as`] seeds signs in with.
const ACCOUNT_PASSWORD: &str = "correct-horse-battery-staple";

/// Seed an account with `role` on a [`dev_with_accounts`] fixture and sign it
/// in through the login route.
///
/// Not `admin@example.com`: the workspace guide prints that address as the
/// local credentials, so a page assertion on it must not be satisfied by the
/// signed-in operator's own email.
pub async fn signed_in_as(ctx: &TestContext, role: &str) -> Session {
    let email = format!("{role}-account@example.com");
    ctx.seed_account(&email, ACCOUNT_PASSWORD, role).await;
    ctx.sign_in(&email, ACCOUNT_PASSWORD).await
}

/// `POST` a JSON body to a `/b/dev` route as an admin, through the router,
/// the admin taken as already resolved ([`TestContext::dispatch_resolved`]).
///
/// Every `/b/dev` API is admin-only and takes its argument as a JSON body, so
/// this is how each of the six `dev_*.rs` integration files opened every
/// request it makes. What these requests exercise is the block behind the
/// gate; who may pass the gate is asked with [`dev_with_accounts`].
pub async fn dev_post(ctx: &TestContext, path: &str, body: serde_json::Value) -> OutputStream {
    ctx.dispatch_resolved_json(admin_msg("create", path), &body)
        .await
}

/// `GET` a `/b/dev` route as an admin, through the router — the admin taken
/// as already resolved, as [`dev_post`].
pub async fn dev_get(ctx: &TestContext, path: &str) -> OutputStream {
    ctx.dispatch_resolved(admin_msg("retrieve", path)).await
}

/// The `/b/dev/api/status` projection — the generation, the block set and the
/// store sizes, as the page reads them.
pub async fn dev_status(ctx: &TestContext) -> serde_json::Value {
    output_json(dev_get(ctx, "/b/dev/api/status").await).await
}

/// The `BlockInfo` a well-behaved `hello` guest reports: one public `GET` on
/// its own prefix and nothing else.
///
/// The name is a parameter because half the rules under test are about the
/// name (a guest outside its namespace, a hyphen, a reserved prefix); the
/// endpoint is not, because no rule reads it.
pub fn hello_info(name: &str) -> BlockInfo {
    BlockInfo::new(name, "0.1.0", "http-handler@v1", "hello").endpoints(vec![BlockEndpoint::get(
        "/b/hello/",
    )
    .auth(AuthLevel::Public)
    .summary("hello")])
}

/// A [`RuntimeControl`] that records what it was asked to do instead of
/// building anything.
///
/// `rebuild` appends the requested block set and bumps the generation counter,
/// so a test can assert both *what* was activated and *how many times* — the
/// two things the real control plane is responsible for and the page keys on.
/// `inspect` reports whatever `BlockInfo` the test set, and `probe` records
/// the spec it was handed, so a test can assert that the guest was executed
/// under the ACCEPTED capabilities rather than its own declaration.
///
/// It also models the *retained runtime* half of design §7.3: `rebuild` keeps
/// the block set it swapped out and `restore_previous` puts it back without
/// rebuilding. That is what makes "the swap was undone, not redone" an
/// assertion a test can make — [`Self::restores`] against [`Self::rebuilds`],
/// with [`Self::live_blocks`] for what is actually serving.
pub struct FakeControl {
    /// Every block set handed to `rebuild`, oldest first.
    pub rebuilt: Mutex<Vec<Vec<DynamicBlockSpec>>>,
    /// The `BlockInfo` a successful `inspect` reports, set by
    /// [`Self::set_validated_info`].
    ///
    /// This is what makes the static rules testable without wasmi: the
    /// executable half of validation is the seam, so a test that wants to
    /// exercise "a guest that declares X" states X here rather than compiling
    /// a guest that declares it.
    validated_info: Mutex<Option<wafer_run::BlockInfo>>,
    /// Set by [`Self::fail_next_inspect`]: the refusal the next `inspect`
    /// answers with, consumed by that call.
    ///
    /// One-shot for the same reason [`Self::fail_rebuild`] is: the
    /// interesting request is usually the one *after* a refusal.
    fail_inspect: Mutex<Option<ValidationFailure>>,
    /// Set by [`Self::fail_next_probe`]: the refusal the next `probe` answers
    /// with, consumed by that call.
    fail_probe: Mutex<Option<ValidationFailure>>,
    /// Every spec handed to `probe`, oldest first.
    ///
    /// Recorded rather than merely counted because the *capabilities* a probe
    /// runs under are the point of the inspect/probe split: a test asserts
    /// that they are the ACCEPTED set, not the guest's raw declaration.
    probed: Mutex<Vec<DynamicBlockSpec>>,
    /// Bumped by every `inspect` call, refusals included. A test that asserts
    /// a request was refused *before* the module was loaded reads this.
    inspections: AtomicU64,
    /// Set by [`Self::fail_next_rebuild`]: the message the next `rebuild`
    /// refuses with, consumed by that call.
    ///
    /// One-shot rather than sticky because the interesting activation is the
    /// one *after* a failure — the previous generation has to still be live,
    /// and a control that refused forever could not show that.
    fail_rebuild: Mutex<Option<String>>,
    /// Set by [`Self::gate_next_rebuild`]: the next `rebuild` parks on this
    /// until the test's sender fires.
    ///
    /// The activation queue's coalescing only exists while an activation is
    /// *in flight*, and nothing else in the fixture blocks: every database
    /// and storage call completes without yielding, so two `request` futures
    /// polled together would run strictly one after the other and the queue
    /// would never be contended. This is the one place a test can hold an
    /// activation open on purpose.
    gate_rebuild: Mutex<Option<futures::channel::oneshot::Receiver<()>>>,
    /// The block set the runtime is serving right now, or `None` before the
    /// first rebuild.
    live: Mutex<Option<Vec<DynamicBlockSpec>>>,
    /// What the last successful `rebuild` swapped out, retained for
    /// `restore_previous` — the fixture's stand-in for the browser control's
    /// retained `Rc<Wafer>`.
    ///
    /// Two layers of `Option` and both are load-bearing: the outer says
    /// whether there is anything to restore *at all* (a `restore_previous`
    /// with nothing retained is an error, exactly as it is in the browser),
    /// the inner is the block set that runtime was serving, which is `None`
    /// before the first rebuild.
    retained: Mutex<Option<Option<Vec<DynamicBlockSpec>>>>,
    /// Bumped by every `restore_previous` that put a runtime back.
    restores: AtomicU64,
    /// Set by [`Self::fail_next_restore`]: the message the next
    /// `restore_previous` refuses with, consumed by that call.
    fail_restore: Mutex<Option<String>>,
    /// Bumped by every successful `rebuild`.
    generation: AtomicU64,
}

impl FakeControl {
    /// A control that accepts everything and starts at generation 0.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            rebuilt: Mutex::new(Vec::new()),
            validated_info: Mutex::new(None),
            fail_inspect: Mutex::new(None),
            fail_probe: Mutex::new(None),
            probed: Mutex::new(Vec::new()),
            inspections: AtomicU64::new(0),
            fail_rebuild: Mutex::new(None),
            gate_rebuild: Mutex::new(None),
            live: Mutex::new(None),
            retained: Mutex::new(None),
            restores: AtomicU64::new(0),
            fail_restore: Mutex::new(None),
            generation: AtomicU64::new(0),
        })
    }

    /// A control whose next `probe` refuses at `stage`.
    pub fn rejecting(stage: ValidationStage, message: &str) -> Arc<Self> {
        let control = Self::new();
        control.fail_next_probe(stage, message);
        control
    }

    /// Make every successful `inspect` report `info`.
    pub fn set_validated_info(&self, info: wafer_run::BlockInfo) {
        *self.validated_info.lock().expect("validated_info mutex") = Some(info);
    }

    /// Make the next `inspect` refuse at `stage` with `message`.
    pub fn fail_next_inspect(&self, stage: ValidationStage, message: &str) {
        *self.fail_inspect.lock().expect("fail_inspect mutex") =
            Some(ValidationFailure::new(stage, message));
    }

    /// Make the next `probe` refuse at `stage` with `message`.
    pub fn fail_next_probe(&self, stage: ValidationStage, message: &str) {
        *self.fail_probe.lock().expect("fail_probe mutex") =
            Some(ValidationFailure::new(stage, message));
    }

    /// How many times `inspect` has been called.
    pub fn inspections(&self) -> u64 {
        self.inspections.load(Ordering::SeqCst)
    }

    /// The specs handed to `probe`, oldest first.
    pub fn probes(&self) -> Vec<DynamicBlockSpec> {
        self.probed.lock().expect("probed mutex").clone()
    }

    /// The block sets handed to `rebuild`, oldest first.
    pub fn rebuilds(&self) -> Vec<Vec<DynamicBlockSpec>> {
        self.rebuilt.lock().expect("rebuilt mutex").clone()
    }

    /// Make the next `rebuild` refuse with `message`, recording nothing.
    ///
    /// The mirror of [`Self::rejecting`] for the other half of the seam: that
    /// one refuses a guest at validation, this one refuses the runtime swap
    /// itself — the failed activation has to survive with the previous
    /// generation still live.
    pub fn fail_next_rebuild(&self, message: &str) {
        *self.fail_rebuild.lock().expect("fail_rebuild mutex") = Some(message.to_string());
    }

    /// Make the next `restore_previous` refuse with `message`, restoring
    /// nothing.
    ///
    /// The retained runtime is *kept*, so the refusal is the swap failing
    /// rather than there being nothing to swap back.
    pub fn fail_next_restore(&self, message: &str) {
        *self.fail_restore.lock().expect("fail_restore mutex") = Some(message.to_string());
    }

    /// How many times `restore_previous` has put a runtime back.
    ///
    /// The counter that separates §7.3's restore from a second `rebuild`: a
    /// test asserting the swap was *undone* rather than *redone* reads this
    /// against [`Self::rebuilds`].
    pub fn restores(&self) -> u64 {
        self.restores.load(Ordering::SeqCst)
    }

    /// The block set the runtime is serving now — what a `rebuild` last
    /// installed, or what a `restore_previous` put back.
    pub fn live_blocks(&self) -> Option<Vec<DynamicBlockSpec>> {
        self.live.lock().expect("live mutex").clone()
    }

    /// Hold the next `rebuild` open until the returned sender fires (or is
    /// dropped), so a test can observe what happens *during* an activation.
    pub fn gate_next_rebuild(&self) -> futures::channel::oneshot::Sender<()> {
        let (tx, rx) = futures::channel::oneshot::channel();
        *self.gate_rebuild.lock().expect("gate_rebuild mutex") = Some(rx);
        tx
    }
}

#[wafer_block::wafer_async_trait]
impl RuntimeControl for FakeControl {
    async fn inspect(&self, _artifact: &[u8]) -> Result<wafer_run::BlockInfo, ValidationFailure> {
        self.inspections.fetch_add(1, Ordering::SeqCst);
        if let Some(failure) = self.fail_inspect.lock().expect("fail_inspect mutex").take() {
            return Err(failure);
        }
        Ok(self
            .validated_info
            .lock()
            .expect("validated_info mutex")
            .clone()
            .unwrap_or_else(|| {
                wafer_run::BlockInfo::new(
                    "site/fake",
                    "0.0.0",
                    "http-handler@v1",
                    "fake inspected guest",
                )
            }))
    }

    async fn probe(
        &self,
        spec: &DynamicBlockSpec,
        _artifact: &[u8],
    ) -> Result<(), ValidationFailure> {
        if let Some(failure) = self.fail_probe.lock().expect("fail_probe mutex").take() {
            return Err(failure);
        }
        self.probed.lock().expect("probed mutex").push(spec.clone());
        Ok(())
    }

    async fn rebuild(&self, blocks: &[DynamicBlockSpec]) -> Result<(), String> {
        // Taken out of the lock before the await: a `MutexGuard` is not held
        // across a suspension point anywhere in this fixture, for the same
        // reason the activation queue never holds one.
        let gate = self.gate_rebuild.lock().expect("gate_rebuild mutex").take();
        if let Some(gate) = gate {
            // A dropped sender releases the gate too: a test that forgets to
            // fire it fails on its assertions, not on a hang.
            let _ = gate.await;
        }
        if let Some(message) = self.fail_rebuild.lock().expect("fail_rebuild mutex").take() {
            return Err(message);
        }
        self.rebuilt
            .lock()
            .expect("rebuilt mutex")
            .push(blocks.to_vec());
        // Retain what was live, then install the new set — the fixture's
        // model of `replace_wafer` handing back the runtime it swapped out.
        let previous = self
            .live
            .lock()
            .expect("live mutex")
            .replace(blocks.to_vec());
        *self.retained.lock().expect("retained mutex") = Some(previous);
        self.generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn restore_previous(&self) -> Result<(), String> {
        if let Some(message) = self.fail_restore.lock().expect("fail_restore mutex").take() {
            return Err(message);
        }
        let Some(previous) = self.retained.lock().expect("retained mutex").take() else {
            return Err("no retained runtime to restore".to_string());
        };
        *self.live.lock().expect("live mutex") = previous;
        self.restores.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn runtime_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// The seed-bundle fetch seam
// ---------------------------------------------------------------------------

/// A [`SeedFetch`] over an in-memory `url -> bytes` map — the same contract
/// the browser's `fetch` wrapper (`impresspress-web`'s `SwFetch`) satisfies,
/// driveable here without a browser. Shared by `tests/dev_seed.rs` and
/// `tests/dev_data_snapshot.rs` rather than defined twice.
#[derive(Default)]
pub struct MapFetch {
    files: BTreeMap<String, Vec<u8>>,
}

impl MapFetch {
    /// Add one `url -> bytes` entry, chainable for building a bundle
    /// fixture inline.
    pub fn with(mut self, url: &str, bytes: &[u8]) -> Self {
        self.files.insert(url.to_string(), bytes.to_vec());
        self
    }
}

impl SeedFetch for MapFetch {
    fn get<'a>(&'a self, url: &'a str) -> seed::FetchFuture<'a> {
        Box::pin(async move {
            self.files
                .get(url)
                .cloned()
                .ok_or_else(|| format!("{url}: not in the bundle"))
        })
    }
}

/// A [`SeedFile`] for `path`, with `sha256`/`size`/`content_type` derived
/// from `bytes` the same way a real exporter would.
///
/// [`seed::import`] checks all three against what the *path* is served as,
/// so a fixture built any other way would fail verification for reasons
/// unrelated to what a test using this means to exercise.
pub fn seed_file(path: &str, bytes: &[u8]) -> SeedFile {
    SeedFile {
        path: path.to_string(),
        sha256: blobs::sha256_hex(bytes),
        size: bytes.len() as u64,
        content_type: paths::content_type_for(path).to_string(),
    }
}

// ---------------------------------------------------------------------------
// The static-shell seam
// ---------------------------------------------------------------------------

/// A [`ShellSource`] over an in-memory `path -> bytes` map — the same contract
/// the browser's `/asset-manifest.json` reader (`impresspress-web`'s
/// `BrowserShellSource`) satisfies, driveable here without a service worker.
///
/// [`Self::new`] carries a plausible shell: the three rendered templates, the
/// hashed wasm-pack pair, `vendor/sql-wasm.wasm` and the asset manifest
/// itself. Its `sw.js` is a dev bundle's — it declares
/// `const DEV_ENABLED = true;` and reads that constant in the isolation-header
/// passthrough, exactly as `impresspress-bundle`'s template renders it — and
/// its bypass list carries the compiler prefix the way a deployment that
/// ships the in-browser toolchain does. So a test can assert BOTH edits the
/// export makes to a shell file: the dev flag turned off, and the bypass for
/// a compiler tree the export does not copy removed.
pub struct FakeShell {
    files: BTreeMap<String, Vec<u8>>,
    /// Set by [`Self::failing_to_list`]: what `list` refuses with.
    list_failure: Option<String>,
    /// How many files `fetch` has been asked for — read by [`Self::fetches`].
    fetched: std::sync::atomic::AtomicUsize,
}

/// The `sw.js` [`FakeShell::new`] serves — a dev bundle's, trimmed to the
/// lines the export acts on (the `DEV_ENABLED` declaration and the compiler's
/// bypass clause) plus the ones it must leave exactly alone (the constant's
/// two readers and the `/seed/` bypass, without which an exported folder
/// could never import the seed shipped beside it).
pub const FAKE_SW_JS: &str = "const DEV_ENABLED = true;\n\
     await initialize({ dev: DEV_ENABLED });\n\
     if (DEV_ENABLED && url.pathname !== '/sw.js') { passthrough(); }\n\
     if (url.pathname.startsWith('/sql-') \
     || url.pathname.startsWith('/__impresspress_dev/compiler/') \
     || url.pathname.startsWith('/seed/')) { return; }\n";

impl Default for FakeShell {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeShell {
    /// A shell with the seven files a real bundle's `files` listing carries.
    pub fn new() -> Self {
        let mut files = BTreeMap::new();
        files.insert("index.html".to_string(), b"<!doctype html>".to_vec());
        files.insert("sw.js".to_string(), FAKE_SW_JS.as_bytes().to_vec());
        files.insert("loader.js".to_string(), b"// loader".to_vec());
        files.insert(
            "impresspress_web-abc123.js".to_string(),
            b"// glue".to_vec(),
        );
        files.insert(
            "impresspress_web_bg-abc123.wasm".to_string(),
            b"\0asm\x01\0\0\0".to_vec(),
        );
        files.insert(
            "vendor/sql-wasm.wasm".to_string(),
            b"\0asm\x01\0\0\0".to_vec(),
        );
        files.insert(
            "asset-manifest.json".to_string(),
            br#"{"buildId":"abc123","assets":{},"files":[]}"#.to_vec(),
        );
        Self {
            files,
            list_failure: None,
            fetched: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// How many shell files have been fetched so far: what an export that
    /// was refused before it read the runtime has to leave at zero.
    pub fn fetches(&self) -> usize {
        self.fetched.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Add or replace one file, chainable.
    pub fn with(mut self, path: &str, bytes: &[u8]) -> Self {
        self.files.insert(path.to_string(), bytes.to_vec());
        self
    }

    /// Drop one file, chainable — for the cases where what matters is what
    /// the shell does NOT carry.
    pub fn without(mut self, path: &str) -> Self {
        self.files.remove(path);
        self
    }

    /// A shell whose `list` refuses: what a deployment whose
    /// `/asset-manifest.json` is missing, unparseable or `files`-less looks
    /// like. An export must fail loudly rather than produce a folder with no
    /// runtime in it.
    pub fn failing_to_list(mut self, message: &str) -> Self {
        self.list_failure = Some(message.to_string());
        self
    }
}

#[wafer_block::wafer_async_trait]
impl ShellSource for FakeShell {
    async fn list(&self) -> Result<Vec<String>, String> {
        if let Some(message) = &self.list_failure {
            return Err(message.clone());
        }
        Ok(self.files.keys().cloned().collect())
    }

    async fn fetch(&self, path: &str) -> Result<Vec<u8>, String> {
        self.fetched
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("{path}: not served by this shell"))
    }
}
