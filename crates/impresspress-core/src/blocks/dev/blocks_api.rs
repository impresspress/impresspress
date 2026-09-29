//! `/b/dev/api/builds/stage` and `/b/dev/api/blocks/{name}/remove` — putting
//! a compiled guest into the runtime, and taking one back out.
//!
//! # Staging is the whole pipeline
//!
//! There is no separate "activate" call. `stage` decodes the artifact, stores
//! it, records the build, runs both halves of validation and — when they pass
//! — requests the activation that makes the block live. That is deliberate:
//! an accepted artifact that is not yet serving is a state the ledger would
//! have to describe and the page would have to reconcile, and nothing in the
//! design wants it.
//!
//! # The order validation runs in
//!
//! `inspect` (read the guest's `BlockInfo` under deny-all capabilities) →
//! `validate_static` (the rules, which turn that declaration into an accepted
//! spec) → `probe` (Init/Start/one request under the ACCEPTED capabilities).
//!
//! Running the lifecycle first — under whatever the guest declared — would
//! execute untrusted code with authority nothing had approved: a module
//! declaring `collections: Any` would have its `Init` run with it, and the
//! refusal would arrive afterwards. `inspect` therefore runs no guest code
//! beyond instantiation and `__wafer_info`, and `probe` runs under exactly
//! the spec `rebuild` will later be handed.
//!
//! # Why a refusal is a 200
//!
//! The caller is an agent. Design §7.4 is explicit that validation refusals
//! are "structured diagnostics in the tool result, never a transport error":
//! a `422` tells the agent its request was bad, a `success: false` with a
//! `cap-collection` diagnostic tells it which line of its block to change.
//! Only a request the transport could not carry — malformed JSON, or an
//! `artifact_base64` that is not base64 — is a `4xx`.
//!
//! # The compile lock
//!
//! Both handlers resolve the *whole* next block set from the one that is
//! active now (design §6.6 allows one compile at a time, and
//! [`ActivationIntent::BlockSet`] carries a complete set rather than a
//! delta). Reading the active set and requesting the activation therefore
//! have to be one indivisible section: two block changes that interleaved
//! between their read and their request would each compose a set from a
//! snapshot that predates the other, and the later one would silently drop
//! the earlier block. [`super::DevShared::compile`] is that section, and it
//! is held from the read through `activation::request`.
//!
//! Site writes are unaffected: [`ActivationIntent::SiteOnly`] composes its
//! block half at dequeue, from whatever is active then, so an edit never
//! waits behind a compile and a compile never loses an edit.

use std::collections::BTreeSet;

use base64ct::{Base64, Encoding};
use wafer_block::BlockInfo;
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream, WaferError};

use super::{
    activation::{self, ActivationIntent},
    artifacts,
    blobs::sha256_hex,
    contracts::{ActivationResponse, StageBuildRequest, StageBuildResponse},
    control::DynamicBlockSpec,
    generation, no_store, no_store_db_error_internal, no_store_error, paths,
    repo::{
        self,
        builds::{BuildStatus, NewBuild},
        generations::GenerationCause,
    },
    validation::{self, Diagnostic},
    DevShared,
};
use crate::blocks::crud;

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/// `POST /b/dev/api/builds/stage` — validate a compiled guest and activate it.
pub async fn handle_stage(
    ctx: &dyn Context,
    shared: &DevShared,
    input: InputStream,
) -> OutputStream {
    let request: StageBuildRequest = match read_body(input).await {
        Ok(request) => request,
        Err(refusal) => return refusal,
    };
    // The size limit is checked twice, on either side of the decode. Before,
    // on a bound the encoding guarantees, so an enormous body is refused
    // without a second allocation the size of the first; after, on the real
    // length, because the bound deliberately under-estimates and must never
    // refuse a legal artifact.
    let bound = paths::min_base64_decoded_len(&request.artifact_base64);
    if bound > validation::MAX_ARTIFACT_BYTES {
        return refused(
            None,
            request.diagnostics,
            Diagnostic::artifact_too_large(bound),
        );
    }
    let Ok(artifact) = Base64::decode_vec(&request.artifact_base64) else {
        return no_store_error(
            ErrorCode::InvalidArgument,
            "`artifact_base64` is not valid base64",
        );
    };

    // Two refusals happen before the guest is executed at all. An oversized
    // module is never stored (the limit exists to bound what the store
    // holds), and a name that can never be registered is never a reason to
    // load an untrusted module — the provisional spec the host would be
    // handed is built from that very name.
    if artifact.len() > validation::MAX_ARTIFACT_BYTES {
        return refused(
            None,
            request.diagnostics,
            Diagnostic::artifact_too_large(artifact.len()),
        );
    }
    if !paths::block_name_is_valid(&request.block_name) {
        return refused(
            None,
            request.diagnostics,
            validation::name_format_diagnostic(&request.block_name),
        );
    }
    // The third pre-execution refusal, for the same reason as the other two:
    // a module built against a different `wafer_guest.rs` is not a module
    // this runtime's ABI describes, and loading it would turn a one-line
    // "replace the module and recompile" into a trap inside wasmi. A request that
    // reports no version at all is a compiler that could not read the file;
    // it is recorded as `0` further down and nothing is checked.
    if let Some(reported) = request.wafer_guest_version {
        if reported != super::WAFER_GUEST_VERSION {
            return refused(
                None,
                request.diagnostics,
                Diagnostic::stale_guest_module(reported, super::WAFER_GUEST_VERSION),
            );
        }
    }

    match stage(ctx, shared, &request, &artifact).await {
        Ok(response) => response,
        Err(e) => no_store_db_error_internal(e, "dev build stage"),
    }
}

/// The part of staging that can fail on storage or the ledger.
///
/// Split out so every `?` is classified in one place by the handler above
/// (`super::no_store_db_error_internal`: a WRAP denial is a `403`, a quota a
/// `429`, anything else a `500` with a correlation id), and the handler is
/// otherwise only the shape of the refusals that are *results*.
async fn stage(
    ctx: &dyn Context,
    shared: &DevShared,
    request: &StageBuildRequest,
    artifact: &[u8],
) -> Result<OutputStream, WaferError> {
    let name = request.block_name.as_str();
    let registered = format!("site/{name}");

    // The row goes in BEFORE the bytes, and that order is what keeps the
    // garbage collector's hands off them: an artifact no generation names yet
    // is protected by its build row alone (see `super::gc`), so bytes stored
    // ahead of the row would be unreachable for as long as the insert took.
    // The hash is the artifact's own content hash either way — `artifacts::put`
    // files it under exactly this key.
    let artifact_sha256 = sha256_hex(artifact);
    let build = repo::builds::insert(
        ctx,
        &NewBuild {
            block_name: registered.clone(),
            source_manifest_sha256: request.source_manifest_sha256.clone().unwrap_or_default(),
            artifact_sha256: artifact_sha256.clone(),
            // Filled in by `set_status` once the guest has reported one; the
            // column is `NOT NULL`, and `"null"` is the JSON for "none yet".
            block_info_json: "null".to_string(),
            diagnostics_json: serde_json::to_string(&request.diagnostics)
                .map_err(encoding_error)?,
            compiler_version: request.compiler_version.clone(),
            artifact_bytes: artifact.len() as u64,
        },
    )
    .await?;

    // Content-addressed, so re-staging identical bytes is a no-op write and
    // two blocks that compiled to the same module share one object.
    //
    // A failed store takes the row back out. The row's whole job is to say
    // "these bytes are stored and on their way to a generation"; left behind
    // for bytes that never arrived it would tell the collector to protect an
    // object that does not exist, and tell `dev_status` the artifact store
    // holds it. Best effort — if the delete fails too, the next boot's
    // `retire_in_flight` closes the row.
    let stored = match artifacts::put(ctx, artifact).await {
        Ok(stored) => stored,
        Err(e) => {
            if let Err(cleanup) = repo::builds::delete(ctx, &build.id).await {
                tracing::error!(
                    build_id = %build.id,
                    error = %cleanup.message,
                    "dev sandbox: could not drop the build row of an artifact that failed to store",
                );
            }
            return Err(e);
        }
    };
    debug_assert_eq!(
        stored, artifact_sha256,
        "artifact key must be its content hash"
    );

    // From here to the activation, the active block set must not change
    // under us — see the module docs.
    let _compiling = shared.compile.lock().await;

    let active = generation::active(ctx).await?;
    let live: Vec<DynamicBlockSpec> = active.map_or_else(Vec::new, |(_row, m)| m.blocks);
    let others: Vec<DynamicBlockSpec> = live
        .iter()
        .filter(|block| block.name != registered)
        .cloned()
        .collect();

    // Step 1: read the guest's own `BlockInfo`, under deny-all capabilities
    // and without running a single lifecycle event. Everything the rules
    // below decide is in that value, including the capability set the guest
    // is asking for.
    let info = match shared.control.inspect(artifact).await {
        Ok(info) => info,
        Err(failure) => {
            let diagnostics = together(&request.diagnostics, vec![Diagnostic::guest(&failure)]);
            invalidate(ctx, &build.id, &diagnostics).await?;
            return Ok(refusal_response(Some(build.id), diagnostics));
        }
    };

    let claimed = match claimed_tool_names(ctx, &others).await? {
        ClaimedToolNames::Complete(claimed) => claimed,
        ClaimedToolNames::Incomplete(diagnostic) => {
            let diagnostics = together(&request.diagnostics, vec![diagnostic]);
            invalidate(ctx, &build.id, &diagnostics).await?;
            return Ok(refusal_response(Some(build.id), diagnostics));
        }
    };

    // Step 2: the rules, against the block set this one is joining. What they
    // return IS the authority the guest gets — nothing else grants any.
    let mut spec = match validation::validate_static(
        name,
        &info,
        &artifact_sha256,
        &validation::builtin_route_prefixes(),
        &others,
        &claimed,
    ) {
        Ok(spec) => spec,
        Err(found) => {
            let diagnostics = together(&request.diagnostics, found);
            invalidate(ctx, &build.id, &diagnostics).await?;
            return Ok(refusal_response(Some(build.id), diagnostics));
        }
    };
    // `BlockInfo` has no field for it, so the version cannot come out of
    // `validate_static` — it is what the *request* reported, checked equal to
    // the sandbox's own above. `0` records "the compiler did not report one".
    spec.wafer_guest_version = request.wafer_guest_version.unwrap_or(0);

    // Step 3: run the guest, under the accepted spec. This is the same value
    // `rebuild` is handed below, so a guest that traps here would have
    // trapped live.
    if let Err(failure) = shared.control.probe(&spec, artifact).await {
        let diagnostics = together(&request.diagnostics, vec![Diagnostic::guest(&failure)]);
        invalidate(ctx, &build.id, &diagnostics).await?;
        return Ok(refusal_response(Some(build.id), diagnostics));
    }

    // The reported `BlockInfo` is recorded now — the guest has just produced
    // it, and a process that dies during the activation below must not lose
    // it — but the row STAYS staged. `Staged` is what tells the collector this
    // artifact is on its way to a generation and must not be touched
    // (`super::gc`), and it is on its way until the activation has minted one.
    // Accepting it here instead would leave the artifact rootless for the
    // whole time the request sits in the activation queue — which is exactly
    // when the activation ahead of it runs the collector.
    let block_info_json = serde_json::to_string(&info).map_err(encoding_error)?;
    repo::builds::set_status(
        ctx,
        &build.id,
        BuildStatus::Staged,
        None,
        Some(&block_info_json),
    )
    .await?;

    let mut blocks = others;
    blocks.push(spec);
    let activated = activation::request(
        ctx,
        shared,
        GenerationCause::BlockCompile,
        ActivationIntent::BlockSet { site: None, blocks },
    )
    .await;

    // Accepted either way: the guest passed every rule and the probe, which is
    // what `Valid` states. Whether the *activation* landed is the generation
    // ledger's business — and it is also what the artifact's reachability now
    // rests on, since a refused activation still leaves a `failed` generation
    // naming it until that row ages out.
    repo::builds::set_status(ctx, &build.id, BuildStatus::Valid, None, None).await?;

    let outcome = match activated {
        Ok(outcome) => outcome,
        Err(e) => return Ok(e.into_response()),
    };

    Ok(no_store().json(&StageBuildResponse {
        build_id: Some(build.id),
        success: true,
        diagnostics: request.diagnostics.clone(),
        generation: Some(outcome.generation),
        progress: outcome.progress,
    }))
}

/// The outcome of gathering every agent tool name already claimed.
///
/// Two cases, because "I could not read one of the active blocks" is not the
/// same as "nothing else claims this name" and must never be spelled the same
/// way. Silently skipping a block would disable half the duplicate rule for
/// exactly the block the rule is about.
enum ClaimedToolNames {
    /// Every active block's `BlockInfo` was read.
    Complete(BTreeSet<String>),
    /// A block in the active set has no readable stored `BlockInfo`, so the
    /// rule cannot be applied and the stage is refused.
    Incomplete(Diagnostic),
}

/// Every agent tool name already claimed: by a built-in block, or by one of
/// the dynamic blocks already in the target generation.
///
/// The dynamic half comes from each block's stored `BlockInfo` rather than
/// from the generation manifest, which carries routes and capabilities but
/// not endpoints. `others` is the target generation with the block being
/// staged taken out, so a block being RECOMPILED never claims against itself.
///
/// The built-in half is `validation::builtin_agent_tool_names` and not
/// `ctx.registered_blocks()` for the same reason, one layer down: a rebuilt
/// runtime has the live dynamic blocks registered in it too, so the
/// unfiltered list carries the previous version of the very block being
/// staged. See that function for why the filter is the `site/` namespace
/// rather than the names in the generation.
async fn claimed_tool_names(
    ctx: &dyn Context,
    others: &[DynamicBlockSpec],
) -> Result<ClaimedToolNames, WaferError> {
    let mut claimed: BTreeSet<String> =
        validation::builtin_agent_tool_names(ctx.registered_blocks());
    for block in others {
        // A block is in the active set because a build row accepted it, so a
        // row that is gone or unreadable is a broken invariant — not a licence
        // to skip the check. Refusing the stage keeps the guarantee ("no two
        // blocks claim one tool name") true; skipping would leave it silently
        // untested for exactly the block the rule is about.
        let Some(row) =
            repo::builds::latest_valid_for_artifact(ctx, &block.artifact_sha256).await?
        else {
            return Ok(ClaimedToolNames::Incomplete(Diagnostic::error(
                validation::BUILD_ROW_MISSING,
                format!(
                    "{} is in the active generation but has no accepted build recording its \
                     BlockInfo, so its agent tool names cannot be checked for collisions; \
                     remove and re-stage it",
                    block.name
                ),
            )));
        };
        let info: BlockInfo = match serde_json::from_str(&row.block_info_json) {
            Ok(info) => info,
            Err(e) => {
                return Ok(ClaimedToolNames::Incomplete(Diagnostic::error(
                    validation::BUILD_ROW_MISSING,
                    format!(
                        "build {} for {} holds a BlockInfo that cannot be read ({e}), so its \
                         agent tool names cannot be checked for collisions; remove and re-stage \
                         that block",
                        row.id, block.name
                    ),
                )));
            }
        };
        claimed.extend(validation::agent_tool_names(std::slice::from_ref(&info)));
    }
    Ok(ClaimedToolNames::Complete(claimed))
}

/// Mark a build refused, keeping the reasons on the row.
async fn invalidate(
    ctx: &dyn Context,
    build_id: &str,
    diagnostics: &[Diagnostic],
) -> Result<(), WaferError> {
    let diagnostics_json = serde_json::to_string(diagnostics).map_err(encoding_error)?;
    repo::builds::set_status(
        ctx,
        build_id,
        BuildStatus::Invalid,
        Some(&diagnostics_json),
        None,
    )
    .await
}

/// A refusal that happened before any row could exist.
fn refused(
    build_id: Option<String>,
    compiler: Vec<Diagnostic>,
    refusal: Diagnostic,
) -> OutputStream {
    refusal_response(build_id, together(&compiler, vec![refusal]))
}

/// The `200` body every refusal answers with.
fn refusal_response(build_id: Option<String>, diagnostics: Vec<Diagnostic>) -> OutputStream {
    no_store().json(&StageBuildResponse {
        build_id,
        success: false,
        diagnostics,
        generation: None,
        progress: Vec::new(),
    })
}

/// The compiler's diagnostics, then the validator's.
///
/// One list rather than two fields: both are things said about this build,
/// and `severity` is what tells a warning from a reason. Ordering is
/// compiler-first so a refusal reads as the last word.
fn together(compiler: &[Diagnostic], found: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut all = compiler.to_vec();
    all.extend(found);
    all
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

/// `POST /b/dev/api/blocks/{name}/remove` — take a block out of the runtime.
///
/// The block's sources stay in the workspace: removal is about what is
/// serving, and an agent that removed a block to fix it needs the code it was
/// fixing. The artifact stays stored too — an earlier generation still names
/// it, and rolling back to that generation has to work.
pub async fn handle_remove(ctx: &dyn Context, shared: &DevShared, msg: &Message) -> OutputStream {
    let name = msg.var("name").to_string();
    if !paths::block_name_is_valid(&name) {
        return no_store_error(
            ErrorCode::InvalidArgument,
            &format!("{name:?} is not a legal block name"),
        );
    }
    match remove(ctx, shared, &name).await {
        Ok(response) => response,
        Err(e) => no_store_db_error_internal(e, "dev block remove"),
    }
}

async fn remove(
    ctx: &dyn Context,
    shared: &DevShared,
    name: &str,
) -> Result<OutputStream, WaferError> {
    let registered = format!("site/{name}");

    // Same section as staging, for the same reason: the next block set is
    // resolved from the current one.
    let _compiling = shared.compile.lock().await;

    let live: Vec<DynamicBlockSpec> = generation::active(ctx)
        .await?
        .map_or_else(Vec::new, |(_row, manifest)| manifest.blocks);
    if !live.iter().any(|block| block.name == registered) {
        // A `404` rather than an idempotent success: an activation that
        // changed nothing would still mint a ledger entry saying a block was
        // removed, and the agent would have no way to tell that from a
        // removal that did something.
        return Ok(no_store_error(
            ErrorCode::NotFound,
            &format!("no block {registered:?} is in the active generation"),
        ));
    }
    let blocks: Vec<DynamicBlockSpec> = live
        .into_iter()
        .filter(|block| block.name != registered)
        .collect();

    let outcome = match activation::request(
        ctx,
        shared,
        GenerationCause::BlockRemove,
        ActivationIntent::BlockSet { site: None, blocks },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(e) => return Ok(e.into_response()),
    };
    Ok(no_store().json(&ActivationResponse {
        generation: outcome.generation,
        progress: outcome.progress,
    }))
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

/// Deserialize a request body, refusing a malformed one with a `no-store` 400.
async fn read_body<T: serde::de::DeserializeOwned>(input: InputStream) -> Result<T, OutputStream> {
    crud::read_json_body_or(input, |detail| {
        no_store_error(
            ErrorCode::InvalidArgument,
            &format!("invalid request body: {detail}"),
        )
    })
    .await
}

/// A diagnostic list or a `BlockInfo` that will not serialize is a bug here,
/// not a caller's mistake — both came out of typed values this process built.
fn encoding_error(e: serde_json::Error) -> WaferError {
    WaferError::new(ErrorCode::Internal, format!("could not encode JSON: {e}"))
}
