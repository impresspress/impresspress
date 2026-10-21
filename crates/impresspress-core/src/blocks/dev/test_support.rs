//! Test doubles for the [`RuntimeControl`], `WorkspaceControl`,
//! `ShellSource` (the export shell) and [`seed::SeedFetch`] seams, plus the request helpers
//! every `dev_*.rs` integration file needs to reach the block at all.
//!
//! Exposed under `test-support` (as well as `cfg(test)`) so the `tests/`
//! integration crates and downstream consumers can drive the dev block without
//! a real runtime behind it.
//!
//! Each double exists where its seam does. [`FakeControl`]'s
//! `RuntimeControl` half, [`MapFetch`], [`seed_file`], [`fake_bypass_rules`]
//! and [`hello_info`] serve the runtime half in every build; its
//! `WorkspaceControl` half, `FakeShell` and the request helpers that reach
//! the `/b/dev` routes exist only where those do (`block-dev`).

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

#[cfg(feature = "block-dev")]
use wafer_run::OutputStream;
use wafer_run::{AuthLevel, BlockEndpoint, BlockInfo};

#[cfg(feature = "block-dev")]
use super::control::{GenerationAnnouncement, ShellSource, WorkspaceControl};
use super::{
    blobs,
    bypass::BypassRules,
    control::{DynamicBlockSpec, RuntimeControl, ValidationFailure, ValidationStage},
    seed,
    seed::{SeedFetch, SeedFile},
};
#[cfg(feature = "block-dev")]
use crate::{
    blocks::auth::repo::users::InitialRole,
    test_support::{admin_msg, output_json, Session, TestContext},
};

// ---------------------------------------------------------------------------
// Reaching the block
// ---------------------------------------------------------------------------

/// A [`TestContext::with_dev`] fixture that can also sign people in — auth's
/// schema and [`TestContext::with_sign_in_added`] on top — for a test about
/// who may reach `/b/dev`, which sends real credentials through
/// [`TestContext::request`].
#[cfg(feature = "block-dev")]
pub async fn dev_with_accounts(control: Arc<dyn WorkspaceControl>) -> TestContext {
    TestContext::with_admin()
        .await
        .with_auth_added()
        .await
        .with_dev_added(control)
        .await
        .with_sign_in_added()
}

/// The password every account [`signed_in_as`] seeds signs in with.
#[cfg(feature = "block-dev")]
const ACCOUNT_PASSWORD: &str = "correct-horse-battery-staple";

/// Seed an account with `role` on a [`dev_with_accounts`] fixture and sign it
/// in through the login route.
///
/// Not the bootstrap admin's address: the workspace guide prints that one as
/// the local credentials, so a page assertion on it must not be satisfied by
/// the signed-in operator's own email.
#[cfg(feature = "block-dev")]
pub async fn signed_in_as(ctx: &TestContext, role: InitialRole) -> Session {
    let email = format!("{}-account@example.com", role.as_str());
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
#[cfg(feature = "block-dev")]
pub async fn dev_post(ctx: &TestContext, path: &str, body: serde_json::Value) -> OutputStream {
    ctx.dispatch_resolved_json(admin_msg("create", path), &body)
        .await
}

/// `GET` a `/b/dev` route as an admin, through the router — the admin taken
/// as already resolved, as [`dev_post`].
#[cfg(feature = "block-dev")]
pub async fn dev_get(ctx: &TestContext, path: &str) -> OutputStream {
    ctx.dispatch_resolved(admin_msg("retrieve", path)).await
}

/// The `/b/dev/api/status` projection — the generation, the block set and the
/// store sizes, as the page reads them.
#[cfg(feature = "block-dev")]
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

/// A [`RuntimeControl`] (and, under `block-dev`, `WorkspaceControl`) that
/// records what it was asked to do instead of building anything.
///
/// `rebuild` appends the requested block set (and, under `block-dev`, bumps
/// the generation counter `runtime_generation` reports), so a test can assert
/// both *what* was activated and *how many times* — the two things the real
/// control plane is responsible for and the page keys on. `inspect` reports
/// whatever `BlockInfo` the test set, and `probe` records the spec it was
/// handed, so a test can assert that the guest was executed under the
/// ACCEPTED capabilities rather than its own declaration.
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
    #[cfg(feature = "block-dev")]
    fail_probe: Mutex<Option<ValidationFailure>>,
    /// Every spec handed to `probe`, oldest first.
    ///
    /// Recorded rather than merely counted because the *capabilities* a probe
    /// runs under are the point of the inspect/probe split: a test asserts
    /// that they are the ACCEPTED set, not the guest's raw declaration.
    #[cfg(feature = "block-dev")]
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
    /// Bumped by every successful `rebuild`; what `runtime_generation`
    /// reports.
    #[cfg(feature = "block-dev")]
    generation: AtomicU64,
    /// Every generation `announce_active` was told about, oldest first — the
    /// fixture's stand-in for the messages the browser control posts to the
    /// page.
    #[cfg(feature = "block-dev")]
    announced: Mutex<Vec<GenerationAnnouncement>>,
}

impl FakeControl {
    /// A control that accepts everything and starts at generation 0.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            rebuilt: Mutex::new(Vec::new()),
            validated_info: Mutex::new(None),
            fail_inspect: Mutex::new(None),
            #[cfg(feature = "block-dev")]
            fail_probe: Mutex::new(None),
            #[cfg(feature = "block-dev")]
            probed: Mutex::new(Vec::new()),
            inspections: AtomicU64::new(0),
            fail_rebuild: Mutex::new(None),
            gate_rebuild: Mutex::new(None),
            live: Mutex::new(None),
            retained: Mutex::new(None),
            restores: AtomicU64::new(0),
            fail_restore: Mutex::new(None),
            #[cfg(feature = "block-dev")]
            generation: AtomicU64::new(0),
            #[cfg(feature = "block-dev")]
            announced: Mutex::new(Vec::new()),
        })
    }

    /// A control whose next `probe` refuses at `stage`.
    #[cfg(feature = "block-dev")]
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
    #[cfg(feature = "block-dev")]
    pub fn fail_next_probe(&self, stage: ValidationStage, message: &str) {
        *self.fail_probe.lock().expect("fail_probe mutex") =
            Some(ValidationFailure::new(stage, message));
    }

    /// How many times `inspect` has been called.
    pub fn inspections(&self) -> u64 {
        self.inspections.load(Ordering::SeqCst)
    }

    /// The specs handed to `probe`, oldest first.
    #[cfg(feature = "block-dev")]
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

    /// Every activation announced to the page, oldest first.
    #[cfg(feature = "block-dev")]
    pub fn announcements(&self) -> Vec<GenerationAnnouncement> {
        self.announced.lock().expect("announced mutex").clone()
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
        #[cfg(feature = "block-dev")]
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
}

#[cfg(feature = "block-dev")]
#[wafer_block::wafer_async_trait]
impl WorkspaceControl for FakeControl {
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

    fn runtime_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn announce_active(&self, generation: &GenerationAnnouncement) {
        self.announced
            .lock()
            .expect("announced mutex")
            .push(generation.clone());
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

/// A [`SeedFile`] for `path`, with `sha256`/`size` derived from `bytes` the
/// same way a real exporter would.
///
/// [`seed::import`] checks both against the bytes it fetches, so a fixture
/// built any other way would fail verification for reasons unrelated to
/// what a test using this means to exercise.
pub fn seed_file(path: &str, bytes: &[u8]) -> SeedFile {
    SeedFile {
        path: path.to_string(),
        sha256: blobs::sha256_hex(bytes),
        size: bytes.len() as u64,
    }
}

// ---------------------------------------------------------------------------
// The export-shell seam
// ---------------------------------------------------------------------------

/// A [`ShellSource`] over an in-memory `path -> body` map — the same contract
/// the browser's export-shell reader (`impresspress-web`'s
/// `BrowserShellSource`) satisfies, driveable here without a service worker.
///
/// It is only a fetch, like the seam: [`ShellListing`](super::ShellListing)
/// reads its `asset-manifest.json` and verifies every body, so a fixture whose
/// manifest disagrees with its files is refused exactly as a deployment's
/// would be.
///
/// [`Self::new`] serves a plausible site shell — the three rendered
/// templates, the hashed wasm-pack pair, `vendor/sql-wasm.wasm` and a
/// manifest listing them with their digests and [`fake_site_bypass_rules`] —
/// for the many tests that need the export to work and are not about the
/// shell. The export tests build theirs from what the real bundler renders
/// (`tests/dev_export.rs`'s `site_shell`, through [`Self::from_files`]).
#[cfg(feature = "block-dev")]
pub struct FakeShell {
    files: BTreeMap<String, Vec<u8>>,
    /// Every path `fetch` has been asked for, in order — read by
    /// [`Self::fetched`].
    fetched: Mutex<Vec<String>>,
}

/// The root the fake says it reads from: where a dev-sandbox deployment
/// carries its export shell (`impresspress-bundle`'s `EXPORT_SHELL_ROOT`,
/// restated because the bundler is not a dependency of this crate).
#[cfg(feature = "block-dev")]
pub const FAKE_SHELL_ROOT: &str = "/__impresspress_dev/export-shell/";

/// The title [`FakeShell::new`]'s boot page shows, wrapped the way the
/// bundler wraps one, for the export to replace.
#[cfg(feature = "block-dev")]
pub const FAKE_SHELL_TITLE: &str = "ImpressPress site";

/// The bypass rules a dev-sandbox worker hands `initialize()`: what
/// `impresspress-bundle` renders for the dev-sandbox deployment
/// (`examples/dev-sandbox/impresspress.toml` — a workspace bundle with the
/// compiler prefix), with `FakeShell`'s wasm-pack base name. The test
/// fixtures build `DevShared` with them in both builds; `tests/dev_export.rs`
/// checks them against the bundler's own.
pub fn fake_bypass_rules() -> BypassRules {
    BypassRules {
        exact: fake_exact_rules(),
        prefixes: [
            "/impresspress_web",
            "/snippets/",
            "/cdn-cgi/",
            "/__impresspress_dev/compiler/",
            "/seed/",
        ]
        .map(String::from)
        .to_vec(),
    }
}

/// The bypass rules [`FakeShell::new`]'s manifest states: what
/// `impresspress-bundle` renders for the export shell
/// (`examples/dev-sandbox/export-shell/impresspress.toml` — a site bundle,
/// no compiler), with the same base name. `tests/dev_export.rs` checks them
/// against the bundler's own too.
#[cfg(feature = "block-dev")]
pub fn fake_site_bypass_rules() -> BypassRules {
    BypassRules {
        exact: fake_exact_rules(),
        prefixes: ["/impresspress_web", "/snippets/", "/cdn-cgi/", "/seed/"]
            .map(String::from)
            .to_vec(),
    }
}

/// The exact rules every sandbox bundle renders, whatever its mode.
fn fake_exact_rules() -> Vec<String> {
    [
        "/sw.js",
        "/loader.js",
        "/manifest.json",
        "/asset-manifest.json",
        "/webllm-engine.js",
        "/embed-engine.js",
        "/t2i-engine.js",
        "/vendor/sql-wasm-esm.js",
        "/vendor/sql-wasm.wasm",
    ]
    .map(String::from)
    .to_vec()
}

#[cfg(feature = "block-dev")]
impl Default for FakeShell {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "block-dev")]
impl FakeShell {
    /// A site shell with the files a real bundle's listing carries, and a
    /// manifest that lists them.
    pub fn new() -> Self {
        Self::from_files([
            (
                "index.html".to_string(),
                format!(
                    "<!doctype html><title>{FAKE_SHELL_TITLE}</title>\
                     <h1><span data-app-title>{FAKE_SHELL_TITLE}</span></h1>"
                )
                .into_bytes(),
            ),
            ("sw.js".to_string(), b"// sw".to_vec()),
            ("loader.js".to_string(), b"// loader".to_vec()),
            (
                "impresspress_web-abc123.js".to_string(),
                b"// glue".to_vec(),
            ),
            (
                "impresspress_web_bg-abc123.wasm".to_string(),
                b"\0asm\x01\0\0\0".to_vec(),
            ),
            (
                "vendor/sql-wasm.wasm".to_string(),
                b"\0asm\x01\0\0\0".to_vec(),
            ),
        ])
        .relisted()
    }

    /// A shell serving exactly `files`, `asset-manifest.json` included if it
    /// is among them — what a directory the bundler rendered serves, read
    /// back file by file.
    pub fn from_files(files: impl IntoIterator<Item = (String, Vec<u8>)>) -> Self {
        Self {
            files: files.into_iter().collect(),
            fetched: Mutex::new(Vec::new()),
        }
    }

    /// The paths fetched so far, in order: what an export refused before it
    /// read the runtime has to have left at the manifest alone.
    pub fn fetched(&self) -> Vec<String> {
        self.fetched.lock().expect("fetched mutex").clone()
    }

    /// Serve `bytes` at `path` and list it — the manifest rewritten to name
    /// it with its digest, as the bundler would have for a bundle carrying
    /// the file. Chainable.
    pub fn with_listed(mut self, path: &str, bytes: &[u8]) -> Self {
        self.files.insert(path.to_string(), bytes.to_vec());
        self.relisted()
    }

    /// Serve `bytes` at `path` WITHOUT touching the manifest — what a static
    /// host does when it answers a path with something other than the file
    /// the shell lists (an SPA fallback's `index.html`, say), or when the
    /// manifest itself is replaced. Chainable.
    pub fn serving(mut self, path: &str, bytes: &[u8]) -> Self {
        self.files.insert(path.to_string(), bytes.to_vec());
        self
    }

    /// Serve nothing at `path`, manifest untouched — a listed file the host
    /// does not have. Chainable.
    pub fn without(mut self, path: &str) -> Self {
        self.files.remove(path);
        self
    }

    /// `asset-manifest.json` rewritten to list every file this serves, with
    /// its digest, and [`fake_site_bypass_rules`].
    fn relisted(mut self) -> Self {
        self.files.remove(super::control::SHELL_MANIFEST);
        let mut files: Vec<String> = self.files.keys().cloned().collect();
        let sha256: BTreeMap<&String, String> = self
            .files
            .iter()
            .map(|(path, bytes)| (path, blobs::sha256_hex(bytes)))
            .collect();
        files.push(super::control::SHELL_MANIFEST.to_string());
        files.sort();
        let manifest = serde_json::json!({
            "buildId": "abc123",
            "assets": {},
            "files": files,
            "bypass": fake_site_bypass_rules(),
            "sha256": sha256,
        });
        self.files.insert(
            super::control::SHELL_MANIFEST.to_string(),
            serde_json::to_vec_pretty(&manifest).expect("a manifest serializes"),
        );
        self
    }
}

#[cfg(feature = "block-dev")]
#[wafer_block::wafer_async_trait]
impl ShellSource for FakeShell {
    fn root(&self) -> &str {
        FAKE_SHELL_ROOT
    }

    async fn fetch(&self, path: &str) -> Result<Vec<u8>, String> {
        self.fetched
            .lock()
            .expect("fetched mutex")
            .push(path.to_string());
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("fetching {FAKE_SHELL_ROOT}{path}: HTTP 404"))
    }
}
