//! The activation queue: one serialized path from "here is the state I want"
//! to "that state is live".
//!
//! # Why a queue at all
//!
//! Activation is not atomic. It journals, may rebuild the runtime, and writes
//! a set of files into a folder browsers are reading. Two of those running at
//! once would interleave their publishes and their journal writes, and the
//! result would be a site assembled from two generations. So exactly one runs
//! at a time, and the rest wait.
//!
//! Requests that arrive during an activation **coalesce** (design §7.3): the
//! queue keeps only the latest desired manifest, and every waiter resolves
//! with the generation that ends up carrying its change. An agent that writes
//! six files in a row therefore publishes twice — once for the file it started
//! with, once for the other five — rather than six times, and no caller is
//! told its write landed before it did.
//!
//! # Why it lives on `DevShared`
//!
//! The browser runtime is rebuilt (and the dev block re-instantiated) by the
//! very activations this queue orders. State held on the block would be
//! discarded halfway through the operation that discarded it, so the queue
//! lives on the `Arc<DevShared>` that outlives any one runtime.
//!
//! # The mutex
//!
//! `std::sync::Mutex`, and the guard is never held across an `await`: every
//! lock is taken inside a small synchronous method ([`ActivationQueue::admit`],
//! [`ActivationQueue::next`]) that returns owned data. That is what keeps the
//! returned futures `Send` on native, and what keeps a single-threaded browser
//! runtime from deadlocking on its own queue.

use std::{collections::BTreeSet, sync::Mutex};

use futures::channel::oneshot;
use serde::{Deserialize, Serialize};
use wafer_run::{context::Context, ErrorCode, OutputStream, WaferError};

use super::{
    blobs,
    contracts::{GenerationSummary, SiteManifest},
    control::DynamicBlockSpec,
    gc,
    generation::{self, GenerationManifest},
    no_store_error_status,
    publisher::publish_site,
    repo::{
        self,
        generations::{GenerationCause, GenerationRow, GenerationStatus, NewGeneration},
        runtime_state::{ActivationPhase, RuntimeState},
    },
    retention, workspace,
};
use crate::util::now_millis;

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

/// What one activation produced.
#[derive(Debug, Clone)]
pub struct ActivationOutcome {
    /// The generation that is now live.
    pub generation: GenerationSummary,
    /// One step per phase the activation passed through, in order.
    pub progress: Vec<ProgressStep>,
}

/// One phase of an activation, with how long it took.
///
/// Published in every mutating tool result (design §7.5) so the page can show
/// where the time went without a push channel.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressStep {
    /// The phase this step covers.
    pub phase: ActivationPhase,
    /// Milliseconds spent in it.
    pub ms: u64,
    /// Human-readable detail for the progress panel.
    pub detail: String,
}

/// Why an activation did not happen — or, for [`Self::RollbackNotAdopted`],
/// why one that did happen did not finish.
///
/// Four kinds, because they mean four different things to the caller: the
/// request described a state that cannot be built (fix the request), the host
/// could not build it (retry or roll back), persistence failed (retry), or a
/// rollback went live and left the workspace behind (retry the rollback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivationError {
    /// The manifest referenced content that is not stored. Reported as `422`:
    /// the request was well-formed and refused on its content.
    Validation(Vec<String>),
    /// [`super::control::RuntimeControl`] refused to build or swap the
    /// runtime. `500`.
    Runtime(String),
    /// The ledger, the journal or the object store failed. `500`.
    Storage(String),
    /// A rollback committed — the generation carrying the target's contents
    /// is live and journalled — and replacing the workspace's `site/` half
    /// with the target's then failed. `500`, because the request did not
    /// finish; but the message says the rollback is live, because telling
    /// the caller it failed would send them after a rollback that happened.
    RollbackNotAdopted {
        /// The generation that went live.
        generation_id: String,
        /// Why the workspace could not be updated.
        message: String,
    },
}

impl ActivationError {
    /// HTTP status this refusal is sent as.
    pub fn status(&self) -> u16 {
        match self {
            Self::Validation(_) => 422,
            Self::Runtime(_) | Self::Storage(_) | Self::RollbackNotAdopted { .. } => 500,
        }
    }

    /// The refusal as a `/b/dev` response.
    ///
    /// The single place `ActivationError` becomes HTTP, so every endpoint that
    /// activates answers the same status for the same failure. Built through
    /// [`super::no_store_error_status`] rather than [`crate::http::err_internal`]
    /// for two reasons: design §12 requires `Cache-Control: no-store` on every
    /// `/b/dev` response including the refusals, and the message *is* the
    /// product here — the sandbox surfaces build and validation diagnostics to
    /// its agent, and a sanitized "internal error" would delete the only thing
    /// the caller can act on. The route is admin-only at the router, so the
    /// detail never reaches an unauthenticated caller.
    pub fn into_response(self) -> OutputStream {
        let code = match self {
            Self::Validation(_) => ErrorCode::InvalidArgument,
            Self::Runtime(_) | Self::Storage(_) | Self::RollbackNotAdopted { .. } => {
                ErrorCode::Internal
            }
        };
        no_store_error_status(code, self.status(), &self.to_string())
    }
}

impl std::fmt::Display for ActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(reasons) => {
                write!(
                    f,
                    "the generation cannot be activated: {}",
                    reasons.join("; ")
                )
            }
            Self::Runtime(message) => write!(f, "the runtime could not be rebuilt: {message}"),
            Self::Storage(message) => write!(f, "the activation could not be stored: {message}"),
            // Says what IS true before what is not, and what to do about it.
            // The retry repairs because it is an ordinary rollback: staged
            // against a live generation with the same manifests,
            // `activate_staged` rebuilds nothing and publishes nothing, and
            // `adopt_site` then runs again.
            Self::RollbackNotAdopted {
                generation_id,
                message,
            } => write!(
                f,
                "the rollback is live — generation {generation_id} is serving the rolled-back \
                 site and blocks — but the workspace's site/ files could not be updated to \
                 match it: {message}. Until they are, the next site write publishes the \
                 pre-rollback site again. Retrying the same rollback repairs the workspace: it \
                 stages the same contents, which are already live, and updates the files."
            ),
        }
    }
}

/// Persistence failures all arrive the same way.
fn storage_error(e: WaferError) -> ActivationError {
    ActivationError::Storage(e.message)
}

// ---------------------------------------------------------------------------
// Intents
// ---------------------------------------------------------------------------

/// What a caller wants to be true, expressed so that it can be resolved
/// against state that is *current when it runs* rather than current when it
/// was asked for.
///
/// This is the whole reason activation takes an intent and not a finished
/// [`GenerationManifest`]. A manifest baked at request time freezes two things
/// that a queued request has no business freezing: the active block set (the
/// journal still names the previous generation while a block activation is in
/// flight, so a site write composed then would publish an empty block set and
/// tear the block back out at dequeue) and the workspace's `site/` half (a
/// caller that read the workspace early but reached the queue late would
/// displace content written after its own read). Both are read inside
/// [`activate`], after the journal, so the newest persisted state always wins.
pub enum ActivationIntent {
    /// Publish the workspace's `site/` half, keeping whatever block set is
    /// live. Never rebuilds the runtime (design §7.2) — the block half is
    /// copied from the generation that is active at dequeue, so
    /// `block_set_changed` is false by construction.
    SiteOnly,
    /// Publish `blocks` as the complete desired block set.
    ///
    /// `site` is `None` for "whatever the workspace holds", which is what a
    /// compile or a block removal wants; `Some` is for a caller that is
    /// publishing a site it did not take from the workspace.
    ///
    /// The block set is the caller's, resolved at request time, so two block
    /// activations in flight together would still let the later one's snapshot
    /// win. Design §6.6 allows one compile at a time and the compile lock is
    /// what upholds that; a future that lifts the lock should carry the block
    /// *delta* here rather than the whole set.
    BlockSet {
        /// The site to publish, or `None` for the workspace's own.
        site: Option<SiteManifest>,
        /// Every block the generation runs.
        blocks: Vec<DynamicBlockSpec>,
    },
    /// Republish an earlier generation's manifests under a new id.
    ///
    /// The target is carried rather than re-read because a ledger row is
    /// immutable: the manifest cannot have changed between the handler's
    /// lookup (which is what answers `404`) and the dequeue.
    Rollback {
        /// The generation whose contents are being republished.
        target: GenerationManifest,
    },
    /// Generation 0, imported from the seed bundle on cold boot.
    ///
    /// Carries a whole manifest because there is no workspace and no active
    /// generation to resolve anything against yet.
    Seed {
        /// The manifest the seed bundle describes.
        manifest: GenerationManifest,
    },
}

impl ActivationIntent {
    /// Whether this intent should replace `pending` in the queue's single
    /// slot.
    ///
    /// A site-only publish never displaces a richer pending intent. It has
    /// nothing of its own to lose: every intent publishes a site, and the ones
    /// that do not name one explicitly read the same persisted workspace at
    /// dequeue that this request has already written to. A rollback or a seed
    /// *does* name one explicitly, and replaces the workspace wholesale, so a
    /// site write racing with it was going to be overwritten either way.
    fn supersedes(&self, pending: &Self) -> bool {
        !matches!(self, Self::SiteOnly) || matches!(pending, Self::SiteOnly)
    }
}

/// Resolve `intent` into the manifest to stage, against the state that is
/// current now.
async fn compose(
    ctx: &dyn Context,
    shared: &super::DevShared,
    intent: ActivationIntent,
    previous: Option<&(GenerationRow, GenerationManifest)>,
) -> Result<GenerationManifest, ActivationError> {
    let active_blocks =
        || previous.map_or_else(Vec::new, |(_row, manifest)| manifest.blocks.clone());
    Ok(match intent {
        ActivationIntent::SiteOnly => {
            GenerationManifest::staged(workspace_site(ctx, shared).await?, active_blocks())
        }
        ActivationIntent::BlockSet { site, blocks } => {
            let site = match site {
                Some(site) => site,
                None => workspace_site(ctx, shared).await?,
            };
            GenerationManifest::staged(site, blocks)
        }
        ActivationIntent::Rollback { target } => {
            GenerationManifest::staged(target.site, target.blocks)
        }
        ActivationIntent::Seed { manifest } => manifest,
    })
}

/// The workspace's `site/` half as a site manifest, read from storage.
///
/// # Why this read is locked
///
/// This is the manifest read that backs **every ordinary edit**, not a rare
/// one: `files::handle_write` and `files::handle_delete` release
/// `DevShared::workspace` before they publish — deliberately, so editing never
/// waits behind a runtime rebuild — and this function then reads the manifest
/// back inside the activation that publish asked for. That released gap is
/// exactly where a second concurrent write takes the lock and saves the
/// manifest, and an unlocked read straddling a save is `super::files`' first
/// failure mode: `workspace::load` snapshots `workspace.json` and then reads
/// its bytes, a save between the two steps replaces the file, and the browser
/// storage layer reports the invalidated snapshot as an internal error rather
/// than a miss. The write whose content was already saved then answers a
/// sanitized `500`.
///
/// So it takes the same mutex as the mutators. The section is one small JSON
/// read; nothing else is inside it.
///
/// # Deadlock
///
/// Stated here rather than by analogy to the other holders, because this is
/// the only acquisition that happens *inside* the activation queue.
///
/// 1. **Nothing enters the queue holding this lock.** Every caller of
///    [`request`] — `files::publish_if_site`, `blocks_api`'s stage and remove,
///    `generations_api`'s rollback, and the browser host's seed import — holds
///    no `DevShared::workspace` guard when it calls. `files.rs` closes its
///    guard's scope before it publishes; the three block-set callers hold
///    `DevShared::compile` instead; the seed importer holds neither. So no
///    task can be waiting on this queue while holding the mutex this takes.
/// 2. **It does not nest with [`adopt_site`].** Both acquisitions live in
///    [`activate`], but they are sequential, not nested: this guard's scope
///    ends when this function returns, which is inside `compose`, and
///    `adopt_site` runs only after `activate_staged` has finished.
/// 3. **The one lock ordering in the block still has no reverse edge.**
///    `compile` is taken above this (`blocks_api`, `generations_api`) and this
///    lock is taken below it; nothing anywhere takes `compile` while holding
///    `workspace`, so `compile → workspace` is the only order that exists.
/// 4. **The guard is never held across an `await` on anything but storage.**
///    `workspace::load` is one object read; there is no activation request, no
///    runtime rebuild and no ledger write inside the section.
async fn workspace_site(
    ctx: &dyn Context,
    shared: &super::DevShared,
) -> Result<SiteManifest, ActivationError> {
    let ws = {
        let _serialized = shared.workspace.lock().await;
        workspace::load(ctx).await.map_err(storage_error)?
    };
    Ok(SiteManifest {
        files: workspace::site_manifest(&ws),
    })
}

// ---------------------------------------------------------------------------
// The queue
// ---------------------------------------------------------------------------

/// What a waiter is told when the activation it was folded into never
/// finished — the driving request was cancelled, or its task died.
const ABANDONED: &str = "activation abandoned before it completed";

/// The serialized activation queue. One per [`super::DevShared`].
#[derive(Default)]
pub struct ActivationQueue {
    inner: Mutex<QueueState>,
}

#[derive(Default)]
struct QueueState {
    /// Whether an activation is in flight.
    running: bool,
    /// The latest desired state, and everyone waiting for it.
    pending: Option<Pending>,
}

/// A coalesced request: one intent, and every caller that will resolve with
/// its outcome.
struct Pending {
    cause: GenerationCause,
    intent: ActivationIntent,
    waiters: Vec<oneshot::Sender<Result<ActivationOutcome, ActivationError>>>,
}

/// The right to drive the queue, released when it is dropped.
///
/// Liveness is the whole point. `running` was previously cleared only by the
/// driver reaching the end of its own drain loop, so a driver that was
/// cancelled — a dropped request future, a task that panicked — left the flag
/// set forever and every later `request` waited on a oneshot nobody would ever
/// send. `Drop` runs on those paths too, which is what makes the release
/// unconditional.
struct QueueLease<'q> {
    queue: &'q ActivationQueue,
    /// Set once the driver has drained the queue cleanly; `Drop` then has
    /// nothing to do.
    released: bool,
}

impl QueueLease<'_> {
    /// Take the next queued request, or release the queue when there is none.
    ///
    /// Releasing and taking are the same operation on purpose: a gap between
    /// "I found nothing pending" and "I marked myself not running" is a gap in
    /// which a new request would queue behind a driver that has already
    /// stopped draining.
    fn next(&mut self) -> Option<Pending> {
        let mut state = self.queue.inner.lock().expect("activation queue mutex");
        let pending = state.pending.take();
        if pending.is_none() {
            state.running = false;
            self.released = true;
        }
        pending
    }
}

impl Drop for QueueLease<'_> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let orphaned = {
            let mut state = self.queue.inner.lock().expect("activation queue mutex");
            state.running = false;
            state.pending.take()
        };
        // Fail the waiters explicitly rather than dropping their senders. A
        // dropped sender is indistinguishable from a bug at the receiving end;
        // an error says what happened, and says it identically on every
        // platform.
        if let Some(pending) = orphaned {
            tracing::warn!(
                waiters = pending.waiters.len(),
                "dev sandbox: an activation was abandoned; failing the requests folded into it",
            );
            for waiter in pending.waiters {
                let _ = waiter.send(Err(ActivationError::Runtime(ABANDONED.to_string())));
            }
        }
    }
}

/// What [`ActivationQueue::admit`] decided for a caller.
enum Admission<'q> {
    /// The caller runs the activation itself, and then drains whatever queued
    /// up behind it. Holds the lease for as long as it does.
    Drive(QueueLease<'q>, GenerationCause, ActivationIntent),
    /// Someone else is running; the caller waits for the coalesced outcome.
    Wait(oneshot::Receiver<Result<ActivationOutcome, ActivationError>>),
}

impl ActivationQueue {
    /// An idle queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many callers are folded into the pending slot right now.
    ///
    /// Test-only observation point: a test that wants to hold an activation
    /// open and prove that requests admitted *meanwhile* coalesce has to know
    /// when they have actually been admitted. Polling futures in a fixed order
    /// is not that — any `.await` on the write path that yields lets the
    /// driver run ahead — so the test waits on this count instead.
    #[cfg(any(test, feature = "test-support"))]
    pub fn pending_waiters(&self) -> usize {
        self.inner
            .lock()
            .expect("activation queue mutex")
            .pending
            .as_ref()
            .map_or(0, |pending| pending.waiters.len())
    }

    /// Admit one request: drive it, or fold it into the pending state.
    ///
    /// Folding keeps every waiter — a caller whose intent was superseded still
    /// resolves, with the generation that carries its change, because that
    /// generation is composed from the persisted state the caller has already
    /// written to.
    fn admit(&self, cause: GenerationCause, intent: ActivationIntent) -> Admission<'_> {
        let mut state = self.inner.lock().expect("activation queue mutex");
        if !state.running {
            state.running = true;
            return Admission::Drive(
                QueueLease {
                    queue: self,
                    released: false,
                },
                cause,
                intent,
            );
        }
        let (tx, rx) = oneshot::channel();
        match state.pending.as_mut() {
            Some(pending) => {
                if intent.supersedes(&pending.intent) {
                    pending.cause = cause;
                    pending.intent = intent;
                }
                pending.waiters.push(tx);
            }
            None => {
                state.pending = Some(Pending {
                    cause,
                    intent,
                    waiters: vec![tx],
                });
            }
        }
        Admission::Wait(rx)
    }
}

// ---------------------------------------------------------------------------
// Requesting an activation
// ---------------------------------------------------------------------------

/// Make `intent` true as a new generation, waiting for it (or for the
/// generation that supersedes it) to go live.
pub async fn request(
    ctx: &dyn Context,
    shared: &super::DevShared,
    cause: GenerationCause,
    intent: ActivationIntent,
) -> Result<ActivationOutcome, ActivationError> {
    match shared.activation.admit(cause, intent) {
        // A dropped sender means the driver went away without releasing the
        // lease — which [`QueueLease::drop`] makes impossible — so this arm is
        // the belt to that braces: a defined error either way, never a panic
        // and never a silent hang.
        Admission::Wait(rx) => rx
            .await
            .unwrap_or_else(|_| Err(ActivationError::Runtime(ABANDONED.to_string()))),
        Admission::Drive(mut lease, cause, intent) => {
            let mine = activate(ctx, shared, cause, intent).await;
            // Drain: whoever queued up while this ran gets their outcome from
            // here, because they have no task of their own to run it on.
            while let Some(pending) = lease.next() {
                let outcome = activate(ctx, shared, pending.cause, pending.intent).await;
                for waiter in pending.waiters {
                    // A caller that went away is not an error; the activation
                    // it asked for still happened.
                    let _ = waiter.send(outcome.clone());
                }
            }
            mine
        }
    }
}

/// Resolve `intent`, stage it as a new generation, and activate it.
///
/// Everything the manifest is made of is read *here* — the journal, the
/// generation it names, the workspace — so a request that waited in the queue
/// is composed against the state it will actually be applied to rather than
/// the state its caller saw.
async fn activate(
    ctx: &dyn Context,
    shared: &super::DevShared,
    cause: GenerationCause,
    intent: ActivationIntent,
) -> Result<ActivationOutcome, ActivationError> {
    // A rollback replaces the workspace's `site/` half as well as publishing
    // it, and the two have to happen under the same queue lease. The workspace
    // — not the ledger — is what the next site write composes its manifest
    // from, so a rollback that published an old site and left the workspace
    // holding the new one is undone by the very next keystroke. Doing it
    // outside the queue is not enough: a site write that dequeued in the gap
    // would republish the pre-rollback site from the workspace the rollback
    // had not rewritten yet, and the published folder and the workspace would
    // then disagree with nothing left to reconcile them.
    //
    // Read here rather than after `compose`, which consumes the intent.
    let adopts_site = matches!(intent, ActivationIntent::Rollback { .. });
    let state = repo::runtime_state::read(ctx)
        .await
        .map_err(storage_error)?;
    let previous = load_previous(ctx, &state).await?;
    let mut manifest = compose(ctx, shared, intent, previous.as_ref()).await?;

    // The id is minted here, not by the repo, because the manifest has to
    // carry it before it is hashed (design §11.3) — and the parent is
    // whatever is active *now*, which for a coalesced request is not what was
    // active when the caller made it.
    let id = repo::new_id();
    manifest.identify(id.clone(), state.active_generation_id.clone());

    let row = repo::generations::insert(
        ctx,
        &NewGeneration {
            id,
            parent_id: manifest.parent_id.clone(),
            cause,
            site_manifest_json: generation::canonical_text(&manifest.site)
                .map_err(storage_error)?,
            block_manifest_json: generation::canonical_text(&manifest.blocks)
                .map_err(storage_error)?,
            manifest_sha256: generation::manifest_sha256(&manifest).map_err(storage_error)?,
        },
    )
    .await
    .map_err(storage_error)?;

    let outcome = activate_staged(ctx, shared, &row, &manifest, previous.as_ref(), &state).await?;
    // Only on success: a rollback that never went live must not leave the
    // workspace pointing at content the published site does not have. The
    // reverse order would rewrite the workspace on every refused rollback —
    // including the ordinary case of a target whose blobs have been collected.
    //
    // Which means a failure here arrives after the commit, with the rollback
    // already serving — so it is reported as exactly that, never as a
    // rollback that did not happen.
    if adopts_site {
        adopt_site(ctx, shared, &manifest.site).await.map_err(|e| {
            ActivationError::RollbackNotAdopted {
                generation_id: row.id.clone(),
                message: e.message,
            }
        })?;
    }
    Ok(outcome)
}

/// Replace the workspace's `site/` half with `site`, leaving `blocks/` alone.
///
/// The blob counters are untouched on purpose: every entry here names content
/// the store already holds (a generation cannot reference a blob that was
/// never written), so nothing has been stored and nothing has been reclaimed.
/// Charging for it would make a rollback look like a fresh upload of the whole
/// site and eat the workspace's quota for content it already paid for.
async fn adopt_site(
    ctx: &dyn Context,
    shared: &super::DevShared,
    site: &SiteManifest,
) -> Result<(), WaferError> {
    // Under the same lock every file mutation takes: this is a
    // read-modify-write of the whole manifest, and a concurrent write that
    // loaded before it and saved after it would resurrect the site half this
    // is replacing.
    //
    // Deadlock-free because the lock is *only* ever held around a
    // read-modify-write of `workspace.json`: `files.rs` drops it before it
    // asks for an activation, so nothing holding it is ever waiting on this
    // queue. The other acquisition inside the queue — `workspace_site`, in
    // `compose` — is sequential with this one rather than nested: its guard's
    // scope ends before `compose` returns, and this runs after
    // `activate_staged`. `workspace_site` carries the full argument.
    let _serialized = shared.workspace.lock().await;
    let mut ws = workspace::load(ctx).await?;
    let stale: Vec<String> = ws
        .files
        .keys()
        .filter(|path| path.starts_with(workspace::SITE_PREFIX))
        .cloned()
        .collect();
    for path in stale {
        ws.remove(&path);
    }
    for entry in &site.files {
        // Through `Workspace::insert` — the only writer of `files` — so the
        // map key and `FileEntry::path` cannot drift apart, and the content
        // type is derived from the path exactly as a write would derive it.
        ws.insert(
            &format!("{}{}", workspace::SITE_PREFIX, entry.path),
            entry.sha256.clone(),
            entry.size,
        );
    }
    workspace::save(ctx, &ws).await
}

/// Drive an already-staged generation to live.
///
/// Split from [`activate`] because boot recovery converges on a row that
/// already exists: re-staging it would mint a second id for one desired state
/// and break the append-only history's parent chain.
async fn activate_staged(
    ctx: &dyn Context,
    shared: &super::DevShared,
    row: &GenerationRow,
    manifest: &GenerationManifest,
    previous: Option<&(GenerationRow, GenerationManifest)>,
    state: &RuntimeState,
) -> Result<ActivationOutcome, ActivationError> {
    let id = row.id.as_str();
    let previous_manifest = previous.map(|(_, manifest)| manifest);
    let mut progress = Progress::start();

    // --- Validate ---------------------------------------------------------
    journal(ctx, state, Some(id), ActivationPhase::Validating).await?;
    set_status(ctx, id, GenerationStatus::Validating, None, None).await?;
    let missing = missing_content(ctx, manifest).await?;
    if !missing.is_empty() {
        let error = ActivationError::Validation(missing);
        abandon(ctx, id, state, &error.to_string()).await?;
        return Err(error);
    }
    progress.record(
        ActivationPhase::Validating,
        format!(
            "{} site files, {} blocks",
            manifest.site.files.len(),
            manifest.blocks.len()
        ),
    );

    // --- Rebuild the runtime, if the block set changed ---------------------
    // `Activating` covers both remaining phases (design §7.2): from here on
    // the runtime swap and the site publish are what a reader of the ledger is
    // watching, and the journal's phase says which of the two is running.
    set_status(ctx, id, GenerationStatus::Activating, None, None).await?;
    let rebuilt = generation::block_set_changed(previous_manifest, manifest);
    if rebuilt {
        journal(ctx, state, Some(id), ActivationPhase::BuildingRuntime).await?;
        if let Err(message) = shared.control.rebuild(&manifest.blocks).await {
            let error = ActivationError::Runtime(message);
            abandon(ctx, id, state, &error.to_string()).await?;
            return Err(error);
        }
        progress.record(
            ActivationPhase::BuildingRuntime,
            format!("{} blocks", manifest.blocks.len()),
        );
    }

    // --- Publish the site -------------------------------------------------
    journal(ctx, state, Some(id), ActivationPhase::Publishing).await?;
    let diff = generation::diff(previous_manifest, manifest);
    if let Err(e) = publish_site(ctx, previous_manifest.map(|m| &m.site), &manifest.site).await {
        let error =
            ActivationError::Storage(restore(ctx, shared, manifest, previous, rebuilt, e).await);
        abandon(ctx, id, state, &error.to_string()).await?;
        return Err(error);
    }
    progress.record(
        ActivationPhase::Publishing,
        format!(
            "{} written, {} removed",
            diff.added_paths.len() + diff.changed_paths.len(),
            diff.removed_paths.len()
        ),
    );

    // --- Commit -----------------------------------------------------------
    let now = repo::now();
    set_status(ctx, id, GenerationStatus::Active, None, Some(&now)).await?;
    if let Some((previous_row, _)) = previous.filter(|(row, _)| row.id != id) {
        // Exactly one generation is `Active` at a time: the column is the
        // row's own lifecycle, and the row that was serving has stopped.
        //
        // The `id` guard is for boot convergence: a journal whose `desired`
        // and `active` name the same generation (a hand-edited row, or a
        // future writer) would otherwise have this supersede the generation it
        // has just activated, leaving nothing live.
        set_status(
            ctx,
            &previous_row.id,
            GenerationStatus::Superseded,
            None,
            None,
        )
        .await?;
    }
    repo::runtime_state::write(
        ctx,
        &RuntimeState {
            active_generation_id: Some(row.id.clone()),
            desired_generation_id: None,
            activation_phase: ActivationPhase::Idle,
            generation: state.generation.saturating_add(1),
        },
    )
    .await
    .map_err(storage_error)?;
    maintain(ctx, shared).await;
    progress.record(ActivationPhase::Active, format!("generation {}", row.id));

    // Re-read rather than patch the local row: the summary the caller is
    // handed must be what the ledger holds, not what this function believes
    // it wrote.
    let activated = repo::generations::get(ctx, &row.id)
        .await
        .map_err(storage_error)?;
    Ok(ActivationOutcome {
        generation: generation::summarize(&activated, manifest),
        progress: progress.steps,
    })
}

/// Retire the generations that have fallen out of the retention window, and
/// reclaim what that made unreachable.
///
/// Runs after the commit, and reports nothing back. That is the point: by this
/// line the generation is live and journalled, and returning a failure here
/// would tell the caller its write did not land when it did. What a failure
/// costs is storage the *next* activation collects instead — so it is logged
/// at `error!` rather than swallowed, and nothing depends on it having run.
///
/// Pruning first, collection second, and never the other way round: the
/// collector's reachability is read off the rows retention keeps, so a
/// collection that ran before the prune would still be protecting the
/// generations the prune is about to delete and would reclaim nothing.
///
/// Also the whole of what a `blocks/` delete does afterwards
/// ([`super::files`]'s `collect_if_unpublished`), which publishes nothing and
/// so never reaches this function through an activation. It is the same pair
/// of steps in the same order for the same reason — a collection there that
/// skipped the prune would be reading reachability off a window a *failed*
/// prune had left too wide, which is the one case this ordering exists for —
/// so it is one function, called from both, rather than a second collection
/// site that has to remember the rule.
pub(super) async fn maintain(ctx: &dyn Context, shared: &super::DevShared) {
    let pruned = match retention::prune(ctx).await {
        Ok(pruned) => pruned,
        Err(e) => {
            tracing::error!(
                error = %e.message,
                "dev sandbox: pruning the generation ledger failed; the window will be \
                 re-applied on the next activation",
            );
            return;
        }
    };

    match gc::collect(ctx, shared).await {
        Ok(report) => tracing::debug!(
            pruned = pruned.len(),
            blobs_deleted = report.blobs_deleted,
            artifacts_deleted = report.artifacts_deleted,
            bytes_freed = report.bytes_freed,
            "dev sandbox: retention and collection ran",
        ),
        Err(e) => tracing::error!(
            error = %e.message,
            pruned = pruned.len(),
            "dev sandbox: collecting unreachable content failed; it will be collected on the \
             next activation",
        ),
    }
}

/// Put the site back the way it was after a failed publish, and describe what
/// happened.
///
/// The rebuilt runtime is rolled back first (design §7.3: a failure after the
/// swap restores the previous `Rc`), then the previous site files are
/// republished with the half-published manifest as the "before" — that is what
/// removes files the failed publish had already added.
///
/// The runtime half is [`RuntimeControl::restore_previous`], not a second
/// `rebuild(previous_blocks)`: §7.3 says the *retained* runtime goes back, and
/// a rebuild is a different runtime that happens to carry the same block set.
/// It also re-seals and re-runs every built-in block's `Init` on a path that
/// is already failing, and can fail again on its own account.
async fn restore(
    ctx: &dyn Context,
    shared: &super::DevShared,
    attempted: &GenerationManifest,
    previous: Option<&(GenerationRow, GenerationManifest)>,
    rebuilt: bool,
    failure: WaferError,
) -> String {
    let mut message = format!("publishing the site failed: {}", failure.message);
    let previous_manifest = previous.map(|(_, manifest)| manifest);
    if rebuilt {
        if let Err(e) = shared.control.restore_previous().await {
            message.push_str(&format!(
                "; restoring the previous runtime also failed: {e}"
            ));
        }
    }
    let restored = match previous_manifest {
        Some(manifest) => publish_site(ctx, Some(&attempted.site), &manifest.site).await,
        None => publish_site(ctx, Some(&attempted.site), &Default::default()).await,
    };
    if let Err(e) = restored {
        message.push_str(&format!(
            "; restoring the previous site also failed: {}",
            e.message
        ));
    }
    message
}

/// Mark a generation `Failed` and put the journal back at rest with the
/// previous generation still live.
async fn abandon(
    ctx: &dyn Context,
    id: &str,
    state: &RuntimeState,
    message: &str,
) -> Result<(), ActivationError> {
    set_status(ctx, id, GenerationStatus::Failed, Some(message), None).await?;
    // `Idle`, not `Failed`: the journal answers "is a recovery owed?", and a
    // generation that has been abandoned owes none. Why it failed is on the
    // row, which keeps it.
    repo::runtime_state::write(
        ctx,
        &RuntimeState {
            active_generation_id: state.active_generation_id.clone(),
            desired_generation_id: None,
            activation_phase: ActivationPhase::Idle,
            generation: state.generation,
        },
    )
    .await
    .map_err(storage_error)
}

/// Journal an in-flight phase, leaving the active generation untouched.
async fn journal(
    ctx: &dyn Context,
    state: &RuntimeState,
    desired: Option<&str>,
    phase: ActivationPhase,
) -> Result<(), ActivationError> {
    repo::runtime_state::write(
        ctx,
        &RuntimeState {
            active_generation_id: state.active_generation_id.clone(),
            desired_generation_id: desired.map(str::to_string),
            activation_phase: phase,
            generation: state.generation,
        },
    )
    .await
    .map_err(storage_error)
}

async fn set_status(
    ctx: &dyn Context,
    id: &str,
    status: GenerationStatus,
    failure: Option<&str>,
    activated_at: Option<&str>,
) -> Result<(), ActivationError> {
    repo::generations::set_status(ctx, id, status, failure, activated_at)
        .await
        .map_err(storage_error)
}

/// The generation the journal says is live, with its manifest.
async fn load_previous(
    ctx: &dyn Context,
    state: &RuntimeState,
) -> Result<Option<(GenerationRow, GenerationManifest)>, ActivationError> {
    generation::active_from(ctx, state)
        .await
        .map_err(storage_error)
}

/// Content the manifest names that the stores do not hold — empty when
/// everything is there.
///
/// Only presence is checked, never content: both stores are content-addressed,
/// so the key a manifest names is the hash of the bytes filed under it, and
/// re-reading and re-hashing every blob would make each keystroke cost a full
/// pass over the site to learn something the key already states.
///
/// Presence is asked as cheaply as each store allows, because this runs on
/// every activation — every site-file save included — over the WHOLE manifest,
/// not just what changed:
///
/// * **Artifacts** are answered by the builds ledger
///   ([`repo::builds::artifact_index`]) with no storage call at all. Every
///   stored artifact has a row — staging inserts the row before it stores the
///   bytes, the seed importer records one for each artifact it stores, and the
///   collector deletes the rows with the bytes — so the index is the store's
///   own table of contents. One ledger read replaces a probe per block, and a
///   block's artifact is up to [`super::validation::MAX_ARTIFACT_BYTES`]. The
///   one way the two can disagree is a collection that deleted an artifact
///   and then failed to drop its rows. Until the next collection drops them
///   (`gc`'s stale-row pass), this answers "stored" for that artifact: a
///   manifest that changes the block set then fails at the rebuild, which
///   reads every artifact it loads, but one that keeps the block set — a site
///   write — commits naming it, as the generation before it already did.
/// * **Blobs** have no ledger, so each is probed with [`blobs::exists`], which
///   opens the object and declines its body rather than reading it.
///
/// The `Err` is a storage failure — the store could not answer — which is a
/// different thing from the store answering that content is gone.
async fn missing_content(
    ctx: &dyn Context,
    manifest: &GenerationManifest,
) -> Result<Vec<String>, ActivationError> {
    let mut missing = Vec::new();
    for sha in generation::site_blob_shas(manifest) {
        if !blobs::exists(ctx, sha).await.map_err(storage_error)? {
            missing.push(format!("no blob is stored for site content {sha}"));
        }
    }
    if !manifest.blocks.is_empty() {
        let stored = repo::builds::artifact_index(ctx)
            .await
            .map_err(storage_error)?;
        for spec in &manifest.blocks {
            if !stored.contains_key(&spec.artifact_sha256) {
                missing.push(format!(
                    "no artifact is stored for block {} ({})",
                    spec.name, spec.artifact_sha256
                ));
            }
        }
    }
    Ok(missing)
}

// ---------------------------------------------------------------------------
// Boot convergence
// ---------------------------------------------------------------------------

/// Converge on whatever the journal says was in flight, and report the block
/// set the host should build its runtime from.
///
/// A non-empty `desired_generation_id` on start is a recovery journal, not a
/// leftover (design §7.3): the process died somewhere between "I decided to
/// activate this" and "it is live". Converging re-runs the same steps on the
/// same staged row. If that fails, the previously active generation's site is
/// restored and the journal cleared, so the sandbox always boots serving
/// something coherent.
///
/// The return value is what the caller cannot get anywhere else: the block set
/// the active generation declares. The runtime is the host's to build (Task
/// 9), so this function decides *what* should be live and hands it over rather
/// than calling `rebuild` itself — at boot there is no previous runtime to
/// swap.
pub async fn converge_on_boot(
    ctx: &dyn Context,
    shared: &super::DevShared,
) -> Result<Vec<DynamicBlockSpec>, String> {
    let state = repo::runtime_state::read(ctx)
        .await
        .map_err(|e| e.message)?;
    let (previous, state) = active_or_clear(ctx, &state).await?;
    retire_abandoned(ctx, &state, previous.as_ref()).await;
    if let Some(desired) = state.desired_generation_id.clone() {
        match load_journalled(ctx, &desired)
            .await
            .map_err(|e| e.message)?
        {
            Journalled::Loaded(loaded) => {
                let (row, manifest) = *loaded;
                // A failed convergence is not a failed boot, and the failure
                // is not discarded: `activate_staged` has already written it
                // to the generation's `failure_message` and put the journal
                // back at rest, so it is readable at
                // `GET /b/dev/api/generations/{id}`. What it cannot do is
                // guarantee the published folder matches the generation that
                // is live again — the interrupted publish may have written
                // half of the desired one — so republish that from the
                // manifest authoritative for it, treating the abandoned
                // manifest as what is currently out there.
                if activate_staged(ctx, shared, &row, &manifest, previous.as_ref(), &state)
                    .await
                    .is_err()
                {
                    restore_active_site(ctx, Some(&manifest.site), previous.as_ref()).await?;
                }
            }
            // The journal names a generation that cannot be loaded: the row
            // is gone, or a column does not parse. Refusing the boot would be
            // the worst possible answer — the journal is persistent, so every
            // subsequent boot would fail identically and the instance would
            // never serve again over a row nothing can use. Treat it as a
            // convergence that failed: restore what is live, clear the
            // journal, and record the refusal on the row when there is one.
            // (A read that merely failed is not this: it returned above, and
            // the next boot converges on the row.)
            Journalled::Dangling(reason) => {
                tracing::error!(
                    generation_id = %desired,
                    error = %reason,
                    "dev sandbox: the activation journal names a generation that cannot be \
                     loaded; restoring the active generation",
                );
                abandon_dangling(ctx, &desired, &reason).await?;
                restore_active_site(ctx, None, previous.as_ref()).await?;
                clear_journal(ctx, &state).await?;
            }
        }
    }

    // Re-read: converging may have activated the desired generation, so the
    // journal this started from is not necessarily the one that is live now.
    // The same `active_or_clear` guard applies — a convergence that failed
    // could have been the thing that left the pointer unreadable.
    let state = repo::runtime_state::read(ctx)
        .await
        .map_err(|e| e.message)?;
    let (active, _) = active_or_clear(ctx, &state).await?;
    Ok(active
        .map(|(_, manifest)| manifest.blocks)
        .unwrap_or_default())
}

/// The message an abandoned row is closed with.
const ABANDONED_AT_BOOT: &str =
    "abandoned at boot: the process ended before this activation finished";

/// Close out the in-flight work the previous process did not finish.
///
/// A generation is `staged`/`validating`/`activating` because an activation is
/// *running*, and a build is `staged` because a compile is. Nothing is running
/// on a process that has just started, so every such row has to be settled —
/// except the generation the journal names, which is the activation this boot
/// is about to converge on and the only one that can still finish.
///
/// "Settled" is not the same as "abandoned". A build row whose artifact is
/// named by the generation that is serving, or by the one being converged on,
/// belongs to a compile that *arrived*: it is accepted rather than closed (see
/// [`repo::builds::retire_in_flight`]), because that row is where the block's
/// `BlockInfo` lives and the duplicate-agent-tool check reads it back.
///
/// Left alone they are not merely untidy. Retention keeps an in-flight
/// generation whatever its age, and the collector keeps a staged build's
/// artifact, so each abandoned row pins content against the workspace's 64 MiB
/// quota (design §6.6) for the life of the instance — a crash loop would fill
/// it with generations nothing can ever activate. An unbounded in-flight set
/// also eventually outgrows the page `retention::retained` reads it through.
///
/// Best effort, like every other step of boot recovery: an instance that
/// cannot tidy its ledger must still come up and serve.
async fn retire_abandoned(
    ctx: &dyn Context,
    state: &RuntimeState,
    previous: Option<&(GenerationRow, GenerationManifest)>,
) {
    let in_flight = match repo::generations::list_in_flight(ctx).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(
                error = %e.message,
                "dev sandbox: could not read the in-flight generations at boot",
            );
            Vec::new()
        }
    };
    for row in in_flight {
        // The journalled one is the activation being converged on, not
        // wreckage: `converge_on_boot` re-runs it a few lines below.
        if state.desired_generation_id.as_deref() == Some(row.id.as_str()) {
            continue;
        }
        if let Err(e) = repo::generations::set_status(
            ctx,
            &row.id,
            GenerationStatus::Failed,
            Some(ABANDONED_AT_BOOT),
            None,
        )
        .await
        {
            tracing::error!(
                generation_id = %row.id,
                error = %e.message,
                "dev sandbox: could not retire an abandoned generation at boot",
            );
        }
    }

    // Builds are settled the same way, against the manifests the journal
    // vouches for rather than against the journal itself — a build row names
    // an artifact, not a generation, so "is this compile still wanted?" is a
    // question only the manifests can answer.
    //
    // Both halves are needed. The active manifest covers a compile whose
    // activation committed before the process died (the row is one
    // `set_status` short of `valid`); the desired manifest covers one whose
    // activation is the very thing this boot is about to converge on. Closing
    // either would leave a block live with no accepted build row recording its
    // `BlockInfo`, and every later stage of *another* block refused for a
    // collision check that cannot be run.
    let mut vouched = BTreeSet::new();
    if let Some((_row, manifest)) = previous {
        vouched.extend(manifest.blocks.iter().map(|b| b.artifact_sha256.clone()));
    }
    if let Some(desired) = state.desired_generation_id.as_deref() {
        match load_journalled(ctx, desired).await {
            Ok(Journalled::Loaded(loaded)) => {
                let (_row, manifest) = &*loaded;
                vouched.extend(manifest.blocks.iter().map(|b| b.artifact_sha256.clone()));
            }
            // A dangling desired vouches for nothing; the dangling-desired
            // arm in `converge_on_boot` deals with the row itself.
            Ok(Journalled::Dangling(_)) => {}
            // A read that failed says nothing about what the desired
            // generation names, so no build can be judged unvouched: retiring
            // them now could close the very compile this boot converges on.
            // They stay in flight for the next boot to settle.
            Err(e) => {
                tracing::error!(
                    generation_id = %desired,
                    error = %e.message,
                    "dev sandbox: could not read the journalled generation; leaving the builds \
                     in flight for the next boot",
                );
                return;
            }
        }
    }

    let diagnostics = serde_json::to_string(&[super::validation::Diagnostic::error(
        super::validation::BUILD_ABANDONED,
        ABANDONED_AT_BOOT,
    )])
    .unwrap_or_else(|_| "[]".to_string());
    match repo::builds::retire_in_flight(ctx, &vouched, &diagnostics).await {
        Ok(settled) => tracing::debug!(
            promoted = settled.promoted,
            retired = settled.retired,
            "dev sandbox: settled the builds left in flight",
        ),
        Err(e) => tracing::error!(
            error = %e.message,
            "dev sandbox: could not settle the builds left in flight at boot",
        ),
    }
}

/// The generation the journal says is live, or `None` after clearing a pointer
/// that cannot be loaded.
///
/// The symmetric half of the dangling-`desired` recovery below, and it exists
/// for the same reason: the journal is persistent. A row that has been deleted,
/// or a `site_manifest_json` that does not parse, would otherwise make
/// [`converge_on_boot`] return `Err` on *every* boot from then on — the
/// instance would never serve again, and no `/b/dev` page would come up to fix
/// it, because the page is served by the runtime the failed boot never built.
///
/// So it is treated exactly like a dangling desired: log at `error!`, record
/// the failure on the row when there is one, clear the pointer, and boot with
/// no active generation. The site files already in `wafer-run/web/site` are
/// left alone — they are the last coherent publish, and nothing readable says
/// what should replace them. What the instance loses is the ledger's claim
/// that they belong to a generation, which is the claim that was corrupt.
///
/// **`desired` is kept.** Only the `active` half is cleared, because an
/// unreadable `active` says nothing about an interrupted activation that is
/// still perfectly loadable — and that activation is the best answer available
/// to "what should this instance serve?". [`converge_on_boot`] therefore still
/// converges on it, with `previous = None`, which republishes its site whole
/// and leaves the instance serving a generation the ledger can describe. A
/// `desired` that is *also* unloadable falls into the dangling-desired arm
/// below and is abandoned there.
///
/// A read of the row that merely failed is not a dangling pointer
/// ([`load_journalled`]): it is returned, and the pointer kept for the next
/// boot.
///
/// Returns the journal as it stands afterwards, so the caller reads the
/// cleared state rather than the one it passed in.
async fn active_or_clear(
    ctx: &dyn Context,
    state: &RuntimeState,
) -> Result<(Option<(GenerationRow, GenerationManifest)>, RuntimeState), String> {
    let Some(id) = state.active_generation_id.clone() else {
        return Ok((None, state.clone()));
    };
    match load_journalled(ctx, &id).await.map_err(|e| e.message)? {
        Journalled::Loaded(loaded) => Ok((Some(*loaded), state.clone())),
        Journalled::Dangling(reason) => {
            tracing::error!(
                generation_id = %id,
                error = %reason,
                "dev sandbox: the activation journal names an active generation that cannot be \
                 loaded; clearing it and booting with nothing dynamic",
            );
            abandon_dangling(ctx, &id, &reason).await?;
            let cleared = RuntimeState {
                active_generation_id: None,
                ..state.clone()
            };
            repo::runtime_state::write(ctx, &cleared)
                .await
                .map_err(|e| e.message)?;
            Ok((None, cleared))
        }
    }
}

/// Republish the active generation's site after a convergence that did not
/// happen.
///
/// `published` is what the interrupted run is believed to have left in the
/// folder, when that is known — passing it is what removes files the failed
/// publish had already added. It is `None` when the desired manifest could not
/// be read at all, and then the restore can only write the active generation's
/// own files back; anything the interrupted run added beyond them stays until
/// the next publish that knows about it. That is a stale extra object, not a
/// wrong `index.html`, because the entrypoint is always rewritten last.
async fn restore_active_site(
    ctx: &dyn Context,
    published: Option<&SiteManifest>,
    previous: Option<&(GenerationRow, GenerationManifest)>,
) -> Result<(), String> {
    let Some((_, manifest)) = previous else {
        return Ok(());
    };
    publish_site(ctx, published, &manifest.site)
        .await
        .map_err(|e| e.message)
}

/// A generation the journal names, as boot recovery reads it.
enum Journalled {
    /// The row and its manifest.
    Loaded(Box<(GenerationRow, GenerationManifest)>),
    /// The row is gone, or a manifest column does not parse — nothing a retry
    /// can change — with why.
    Dangling(String),
}

/// Load the generation `id` the journal names, telling a dangling pointer
/// ([`Journalled::Dangling`]) from a read that failed (`Err`).
///
/// The two must not be confused: a dangling pointer is cleared and its row
/// abandoned, which is right for a row nothing can ever use and destructive
/// for one a transient fault or a WRAP denial merely hid. So only a missing
/// row, or a row or manifest that does not decode, is dangling; every other
/// failure is returned for boot to give up on, leaving the journal as it
/// was for the next boot.
async fn load_journalled(ctx: &dyn Context, id: &str) -> Result<Journalled, WaferError> {
    let row = match repo::generations::lookup(ctx, id).await? {
        Some(Ok(row)) => row,
        Some(Err(e)) => return Ok(Journalled::Dangling(e.message)),
        None => {
            return Ok(Journalled::Dangling(format!(
                "generation {id} does not exist"
            )))
        }
    };
    Ok(match generation::from_row(&row) {
        Ok(manifest) => Journalled::Loaded(Box::new((row, manifest))),
        Err(e) => Journalled::Dangling(e.message),
    })
}

/// Record why a generation the journal pointed at could not be converged on.
///
/// Best effort by design: the row may not exist at all (that is one of the two
/// ways loading it fails), and a journal that cannot be cleaned up must not
/// stop the instance from booting.
async fn abandon_dangling(ctx: &dyn Context, id: &str, message: &str) -> Result<(), String> {
    match repo::generations::get(ctx, id).await {
        Ok(_) => {
            repo::generations::set_status(ctx, id, GenerationStatus::Failed, Some(message), None)
                .await
                .map_err(|e| e.message)
        }
        // Nothing to mark: the journal outlived the row.
        Err(_) => Ok(()),
    }
}

/// Put the journal back at rest, leaving the active generation alone.
async fn clear_journal(ctx: &dyn Context, state: &RuntimeState) -> Result<(), String> {
    repo::runtime_state::write(
        ctx,
        &RuntimeState {
            active_generation_id: state.active_generation_id.clone(),
            desired_generation_id: None,
            activation_phase: ActivationPhase::Idle,
            generation: state.generation,
        },
    )
    .await
    .map_err(|e| e.message)
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// Accumulates one [`ProgressStep`] per phase, each timed from the end of the
/// previous one.
struct Progress {
    since: u64,
    steps: Vec<ProgressStep>,
}

impl Progress {
    fn start() -> Self {
        Self {
            since: now_millis(),
            steps: Vec::new(),
        }
    }

    fn record(&mut self, phase: ActivationPhase, detail: String) {
        let now = now_millis();
        self.steps.push(ProgressStep {
            phase,
            // Saturating because `now_millis` is wall-clock: a clock that
            // steps backwards mid-activation must report 0 ms, not a phase
            // that took eighteen quintillion of them.
            ms: now.saturating_sub(self.since),
            detail,
        });
        self.since = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::dev::{
            test_support::FakeControl,
            workspace::{FileEntry, Workspace},
        },
        test_support::TestContext,
    };

    fn site_only() -> ActivationIntent {
        ActivationIntent::SiteOnly
    }

    fn entry(path: &str, sha: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            sha256: sha.to_string(),
            size: 4,
            content_type: "text/html; charset=utf-8".to_string(),
        }
    }

    /// Adopting a site manifest replaces the whole `site/` half and leaves
    /// `blocks/` alone — a rollback is a site+block republish, not a workspace
    /// wipe.
    #[tokio::test]
    async fn adopting_a_site_replaces_only_the_site_half() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let mut ws = Workspace::default();
        ws.insert("site/index.html", "new".to_string(), 4);
        ws.insert("site/added-later.css", "css".to_string(), 4);
        ws.insert("blocks/hello/src/lib.rs", "rs".to_string(), 4);
        ws.record_blob_stored(4);
        ws.record_blob_stored(4);
        ws.record_blob_stored(4);
        workspace::save(&ctx, &ws).await.expect("save");

        adopt_site(
            &ctx,
            &super::super::DevShared::new(
                FakeControl::new(),
                std::sync::Arc::new(super::super::test_support::FakeShell::new()),
            ),
            &SiteManifest {
                files: vec![entry("index.html", "old")],
            },
        )
        .await
        .expect("adopt");

        let after = workspace::load(&ctx).await.expect("load");
        assert_eq!(
            after.files.keys().collect::<Vec<_>>(),
            vec!["blocks/hello/src/lib.rs", "site/index.html"],
            "a path the target generation did not have must be dropped"
        );
        assert_eq!(after.get("site/index.html").expect("entry").sha256, "old");
        // Nothing was stored and nothing was reclaimed: the target's blobs
        // were already paid for.
        assert_eq!(after.blob_bytes, ws.blob_bytes);
        assert_eq!(after.blob_count, ws.blob_count);
        // And the projection round-trips: what was adopted is what a site
        // manifest reads back.
        assert_eq!(
            workspace::site_manifest(&after),
            vec![entry("index.html", "old")]
        );
    }

    fn block_set(marker: &str) -> ActivationIntent {
        ActivationIntent::BlockSet {
            site: None,
            blocks: vec![DynamicBlockSpec {
                name: format!("site/{marker}"),
                artifact_sha256: marker.to_string(),
                routes: Vec::new(),
                capabilities: wafer_block::BlockCapabilities::default(),
                wafer_guest_version: 1,
            }],
        }
    }

    fn block_marker(intent: &ActivationIntent) -> Option<&str> {
        match intent {
            ActivationIntent::BlockSet { blocks, .. } => {
                Some(blocks.first()?.artifact_sha256.as_str())
            }
            _ => None,
        }
    }

    /// The first caller drives; the queue is then busy.
    #[test]
    fn the_first_request_drives_and_the_rest_wait() {
        let queue = ActivationQueue::new();
        let admitted = queue.admit(GenerationCause::SiteWrite, site_only());
        assert!(matches!(
            admitted,
            Admission::Drive(_, GenerationCause::SiteWrite, ActivationIntent::SiteOnly)
        ));
        assert!(matches!(
            queue.admit(GenerationCause::SiteWrite, site_only()),
            Admission::Wait(_)
        ));
    }

    /// Coalescing: the newest intent wins, and every waiter is carried over to
    /// it — including the one whose own intent was displaced.
    #[test]
    fn queued_requests_coalesce_onto_the_newest_intent() {
        let queue = ActivationQueue::new();
        let mut driver = expect_drive(queue.admit(GenerationCause::SiteWrite, site_only()));
        let _first = queue.admit(GenerationCause::SiteWrite, site_only());
        let _second = queue.admit(GenerationCause::BlockCompile, block_set("bb"));

        let pending = driver.next().expect("something is pending");
        assert_eq!(block_marker(&pending.intent), Some("bb"));
        assert_eq!(pending.cause, GenerationCause::BlockCompile);
        assert_eq!(pending.waiters.len(), 2, "the displaced waiter is kept");
    }

    /// A site-only publish never displaces a pending block change. Both
    /// resolve the same persisted workspace at dequeue, so the site write is
    /// carried either way — but replacing the intent would drop the block set
    /// the compile staged.
    #[test]
    fn a_site_write_does_not_displace_a_pending_block_change() {
        let queue = ActivationQueue::new();
        let mut driver = expect_drive(queue.admit(GenerationCause::SiteWrite, site_only()));
        let _compile = queue.admit(GenerationCause::BlockCompile, block_set("bb"));
        let _write = queue.admit(GenerationCause::SiteWrite, site_only());

        let pending = driver.next().expect("something is pending");
        assert_eq!(block_marker(&pending.intent), Some("bb"));
        assert_eq!(pending.cause, GenerationCause::BlockCompile);
        assert_eq!(pending.waiters.len(), 2, "the site writer still resolves");
    }

    /// The reverse still replaces: a block change is not subsumed by anything.
    #[test]
    fn a_block_change_displaces_a_pending_site_write() {
        let queue = ActivationQueue::new();
        let mut driver = expect_drive(queue.admit(GenerationCause::SiteWrite, site_only()));
        let _write = queue.admit(GenerationCause::SiteWrite, site_only());
        let _compile = queue.admit(GenerationCause::BlockCompile, block_set("bb"));

        let pending = driver.next().expect("something is pending");
        assert_eq!(block_marker(&pending.intent), Some("bb"));
    }

    /// Draining an empty queue releases it, so the next request drives.
    #[test]
    fn the_queue_is_released_only_when_nothing_is_pending() {
        let queue = ActivationQueue::new();
        let mut driver = expect_drive(queue.admit(GenerationCause::SiteWrite, site_only()));
        let _waiter = queue.admit(GenerationCause::SiteWrite, site_only());

        assert!(driver.next().is_some());
        // Still running: the driver has not finished the batch it just took.
        assert!(matches!(
            queue.admit(GenerationCause::SiteWrite, site_only()),
            Admission::Wait(_)
        ));
        assert!(driver.next().is_some());
        assert!(driver.next().is_none());
        drop(driver);
        assert!(matches!(
            queue.admit(GenerationCause::SiteWrite, site_only()),
            Admission::Drive(..)
        ));
    }

    /// A driver that goes away without draining — a cancelled request future,
    /// a task that died — must release the queue and tell its waiters, not
    /// leave every later request hanging on a oneshot nobody will send.
    #[test]
    fn a_dropped_lease_releases_the_queue_and_fails_its_waiters() {
        let queue = ActivationQueue::new();
        let driver = expect_drive(queue.admit(GenerationCause::SiteWrite, site_only()));
        let Admission::Wait(mut waiter) = queue.admit(GenerationCause::SiteWrite, site_only())
        else {
            panic!("the second request must wait");
        };

        drop(driver);

        match waiter.try_recv() {
            Ok(Some(Err(ActivationError::Runtime(message)))) => {
                assert!(message.contains("abandoned"), "{message}");
            }
            other => panic!("the orphaned waiter must be failed, not dropped: {other:?}"),
        }
        assert!(
            matches!(
                queue.admit(GenerationCause::SiteWrite, site_only()),
                Admission::Drive(..)
            ),
            "the queue must be free again"
        );
    }

    fn expect_drive(admission: Admission<'_>) -> QueueLease<'_> {
        match admission {
            Admission::Drive(lease, _, _) => lease,
            Admission::Wait(_) => panic!("expected to drive"),
        }
    }

    #[test]
    fn refusals_carry_the_status_the_caller_should_see() {
        assert_eq!(
            ActivationError::Validation(vec!["no blob".to_string()]).status(),
            422
        );
        assert_eq!(ActivationError::Runtime("boom".to_string()).status(), 500);
        assert_eq!(ActivationError::Storage("db".to_string()).status(), 500);
        // The message is the product: it must name what went wrong.
        assert!(ActivationError::Runtime("wasmi: boom".to_string())
            .to_string()
            .contains("wasmi: boom"));
        assert!(
            ActivationError::Validation(vec!["a".to_string(), "b".to_string()])
                .to_string()
                .contains("a; b")
        );
    }

    #[test]
    fn progress_times_each_phase_and_ends_at_active() {
        let mut progress = Progress::start();
        progress.record(ActivationPhase::Validating, "1 file".to_string());
        progress.record(ActivationPhase::Publishing, "1 written".to_string());
        progress.record(ActivationPhase::Active, "generation g1".to_string());
        let phases: Vec<ActivationPhase> = progress.steps.iter().map(|s| s.phase).collect();
        assert_eq!(
            phases,
            vec![
                ActivationPhase::Validating,
                ActivationPhase::Publishing,
                ActivationPhase::Active
            ]
        );
        assert_eq!(progress.steps[0].detail, "1 file");
    }
}
