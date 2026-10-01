//! The executable half of activation: the seam between the `impresspress/dev`
//! control plane and whatever owns the live runtime.
//!
//! `impresspress-core` can describe a block set ([`DynamicBlockSpec`]) and
//! decide what should be live, but it cannot build a `Wafer` — the runtime is
//! held by the host (`impresspress-web`'s service worker on the browser
//! target, the native/CF builder elsewhere). [`RuntimeControl`] is that
//! boundary: the dev block hands over a validated spec set and the host
//! answers with a rebuilt, swapped-in runtime.
//!
//! # Validation is three steps, in this order
//!
//! 1. [`RuntimeControl::inspect`] — instantiate the module under
//!    `BlockCapabilities::none()` and read its `BlockInfo`. No lifecycle
//!    event runs, so no guest code that could use a capability runs either.
//! 2. The caller's static rules (`super::validation`) — names, route
//!    prefixes, capability namespaces, collisions. Pure data over that
//!    `BlockInfo`; they are what turn the guest's *declaration* into an
//!    accepted [`DynamicBlockSpec`].
//! 3. [`RuntimeControl::probe`] — Init, Start and one request under the
//!    capabilities of that **accepted** spec.
//!
//! The order is the whole point. A single `validate` that ran the lifecycle
//! under the guest's own declaration would execute untrusted code under
//! authority nothing had approved yet — a module declaring
//! `collections: Any` would have its `Init` run with it. Splitting the seam
//! means the only capabilities a guest ever executes under are the ones step
//! 2 accepted.

use serde::{Deserialize, Serialize};

use super::repo::generations::GenerationCause;

/// Access tier a dynamically-registered block asks the router to apply to one
/// of its route prefixes.
///
/// The serialized spelling is the one the generation manifest uses
/// (design §11.3: `"access": "Public"`), and [`Self::as_str`] / [`Self::parse`]
/// speak exactly that spelling — the value has one representation everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum RouteAccessKind {
    /// No auth check; anyone may reach the route.
    Public,
    /// A resolved user identity is required.
    Authenticated,
    /// The `admin` role is required.
    Admin,
}

impl RouteAccessKind {
    /// Canonical string form (matches the serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "Public",
            Self::Authenticated => "Authenticated",
            Self::Admin => "Admin",
        }
    }

    /// Inverse of [`Self::as_str`]. `None` for an unrecognized value — an
    /// unknown access tier is never silently narrowed or widened.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "Public" => Some(Self::Public),
            "Authenticated" => Some(Self::Authenticated),
            "Admin" => Some(Self::Admin),
            _ => None,
        }
    }

    /// The router tier this maps onto. The two enums are the same three-tier
    /// ladder; this is the single place a dynamic block's declared tier is
    /// bridged into [`crate::routing::RouteAccess`].
    pub fn to_route_access(self) -> crate::routing::RouteAccess {
        match self {
            Self::Public => crate::routing::RouteAccess::Public,
            Self::Authenticated => crate::routing::RouteAccess::Authenticated,
            Self::Admin => crate::routing::RouteAccess::Admin,
        }
    }
}

/// One route prefix a dynamically-registered block serves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DynamicRoute {
    /// Route prefix, normalized and under `/b/{block}/`.
    pub prefix: String,
    /// Access tier the router enforces for the prefix.
    pub access: RouteAccessKind,
}

/// Everything the host needs to put one compiled guest into a runtime.
///
/// This is also the block entry of the generation manifest (design §11.3), so
/// a stored `block_manifest_json` decodes straight into `Vec<DynamicBlockSpec>`
/// with no intermediate shape.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DynamicBlockSpec {
    /// Registered block name (`site/{name}`).
    pub name: String,
    /// SHA-256 of the `wasm32-wasip1` artifact, hex-encoded.
    pub artifact_sha256: String,
    /// Route prefixes the block serves.
    pub routes: Vec<DynamicRoute>,
    /// Capabilities the guest is loaded under. Deny-by-default; the caller
    /// validates the declared set against the block's own namespace before
    /// this ever reaches [`RuntimeControl`].
    ///
    /// `BlockCapabilities` is a producer type that derives neither
    /// `JsonSchema` nor `PartialEq`. It is published as a free-form object
    /// here, and compared field-by-field in the `PartialEq` impl below.
    #[schemars(with = "serde_json::Value")]
    pub capabilities: wafer_block::BlockCapabilities,
    /// `WAFER_GUEST_VERSION` of the `wafer_guest` crate the artifact was
    /// built against; `0` when the compiler did not report one.
    pub wafer_guest_version: u32,
}

/// Field-by-field capability comparison.
///
/// `wafer_block::BlockCapabilities` derives `Debug, Clone, Default, Serialize,
/// Deserialize` and nothing else, so [`DynamicBlockSpec`] cannot derive
/// `PartialEq`. Destructuring (rather than reading fields through `.`) is what
/// keeps this honest: when the producer adds a capability — the `schema`
/// capability landing beside `ddl`, for one — this stops compiling instead of
/// silently declaring two different capability sets equal.
fn capabilities_eq(
    left: &wafer_block::BlockCapabilities,
    right: &wafer_block::BlockCapabilities,
) -> bool {
    let wafer_block::BlockCapabilities {
        collections,
        raw_sql,
        ddl,
        schema,
        storage_folders,
        crypto,
        network,
        config,
        vector_indexes,
        callable_blocks,
        headers,
    } = left;
    let wafer_block::capabilities::HeaderPolicy {
        readable,
        writable,
        masked,
    } = headers;

    *collections == right.collections
        && *raw_sql == right.raw_sql
        && *ddl == right.ddl
        && *schema == right.schema
        && *storage_folders == right.storage_folders
        && *crypto == right.crypto
        && *network == right.network
        && *config == right.config
        && *vector_indexes == right.vector_indexes
        && *callable_blocks == right.callable_blocks
        && *readable == right.headers.readable
        && *writable == right.headers.writable
        && *masked == right.headers.masked
}

impl PartialEq for DynamicBlockSpec {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.artifact_sha256 == other.artifact_sha256
            && self.routes == other.routes
            && self.wafer_guest_version == other.wafer_guest_version
            && capabilities_eq(&self.capabilities, &other.capabilities)
    }
}

impl Eq for DynamicBlockSpec {}

/// Where a guest failed [`RuntimeControl::inspect`] or
/// [`RuntimeControl::probe`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStage {
    /// wasmi could not compile or instantiate the artifact.
    Load,
    /// `BlockInfo` did not parse, or failed WAFER validation.
    Info,
    /// The `Init` lifecycle event trapped or errored.
    Init,
    /// The `Start` lifecycle event trapped or errored.
    Start,
    /// The single probe request trapped or errored.
    Probe,
}

impl ValidationStage {
    /// Canonical string form (matches the serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Info => "info",
            Self::Init => "init",
            Self::Start => "start",
            Self::Probe => "probe",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` for an unrecognized value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "load" => Some(Self::Load),
            "info" => Some(Self::Info),
            "init" => Some(Self::Init),
            "start" => Some(Self::Start),
            "probe" => Some(Self::Probe),
            _ => None,
        }
    }
}

/// A structured refusal. Surfaced to the agent as a diagnostic in the tool
/// result, never as a transport error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidationFailure {
    /// The stage that failed.
    pub stage: ValidationStage,
    /// Operator/agent-facing explanation.
    pub message: String,
}

impl ValidationFailure {
    /// Build a failure for `stage` with `message`.
    pub fn new(stage: ValidationStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ValidationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.stage.as_str(), self.message)
    }
}

/// The host's half of activation.
///
/// `MaybeSend + MaybeSync` (rather than a hard `Send + Sync`) is what lets the
/// browser implementation hold the live `Rc<Wafer>` it is required to hold:
/// the bound is `Send + Sync` on native and unbounded on wasm32, exactly like
/// [`crate::blocks::llm::provider_admin::ProviderAdmin`] and
/// [`crate::FeatureConfig`].
#[wafer_block::wafer_async_trait]
pub trait RuntimeControl: wafer_run::MaybeSend + wafer_run::MaybeSync {
    /// Step 1: compile and instantiate `artifact` under
    /// `wafer_block::BlockCapabilities::none()` and return the `BlockInfo` it
    /// reports.
    ///
    /// **No lifecycle event and no request may run here.** The guest's own
    /// capability declaration is *inside* the value this returns, so nothing
    /// has approved it yet; the deny-all set is what makes reading the
    /// declaration safe. Failures are [`ValidationStage::Load`] (wasmi could
    /// not compile or instantiate) or [`ValidationStage::Info`] (`BlockInfo`
    /// did not parse, or failed WAFER validation).
    ///
    /// The `BlockInfo` must be returned exactly as the guest reported it —
    /// the caller's rules read the name, the endpoints, the agent tool names,
    /// `requires` and `capabilities` out of it, and a host that normalized
    /// any of those would silently disable a rule.
    async fn inspect(&self, artifact: &[u8]) -> Result<wafer_block::BlockInfo, ValidationFailure>;

    /// Step 3: run `Init`, `Start` and one probe request under
    /// `spec.capabilities`.
    ///
    /// `spec` is the **accepted** spec — the one the caller's static rules
    /// produced and approved, not the guest's raw declaration. It is also
    /// exactly what [`Self::rebuild`] will be handed, so a guest that traps
    /// here would have trapped live.
    ///
    /// A dry run: nothing is swapped in, and a trap must fail this call
    /// without poisoning the outer runtime (design §6.6). Failures are
    /// [`ValidationStage::Init`], [`ValidationStage::Start`] or
    /// [`ValidationStage::Probe`].
    async fn probe(
        &self,
        spec: &DynamicBlockSpec,
        artifact: &[u8],
    ) -> Result<(), ValidationFailure>;

    /// Rebuild the runtime with exactly this block set and swap it in.
    ///
    /// A successful call **retains** the runtime it swapped out, so that
    /// [`Self::restore_previous`] can put it back. The retained handle is
    /// released by the next successful `rebuild`, or by the `restore_previous`
    /// that consumes it.
    async fn rebuild(&self, blocks: &[DynamicBlockSpec]) -> Result<(), String>;

    /// Restore the runtime the last successful [`Self::rebuild`] swapped out,
    /// **without rebuilding it**.
    ///
    /// This is design §7.3 step 4: an activation swaps the runtime in before
    /// it publishes the site, "retaining the previous `Rc`", and "a failure
    /// after step 4 restores the previous `Rc`". Restoring the *value* rather
    /// than rebuilding from the same block set matters for three reasons: a
    /// rebuild is not guaranteed to reproduce the runtime it is undoing (it
    /// re-seals and re-runs every built-in block's `Init`), it costs that work
    /// twice on a path that is already failing, and it can itself fail —
    /// which is precisely when the rollback is needed.
    ///
    /// `Err` when there is nothing retained (no rebuild has succeeded since
    /// the last restore) or when the swap itself failed. Both leave the
    /// caller's failure report to say so; the runtime is then whatever the
    /// rebuild left live.
    async fn restore_previous(&self) -> Result<(), String>;

    /// Monotonic runtime generation counter (bumped by every successful
    /// rebuild). The `/b/dev` page re-registers its agent tools whenever this
    /// changes.
    fn runtime_generation(&self) -> u64;

    /// Tell every open page that `generation` is now live (design §2.6).
    ///
    /// Called once per activation, after it has committed, whoever caused
    /// it: its ledger row and journal are written and say it is active, and
    /// the reply to the request that caused it follows the announcement. It
    /// is how a tab
    /// learns of a generation another tab (or an agent driving one) made, and
    /// how the page's own preview learns it may reload without first reading
    /// the status back.
    ///
    /// Fire-and-forget, hence synchronous and infallible: the activation has
    /// already happened, so a failure to deliver the news must not be
    /// reported as a failure to activate. An implementation logs what it
    /// could not deliver; a page that missed the message still sees the
    /// generation on its next status read.
    fn announce_active(&self, generation: &GenerationAnnouncement);
}

/// What [`RuntimeControl::announce_active`] tells the page about a
/// generation that has just gone live.
///
/// # The `dev-generation` message
///
/// This is the canonical statement of the wire shape; the browser's
/// `dev_runtime.rs::announcement_message` builds it and `dev.js`'s
/// `onGenerationActive` reads it. Every window on the origin receives, via
/// `postMessage`, a plain object with exactly four fields:
///
/// - `type`: the string `"dev-generation"`, which is how a window tells this
///   message from anything else the service worker posts;
/// - `id`: [`Self::id`];
/// - `cause`: [`Self::cause`] in [`GenerationCause::as_str`]'s snake_case
///   spelling (`site_write`, `site_delete`, `block_compile`, `block_remove`,
///   `rollback`, `seed`), the same spelling the `/b/dev` API uses;
/// - `changed_paths`: [`Self::changed_paths`], an array of strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationAnnouncement {
    /// The generation that is now active.
    pub id: String,
    /// What created it. Only `site_write` and `site_delete` leave the block
    /// set as it was; any other cause may have changed the blocks the site
    /// calls.
    pub cause: GenerationCause,
    /// Every workspace path (`site/…`) the activation wrote to or removed
    /// from the published site, in the order the publisher touched them.
    /// Empty when the site did not change — a compile, a block removal. The
    /// page reads it to tell a CSS-only change (swap the stylesheets) from
    /// any other (reload the preview).
    pub changed_paths: Vec<String>,
}

/// The static shell the sandbox is running inside, as the export reads it.
///
/// `impresspress-core` describes what an export bundle should contain, but it
/// cannot read the deployment's own `index.html`, `sw.js` or wasm: those are
/// static files the host serves, and the only thing that can fetch them is
/// whatever is running the runtime — the service worker's own `fetch` in the
/// browser, an in-memory map in the host tests. This is that seam, and it is
/// the mirror of [`RuntimeControl`]: one describes the runtime the host
/// builds, the other the files the host was shipped as.
///
/// # Why a listing and not a fixed set of names
///
/// The shell's filenames are content-hashed and change with every build
/// (`impresspress-bundle`'s `rename`), and the wasm-bindgen glue pulls in a
/// `snippets/` tree whose shape depends on the crate. Nothing in this crate
/// can know them. `impresspress-bundle` writes them all into
/// `/asset-manifest.json`'s `files` for exactly this reason, and
/// [`Self::list`] is what reads it back — so a file the bundler produced is
/// a file the export ships, with no list to keep in step by hand.
///
/// `MaybeSend + MaybeSync` for the same reason [`RuntimeControl`] is: the
/// browser implementation resolves through a `JsFuture`, which is not `Send`,
/// and the sandbox is single-threaded on every target it runs on.
#[wafer_block::wafer_async_trait]
pub trait ShellSource: wafer_run::MaybeSend + wafer_run::MaybeSync {
    /// Every file of the running static shell, relative to the site root
    /// (`index.html`, `vendor/sql-wasm.wasm`), as `/asset-manifest.json`'s
    /// `files` lists them.
    ///
    /// `Err` — rather than an empty list — when the manifest cannot be read
    /// or carries no `files`: an export with no runtime in it is a folder
    /// that cannot be served, and it must fail loudly rather than quietly
    /// produce one.
    async fn list(&self) -> Result<Vec<String>, String>;

    /// The bytes of one file [`Self::list`] named, by that same relative
    /// path.
    async fn fetch(&self, path: &str) -> Result<Vec<u8>, String>;
}
