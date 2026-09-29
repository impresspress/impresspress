//! Reclaiming the blobs and artifacts nothing can reach any more.
//!
//! Both stores are content-addressed and nothing in them is ever edited in
//! place, so left alone they only grow: overwriting one page two hundred times
//! leaves two hundred blobs behind, and every compile of a block leaves
//! another artifact. Design §6.6 bounds a workspace at 64 MiB of *stored*
//! blobs, which is a bound only because this runs.
//!
//! # What is reachable
//!
//! A **blob** is reachable from a [retained][super::retention] generation's
//! site manifest, or from a workspace entry. The second half is not
//! redundant: a block's sources live in the workspace and in no generation at
//! all — a generation carries the compiled artifact, not the crate it came
//! from — so a collector that only read the ledger would delete a block's
//! source tree the moment it was written.
//!
//! An **artifact** is reachable from a retained generation's block manifest,
//! or from a build row that is still [`repo::builds::BuildStatus::Staged`] —
//! a compile that has stored its bytes and not yet reached a generation.
//! Staging writes the row before the bytes and leaves it staged until its
//! activation has minted a manifest; a site write's collection can run in that
//! window, and without the build rows it would delete the artifact the compile
//! is about to activate.
//!
//! A status, never an age. A browser compile takes tens of seconds and an
//! agent's site writes arrive in bursts, so any rule that expired the
//! protection by time would collect the artifact of a compile that was merely
//! slow.
//!
//! Everything else in the two folders goes. Unreachable is not "probably
//! unused": a blob no retained generation and no workspace path names cannot
//! be read back by any request the sandbox can serve, because every read
//! addresses content through one of those two.
//!
//! # The ordering invariant
//!
//! **List first, then read the roots.** The candidate set is fixed by the
//! folder listing, and every root is read after it, so anything stored *after*
//! the listing is not a candidate at all and needs no root to protect it. The
//! reverse order — roots, then listing — has a hole with no bottom: a compile
//! that inserts its build row after the roots are read and stores its bytes
//! before the listing produces an object that is a candidate and has no root,
//! and the collector deletes the artifact the compile is about to activate.
//!
//! That is why staging inserts its build row *before* it stores the artifact
//! (`super::blocks_api`). Together the two orderings close the interval: bytes
//! in the listing were stored before it, their row was written before them, so
//! the root read that follows the listing cannot miss it.
//!
//! The blob listing is taken under the same lock a file write takes, and the
//! lock is held until the blob half is done. A write stores its blob and saves
//! the entry naming it inside one lock hold, so the workspace read under that
//! lock names every blob the listing holds that a write still wants.
//!
//! # The counters are read off the listing
//!
//! [`workspace::Workspace::blob_bytes`] is what the 64 MiB quota bounds, and
//! the writers charge it as they store. A charge can be lost — a file write
//! whose blob stored and whose `workspace.json` save failed leaves a blob in
//! the store that no counter includes — and nothing on the write path can see
//! that happen. So every collection sets both counters to what the listing
//! says the store holds once the deletes are done, rather than subtracting
//! what it freed from whatever the counters said. A subtraction would turn
//! that lost charge into a permanent under-count: the collector would free
//! the uncharged blob and take its size off a total that never included it.
//! Read off the listing, a drift in either direction lasts until the next
//! collection, and the quota is exact again after it.
//!
//! That is the other reason the lock is taken *before* the blob listing: a
//! write that stored a blob after an unlocked listing would be charged in the
//! workspace and missing from the listing, and resetting the counters from it
//! would drop the charge.
//!
//! Each artifact is asked about once more, immediately before it goes, in case
//! a stage arrived in between ([`repo::builds::is_in_flight_for_artifact`]) —
//! cheap, because only a deletion pays for it.
//!
//! # When it runs
//!
//! At the end of every successful activation, after retention has pruned
//! (`super::activation`), and after a `blocks/` file delete, which changes what
//! the workspace names without publishing anything. Those are the two moments
//! content stops being reachable.

use std::collections::BTreeSet;

use wafer_core::clients::storage::{self, ListOptions, ObjectInfo};
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{
    artifacts, blobs, contracts::StorageUsage, generation, repo, retention, workspace, DevShared,
};

/// How many objects one storage listing asks for.
///
/// Large enough that a sandbox-sized store is one round trip, small enough
/// that a page is not an unbounded allocation.
const PAGE: i64 = 500;

/// What one collection reclaimed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    /// How many blobs were deleted.
    pub blobs_deleted: u32,
    /// How many artifacts were deleted.
    pub artifacts_deleted: u32,
    /// Total size of everything deleted, blobs and artifacts together.
    ///
    /// Not the workspace's quota figure: only the blob half counts against
    /// that ([`workspace::Workspace::blob_bytes`]), which is what design §6.6
    /// bounds. This is the storage figure.
    pub bytes_freed: u64,
    /// How many build rows were dropped because the artifact they name is no
    /// longer stored.
    pub build_rows_dropped: u32,
}

/// A seam the collector yields at, once, between its listing and its roots.
///
/// The module's whole soundness argument is an ordering one, and orderings are
/// exactly what a serial test cannot observe: nothing in the fixture's storage
/// or database yields, so no compile can interleave itself into the gap the
/// argument is about. This is the one place a test can put something there.
///
/// Production passes [`Uninterrupted`]. There is no `cfg(test)` on the trait
/// because a seam that only exists under `cfg(test)` is a seam whose shipped
/// build is a different function from the tested one.
#[wafer_block::wafer_async_trait]
pub trait GcInterleave: wafer_run::MaybeSend + wafer_run::MaybeSync {
    /// Called once, after both folders have been listed and before any root
    /// has been read.
    ///
    /// **Called with `DevShared::workspace` held** — the collector takes it
    /// before the blob listing (see "The counters are read off the listing").
    /// An implementation that writes a workspace file, or does anything else
    /// that takes that lock, deadlocks: it waits on the collector that is
    /// waiting on it.
    async fn after_listing(&self);
}

/// The [`GcInterleave`] production uses: nothing happens in the gap.
pub struct Uninterrupted;

#[wafer_block::wafer_async_trait]
impl GcInterleave for Uninterrupted {
    async fn after_listing(&self) {}
}

/// Delete every blob and artifact nothing retained can reach.
pub async fn collect(ctx: &dyn Context, shared: &DevShared) -> Result<GcReport, WaferError> {
    collect_interleaved(ctx, shared, &Uninterrupted).await
}

/// [`collect`], with the [`GcInterleave`] seam exposed.
///
/// Public for `tests/dev_gc.rs`; every production caller wants [`collect`].
pub async fn collect_interleaved(
    ctx: &dyn Context,
    shared: &DevShared,
    interleave: &dyn GcInterleave,
) -> Result<GcReport, WaferError> {
    // 0. The settled build rows, read before the artifact listing — see
    //    `stale_build_rows` for why that order makes the rows it drops safe
    //    to drop.
    let settled = repo::builds::list_settled(ctx).await?;

    // 1. The listings, first and before any root is read. They fix the
    //    candidate set: an object stored after this point is not in it.
    //
    //    The artifact folder is listed before the workspace lock is taken:
    //    nothing in `workspace.json` describes an artifact, and a listing is
    //    `O(folder)` on OPFS, so holding the lock across it would only make
    //    saves and the status poll wait longer.
    let artifact_objects = list_all(ctx, artifacts::FOLDER).await?;

    // The workspace lock, before the blob listing and held until the blob
    // half is done (see "The counters are read off the listing" above).
    //
    // Deadlock-free for the reason `activation::adopt_site` documents:
    // `files.rs` releases the lock before it asks for an activation, so
    // nothing holding it is ever waiting on the queue this runs under, and
    // nothing below takes another lock while holding it.
    let serialized = shared.workspace.lock().await;
    let blob_objects = list_all(ctx, blobs::FOLDER).await?;

    interleave.after_listing().await;

    // 2. The roots, all read after the listings.
    let retained = retention::retained(ctx).await?;
    let mut live_blobs = BTreeSet::new();
    let mut live_artifacts = BTreeSet::new();
    for row in &retained {
        // Through the manifest rather than the stored column text: the row's
        // two halves ARE the manifest (`generation::from_row` is exact), and
        // reading the shas off a parsed manifest is what keeps this and the
        // activation's own content check reading the same fields.
        let manifest = generation::from_row(row)?;
        live_blobs.extend(manifest.site.files.iter().map(|entry| entry.sha256.clone()));
        live_artifacts.extend(
            manifest
                .blocks
                .iter()
                .map(|spec| spec.artifact_sha256.clone()),
        );
    }
    // Plus every compile that has stored an artifact and not yet reached a
    // generation — a *status*, not an age: a browser compile takes tens of
    // seconds, and a rule that expired the protection by time would collect
    // the artifact of a compile that was merely slow.
    for build in repo::builds::list_in_flight(ctx).await? {
        live_artifacts.insert(build.artifact_sha256);
    }

    // 3. The deletes. The artifact half needs no workspace lock — nothing in
    //    `workspace.json` describes an artifact — so it is released first.
    let mut report = GcReport::default();
    collect_blobs(ctx, blob_objects, live_blobs, &mut report).await?;
    drop(serialized);
    let stale = stale_build_rows(settled, &artifact_objects);
    collect_artifacts(ctx, artifact_objects, &live_artifacts, &mut report).await?;
    for id in stale {
        repo::builds::delete(ctx, &id).await?;
        report.build_rows_dropped += 1;
    }
    Ok(report)
}

/// What the two stores, the workspace and the ledger hold.
///
/// Read from the counters and the ledger, never by walking the stores. The
/// `/b/dev` page polls status every ~300 ms while a tool call is outstanding,
/// and a storage `list` is `O(folder)` on the OPFS backend the sandbox runs
/// on — one full listing path is enough, and it belongs to [`collect`], which
/// runs once per activation rather than three times a second.
///
/// The two sources are the same bytes counted at the two ends that maintain
/// them: [`workspace::Workspace`]'s blob counters are charged by the file
/// writes that store blobs and reset from the store's own listing by every
/// [`collect`], and the builds table has a row per stored artifact because
/// staging writes the row before the bytes and [`collect`] deletes the row
/// with them.
///
/// The manifest read is under `DevShared::workspace`, like every other read of
/// it (`super::files`' header): this is the *poll* the page runs three times a
/// second while a tool call is outstanding, so it is the read most likely to
/// land inside [`collect_blobs`]'s delete-and-save — and a progress panel that
/// answers `500` while the collector works is the user-visible shape of that
/// race. Pacing behind the collector is what the panel wants anyway.
///
/// Deadlock-free on the same rule as the mutators: the lock is held around the
/// manifest load and nothing else, never across an activation.
pub async fn storage_usage(
    ctx: &dyn Context,
    shared: &DevShared,
) -> Result<StorageUsage, WaferError> {
    let ws = {
        let _serialized = shared.workspace.lock().await;
        workspace::load(ctx).await?
    };
    let artifacts = repo::builds::artifact_index(ctx).await?;
    Ok(StorageUsage {
        blobs: ws.blob_count,
        blobs_bytes: ws.blob_bytes,
        artifacts: artifacts.len() as u32,
        artifacts_bytes: artifacts.values().sum(),
        workspace_files: ws.files.len() as u32,
        retained_generations: retention::retained(ctx).await?.len() as u32,
    })
}

/// Delete the unreachable blobs, then set the workspace's blob counters to
/// what the store holds.
///
/// The caller holds `DevShared::workspace`, and took it before it listed
/// `candidates`. That is what makes both halves of this sound. The workspace
/// read here cannot miss an entry for a blob that is a candidate: a write
/// stores its blob and saves the entry naming it inside one lock hold, so
/// either it finished before the listing (a root) or it has not started. And
/// `candidates` minus what this deletes is exactly what the store holds, so it
/// is what the counters are set to — see the module docs for why they are
/// reset rather than decremented.
///
/// A delete that fails stops the deleting but not the reset: the objects not
/// yet deleted, the failed one included, are still in the store and are
/// counted as such, and the counters are saved before the failure is
/// returned. Returning first would discard the credit for every blob this
/// pass had already freed.
async fn collect_blobs(
    ctx: &dyn Context,
    candidates: Vec<ObjectInfo>,
    mut live: BTreeSet<String>,
    report: &mut GcReport,
) -> Result<(), WaferError> {
    let mut ws = workspace::load(ctx).await?;
    live.extend(ws.files.values().map(|entry| entry.sha256.clone()));

    let mut stored_bytes = 0u64;
    let mut stored_count = 0u32;
    let mut failure = None;
    for object in candidates {
        let size = size_of(&object);
        if failure.is_none() && !live.contains(&object.key) {
            match blobs::delete(ctx, &object.key).await {
                Ok(()) => {
                    report.blobs_deleted += 1;
                    report.bytes_freed += size;
                    continue;
                }
                Err(e) => failure = Some(e),
            }
        }
        stored_bytes = stored_bytes.saturating_add(size);
        stored_count = stored_count.saturating_add(1);
    }
    // Only when something changed: the collector runs after every activation,
    // and rewriting `workspace.json` each time to store the same bytes would
    // make every keystroke cost an extra object write.
    if ws.reset_blob_totals(stored_bytes, stored_count) {
        workspace::save(ctx, &ws).await?;
    }
    failure.map_or(Ok(()), Err)
}

/// The settled build rows whose artifact the store no longer holds.
///
/// The collector deletes an artifact and then the rows naming it; if the row
/// delete fails, the rows outlive their bytes. The artifact half of
/// [`collect_artifacts`] walks the folder listing and so never meets them
/// again, yet `dev_status` counts them in the artifact total and activation
/// answers "is this artifact stored?" from them
/// ([`repo::builds::artifact_index`]). They are found here instead, by the
/// other direction: a row whose artifact is not in the listing.
///
/// Only rows that were already settled — `valid` or `invalid`, never
/// `staged` — **when they were read, before the listing**. Every path that
/// settles a row does so after its artifact is stored (staging stores the
/// bytes before it validates them; the seed importer stores them before it
/// records the row), so a row settled before the listing had its bytes down
/// before the listing too, and missing from it means gone. A staged row makes
/// no such promise: its bytes may be stored a moment after the listing, which
/// is why staging's rows are roots rather than candidates here. And the rows
/// are dropped by id, so a compile that stages the same bytes again after the
/// read keeps its own row.
fn stale_build_rows(settled: Vec<repo::builds::SettledRow>, listed: &[ObjectInfo]) -> Vec<String> {
    let stored: BTreeSet<&str> = listed
        .iter()
        .filter_map(|object| artifacts::sha_of_key(&object.key))
        .collect();
    settled
        .into_iter()
        .filter(|row| !stored.contains(row.artifact_sha256.as_str()))
        .map(|row| row.id)
        .collect()
}

/// Delete the unreachable artifacts and the build rows that named them.
async fn collect_artifacts(
    ctx: &dyn Context,
    candidates: Vec<ObjectInfo>,
    live: &BTreeSet<String>,
    report: &mut GcReport,
) -> Result<(), WaferError> {
    for object in candidates {
        // A key this block did not write is left alone. Nothing else writes
        // the folder, so this arm is unreachable in practice — but deleting an
        // object whose hash cannot be read is deleting something the collector
        // cannot claim to have reasoned about.
        let Some(sha) = artifacts::sha_of_key(&object.key) else {
            continue;
        };
        if live.contains(sha) {
            continue;
        }
        // One last look, immediately before the object goes: the root set was
        // read a few awaits ago, and a stage that inserted its row after that
        // read would not be in it. Only a deletion pays for this query.
        if repo::builds::is_in_flight_for_artifact(ctx, sha).await? {
            continue;
        }
        artifacts::delete(ctx, sha).await?;
        // The rows go with the bytes, and in that order: a row claiming an
        // accepted artifact the store no longer holds is what
        // `repo::builds::latest_valid_for_artifact` would hand the
        // duplicate-tool check as a loadable block.
        repo::builds::delete_for_artifact(ctx, sha).await?;
        report.artifacts_deleted += 1;
        report.bytes_freed += size_of(&object);
    }
    Ok(())
}

/// Every object in `folder`, walked until the listing is exhausted.
///
/// The only full-listing path in the block, and it is reached only from
/// [`collect_interleaved`] — `dev_status` reports the stores from the counters
/// that track them, so nothing walks a folder on a poll.
///
/// Two paging modes, because the backends this runs on differ. A store that
/// answers a `next_cursor` is paged by that token, which is what keeps a deep
/// page from re-walking the prefix; one that does not — the sandbox's own
/// in-memory and filesystem backends — is paged by offset. A cursor-only walk
/// would stop after the first page on those and quietly under-collect, which
/// is the one failure mode a collector must not have.
///
/// A folder nothing has written yet is an empty listing. The backends
/// genuinely disagree about it (see [`blobs::exists`]) and an empty store has
/// nothing to collect either way.
async fn list_all(ctx: &dyn Context, folder: &str) -> Result<Vec<ObjectInfo>, WaferError> {
    let mut objects = Vec::new();
    let mut cursor = Some(String::new());
    let mut offset = 0i64;
    loop {
        let page = match storage::list(
            ctx,
            folder,
            &ListOptions {
                prefix: String::new(),
                limit: PAGE,
                offset,
                cursor: cursor.clone(),
            },
        )
        .await
        {
            Ok(page) => page,
            Err(e) if e.code == ErrorCode::NotFound => return Ok(objects),
            Err(e) => return Err(e),
        };
        let count = page.objects.len() as i64;
        objects.extend(page.objects);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            // No token, and the page was full: an offset-only backend, which
            // pages by hand from here.
            None if count == PAGE => {
                cursor = None;
                offset += count;
            }
            None => return Ok(objects),
        }
    }
}

/// One object's size, as a byte count.
///
/// `ObjectInfo::size` is signed because the wire type is; a negative size is
/// not a thing an object store can hold, and clamping beats a wrapping cast
/// that would count a sixteen-exabyte object into the workspace's quota.
fn size_of(object: &ObjectInfo) -> u64 {
    object.size.max(0) as u64
}
