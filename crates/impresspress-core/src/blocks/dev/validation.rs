//! Static validation of a staged guest block (design §7.4, §6.5).
//!
//! Validation has three steps, and this module is the middle one. The
//! executable steps are the host's: [`super::control::RuntimeControl::inspect`]
//! loads the artifact under no capabilities and parses `BlockInfo`, and
//! [`super::control::RuntimeControl::probe`] runs `Init`/`Start` and one
//! request under the spec the rules below accepted. This module is the pure
//! data half: rules over the `BlockInfo` the guest reported, the name it was
//! compiled under, and the block set it is joining.
//!
//! # Why the rules live beside the spec and not in the host
//!
//! Every rule here is a statement about a [`DynamicBlockSpec`] and its
//! neighbours — nothing needs a runtime to decide. Keeping them out of the
//! `RuntimeControl` implementation means the browser host and the native host
//! cannot come to different conclusions about the same guest, and it means
//! the whole rule set is testable without wasmi.
//!
//! # Why a refusal is a value and not an error
//!
//! Every refusal is a [`Diagnostic`] with a stable `code`. The sandbox's
//! caller is an agent: an HTTP error would tell it "something went wrong",
//! and a coded diagnostic tells it what to change. So [`validate_static`]
//! collects *every* rule that fires rather than stopping at the first, and
//! the staging endpoint answers `200` with `success: false` (design §7.4:
//! "refusals are structured diagnostics in the tool result, never a transport
//! error").
//!
//! # Totality
//!
//! [`validate_static`] assumes nothing about its inputs — in particular it
//! does not assume `name` is a legal block name, because the rule that says
//! so ([`NAME_FORMAT`]) is one of its own. The staging handler refuses a
//! malformed name *before* it executes the guest (see
//! [`name_format_diagnostic`]); that is an ordering decision about running
//! untrusted code, not a precondition this function may rely on.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use wafer_block::{capabilities::HeaderPolicy, wrap, Allowlist, BlockCapabilities, BlockInfo};

use super::{
    control::{DynamicBlockSpec, DynamicRoute, RouteAccessKind, ValidationFailure},
    paths,
};

/// Largest guest artifact the sandbox accepts (design §6.6).
///
/// Checked before the artifact is stored or executed: an oversized module is
/// refused as a diagnostic, so a compile that produced one costs nothing but
/// the decode.
pub const MAX_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;

/// Block-name prefixes a guest may never claim.
///
/// A guest is always registered as `site/{name}`, so nothing it *can* be
/// named starts with one of these. The rule exists for what the guest
/// *reports*: a `BlockInfo` naming itself `impresspress/admin` is a guest
/// trying to be mistaken for a built-in, and saying so with its own code is
/// more useful than the bare name mismatch that also fires.
pub const RESERVED_NAME_PREFIXES: &[&str] = &["wafer-run/", "impresspress/"];

/// The namespace every dynamically-registered block is named under.
///
/// A guest is admitted as `site/{name}` and under no other name:
/// [`validate_static`] and [`validate_spec`] both refuse a `BlockInfo` whose
/// name is not this prefix plus the name it was compiled as
/// ([`NAME_MISMATCH`]), and [`RESERVED_NAME_PREFIXES`] refuses one that
/// reaches for a built-in's namespace instead. Those two rules together make
/// this prefix a *partition* of a running runtime's registered block list:
/// everything under it is a sandbox block, everything else is a built-in.
/// [`builtin_agent_tool_names`] is what reads it that way.
pub const BLOCK_NAMESPACE: &str = "site/";

/// The only blocks a guest may declare as callable (design §6.5).
///
/// Cross-block calls into feature blocks are out of scope for v1: a guest's
/// *frontend* talks to `impresspress/products` over HTTP like any page, and
/// the guest itself reaches only the four platform services.
pub const ALLOWED_CALLABLE_BLOCKS: &[&str] = &[
    "wafer-run/database",
    "wafer-run/storage",
    "wafer-run/config",
    "wafer-run/logger",
];

// ---------------------------------------------------------------------------
// Diagnostic codes
// ---------------------------------------------------------------------------

/// `name` is not a legal block name.
pub const NAME_FORMAT: &str = "name-format";
/// The reported `BlockInfo.name` claims a reserved namespace.
pub const NAME_RESERVED: &str = "name-reserved";
/// The reported `BlockInfo.name` is not `site/{name}`.
pub const NAME_MISMATCH: &str = "name-mismatch";
/// The route prefix the name produces is not a normalized prefix.
pub const ROUTE_PREFIX_CODE: &str = "route-prefix";
/// The route prefix overlaps a built-in route or another block's.
pub const ROUTE_COLLISION: &str = "route-collision";
/// A declared endpoint sits outside the block's own route prefix.
pub const ENDPOINT_OUTSIDE_ROUTES: &str = "endpoint-outside-routes";
/// The guest declares no endpoints at all.
pub const ENDPOINTS_EMPTY: &str = "endpoints-empty";
/// An agent tool name is already claimed.
pub const TOOL_NAME_DUPLICATE: &str = "tool-name-duplicate";
/// A declared collection is outside `site__{name}__*` (a hyphen in `name`
/// spelled `_`), or is not a plain `[a-z0-9_]` identifier.
pub const CAP_COLLECTION: &str = "cap-collection";
/// A declared storage folder is outside `site/{name}`.
pub const CAP_FOLDER: &str = "cap-folder";
/// A declared config key is outside `SITE__{NAME}__*` (a hyphen in `name`
/// spelled `_`).
pub const CAP_CONFIG: &str = "cap-config";
/// The guest declared `raw_sql`.
pub const CAP_RAW_SQL: &str = "cap-raw-sql";
/// The guest declared `ddl`.
pub const CAP_DDL: &str = "cap-ddl";
/// The guest declared network access.
pub const CAP_NETWORK: &str = "cap-network";
/// The guest declared crypto access.
pub const CAP_CRYPTO: &str = "cap-crypto";
/// The guest declared vector indexes.
pub const CAP_VECTOR: &str = "cap-vector";
/// The guest declared a callable block it may not call.
pub const CAP_CALLABLE: &str = "cap-callable";
/// `callable_blocks` and `requires` describe different sets.
pub const CAP_REQUIRES_MISMATCH: &str = "cap-requires-mismatch";
/// The guest declared readable or writable sensitive headers.
pub const CAP_HEADERS: &str = "cap-headers";
/// Activating the block would put more than [`paths::MAX_BLOCKS`] blocks in
/// the runtime.
pub const TOO_MANY_BLOCKS: &str = "too-many-blocks";
/// A block already in the active set has no readable stored `BlockInfo`, so
/// the duplicate-agent-tool rule cannot be applied against it.
pub const BUILD_ROW_MISSING: &str = "build-row-missing";
/// The artifact is over [`MAX_ARTIFACT_BYTES`].
pub const ARTIFACT_TOO_LARGE: &str = "artifact-too-large";
/// The artifact was compiled against a `wafer_guest.rs` that is not the one
/// the sandbox scaffolds.
pub const WAFER_GUEST_VERSION_CODE: &str = "wafer-guest-version";
/// A staged build the previous process did not finish, retired on the next
/// boot (`super::activation::converge_on_boot`).
pub const BUILD_ABANDONED: &str = "build-abandoned";

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// How serious a [`Diagnostic`] is.
///
/// The four levels rustc emits, so a compiler diagnostic the `/b/dev` page
/// forwards keeps the level it was produced with rather than being mapped
/// onto a smaller ladder on the way in.
///
/// Unlike the block's other closed enums this has no `as_str`/`parse` pair:
/// it is never a database column, a URL segment or a manifest key, so JSON is
/// its only representation and serde is its only spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The build is refused, or the compile failed.
    Error,
    /// The build stands; something is worth changing.
    Warning,
    /// Additional context for a nearby error or warning.
    Note,
    /// A suggested fix.
    Help,
}

/// One thing the compiler or the validator has to say about a build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    /// How serious it is. Anything `error` means the build was refused.
    pub severity: Severity,
    /// Stable machine-readable identifier, e.g. `cap-collection` for a
    /// capability outside the block's namespace or `guest-init` for a trap
    /// in the guest's `Init`. Match on this rather than on `message`.
    ///
    /// `null` when whoever produced the diagnostic had no code for it. Every
    /// diagnostic this crate produces has one — they are the constants above
    /// — but a *compiler* diagnostic forwarded by `/b/dev` need not: rustc
    /// numbers some of what it says (`E0425`) and not the rest, and the page
    /// forwards what the compiler gave it. This field is optional for the
    /// same reason `file`/`line`/`column` are: "when the compiler reported
    /// one". Inventing a placeholder on the way in would put a value in the
    /// build's stored record that nothing ever said.
    #[serde(default)]
    pub code: Option<String>,
    /// What is wrong, and what to change.
    pub message: String,
    /// Workspace-relative source file the diagnostic is about, when the
    /// compiler reported one.
    pub file: Option<String>,
    /// 1-based line in `file`, when the compiler reported one.
    pub line: Option<u32>,
    /// 1-based column in `file`, when the compiler reported one.
    pub column: Option<u32>,
}

impl Diagnostic {
    /// A refusal with `code`, carrying no source position.
    ///
    /// Validation rules are statements about a block's declarations, not
    /// about a span in a file: the compiler already accepted the source. The
    /// `file`/`line`/`column` fields exist for the diagnostics the *compiler*
    /// produced and the page forwards.
    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: Some(code.to_string()),
            message: message.into(),
            file: None,
            line: None,
            column: None,
        }
    }

    /// The refusal an oversized artifact produces.
    ///
    /// Built here rather than at the call site so [`ARTIFACT_TOO_LARGE`] and
    /// the limit it reports cannot drift apart from [`MAX_ARTIFACT_BYTES`].
    ///
    /// `len` is a *lower bound* on the artifact's size, not necessarily its
    /// exact length: the staging handler refuses an over-large body before
    /// decoding it, and at that point all it knows is what the encoding
    /// guarantees. The message says so rather than reporting a number it
    /// cannot stand behind.
    pub fn artifact_too_large(len: usize) -> Self {
        Self::error(
            ARTIFACT_TOO_LARGE,
            format!(
                "the artifact is at least {len} bytes; the sandbox accepts at most \
                 {MAX_ARTIFACT_BYTES}. Build with release size settings (opt-level = \"z\", \
                 lto, strip)."
            ),
        )
    }

    /// The refusal a stale vendored guest module produces.
    ///
    /// The module IS the ABI: it renders the `BlockInfo` the validator reads,
    /// decodes the request frame and writes the response frame. A block built
    /// against an older copy is therefore speaking a contract this runtime no
    /// longer guarantees, and the failure that would surface — a trap, or a
    /// `BlockInfo` that does not parse — says nothing about the cause.
    ///
    /// Refused before the artifact is stored or executed: the version is
    /// knowable without running anything, and the fix (replace the module,
    /// then recompile) does not depend on what the module would have
    /// reported.
    pub fn stale_guest_module(reported: u32, current: u32) -> Self {
        Self::error(
            WAFER_GUEST_VERSION_CODE,
            format!(
                "the artifact was compiled against wafer_guest.rs version {reported}; this \
                 sandbox writes and speaks version {current}. Replace the block's \
                 src/wafer_guest.rs with the current module (`wafer_guest_module` in \
                 GET /b/dev/api/reference) and compile again; the block's own files are \
                 unchanged."
            ),
        )
    }

    /// The refusal a failed [`super::control::RuntimeControl::inspect`] or
    /// [`super::control::RuntimeControl::probe`] produces.
    ///
    /// The code is `guest-{stage}` — `guest-load`, `guest-info`,
    /// `guest-init`, `guest-start`, `guest-probe` — so the stage is machine
    /// readable and the enum stays the single owner of its spelling.
    pub fn guest(failure: &ValidationFailure) -> Self {
        Self::error(
            &format!("guest-{}", failure.stage.as_str()),
            failure.message.clone(),
        )
    }
}

/// The refusal a malformed block name produces.
///
/// Exposed on its own because the staging handler refuses the name *before*
/// it hands the artifact to [`super::control::RuntimeControl::inspect`]: a
/// name that can never be registered would otherwise have an untrusted module
/// loaded and executed, and the provisional spec the host is handed would
/// carry a route prefix built from a name the router could not serve.
/// [`validate_static`] applies the same rule through this same function, so
/// the two paths cannot describe the name differently.
///
/// The rule text itself is [`paths::BLOCK_NAME_RULE`], rendered rather than
/// restated: this message and [`paths::PathError::BadBlockName`] are the two
/// places the rule is spoken to an agent, and a second spelling is how the
/// first one went stale.
pub fn name_format_diagnostic(name: &str) -> Diagnostic {
    Diagnostic::error(
        NAME_FORMAT,
        format!(
            "{name:?} is not a legal block name: {}",
            paths::BLOCK_NAME_RULE
        ),
    )
}

// ---------------------------------------------------------------------------
// Route prefixes
// ---------------------------------------------------------------------------

/// Every route prefix the built-in router already owns.
///
/// `crate::routing::ROUTES` plus the sandbox's own [`super::ROUTE_PREFIX`],
/// which is registered as a `crate::routing::ExtraRoute` and so is absent
/// from the built-in table. A deployment that adds further extra routes
/// passes them alongside these — the caller owns that list because
/// `Context` does not publish one.
pub fn builtin_route_prefixes() -> Vec<&'static str> {
    crate::routing::ROUTES
        .iter()
        .map(|route| route.prefix)
        .chain(std::iter::once(super::ROUTE_PREFIX))
        .collect()
}

/// Every agent tool name the given blocks' endpoints declare.
///
/// The one place a `BlockInfo` is turned into the set
/// [`validate_static`]'s duplicate rule reads. Both callers of that rule
/// build the set from a mixture of sources — the staging path from the
/// built-ins ([`builtin_agent_tool_names`]) plus each active dynamic block's
/// *stored* `BlockInfo`, the seed importer from the built-ins plus the other
/// blocks the same bundle carries — and a name that only one of them knew
/// how to extract would leave the rule half-applied on the other path. Which
/// blocks go IN is each caller's decision and a load-bearing one; see
/// [`builtin_agent_tool_names`] for the half that is easy to get wrong.
pub fn agent_tool_names<'a>(infos: impl IntoIterator<Item = &'a BlockInfo>) -> BTreeSet<String> {
    infos
        .into_iter()
        .flat_map(|info| info.endpoints.iter())
        .filter_map(|endpoint| endpoint.agent_tool.as_ref().map(|tool| tool.name.clone()))
        .collect()
}

/// Every agent tool name claimed by a **built-in** block of this runtime,
/// given the runtime's full registered block list.
///
/// The distinction is the whole point, and getting it wrong is a real bug the
/// sandbox shipped: `Context::registered_blocks` is the runtime's sealed
/// snapshot of *everything* registered, and a runtime that has been rebuilt
/// for a generation has every ACTIVE dynamic block registered in it too (they
/// arrive through `ImpresspressBuilder::extra_block`; see
/// `impresspress-web/src/runtime_factory.rs`). Seeding the claimed set from
/// that list unfiltered makes a block collide with ITSELF the moment it is
/// recompiled: the live previous version holds the name, and the staged new
/// version — the same block, the same tool — is refused
/// [`TOOL_NAME_DUPLICATE`] for a name nothing else has ever claimed. The only
/// way out was to remove the block and compile it again from nothing, which
/// takes the block offline for a full compile.
///
/// # Why the namespace and not the generation's names
///
/// Excluding "the blocks in the target generation" would fix the case above
/// and leave the shape of the bug behind: the registered list describes the
/// runtime as it is RUNNING, which is not the generation being staged into. A
/// block dropped from the generation but not yet rebuilt away, or a runtime
/// still one generation behind, would leave a stale dynamic block sitting in
/// the built-in half — a phantom claim on a name no live block holds. The
/// namespace rule cannot drift that way because it is not bookkeeping: it is
/// the same [`BLOCK_NAMESPACE`] rule `validate_static` enforces on every
/// guest on the way in, so "registered and under `site/`" and "is a sandbox
/// block" are the same statement.
///
/// The dynamic half of the claimed set is built separately and from the
/// authoritative source — each active block's *stored* `BlockInfo`, which is
/// what the generation it belongs to actually accepted — by
/// `super::blocks_api::claimed_tool_names` on the staging path and by
/// `super::seed::import` on the seed path.
pub fn builtin_agent_tool_names(registered: &[BlockInfo]) -> BTreeSet<String> {
    agent_tool_names(
        registered
            .iter()
            .filter(|info| !info.name.starts_with(BLOCK_NAMESPACE)),
    )
}

/// Whether `prefix` is a normalized route prefix: absolute, `/`-terminated,
/// and free of segments that would make prefix matching a statement about
/// something other than whole path segments.
///
/// The router matches by `str::starts_with`, so an unnormalized prefix is not
/// a cosmetic problem: `/b/a/../admin/` textually sits under `/b/a/` while
/// naming somebody else's routes.
fn is_normalized_prefix(prefix: &str) -> bool {
    if !prefix.starts_with('/') || !prefix.ends_with('/') {
        return false;
    }
    // `"/b/x/".split('/')` is `["", "b", "x", ""]`: the leading and trailing
    // empties are the two `/`s that must be there, and every segment between
    // them must be a real one.
    let segments: Vec<&str> = prefix.split('/').collect();
    segments.len() > 2
        && segments[1..segments.len() - 1].iter().all(|segment| {
            !segment.is_empty()
                && *segment != "."
                && *segment != ".."
                && !segment.contains('\\')
                && !segment.chars().any(char::is_control)
        })
}

/// Whether two route prefixes can both be served.
///
/// The router picks the first prefix that `starts_with`-matches, so a prefix
/// that is a prefix of another — in *either* direction — means one of the two
/// blocks never sees some of its own requests. `/b/admin/` and `/b/admin`
/// collide; `/b/hell/` and `/b/hello/` do not.
fn prefixes_overlap(left: &str, right: &str) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// Check every static rule for a guest compiled as `name`, and build the spec
/// the runtime is handed when they all pass.
///
/// * `name` — the short block name from the request (`hello`), *not* the
///   registered `site/hello`.
/// * `info` — what the guest reported through
///   [`super::control::RuntimeControl::inspect`].
/// * `artifact_sha256` — the stored artifact's hash.
/// * `builtin_routes` — every prefix the router already owns; see
///   [`builtin_route_prefixes`].
/// * `active` — the other blocks in the target generation, this one excluded.
/// * `claimed_tool_names` — every `agent_tool` name already taken, by a
///   built-in block or by one of `active`. The producer's manifest `seal()`
///   is the last line against a duplicate; this is the first, and the only
///   one that can still say *which* build to change.
pub fn validate_static(
    name: &str,
    info: &BlockInfo,
    artifact_sha256: &str,
    builtin_routes: &[&str],
    active: &[DynamicBlockSpec],
    claimed_tool_names: &BTreeSet<String>,
) -> Result<DynamicBlockSpec, Vec<Diagnostic>> {
    let mut found = Vec::new();
    let registered = format!("{BLOCK_NAMESPACE}{name}");
    let prefix = format!("/b/{name}/");

    // --- Identity ---------------------------------------------------------
    if !paths::block_name_is_valid(name) {
        found.push(name_format_diagnostic(name));
    }
    if let Some(reserved) = RESERVED_NAME_PREFIXES
        .iter()
        .find(|reserved| info.name.starts_with(**reserved))
    {
        found.push(Diagnostic::error(
            NAME_RESERVED,
            format!(
                "the guest reports its name as {:?}; {reserved:?} is reserved for built-in blocks",
                info.name
            ),
        ));
    }
    if info.name != registered {
        found.push(Diagnostic::error(
            NAME_MISMATCH,
            format!(
                "the guest reports its name as {:?}; a block compiled as {name:?} must call \
                 itself {registered:?}",
                info.name
            ),
        ));
    }

    // --- Routes -----------------------------------------------------------
    check_route_prefix(&prefix, builtin_routes, active, &mut found);

    // --- Endpoints --------------------------------------------------------
    // A guest that declares NO endpoint is refused outright, and the reason is
    // authorization rather than tidiness. The accepted spec below carries one
    // `Public` prefix as a floor, on the understanding that what a caller may
    // actually reach is decided per endpoint by the guest's own declarations
    // (`routing::extra_route_access`). A guest with an empty endpoint list
    // declares nothing to decide with, so every path it serves would rest on
    // the floor alone.
    //
    // The routing layer no longer *depends* on this — a refined extra route
    // sends an undeclared path to `Authenticated` whether or not the block
    // declared anything — but a block whose every route 403s a logged-out
    // visitor is not what the agent asked for either. Saying so here turns a
    // silently-inert block into one line the agent can act on, which is the
    // whole point of the staging diagnostics.
    if info.endpoints.is_empty() {
        found.push(Diagnostic::error(
            ENDPOINTS_EMPTY,
            format!(
                "the guest declares no endpoints; a block serves {prefix:?} through the endpoints                  it declares in `BlockInfo::endpoints`, and one with none is unreachable —                  declare at least one, with the `auth` level each path is meant to have (a                  genuinely public page must say `Public` explicitly)"
            ),
        ));
    }
    for endpoint in &info.endpoints {
        if !endpoint.path.starts_with(&prefix) {
            found.push(Diagnostic::error(
                ENDPOINT_OUTSIDE_ROUTES,
                format!(
                    "the guest declares {} {:?}, which is outside its own route prefix {prefix:?}",
                    endpoint.method, endpoint.path,
                ),
            ));
        }
    }

    // --- Agent tool names -------------------------------------------------
    let mut mine: BTreeSet<&str> = BTreeSet::new();
    for endpoint in &info.endpoints {
        let Some(tool) = endpoint.agent_tool.as_ref() else {
            continue;
        };
        if claimed_tool_names.contains(&tool.name) {
            found.push(Diagnostic::error(
                TOOL_NAME_DUPLICATE,
                format!(
                    "the agent tool name {:?} is already registered by another block; an MCP \
                     client shown two tools with one name drops one of them silently",
                    tool.name
                ),
            ));
        } else if !mine.insert(tool.name.as_str()) {
            found.push(Diagnostic::error(
                TOOL_NAME_DUPLICATE,
                format!(
                    "the guest declares the agent tool name {:?} on more than one endpoint",
                    tool.name
                ),
            ));
        }
    }

    // --- The size of the block set ----------------------------------------
    // `active` is every OTHER block in the target generation, so activating
    // this one makes the set one larger. The quota in `files.rs` bounds how
    // many block *source trees* the workspace holds, which is not the same
    // number: a block whose sources were deleted after it was staged is
    // still in the runtime. This is the check on what actually runs.
    if active.len() >= paths::MAX_BLOCKS {
        found.push(Diagnostic::error(
            TOO_MANY_BLOCKS,
            format!(
                "the runtime already serves {} blocks and the sandbox allows {}; \
                 remove one before adding another",
                active.len(),
                paths::MAX_BLOCKS,
            ),
        ));
    }

    // --- Capabilities -----------------------------------------------------
    // A guest that declares nothing gets `none()`, and that is also what the
    // spec it is loaded under carries: deny-by-default is the value, not an
    // absence the host has to interpret.
    let capabilities = info
        .capabilities
        .clone()
        .unwrap_or_else(BlockCapabilities::none);
    check_capabilities(name, Some(&info.requires), &capabilities, &mut found);

    if !found.is_empty() {
        return Err(found);
    }
    Ok(DynamicBlockSpec {
        name: registered,
        artifact_sha256: artifact_sha256.to_string(),
        // One prefix, `Public` at the router — the FLOOR, not the decision.
        //
        // The sandbox is registered as a `routing::ExtraRoute`, and until the
        // fix that landed with these rules an extra route enforced its own
        // tier alone: a guest endpoint declared `Admin` was served to
        // anonymous callers. A guest's routes are registered with
        // `add_refined_route`, so `routing::extra_route_access` combines this
        // floor with the endpoint the guest declared for the path — and sends
        // a path it declared NOTHING for to `Authenticated` rather than to
        // this tier.
        //
        // That refinement is unconditional: it does not ask whether the block
        // declared any endpoint, which is what makes the floor safe even for
        // a spec this function never saw (the seed importer admits specs with
        // no `BlockInfo` in hand). The `ENDPOINTS_EMPTY` rule above is the
        // agent-facing half of the same concern, not the security half.
        //
        // So `Public` here is safe *and* right: a guest that wants a
        // genuinely public route has to declare the endpoint `Public`, which
        // is the same bargain every built-in block makes.
        routes: vec![DynamicRoute {
            prefix,
            access: RouteAccessKind::Public,
        }],
        capabilities,
        // Filled by the staging handler from the version the *request*
        // reported (spec amendment 8), which it has already checked against
        // the sandbox's own. `BlockInfo` carries no such field, so there is
        // nothing here to read it from; `0` is "not reported".
        wafer_guest_version: 0,
    })
}

/// Check every rule that can be decided from an **accepted**
/// [`DynamicBlockSpec`] alone — no `BlockInfo`, no
/// [`super::control::RuntimeControl`], no artifact.
///
/// [`validate_static`] is the staging path's entry point and needs the guest's
/// own `BlockInfo`, which only a runtime can produce. The seed importer
/// (design §10.2) has no runtime: it is handed specs another instance already
/// accepted, over a `/seed/` prefix the service worker deliberately *bypasses*
/// — so the bytes are whatever the static host serves. Spec §13's
/// deny-by-default capabilities have to hold on that entry point too, and this
/// is the subset of the rules that can be applied there.
///
/// The rule *bodies* are [`validate_static`]'s own — [`check_route_prefix`]
/// and [`check_capabilities`], called with the same arguments — so the two
/// entry points cannot come to different conclusions about the same guest.
/// What is missing is exactly what needs the guest's report: the
/// name-mismatch, endpoint-inside-routes and duplicate-agent-tool rules all
/// read `BlockInfo`, and `callable_blocks`/`requires` cannot be compared with
/// only one of the pair in hand (the entries are still checked against
/// [`ALLOWED_CALLABLE_BLOCKS`]).
///
/// * `spec` — the block being admitted.
/// * `builtin_routes` — every prefix the router already owns; see
///   [`builtin_route_prefixes`].
/// * `others` — the other blocks in the same set, this one excluded.
pub fn validate_spec(
    spec: &DynamicBlockSpec,
    builtin_routes: &[&str],
    others: &[DynamicBlockSpec],
) -> Result<(), Vec<Diagnostic>> {
    let mut found = Vec::new();
    // `strip_prefix` with no fallback: a spec whose name is not `site/{name}`
    // has no short name, and inventing one by falling back to `spec.name`
    // validates the WRONG namespace. `"hello"` would then pass every rule
    // below — the format check, the route-prefix check, the collision check —
    // and be refused only by `wafer_run::runtime::validate_block_name` inside
    // the rebuild, long after the seed importer has written its blobs, its
    // artifacts and a generation row. `seed::is_fresh` is false from that row
    // onwards, so the instance can never seed again and the operator is left
    // with an opaque rebuild failure instead of this diagnostic.
    let Some(name) = spec.name.strip_prefix(BLOCK_NAMESPACE) else {
        return Err(vec![Diagnostic::error(
            NAME_MISMATCH,
            format!(
                "the block is registered as {:?}; a dynamically-registered block must be named                  `site/{{name}}`",
                spec.name
            ),
        )]);
    };

    // --- Identity ---------------------------------------------------------
    if !paths::block_name_is_valid(name) {
        found.push(name_format_diagnostic(name));
    }
    if let Some(reserved) = RESERVED_NAME_PREFIXES
        .iter()
        .find(|reserved| spec.name.starts_with(**reserved))
    {
        found.push(Diagnostic::error(
            NAME_RESERVED,
            format!(
                "the block is registered as {:?}; {reserved:?} is reserved for built-in blocks",
                spec.name
            ),
        ));
    }

    // --- Routes -----------------------------------------------------------
    // A guest serves exactly the one prefix its name produces
    // ([`validate_static`] builds the accepted spec that way), so a spec
    // carrying any other prefix was not produced by these rules — it is a
    // route claim, and it is refused as one.
    let prefix = format!("/b/{name}/");
    check_route_prefix(&prefix, builtin_routes, others, &mut found);
    for route in &spec.routes {
        if route.prefix != prefix {
            found.push(Diagnostic::error(
                ROUTE_PREFIX_CODE,
                format!(
                    "the block claims the route {:?}; a block named {name:?} serves {prefix:?} \
                     and nothing else",
                    route.prefix
                ),
            ));
        }
    }

    // --- Capabilities -----------------------------------------------------
    check_capabilities(name, None, &spec.capabilities, &mut found);

    if found.is_empty() {
        Ok(())
    } else {
        Err(found)
    }
}

/// The route rules for one block's prefix: normalized, and colliding with
/// neither a built-in route nor another block's.
///
/// The block serves exactly one prefix, derived from its name. So the first
/// rule is what a malformed name turns into once it reaches the router:
/// `name-format` says the name is illegal, `route-prefix` says the route it
/// would produce is unsafe to match by prefix.
fn check_route_prefix(
    prefix: &str,
    builtin_routes: &[&str],
    others: &[DynamicBlockSpec],
    found: &mut Vec<Diagnostic>,
) {
    if !is_normalized_prefix(prefix) {
        found.push(Diagnostic::error(
            ROUTE_PREFIX_CODE,
            format!("{prefix:?} is not a normalized route prefix"),
        ));
    }
    for builtin in builtin_routes {
        if prefixes_overlap(prefix, builtin) {
            found.push(Diagnostic::error(
                ROUTE_COLLISION,
                format!("{prefix:?} collides with the built-in route {builtin:?}"),
            ));
        }
    }
    for other in others {
        for route in &other.routes {
            if prefixes_overlap(prefix, &route.prefix) {
                found.push(Diagnostic::error(
                    ROUTE_COLLISION,
                    format!(
                        "{prefix:?} collides with {:?}, served by {:?}",
                        route.prefix, other.name
                    ),
                ));
            }
        }
    }
}

/// The §6.5 capability rules.
///
/// Every one of them is "the guest may only name things inside its own
/// namespace, and may not have the capability at all otherwise". The
/// allowlists are three-state ([`Allowlist`]): `None` is the sandboxed
/// default and always passes, `Any` never does — an unrestricted capability
/// is by definition outside any namespace — and `Only` is checked entry by
/// entry.
///
/// `requires` is the guest's own `BlockInfo::requires`, or `None` for a caller
/// that has no `BlockInfo` to read it from ([`validate_spec`]). It gates only
/// the `callable_blocks`/`requires` agreement rule: with one half of the pair
/// missing there is nothing to compare, and inventing an empty set for the
/// other half would refuse every guest that legitimately uses a service.
fn check_capabilities(
    name: &str,
    requires: Option<&[String]>,
    capabilities: &BlockCapabilities,
    found: &mut Vec<Diagnostic>,
) {
    // Destructured exhaustively, never read field by field.
    //
    // This function is the sandbox's authority gate: whatever it does not
    // refuse is granted to an untrusted guest verbatim, because the accepted
    // spec carries the guest's own declaration. A capability the producer
    // adds that nobody enumerated here would therefore be handed over in
    // silence — which is exactly how `headers` (a policy that can give a
    // guest the admin session cookie on its own route) went unnoticed.
    // Destructuring makes the compiler the enforcement: a new field upstream
    // breaks this build, the same way `capabilities_eq` in `control.rs` does.
    let BlockCapabilities {
        collections,
        raw_sql,
        ddl,
        // MAY be true (spec amendment 10). The structured schema ops are
        // authorized on the table as well as on the schema sentinel, so they
        // cannot reach outside `collections`; raw `ddl` runs an arbitrary
        // statement and can, which is why only that one is refused.
        schema: _schema,
        storage_folders,
        crypto,
        network,
        config,
        vector_indexes,
        callable_blocks,
        headers,
    } = capabilities;
    let HeaderPolicy {
        readable,
        writable,
        // `masked` only ever ADDS to the default-denied set, in both
        // directions, so it can only narrow what a guest sees or sends.
        masked: _masked,
    } = headers;

    let folder = format!("site/{name}");
    // The runtime's own spelling of the block's resource namespace, never a
    // restatement of it: `site/my-shop` owns `site__my_shop__*` collections
    // and `SITE__MY_SHOP__*` config keys. A hyphen cannot appear there — the
    // database strips it from a table name before building SQL, so
    // `site__my-shop__notes` would be authorized as `site/my-shop`'s and then
    // read and written as `site__myshop__notes`, the table of guest `myshop`.
    let collection_prefix = collection_prefix(name);
    let config_prefix = collection_prefix.to_uppercase();

    check_allowlist(
        collections,
        CAP_COLLECTION,
        "collection",
        &format!("{collection_prefix}*"),
        |entry| entry.starts_with(&collection_prefix),
        found,
    );
    // Inside the namespace is not enough: the name reaching SQL must be the
    // name that was authorized, byte for byte, and only a plain identifier
    // survives the database's identifier handling unchanged.
    if let Allowlist::Only(entries) = collections {
        for entry in entries
            .iter()
            .filter(|entry| !is_plain_collection_name(entry))
        {
            found.push(Diagnostic::error(
                CAP_COLLECTION,
                format!(
                    "the collection {entry:?} is not a plain identifier; a collection name is \
                     lowercase letters, digits and `_` only"
                ),
            ));
        }
    }
    check_allowlist(
        storage_folders,
        CAP_FOLDER,
        "storage folder",
        &folder,
        |entry| {
            // Three hazards, all of which look like a legal entry:
            //
            // * an entry ending in `/` matches NOTHING upstream
            //   (`BlockCapabilities::allows_storage_folder`), so granting one
            //   hands back a capability that silently denies everything;
            // * an entry with an empty, `.` or `..` segment is not a folder
            //   name at all — `site/hello/../other` textually sits under
            //   `site/hello` while naming a sibling, and nothing normalizes
            //   it. `wrap::is_traversal_safe_path` is the producer's own rule
            //   for that shape, used here rather than restated;
            // * anything outside the block's own folder.
            !entry.ends_with('/')
                && wrap::is_traversal_safe_path(entry)
                && (entry == folder || entry.starts_with(&format!("{folder}/")))
        },
        found,
    );
    check_allowlist(
        config,
        CAP_CONFIG,
        "config key",
        &format!("{config_prefix}*"),
        |entry| entry.starts_with(&config_prefix),
        found,
    );

    if *raw_sql {
        found.push(Diagnostic::error(
            CAP_RAW_SQL,
            "raw SQL is never granted to a guest; use the typed database ops",
        ));
    }
    if *ddl {
        found.push(Diagnostic::error(
            CAP_DDL,
            "raw DDL is never granted to a guest; declare `schema: true` for \
             the structured table ops on your own collections",
        ));
    }
    if *crypto {
        found.push(Diagnostic::error(
            CAP_CRYPTO,
            "the crypto service is never granted to a guest",
        ));
    }
    if network.is_enabled() {
        found.push(Diagnostic::error(
            CAP_NETWORK,
            "guests have no network access; a page talks to other origins, a block does not",
        ));
    }
    if vector_indexes.is_enabled() {
        found.push(Diagnostic::error(
            CAP_VECTOR,
            "vector indexes are never granted to a guest",
        ));
    }
    if !readable.is_empty() || !writable.is_empty() {
        found.push(Diagnostic::error(
            CAP_HEADERS,
            format!(
                "the guest declares readable [{}] and writable [{}] sensitive headers; a \
                 sandboxed block is never granted either — it serves an unauthenticated \
                 route, and the admin session cookie and `authorization` travel on it",
                readable.join(", "),
                writable.join(", "),
            ),
        ));
    }

    // `callable_blocks` and `requires` are two spellings of one fact — which
    // platform services this guest uses. They must agree, so the registry's
    // dependency view and the capability the runtime enforces cannot describe
    // different blocks.
    let allowed: BTreeSet<&str> = ALLOWED_CALLABLE_BLOCKS.iter().copied().collect();
    // `None` for `Any`: an unrestricted set names no services, so there is
    // nothing to compare `requires` against once it has been refused.
    let declared: Option<BTreeSet<&str>> = match callable_blocks {
        Allowlist::None => Some(BTreeSet::new()),
        Allowlist::Any => {
            found.push(Diagnostic::error(
                CAP_CALLABLE,
                format!(
                    "`callable_blocks` must name exactly the services the guest uses; \
                     allowed: {}",
                    ALLOWED_CALLABLE_BLOCKS.join(", ")
                ),
            ));
            None
        }
        Allowlist::Only(entries) => {
            let declared: BTreeSet<&str> = entries.iter().map(String::as_str).collect();
            for entry in declared.difference(&allowed) {
                found.push(Diagnostic::error(
                    CAP_CALLABLE,
                    format!(
                        "a guest may not call {entry:?}; allowed: {}",
                        ALLOWED_CALLABLE_BLOCKS.join(", ")
                    ),
                ));
            }
            Some(declared)
        }
    };
    if let (Some(declared), Some(requires)) = (declared, requires) {
        let required: BTreeSet<&str> = requires.iter().map(String::as_str).collect();
        compare_requires(&declared, &required, found);
    }
}

/// Refuse an allowlist that is `Any`, or whose entries are not all inside the
/// block's namespace.
fn check_allowlist(
    allowlist: &Allowlist,
    code: &str,
    noun: &str,
    namespace: &str,
    inside: impl Fn(&str) -> bool,
    found: &mut Vec<Diagnostic>,
) {
    match allowlist {
        Allowlist::None => {}
        Allowlist::Any => found.push(Diagnostic::error(
            code,
            format!("every declared {noun} must be under {namespace:?}; `Any` is not a namespace"),
        )),
        Allowlist::Only(entries) => {
            for entry in entries {
                if !inside(entry) {
                    found.push(Diagnostic::error(
                        code,
                        format!("the {noun} {entry:?} is outside {namespace:?}"),
                    ));
                }
            }
        }
    }
}

/// The collection prefix guest `name` owns: `site__{name}__`, a hyphen in
/// `name` spelled `_` — [`wrap::resource_prefix`] of the block id, which is
/// the prefix WRAP's own-namespace rule maps back to `site/{name}`.
pub fn collection_prefix(name: &str) -> String {
    wrap::resource_prefix(&format!("site/{name}"))
}

/// Whether `entry` is a collection name the database uses as written:
/// non-empty, ASCII lowercase letters, digits and `_`.
fn is_plain_collection_name(entry: &str) -> bool {
    !entry.is_empty()
        && entry
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Refuse a `callable_blocks` set that does not equal `requires`.
fn compare_requires(
    declared: &BTreeSet<&str>,
    required: &BTreeSet<&str>,
    found: &mut Vec<Diagnostic>,
) {
    if declared != required {
        found.push(Diagnostic::error(
            CAP_REQUIRES_MISMATCH,
            format!(
                "`callable_blocks` is [{}] but `requires` is [{}]; they name the same services \
                 and must match",
                declared.iter().copied().collect::<Vec<_>>().join(", "),
                required.iter().copied().collect::<Vec<_>>().join(", "),
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use wafer_run::{AuthLevel, BlockEndpoint};

    use super::*;

    /// A guest that reports `name` and serves the one endpoint that name
    /// implies, so a fixture never trips `endpoint-outside-routes` by
    /// accident when a test changes the block's name.
    fn info(name: &str) -> BlockInfo {
        let short = name.strip_prefix("site/").unwrap_or(name);
        BlockInfo::new(name, "0.1.0", "http-handler@v1", "hello").endpoints(vec![
            BlockEndpoint::get(&format!("/b/{short}/"))
                .auth(AuthLevel::Public)
                .summary("hello"),
        ])
    }

    fn run(name: &str, info: &BlockInfo) -> Result<DynamicBlockSpec, Vec<Diagnostic>> {
        validate_static(
            name,
            info,
            "sha",
            &builtin_route_prefixes(),
            &[],
            &BTreeSet::new(),
        )
    }

    fn codes(result: &Result<DynamicBlockSpec, Vec<Diagnostic>>) -> Vec<&str> {
        result
            .as_ref()
            .err()
            .map(|found| {
                found
                    .iter()
                    // Every diagnostic this crate produces carries a code, so
                    // a `None` here is a producer that forgot one rather than
                    // a compiler diagnostic — and it must fail the assertion
                    // it lands in, not silently match nothing.
                    .map(|d| d.code.as_deref().unwrap_or("<no code>"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_well_formed_guest_produces_the_spec_the_runtime_is_handed() {
        let spec = run("hello", &info("site/hello")).expect("valid");
        assert_eq!(spec.name, "site/hello");
        assert_eq!(spec.artifact_sha256, "sha");
        assert_eq!(spec.routes.len(), 1);
        assert_eq!(spec.routes[0].prefix, "/b/hello/");
        assert_eq!(spec.routes[0].access, RouteAccessKind::Public);
        assert_eq!(spec.wafer_guest_version, 0);
        // Declaring nothing means `none()`, not "the host decides".
        assert!(!spec.capabilities.raw_sql);
        assert!(!spec.capabilities.collections.is_enabled());
    }

    /// A guest that declares nothing is refused, and refused with a message
    /// that says what to declare. The accepted spec's one `Public` prefix is a
    /// floor that only the guest's own endpoint declarations can lift, so a
    /// guest with none has said nothing about what any of its paths are for.
    #[test]
    fn a_guest_that_declares_no_endpoints_is_refused() {
        let bare = BlockInfo::new("site/hello", "0.1.0", "http-handler@v1", "hello");
        let result = run("hello", &bare);
        let found = codes(&result);
        assert!(found.contains(&ENDPOINTS_EMPTY), "{found:?}");
    }

    /// A malformed name fires both name rules: the name itself is illegal,
    /// and the prefix it produces cannot be matched safely. Neither subsumes
    /// the other — this is what keeps `route-prefix` a real check rather than
    /// a comment.
    #[test]
    fn a_malformed_name_refuses_both_the_name_and_the_prefix_it_would_produce() {
        let result = run("a/../admin", &info("site/a/../admin"));
        let found = codes(&result);
        assert!(found.contains(&NAME_FORMAT), "{found:?}");
        assert!(found.contains(&ROUTE_PREFIX_CODE), "{found:?}");
    }

    /// A seed spec whose name is not `site/{name}` is refused HERE, by the
    /// rules, rather than by `wafer_run`'s registration inside a rebuild —
    /// which happens after the importer has written its blobs, its artifacts
    /// and a generation row, leaving the instance unable to seed again.
    #[test]
    fn a_spec_outside_the_site_namespace_is_refused_rather_than_validated_as_a_short_name() {
        let spec = DynamicBlockSpec {
            name: "hello".to_string(),
            artifact_sha256: "sha".to_string(),
            routes: vec![DynamicRoute {
                prefix: "/b/hello/".to_string(),
                access: RouteAccessKind::Public,
            }],
            capabilities: wafer_block::BlockCapabilities::none(),
            wafer_guest_version: 0,
        };
        let found = validate_spec(&spec, &builtin_route_prefixes(), &[]).expect_err("refused");
        assert_eq!(
            found
                .iter()
                .map(|d| d.code.as_deref().unwrap_or(""))
                .collect::<Vec<_>>(),
            vec![NAME_MISMATCH]
        );
    }

    #[test]
    fn a_reserved_name_is_reported_as_reserved_as_well_as_mismatched() {
        let result = run("hello", &info("impresspress/admin"));
        let found = codes(&result);
        assert!(found.contains(&NAME_RESERVED), "{found:?}");
        assert!(found.contains(&NAME_MISMATCH), "{found:?}");
    }

    #[test]
    fn every_builtin_prefix_is_defended_in_both_directions() {
        // A block named after a built-in prefix's own segment.
        let admin = run("admin", &info("site/admin"));
        assert!(codes(&admin).contains(&ROUTE_COLLISION), "{admin:?}");
        // And the sandbox's own prefix, which is an extra route rather than a
        // member of `routing::ROUTES`.
        let dev = run("dev", &info("site/dev"));
        assert!(codes(&dev).contains(&ROUTE_COLLISION), "{dev:?}");
        // `admins` is now ACCEPTED, and that is a fix rather than a
        // regression. This used to collide: `/b/admin` was registered as a
        // second, slash-less route and the router matched by `starts_with`,
        // so `/b/admin` really did swallow `/b/admins/…` and staging had to
        // refuse the name on the router's behalf.
        //
        // `routing::route_prefix_matches` now matches the bare form by
        // EQUALITY (`prefix.strip_suffix('/') == Some(path)`) instead, so
        // `/b/admin/` serves exactly `/b/admin` and everything genuinely
        // under it. `/b/admins/` is not under it by any of the three arms.
        // The hazard was closed at the router, which is the only place it
        // could be closed for every caller rather than for blocks that
        // happen to go through staging — so the name is free.
        assert!(run("admins", &info("site/admins")).is_ok());
        // A name that shares a leading substring with a slash-terminated
        // built-in prefix does NOT collide: `/b/storag/` is not under
        // `/b/storage/` and vice versa.
        assert!(run("storag", &info("site/storag")).is_ok());
    }

    #[test]
    fn a_prefix_nested_under_another_dynamic_block_is_refused() {
        let other = DynamicBlockSpec {
            name: "site/shop".to_string(),
            artifact_sha256: "other".to_string(),
            routes: vec![DynamicRoute {
                prefix: "/b/shop/".to_string(),
                access: RouteAccessKind::Public,
            }],
            capabilities: BlockCapabilities::none(),
            wafer_guest_version: 0,
        };
        let result = validate_static(
            "shop",
            &info("site/shop"),
            "sha",
            &builtin_route_prefixes(),
            std::slice::from_ref(&other),
            &BTreeSet::new(),
        );
        assert!(codes(&result).contains(&ROUTE_COLLISION), "{result:?}");
    }

    #[test]
    fn an_endpoint_outside_the_blocks_prefix_is_refused() {
        let mut declared = info("site/hello");
        declared.endpoints.push(
            BlockEndpoint::post("/b/auth/api/reset-password")
                .auth(AuthLevel::Public)
                .summary("x"),
        );
        assert!(codes(&run("hello", &declared)).contains(&ENDPOINT_OUTSIDE_ROUTES));
    }

    #[test]
    fn a_tool_name_is_refused_when_claimed_elsewhere_and_when_claimed_twice_here() {
        let mut declared = info("site/hello");
        declared.endpoints = vec![
            BlockEndpoint::get("/b/hello/a")
                .auth(AuthLevel::Public)
                .summary("a")
                .agent_tool("greet", "a"),
            BlockEndpoint::get("/b/hello/b")
                .auth(AuthLevel::Public)
                .summary("b")
                .agent_tool("greet", "b"),
        ];
        // Self-duplicate: one of the two endpoints has to change.
        let result = run("hello", &declared);
        let found = codes(&result);
        assert_eq!(
            found.iter().filter(|c| **c == TOOL_NAME_DUPLICATE).count(),
            1,
            "{found:?}"
        );

        // Claimed by somebody else: both endpoints are refused, because
        // either one alone would still shadow the existing tool.
        let claimed = BTreeSet::from(["greet".to_string()]);
        let result = validate_static(
            "hello",
            &declared,
            "sha",
            &builtin_route_prefixes(),
            &[],
            &claimed,
        );
        assert_eq!(
            codes(&result)
                .iter()
                .filter(|c| **c == TOOL_NAME_DUPLICATE)
                .count(),
            2,
        );
    }

    #[test]
    fn the_namespaced_capability_set_is_accepted_and_carried_verbatim() {
        let mut declared = info("site/hello");
        declared.requires = vec!["wafer-run/database".to_string()];
        declared.capabilities = Some(BlockCapabilities {
            collections: Allowlist::Only(BTreeSet::from(["site__hello__notes".to_string()])),
            schema: true,
            storage_folders: Allowlist::Only(BTreeSet::from(["site/hello".to_string()])),
            config: Allowlist::Only(BTreeSet::from(["SITE__HELLO__GREETING".to_string()])),
            callable_blocks: Allowlist::Only(BTreeSet::from(["wafer-run/database".to_string()])),
            ..BlockCapabilities::none()
        });
        let spec = run("hello", &declared).expect("valid");
        assert!(spec.capabilities.schema);
        assert!(spec.capabilities.allows_collection("site__hello__notes"));
        assert!(spec.capabilities.allows_storage_folder("site/hello/a.json"));
    }

    /// A hyphenated guest's namespace is the runtime's spelling of it:
    /// `site/my-shop` owns `site__my_shop__*` and `SITE__MY_SHOP__*`, which
    /// WRAP maps back to `site/my-shop` and the database uses as written.
    #[test]
    fn a_hyphenated_guest_claims_the_underscore_spelled_namespace() {
        let mut declared = info("site/my-shop");
        declared.requires = vec!["wafer-run/database".to_string()];
        declared.capabilities = Some(BlockCapabilities {
            collections: Allowlist::Only(BTreeSet::from(["site__my_shop__notes".to_string()])),
            schema: true,
            config: Allowlist::Only(BTreeSet::from(["SITE__MY_SHOP__GREETING".to_string()])),
            callable_blocks: Allowlist::Only(BTreeSet::from(["wafer-run/database".to_string()])),
            ..BlockCapabilities::none()
        });
        let spec = run("my-shop", &declared).expect("the underscore spelling is the guest's own");
        assert!(spec.capabilities.allows_collection("site__my_shop__notes"));
        assert_eq!(
            wrap::resource_owner("site__my_shop__notes").as_deref(),
            Some("site/my-shop"),
        );
        assert_eq!(
            wrap::resource_owner("SITE__MY_SHOP__GREETING").as_deref(),
            Some("site/my-shop"),
        );
    }

    /// The hyphenated spelling is refused for both: as a collection it is
    /// read and written as `site__myshop__*` — another guest's tables — and
    /// as a config key it is not the prefix the runtime derives for the id.
    /// So is a twin guest's namespace.
    #[test]
    fn a_hyphenated_guest_cannot_claim_the_hyphen_or_the_twin_spelling() {
        for (collection, config) in [
            ("site__my-shop__notes", "SITE__MY-SHOP__GREETING"),
            ("site__myshop__notes", "SITE__MYSHOP__GREETING"),
        ] {
            let mut declared = info("site/my-shop");
            declared.capabilities = Some(BlockCapabilities {
                collections: Allowlist::Only(BTreeSet::from([collection.to_string()])),
                config: Allowlist::Only(BTreeSet::from([config.to_string()])),
                ..BlockCapabilities::none()
            });
            let result = run("my-shop", &declared);
            let found = codes(&result);
            assert!(found.contains(&CAP_COLLECTION), "{collection}: {found:?}");
            assert!(found.contains(&CAP_CONFIG), "{config}: {found:?}");
        }
    }

    /// Inside the namespace but not a plain identifier: the suffix would be
    /// stripped on the way to SQL, so the table used is not the one claimed.
    #[test]
    fn a_collection_that_is_not_a_plain_identifier_is_refused() {
        for collection in ["site__hello__my-notes", "site__hello__Notes"] {
            let mut declared = info("site/hello");
            declared.capabilities = Some(BlockCapabilities {
                collections: Allowlist::Only(BTreeSet::from([collection.to_string()])),
                ..BlockCapabilities::none()
            });
            let result = run("hello", &declared);
            assert!(
                codes(&result).contains(&CAP_COLLECTION),
                "{collection}: {result:?}"
            );
        }
    }

    #[test]
    fn a_capability_outside_the_namespace_is_refused_per_capability() {
        let mut declared = info("site/hello");
        declared.capabilities = Some(BlockCapabilities {
            collections: Allowlist::Only(BTreeSet::from([
                "impresspress__admin__variables".to_string()
            ])),
            storage_folders: Allowlist::Only(BTreeSet::from(["site/other".to_string()])),
            config: Allowlist::Only(BTreeSet::from(["APP_NAME".to_string()])),
            ..BlockCapabilities::none()
        });
        let result = run("hello", &declared);
        let found = codes(&result);
        assert!(found.contains(&CAP_COLLECTION), "{found:?}");
        assert!(found.contains(&CAP_FOLDER), "{found:?}");
        assert!(found.contains(&CAP_CONFIG), "{found:?}");
    }

    /// A storage-folder entry ending in `/` matches nothing upstream, so
    /// granting it would hand the guest a capability that silently denies
    /// every key it names.
    #[test]
    fn a_storage_folder_with_a_trailing_slash_is_refused() {
        let mut declared = info("site/hello");
        declared.capabilities = Some(BlockCapabilities {
            storage_folders: Allowlist::Only(BTreeSet::from(["site/hello/".to_string()])),
            ..BlockCapabilities::none()
        });
        assert!(codes(&run("hello", &declared)).contains(&CAP_FOLDER));
    }

    #[test]
    fn an_unrestricted_allowlist_is_never_a_namespace() {
        let mut declared = info("site/hello");
        declared.capabilities = Some(BlockCapabilities::unrestricted());
        let result = run("hello", &declared);
        let found = codes(&result);
        for expected in [
            CAP_COLLECTION,
            CAP_FOLDER,
            CAP_CONFIG,
            CAP_RAW_SQL,
            CAP_DDL,
            CAP_CRYPTO,
            CAP_NETWORK,
            CAP_VECTOR,
            CAP_CALLABLE,
        ] {
            assert!(found.contains(&expected), "missing {expected}: {found:?}");
        }
    }

    #[test]
    fn schema_alone_is_not_a_refusal() {
        let mut declared = info("site/hello");
        declared.capabilities = Some(BlockCapabilities {
            schema: true,
            ..BlockCapabilities::none()
        });
        assert!(run("hello", &declared).is_ok());
    }

    #[test]
    fn callable_blocks_and_requires_must_name_the_same_services() {
        let mut declared = info("site/hello");
        declared.requires = vec!["wafer-run/database".to_string()];
        assert!(codes(&run("hello", &declared)).contains(&CAP_REQUIRES_MISMATCH));

        declared.capabilities = Some(BlockCapabilities {
            callable_blocks: Allowlist::Only(BTreeSet::from(["wafer-run/database".to_string()])),
            ..BlockCapabilities::none()
        });
        assert!(run("hello", &declared).is_ok());

        // A service outside the four is refused even when `requires` agrees.
        declared.requires = vec!["impresspress/products".to_string()];
        declared.capabilities = Some(BlockCapabilities {
            callable_blocks: Allowlist::Only(BTreeSet::from(["impresspress/products".to_string()])),
            ..BlockCapabilities::none()
        });
        let result = run("hello", &declared);
        let found = codes(&result);
        assert!(found.contains(&CAP_CALLABLE), "{found:?}");
        assert!(!found.contains(&CAP_REQUIRES_MISMATCH), "{found:?}");
    }

    #[test]
    fn prefix_normalization_and_overlap_are_about_whole_segments() {
        assert!(is_normalized_prefix("/b/hello/"));
        assert!(!is_normalized_prefix("/b/hello"));
        assert!(!is_normalized_prefix("b/hello/"));
        assert!(!is_normalized_prefix("/b//hello/"));
        assert!(!is_normalized_prefix("/b/../hello/"));
        assert!(!is_normalized_prefix("/"));

        assert!(prefixes_overlap("/b/admin/", "/b/admin"));
        assert!(prefixes_overlap("/b/admin", "/b/admin/settings"));
        assert!(!prefixes_overlap("/b/hell/", "/b/hello/"));
    }

    #[test]
    fn a_guest_refusal_becomes_a_coded_diagnostic() {
        let failure =
            ValidationFailure::new(super::super::control::ValidationStage::Init, "trap: oops");
        let diagnostic = Diagnostic::guest(&failure);
        assert_eq!(diagnostic.code.as_deref(), Some("guest-init"));
        assert_eq!(diagnostic.message, "trap: oops");
        assert_eq!(diagnostic.severity, Severity::Error);
    }

    #[test]
    fn the_oversize_refusal_reports_the_limit_it_enforces() {
        let diagnostic = Diagnostic::artifact_too_large(MAX_ARTIFACT_BYTES + 1);
        assert_eq!(diagnostic.code.as_deref(), Some(ARTIFACT_TOO_LARGE));
        assert!(
            diagnostic.message.contains(&MAX_ARTIFACT_BYTES.to_string()),
            "{}",
            diagnostic.message
        );
    }

    /// The page sends compiler diagnostics, and rustc reports plenty that
    /// have no span. Omitting `file`/`line`/`column` has to mean "no
    /// position", not "malformed request" — the published input schema says
    /// they are optional and serde must agree.
    #[test]
    fn a_diagnostic_without_a_source_position_deserializes() {
        let parsed: Diagnostic = serde_json::from_str(
            r#"{"severity":"warning","code":"unused_imports","message":"unused import"}"#,
        )
        .expect("a diagnostic with no span is well-formed");
        assert_eq!(parsed.severity, Severity::Warning);
        assert_eq!(parsed.file, None);
        assert_eq!(parsed.line, None);
        assert_eq!(parsed.column, None);
    }

    #[test]
    fn severity_spells_itself_the_same_way_in_json_as_in_the_source() {
        for (severity, spelling) in [
            (Severity::Error, "error"),
            (Severity::Warning, "warning"),
            (Severity::Note, "note"),
            (Severity::Help, "help"),
        ] {
            assert_eq!(
                serde_json::to_value(severity).expect("serialize"),
                serde_json::json!(spelling),
            );
        }
    }

    #[test]
    fn the_builtin_prefix_list_covers_the_router_and_the_sandbox_itself() {
        let prefixes = builtin_route_prefixes();
        assert_eq!(prefixes.len(), crate::routing::ROUTES.len() + 1);
        assert!(prefixes.contains(&super::super::ROUTE_PREFIX));
        assert!(prefixes.contains(&"/b/admin/"));
    }
}
