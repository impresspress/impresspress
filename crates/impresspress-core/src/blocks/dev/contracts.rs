//! Typed request/response contracts for the `/b/dev` JSON API.
//!
//! Every `///` doc comment here is published: the derived schema carries it
//! into `/openapi.json` and, through that, into the agent tool descriptions
//! the `/b/dev` page registers. Write them for the agent, not for the reader
//! of this file.

use serde::{Deserialize, Serialize};

use super::{
    activation::ProgressStep,
    control::{DynamicBlockSpec, DynamicRoute},
    generation::{GenerationDiff, GenerationManifest},
    paths::MAX_BATCH_FILES,
    repo::{
        generations::{GenerationCause, GenerationStatus},
        runtime_state::ActivationPhase,
    },
    scaffold::Template,
    validation::Diagnostic,
    workspace::FileEntry,
};

/// Response of `GET /b/dev/api/status`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusResponse {
    /// The active generation, or null on a fresh instance.
    pub active_generation: Option<GenerationSummary>,
    /// Bumped on every runtime rebuild; the page refreshes tool registrations
    /// when it changes.
    pub runtime_generation: u64,
    /// Blocks in the active generation.
    pub blocks: Vec<ActiveBlockView>,
    /// The activation in progress, if any.
    pub activation: Option<ActivationView>,
    /// What the sandbox's content stores hold right now.
    pub storage: StorageUsage,
    /// `WAFER_GUEST_VERSION` of the `wafer_guest` crate this sandbox serves
    /// (`GET /b/dev/api/guest`) and compiles blocks against.
    pub wafer_guest_version: u32,
    // `dev_runtime::install` logs a refused seed and carries on, because a
    // sandbox that refuses to boot is one whose `/b/dev` page — the only thing
    // that could fix it — never comes up; this field is what makes the cause
    // readable without the service worker's console. Read from the same
    // variables row an admin sees, which is the only surface an exported site
    // has (it has no `/b/dev`). `dev.js` does not render it: the page polls
    // this endpoint several times a second, so a log line would need its own
    // "said this already" state, and the agent that would act on a refused
    // seed reads `dev_status` rather than the log.
    /// Why this instance's seed import was refused, if it was.
    ///
    /// `null` on every healthy instance — including one that never had a
    /// seed bundle to import. A sandbox whose seed was refused boots with an
    /// empty site and no other sign of it, so this is where the cause is
    /// read. An admin sees the same text on `/b/admin/settings/variables`.
    pub seed_error: Option<String>,
    /// The template this sandbox was seeded from, or null when the seed
    /// carried no sandbox block. What an agent reads to know which guide
    /// `dev_read_reference.site_markdown` will be.
    pub template: Option<String>,
}

/// What the sandbox's two content stores, its workspace and its ledger hold.
///
/// Read from the workspace's own counters and the builds table on every
/// request, never by walking the stores — see [`super::gc::storage_usage`],
/// which owns that decision and the reason for it (a storage `list` is
/// `O(folder)` on the OPFS backend, and the page polls this three times a
/// second while a tool call is outstanding). The one full listing belongs to
/// the collector, which runs once per activation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorageUsage {
    /// How many content-addressed blobs are stored. Includes blobs no file
    /// names any more but a retained generation still does.
    pub blobs: u32,
    /// Total size of those blobs, in bytes.
    pub blobs_bytes: u64,
    /// How many compiled block artifacts are stored.
    pub artifacts: u32,
    /// Total size of those artifacts, in bytes.
    pub artifacts_bytes: u64,
    /// How many files the workspace currently holds.
    pub workspace_files: u32,
    /// How many generations the ledger is keeping: the retention window, plus
    /// anything older that is still serving or still in flight.
    pub retained_generations: u32,
}

/// One entry in the publication ledger, as the status and generation views
/// publish it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationSummary {
    /// Generation id.
    pub id: String,
    /// The generation this one was derived from, or null for the first.
    pub parent_id: Option<String>,
    /// What created this generation.
    pub cause: GenerationCause,
    /// Where the generation sits in its lifecycle.
    pub status: GenerationStatus,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 time the generation went live, or null if it never did.
    pub activated_at: Option<String>,
    // Written with the status by each writer of `failed` (listed on
    // `GenerationStatus::Failed`) and by nothing else: `set_status` leaves the
    // stored message alone when given none, and no transition leaves
    // `failed`. For an activation that failed it is `ActivationError`'s
    // `Display`, which is also the refusal `ActivationError::into_response`
    // answers the publishing request with.
    /// Why the generation failed: set when `status` is `failed`, null
    /// otherwise.
    ///
    /// When its validation or activation failed, this is the message a
    /// request that publishes it is refused with, and it names what failed:
    /// site content or a block artifact that is not stored, the error that
    /// stopped the blocks from loading, or a storage failure. When the
    /// sandbox stopped before its activation finished, it says that instead,
    /// which is not a verdict on the generation's content.
    pub failure_message: Option<String>,
    /// Number of files in the generation's site manifest.
    pub site_files: u32,
    /// Number of blocks in the generation's block manifest.
    pub blocks: u32,
}

/// A block serving in the active generation.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActiveBlockView {
    /// Registered block name.
    pub name: String,
    /// SHA-256 of the artifact the block was loaded from, hex-encoded.
    pub artifact_sha256: String,
    /// Route prefixes the block serves, with the access tier the router
    /// enforces for each.
    pub routes: Vec<DynamicRoute>,
}

/// An activation the sandbox is working through right now.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivationView {
    /// The generation being activated.
    pub generation_id: String,
    /// Which phase it has reached.
    pub phase: ActivationPhase,
    /// Human-readable detail for the progress panel.
    pub detail: String,
}

/// The `site` half of a generation manifest (design §11.3).
///
/// Its entries are [`FileEntry`] — the workspace manifest's own type, with
/// the `site/` prefix stripped. A generation IS the workspace's `site/`
/// entries frozen, so a separate identically-shaped type would be a mapping
/// layer between two spellings of one thing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SiteManifest {
    /// Every file the generation publishes, path relative to the site root.
    #[serde(default)]
    pub files: Vec<FileEntry>,
}

impl ActiveBlockView {
    /// Project a block manifest entry into the view. The manifest entry and
    /// the spec the runtime is built from are the same type
    /// ([`DynamicBlockSpec`]), so this is the only place the wire view drops
    /// fields — capabilities and the guest ABI version are not published.
    pub fn from_spec(spec: &DynamicBlockSpec) -> Self {
        Self {
            name: spec.name.clone(),
            artifact_sha256: spec.artifact_sha256.clone(),
            routes: spec.routes.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// The generations API (`/b/dev/api/generations*`)
// ---------------------------------------------------------------------------

/// Query parameters of `GET /b/dev/api/generations`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GenerationListQuery {
    /// How many generations to return, newest first. Defaults to the
    /// retention window (20) and is capped at 200.
    pub limit: Option<u32>,
}

impl GenerationListQuery {
    /// Read the query off a request.
    ///
    /// The one place `?limit=` is parsed, so the published schema and the
    /// handler cannot describe different parameters. A value that is not a
    /// number is `None` — the default — rather than a `400`: a listing is a
    /// read, and refusing it teaches a caller nothing it could not see from
    /// the page size it got back.
    pub fn from_message(msg: &wafer_run::Message) -> Self {
        Self {
            limit: msg.query("limit").parse().ok(),
        }
    }
}

/// Path parameters of every `/b/dev/api/generations/{id}*` route.
///
/// Declared as a type rather than a hand-written schema so the published
/// parameter and the `{id}` the router binds cannot describe different things:
/// `wafer_core::discovery` cross-checks the declared names against the path
/// template's placeholders when it builds the agent tool for the endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GenerationPathParams {
    /// The generation's id, as `GET /b/dev/api/generations` reports it.
    pub id: String,
}

/// Response of `GET /b/dev/api/generations`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationListResponse {
    /// Matching generations, newest first.
    pub generations: Vec<GenerationSummary>,
}

/// Response of `GET /b/dev/api/generations/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationDetail {
    /// The ledger entry.
    pub summary: GenerationSummary,
    /// The manifest the generation publishes: its site files and its blocks.
    pub manifest: GenerationManifest,
    /// What this generation changed relative to the one it was derived from.
    /// A generation with no parent adds everything it holds.
    pub diff_from_parent: GenerationDiff,
}

/// Response of every endpoint whose whole job is to activate a generation.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivationResponse {
    /// The generation that went live.
    pub generation: GenerationSummary,
    /// One entry per phase the activation passed through, with how long it
    /// took. The last is always `active`.
    pub progress: Vec<ProgressStep>,
}

// ---------------------------------------------------------------------------
// The files API (`/b/dev/api/files*`)
// ---------------------------------------------------------------------------

/// Query parameters of `GET /b/dev/api/files`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileListQuery {
    /// List only files whose workspace path starts with this prefix, e.g.
    /// `site/` or `blocks/hello/`. Omit to list the whole workspace.
    pub prefix: Option<String>,
}

impl FileListQuery {
    /// Read the query off a request.
    ///
    /// The type has a runtime user, not just a published schema: this is the
    /// one place `?prefix=` is parsed, so the schema and the handler cannot
    /// describe different parameters.
    pub fn from_message(msg: &wafer_run::Message) -> Self {
        let prefix = msg.query("prefix");
        Self {
            prefix: (!prefix.is_empty()).then(|| prefix.to_string()),
        }
    }
}

/// Response of `GET /b/dev/api/files`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileListResponse {
    /// Matching files, in path order.
    pub files: Vec<FileEntry>,
}

/// How a file's bytes are carried in a JSON body.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum FileEncoding {
    /// `content` is the file's text. The default.
    #[default]
    Utf8,
    /// `content` is the file's bytes, standard base64 with padding.
    Base64,
}

/// Request of `POST /b/dev/api/files/read`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileReadRequest {
    /// Workspace-relative path, e.g. `site/index.html`.
    pub path: String,
}

/// Response of `POST /b/dev/api/files/read`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileReadResponse {
    /// Workspace-relative path that was read.
    pub path: String,
    /// SHA-256 of the content, hex-encoded. Pass it back as
    /// `expected_sha256` to write over what you just read.
    pub sha256: String,
    /// Size in bytes of the decoded content.
    pub size: u64,
    /// How `content` is encoded. Text files come back as `utf8`; anything
    /// else, including text that is not valid UTF-8, comes back as `base64`.
    pub encoding: FileEncoding,
    /// The file's content, in `encoding`.
    pub content: String,
}

/// Request of `POST /b/dev/api/files/write`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileWriteRequest {
    /// Workspace-relative path under `site/` or `blocks/<name>/`.
    pub path: String,
    /// The file's content, in `encoding`.
    pub content: String,
    /// How `content` is encoded. Defaults to `utf8`.
    #[serde(default)]
    pub encoding: FileEncoding,
    /// The SHA-256 the file has now, as your last read returned it, or
    /// `null` if you expect the file not to exist yet. Leaving the field out
    /// is the same as `null`. If the file changed since you read it, the
    /// write is refused with `409` and the file's current hash: re-read it,
    /// then write again.
    #[serde(default)]
    pub expected_sha256: Option<String>,
}

/// Response of `POST /b/dev/api/files/write`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileWriteResponse {
    /// Workspace-relative path that was written.
    pub path: String,
    /// SHA-256 of the stored content, hex-encoded. Pass it as the next
    /// write's `expected_sha256`.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
    /// The generation this write published, when it published one. A write
    /// under `site/` publishes; a write under `blocks/` does not — only a
    /// compile turns block source into a published block.
    pub generation: Option<GenerationSummary>,
    /// One entry per phase the write's activation passed through, with how
    /// long it took. The last is `active`. Empty when nothing was published.
    pub progress: Vec<ProgressStep>,
}

/// Request of `POST /b/dev/api/files/write-batch`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileWriteBatchRequest {
    /// The files to write, 1 to 64, all under `site/` or all under one
    /// `blocks/<name>/`. Each entry is exactly a `dev_write_file` request.
    #[schemars(length(min = 1, max = MAX_BATCH_FILES))]
    pub files: Vec<FileWriteRequest>,
}

/// Response of `POST /b/dev/api/files/write-batch`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileWriteBatchResponse {
    /// Every file written, in path order, each with the hash to pass as its
    /// next `expected_sha256`.
    pub files: Vec<FileEntry>,
    /// The one generation the batch published, or null for a `blocks/`
    /// batch — only a compile turns block source into a published block.
    pub generation: Option<GenerationSummary>,
    /// One entry per phase the batch's activation passed through, with how
    /// long it took. The last is `active`. Empty when nothing was published.
    pub progress: Vec<ProgressStep>,
}

/// The `409` of a batch: every entry whose `expected_sha256` did not
/// describe the file as it stands. Nothing was written.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileWriteBatchConflict {
    /// In request order.
    pub conflicts: Vec<FileConflict>,
}

/// Request of `POST /b/dev/api/files/delete`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileDeleteRequest {
    /// Workspace-relative path to remove.
    pub path: String,
    /// The SHA-256 you expect the file to have right now. A mismatch — a
    /// file that changed, or is already gone — is a `409`.
    pub expected_sha256: String,
}

/// Response of `POST /b/dev/api/files/delete`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileDeleteResponse {
    /// Workspace-relative path that was removed.
    pub path: String,
    /// The generation this delete published, when it published one. A delete
    /// under `site/` publishes; a delete under `blocks/` does not.
    pub generation: Option<GenerationSummary>,
    /// One entry per phase the delete's activation passed through, with how
    /// long it took. The last is `active`. Empty when nothing was published.
    pub progress: Vec<ProgressStep>,
}

/// Body of the `409` a write or delete answers when `expected_sha256` does not
/// describe the file as it stands.
///
/// It reports the *current* state so a caller can re-read, merge and retry
/// without a second round trip. Both fields are `null` when the path holds no
/// file at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileConflict {
    /// Workspace-relative path the conflict is about.
    pub path: String,
    /// SHA-256 the file currently has, or `null` when there is no file.
    pub current_sha256: Option<String>,
    /// Size the file currently has, or `null` when there is no file.
    pub current_size: Option<u64>,
}

impl FileConflict {
    /// Describe `path` given the entry it currently holds, if any.
    pub fn new(path: &str, current: Option<&FileEntry>) -> Self {
        Self {
            path: path.to_string(),
            current_sha256: current.map(|entry| entry.sha256.clone()),
            current_size: current.map(|entry| entry.size),
        }
    }
}

// ---------------------------------------------------------------------------
// Staging and removing blocks (`/b/dev/api/builds/stage`, `/b/dev/api/blocks*`)
// ---------------------------------------------------------------------------

/// Request of `POST /b/dev/api/builds/stage`.
///
/// One compiled `wasm32-wasip1` module, with everything needed to explain it
/// afterwards. Staging validates the artifact and, if it passes, activates a
/// new generation carrying it — there is no separate "activate" call.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StageBuildRequest {
    /// The block's short name, e.g. `hello` for the sources under
    /// `blocks/hello/`. It is registered as `site/hello` and serves
    /// `/b/hello/`; do not send either of those longer forms.
    pub block_name: String,
    /// The compiled module, standard base64 with padding. At most 4 MiB
    /// decoded.
    pub artifact_base64: String,
    /// SHA-256 of the source manifest the compile ran against, so a stored
    /// build can be traced back to the exact sources. Omit if the compiler
    /// did not report one.
    #[serde(default)]
    pub source_manifest_sha256: Option<String>,
    /// Pinned toolchain revision that produced the artifact.
    pub compiler_version: String,
    /// Diagnostics the compiler produced, warnings included. They are stored
    /// with the build and returned alongside any the validator adds.
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    /// The `WAFER_GUEST_VERSION` of the `wafer_guest` crate the compiler
    /// session was started with: `version` from `GET /b/dev/api/guest`.
    ///
    /// A value that is not the sandbox's own is refused with a
    /// `wafer-guest-version` diagnostic: the guest crate IS the ABI, so a
    /// block built against an older copy is talking a contract this runtime
    /// no longer speaks. Reload the workspace page (a fresh compiler session
    /// fetches and builds the current crate) and compile again.
    ///
    /// Omit it only if the compiler session has no guest crate. It is then
    /// recorded as `0` — "unknown" — and nothing is checked.
    #[serde(default)]
    pub wafer_guest_version: Option<u32>,
}

/// Response of `POST /b/dev/api/builds/stage`.
///
/// A refused block is a result, not a transport failure: the status is `200`
/// with `success: false` and the reasons in `diagnostics`. Only a malformed
/// request — bad JSON, or `artifact_base64` that is not base64 — is a `4xx`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StageBuildResponse {
    /// The stored build's id, or null when the request was refused before a
    /// build could be recorded (an artifact over the size limit is never
    /// stored, so there is no build record to point at).
    pub build_id: Option<String>,
    /// Whether the block was accepted and activated.
    pub success: bool,
    /// Everything known about this build: the diagnostics the compiler
    /// reported, then any the validator added. `severity` tells them apart —
    /// a refusal is always an `error`.
    pub diagnostics: Vec<Diagnostic>,
    /// The generation the accepted block went live in, or null when the
    /// build was refused.
    pub generation: Option<GenerationSummary>,
    /// One entry per phase the activation passed through, with how long it
    /// took. Empty when nothing was activated.
    pub progress: Vec<ProgressStep>,
}

/// Path parameters of every `/b/dev/api/blocks/{name}*` route.
///
/// Declared as a type rather than a hand-written schema for the same reason
/// [`GenerationPathParams`] is: `wafer_core::discovery` cross-checks the
/// declared names against the path template's placeholders when it builds the
/// agent tool for the endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlockPathParams {
    /// The block's short name, e.g. `hello` for the block registered as
    /// `site/hello`.
    pub name: String,
}

// ---------------------------------------------------------------------------
// Scaffolding a block (`/b/dev/api/blocks`, `/b/dev/api/reference`)
// ---------------------------------------------------------------------------

/// Request of `POST /b/dev/api/blocks`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateBlockRequest {
    /// The block's short name, e.g. `newsletter`. It becomes the directory
    /// `blocks/<name>/`, the crate name, the block id `site/<name>`, the
    /// route prefix `/b/<name>/` and the collection prefix `site__<name>__`
    /// (a hyphen spelled `_` there). 2 to 32 characters: a lowercase letter
    /// followed by lowercase letters, digits and hyphens, with no doubled
    /// hyphen and no trailing hyphen.
    pub name: String,
    /// Which starting point to write. `hello` is one public `GET` and
    /// nothing else; `table` is a newsletter block with a database table, an
    /// agent tool and two admin reads — start there for anything that stores
    /// data.
    pub template: Template,
}

/// Response of `POST /b/dev/api/blocks`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateBlockResponse {
    /// The block's short name, as written.
    pub name: String,
    /// The files that were written, in path order. Read or edit them through
    /// the files API; the block does not serve until it is compiled.
    pub files: Vec<FileEntry>,
}

/// Response of `GET /b/dev/api/reference`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReferenceResponse {
    /// The `WAFER_GUEST_VERSION` of the guest SDK crate this reference
    /// documents and every scaffolded block depends on.
    pub wafer_guest_version: u32,
    /// The backend-block (Rust) authoring guide, as Markdown: the API, the
    /// host services, the namespace rules, the limits, the diagnostic codes,
    /// and both templates in full.
    pub markdown: String,
    /// The template this sandbox was seeded from (`dev_status.template`), or
    /// null when the seed carried no sandbox block.
    pub template: Option<String>,
    /// The task this template was designed to walk an agent through, as the
    /// workspace page suggests it. Empty when the template suggests none or
    /// the sandbox carries no seed.
    pub suggested_prompt: String,
    /// This sandbox's site-authoring guide, as Markdown: the CSS framework
    /// it ships, the page skeleton, the storefront element, the catalog API,
    /// what a write refuses. Read it before writing under `site/`. Null when
    /// the seed carried none.
    pub site_markdown: Option<String>,
}

/// Response of `GET /b/dev/api/guest`.
///
/// Everything the compiler session needs before the first compile: the
/// guest crate a block depends on, and a block to build it with.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuestResponse {
    /// `WAFER_GUEST_VERSION` of the crate below. A block compiled in a
    /// session started from this response reports this number to
    /// `POST /b/dev/api/builds/stage`.
    pub version: u32,
    /// The `wafer_guest` crate, crate-relative: `Cargo.toml`, `src/lib.rs`.
    pub files: std::collections::BTreeMap<String, String>,
    /// The `hello` template scaffolded as `hello`: what the compiler builds
    /// once at start-up so the crate above is compiled before any real block.
    pub warmup: WarmupCrate,
}

/// A block crate the compiler builds to warm its target directory.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WarmupCrate {
    /// Cargo's package name, which names the artifact and the VFS directory:
    /// the crate is built at `blocks/<crate_name>/`, with `blocks/` beside
    /// `wafer_guest/`, which is the layout its `../../wafer_guest` dependency
    /// needs.
    pub crate_name: String,
    /// Crate-relative paths: `Cargo.toml`, `src/lib.rs`.
    pub files: std::collections::BTreeMap<String, String>,
}

/// One entry of the export bundle: where it lands in the zip, and how big it
/// is.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportFile {
    /// Path inside the archive (`sw.js`, `seed/site/index.html`).
    pub path: String,
    /// The entry's uncompressed size in bytes. The archive stores entries
    /// uncompressed, so this is also what it costs in the zip.
    pub bytes: u64,
}

/// Response of `GET /b/dev/api/export/manifest` — what
/// `GET /b/dev/api/export` would produce, without producing it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportManifest {
    /// The generation the export is a snapshot of — the one that is live now.
    /// The downloaded file is named after its first eight characters.
    pub generation_id: String,
    /// Every entry of the archive, in the order it is written.
    pub files: Vec<ExportFile>,
    /// Total size of every entry's content. The archive itself is slightly
    /// larger: each entry carries a local header and a central directory
    /// record naming it.
    pub total_bytes: u64,
    /// How many of `files` are the runtime shell (the service worker, the
    /// wasm, the loader — everything that makes the folder runnable).
    pub shell_files: u32,
    /// How many are the site's own files, under `seed/site/`.
    pub site_files: u32,
    /// How many compiled blocks the export carries. Each contributes its
    /// `.wasm` plus its whole source tree.
    pub blocks: u32,
    /// Rows the data snapshot carries, per table — products, offers,
    /// settings and accounts (`seed/data.json`).
    pub tables: std::collections::BTreeMap<String, usize>,
}
