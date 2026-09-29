//! Retention, garbage collection and the storage figures `dev_status` reports.
//!
//! Gated on `block-dev` for the same reason `dev_activation.rs` is: the block
//! does not exist in a default-feature build, so these tests must not compile
//! there.
//!
//! Everything here is driven through the HTTP surface an agent uses, because
//! the property under test is a whole-loop one: a write publishes a
//! generation, the generation falls out of the retention window, the row goes,
//! and only then can the content it named go. A test that called the collector
//! directly would prove the collector's arithmetic and nothing about the loop.
#![cfg(feature = "block-dev")]

use base64ct::{Base64, Encoding as _};
use impresspress_core::{
    blocks::dev::{
        activation::{self, ActivationIntent},
        artifacts, blobs,
        contracts::SiteManifest,
        control::{DynamicBlockSpec, DynamicRoute, RouteAccessKind},
        gc::{self, GcInterleave},
        generation::{self, GenerationManifest},
        paths,
        repo::{
            self,
            builds::{BuildStatus, NewBuild},
            generations::{self, GenerationCause, GenerationStatus, NewGeneration},
            runtime_state::{self, ActivationPhase, RuntimeState},
        },
        retention,
        test_support::{dev_post, FakeControl},
        workspace::{self, FileEntry},
        WAFER_GUEST_VERSION,
    },
    test_support::{admin_msg, output_http_status, output_json, FailingDbOpContext, TestContext},
};
use serde_json::json;
use wafer_core::clients::storage;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `content` at `path`, expecting the file to hold `expected`, and
/// return the sha the write reports.
async fn write_file(
    ctx: &TestContext,
    path: &str,
    content: &str,
    expected: Option<&str>,
) -> String {
    let out = output_json(
        dev_post(
            ctx,
            "/b/dev/api/files/write",
            json!({"path": path, "content": content, "expected_sha256": expected}),
        )
        .await,
    )
    .await;
    out["sha256"]
        .as_str()
        .unwrap_or_else(|| panic!("no sha256 in {out}"))
        .to_string()
}

/// The `storage` half of `GET /b/dev/api/status`.
async fn storage_of(ctx: &TestContext) -> serde_json::Value {
    let status = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/status"))
            .await,
    )
    .await;
    status["storage"].clone()
}

/// One block named `name`, with its own artifact stored and the accepted
/// build row a real compile would have left behind.
async fn block_spec(ctx: &TestContext, name: &str) -> DynamicBlockSpec {
    let bytes = format!("\0asm\x01{name}").into_bytes();
    let artifact_sha256 = artifacts::put(ctx, &bytes)
        .await
        .expect("store the artifact a manifest names");
    let spec = DynamicBlockSpec {
        name: format!("site/{name}"),
        artifact_sha256: artifact_sha256.clone(),
        routes: vec![DynamicRoute {
            prefix: format!("/b/{name}/"),
            access: RouteAccessKind::Public,
        }],
        capabilities: wafer_block::BlockCapabilities::default(),
        wafer_guest_version: WAFER_GUEST_VERSION,
    };
    accept_build(ctx, &spec, bytes.len() as u64).await;
    spec
}

/// The `valid` build row staging leaves behind once its activation has landed.
async fn accept_build(ctx: &TestContext, spec: &DynamicBlockSpec, artifact_bytes: u64) -> String {
    let row = repo::builds::insert(
        ctx,
        &NewBuild {
            block_name: spec.name.clone(),
            source_manifest_sha256: "src".to_string(),
            artifact_sha256: spec.artifact_sha256.clone(),
            block_info_json: "null".to_string(),
            diagnostics_json: "[]".to_string(),
            compiler_version: "rubrc@pinned".to_string(),
            artifact_bytes,
        },
    )
    .await
    .expect("insert build");
    repo::builds::set_status(ctx, &row.id, BuildStatus::Valid, None, None)
        .await
        .expect("accept build");
    row.id
}

/// The `staged` build row a compile holds from before its bytes are stored
/// until its activation has minted a generation.
async fn stage_build(ctx: &TestContext, artifact_sha256: &str, artifact_bytes: u64) -> String {
    repo::builds::insert(
        ctx,
        &NewBuild {
            block_name: "site/pending".to_string(),
            source_manifest_sha256: "src".to_string(),
            artifact_sha256: artifact_sha256.to_string(),
            block_info_json: "null".to_string(),
            diagnostics_json: "[]".to_string(),
            compiler_version: "rubrc@pinned".to_string(),
            artifact_bytes,
        },
    )
    .await
    .expect("insert build")
    .id
}

/// A generation carrying `site`, staged and never activated — what a crash
/// between the ledger insert and the runtime swap leaves behind.
async fn stage_generation(ctx: &TestContext, site: SiteManifest) -> String {
    stage_generation_of(ctx, site, Vec::new()).await
}

/// [`stage_generation`] with a block set.
async fn stage_generation_of(
    ctx: &TestContext,
    site: SiteManifest,
    blocks: Vec<DynamicBlockSpec>,
) -> String {
    let id = repo::new_id();
    let mut manifest = GenerationManifest::staged(site, blocks);
    manifest.identify(id.clone(), None);
    generations::insert(
        ctx,
        &NewGeneration {
            id: id.clone(),
            parent_id: None,
            cause: GenerationCause::SiteWrite,
            site_manifest_json: generation::canonical_text(&manifest.site).expect("canonical"),
            block_manifest_json: generation::canonical_text(&manifest.blocks).expect("canonical"),
            manifest_sha256: generation::manifest_sha256(&manifest).expect("hash"),
        },
    )
    .await
    .expect("stage");
    id
}

/// A block spec whose artifact is stored but which has no build row at all —
/// the shape a caller wants when it is about to write the row itself.
async fn spec_only(ctx: &TestContext, name: &str) -> (DynamicBlockSpec, Vec<u8>) {
    let bytes = format!("\0asm\x01{name}").into_bytes();
    let artifact_sha256 = artifacts::put(ctx, &bytes).await.expect("store");
    (
        DynamicBlockSpec {
            name: format!("site/{name}"),
            artifact_sha256,
            routes: vec![DynamicRoute {
                prefix: format!("/b/{name}/"),
                access: RouteAccessKind::Public,
            }],
            capabilities: wafer_block::BlockCapabilities::default(),
            wafer_guest_version: WAFER_GUEST_VERSION,
        },
        bytes,
    )
}

/// A site manifest naming one file whose blob is stored but which no
/// workspace path holds.
async fn site_only_blob(ctx: &TestContext, content: &str) -> (SiteManifest, String) {
    let (sha256, _stored) = blobs::put(ctx, content.as_bytes())
        .await
        .expect("store the blob a manifest names");
    (
        SiteManifest {
            files: vec![FileEntry {
                path: "index.html".to_string(),
                sha256: sha256.clone(),
                size: content.len() as u64,
                content_type: "text/html; charset=utf-8".to_string(),
            }],
        },
        sha256,
    )
}

// ---------------------------------------------------------------------------
// Blobs
// ---------------------------------------------------------------------------

/// The whole loop: 25 versions of one page, 25 generations, 20 retained — and
/// the five blobs only the deleted generations named are gone while the live
/// one is not.
#[tokio::test]
async fn gc_deletes_blobs_no_retained_generation_or_workspace_references() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let mut sha = None;
    let mut first_blob = None;
    for i in 0..25 {
        let written = write_file(&ctx, "site/index.html", &format!("v{i}"), sha.as_deref()).await;
        first_blob.get_or_insert(written.clone());
        sha = Some(written);
    }

    let first_blob = first_blob.expect("25 writes stored a first blob");
    let last_blob = sha.expect("25 writes stored a last blob");
    assert!(
        !blobs::exists(&ctx, &first_blob).await.expect("exists"),
        "v0 is named only by generations retention has deleted",
    );
    assert!(
        blobs::exists(&ctx, &last_blob).await.expect("exists"),
        "v24 is what the workspace and the active generation both name",
    );

    let storage = storage_of(&ctx).await;
    assert_eq!(storage["retained_generations"], 20);
    assert_eq!(
        storage["blobs"], 20,
        "one blob per retained generation, and the newest is the workspace's: {storage}",
    );
    assert_eq!(storage["workspace_files"], 1);
    // `v5` … `v9` are two bytes, `v10` … `v24` are three.
    assert_eq!(storage["blobs_bytes"], 5 * 2 + 15 * 3);

    // The workspace's own accounting was credited, not just the store: the
    // 64 MiB quota is read off these counters, so a collector that freed
    // bytes without crediting them would shrink the store and leave the
    // workspace believing it was still full.
    let ws = workspace::load(&ctx).await.expect("load workspace");
    assert_eq!(ws.blob_count, 20, "25 stored, 5 reclaimed");
    assert_eq!(ws.blob_bytes, 5 * 2 + 15 * 3);

    // `dev_status` reports those counters rather than walking the store, so
    // something has to check the two against each other: a counter that had
    // drifted would look exactly like a collector that was keeping up.
    let stored = storage::list(&ctx, blobs::FOLDER, &storage::ListOptions::default())
        .await
        .expect("list the blob store");
    assert_eq!(stored.objects.len(), 20, "the counters describe the store");
    assert_eq!(
        stored.objects.iter().map(|o| o.size).sum::<i64>(),
        (5 * 2 + 15 * 3) as i64,
    );
}

/// A block's sources live in the workspace and in no generation at all — a
/// generation carries the compiled artifact, not the crate it came from. A
/// collector that only read the ledger would delete a block's source tree the
/// moment it was written.
#[tokio::test]
async fn gc_never_deletes_a_blob_the_workspace_still_names_even_if_no_generation_does() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let src_sha = write_file(&ctx, "blocks/hello/src/lib.rs", "// src", None).await;

    // Enough site writes to push the window past everything, so the source
    // blob's survival cannot be an accident of nothing having been collected.
    let mut sha = None;
    for i in 0..24 {
        sha = Some(write_file(&ctx, "site/page.html", &format!("v{i}"), sha.as_deref()).await);
    }

    assert!(
        blobs::exists(&ctx, &src_sha).await.expect("exists"),
        "the workspace still names it, so it is still reachable",
    );
    let storage = storage_of(&ctx).await;
    assert_eq!(storage["workspace_files"], 2);
    assert_eq!(
        storage["blobs"], 21,
        "twenty retained site versions plus the block source: {storage}",
    );

    // And it goes the moment nothing names it — on the delete itself, not on
    // some later unrelated site write. A `blocks/` delete publishes nothing
    // (design §7.2), so without the collector running here the blob would stay
    // charged against the workspace's quota until the agent happened to edit
    // the site.
    let deleted = dev_post(
        &ctx,
        "/b/dev/api/files/delete",
        json!({"path": "blocks/hello/src/lib.rs", "expected_sha256": src_sha}),
    )
    .await;
    output_json(deleted).await;
    assert!(
        !blobs::exists(&ctx, &src_sha).await.expect("exists"),
        "the delete that orphaned it is what reclaims it",
    );
    let ws = workspace::load(&ctx).await.expect("load workspace");
    assert!(
        !ws.files.contains_key("blocks/hello/src/lib.rs"),
        "and the entry is gone with it",
    );
    assert_eq!(
        storage_of(&ctx).await["blobs"],
        20,
        "the quota accounting was credited, not just the store",
    );
    let _ = sha;
}

/// The figures move as content is reclaimed — that is what makes them worth
/// polling. Asserted against a fresh instance and against one whose window has
/// started deleting, so a hard-coded zero could not pass either.
#[tokio::test]
async fn dev_status_reports_the_stores_as_the_collector_shrinks_them() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;

    let fresh = storage_of(&ctx).await;
    assert_eq!(fresh["blobs"], 0);
    assert_eq!(fresh["blobs_bytes"], 0);
    assert_eq!(fresh["artifacts"], 0);
    assert_eq!(fresh["workspace_files"], 0);
    assert_eq!(fresh["retained_generations"], 0);

    let mut sha = None;
    for i in 0..5 {
        sha = Some(write_file(&ctx, "site/index.html", &format!("v{i}"), sha.as_deref()).await);
    }
    let inside = storage_of(&ctx).await;
    assert_eq!(inside["blobs"], 5, "nothing has fallen out of the window");
    assert_eq!(inside["blobs_bytes"], 5 * 2);
    assert_eq!(inside["retained_generations"], 5);

    // Twenty more, so fifteen of the first twenty-five generations go.
    for i in 5..25 {
        sha = Some(write_file(&ctx, "site/index.html", &format!("v{i}"), sha.as_deref()).await);
    }
    let collected = storage_of(&ctx).await;
    assert_eq!(collected["blobs"], 20);
    assert_eq!(collected["retained_generations"], 20);
    assert!(
        collected["blobs_bytes"].as_u64().expect("bytes")
            > inside["blobs_bytes"].as_u64().expect("bytes"),
        "twenty three-byte blobs outweigh five two-byte ones: {collected}",
    );
}

// ---------------------------------------------------------------------------
// The quota counters
// ---------------------------------------------------------------------------

/// What the blob store really holds, read off the store itself rather than off
/// the counters under test.
async fn stored_blobs(ctx: &TestContext) -> (u64, u32) {
    let listing = storage::list(ctx, blobs::FOLDER, &storage::ListOptions::default())
        .await
        .expect("list the blob store");
    (
        listing.objects.iter().map(|o| o.size as u64).sum(),
        listing.objects.len() as u32,
    )
}

/// A write whose blob stores and whose manifest save fails leaves a blob no
/// counter includes. The collector then frees it — it is unreachable — and the
/// counters must come out saying what the store holds. Subtracting the freed
/// size instead would take it off a total that never included it, and the
/// quota would under-count for good: the refusal at the end is a write the
/// workspace genuinely has no room for.
///
/// Sized so the difference is the verdict. The store sits 250 KiB under the
/// limit; the lost 250 KiB charge, subtracted, would leave it looking 500 KiB
/// under, and the final 400 KiB write would be let through.
#[tokio::test]
async fn a_blob_whose_charge_was_lost_leaves_the_quota_exact_after_collection() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();
    const KIB: usize = 1024;

    // Fixture: a workspace whose store is 250 KiB short of full, all of it
    // one reachable blob. Staged directly in the object store — reaching it
    // through the files API would be 128 writes of the largest file the API
    // takes, and even one 64 MiB trip through the storage block's message
    // codec costs seconds in a debug build — under a stand-in key rather than
    // its hash, which nothing here reads back.
    let headroom = 250 * KIB as u64;
    let big = vec![b'b'; (paths::MAX_WORKSPACE_BYTES - headroom) as usize];
    let big_key = "0".repeat(64);
    ctx.storage_service()
        .put(
            &format!("impresspress/dev/{}", blobs::FOLDER),
            &big_key,
            &big,
            "application/octet-stream",
        )
        .await
        .expect("store the big blob");
    let mut ws = workspace::Workspace::default();
    ws.insert("blocks/shop/data.bin", big_key, big.len() as u64);
    ws.record_blob_stored(big.len() as u64);
    workspace::save(&ctx, &ws)
        .await
        .expect("stage the workspace");

    // A 250 KiB write that exactly fits, and whose manifest save fails after its blob
    // is down. A `blocks/` path, so no activation (and no collection) runs
    // behind it.
    ctx.fail_next_storage_put_to("impresspress/dev", "", "workspace.json", "disk is full");
    let lost = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({
            "path": "blocks/shop/lost.txt",
            "content": "l".repeat(250 * KIB),
            "expected_sha256": null,
        }),
    )
    .await;
    assert_eq!(output_http_status(lost).await, 500);
    let (stored_bytes, _) = stored_blobs(&ctx).await;
    assert_eq!(
        stored_bytes,
        paths::MAX_WORKSPACE_BYTES,
        "the blob is in the store…",
    );
    assert_eq!(
        workspace::load(&ctx).await.expect("load").blob_bytes,
        paths::MAX_WORKSPACE_BYTES - headroom,
        "…and no counter includes it",
    );

    let report = gc::collect(&ctx, &shared).await.expect("collect");
    assert_eq!(report.blobs_deleted, 1, "the unreachable blob went");

    let after = workspace::load(&ctx).await.expect("load");
    assert_eq!(
        (after.blob_bytes, after.blob_count),
        stored_blobs(&ctx).await,
        "the counters say what the store holds",
    );

    // 400 KiB more would take the store 150 KiB past the limit.
    let over = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({
            "path": "blocks/shop/over.txt",
            "content": "o".repeat(400 * KIB),
            "expected_sha256": null,
        }),
    )
    .await;
    assert_eq!(output_http_status(over).await, 413);
}

/// The collector takes the workspace lock BEFORE it lists the blob store, and
/// the counters depend on it. A write that stored its blob and saved its charge
/// after an unlocked listing would be charged in the workspace and absent from
/// the listing, and the reset would drop the charge.
///
/// Forced, not hoped for: the write is parked inside its own lock hold (on its
/// `workspace.json` read), the collection is started while it is parked, and
/// the write is released only once the collection has had every chance to run
/// as far as it can. Locked first, the collection waits for the write and
/// lists its blob; listing first, it lists without it, waits, and resets the
/// counters from a listing that is missing the write's blob.
#[tokio::test]
async fn a_write_landing_as_a_collection_starts_keeps_its_charge() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();

    let hold = ctx.hold_next_storage_get("impresspress/dev", "", "workspace.json");
    let write = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "blocks/shop/late.txt", "content": "late", "expected_sha256": null}),
    );
    let collection = async {
        while !hold.was_reached() {
            tokio::task::yield_now().await;
        }
        gc::collect(&ctx, &shared).await
    };
    let driver = async {
        while !hold.was_reached() {
            tokio::task::yield_now().await;
        }
        // Far fewer than the park's budget, far more than the collection
        // needs to reach the lock (or, listing first, to list and then reach
        // it).
        for _ in 0..1_000 {
            tokio::task::yield_now().await;
        }
        hold.release();
    };
    let (written, collected, ()) = tokio::join!(write, collection, driver);
    assert!(hold.was_reached());
    assert!(
        !hold.budget_expired(),
        "the write was released, not timed out"
    );
    assert_eq!(output_http_status(written).await, 200);
    collected.expect("collect");

    let after = workspace::load(&ctx).await.expect("load");
    assert_eq!(
        (after.blob_bytes, after.blob_count),
        stored_blobs(&ctx).await,
        "the write's charge survived the collection",
    );
    assert_eq!(after.blob_count, 1);
}

/// A build row can outlive its artifact — the collector deletes the bytes and
/// then the rows, and the second step can fail. The folder walk never meets
/// such a row again, yet `dev_status` counts it and activation treats its
/// artifact as stored. The next collection drops it; a staged row, whose
/// bytes may simply not have arrived yet, it leaves alone.
#[tokio::test]
async fn a_settled_build_row_whose_artifact_is_gone_is_dropped() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();

    let (spec, bytes) = spec_only(&ctx, "gone").await;
    let orphan = accept_build(&ctx, &spec, bytes.len() as u64).await;
    artifacts::delete(&ctx, &spec.artifact_sha256)
        .await
        .expect("the bytes go, the row stays");
    let pending = stage_build(&ctx, &blobs::sha256_hex(b"\0asm\x01pending"), 16).await;
    assert_eq!(storage_of(&ctx).await["artifacts"], 2);

    let report = gc::collect(&ctx, &shared).await.expect("collect");
    assert_eq!(report.build_rows_dropped, 1);
    assert!(
        repo::builds::get(&ctx, &orphan).await.is_err(),
        "the row naming nothing is gone"
    );
    repo::builds::get(&ctx, &pending)
        .await
        .expect("a staged row is a compile on its way, not an orphan");
    assert_eq!(storage_of(&ctx).await["artifacts"], 1);
}

/// A delete that fails partway through a collection must not throw away the
/// credit for the blobs the same pass already freed: the counters are saved
/// before the failure is returned, and say what the store still holds —
/// including the blob whose delete failed.
#[tokio::test]
async fn a_collection_that_fails_partway_keeps_what_it_already_freed() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();

    // Two unreachable, charged blobs, named so the one whose delete fails
    // sorts SECOND: the collector walks the listing in key order and stops
    // at the failure, so the first has to be freed before it for the credit
    // to be at stake.
    let (mut first, mut second) = (b"garbage one".to_vec(), b"garbage two".to_vec());
    if blobs::sha256_hex(&first) > blobs::sha256_hex(&second) {
        std::mem::swap(&mut first, &mut second);
    }
    let mut ws = workspace::Workspace::default();
    for bytes in [&first, &second] {
        blobs::put(&ctx, bytes).await.expect("store");
        ws.record_blob_stored(bytes.len() as u64);
    }
    workspace::save(&ctx, &ws)
        .await
        .expect("stage the workspace");

    let second_sha = blobs::sha256_hex(&second);
    ctx.fail_next_storage_delete_of("impresspress/dev", "blobs", &second_sha, "busy");
    gc::collect(&ctx, &shared)
        .await
        .expect_err("the failed delete is reported");

    assert!(!blobs::exists(&ctx, &blobs::sha256_hex(&first))
        .await
        .expect("exists"));
    assert!(blobs::exists(&ctx, &second_sha).await.expect("exists"));
    let after = workspace::load(&ctx).await.expect("load");
    assert_eq!(
        (after.blob_bytes, after.blob_count),
        (second.len() as u64, 1),
        "the first blob's credit survived the failure",
    );
}

// ---------------------------------------------------------------------------
// Artifacts
// ---------------------------------------------------------------------------

/// An artifact is reachable from a retained generation's block manifest. A
/// block that has been replaced keeps its artifact for as long as a generation
/// that can be rolled back to names it, and loses it — with its build row —
/// once none does.
#[tokio::test]
async fn gc_deletes_the_artifact_and_the_build_row_of_a_block_no_generation_names() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();

    let spec = block_spec(&ctx, "hello").await;
    let superseded = spec.artifact_sha256.clone();

    activation::request(
        &ctx,
        &shared,
        repo::generations::GenerationCause::BlockCompile,
        ActivationIntent::BlockSet {
            site: None,
            blocks: vec![spec],
        },
    )
    .await
    .expect("the block activates");

    // A second compile of the same block: the first artifact is now named only
    // by generations that are still inside the window.
    let replacement = block_spec(&ctx, "hello2").await;
    let kept = replacement.artifact_sha256.clone();
    activation::request(
        &ctx,
        &shared,
        repo::generations::GenerationCause::BlockCompile,
        ActivationIntent::BlockSet {
            site: None,
            blocks: vec![replacement],
        },
    )
    .await
    .expect("the replacement activates");
    assert!(
        artifacts::exists(&ctx, &superseded).await.expect("exists"),
        "a generation inside the window still names it, so it can be rolled back to",
    );

    // Twenty site writes push both of those generations out of the window.
    let mut sha = None;
    for i in 0..21 {
        sha = Some(write_file(&ctx, "site/index.html", &format!("v{i}"), sha.as_deref()).await);
    }

    assert!(
        !artifacts::exists(&ctx, &superseded).await.expect("exists"),
        "no retained generation names it any more",
    );
    assert!(
        artifacts::exists(&ctx, &kept).await.expect("exists"),
        "the active generation still runs it",
    );
    // The row went with the bytes: a build claiming an accepted artifact the
    // store no longer holds is what the duplicate-tool check would read back.
    assert_eq!(
        repo::builds::latest_valid_for_artifact(&ctx, &superseded)
            .await
            .expect("lookup"),
        None,
    );
    assert_eq!(storage_of(&ctx).await["artifacts"], 1);
}

/// A compile stores its artifact before any generation names it, and its build
/// row is what protects the bytes for as long as that lasts.
///
/// The window is a *status*, not a stretch of time. A browser compile takes
/// tens of seconds and an agent's site writes arrive in bursts, so the 21
/// writes here are exactly what an age-based rule ("younger than the oldest
/// retained generation") would let past — each one activates, each activation
/// collects, and by the last the staged build is older than every generation
/// the window keeps.
#[tokio::test]
async fn a_staged_build_protects_its_artifact_through_a_burst_of_site_writes() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;

    // The state `POST /b/dev/api/builds/stage` is in between its row and its
    // activation: row staged, bytes stored, no generation.
    let bytes = b"\0asm\x01staged";
    let artifact = blobs::sha256_hex(bytes);
    stage_build(&ctx, &artifact, bytes.len() as u64).await;
    artifacts::put(&ctx, bytes).await.expect("store");

    // The compile is still running while the agent edits the site.
    let mut sha = None;
    for i in 0..21 {
        sha = Some(write_file(&ctx, "site/index.html", &format!("v{i}"), sha.as_deref()).await);
    }
    assert!(
        artifacts::exists(&ctx, &artifact).await.expect("exists"),
        "a slow compile is still a compile: its row says the bytes are on their way",
    );

    // And the protection ends with the status, not with a clock: a refused
    // compile's artifact goes on the next collection.
    let row = repo::builds::list_in_flight(&ctx)
        .await
        .expect("list")
        .pop()
        .expect("the staged row");
    repo::builds::set_status(&ctx, &row.id, BuildStatus::Invalid, None, None)
        .await
        .expect("refuse");
    write_file(&ctx, "site/index.html", "v21", sha.as_deref()).await;
    assert!(
        !artifacts::exists(&ctx, &artifact).await.expect("exists"),
        "nothing is on its way to a generation any more",
    );
}

/// The collector lists before it reads its roots, and that ordering is the
/// whole of its soundness: an object stored after the listing is not a
/// candidate, and a root written after the listing is still read.
///
/// Unobservable without a seam — nothing in the fixture yields, so no compile
/// can interleave itself into the gap — so `GcInterleave` puts one there.
/// Under the reverse order (roots, then listing) both halves below are
/// deleted.
#[tokio::test]
async fn a_stage_that_lands_between_the_listing_and_the_roots_keeps_its_artifact() {
    /// Stages a build the way `blocks_api` does: the row for bytes that are
    /// already stored, then a second artifact stored from scratch.
    struct StageMidCollect<'a> {
        ctx: &'a TestContext,
        listed: String,
        unlisted: Vec<u8>,
    }

    #[wafer_block::wafer_async_trait]
    impl GcInterleave for StageMidCollect<'_> {
        async fn after_listing(&self) {
            // (a) A root for an artifact the listing already saw. The roots
            //     are read after this, so it must be seen.
            stage_build(self.ctx, &self.listed, 16).await;
            // (b) An artifact stored after the listing, with no root at all.
            //     It is not a candidate, so it needs none.
            artifacts::put(self.ctx, &self.unlisted)
                .await
                .expect("store");
        }
    }

    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();

    // An artifact in the store that nothing names — a candidate, and without
    // the interleaved row it would go.
    let listed = artifacts::put(&ctx, b"\0asm\x01listed")
        .await
        .expect("store");
    // A second one that no generation and no row will ever name, so the
    // collection below is doing real work rather than nothing at all.
    let doomed = artifacts::put(&ctx, b"\0asm\x01doomed")
        .await
        .expect("store");

    let unlisted = b"\0asm\x01unlisted".to_vec();
    let unlisted_sha = blobs::sha256_hex(&unlisted);
    let interleave = StageMidCollect {
        ctx: &ctx,
        listed: listed.clone(),
        unlisted,
    };
    let report = gc::collect_interleaved(&ctx, &shared, &interleave)
        .await
        .expect("collect");

    assert_eq!(report.artifacts_deleted, 1, "the unreferenced one went");
    assert!(!artifacts::exists(&ctx, &doomed).await.expect("exists"));
    assert!(
        artifacts::exists(&ctx, &listed).await.expect("exists"),
        "its build row was written before the roots were read",
    );
    assert!(
        artifacts::exists(&ctx, &unlisted_sha)
            .await
            .expect("exists"),
        "it was stored after the listing, so it was never a candidate",
    );
}

// ---------------------------------------------------------------------------
// Retention keeps what is live
// ---------------------------------------------------------------------------

/// Twenty refused activations after a good one do not make the good one
/// collectable: the active generation is what the site *is*, and its blobs are
/// what the site serves.
///
/// Driven through real refusals — a `rebuild` the control refuses leaves a
/// `Failed` row and the previous generation still live — so the fixture is the
/// state a run of bad compiles actually produces.
#[tokio::test]
async fn retention_keeps_the_serving_generation_and_its_blobs_under_a_run_of_failures() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    let shared = ctx.dev_shared();

    let live_blob = write_file(&ctx, "site/index.html", "live", None).await;
    let active = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/status"))
            .await,
    )
    .await["active_generation"]["id"]
        .as_str()
        .expect("an active generation")
        .to_string();

    // Twenty-two failed block activations, each leaving a row newer than the
    // one that is serving.
    for i in 0..22 {
        control.fail_next_rebuild("wasmi: boom");
        let spec = block_spec(&ctx, &format!("b{i}")).await;
        activation::request(
            &ctx,
            &shared,
            repo::generations::GenerationCause::BlockCompile,
            ActivationIntent::BlockSet {
                site: None,
                blocks: vec![spec],
            },
        )
        .await
        .expect_err("a refused rebuild refuses the activation");
    }

    // Retention runs on a successful activation, and the serving generation is
    // by then far outside the newest twenty.
    let pruned = retention::prune(&ctx).await.expect("prune");
    assert!(!pruned.is_empty(), "the ledger is past the window");
    assert!(
        pruned.iter().all(|row| row.id != active),
        "the serving generation is not retention's to delete: {pruned:?}",
    );
    assert_eq!(
        generations::get(&ctx, &active).await.expect("get").status,
        GenerationStatus::Active,
    );

    // And a `Failed` row inside the window keeps its status: nothing rewrites
    // a status because a row got old.
    let newest = generations::list_recent(&ctx, 1).await.expect("list");
    assert_eq!(newest[0].status, GenerationStatus::Failed);

    // The collector reads the same retained set, so the live site's blob
    // survives a ledger dominated by failures — and the assertion is
    // discriminating because that write REPLACES the workspace entry naming
    // it, leaving the serving generation as its only root.
    write_file(&ctx, "site/index.html", "replaced", Some(&live_blob)).await;
    let ws = workspace::load(&ctx).await.expect("load workspace");
    assert!(
        !ws.references(&live_blob),
        "the fixture must leave the workspace naming something else",
    );
    assert!(
        blobs::exists(&ctx, &live_blob).await.expect("exists"),
        "a generation inside the window still names it",
    );
}

/// The serving generation's manifest is a root in its own right. A collector
/// that only trusted the workspace would delete the bytes the site is being
/// served from the moment a generation stopped matching the editable state —
/// which is what a rollback, and every `BlockSet` intent carrying an explicit
/// site, produces.
#[tokio::test]
async fn a_blob_only_the_active_generation_names_survives_collection() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();

    // Published straight from a manifest, so the workspace never names it.
    let (site, served) = site_only_blob(&ctx, "<h1>live</h1>").await;
    activation::request(
        &ctx,
        &shared,
        GenerationCause::SiteWrite,
        ActivationIntent::BlockSet {
            site: Some(site),
            blocks: Vec::new(),
        },
    )
    .await
    .expect("the manifest activates");

    // Something genuinely unreachable, so the collection below is doing work.
    let (orphan, _stored) = blobs::put(&ctx, b"nobody names me").await.expect("put");

    let ws = workspace::load(&ctx).await.expect("load workspace");
    assert!(
        ws.files.is_empty() && !ws.references(&served),
        "the fixture is only meaningful if the workspace does not name it",
    );

    let report = gc::collect(&ctx, &shared).await.expect("collect");
    assert_eq!(report.blobs_deleted, 1);
    assert!(!blobs::exists(&ctx, &orphan).await.expect("exists"));
    assert!(
        blobs::exists(&ctx, &served).await.expect("exists"),
        "the generation that is serving names it, and that is a root",
    );
}

/// The builds table is the index of what the artifact store holds, so a row
/// for bytes that never made it in is a lie in both directions: the collector
/// would protect an object that does not exist, and `dev_status` would report
/// it as stored.
#[tokio::test]
async fn a_stage_whose_artifact_cannot_be_stored_leaves_no_build_row() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    ctx.fail_next_storage_put("the disk is on fire");

    let out = dev_post(
        &ctx,
        "/b/dev/api/builds/stage",
        json!({
            "block_name": "hello",
            "artifact_base64": Base64::encode_string(b"\0asm\x01\0\0\0"),
            "compiler_version": "test",
            "diagnostics": [],
        }),
    )
    .await;
    assert_eq!(output_http_status(out).await, 500);

    assert!(
        repo::builds::list_in_flight(&ctx)
            .await
            .expect("list")
            .is_empty(),
        "the row went back out with the bytes that never arrived",
    );
    assert!(repo::builds::artifact_index(&ctx)
        .await
        .expect("index")
        .is_empty());
    assert_eq!(storage_of(&ctx).await["artifacts"], 0);
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

/// An in-flight generation is kept because an activation might still finish
/// it. Nothing is running on a process that has just started, so a staged row
/// at boot is wreckage — and left staged it would keep its blobs against the
/// workspace quota for the life of the instance, however far down the ledger
/// it fell.
#[tokio::test]
async fn an_orphaned_staged_generation_is_retired_at_boot_and_its_blobs_collected() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();

    let (site, pinned) = site_only_blob(&ctx, "<h1>never activated</h1>").await;
    let orphan = stage_generation(&ctx, site).await;

    // Push it well past the retention window.
    let mut sha = None;
    for i in 0..21 {
        sha = Some(write_file(&ctx, "site/index.html", &format!("v{i}"), sha.as_deref()).await);
    }
    assert_eq!(
        generations::get(&ctx, &orphan).await.expect("get").status,
        GenerationStatus::Staged,
        "an in-flight row is not retention's to delete",
    );
    assert!(
        blobs::exists(&ctx, &pinned).await.expect("exists"),
        "and so its blob is pinned — which is the leak",
    );

    // The journal names nothing, so nothing is owed and the row is wreckage.
    activation::converge_on_boot(&ctx, &shared)
        .await
        .expect("boot");
    let retired = generations::get(&ctx, &orphan).await.expect("get");
    assert_eq!(retired.status, GenerationStatus::Failed);
    assert!(
        retired
            .failure_message
            .as_deref()
            .unwrap_or_default()
            .contains("abandoned at boot"),
        "the row must say why it was closed: {retired:?}",
    );

    // Now it is ordinary history, so the next activation prunes it and the
    // collector reclaims what only it named.
    write_file(&ctx, "site/index.html", "v21", sha.as_deref()).await;
    assert_eq!(
        generations::get(&ctx, &orphan)
            .await
            .expect_err("pruned")
            .code,
        wafer_run::ErrorCode::NotFound,
    );
    assert!(
        !blobs::exists(&ctx, &pinned).await.expect("exists"),
        "nothing names it any more",
    );
}

/// A crash in the middle of a block compile is the case the journal exists
/// for, and boot must not turn it into a permanently broken sandbox.
///
/// Convergence makes the block live again, so its build row is where
/// `latest_valid_for_artifact` finds the `BlockInfo` the duplicate-agent-tool
/// check reads — retiring that row would leave a live block with no accepted
/// build, and every later `stage` of *another* block refused for a collision
/// check it cannot run. So a staged row whose artifact a vouched-for manifest
/// names is accepted, and only the rest are closed.
#[tokio::test]
async fn a_staged_build_the_journal_vouches_for_is_accepted_at_boot() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    let shared = ctx.dev_shared();

    // The state a crash mid-compile leaves: the artifact stored, the build row
    // still staged, the generation staged, and the journal pointing at it.
    let (spec, bytes) = spec_only(&ctx, "hello").await;
    let converging = spec.artifact_sha256.clone();
    let build = stage_build(&ctx, &converging, bytes.len() as u64).await;
    let desired = stage_generation_of(&ctx, SiteManifest::default(), vec![spec]).await;
    runtime_state::write(
        &ctx,
        &RuntimeState {
            active_generation_id: None,
            desired_generation_id: Some(desired.clone()),
            activation_phase: ActivationPhase::BuildingRuntime,
            generation: 0,
        },
    )
    .await
    .expect("journal the interrupted activation");

    // A second compile that got nowhere: nothing vouches for it.
    let (_orphan_spec, orphan_bytes) = spec_only(&ctx, "orphan").await;
    let orphan_artifact = blobs::sha256_hex(&orphan_bytes);
    let orphan_build = stage_build(&ctx, &orphan_artifact, orphan_bytes.len() as u64).await;

    let blocks = activation::converge_on_boot(&ctx, &shared)
        .await
        .expect("boot");
    assert_eq!(
        blocks.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
        vec!["site/hello"],
        "the journalled activation is the one that converges",
    );

    // The row for the block that is now live was accepted, not closed — and
    // it is reachable through the lookup the collision check uses.
    assert_eq!(
        repo::builds::get(&ctx, &build).await.expect("get").status,
        BuildStatus::Valid,
    );
    assert!(
        repo::builds::latest_valid_for_artifact(&ctx, &converging)
            .await
            .expect("lookup")
            .is_some(),
        "a live block must have an accepted build recording its BlockInfo",
    );

    // The one nothing vouches for was closed instead, which unpinned its
    // artifact — and the collection the converged activation ran has already
    // taken both the object and the row. (The retirement's own message is
    // asserted by the boot test below, which converges nothing and so leaves
    // the row to look at.)
    assert!(
        repo::builds::list_in_flight(&ctx)
            .await
            .expect("list")
            .is_empty(),
        "boot settles every staged row, one way or the other",
    );
    assert!(
        !artifacts::exists(&ctx, &orphan_artifact)
            .await
            .expect("exists"),
        "no compile is coming for it",
    );
    assert_eq!(
        repo::builds::get(&ctx, &orphan_build)
            .await
            .expect_err("collected with its artifact")
            .code,
        wafer_run::ErrorCode::NotFound,
    );
    assert!(
        artifacts::exists(&ctx, &converging).await.expect("exists"),
        "the generation that converged names it",
    );
}

/// A journalled generation that could not be READ is not a dangling one. A
/// transient fault or a WRAP denial on the generations table says nothing
/// about what the desired generation names, so boot must neither retire the
/// builds it may vouch for nor abandon the generation: it gives up, and leaves
/// the journal for the next boot to converge on.
#[tokio::test]
async fn a_failed_read_of_the_journalled_generation_settles_nothing() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    let shared = ctx.dev_shared();

    let (spec, bytes) = spec_only(&ctx, "hello").await;
    let build = stage_build(&ctx, &spec.artifact_sha256, bytes.len() as u64).await;
    let desired = stage_generation_of(&ctx, SiteManifest::default(), vec![spec]).await;
    let journal = RuntimeState {
        active_generation_id: None,
        desired_generation_id: Some(desired.clone()),
        activation_phase: ActivationPhase::BuildingRuntime,
        generation: 0,
    };
    runtime_state::write(&ctx, &journal)
        .await
        .expect("journal the interrupted activation");

    let unreadable = FailingDbOpContext::failing_with(
        ctx.clone(),
        vec![("database.get", generations::TABLE)],
        wafer_run::WaferError::new(
            wafer_run::ErrorCode::PermissionDenied,
            "WRAP: impresspress/dev holds no grant on this table",
        ),
    );
    activation::converge_on_boot(&unreadable, &shared)
        .await
        .expect_err("a boot that cannot read its journal gives up");

    assert_eq!(
        repo::builds::get(&ctx, &build).await.expect("get").status,
        BuildStatus::Staged,
        "the compile the journal may vouch for is left in flight",
    );
    assert_ne!(
        generations::get(&ctx, &desired).await.expect("get").status,
        GenerationStatus::Failed,
        "the journalled generation is not abandoned",
    );
    assert_eq!(
        runtime_state::read(&ctx)
            .await
            .expect("read")
            .desired_generation_id,
        Some(desired.clone()),
        "the journal is kept for the next boot",
    );

    // The next boot, reading fine, converges on it.
    let blocks = activation::converge_on_boot(&ctx, &shared)
        .await
        .expect("boot");
    assert_eq!(
        blocks.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
        vec!["site/hello"],
    );
    assert_eq!(
        repo::builds::get(&ctx, &build).await.expect("get").status,
        BuildStatus::Valid,
    );
}

/// Builds have no journal to be named by, so boot settles each staged row
/// against the manifests instead — and the *serving* manifest vouches for one
/// just as the journalled one does. A crash between an activation committing
/// and its build row being accepted is one `set_status` wide, and closing that
/// row would leave a live block with no `BlockInfo` on record.
#[tokio::test]
async fn boot_accepts_the_staged_build_of_a_live_block_and_closes_the_rest() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();

    // A block that IS serving, whose build row never got as far as `valid`:
    // staged before its activation, as `blocks_api` stages it, and still
    // staged after it.
    let (spec, live_bytes) = spec_only(&ctx, "live").await;
    let live_artifact = spec.artifact_sha256.clone();
    let live_build = stage_build(&ctx, &live_artifact, live_bytes.len() as u64).await;
    activation::request(
        &ctx,
        &shared,
        GenerationCause::BlockCompile,
        ActivationIntent::BlockSet {
            site: None,
            blocks: vec![spec],
        },
    )
    .await
    .expect("the block activates");

    // And a compile that got nowhere at all.
    let bytes = b"\0asm\x01abandoned";
    let artifact = artifacts::put(&ctx, bytes).await.expect("store");
    let row = stage_build(&ctx, &artifact, bytes.len() as u64).await;

    // The journal owes nothing, so the active generation is the only thing
    // that can vouch for either of them.
    activation::converge_on_boot(&ctx, &shared)
        .await
        .expect("boot");

    let promoted = repo::builds::get(&ctx, &live_build).await.expect("get");
    assert_eq!(
        promoted.status,
        BuildStatus::Valid,
        "the generation that is serving names its artifact",
    );
    let retired = repo::builds::get(&ctx, &row).await.expect("get");
    assert_eq!(retired.status, BuildStatus::Invalid);
    assert!(
        retired.diagnostics_json.contains("abandoned at boot"),
        "the row must say why it was closed: {}",
        retired.diagnostics_json,
    );

    gc::collect(&ctx, &shared).await.expect("collect");
    assert!(
        !artifacts::exists(&ctx, &artifact).await.expect("exists"),
        "no compile is coming for it",
    );
    assert!(
        artifacts::exists(&ctx, &live_artifact)
            .await
            .expect("exists"),
        "and the serving block keeps its own",
    );
}
