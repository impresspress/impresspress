//! `impresspress/dev` — the browser development sandbox's control plane.
//!
//! The block owns the publication ledger (generations, builds, the activation
//! journal), validates and activates what the `/b/dev` page produces, and
//! publishes the resulting site into `wafer-run/web/site`. It does **not** own
//! the runtime: building and swapping a `Wafer` is the host's job, reached
//! through the [`control::RuntimeControl`] seam.
//!
//! # Why this block is hand-written
//!
//! Every other impresspress feature block goes through
//! [`crate::impresspress_feature_block!`], which generates a zero-argument
//! `new()`. This block's constructor takes the shared state that carries the
//! `RuntimeControl` handle, so the struct and `impl Block` are written out in
//! the same shape the macro would produce (as `impresspress/llm` already is).
//!
//! # Why it is not in the block manifest
//!
//! Registration happens from the consumer, via `ImpresspressBuilder::extra_block`
//! and `add_route`, gated on the `browser-devtools` feature — the block is
//! registered wherever the feature is compiled in, and only its `/b/dev`
//! ROUTE is keyed on the deployment being a workspace rather than an exported
//! bundle (`SandboxMode`, spec amendment 19) — never on a stored variable.
//! `feature_block_manifest!` only enumerates blocks whose constructors take no
//! arguments, and — more to the point — the sandbox's security model
//! (design §13) depends on this block being absent from every normal
//! deployment, not merely disabled in one.

pub mod activation;
pub mod artifacts;
pub mod assets;
pub mod blobs;
pub mod blocks_api;
pub mod bypass;
pub mod contracts;
pub mod control;
pub mod data_snapshot;
pub mod enter;
pub mod export;
pub mod files;
pub mod gc;
pub mod generation;
pub mod generations_api;
pub mod migrations;
pub mod page;
pub mod paths;
pub mod publisher;
pub mod repo;
pub mod retention;
pub mod scaffold;
pub mod seed;
pub mod status;
pub mod tools;
pub mod validation;
pub mod workspace;
pub mod zip;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(test)]
mod error_mapping_tests;

use std::sync::Arc;

use wafer_run::{
    context::Context, Block, BlockInfo, CollectionSchema, HttpMethod, InputStream, InstanceMode,
    LifecycleEvent, Message, OutputStream, WaferError,
};

pub use self::{
    bypass::{BypassRule, BypassRules},
    control::{
        DynamicBlockSpec, DynamicRoute, RouteAccessKind, RuntimeControl, ShellSource,
        ValidationFailure, ValidationStage,
    },
};
use crate::{
    endpoint_match::{self, request_schema_of, response_schema_of, EndpointRoute},
    http::ResponseBuilder,
};

/// Registered block name.
pub const BLOCK_NAME: &str = "impresspress/dev";

/// The route prefix the workspace is served under. Registered as a
/// [`crate::routing::ExtraRoute`] at `Admin`, so the router — not any handler
/// in this module — is what keeps the sandbox admin-only.
pub const ROUTE_PREFIX: &str = "/b/dev";

/// The one-click entry page (2026-10-02 amendment to design §4): the single
/// path under [`ROUTE_PREFIX`] an anonymous visitor can reach. See [`enter`].
pub const ENTER_PATH: &str = "/b/dev/enter";

/// Every route a **workspace** registers for this block, in the order the
/// router must be given them.
///
/// The order is the point, and is why the list is stated once here rather
/// than at each registration site (the browser runtime factory and the test
/// fixture): `routing::route_to_block` takes the FIRST extra route whose
/// prefix matches, and `/b/dev/enter` also matches `/b/dev`. The entry row
/// has to come first, or the `Admin` prefix would claim it and the page whose
/// whole purpose is to sign a visitor in would demand a session.
///
/// An exported bundle registers none of them ([`DevBlock::runtime_only`]).
pub const WORKSPACE_ROUTES: &[(&str, crate::routing::RouteAccess)] = &[
    (ENTER_PATH, crate::routing::RouteAccess::Public),
    (ROUTE_PREFIX, crate::routing::RouteAccess::Admin),
];

/// ABI version of the guest SDK crate (`crates/wafer-guest`) that scaffolded
/// blocks depend on. Must equal `wafer_guest::WAFER_GUEST_VERSION`, which the
/// parity test asserts. Published in the status response so the page can
/// tell whether a block it compiled earlier was built against an older SDK.
pub const WAFER_GUEST_VERSION: u32 = 2;

/// In-block dispatch targets, one per declared HTTP endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// `GET /b/dev` — the workspace document.
    Page,
    /// `GET /b/dev/enter` — the one-click entry page.
    Enter,
    /// `GET /b/dev/static/dev.js`
    PageScript,
    /// `GET /b/dev/static/dev.css`
    PageStylesheet,
    /// `GET /b/dev/static/compiler-adapter.js`
    PageCompilerAdapter,
    /// `GET /b/dev/api/status`
    ApiStatus,
    /// `GET /b/dev/api/files`
    ApiFilesList,
    /// `POST /b/dev/api/files/read`
    ApiFilesRead,
    /// `POST /b/dev/api/files/write`
    ApiFilesWrite,
    /// `POST /b/dev/api/files/write-batch`
    ApiFilesWriteBatch,
    /// `POST /b/dev/api/files/delete`
    ApiFilesDelete,
    /// `GET /b/dev/api/generations`
    ApiGenerations,
    /// `GET /b/dev/api/generations/{id}`
    ApiGenerationDetail,
    /// `POST /b/dev/api/generations/{id}/rollback`
    ApiGenerationRollback,
    /// `POST /b/dev/api/builds/stage`
    ApiBuildStage,
    /// `POST /b/dev/api/blocks`
    ApiBlockCreate,
    /// `POST /b/dev/api/blocks/{name}/remove`
    ApiBlockRemove,
    /// `GET /b/dev/api/reference`
    ApiReference,
    /// `GET /b/dev/api/guest`
    ApiGuest,
    /// `GET /b/dev/api/tools.json`
    ApiToolsJson,
    /// `GET /b/dev/api/export/manifest`
    ApiExportManifest,
    /// `GET /b/dev/api/export`
    ApiExport,
}

/// The block's HTTP surface: what `handle()` dispatches on and what the
/// workspace `info()` generates its endpoints from (an exported bundle
/// declares none of it; see [`DevBlock::runtime_only`]). Every row but one is
/// `Admin` (design §13); the router is the sole gate, so the declaration is
/// what pins that tier where the router can enforce it. The exception is the
/// entry page, which is `Public` because signing the visitor in is its job
/// ([`enter`]). The matcher binds
/// `{id}` / `{name}` into `req.param.*` for the handlers' `msg.var` readers.
///
/// Reading and deleting are `POST`s, not a `GET` with a query and a `DELETE`
/// with one: a workspace path is a `/`-separated string with its own
/// separators, and putting it in the URL would mean every client had to
/// percent-encode it correctly to name a file in a subdirectory. The path
/// travels in the JSON body, where it needs no encoding at all.
pub const ROUTES: &[EndpointRoute<Route>] = &[
    // The document and its three assets carry no schemas — there is no JSON
    // contract to describe, and `has_schema()` therefore keeps all four out
    // of `/openapi.json` exactly as it keeps the HTML pages of every other
    // block out.
    EndpointRoute::admin(HttpMethod::Get, "/b/dev", Route::Page)
        .summary("The workspace document")
        .description(
            "The sandbox's HTML workspace: file tree and editor, the live site in a \
             sandboxed iframe, the activation progress panel, and the page-scoped agent \
             tools registered from /b/dev/api/tools.json.",
        ),
    // `Public`, and the only row that is: this page is what turns an
    // anonymous visitor into the signed-in sandbox admin, so it cannot ask
    // for a session. It is reachable without one only because
    // `WORKSPACE_ROUTES` registers its path ahead of the `Admin` prefix.
    EndpointRoute::public(HttpMethod::Get, ENTER_PATH, Route::Enter)
        .summary("One-click entry to the workspace")
        .description(
            "Signs the visitor in as this browser-local sandbox's bootstrap admin, \
             through the auth block's own login endpoint, and opens /b/dev. Exists \
             only in a sandbox workspace; an exported site has no such page.",
        ),
    EndpointRoute::admin(HttpMethod::Get, "/b/dev/static/dev.js", Route::PageScript)
        .summary("The workspace page's script"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/dev/static/dev.css",
        Route::PageStylesheet,
    )
    .summary("The workspace page's stylesheet"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/dev/static/compiler-adapter.js",
        Route::PageCompilerAdapter,
    )
    .summary("The page half of the in-browser compiler protocol"),
    EndpointRoute::admin(HttpMethod::Get, "/b/dev/api/status", Route::ApiStatus)
        .summary("Sandbox status")
        .output(response_schema_of::<contracts::StatusResponse>),
    EndpointRoute::admin(HttpMethod::Get, "/b/dev/api/files", Route::ApiFilesList)
        .summary("List workspace files")
        .query_params(request_schema_of::<contracts::FileListQuery>)
        .output(response_schema_of::<contracts::FileListResponse>),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/dev/api/files/read",
        Route::ApiFilesRead,
    )
    .summary("Read a workspace file")
    .input(request_schema_of::<contracts::FileReadRequest>)
    .output(response_schema_of::<contracts::FileReadResponse>),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/dev/api/files/write",
        Route::ApiFilesWrite,
    )
    .summary("Write a workspace file")
    .input(request_schema_of::<contracts::FileWriteRequest>)
    .output(response_schema_of::<contracts::FileWriteResponse>),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/dev/api/files/write-batch",
        Route::ApiFilesWriteBatch,
    )
    .summary("Write several workspace files as one change")
    .input(request_schema_of::<contracts::FileWriteBatchRequest>)
    .output(response_schema_of::<contracts::FileWriteBatchResponse>),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/dev/api/files/delete",
        Route::ApiFilesDelete,
    )
    .summary("Delete a workspace file")
    .input(request_schema_of::<contracts::FileDeleteRequest>)
    .output(response_schema_of::<contracts::FileDeleteResponse>),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/dev/api/generations",
        Route::ApiGenerations,
    )
    .summary("List generations")
    .query_params(request_schema_of::<contracts::GenerationListQuery>)
    .output(response_schema_of::<contracts::GenerationListResponse>),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/dev/api/generations/{id}",
        Route::ApiGenerationDetail,
    )
    .summary("Read one generation")
    .path_params(request_schema_of::<contracts::GenerationPathParams>)
    .output(response_schema_of::<contracts::GenerationDetail>),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/dev/api/generations/{id}/rollback",
        Route::ApiGenerationRollback,
    )
    .summary("Republish an earlier generation")
    .path_params(request_schema_of::<contracts::GenerationPathParams>)
    .output(response_schema_of::<contracts::ActivationResponse>),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/dev/api/builds/stage",
        Route::ApiBuildStage,
    )
    .summary("Stage and activate a compiled block")
    .input(request_schema_of::<contracts::StageBuildRequest>)
    .output(response_schema_of::<contracts::StageBuildResponse>),
    EndpointRoute::admin(HttpMethod::Post, "/b/dev/api/blocks", Route::ApiBlockCreate)
        .summary("Scaffold a new block from a template")
        .description(
            "Writes blocks/<name>/{Cargo.toml, src/lib.rs}, instantiated from the \
             template. The manifest depends on the guest SDK crate by path \
             (`../../wafer_guest`), which the sandbox provides; the block never \
             contains the SDK. Writing source activates nothing; compile the block to \
             make it serve.",
        )
        .input(request_schema_of::<contracts::CreateBlockRequest>)
        .output(response_schema_of::<contracts::CreateBlockResponse>),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/dev/api/blocks/{name}/remove",
        Route::ApiBlockRemove,
    )
    .summary("Remove a block from the runtime")
    .path_params(request_schema_of::<contracts::BlockPathParams>)
    .output(response_schema_of::<contracts::ActivationResponse>),
    EndpointRoute::admin(HttpMethod::Get, "/b/dev/api/reference", Route::ApiReference)
        .summary("The backend-block authoring reference")
        .description(
            "The guide for writing a block: the wafer_guest crate's API, the database / \
             storage / config services, the namespace and capability rules, the limits, \
             the diagnostic codes, and both templates in full.",
        )
        .output(response_schema_of::<contracts::ReferenceResponse>),
    EndpointRoute::admin(HttpMethod::Get, "/b/dev/api/guest", Route::ApiGuest)
        .summary("The guest SDK crate the compiler builds against")
        .description(
            "The `wafer_guest` crate every block depends on by path, plus the block the \
             compiler builds once at session start to warm it. The page fetches this before \
             starting the toolchain; an agent only needs it to read the SDK's source.",
        )
        .output(response_schema_of::<contracts::GuestResponse>),
    // Deliberately carries no `.agent_tool(..)`: this endpoint IS a tool
    // manifest, and a tool that named itself in its own output is exactly
    // the leak `dev_tools_manifest.rs`'s
    // `no_dev_or_shop_tool_leaks_into_the_global_manifest` exists to catch.
    // See `tools` for what it publishes instead.
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/dev/api/tools.json",
        Route::ApiToolsJson,
    )
    .summary("Page-scoped WebMCP tool manifest")
    .description(
        "The curated `dev_*` and `shop_*` tools the /b/dev page registers for its \
         in-page agent — a page-scoped WebMCP manifest, not the deployment-wide one \
         at /b/webmcp/manifest.json.",
    ),
    // `/export/manifest` BEFORE `/export`: `endpoint_match` walks this table
    // in order and `/b/dev/api/export` is a literal template, not a prefix,
    // so the order is not load-bearing today — it is written this way so the
    // more specific path stays first if either ever gains a `{…}` segment.
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/dev/api/export/manifest",
        Route::ApiExportManifest,
    )
    .summary("Preview the export bundle")
    .description(
        "What `GET /b/dev/api/export` would produce, without producing it: every \
         entry of the zip with its size, the totals, and the rows of each data \
         table the snapshot carries. Read it to see what an export would contain \
         before downloading one.",
    )
    .output(response_schema_of::<contracts::ExportManifest>),
    // No `.output(..)`: the body is a zip, not JSON. Declared all the same,
    // because the declaration is what pins its `Admin` tier where the router
    // can enforce it.
    EndpointRoute::admin(HttpMethod::Get, "/b/dev/api/export", Route::ApiExport)
        .summary("Export the site as a runnable static bundle")
        .description(
            "A zip of the runtime shell with the sandbox turned off, the site files, \
             every compiled block and its source, and a data snapshot — servable from \
             any static host and re-importable as a seed.",
        ),
];

/// A response builder pre-seeded with `Cache-Control: no-store`.
///
/// Design §12 requires every `/b/dev` response to be `no-store`. Seeding it at
/// construction — rather than post-filtering the returned `OutputStream` — is
/// what makes that unconditional: an `OutputStream`'s meta is fixed when it is
/// built, so a wrapper would have to buffer the whole (possibly streaming)
/// response to change it.
pub(crate) fn no_store() -> ResponseBuilder {
    ResponseBuilder::new().set_header("Cache-Control", "no-store")
}

/// The error-terminal counterpart of [`no_store`].
///
/// An error terminal is still a response on the wire: the HTTP boundary turns
/// `WaferError::meta` into headers before rendering the
/// `{"error", "message"}` body (`wafer_block::http_codec`'s
/// `collect_http_response`). So a refusal from this block can carry the header
/// too, which is what keeps "every `/b/dev` response is `no-store`" true for
/// the 404 as well as the 200 — a cached 404 for a route a later generation
/// adds would look permanently missing.
///
/// The one deliberate exception is [`crate::http::err_internal`]: it owns the
/// correlation-id logging and message sanitizing that must not be reproduced
/// here, and returns an already-sealed `OutputStream`. A 5xx is not cached by
/// any client without explicit headers, so the block keeps the shared
/// sanitizer rather than hand-rolling a header-carrying copy of it.
pub(crate) fn no_store_error(code: wafer_run::ErrorCode, message: &str) -> OutputStream {
    OutputStream::error(no_store_wafer_error(code, message))
}

/// [`no_store_error`] for a refusal whose HTTP status is not the one its
/// [`wafer_run::ErrorCode`] maps to.
///
/// The quota refusals in [`files`] are `413`s, and `ErrorCode` has no
/// payload-too-large member. `wafer_block::http_codec::resolve_error_status`
/// lets an explicit `resp.status` on the error's meta win over the
/// code-derived default, which is how a status outside the enum is expressed
/// without inventing a code the rest of the runtime would not understand.
pub(crate) fn no_store_error_status(
    code: wafer_run::ErrorCode,
    status: u16,
    message: &str,
) -> OutputStream {
    let mut error = no_store_wafer_error(code, message);
    error.meta.push(wafer_run::MetaEntry {
        key: wafer_block::meta::META_RESP_STATUS.to_string(),
        value: status.to_string(),
    });
    OutputStream::error(error)
}

/// [`crate::blocks::crud::db_error`] for a `/b/dev` response.
///
/// The classification is not re-derived here — `crud::classify_db_error` is
/// the one place that decides what a failed database call means, and a
/// second copy is exactly what `tests/error_door.rs` exists to stop. What
/// this adds is the sealing: every `/b/dev` response carries
/// `Cache-Control: no-store` (design §12), and an `OutputStream`'s meta is
/// fixed when it is built, so the header has to go on the error *before* it
/// is sealed. The `Internal` arm still goes through
/// [`crate::http::err_internal`], the one deliberate exception
/// [`no_store_error`] already documents.
pub(crate) fn no_store_db_error(error: WaferError, not_found: &str, context: &str) -> OutputStream {
    seal_no_store(
        crate::blocks::crud::classify_db_error(error, Some(not_found), context),
        context,
    )
}

/// [`no_store_db_error`] for a call whose `NotFound` is NOT the client's row
/// — the [`crate::blocks::crud::db_error_internal`] of this pair, and split
/// for the same reason: a `NotFound` from a listing the block addressed
/// itself names no generation of the caller's.
pub(crate) fn no_store_db_error_internal(error: WaferError, context: &str) -> OutputStream {
    seal_no_store(
        crate::blocks::crud::classify_db_error(error, None, context),
        context,
    )
}

/// Seal a [`crate::blocks::crud::DbFailure`] as a `/b/dev` response.
fn seal_no_store(failure: crate::blocks::crud::DbFailure, context: &str) -> OutputStream {
    match failure {
        crate::blocks::crud::DbFailure::Refused(refusal) => {
            OutputStream::error(with_no_store(refusal.into_error()))
        }
        crate::blocks::crud::DbFailure::Internal(fault) => {
            crate::http::err_internal(context, fault.into_error())
        }
    }
}

/// A [`WaferError`] carrying the block-wide `Cache-Control: no-store`.
fn no_store_wafer_error(code: wafer_run::ErrorCode, message: &str) -> WaferError {
    with_no_store(WaferError::new(code, message))
}

/// `error` with the block-wide `Cache-Control: no-store` header attached.
fn with_no_store(mut error: WaferError) -> WaferError {
    error.meta.push(wafer_run::MetaEntry {
        key: format!(
            "{}Cache-Control",
            wafer_block::meta::META_RESP_HEADER_PREFIX
        ),
        value: "no-store".to_string(),
    });
    error
}

/// The WRAP grants the sandbox needs beyond its own namespace.
///
/// The dev block's tables (`impresspress__dev__*`) self-admit under the
/// own-namespace rule, and its blobs and artifacts live under its own storage
/// prefix. Two kinds of resource it must be *granted* instead, because it
/// does not own them — so neither grant can be declared in
/// `BlockInfo::grants` (a block may only grant what it owns) and both are
/// handed to the runtime by whoever registers the block:
///
/// - the published site, which `wafer-run/web` owns;
/// - every [`data_snapshot::TABLE_ALLOWLIST`] table, one exact grant each
///   (never a `{org}__{block}__*` prefix) so the grant set says exactly what
///   [`data_snapshot::export`]/[`data_snapshot::import`] actually touch — the
///   same closed-list discipline the allowlist itself exists for. This is
///   the dev block's own control-plane logic reading and writing another
///   block's rows directly, not a delegated call through that block's own
///   authorized handler, so WRAP has no other way to see it as legitimate.
pub fn wrap_grants() -> Vec<wafer_run::ResourceGrant> {
    let mut grants = vec![
        wafer_run::ResourceGrant::read_write(BLOCK_NAME, "wafer-run/web/site/*")
            .typed(wafer_run::ResourceType::Storage),
    ];
    grants.extend(data_snapshot::TABLE_ALLOWLIST.iter().map(|(table, _mode)| {
        wafer_run::ResourceGrant::read_write(BLOCK_NAME, table).typed(wafer_run::ResourceType::Db)
    }));
    grants
}

/// State shared by every `/b/dev` handler.
///
/// Held behind an `Arc` because the activation queue is shared mutable state
/// that outlives any one request — and outlives the runtime an activation
/// rebuilds.
pub struct DevShared {
    /// The host's half of activation: builds and swaps the live runtime.
    pub control: Arc<dyn RuntimeControl>,
    /// The host's other half: the static shell this deployment is running
    /// inside of, which [`export`] copies into the bundle it hands the user.
    ///
    /// Beside `control` rather than inside it because the two answer
    /// different questions — one builds a runtime, the other reads files the
    /// host was shipped as — and because a host may legitimately have one
    /// without the other (an export is a read; a runtime rebuild is not).
    pub shell: Arc<dyn ShellSource>,
    /// The paths the service worker in front of this runtime leaves to the
    /// static host — what it handed `initialize({ bypass })`, kept for the
    /// worker's life. A site file at one of them would never be shown, so
    /// every site write and the seed import refuse it ([`bypass`]).
    ///
    /// Fixed at construction because the worker that handed them over is the
    /// one that routes every request this runtime ever sees: a redeploy
    /// brings a new worker, and a new worker boots a new runtime.
    pub bypass: BypassRules,
    /// The one serialized path from a desired state to a live one.
    ///
    /// On the shared state rather than on [`DevBlock`] because activation is
    /// what re-instantiates the block: a queue owned by the block would be
    /// dropped halfway through the operation that dropped it.
    pub activation: activation::ActivationQueue,
    /// Serializes the read-modify-write of `workspace.json`.
    ///
    /// Every workspace mutation is load-whole-manifest, change one entry,
    /// save-whole-manifest. Two of those interleaving between the load and the
    /// save would each save a manifest built from a snapshot that predates the
    /// other, and the later save would drop the earlier writer's entry — even
    /// though both passed their own per-path `expected_sha256` check, because
    /// that check is about the path each writer names and says nothing about
    /// the rest of the file.
    ///
    /// An **async** mutex, because the critical section spans storage calls;
    /// a `std::sync::Mutex` cannot be held across an `await` at all. It guards
    /// the manifest only: it is released before the activation the mutation
    /// requests, so ordinary editing never waits behind a publish.
    pub workspace: futures::lock::Mutex<()>,
    /// Serializes every change to the *block* set.
    ///
    /// A block change is resolved as a whole desired set —
    /// `ActivationIntent::BlockSet` carries every block the generation runs,
    /// not a delta — from the set that is active when the request is made.
    /// Two of those interleaving between their read and their
    /// `activation::request` would each compose a set from a snapshot that
    /// predates the other, and the later one would drop the earlier block
    /// without anything having failed.
    ///
    /// Design §6.6 allows one compile at a time, and this is what upholds it.
    /// A future that lifts the restriction has to carry the block *delta* in
    /// the intent instead; the lock is the cheaper half of that trade while
    /// the sandbox has one agent and one page.
    ///
    /// An **async** mutex for the same reason [`Self::workspace`] is: the
    /// critical section spans storage, the ledger and the runtime rebuild. It
    /// is a separate lock because it guards a different thing — a site write
    /// must never wait behind a compile, and `ActivationIntent::SiteOnly`
    /// composes its block half at dequeue precisely so it does not have to.
    pub compile: futures::lock::Mutex<()>,
    /// Serializes `activation::maintain` — retention's prune and the
    /// collector — against itself.
    ///
    /// Scheduled after a request's reply, maintenance runs outside the
    /// activation queue, so two of them can overlap: the tasks one request
    /// left behind run together, and the next request's can start before
    /// they finish. Two prunes would delete the same rows and two
    /// collections the same objects, and the slower one would fail on what
    /// the faster one had already removed. One at a time, each pass reads
    /// what the previous one left.
    ///
    /// Taken first and held for the whole pass; the collector takes
    /// [`Self::workspace`] inside it, and nothing takes this while holding
    /// that, or while waiting on the activation queue.
    pub maintenance: futures::lock::Mutex<()>,
}

impl DevShared {
    /// Build the shared state around the two host seams and the service
    /// worker's bypass rules.
    ///
    /// [`RuntimeControl`] is bounded on `MaybeSend + MaybeSync`, which is
    /// unbounded on wasm32 — that is what lets the browser control hold the
    /// live `Rc<Wafer>` and its factory. The `Arc` this returns is therefore
    /// not `Send`/`Sync` on that target, and on a single-threaded one it does
    /// not need to be; see the crate-level
    /// `expect(clippy::arc_with_non_send_sync)` in `lib.rs` for why the lint
    /// that says so is off for wasm32 only.
    pub fn new(
        control: Arc<dyn RuntimeControl>,
        shell: Arc<dyn ShellSource>,
        bypass: BypassRules,
    ) -> Arc<Self> {
        Arc::new(Self {
            control,
            shell,
            bypass,
            activation: activation::ActivationQueue::new(),
            workspace: futures::lock::Mutex::new(()),
            compile: futures::lock::Mutex::new(()),
            maintenance: futures::lock::Mutex::new(()),
        })
    }
}

/// The browser development sandbox control plane (`impresspress/dev`).
pub struct DevBlock {
    shared: Arc<DevShared>,
    /// Whether the runtime that registered this block also routed
    /// [`ROUTE_PREFIX`] — the workspace half of the sandbox.
    ///
    /// The block is registered in BOTH of `SandboxMode`'s compiled-in modes,
    /// because its `lifecycle(Init)` is what creates the ledger tables the
    /// seed import and the generation history write to. Only the workspace
    /// mode routes it. What this field carries is that difference, into the
    /// one place that would otherwise describe a surface that is not there:
    /// [`Block::info`] — whose `endpoints` become `/openapi.json` and whose
    /// `admin_url` becomes an "Open" button on `/b/admin/blocks`, for every
    /// registered block, routed or not.
    workspace: bool,
}

impl DevBlock {
    /// The sandbox with its workspace half: this runtime routes
    /// [`ROUTE_PREFIX`], so the endpoints below are real and `/b/dev` is
    /// where an admin goes to reach them.
    pub fn with_workspace(shared: Arc<DevShared>) -> Self {
        Self {
            shared,
            workspace: true,
        }
    }

    /// The runtime half alone — **an exported bundle**.
    ///
    /// Registered for its migrations, its ledger and its seed import, with no
    /// `/b/dev` route (design amendment 19). Declaring the endpoints anyway
    /// would publish a dozen 404s in the exported site's `/openapi.json` and
    /// put an "Open" link to a page that does not exist on its admin's own
    /// blocks page — the exact drift
    /// `routes_and_endpoints_stay_in_lockstep` exists to catch, one level up
    /// from where that test can see it.
    pub fn runtime_only(shared: Arc<DevShared>) -> Self {
        Self {
            shared,
            workspace: false,
        }
    }
}

#[wafer_block::wafer_async_trait]
impl Block for DevBlock {
    fn info(&self) -> BlockInfo {
        // The half that is true in both modes: the block exists, it owns
        // these tables, and its migrations ran. An exported bundle's ledger
        // is as real as a workspace's.
        let base = BlockInfo::new(
            BLOCK_NAME,
            "0.1.0",
            "http-handler@v1",
            "Browser development sandbox control plane",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec![
            "wafer-run/database".into(),
            "wafer-run/storage".into(),
            "wafer-run/config".into(),
        ])
        // Advisory table list — the schema itself lives solely in
        // `migrations/001_dev_schema.{sqlite,postgres}.sql`.
        .collections(vec![
            CollectionSchema::new(repo::generations::TABLE),
            CollectionSchema::new(repo::builds::TABLE),
            CollectionSchema::new(repo::runtime_state::TABLE),
        ])
        .category(wafer_run::BlockCategory::Feature)
        // The sandbox is registered only where it is meant to exist; a
        // deployment turns it off by not building it, not by an admin toggle
        // that would leave a half-live control plane behind.
        .can_disable(false);

        // The half that depends on a route existing. An exported bundle has
        // none of it — see `Self::runtime_only`.
        if !self.workspace {
            return base;
        }

        base.endpoints(endpoint_match::declare(ROUTES))
            .admin_url(ROUTE_PREFIX)
    }

    async fn handle(
        &self,
        ctx: &dyn Context,
        mut msg: Message,
        input: InputStream,
    ) -> OutputStream {
        // An exported bundle has no HTTP surface. The router already says so
        // — nothing routes to this block there — and this says it again from
        // the inside, because one row of `ROUTES` is `Public` and hands out a
        // session: that page must not be one stray route registration away
        // from existing on a site that was exported to be served for real.
        if !self.workspace {
            return no_store_error(wafer_run::ErrorCode::NotFound, "endpoint not found");
        }
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return no_store_error(wafer_run::ErrorCode::NotFound, "endpoint not found");
        };
        // Every route but `Enter` is `RouteAccess::Admin` at the router;
        // handlers do not re-check the caller's role.
        match route {
            Route::Page => page::handle(ctx, &msg).await,
            Route::Enter => enter::handle(ctx).await,
            Route::PageScript => page::handle_script(&msg),
            Route::PageStylesheet => page::handle_stylesheet(&msg),
            Route::PageCompilerAdapter => page::handle_compiler_adapter(&msg),
            Route::ApiStatus => status::handle(ctx, &self.shared).await,
            Route::ApiFilesList => files::handle_list(ctx, &self.shared, &msg).await,
            Route::ApiFilesRead => files::handle_read(ctx, &self.shared, input).await,
            Route::ApiFilesWrite => files::handle_write(ctx, &self.shared, input).await,
            Route::ApiFilesWriteBatch => files::handle_write_batch(ctx, &self.shared, input).await,
            Route::ApiFilesDelete => files::handle_delete(ctx, &self.shared, input).await,
            Route::ApiGenerations => generations_api::handle_list(ctx, &msg).await,
            Route::ApiGenerationDetail => generations_api::handle_detail(ctx, &msg).await,
            Route::ApiGenerationRollback => {
                generations_api::handle_rollback(ctx, &self.shared, &msg).await
            }
            Route::ApiBuildStage => blocks_api::handle_stage(ctx, &self.shared, input).await,
            Route::ApiBlockCreate => scaffold::handle_create(ctx, &self.shared, input).await,
            Route::ApiBlockRemove => blocks_api::handle_remove(ctx, &self.shared, &msg).await,
            Route::ApiReference => scaffold::handle_reference(ctx).await,
            Route::ApiGuest => scaffold::handle_guest(ctx).await,
            Route::ApiToolsJson => tools::handle(ctx).await,
            Route::ApiExportManifest => export::handle_manifest(ctx, &self.shared).await,
            Route::ApiExport => export::handle_export(ctx, &self.shared).await,
        }
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        crate::migration_helper::lifecycle_init(
            ctx,
            &event,
            BLOCK_NAME,
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        control::{RouteAccessKind, ValidationStage},
        repo::{
            builds::BuildStatus,
            generations::{GenerationCause, GenerationStatus},
            runtime_state::ActivationPhase,
        },
        *,
    };

    // Every closed enum below must round-trip through `as_str` / `parse`,
    // **and** `as_str` must be the same spelling serde uses.
    //
    // The two halves are the point: `as_str` is what the SQL `CHECK`
    // constraints and the stored columns see, serde is what the HTTP
    // contracts and the generation manifest see. A value with two spellings
    // is exactly the implicit mapping layer this repo forbids, and it would
    // stay invisible until a stored row failed to load or a manifest failed
    // to match.

    #[test]
    fn activation_phase_spellings_agree() {
        for phase in [
            ActivationPhase::Idle,
            ActivationPhase::Validating,
            ActivationPhase::BuildingRuntime,
            ActivationPhase::Publishing,
            ActivationPhase::Active,
            ActivationPhase::Failed,
        ] {
            assert_eq!(ActivationPhase::parse(phase.as_str()), Some(phase));
            assert_eq!(
                serde_json::to_value(phase).expect("serialize"),
                serde_json::json!(phase.as_str()),
            );
        }
        assert_eq!(ActivationPhase::parse("Idle"), None);
        assert_eq!(ActivationPhase::parse(""), None);
    }

    #[test]
    fn generation_status_spellings_agree() {
        for status in [
            GenerationStatus::Staged,
            GenerationStatus::Validating,
            GenerationStatus::Activating,
            GenerationStatus::Active,
            GenerationStatus::Failed,
            GenerationStatus::Superseded,
        ] {
            assert_eq!(GenerationStatus::parse(status.as_str()), Some(status));
            assert_eq!(
                serde_json::to_value(status).expect("serialize"),
                serde_json::json!(status.as_str()),
            );
        }
        assert_eq!(GenerationStatus::parse("archived"), None);
    }

    #[test]
    fn generation_cause_spellings_agree_and_classify_rebuilds() {
        for cause in [
            GenerationCause::SiteWrite,
            GenerationCause::SiteDelete,
            GenerationCause::BlockCompile,
            GenerationCause::BlockRemove,
            GenerationCause::Rollback,
            GenerationCause::Seed,
        ] {
            assert_eq!(GenerationCause::parse(cause.as_str()), Some(cause));
            assert_eq!(
                serde_json::to_value(cause).expect("serialize"),
                serde_json::json!(cause.as_str()),
            );
        }
        // Site edits republish without touching the runtime (design §7.2);
        // everything else changes the block set.
        assert!(!GenerationCause::SiteWrite.rebuilds_runtime());
        assert!(!GenerationCause::SiteDelete.rebuilds_runtime());
        assert!(GenerationCause::BlockCompile.rebuilds_runtime());
        assert!(GenerationCause::BlockRemove.rebuilds_runtime());
        assert!(GenerationCause::Rollback.rebuilds_runtime());
        assert!(GenerationCause::Seed.rebuilds_runtime());
    }

    #[test]
    fn build_status_and_validation_stage_spellings_agree() {
        for status in [
            BuildStatus::Staged,
            BuildStatus::Valid,
            BuildStatus::Invalid,
        ] {
            assert_eq!(BuildStatus::parse(status.as_str()), Some(status));
            assert_eq!(
                serde_json::to_value(status).expect("serialize"),
                serde_json::json!(status.as_str()),
            );
        }
        for stage in [
            ValidationStage::Load,
            ValidationStage::Info,
            ValidationStage::Init,
            ValidationStage::Start,
            ValidationStage::Probe,
        ] {
            assert_eq!(ValidationStage::parse(stage.as_str()), Some(stage));
            assert_eq!(
                serde_json::to_value(stage).expect("serialize"),
                serde_json::json!(stage.as_str()),
            );
        }
    }

    #[test]
    fn route_access_kind_matches_the_manifest_spelling_and_the_router_tier() {
        // Design §11.3 writes `"access": "Public"` — PascalCase, unlike the
        // snake_case status/cause columns. Pin it: a rename would silently
        // invalidate every stored manifest.
        for (kind, spelling, tier) in [
            (
                RouteAccessKind::Public,
                "Public",
                crate::routing::RouteAccess::Public,
            ),
            (
                RouteAccessKind::Authenticated,
                "Authenticated",
                crate::routing::RouteAccess::Authenticated,
            ),
            (
                RouteAccessKind::Admin,
                "Admin",
                crate::routing::RouteAccess::Admin,
            ),
        ] {
            assert_eq!(kind.as_str(), spelling);
            assert_eq!(RouteAccessKind::parse(spelling), Some(kind));
            assert_eq!(
                serde_json::to_value(kind).expect("serialize"),
                serde_json::json!(spelling),
            );
            assert_eq!(kind.to_route_access(), tier);
        }
    }

    #[test]
    fn wrap_grants_cover_the_published_site_and_the_data_snapshot_allowlist() {
        let grants = wrap_grants();
        let site_grant = &grants[0];
        assert_eq!(site_grant.grantee, BLOCK_NAME);
        assert_eq!(site_grant.resource, "wafer-run/web/site/*");
        assert_eq!(site_grant.write, wafer_block::GrantWrite::Full);
        // Typed to Storage: an untyped grant would also admit a database
        // collection or config key that happened to match the pattern.
        assert_eq!(
            site_grant.resource_type,
            Some(wafer_run::ResourceType::Storage)
        );

        // Every other grant is one exact, `Db`-typed, read-write entry per
        // `TABLE_ALLOWLIST` table — never a `{org}__{block}__*` prefix, which
        // would also admit a table `TABLE_EXCLUDED` deliberately keeps this
        // block off, and never untyped, which would also admit a Config key
        // or Vector collection that happened to share the name.
        let db_grants = &grants[1..];
        assert_eq!(db_grants.len(), data_snapshot::TABLE_ALLOWLIST.len());
        for grant in db_grants {
            assert_eq!(grant.grantee, BLOCK_NAME);
            assert_eq!(grant.write, wafer_block::GrantWrite::Full);
            assert_eq!(grant.resource_type, Some(wafer_run::ResourceType::Db));
            assert!(
                !grant.resource.ends_with('*'),
                "{:?} is a prefix grant, not an exact table name",
                grant.resource
            );
            assert!(
                data_snapshot::TABLE_ALLOWLIST
                    .iter()
                    .any(|(table, _)| *table == grant.resource),
                "{:?} is not on TABLE_ALLOWLIST",
                grant.resource
            );
        }
    }

    /// `info().endpoints` is generated from `ROUTES`; nothing else declares
    /// an endpoint for this block. Every row and every declaration but the
    /// entry page's is `Admin`, so the summary is compared too: it is what
    /// proves the rows carry the declaration rather than merely matching it.
    #[test]
    fn info_endpoints_come_from_the_table() {
        use wafer_run::Block as _;

        let declared = DevBlock::with_workspace(DevShared::new(
            test_support::FakeControl::new(),
            std::sync::Arc::new(test_support::FakeShell::new()),
            test_support::fake_bypass_rules(),
        ))
        .info()
        .endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
            assert_eq!(ep.summary, row.summary, "{}", row.template);
        }
    }

    /// Exactly one row is reachable without a session, and it is the entry
    /// page. A second `Public` row would be an unauthenticated door into the
    /// control plane, so the count is pinned rather than left to review.
    #[test]
    fn the_entry_page_is_the_only_public_row() {
        let public: Vec<&str> = ROUTES
            .iter()
            .filter(|row| row.auth != wafer_run::AuthLevel::Admin)
            .map(|row| row.template)
            .collect();
        assert_eq!(public, [ENTER_PATH]);
        // And the router is handed its path before the `Admin` prefix that
        // would otherwise claim it (first match wins).
        assert_eq!(WORKSPACE_ROUTES[0].0, ENTER_PATH);
        assert_eq!(WORKSPACE_ROUTES[1].0, ROUTE_PREFIX);
        assert!(ENTER_PATH.starts_with(ROUTE_PREFIX));
    }

    #[tokio::test]
    async fn every_response_the_block_builds_is_no_store() {
        let buf = no_store()
            .json(&serde_json::json!({}))
            .collect_buffered()
            .await
            .expect("respond");
        assert_eq!(
            wafer_run::MetaGet::get(&buf.meta, "resp.header.Cache-Control"),
            Some("no-store"),
        );
    }
}
