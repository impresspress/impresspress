//! Seed-on-boot — generation 0, imported from a bundle the origin serves.
//!
//! A sandbox instance is empty the first time it boots: no workspace, no
//! generations, nothing published. Design §10.2 fills that gap from a **seed
//! bundle** served as static files next to the app shell, so
//! `dev.impresspress.org` can ship a welcome site and an exported bundle can
//! ship the user's own shop through one mechanism.
//!
//! The bundle's layout is design §10.1's, and it is stated once — [`ROOT`],
//! [`MANIFEST_URL`], [`site_url`], [`artifact_url`], [`source_url`]. The
//! exporter (Plan 4) writes those paths and this reads them; a layout spelled
//! at two sites is a layout that can be spelled two different ways.
//!
//! # Why a fetch seam rather than a URL
//!
//! `impresspress-core` has no HTTP client, and the host that does — the
//! service worker's own `fetch` — cannot be reached from here at all. So the
//! importer takes a [`SeedFetch`]: the browser passes a wrapper around
//! `self.fetch`, and the host tests pass an in-memory map. That is also what
//! makes every rule below testable without a browser.
//!
//! # What is verified
//!
//! Every file's declared `sha256`, `size` and `content_type` are checked
//! against the bytes that actually arrived, and every workspace path the
//! bundle would create is run through [`paths::validate_path`]. A seed is
//! same-origin content, but it is still content this instance did not
//! produce: a manifest naming `site/../../elsewhere`, or claiming a hash it
//! does not have, is refused with a message that names the path.
//!
//! Every seeded block is checked TWICE, by the same two entry points the
//! staging path uses, and in the same order:
//!
//! 1. [`validation::validate_spec`] over the manifest's declared spec, before
//!    a single byte is fetched — its name, its route prefix against the
//!    built-ins and against the rest of the bundle, and every §6.5 capability
//!    rule. A seed is loaded with exactly the capabilities its manifest
//!    declares, so without this a bundle asking for `raw_sql` or a collection
//!    outside its own namespace would be granted it verbatim, and §10.1 makes
//!    exports deliberately re-importable by someone who did not write them.
//! 2. [`super::control::RuntimeControl::inspect`] over the fetched artifact,
//!    then [`validation::validate_static`] over the `BlockInfo` the guest
//!    itself reports — the four rules that need that report and that step 1
//!    structurally cannot apply: that the guest calls itself `site/<name>`,
//!    that its endpoints fall inside its own route prefix, that its agent
//!    tool names collide with nothing else in the bundle or in the runtime,
//!    and that `callable_blocks` matches `requires`. The accepted spec that
//!    produces must then EQUAL the one the manifest declared: a bundle whose
//!    manifest grants a block authority the guest does not ask for is a
//!    bundle whose manifest is not a description of its own artifact.
//!
//! `inspect` runs the module under `BlockCapabilities::none()` and reads
//! `__wafer_info` — no lifecycle event, no request — which is why running it
//! on content the static host served is safe. That report is also what is
//! recorded on the seeded build row, so the duplicate-tool-name rule can be
//! applied to whatever is staged NEXT: without it a seeded block left the
//! staging path with no readable `BlockInfo` for an active block, which
//! `blocks_api::claimed_tool_names` refuses outright — a seeded instance
//! could never compile a block of its own.
//!
//! # What is still *not* verified
//!
//! [`super::control::RuntimeControl::probe`] is not run: no `Init`, no
//! `Start`, no request. Those execute guest code under the accepted
//! capabilities, and a seed import happens during boot, before anything is
//! serving — the activation the caller requests next rebuilds the runtime
//! with the block in it, which is where the guest first runs. A seeded guest
//! that traps in `Init` therefore fails the activation rather than the
//! import, and the sandbox falls back exactly as it does for any failed
//! activation.

use serde::{Deserialize, Serialize};
use wafer_run::context::Context;

use super::{
    artifacts, blobs,
    contracts::SiteManifest,
    control::{DynamicBlockSpec, RuntimeControl},
    data_snapshot,
    generation::GenerationManifest,
    paths,
    repo::{self, runtime_state},
    validation,
    workspace::{self, Workspace},
};
// audit-allow: the dev block grants itself this table — `dev::wrap_grants()` maps every `TABLE_ALLOWLIST` entry to `read_write(BLOCK_NAME, table)`, which the runtime honours from its flat grant list; the audit attributes grants to the declaring file's block and so cannot see it
use crate::platform_state::variables::{self, VariablePatch};

/// Seed-manifest schema version this build reads.
///
/// Separate from [`super::generation::SCHEMA_VERSION`] on purpose: a seed
/// bundle is an interchange format between two *builds* of the sandbox, while
/// a generation manifest is this instance's own stored shape. They are `1`
/// together today and are free to move apart.
pub const SCHEMA_VERSION: u32 = 1;

/// URL prefix the seed bundle is served under.
///
/// Also the service worker's bypass prefix (`impresspress-bundle`'s
/// `build_template_vars` adds it when `[dev] enabled`), because these files
/// are served by the static host and must not be answered from the published
/// site the runtime owns.
pub const ROOT: &str = "/seed/";

/// The one file whose presence decides whether there is a seed at all.
pub const MANIFEST_URL: &str = "/seed/manifest.json";

/// One file in a seed bundle.
///
/// The workspace's own [`workspace::FileEntry`], not a parallel shape: a seed
/// entry *is* a workspace entry (path, blob hash, size, served type), and the
/// importer's whole job is to turn one into the other. A second definition
/// would be a second thing to keep in step with the manifest a generation
/// stores.
pub use super::workspace::FileEntry as SeedFile;

/// One block a seed bundle carries: what to register, and the source that
/// produced it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SeedBlock {
    /// The spec the runtime registers the guest under — the accepted spec the
    /// exporting instance's ledger held, carried verbatim.
    pub spec: DynamicBlockSpec,
    /// The block's crate sources, rooted at the crate (`src/lib.rs`), so the
    /// import is editable and re-compilable rather than a binary drop.
    pub sources: Vec<SeedFile>,
}

/// What a seed bundle describes.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SeedManifest {
    /// Bundle schema version; must be [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The generation id this bundle was exported from, for provenance. Not
    /// reused as the imported generation's id — the importing instance mints
    /// its own, and two instances seeded from one bundle must not claim the
    /// same generation.
    pub source_generation: Option<String>,
    /// Site files, paths relative to the site root (`index.html`), exactly as
    /// a generation's site manifest holds them.
    pub site: Vec<SeedFile>,
    /// Blocks the seeded generation runs.
    pub blocks: Vec<SeedBlock>,
    /// The data snapshot file (design §10.1, amendment 9: `seed/data.json`),
    /// or `None` when the bundle carries no rows.
    ///
    /// A [`SeedFile`] like every other referenced file — `path` relative to
    /// [`ROOT`] (`"data.json"`, giving the URL [`data_url`] builds) — not a
    /// bare path string, so [`import`] verifies its hash, size and content
    /// type exactly as it does for every `site`/block-source entry (design
    /// §10.2) instead of trusting this one file unchecked.
    pub data: Option<SeedFile>,
}

/// A future produced by [`SeedFetch::get`].
///
/// `LocalBoxFuture`, not `BoxFuture`: the browser's `fetch` resolves through a
/// `JsFuture`, which is not `Send`, and the sandbox is single-threaded on
/// every target it runs on. A `Send` bound here would be a bound the only real
/// implementation cannot satisfy.
pub type FetchFuture<'a> = futures::future::LocalBoxFuture<'a, Result<Vec<u8>, String>>;

/// How the importer reads one file of the seed bundle.
///
/// A trait rather than the `&dyn Fn(&str) -> …` this started as: a closure
/// whose returned future borrows its argument needs a higher-ranked bound
/// (`for<'a> Fn(&'a str) -> FetchFuture<'a>`) that closure inference does not
/// produce, so every call site would have to spell the bound out. The trait
/// says the same thing and is implementable in three lines — which is what
/// both the service worker's `fetch` wrapper and the host tests' in-memory map
/// are.
pub trait SeedFetch {
    /// Fetch `url` (an absolute path under [`ROOT`]).
    ///
    /// A file the bundle's manifest names but does not carry is an `Err`: the
    /// manifest is the contract, and a partially-shipped bundle must not
    /// import as a partial site. Only the *manifest itself* is allowed to be
    /// absent, and that is the caller's probe, not this call.
    fn get<'a>(&'a self, url: &'a str) -> FetchFuture<'a>;
}

/// URL of one site file.
pub fn site_url(path: &str) -> String {
    format!("{ROOT}site/{path}")
}

/// URL of one block's compiled artifact.
///
/// `name` is the short workspace name (`hello`), not the registered
/// `site/hello` — see [`short_name`].
pub fn artifact_url(name: &str) -> String {
    format!("{ROOT}blocks/{name}.wasm")
}

/// URL of one file of a block's crate sources.
pub fn source_url(name: &str, path: &str) -> String {
    format!("{ROOT}blocks/{name}/{path}")
}

/// URL of the data snapshot file.
pub fn data_url(path: &str) -> String {
    format!("{ROOT}{path}")
}

/// The content type `seed/data.json` is served as, and so the one the
/// manifest must declare for it.
///
/// Here rather than beside the exporter's [`SeedManifest.data`] writer for
/// the reason [`ROOT`] and the URL builders above are here: this module owns
/// the bundle's layout, [`super::export`] writes what it reads, and a string
/// spelled at both ends is a string that can be spelled two ways.
pub const DATA_CONTENT_TYPE: &str = "application/json";

/// Largest data snapshot a bundle may carry, in bytes.
///
/// One constant for both ends of the format: the importer refuses a larger
/// `data.json` before fetching it, and `super::export` refuses to write one.
/// A bound only the importer knew about would let an export succeed and hand
/// over a bundle whose own importer then rejects it — on a cold boot, where
/// the refusal leaves the imported instance's site empty.
///
/// Sized for a shop, not for an editable file: the snapshot carries every row
/// of the exported tables — each account with its credentials and role, each
/// product with its offers — so a limit on the scale of
/// [`paths::MAX_FILE_BYTES`] would refuse an ordinary shop of a few hundred
/// accounts.
///
/// What bounded it from above was write cost, not memory: in the browser
/// every database call that returns has saved the WHOLE sql.js database to
/// OPFS (`dbFlush` in `impresspress-browser`'s `bridge.js`), so a row-per-call
/// import saved it once per row. [`data_snapshot::import`] writes the whole
/// snapshot in one `db::batch`, so an import costs one whole-database save.
/// The limit stays at 2 MiB until a batched import
/// of a larger snapshot has been timed on a real browser cold boot; that
/// measurement, not the write count, is what a higher value has to rest on.
pub const MAX_DATA_BYTES: usize = 2 * 1024 * 1024;

/// The short workspace name of a registered block (`site/hello` → `hello`).
///
/// The workspace directory, the artifact URL and the route prefix are all
/// built from the short name, while the manifest carries the registered one.
/// A spec whose name is not `site/…` keeps its name verbatim, and then fails
/// [`paths::block_name_is_valid`] below — refusing it there, with the name in
/// the message, beats silently inventing a directory for it.
pub fn short_name(registered: &str) -> &str {
    registered.strip_prefix("site/").unwrap_or(registered)
}

/// Whether this instance has never published anything.
///
/// Both halves are required. `active_generation_id` alone would call an
/// instance whose only generation *failed* fresh, and re-seeding over a
/// workspace someone has already edited is the one outcome a seed import must
/// never produce; an empty ledger alone would miss nothing today but says
/// what is actually meant — "nothing has ever been staged here".
pub async fn is_fresh(ctx: &dyn Context) -> Result<bool, String> {
    let state = runtime_state::read(ctx).await.map_err(|e| e.message)?;
    if state.active_generation_id.is_some() {
        return Ok(false);
    }
    let recent = repo::generations::list_recent(ctx, 1)
        .await
        .map_err(|e| e.message)?;
    Ok(recent.is_empty())
}

/// Import `manifest` into a fresh instance and return the generation 0 the
/// caller should activate.
///
/// `Ok(None)` when the instance is not fresh — a second boot, or a bundle that
/// was already imported. That is the ordinary case on every boot after the
/// first, not an error.
///
/// `control` is the runtime seam ([`RuntimeControl::inspect`]) — the boot
/// caller has one (`impresspress-web`'s `install` builds the control before
/// the first runtime), and the module docs above say why an importer that
/// did not use it left four validation rules unapplied and every later
/// staging attempt refused.
///
/// The order is: check every declared spec, fetch and inspect every artifact,
/// check every guest report, then write the blobs and artifacts, then save
/// the workspace. A workspace saved before its blobs would name content that
/// is not stored, and every later read of those paths would be a 500; in this
/// order a failure part-way leaves stored bytes that no manifest names, which
/// costs storage and nothing else — and a bundle refused for what its guests
/// report has stored nothing at all.
///
/// The returned manifest is *staged* — it has no id and no parent. Minting
/// those is [`super::activation::request`]'s job, exactly as for every other
/// generation.
pub async fn import(
    ctx: &dyn Context,
    control: &dyn RuntimeControl,
    manifest: &SeedManifest,
    fetch: &dyn SeedFetch,
) -> Result<Option<GenerationManifest>, String> {
    let outcome = import_bundle(ctx, control, manifest, fetch).await;
    match &outcome {
        // An attempt that ran and failed, and an attempt that ran and worked:
        // both are facts about THIS instance, and the second has to clear the
        // first or a fixed bundle would boot into a stale complaint. A
        // `Ok(None)` made no attempt at all (this instance is not fresh) and
        // writes nothing.
        Err(message) => record_failure(ctx, message).await,
        Ok(Some(_)) => clear_failure(ctx).await,
        Ok(None) => {}
    }
    outcome
}

/// [`import`] proper — everything the refusal recorder above wraps.
async fn import_bundle(
    ctx: &dyn Context,
    control: &dyn RuntimeControl,
    manifest: &SeedManifest,
    fetch: &dyn SeedFetch,
) -> Result<Option<GenerationManifest>, String> {
    if !is_fresh(ctx).await? {
        return Ok(None);
    }
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "the seed bundle declares schema_version {}; this build reads {SCHEMA_VERSION}",
            manifest.schema_version
        ));
    }
    if manifest.blocks.len() > paths::MAX_BLOCKS {
        return Err(format!(
            "the seed bundle carries {} blocks; the limit is {}",
            manifest.blocks.len(),
            paths::MAX_BLOCKS
        ));
    }

    // Every spec, before a single byte is fetched. Two reasons for the
    // ordering: a bundle whose second block is refused must not have left the
    // first one's artifact in the store, and the rules that read the rest of
    // the set need the whole set in hand. `validate_spec` is also what refuses
    // an unregisterable name, which everything below derives a URL and a
    // workspace directory from.
    let specs: Vec<DynamicBlockSpec> = manifest.blocks.iter().map(|b| b.spec.clone()).collect();
    let builtin_routes = validation::builtin_route_prefixes();
    for (index, spec) in specs.iter().enumerate() {
        let others: Vec<DynamicBlockSpec> = specs
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, spec)| spec.clone())
            .collect();
        if let Err(found) = validation::validate_spec(spec, &builtin_routes, &others) {
            return Err(refusal(&spec.name, &found));
        }
        // The same gate `blocks_api::handle_stage` applies to a compile, for
        // the same reason and at the same point: before the module is fetched,
        // stored or executed. A block built against a different
        // `wafer_guest.rs` speaks a contract this runtime no longer
        // guarantees, and loading it turns a one-line "replace the module and
        // recompile" into a trap inside wasmi — on the one boot that can least
        // explain it. `validate_static` cannot produce this verdict
        // (`BlockInfo` carries no such field, spec amendment 8), so the
        // manifest is its only source; that is a reason to compare the number
        // the manifest carries, not a reason to take it on trust.
        //
        // Zero is "no version reported", exactly as it is on the staging path:
        // a compile whose request carried no `wafer_guest_version` — a
        // compiler that could not read the file — is recorded as `0` and
        // nothing is checked. The generation manifest keeps that `0` and an
        // export carries it forward, so a bundle whose block was built that
        // way must still import. (Not a compatibility allowance for older
        // bundles: `DynamicBlockSpec.wafer_guest_version` has no
        // `serde(default)`, so a manifest predating the field fails to
        // deserialize long before this line.)
        if spec.wafer_guest_version != 0 && spec.wafer_guest_version != super::WAFER_GUEST_VERSION {
            return Err(refusal(
                &spec.name,
                &[validation::Diagnostic::stale_guest_module(
                    spec.wafer_guest_version,
                    super::WAFER_GUEST_VERSION,
                )],
            ));
        }
    }

    // Every artifact, fetched and INSPECTED before anything is stored.
    //
    // Held in memory as a set rather than processed one block at a time,
    // because the duplicate-agent-tool rule below is a statement about the
    // whole bundle: block two's tool names have to be checked against block
    // one's report, and block one's against block two's. The worst case is
    // `MAX_BLOCKS` × `MAX_ARTIFACT_BYTES`, and a real bundle carries one or
    // two blocks of a few hundred KiB — the alternative, fetching each
    // artifact twice, would pay the network cost of the whole bundle again on
    // the one boot that can least afford it.
    let mut inspected: Vec<(&SeedBlock, Vec<u8>, wafer_block::BlockInfo)> =
        Vec::with_capacity(manifest.blocks.len());
    for block in &manifest.blocks {
        let name = short_name(&block.spec.name);
        // The artifact is content-addressed by the spec itself, so the spec's
        // own `artifact_sha256` is the check — there is no second declaration
        // of it to disagree with.
        let url = artifact_url(name);
        let bytes = fetch.get(&url).await?;
        // The same bound the staging path enforces, for the same reason: the
        // limit exists to bound what the artifact store holds, and a bundle
        // that arrived over the network is no more entitled to exceed it than
        // one that arrived over `POST /b/dev/api/builds/stage`. Checked beside
        // the hash and before the `put`, so an oversized module is never
        // stored. The wording is `Diagnostic::artifact_too_large`'s, which is
        // where the limit and its remedy are stated.
        if bytes.len() > validation::MAX_ARTIFACT_BYTES {
            return Err(format!(
                "{url}: {}",
                validation::Diagnostic::artifact_too_large(bytes.len()).message
            ));
        }
        let actual = blobs::sha256_hex(&bytes);
        if actual != block.spec.artifact_sha256 {
            return Err(format!(
                "{url}: the artifact hashes to {actual}, but block {:?} declares {}",
                block.spec.name, block.spec.artifact_sha256
            ));
        }
        // Under `BlockCapabilities::none()`, with no lifecycle event — the
        // guest's own capability declaration is INSIDE the value this
        // returns, so the deny-all set is what makes reading it safe. See
        // `control::RuntimeControl::inspect`.
        let info = control
            .inspect(&bytes)
            .await
            .map_err(|failure| format!("{url}: {failure}"))?;
        inspected.push((block, bytes, info));
    }

    // The four rules that need the guest's own report. `claimed` is every
    // agent tool name already taken: by a built-in block of this runtime, and
    // by every OTHER block the same bundle carries — the seeded set is
    // admitted as one generation, so two of its blocks claiming one name is
    // the same collision as a staged block colliding with an active one.
    for (index, (block, _bytes, info)) in inspected.iter().enumerate() {
        let name = short_name(&block.spec.name);
        let others: Vec<DynamicBlockSpec> = specs
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, spec)| spec.clone())
            .collect();
        // Built-ins only. `registered_blocks` is the runtime's whole sealed
        // snapshot, dynamic blocks included, and a seed import that ran on an
        // instance already serving one would otherwise count that block's
        // tools as claimed by a built-in — see
        // `validation::builtin_agent_tool_names`.
        let mut claimed = validation::builtin_agent_tool_names(ctx.registered_blocks());
        for (other, (_, _, other_info)) in inspected.iter().enumerate() {
            if other != index {
                claimed.extend(validation::agent_tool_names(std::slice::from_ref(
                    other_info,
                )));
            }
        }
        let accepted = validation::validate_static(
            name,
            info,
            &block.spec.artifact_sha256,
            &builtin_routes,
            &others,
            &claimed,
        )
        .map_err(|found| refusal(&block.spec.name, &found))?;
        // The accepted spec is what the RULES produced from the guest's own
        // report; the manifest's is what the bundle asked for. They have to
        // be the same value, or the manifest is granting authority its
        // artifact never asked for — the one thing a re-importable export
        // format makes trivially forgeable. `wafer_guest_version` is the sole
        // exception: `BlockInfo` carries no such field (spec amendment 8), so
        // `validate_static` leaves it `0` and the manifest is its only source.
        let accepted = DynamicBlockSpec {
            wafer_guest_version: block.spec.wafer_guest_version,
            ..accepted
        };
        if accepted != block.spec {
            return Err(format!(
                "the seed bundle's block {:?} declares a spec the guest does not report: the \
                 manifest says {}, the module says {}",
                block.spec.name,
                describe(&block.spec),
                describe(&accepted),
            ));
        }
    }

    let mut ws = Workspace::default();
    for entry in &manifest.site {
        let workspace_path = format!("{}{}", workspace::SITE_PREFIX, entry.path);
        let bytes = fetch_verified(fetch, &site_url(&entry.path), entry, &workspace_path).await?;
        store(ctx, &mut ws, &workspace_path, &bytes).await?;
    }

    for (block, bytes, info) in &inspected {
        let name = short_name(&block.spec.name);
        artifacts::put(ctx, bytes).await.map_err(|e| e.message)?;
        record_seeded_build(ctx, &block.spec, bytes.len() as u64, info).await?;

        for entry in &block.sources {
            let workspace_path = format!("{}{name}/{}", workspace::BLOCKS_PREFIX, entry.path);
            let bytes = fetch_verified(
                fetch,
                &source_url(name, &entry.path),
                entry,
                &workspace_path,
            )
            .await?;
            store(ctx, &mut ws, &workspace_path, &bytes).await?;
        }
    }

    workspace::save(ctx, &ws).await.map_err(|e| e.message)?;

    // The data snapshot, if the bundle carries one — after every file and
    // block artifact is stored (so a snapshot referencing this generation's
    // own content lands on a workspace that already has it), before the
    // staged manifest is handed back for activation. Verified the same way
    // as every other referenced file (design §10.2) — hash, size and
    // content type — via `fetch_and_verify`, not trusted unchecked the way
    // a bare `Option<String>` path could only ever be.
    if let Some(declared) = &manifest.data {
        let url = data_url(&declared.path);
        // The raw bytes live only as long as the parse: the rows are applied
        // from the parsed snapshot, so holding up to `MAX_DATA_BYTES` of JSON
        // beside them through every database write would be a second copy of
        // the snapshot for nothing.
        let snapshot: data_snapshot::DataSnapshot = {
            let bytes =
                fetch_and_verify(fetch, &url, declared, DATA_CONTENT_TYPE, MAX_DATA_BYTES).await?;
            serde_json::from_slice(&bytes)
                .map_err(|e| format!("{url}: not a valid data snapshot: {e}"))?
        };
        data_snapshot::import(ctx, &snapshot)
            .await
            .map_err(|e| format!("{url}: {}", e.message))?;
    }

    Ok(Some(GenerationManifest::staged(
        SiteManifest {
            files: workspace::site_manifest(&ws),
        },
        manifest.blocks.iter().map(|b| b.spec.clone()).collect(),
    )))
}

/// Fetch one declared file and check it is what the manifest said it was —
/// against `served_as`, the content type this instance considers correct for
/// it.
///
/// All three declared properties are load-bearing, so all three are checked:
/// `sha256` is what design §10.2 requires ("verify every referenced file's
/// hash"); `size` costs nothing over bytes already in hand and catches a
/// manifest built from a different tree; `content_type` is what the file will
/// actually be served as, so a bundle claiming a different one was produced
/// by an exporter that does not agree with this build about how files are
/// served.
///
/// `max_bytes` is the limit for this kind of file — [`paths::MAX_FILE_BYTES`]
/// for a workspace file, [`MAX_DATA_BYTES`] for the data snapshot — and the
/// declared size is checked against it before anything is fetched.
async fn fetch_and_verify(
    fetch: &dyn SeedFetch,
    url: &str,
    declared: &SeedFile,
    served_as: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    if declared.size > max_bytes as u64 {
        return Err(format!(
            "{url}: declares {} bytes; the limit for this file is {max_bytes}",
            declared.size,
        ));
    }

    let bytes = fetch.get(url).await?;
    let actual = blobs::sha256_hex(&bytes);
    if actual != declared.sha256 {
        return Err(format!(
            "{url}: content hashes to {actual}, but the manifest declares {}",
            declared.sha256
        ));
    }
    if bytes.len() as u64 != declared.size {
        return Err(format!(
            "{url}: content is {} bytes, but the manifest declares {}",
            bytes.len(),
            declared.size
        ));
    }
    if declared.content_type != served_as {
        return Err(format!(
            "{url}: the manifest declares content type {:?}, but this build serves it as {served_as:?}",
            declared.content_type
        ));
    }
    Ok(bytes)
}

/// [`fetch_and_verify`] for a file that lands in the workspace (`site/…`,
/// `blocks/<name>/…`): `workspace_path` both gates the fetch (a manifest
/// entry naming `../../elsewhere` must not cause a request for it either)
/// and derives the content type every such file is checked against.
async fn fetch_verified(
    fetch: &dyn SeedFetch,
    url: &str,
    declared: &SeedFile,
    workspace_path: &str,
) -> Result<Vec<u8>, String> {
    paths::validate_path(workspace_path)
        .map_err(|e| format!("the seed bundle names {workspace_path:?}: {e}"))?;
    let served = paths::content_type_for(workspace_path);
    fetch_and_verify(fetch, url, declared, served, paths::MAX_FILE_BYTES).await
}

/// One block's refusal, with every diagnostic's message and code.
///
/// Shared by both validation passes so a boot refusal reads the same whether
/// it came from the declared spec or from the guest's own report.
fn refusal(name: &str, found: &[validation::Diagnostic]) -> String {
    format!(
        "the seed bundle's block {name:?} cannot be registered: {}",
        found
            .iter()
            // A diagnostic with no code is a compiler's, and the seed
            // bundle's blocks are checked by the VALIDATOR, whose
            // diagnostics all carry one — so the bare-message arm should
            // never run here. It is written out rather than unwrapped
            // because a boot refusal that panicked instead of naming the
            // block would be the worst possible failure of this function.
            .map(|d| match &d.code {
                Some(code) => format!("{} [{}]", d.message, code),
                None => d.message.clone(),
            })
            .collect::<Vec<_>>()
            .join("; ")
    )
}

/// One spec as a line a human can compare against another.
///
/// Only the fields the accepted-equals-declared check can disagree on —
/// `name` and `artifact_sha256` are already pinned by the checks above, so
/// printing them again would bury the difference. `BlockCapabilities` derives
/// `Debug` and nothing that renders more usefully.
fn describe(spec: &DynamicBlockSpec) -> String {
    format!(
        "routes {:?}, capabilities {:?}",
        spec.routes
            .iter()
            .map(|route| format!("{} ({})", route.prefix, route.access.as_str()))
            .collect::<Vec<_>>(),
        spec.capabilities,
    )
}

/// Record a seeded artifact in the builds table.
///
/// A seed drops compiled artifacts straight into the store, which would
/// otherwise leave them the only artifacts no build row describes — and that
/// table is what `dev_status` reports the artifact store from, and what
/// `super::gc` deletes a row from when it collects the bytes. A seeded
/// instance would report an empty artifact store while running four blocks.
///
/// Written straight to [`BuildStatus::Valid`]: the bundle's spec is the
/// accepted spec the exporting instance's ledger held, and generation 0 —
/// which the caller activates next — already names the artifact, so the row is
/// never the thing keeping it reachable and must not be left `Staged`, which
/// is the collector's "a compile is still coming" marker.
///
/// `block_info_json` is the report [`RuntimeControl::inspect`] just read out
/// of the artifact — the same value `blocks_api::stage` records, obtained the
/// same way. It is not decoration: `blocks_api::claimed_tool_names` reads the
/// stored `BlockInfo` of every block in the active generation to apply the
/// duplicate-agent-tool rule, and refuses the whole stage when one cannot be
/// read. A `"null"` here (which is what this wrote before the importer had a
/// control) therefore meant a seeded instance could never compile a block of
/// its own — the first `dev_compile_block` came back `build-row-missing`.
async fn record_seeded_build(
    ctx: &dyn Context,
    spec: &DynamicBlockSpec,
    artifact_bytes: u64,
    info: &wafer_block::BlockInfo,
) -> Result<(), String> {
    let block_info_json = serde_json::to_string(info).map_err(|e| {
        format!(
            "the seeded block {:?}'s BlockInfo did not encode: {e}",
            spec.name
        )
    })?;
    let row = repo::builds::insert(
        ctx,
        &repo::builds::NewBuild {
            block_name: spec.name.clone(),
            source_manifest_sha256: String::new(),
            artifact_sha256: spec.artifact_sha256.clone(),
            block_info_json,
            diagnostics_json: "[]".to_string(),
            compiler_version: SEEDED_COMPILER_VERSION.to_string(),
            artifact_bytes,
        },
    )
    .await
    .map_err(|e| e.message)?;
    repo::builds::set_status(ctx, &row.id, repo::builds::BuildStatus::Valid, None, None)
        .await
        .map_err(|e| e.message)
}

/// What a seeded build records where a compile would record its toolchain: the
/// bundle produced the bytes, and no toolchain ran here.
const SEEDED_COMPILER_VERSION: &str = "seed-bundle";

// ---------------------------------------------------------------------------
// Recording a refused import where someone can see it
// ---------------------------------------------------------------------------

/// Admin variable a refused seed import writes its message to.
///
/// Block-scoped (`{ORG}__{BLOCK}__*`), not `WAFER_RUN_SHARED__*`: this is the
/// dev block's own record of what happened on this instance's first boot, not
/// a setting an operator configures, and the block-scoped prefix is what keeps
/// it out of every deployment that does not carry the block. It is also what
/// keeps it out of every EXPORT — `data_snapshot::variable_is_exportable`
/// refuses an `IMPRESSPRESS_`-prefixed key outright, so one instance's seed
/// failure can never travel into another instance's bundle.
///
/// The variables table rather than the console because of where the failure
/// lands: an exported bundle has no `/b/dev` (design amendment 19), so the
/// page that would otherwise show a refused import does not exist there. What
/// the owner of an exported site does have is `/b/admin/settings/variables`.
pub const SEED_ERROR_KEY: &str = "IMPRESSPRESS__DEV__SEED_ERROR";

/// Name and description the row carries when this creates it, so the
/// variables page shows a sentence rather than a bare key.
const SEED_ERROR_NAME: &str = "Seed import error";
const SEED_ERROR_DESCRIPTION: &str =
    "The last seed import on this instance refused the bundle served at /seed/, and this is \
     why. The site is empty because nothing was imported. Fix the bundle and load the site in \
     a fresh browser profile (or clear this site's data) to retry; the row clears itself on \
     the next import that succeeds.";

/// Record a refused import.
///
/// Best-effort by construction, and logged rather than returned: the caller is
/// already returning a failure, and a write that could turn "the seed was
/// refused" into "the seed was refused AND the ledger is broken" would replace
/// the diagnosis with a worse one.
///
/// Public for the one other failure with the same symptom and the same
/// permanence: the boot caller activates the generation [`import`] hands back,
/// and a seed that imported but would not activate leaves an equally empty
/// site that no later boot retries (the failed generation is in the ledger, so
/// `is_fresh` is false from then on).
pub async fn record_failure(ctx: &dyn Context, message: &str) {
    // `key` is the table's `UNIQUE` column and `upsert_by_key` is the one
    // write path every platform shares (the Cloudflare row cache refuses the
    // atomic upsert on this table); a boot that fails twice updates one row.
    // `created_at` is not in the patch — the row's age is when the first
    // refusal happened.
    let written = variables::upsert_by_key(
        ctx,
        SEED_ERROR_KEY,
        VariablePatch {
            value: Some(message.to_string()),
            name: Some(SEED_ERROR_NAME.to_string()),
            description: Some(SEED_ERROR_DESCRIPTION.to_string()),
            // Not a secret: it is a diagnostic about this instance's own
            // boot, and masking it would hide the one thing it exists to say.
            sensitive: Some(false),
            ..Default::default()
        },
    )
    .await;
    if let Err(e) = written {
        tracing::error!(
            error = %e.message,
            "dev sandbox: the seed import was refused and the refusal could not be recorded in \
             {SEED_ERROR_KEY}",
        );
    }
}

/// Clear the row a previous refusal left, after an import that worked.
///
/// Deleted rather than blanked: a healthy instance's variables page should not
/// carry an empty explanation of a failure that is over. Deleting a key with
/// no row affects nothing, which is what makes this callable unconditionally.
async fn clear_failure(ctx: &dyn Context) {
    let cleared = variables::delete_by_key(ctx, SEED_ERROR_KEY).await;
    if let Err(e) = cleared {
        tracing::error!(
            error = %e.message,
            "dev sandbox: the seed imported, but a previous refusal recorded in \
             {SEED_ERROR_KEY} could not be cleared",
        );
    }
}

/// The message a refused import left, if there is one.
///
/// Read from the row rather than through [`Context::config_get`]: the config
/// snapshot a runtime resolves is built at boot, *before* the import that
/// writes this runs, so a config read would answer with the previous boot's
/// answer to a question about this one.
pub async fn last_failure(ctx: &dyn Context) -> Result<Option<String>, wafer_run::WaferError> {
    Ok(variables::get_by_key(ctx, SEED_ERROR_KEY)
        .await?
        .map(|row| row.value)
        .filter(|message| !message.is_empty()))
}

/// Store one verified file's bytes and record it in the workspace under
/// construction.
///
/// Charges [`Workspace::record_blob_stored`] only when the store actually
/// grew, exactly as the files API does — two seed paths carrying the same
/// asset cost one blob and must be counted once.
async fn store(
    ctx: &dyn Context,
    ws: &mut Workspace,
    workspace_path: &str,
    bytes: &[u8],
) -> Result<(), String> {
    if ws.files.len() >= paths::MAX_FILES {
        return Err(format!(
            "the seed bundle carries more than the {} files a workspace holds",
            paths::MAX_FILES
        ));
    }
    // The same file/directory rule the files API enforces. A bundle is a set
    // of paths from another instance, and nothing about crossing the wire
    // makes a pair of them agree about whether a name is a directory — while
    // a bundle that got in would wedge this instance's publisher for good.
    if let Some(clash) = ws.path_collision(workspace_path) {
        return Err(format!(
            "the seed bundle carries both {workspace_path:?} and {clash:?}; one name cannot be              a file in one entry and a directory in the other"
        ));
    }
    let sha = blobs::sha256_hex(bytes);
    let grows_by = if ws.references(&sha) {
        0
    } else {
        bytes.len() as u64
    };
    if ws.blob_bytes.saturating_add(grows_by) > paths::MAX_WORKSPACE_BYTES {
        return Err(format!(
            "the seed bundle is larger than the {}-byte workspace quota",
            paths::MAX_WORKSPACE_BYTES
        ));
    }
    match blobs::put_hashed(ctx, &sha, bytes)
        .await
        .map_err(|e| e.message)?
    {
        blobs::Stored::New => ws.record_blob_stored(bytes.len() as u64),
        blobs::Stored::Deduplicated => {}
    }
    ws.insert(workspace_path, sha, bytes.len() as u64);
    Ok(())
}
