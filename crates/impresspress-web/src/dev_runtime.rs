//! The browser half of the development sandbox: wasmi validation, the runtime
//! rebuild, and what happens on a cold boot.
//!
//! `impresspress-core` decides *what* should be live; this decides *how*. The
//! seam is [`RuntimeControl`], and [`BrowserRuntimeControl`] is its only real
//! implementation: it compiles guest artifacts with wasmi, rebuilds a `Wafer`
//! through the shared [`RuntimeFactory`], and swaps it in.
//!
//! # The cycle, and where it is closed
//!
//! `DevShared` holds the control; the factory holds `DevShared`; the control
//! rebuilds through the factory. Something has to be late-bound, and it is the
//! control's factory handle ([`BrowserRuntimeControl::set_factory`]) — because
//! the *other* order does not work at all. The cold-start runtime has to
//! carry `/b/dev` (otherwise the page 404s until the first rebuild, and a
//! sandbox with no blocks would never rebuild), so `DevShared` must exist
//! before the first `factory.build`. A factory cannot be built before then;
//! a control can, and it needs its factory only at the first `rebuild`, which
//! cannot happen until boot has finished. See [`attach`].
//!
//! # Two contexts that are not the runtime's
//!
//! Both exist because their callers run outside any request:
//!
//! * [`DenyAllContext`] — what a guest is probed under. It grants nothing, on
//!   purpose (see [`BrowserRuntimeControl::probe`]).
//! * [`BootContext`] — what boot convergence and the seed import run under.
//!   It grants the dev block the same reach a request would, and is only ever
//!   constructed by [`install`].
//!
//! # Why no `unsafe impl Send`/`Sync`
//!
//! Every trait this module implements or hands values to (`RuntimeControl`,
//! `Context`, `Block`) is bounded on `wafer_run::MaybeSend + MaybeSync`, which
//! is unbounded on `wasm32`. `Rc`, `Cell` and `RefCell` therefore cross those
//! boundaries without an `unsafe` marker impl; the only cost is
//! `clippy::arc_with_non_send_sync`, which the crate-level
//! `expect(clippy::arc_with_non_send_sync)` in `lib.rs` turns off for wasm32
//! and only wasm32.
//!
//! It fires at three sites here — both `Context::clone_arc` impls and
//! `BrowserRuntimeControl::new` — and NOT on [`attach`]'s
//! `Arc::new(BrowserShellSource)`, because `BrowserShellSource` is a fieldless
//! unit struct and so is genuinely `Send + Sync`. That site carried a per-site
//! allow for a lint that never fired there; it is gone.

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
};

use impresspress_core::blocks::dev::{
    activation::{self, ActivationIntent},
    artifacts,
    control::{DynamicBlockSpec, RuntimeControl, ShellSource, ValidationFailure, ValidationStage},
    repo::generations::GenerationCause,
    seed::{self, SeedManifest},
    DevShared, BLOCK_NAME,
};
use wafer_run::{
    context::Context, wasm::WasmiBlock, Block, BlockInfo, BlockRuntime, ErrorCode, FuelLimit,
    InputStream, LifecycleEvent, LifecycleType, Message, OutputStream, ResourceLimits,
    TerminalNotResponse, WaferError,
};
use wasm_bindgen::{prelude::*, JsCast};
use wasm_bindgen_futures::JsFuture;

use crate::runtime_factory::{DynamicBlock, RuntimeFactory};

/// Per-call wasmi fuel budget for a guest block (design §6.6).
///
/// The producer's own default, restated rather than inherited: the sandbox
/// must not silently follow a change to `wafer_run::DEFAULT_FUEL`, because
/// this number is what bounds how long one keystroke's request can run inside
/// a service worker that has no other way to be interrupted.
const GUEST_FUEL: u64 = 100_000_000;

/// Per-call linear-memory cap for a guest block, in 64 KiB wasmi pages
/// (256 pages = 16 MiB).
const GUEST_MEMORY_PAGES: u32 = 256;

/// The limits every guest — probed or live — is loaded under.
///
/// One function, so a guest cannot be probed under bounds it will not run
/// under. `..Default::default()` keeps the producer's SEC-03 caps (host bytes,
/// live streams, table elements), which the sandbox has no reason to move.
fn guest_limits() -> ResourceLimits {
    ResourceLimits {
        fuel: FuelLimit::Metered(GUEST_FUEL),
        memory_pages: GUEST_MEMORY_PAGES,
        ..Default::default()
    }
}

/// Compile `artifact` as the guest `spec` describes.
///
/// The capabilities are `spec.capabilities` — the **accepted** set the dev
/// block's static rules produced, never the guest's own declaration. That is
/// the whole security property of the inspect → rules → probe order, and it is
/// preserved by this function having no other source for them. `WasmiBlock`
/// keeps them as the guest's bound: sealing the runtime narrows them to what
/// the guest declares (and its `capabilities` block config), and never widens
/// them, so a declaration can only ask for less than the spec admits.
pub fn load_guest(
    spec: &DynamicBlockSpec,
    artifact: &[u8],
) -> Result<Arc<dyn Block>, wafer_run::RuntimeError> {
    let block = WasmiBlock::load_with_capabilities_and_limits(
        artifact,
        spec.capabilities.clone(),
        guest_limits(),
    )?;
    Ok(Arc::new(block))
}

// ---------------------------------------------------------------------------
// The probe context
// ---------------------------------------------------------------------------

/// The [`Context`] a guest is probed under: every host call is denied.
///
/// A probe is a dry run of untrusted code, and it happens *before* anything
/// has been registered — there is no `wafer-run/database` to route to that
/// would not be acting on the live instance's data. So the answer to every
/// host call is `PermissionDenied`, and the guest's own capability set (which
/// wasmi enforces independently) never gets the chance to matter.
///
/// The denial *count* is the load-bearing part. It is how
/// [`BrowserRuntimeControl::probe`] tells "this guest's `Init` failed because
/// the probe denied it the database" — expected, and not a reason to refuse a
/// block — from "this guest's `Init` failed on its own", which is. Nothing
/// about the guest's error code or message is consulted for that: a template
/// that wraps a host error in its own is judged the same as one that
/// propagates it.
#[derive(Default)]
struct DenyAllContext {
    /// How many host calls this context has refused, over the whole probe.
    denials: Cell<u32>,
}

impl DenyAllContext {
    /// The refusal count, for comparing before and after one probe step.
    fn denials(&self) -> u32 {
        self.denials.get()
    }

    fn deny(&self, what: &str) -> WaferError {
        self.denials.set(self.denials.get().saturating_add(1));
        WaferError::new(
            ErrorCode::PermissionDenied,
            format!("the validation probe grants no host access: {what} is unreachable"),
        )
    }
}

#[wafer_block::wafer_async_trait]
impl Context for DenyAllContext {
    async fn call_block(
        &self,
        block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::error(self.deny(&format!("block {block_name:?}")))
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        // A fresh counter rather than a shared one: the clone is handed out
        // for a caller to retain, and a probe step's verdict must be decided
        // by the calls made *during* that step through the context the step
        // was given.
        Arc::new(Self::default())
    }

    /// Deny, and count.
    ///
    /// The trait's own default already denies (it is fail-closed), so this
    /// override exists for the counter — and to say in one place that this
    /// context refuses on purpose rather than by omission.
    fn check_resource_access(
        &self,
        resource: &str,
        _resource_type: wafer_run::ResourceType,
        _access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        Err(self.deny(&format!("resource {resource:?}")))
    }

    /// Admits nothing, as [`Self::check_resource_access`] does. Not counted:
    /// this is a probe, never the authorization itself.
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: wafer_run::ResourceType,
        _access: wafer_block::ResourceAccess,
    ) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// The control
// ---------------------------------------------------------------------------

thread_local! {
    /// The one strong handle to the sandbox's runtime factory.
    ///
    /// The factory is a process-lifetime singleton — every rebuild goes
    /// through it, and it owns the platform services every runtime shares — so
    /// it is owned here, by the module, exactly as the live `Wafer` is owned
    /// by `impresspress_browser::runtime`. That is what lets
    /// [`BrowserRuntimeControl`] hold a [`Weak`] and the reference graph stay
    /// acyclic: control → (weak) factory → `DevShared` → control.
    static FACTORY: RefCell<Option<Rc<RuntimeFactory>>> = const { RefCell::new(None) };
}

/// The host's half of activation, backed by wasmi and the shared
/// [`RuntimeFactory`].
pub struct BrowserRuntimeControl {
    /// The factory every rebuild goes through, once [`Self::set_factory`] has
    /// closed the cycle described in the module header.
    ///
    /// `Weak`, because the factory holds the `DevShared` that holds this
    /// control: a strong handle here would be a reference cycle, and the
    /// factory transitively owns every platform service, so it is not a cycle
    /// anyone would want leaked. The strong handle lives in [`FACTORY`].
    factory: RefCell<Weak<RuntimeFactory>>,
    /// Bumped by every successful rebuild; read by `GET /b/dev/api/status`.
    generation: Cell<u64>,
    /// The block set of the last **successful** rebuild, or `None` before the
    /// first one.
    ///
    /// Read by [`install`] to decide whether the runtime already carries the
    /// block set the ledger says is active. The generation counter alone
    /// cannot answer that: `activation::restore` folds a failed
    /// `restore_previous` into a message and returns, so a runtime can be
    /// left carrying the block set of a generation the ledger has just marked
    /// `Failed` — with the counter bumped by the rebuild that *did* succeed
    /// on the way in.
    last_built: RefCell<Option<Vec<DynamicBlockSpec>>>,
    /// The runtime the last successful [`RuntimeControl::rebuild`] swapped
    /// out, and the block set it was serving — design §7.3 step 4's "retaining
    /// the previous `Rc`".
    ///
    /// This is the whole point of [`impresspress_browser::replace_wafer`]
    /// handing its return value back: the old `Wafer` lives exactly as long as
    /// the last handle to it, so a caller that dropped the `Rc` could not undo
    /// the swap and had to build a *different* runtime from the same block set
    /// instead. Taken by [`RuntimeControl::restore_previous`], and replaced by
    /// the next successful rebuild — a runtime two activations old is not
    /// something any failure path asks for, and holding it would pin its
    /// wasmi instances and linear memories for no reader.
    ///
    /// `last_built` travels with it because `already_built` has to describe
    /// the runtime that is *live*, and a restore changes which one that is.
    retained: RefCell<Option<RetainedRuntime>>,
}

/// The runtime a rebuild swapped out, and what it was serving.
///
/// A named pair rather than a tuple because the two halves are restored
/// together or not at all: putting the `Wafer` back without its block set
/// would leave `already_built` describing a runtime that is no longer live,
/// and boot would then decline to rebuild a set the runtime does not carry.
struct RetainedRuntime {
    /// The `Rc` [`impresspress_browser::replace_wafer`] handed back.
    wafer: Rc<wafer_run::Wafer>,
    /// The `last_built` value that went with it — `None` before any rebuild
    /// had succeeded.
    blocks: Option<Vec<DynamicBlockSpec>>,
}

impl BrowserRuntimeControl {
    /// A control with no factory yet. `Arc` because that is what `DevShared`
    /// holds; single-threaded contents are fine (see the module header).
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            factory: RefCell::new(Weak::new()),
            generation: Cell::new(0),
            last_built: RefCell::new(None),
            retained: RefCell::new(None),
        })
    }

    /// Close the control → factory half of the cycle, taking ownership of the
    /// strong handle in [`FACTORY`].
    pub fn set_factory(&self, factory: &Rc<RuntimeFactory>) {
        FACTORY.with(|slot| *slot.borrow_mut() = Some(Rc::clone(factory)));
        *self.factory.borrow_mut() = Rc::downgrade(factory);
    }

    /// The factory, upgraded out of the `Weak`.
    ///
    /// Taken out of the `RefCell` rather than borrowed across the caller's
    /// `await`s: a borrow held across a suspension point is a panic waiting
    /// for the first concurrent activation.
    fn factory(&self) -> Result<Rc<RuntimeFactory>, String> {
        self.factory
            .borrow()
            .upgrade()
            .ok_or_else(|| "the sandbox runtime factory is not installed".to_string())
    }

    /// Whether the live runtime was last built from exactly `blocks`.
    ///
    /// Compared by name and artifact hash — the two fields that decide what
    /// code is registered under which name. Capabilities and routes are
    /// functions of an accepted spec that is itself keyed by those two, so a
    /// difference in either would be a difference here first.
    fn already_built(&self, blocks: &[DynamicBlockSpec]) -> bool {
        let built = self.last_built.borrow();
        let Some(built) = built.as_deref() else {
            return blocks.is_empty();
        };
        built.len() == blocks.len()
            && built
                .iter()
                .zip(blocks)
                .all(|(a, b)| a.name == b.name && a.artifact_sha256 == b.artifact_sha256)
    }
}

#[wafer_block::wafer_async_trait]
impl RuntimeControl for BrowserRuntimeControl {
    /// Read the guest's `BlockInfo` without running any of its code beyond
    /// instantiation and `__wafer_info`, under `BlockCapabilities::none()`.
    ///
    /// `Block::info()` is infallible by contract, so a guest that cannot
    /// report its own info does not error — wasmi logs and answers the
    /// placeholder `BlockInfo::new("unknown", "0.0.0", "unknown", …)`. That
    /// placeholder is indistinguishable from a real (if absurd) declaration by
    /// name alone, so the discriminator used here is
    /// [`BlockInfo::runtime`]: a successful read is stamped
    /// [`BlockRuntime::Wasm`] by the loader, and the placeholder keeps the
    /// `Native` default. Refusing on the name instead would also refuse a
    /// guest that legitimately called itself `unknown` — and, worse, would
    /// accept a failed read from a guest whose name happened not to be.
    async fn inspect(&self, artifact: &[u8]) -> Result<BlockInfo, ValidationFailure> {
        let block = WasmiBlock::load_with_capabilities_and_limits(
            artifact,
            wafer_block::BlockCapabilities::none(),
            guest_limits(),
        )
        .map_err(|e| {
            ValidationFailure::new(
                ValidationStage::Load,
                format!("the module did not compile or instantiate: {e}"),
            )
        })?;

        let info = block.info();
        if info.runtime != BlockRuntime::Wasm {
            return Err(ValidationFailure::new(
                ValidationStage::Info,
                "the module did not return a BlockInfo from `__wafer_info` — it is missing the \
                 export, its memory, or the value it wrote could not be decoded",
            ));
        }
        // Validated as the name it reports. `validate_static` then refuses a
        // report that is not `site/{name}` (NAME_MISMATCH), the name the
        // runtime registers it under, so the two cannot differ at registration.
        info.validate(&info.name)
            .map_err(|e| ValidationFailure::new(ValidationStage::Info, format!("{e}")))?;
        Ok(info)
    }

    /// Run `Init`, `Start` and one request under the **accepted** spec's
    /// capabilities, swapping nothing in.
    ///
    /// # What each step proves, and what it does not
    ///
    /// The seam gives the host a `Result`/`OutputStream`, not a trap flag: a
    /// wasmi trap is delivered as an ordinary `ErrorCode::Internal`, which is
    /// also what a guest can return on its own. So neither step can name a
    /// trap exactly, and each is judged on the discriminator that is real:
    ///
    /// * **`Init` / `Start`** fail the probe when they error *without the
    ///   probe context having denied them anything*. A guest whose `Init`
    ///   calls `ensure_table` will be denied by [`DenyAllContext`] and fail;
    ///   that is the probe's own doing, not a defect, so it passes. An `Init`
    ///   that fails having asked the host for nothing failed on its own, and
    ///   would fail identically once live.
    /// * **The request** passes on any terminal except an
    ///   `ErrorCode::Internal` error or a stream that ends with no terminal at
    ///   all. A `GET` of a guest's route root is *expected* to 404 on a block
    ///   that serves only sub-paths, so an application error here proves
    ///   nothing either way — but every host-side failure (a trap, fuel
    ///   exhaustion, a missing `__wafer_handle`, a guest result that does not
    ///   decode) arrives as `Internal`, and a guest that answers `Internal` to
    ///   a bare `GET` of its own prefix is broken whichever produced it.
    ///
    /// A guest with no declared route is not requested at all; there is no URL
    /// to make up, and the static rules have already refused a block whose
    /// endpoints lie outside its prefix.
    async fn probe(
        &self,
        spec: &DynamicBlockSpec,
        artifact: &[u8],
    ) -> Result<(), ValidationFailure> {
        let block = load_guest(spec, artifact).map_err(|e| {
            ValidationFailure::new(
                ValidationStage::Load,
                format!("the module did not compile or instantiate: {e}"),
            )
        })?;
        let ctx = DenyAllContext::default();

        for (event_type, stage) in [
            (LifecycleType::Init, ValidationStage::Init),
            (LifecycleType::Start, ValidationStage::Start),
        ] {
            let before = ctx.denials();
            let outcome = block
                .lifecycle(
                    &ctx,
                    LifecycleEvent {
                        event_type,
                        data: Vec::new(),
                    },
                )
                .await;
            if let Err(e) = outcome {
                if ctx.denials() == before {
                    return Err(ValidationFailure::new(
                        stage,
                        format!(
                            "{:?} failed without asking the host for anything: {}",
                            event_type, e.message
                        ),
                    ));
                }
            }
        }

        let Some(route) = spec.routes.first() else {
            return Ok(());
        };
        let msg = wafer_block::http_codec::build_http_message(
            "GET",
            &route.prefix,
            "",
            "127.0.0.1",
            std::iter::empty::<(&str, &str)>(),
        );
        match block
            .handle(&ctx, msg, InputStream::empty())
            .await
            .collect_buffered()
            .await
        {
            Ok(_) => Ok(()),
            Err(TerminalNotResponse::Error(e)) if e.code == ErrorCode::Internal => {
                Err(ValidationFailure::new(
                    ValidationStage::Probe,
                    format!(
                        "GET {} failed inside the runtime: {}",
                        route.prefix, e.message
                    ),
                ))
            }
            Err(TerminalNotResponse::Malformed) => Err(ValidationFailure::new(
                ValidationStage::Probe,
                format!(
                    "GET {} produced no terminal event — the guest did not complete its ABI \
                     contract",
                    route.prefix
                ),
            )),
            // Error (application), Drop, Halt, Continue: the guest ran and
            // answered. Whether it answered 200 or 404 is not this step's
            // question.
            Err(_) => Ok(()),
        }
    }

    /// Build a runtime with exactly `blocks` and swap it in.
    ///
    /// Artifacts are read through the platform `StorageService` directly:
    /// there is no request in flight here (an activation can be driven by boot
    /// convergence, where no `Context` exists at all), and
    /// [`artifacts::get_direct`] is where the key layout is stated so the two
    /// readers cannot drift.
    ///
    /// Nothing is swapped until every guest has loaded and the whole runtime
    /// has booted, so a failure anywhere leaves the live runtime exactly as it
    /// was. The generation counter is bumped last, after the swap, because it
    /// is what the `/b/dev` page keys its tool re-registration on.
    ///
    /// The `Rc` [`impresspress_browser::replace_wafer`] hands back is kept in
    /// [`Self::retained`], which is what makes the swap reversible — see
    /// [`Self::restore_previous`].
    async fn rebuild(&self, blocks: &[DynamicBlockSpec]) -> Result<(), String> {
        let factory = self.factory()?;
        let storage = impresspress_browser::make_storage_service();

        let mut dynamic: Vec<DynamicBlock> = Vec::with_capacity(blocks.len());
        for spec in blocks {
            let artifact = artifacts::get_direct(&storage, &spec.artifact_sha256)
                .await
                .map_err(|e| format!("block {}: {}", spec.name, e.message))?;
            let block =
                load_guest(spec, &artifact).map_err(|e| format!("block {}: {e}", spec.name))?;
            dynamic.push((spec.clone(), block));
        }

        let wafer = factory
            .build(&dynamic)
            .await
            .map_err(|e| describe_js(&e, "building the runtime"))?;
        let previous = impresspress_browser::replace_wafer(wafer).map_err(|e| e.to_string())?;
        let was_built = self.last_built.borrow_mut().replace(blocks.to_vec());
        *self.retained.borrow_mut() = Some(RetainedRuntime {
            wafer: previous,
            blocks: was_built,
        });
        self.generation.set(self.generation.get().saturating_add(1));
        Ok(())
    }

    /// Put the retained runtime back, without building one.
    ///
    /// [`impresspress_browser::restore_wafer`] takes the very `Rc` the last
    /// [`Self::rebuild`] swapped out, so this is a pointer swap: no artifact
    /// is read, no guest is compiled, no block's `Init` runs a second time.
    /// That is what design §7.3 asks for, and it is also the only version of
    /// this that cannot fail on its own account once there is something to
    /// restore.
    ///
    /// The generation counter still moves: the live runtime changed, and the
    /// `/b/dev` page's tool registration is keyed on the counter rather than
    /// on which direction the change went.
    async fn restore_previous(&self) -> Result<(), String> {
        // Taken out of the `RefCell` before anything else touches it — the
        // same rule `factory()` follows, and the reason `retained` is never
        // borrowed across a call.
        let Some(retained) = self.retained.borrow_mut().take() else {
            return Err("no previous runtime was retained to restore".to_string());
        };
        impresspress_browser::restore_wafer(retained.wafer);
        *self.last_built.borrow_mut() = retained.blocks;
        self.generation.set(self.generation.get().saturating_add(1));
        Ok(())
    }

    fn runtime_generation(&self) -> u64 {
        self.generation.get()
    }
}

// ---------------------------------------------------------------------------
// The boot context
// ---------------------------------------------------------------------------

/// The [`Context`] the boot-time half of the sandbox runs under.
///
/// Boot convergence and the seed import are the dev block's own work, but they
/// happen with no request in flight, so there is no `RuntimeContext` for them
/// — `Wafer::make_context` is `pub(crate)` and `Wafer::run_block` builds a
/// context for a *block it dispatches to*, which is not what a host-side
/// caller of `impresspress_core::blocks::dev::activation` needs.
///
/// So this is that context, and it is deliberately thin:
///
/// * `call_block` looks the block up in the live runtime and calls it. The
///   runtime has already been sealed and every block's `Init` has run (see
///   `builder::boot`), so there is no lazy init left to drive.
/// * `caller_id` is the dev block. This is the load-bearing field: the
///   storage handler behind `wafer-run/storage` resolves every plain folder
///   into the caller's namespace, so a boot that reported no caller would be
///   refused every plain folder, and one that reported another block would
///   read and write that block's objects instead of `impresspress/dev/…`.
/// * `check_resource_access` runs the **real** WRAP check — the identical
///   `wrap::check_access` call `RuntimeContext::check_resource_access` makes,
///   keyed on the same caller and against the runtime's own grants and admin
///   block. Boot is not a reason to grant the dev block more than a request
///   would, and a blanket `Ok(())` here would have made boot the one path on
///   which a bug in any of it was invisible. It admits what it has to:
///   `impresspress__dev__*` tables self-admit under the own-resource rule,
///   `__ddl__` / `__schema__` admit any attributable caller, and storage
///   resources reach this call as the path the storage handler resolved:
///   `impresspress/dev/…` for the block's own blobs and artifacts, which the
///   owner rule admits, and `wafer-run/web/site/…` for the publisher, which
///   the runtime grant `wafer-run/web/site/*` (`blocks::dev::wrap_grants`)
///   admits — exactly as for a request, since the handler authorizes through
///   this context's `check_resource_access` and nothing else.
///
/// The one thing boot does not reproduce is `RuntimeContext::dispatch_call`'s
/// `requires` / `allows_call_block` gate, which asks whether the *calling
/// block* may call the callee. There is no calling block here — the host is
/// asking on the dev block's behalf, in the same sense `Wafer::run_block` is
/// documented to be a trusted entry.
///
/// The `Rc` is pinned to the runtime that was live when [`install`] started,
/// and a rebuild during boot swaps a *different* one in behind it. That is
/// deliberate and harmless: every platform service is shared by construction
/// (`RuntimeFactory` builds them once), so the database and object store this
/// keeps reaching are the same ones the new runtime holds. The only thing that
/// goes stale is the block set, which nothing on the boot path reads.
#[derive(Clone)]
struct BootContext {
    wafer: Rc<wafer_run::Wafer>,
}

#[wafer_block::wafer_async_trait]
impl Context for BootContext {
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        // The runtime's answer for nothing to dispatch to. `NotFound` is a
        // service saying the thing a request names does not exist, which a
        // client such as the config reader takes for "unset".
        let Some(block) = self.wafer.lookup_block(block_name).map(|(_, block)| block) else {
            return OutputStream::error(WaferError::new(
                ErrorCode::Unimplemented,
                format!("block not found: {block_name}"),
            ));
        };
        block.handle(self, msg, input).await
    }

    fn is_cancelled(&self) -> bool {
        false
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.wafer.config_snapshot().get(key).map(String::as_str)
    }

    fn caller_id(&self) -> Option<&str> {
        Some(BLOCK_NAME)
    }

    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }

    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        wafer_run::wrap::check_access(
            Some(BLOCK_NAME),
            resource,
            access,
            Some(&resource_type),
            self.wafer.wrap_grants(),
            self.wafer.wrap_admin_block(),
        )
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.check_resource_access(resource, resource_type, access)
            .is_ok()
    }
}

// ---------------------------------------------------------------------------
// Fetching the seed bundle
// ---------------------------------------------------------------------------

/// [`seed::SeedFetch`] over the service worker's own `fetch`.
///
/// `web_sys::window()` is `None` in a service worker, so the global is reached
/// as a [`web_sys::ServiceWorkerGlobalScope`]. These requests are *not* served
/// by this worker's own fetch handler (a worker does not intercept its own
/// outgoing requests), and the bundle additionally adds [`seed::ROOT`] to the
/// service worker's bypass list so a page asking for the same files reaches
/// the static host too.
struct SwFetch;

impl SwFetch {
    /// `Ok(None)` for a 404.
    ///
    /// Only [`seed::MANIFEST_URL`] is allowed to be absent — that is how a
    /// bundle says "no seed" — so the [`seed::SeedFetch`] impl below turns the
    /// same answer into an error for every other file.
    async fn try_get(&self, url: &str) -> Result<Option<Vec<u8>>, String> {
        let global: web_sys::ServiceWorkerGlobalScope = js_sys::global()
            .dyn_into()
            .map_err(|_| "the seed can only be fetched from a service worker global".to_string())?;
        let response: web_sys::Response = JsFuture::from(global.fetch_with_str(url))
            .await
            .map_err(|e| describe_js(&e, &format!("fetching {url}")))?
            .dyn_into()
            .map_err(|_| format!("fetching {url}: the response was not a Response"))?;

        if response.status() == 404 {
            return Ok(None);
        }
        if !response.ok() {
            return Err(format!("fetching {url}: HTTP {}", response.status()));
        }
        let buffer = JsFuture::from(
            response
                .array_buffer()
                .map_err(|e| describe_js(&e, &format!("reading {url}")))?,
        )
        .await
        .map_err(|e| describe_js(&e, &format!("reading {url}")))?;
        Ok(Some(
            js_sys::Uint8Array::new(&buffer.unchecked_into::<js_sys::ArrayBuffer>()).to_vec(),
        ))
    }
}

impl seed::SeedFetch for SwFetch {
    fn get<'a>(&'a self, url: &'a str) -> seed::FetchFuture<'a> {
        Box::pin(async move {
            self.try_get(url)
                .await?
                .ok_or_else(|| format!("{url}: the seed manifest names it, but it is not served"))
        })
    }
}

/// [`ShellSource`] over `/asset-manifest.json` and the service worker's own
/// `fetch` — how the export reads the static files this deployment was
/// shipped as.
///
/// # Why fetching from in here reaches the network
///
/// Two of the paths this fetches — `/` and `/index.html` — are ones the
/// worker's own `fetch` handler deliberately INTERCEPTS (`sw.js.tmpl` says so
/// outright: they are intercepted "so the consumer's router can render a UI
/// block at root"), and `/sw.js` is on the bypass list. Neither matters here,
/// because a service worker does not intercept its OWN outgoing requests: the
/// `fetch` event fires for requests from the clients a worker controls, not
/// for requests the worker itself makes. So `global.fetch('/index.html')`
/// from inside this worker goes to the network — it is the deployment's real
/// `index.html` that comes back, not the runtime's rendered landing page,
/// which is exactly what an export has to copy.
///
/// `cache: 'no-store'` because the HTTP cache is the one thing that could
/// hand this a file from a previous deployment. An export naming this
/// build's content-hashed script while carrying the previous build's bytes
/// would be a folder that 404s its own runtime.
struct BrowserShellSource;

impl BrowserShellSource {
    /// One file, by absolute path, straight from the network.
    async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let global: web_sys::ServiceWorkerGlobalScope = js_sys::global()
            .dyn_into()
            .map_err(|_| "the static shell can only be read from a service worker".to_string())?;
        let init = web_sys::RequestInit::new();
        init.set_cache(web_sys::RequestCache::NoStore);
        let request = web_sys::Request::new_with_str_and_init(url, &init)
            .map_err(|e| describe_js(&e, &format!("building a request for {url}")))?;
        let response: web_sys::Response = JsFuture::from(global.fetch_with_request(&request))
            .await
            .map_err(|e| describe_js(&e, &format!("fetching {url}")))?
            .dyn_into()
            .map_err(|_| format!("fetching {url}: the response was not a Response"))?;
        if !response.ok() {
            return Err(format!("fetching {url}: HTTP {}", response.status()));
        }
        let buffer = JsFuture::from(
            response
                .array_buffer()
                .map_err(|e| describe_js(&e, &format!("reading {url}")))?,
        )
        .await
        .map_err(|e| describe_js(&e, &format!("reading {url}")))?;
        Ok(js_sys::Uint8Array::new(&buffer.unchecked_into::<js_sys::ArrayBuffer>()).to_vec())
    }
}

/// Where the bundler writes the shell's own file listing.
const ASSET_MANIFEST_URL: &str = "/asset-manifest.json";

/// The one field of it this reads.
#[derive(serde::Deserialize)]
struct AssetManifestFiles {
    /// Every file the bundler left in the dist directory, relative and
    /// sorted (`impresspress-bundle`'s `AssetManifest::files`).
    #[serde(default)]
    files: Vec<String>,
}

#[wafer_block::wafer_async_trait]
impl ShellSource for BrowserShellSource {
    async fn list(&self) -> Result<Vec<String>, String> {
        let bytes = self.get(ASSET_MANIFEST_URL).await?;
        let manifest: AssetManifestFiles = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{ASSET_MANIFEST_URL} did not parse: {e}"))?;
        if manifest.files.is_empty() {
            // An empty list is not "a shell with no files" — it is a manifest
            // written by a bundler that predates the `files` field, and the
            // export would silently produce a folder with no runtime in it.
            return Err(format!(
                "{ASSET_MANIFEST_URL} lists no files; this bundle was built by an \
                 impresspress-bundle that does not write the shell's file list"
            ));
        }
        Ok(manifest.files)
    }

    async fn fetch(&self, path: &str) -> Result<Vec<u8>, String> {
        self.get(&format!("/{path}")).await
    }
}

// ---------------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------------

/// The two halves of an attached sandbox: the control the host drives, and the
/// shared state the `impresspress/dev` block runs on.
///
/// Both are needed by [`install`] and neither can be reached from the other:
/// `DevShared` holds the control only as `Arc<dyn RuntimeControl>` (no
/// downcast), and the control deliberately does not hold `DevShared` — that
/// would be the reference cycle the [`Weak`] factory handle exists to avoid.
pub struct Sandbox {
    control: Arc<BrowserRuntimeControl>,
    shared: Arc<DevShared>,
}

/// Attach the sandbox control plane to `factory`, before the first runtime is
/// built.
///
/// Returns the shared factory handle and — whenever the sandbox's RUNTIME half
/// is present — the [`Sandbox`] [`install`] needs.
///
/// Keyed on [`SandboxMode::runtime_present`](crate::SandboxMode::runtime_present), **not** on the `dev` flag. An
/// exported bundle boots with `{ dev: false }` and still needs every one of
/// these: the control that loads its seeded blocks, the `DevShared` that owns
/// the activation queue, and the [`install`] below that imports the `seed/`
/// the archive ships beside the shell. Keying this on the flag is what made an
/// exported site come up empty. `None` is therefore the "feature off =
/// nothing" rule alone (design §13): no control, no `DevShared`, so
/// `RuntimeFactory::with_dev` is never called and the runtime that gets built
/// is byte-identical to one that never asked for a sandbox. Which of the two
/// compiled-in modes this is decides what the FACTORY registers, not whether
/// there is a control at all — see [`SandboxMode`](crate::SandboxMode).
pub fn attach(factory: RuntimeFactory) -> (Rc<RuntimeFactory>, Option<Sandbox>) {
    if !factory.mode.runtime_present() {
        return (Rc::new(factory), None);
    }
    let control = BrowserRuntimeControl::new();
    // `ShellSource` is `MaybeSend + MaybeSync`, unbounded on wasm32, for the
    // same reason `RuntimeControl` is — this implementation resolves through a
    // `JsFuture`.
    let shell: Arc<dyn ShellSource> = Arc::new(BrowserShellSource);
    let shared = DevShared::new(control.clone(), shell);
    let factory = Rc::new(factory.with_dev(shared.clone()));
    control.set_factory(&factory);
    (factory, Some(Sandbox { control, shared }))
}

/// Bring the sandbox up on the runtime that was just stored.
///
/// Runs in BOTH compiled-in modes ([`Exported`](crate::SandboxMode::Exported)
/// as well as [`Workspace`](crate::SandboxMode::Workspace)). For an exported bundle this is the entire
/// reason the folder works at all: step 1 below is what reads the `seed/` the
/// archive ships beside the shell, and steps 2–3 are what put its blocks in
/// the runtime.
///
/// In order:
///
/// 1. **Seed**, when this instance has never published anything. A bundle with
///    no `/seed/manifest.json` is the ordinary case and not an error.
/// 2. **Converge** on whatever the activation journal says was in flight, and
///    learn the block set the active generation declares.
/// 3. **Rebuild**, unless the runtime already carries exactly that block set,
///    *before returning*. Requests only start arriving once `initialize()` has
///    resolved, so this is what keeps a request from being served by a runtime
///    whose blocks are still pending.
///
/// The guard compares the converged set against the set of the last
/// **successful** rebuild ([`BrowserRuntimeControl::already_built`]), not
/// against a counter of how many rebuilds happened. Steps 1–2 do rebuild on
/// their own — activating a seed that carries blocks, or converging on an
/// interrupted activation — and rebuilding a second, identical runtime
/// (migrations, block init and all) on the one boot that can least afford it
/// is worth avoiding. But "some rebuild happened" is not the same claim:
/// `activation::restore` folds a failed `rebuild(previous)` into a message and
/// carries on, so a runtime can be left carrying the block set of a generation
/// the ledger has just marked `Failed` while the counter records the rebuild
/// that *did* succeed on the way in. Comparing the sets is what makes this
/// step corrective rather than merely idempotent — including the case where
/// the ledger says nothing is active and the runtime is holding a block.
///
/// Every step logs its own failure and continues rather than failing
/// `initialize()`. A sandbox that refuses to boot is a sandbox whose `/b/dev`
/// page — the only thing that could fix it — never comes up; a sandbox that
/// boots with nothing dynamic still serves the page, the ledger and the
/// diagnostics that say why.
pub async fn install(sandbox: &Sandbox) {
    let Some(wafer) = impresspress_browser::current_wafer() else {
        web_sys::console::error_1(
            &"impresspress: the dev sandbox cannot install before a runtime is stored".into(),
        );
        return;
    };
    let ctx = BootContext { wafer };
    let shared = &sandbox.shared;

    if let Err(e) = seed_on_boot(&ctx, shared).await {
        // What happened, and what to do about it. This is the whole of the
        // failure's visibility on an EXPORTED bundle's first boot — the site
        // is empty, there is no `/b/dev` to ask, and a visitor sees a blank
        // page — so the line says where the same message is recorded for the
        // site's own admin, and that the instance stays in this state until
        // it is thrown away.
        web_sys::console::error_1(
            &format!(
                "impresspress: the seed bundle at {} was not imported: {e}\nThis site will \
                 stay empty. The same message is recorded as {} on \
                 /b/admin/settings/variables. Fix the bundle under /seed/, then load the site \
                 in a fresh browser profile (or clear this site's data) to retry — an instance \
                 only ever seeds once.",
                seed::MANIFEST_URL,
                seed::SEED_ERROR_KEY,
            )
            .into(),
        );
    }

    let blocks = match activation::converge_on_boot(&ctx, shared).await {
        Ok(blocks) => blocks,
        Err(e) => {
            web_sys::console::error_1(
                &format!("impresspress: dev sandbox boot convergence: {e}").into(),
            );
            return;
        }
    };
    if sandbox.control.already_built(&blocks) {
        return;
    }
    if let Err(e) = shared.control.rebuild(&blocks).await {
        web_sys::console::error_1(
            &format!(
                "impresspress: dev sandbox could not load its {} active block(s): {e}",
                blocks.len()
            )
            .into(),
        );
        return;
    }
    web_sys::console::log_1(
        &format!(
            "impresspress: dev sandbox runtime carries {} block(s)",
            blocks.len()
        )
        .into(),
    );
}

/// Import generation 0 from the origin's seed bundle, when there is one and
/// this instance has never published anything.
async fn seed_on_boot(ctx: &dyn Context, shared: &Arc<DevShared>) -> Result<(), String> {
    // Freshness first: this runs on every boot, and an instance that has been
    // used has no business fetching a bundle it will refuse to import.
    if !seed::is_fresh(ctx).await? {
        return Ok(());
    }
    let fetch = SwFetch;
    let Some(bytes) = fetch.try_get(seed::MANIFEST_URL).await? else {
        return Ok(());
    };
    // A 200 whose body is not a seed manifest is "no seed", not a failure. A
    // static host that answers every unknown path with the SPA's `index.html`
    // is the common case, and on such a host the *absence* of a bundle looks
    // exactly like this. Said at info: worth seeing when a seed was expected,
    // and not a fault when one was not.
    let manifest: SeedManifest = match serde_json::from_slice(&bytes) {
        Ok(manifest) => manifest,
        Err(e) => {
            web_sys::console::info_1(
                &format!(
                    "impresspress: {} did not parse as a seed manifest ({e}) — booting with \
                     no seed",
                    seed::MANIFEST_URL
                )
                .into(),
            );
            return Ok(());
        }
    };
    // `shared.control` is the same `BrowserRuntimeControl` every later
    // activation rebuilds through: the importer uses it to `inspect` each
    // seeded artifact under deny-all capabilities and run the four validation
    // rules that need the guest's own `BlockInfo` (see `seed`'s module docs).
    let Some(generation) = seed::import(ctx, shared.control.as_ref(), &manifest, &fetch).await?
    else {
        return Ok(());
    };
    // A seed that imported but could not be activated leaves exactly the
    // symptom the import path records — an empty site — and leaves it
    // permanently: the failed generation is in the ledger, so `is_fresh` is
    // false on every later boot and nothing retries. The importer's own
    // recorder is what the admin reads, so this failure is written there too
    // rather than to the console alone.
    let outcome = match activation::request(
        ctx,
        shared,
        GenerationCause::Seed,
        ActivationIntent::Seed {
            manifest: generation,
        },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(e) => {
            let message = format!("the seed imported but could not be activated: {e}");
            seed::record_failure(ctx, &message).await;
            return Err(message);
        }
    };
    web_sys::console::log_1(
        &format!(
            "impresspress: dev sandbox imported the seed as generation {} ({} site files, {} \
             blocks)",
            outcome.generation.id, outcome.generation.site_files, outcome.generation.blocks
        )
        .into(),
    );
    Ok(())
}

/// A `JsValue` rejection as a message, prefixed with what was being attempted.
///
/// `JsValue`'s own `Debug` renders an `Error` as `JsValue(Error: …)`, which is
/// what would otherwise end up in a `ValidationFailure` an agent reads.
fn describe_js(value: &JsValue, doing: &str) -> String {
    let detail = value
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(value, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{value:?}"));
    format!("{doing}: {detail}")
}
