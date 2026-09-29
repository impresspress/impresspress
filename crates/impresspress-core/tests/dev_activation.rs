//! Generations, the activation queue, the site publisher and boot recovery.
//!
//! Gated on `block-dev` for the same reason `dev_files.rs` is: the block does
//! not exist in a default-feature build, so these tests must not compile
//! there.
#![cfg(feature = "block-dev")]

use impresspress_core::{
    blocks::dev::{
        activation::{self, ActivationError, ActivationIntent},
        artifacts, blobs,
        contracts::SiteManifest,
        control::{DynamicBlockSpec, DynamicRoute, RouteAccessKind},
        generation::{self, GenerationManifest},
        repo::{
            self,
            generations::{self, GenerationCause, GenerationStatus, NewGeneration},
            runtime_state::{self, ActivationPhase, RuntimeState},
        },
        test_support::{
            dev_get, dev_post, dev_status, dev_with_accounts, signed_in_as, FakeControl,
        },
        workspace::FileEntry,
        WAFER_GUEST_VERSION,
    },
    test_support::{
        admin_msg, anon_msg, output_http_header, output_http_status, output_json, TestContext,
    },
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `content` at `path`, expecting the file to hold `expected`.
async fn write_file(
    ctx: &TestContext,
    path: &str,
    content: &str,
    expected: Option<&str>,
) -> serde_json::Value {
    output_json(
        dev_post(
            ctx,
            "/b/dev/api/files/write",
            json!({"path": path, "content": content, "expected_sha256": expected}),
        )
        .await,
    )
    .await
}

/// The published site object at `key`, or `None` when nothing is there.
async fn served(ctx: &TestContext, key: &str) -> Option<Vec<u8>> {
    ctx.storage_get("wafer-run/web", "site", key).await.ok()
}

/// The sha256 of `content`, as the files API reports it — the value a write
/// passes back as `expected_sha256`.
fn sha_of(content: &str) -> String {
    blobs::sha256_hex(content.as_bytes())
}

// ---------------------------------------------------------------------------
// A site write publishes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_site_write_creates_and_activates_a_generation_without_rebuilding_the_runtime() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;

    let w = write_file(&ctx, "site/index.html", "<h1>v1</h1>", None).await;
    assert_eq!(w["generation"]["cause"], "site_write");
    assert_eq!(w["generation"]["status"], "active");
    assert_eq!(w["generation"]["site_files"], 1);
    assert_eq!(w["generation"]["blocks"], 0);
    assert!(
        control.rebuilds().is_empty(),
        "site-only changes never rebuild the runtime: {:?}",
        control.rebuilds()
    );

    // The published site folder holds exactly the manifest.
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(b"<h1>v1</h1>".as_slice())
    );

    let status = dev_status(&ctx).await;
    assert_eq!(status["active_generation"]["id"], w["generation"]["id"]);
    // Nothing is in flight once the request has answered.
    assert_eq!(status["activation"], serde_json::Value::Null);
}

#[tokio::test]
async fn deleting_a_site_file_removes_it_from_the_served_folder() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;

    let w = write_file(&ctx, "site/a.css", "a{}", None).await;
    let sha = w["sha256"].as_str().expect("sha256").to_string();
    assert_eq!(
        served(&ctx, "a.css").await.as_deref(),
        Some(b"a{}".as_slice())
    );

    let d = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/delete",
            json!({"path": "site/a.css", "expected_sha256": sha}),
        )
        .await,
    )
    .await;
    assert_eq!(d["generation"]["cause"], "site_delete");
    assert_eq!(d["generation"]["site_files"], 0);
    assert!(
        served(&ctx, "a.css").await.is_none(),
        "the published site must not keep a file the generation dropped"
    );
}

#[tokio::test]
async fn block_source_writes_do_not_create_generations() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;

    let w = write_file(&ctx, "blocks/hello/src/lib.rs", "// rust", None).await;
    assert_eq!(w["generation"], serde_json::Value::Null);

    let l = output_json(dev_get(&ctx, "/b/dev/api/generations").await).await;
    assert!(
        l["generations"].as_array().expect("generations").is_empty(),
        "only a compile publishes a block: {l}"
    );
    assert_eq!(
        dev_status(&ctx).await["active_generation"],
        serde_json::Value::Null
    );

    // Deleting block source publishes nothing either.
    let sha = w["sha256"].as_str().expect("sha256").to_string();
    let d = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/delete",
            json!({"path": "blocks/hello/src/lib.rs", "expected_sha256": sha}),
        )
        .await,
    )
    .await;
    assert_eq!(d["generation"], serde_json::Value::Null);
}

/// Validation asks whether every blob and artifact the manifest names is
/// stored, over the WHOLE manifest, on every activation — every keystroke-save
/// included. Asking by downloading would make each save transfer every blob of
/// the site and every block's artifact (up to 4 MiB each), so an activation
/// must read in full only what it publishes.
///
/// Counted at the object store, underneath the storage block: a buffered `get`
/// is a full-body read, a `get_streaming` answers from the object's metadata
/// and transfers only what its consumer reads.
#[tokio::test]
async fn a_site_write_reads_in_full_only_the_content_it_publishes() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    let shared = ctx.dev_shared();

    // A live block and two site files: everything validation has to vouch for
    // on the next save.
    write_file(&ctx, "site/index.html", "<h1>v1</h1>", None).await;
    write_file(&ctx, "site/style.css", "h1{}", None).await;
    let intent = compile_of(&ctx, &["hello"]).await;
    activation::request(&ctx, &shared, GenerationCause::BlockCompile, intent)
        .await
        .expect("the block activates");

    let before = ctx.storage_reads().len();
    write_file(
        &ctx,
        "site/index.html",
        "<h1>v2</h1>",
        Some(&sha_of("<h1>v1</h1>")),
    )
    .await;
    let reads: Vec<String> = ctx.storage_reads()[before..].to_vec();

    // The one blob a full read is owed: the changed page, which the publisher
    // copies into the site folder. Nothing else — not the unchanged
    // stylesheet, not the new page a second time.
    let published = format!("get impresspress/dev/blobs/{}", sha_of("<h1>v2</h1>"));
    let full_blob_reads: Vec<&String> = reads
        .iter()
        .filter(|read| read.starts_with("get impresspress/dev/blobs/"))
        .collect();
    assert_eq!(full_blob_reads, vec![&published], "all reads: {reads:#?}");
    // And no artifact read of any kind: the builds ledger answers for them.
    assert!(
        !reads
            .iter()
            .any(|read| read.contains("impresspress/dev/artifacts/")),
        "all reads: {reads:#?}",
    );
    // The presence checks did run, as probes: the unchanged stylesheet was
    // asked about without being transferred.
    assert!(
        reads.contains(&format!(
            "get_streaming impresspress/dev/blobs/{}",
            sha_of("h1{}")
        )),
        "all reads: {reads:#?}",
    );
}

// ---------------------------------------------------------------------------
// Driving the queue directly
// ---------------------------------------------------------------------------

/// A site manifest holding one `index.html` with `content`, its blob stored.
async fn site_of(ctx: &TestContext, content: &str) -> SiteManifest {
    let (sha256, _stored) = blobs::put(ctx, content.as_bytes())
        .await
        .expect("store the blob a manifest names");
    SiteManifest {
        files: vec![FileEntry {
            path: "index.html".to_string(),
            sha256,
            size: content.len() as u64,
            content_type: "text/html; charset=utf-8".to_string(),
        }],
    }
}

/// One block named `name`, stored the way staging stores one — a build row,
/// then the artifact — so validation passes.
///
/// The row is not decoration: activation answers "is this artifact stored?"
/// from the builds ledger (`activation::missing_content`), and every path that
/// stores an artifact in production writes one. A spec whose bytes were put
/// with no row is a state only a test can build.
async fn block_spec(ctx: &TestContext, name: &str) -> DynamicBlockSpec {
    // Distinct bytes per block, so two specs are distinguishable by artifact
    // as well as by name.
    let bytes = format!("\0asm\x01{name}").into_bytes();
    let registered = format!("site/{name}");
    repo::builds::insert(
        ctx,
        &repo::builds::NewBuild {
            block_name: registered.clone(),
            source_manifest_sha256: "src".to_string(),
            artifact_sha256: blobs::sha256_hex(&bytes),
            block_info_json: "null".to_string(),
            diagnostics_json: "[]".to_string(),
            compiler_version: "rubrc@pinned".to_string(),
            artifact_bytes: bytes.len() as u64,
        },
    )
    .await
    .expect("record the build");
    let artifact_sha256 = artifacts::put(ctx, &bytes)
        .await
        .expect("store the artifact a manifest names");
    DynamicBlockSpec {
        name: registered,
        artifact_sha256,
        routes: vec![DynamicRoute {
            prefix: format!("/b/{name}/"),
            access: RouteAccessKind::Public,
        }],
        capabilities: wafer_block::BlockCapabilities::default(),
        wafer_guest_version: WAFER_GUEST_VERSION,
    }
}

/// The intent Task 8 will produce from a successful compile: the workspace's
/// own site, plus this block set.
async fn compile_of(ctx: &TestContext, names: &[&str]) -> ActivationIntent {
    let mut blocks = Vec::new();
    for name in names {
        blocks.push(block_spec(ctx, name).await);
    }
    ActivationIntent::BlockSet { site: None, blocks }
}

/// The block names the active generation declares, sorted.
async fn active_block_names(ctx: &TestContext) -> Vec<String> {
    let status = dev_status(ctx).await;
    let mut names: Vec<String> = status["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .map(|b| b["name"].as_str().expect("name").to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn rollback_republishes_an_earlier_generation_as_a_new_one() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;

    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let sha1 = g1["sha256"].as_str().expect("sha256").to_string();
    write_file(&ctx, "site/index.html", "v2", Some(&sha1)).await;
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v2"[..])
    );

    let id1 = g1["generation"]["id"].as_str().expect("id").to_string();
    let r = output_json(
        dev_post(
            &ctx,
            &format!("/b/dev/api/generations/{id1}/rollback"),
            json!({}),
        )
        .await,
    )
    .await;
    assert_eq!(r["generation"]["cause"], "rollback");
    assert_ne!(r["generation"]["id"], json!(id1), "history is append-only");
    assert!(
        r["generation"]["parent_id"].is_string(),
        "a rollback is derived from whatever was live, not from its target"
    );
    // The progress list ends at `active`, as every activation's does.
    let phases: Vec<&str> = r["progress"]
        .as_array()
        .expect("progress")
        .iter()
        .map(|s| s["phase"].as_str().expect("phase"))
        .collect();
    assert_eq!(phases.last(), Some(&"active"), "{phases:?}");
    assert!(
        !phases.contains(&"building_runtime"),
        "a site-only rollback rebuilds nothing"
    );

    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v1"[..])
    );
    // The workspace follows the rollback so the next edit starts from v1.
    let read = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "site/index.html"}),
        )
        .await,
    )
    .await;
    assert_eq!(read["content"], "v1");
    assert_eq!(read["sha256"], json!(sha1));
}

/// A rollback commits and only then adopts its site into the workspace, so an
/// adoption that fails arrives with the rollback already serving. Reported as
/// a plain failure, the caller would go after a rollback that happened; the
/// refusal has to say it is live and what repairs the workspace — and the
/// repair it names has to work.
#[tokio::test]
async fn a_rollback_whose_workspace_update_fails_says_it_is_live_and_a_retry_repairs_it() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;

    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let id1 = g1["generation"]["id"].as_str().expect("id").to_string();
    write_file(&ctx, "site/index.html", "v2", Some(&sha_of("v1"))).await;

    // The publish goes through; the save of `workspace.json` that adopts the
    // rolled-back site does not.
    ctx.fail_next_storage_put_to("impresspress/dev", "", "workspace.json", "disk is full");
    let rollback_path = format!("/b/dev/api/generations/{id1}/rollback");
    let refused = wafer_block::http_codec::collect_http_response(
        dev_post(&ctx, &rollback_path, json!({})).await,
    )
    .await;
    assert_eq!(refused.status, 500);
    let body: serde_json::Value = serde_json::from_slice(&refused.body).expect("json refusal");
    let message = body["message"].as_str().expect("message");
    assert!(message.contains("the rollback is live"), "{message}");
    assert!(message.contains("disk is full"), "{message}");
    assert!(
        message.contains("Retrying the same rollback repairs the workspace"),
        "{message}"
    );

    // Which is true: the rolled-back site is what is served, and a
    // generation carrying it is the active one.
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v1"[..])
    );
    let status = dev_status(&ctx).await;
    let live = status["active_generation"]["id"]
        .as_str()
        .expect("an active generation")
        .to_string();
    assert!(
        message.contains(&live),
        "the message names {live}: {message}"
    );
    // And the workspace was left behind, still holding what the rollback
    // replaced.
    let read = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "site/index.html"}),
        )
        .await,
    )
    .await;
    assert_eq!(read["content"], "v2");

    // The retry the message prescribes.
    let retried = output_json(dev_post(&ctx, &rollback_path, json!({})).await).await;
    assert_eq!(retried["generation"]["cause"], "rollback", "{retried}");
    let read = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "site/index.html"}),
        )
        .await,
    )
    .await;
    assert_eq!(read["content"], "v1", "the retry adopted the site");
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v1"[..])
    );
}

/// A rollback rewrites the workspace as well as publishing, and both have to
/// happen under one queue lease.
///
/// The window this closes: with the adoption outside the queue, a site write
/// admitted while the rollback was in flight dequeued *between* the rollback's
/// publish and its workspace rewrite. It composed its manifest from the
/// workspace the rollback had not yet touched, republished the pre-rollback
/// site over the rolled-back one, and the adoption then rewrote the workspace
/// to the rollback's — leaving the published folder and the workspace
/// disagreeing, with nothing left to reconcile them.
#[tokio::test]
async fn a_site_write_racing_a_rollback_leaves_the_workspace_and_the_site_agreeing() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    let shared = ctx.dev_shared();

    // g1: v1 and no blocks — the rollback target.
    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let id1 = g1["generation"]["id"].as_str().expect("id").to_string();
    // A block, so rolling back to g1 changes the block set and therefore
    // rebuilds — which is the one place the fixture can hold the queue open.
    activation::request(
        &ctx,
        &shared,
        GenerationCause::BlockCompile,
        compile_of(&ctx, &["hello"]).await,
    )
    .await
    .expect("the block activates");
    // v2, so the workspace and the rollback target genuinely differ.
    write_file(&ctx, "site/index.html", "v2", Some(&sha_of("v1"))).await;

    let rollback_path = format!("/b/dev/api/generations/{id1}/rollback");
    let v2 = sha_of("v2");
    let release = control.gate_next_rebuild();
    let (rollback, write, ()) = tokio::join!(
        dev_post(&ctx, &rollback_path, json!({})),
        write_file(&ctx, "site/index.html", "v3", Some(&v2)),
        async {
            let _ = release.send(());
        },
    );
    let rollback = output_json(rollback).await;
    assert_eq!(rollback["generation"]["cause"], "rollback", "{rollback}");
    assert!(write["generation"].is_object(), "{write}");

    // The last intent to dequeue was the site write, and it composed its
    // manifest from the workspace the rollback had already adopted — so the
    // two agree, and they agree on the rolled-back content.
    let published = served(&ctx, "index.html").await;
    assert_eq!(published.as_deref(), Some(&b"v1"[..]));
    let read = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "site/index.html"}),
        )
        .await,
    )
    .await;
    assert_eq!(read["content"], "v1", "{read}");
    assert_eq!(read["sha256"], json!(sha_of("v1")));

    // Two rebuilds — the compile and the rollback. A site-only publish never
    // rebuilds, whichever generation it is composed against.
    assert_eq!(control.rebuilds().len(), 2);
}

#[tokio::test]
async fn a_failed_runtime_rebuild_leaves_the_previous_generation_active() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    write_file(&ctx, "site/index.html", "v1", None).await;
    let before = dev_status(&ctx).await;

    control.fail_next_rebuild("wasmi: boom");
    // Task 8 stages a block; here drive the queue directly with a manifest
    // that carries one.
    let err = activation::request(
        &ctx,
        &ctx.dev_shared(),
        GenerationCause::BlockCompile,
        compile_of(&ctx, &["hello"]).await,
    )
    .await
    .expect_err("a refused rebuild must refuse the activation");
    assert!(
        matches!(&err, ActivationError::Runtime(m) if m.contains("boom")),
        "{err:?}"
    );

    let after = dev_status(&ctx).await;
    assert_eq!(
        after["active_generation"]["id"],
        before["active_generation"]["id"]
    );
    // The journal is back at rest: nothing is owed on the next boot.
    assert_eq!(after["activation"], serde_json::Value::Null);
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v1"[..])
    );

    let l = output_json(dev_get(&ctx, "/b/dev/api/generations").await).await;
    assert_eq!(l["generations"][0]["status"], "failed");
    assert_eq!(l["generations"][0]["cause"], "block_compile");
    assert_eq!(l["generations"][1]["status"], "active");
}

#[tokio::test]
async fn a_manifest_naming_content_that_is_not_stored_is_refused() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    // An explicit site (rather than the workspace's own) is the only way to
    // name content the store does not hold: a `SiteOnly` intent reads the
    // workspace, whose every entry is stored by construction.
    let mut site = site_of(&ctx, "v1").await;
    site.files[0].sha256 = blobs::sha256_hex(b"never stored");

    let err = activation::request(
        &ctx,
        &ctx.dev_shared(),
        GenerationCause::SiteWrite,
        ActivationIntent::BlockSet {
            site: Some(site),
            blocks: Vec::new(),
        },
    )
    .await
    .expect_err("a manifest naming content that is not stored cannot activate");
    assert!(matches!(err, ActivationError::Validation(_)), "{err:?}");
    assert_eq!(err.status(), 422);

    // The staged generation is on the ledger as `failed`, not silently gone.
    let l = output_json(dev_get(&ctx, "/b/dev/api/generations").await).await;
    assert_eq!(l["generations"][0]["status"], "failed");
    assert_eq!(
        dev_status(&ctx).await["active_generation"],
        serde_json::Value::Null
    );
}

/// The 422 as a caller sees it, through a real endpoint: a rollback to a
/// generation whose blobs have been collected.
#[tokio::test]
async fn rolling_back_to_a_generation_whose_content_is_gone_is_a_422() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let sha1 = g1["sha256"].as_str().expect("sha256").to_string();
    write_file(&ctx, "site/index.html", "v2", Some(&sha1)).await;

    blobs::delete(&ctx, &sha1).await.expect("collect the blob");

    let id1 = g1["generation"]["id"].as_str().expect("id").to_string();
    let out = dev_post(
        &ctx,
        &format!("/b/dev/api/generations/{id1}/rollback"),
        json!({}),
    )
    .await;
    assert_eq!(output_http_status(out).await, 422);

    // Nothing moved: the site and the workspace both still hold v2.
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v2"[..])
    );
    let read = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "site/index.html"}),
        )
        .await,
    )
    .await;
    assert_eq!(read["content"], "v2");
}

// ---------------------------------------------------------------------------
// Boot recovery
// ---------------------------------------------------------------------------

/// Stage a generation the way a crashed activation would have left one: the
/// row exists, its blobs are stored, and the journal points at it mid-publish.
async fn insert_staged_generation(ctx: &TestContext, content: &str) -> String {
    let state = runtime_state::read(ctx).await.expect("read journal");
    let id = repo::new_id();
    let mut manifest = GenerationManifest::staged(site_of(ctx, content).await, Vec::new());
    manifest.identify(id.clone(), state.active_generation_id.clone());

    generations::insert(
        ctx,
        &NewGeneration {
            id,
            parent_id: manifest.parent_id.clone(),
            cause: GenerationCause::SiteWrite,
            site_manifest_json: generation::canonical_text(&manifest.site).expect("canonical"),
            block_manifest_json: generation::canonical_text(&manifest.blocks).expect("canonical"),
            manifest_sha256: generation::manifest_sha256(&manifest).expect("hash"),
        },
    )
    .await
    .expect("stage the generation");

    runtime_state::write(
        ctx,
        &RuntimeState {
            desired_generation_id: Some(manifest.generation_id.clone()),
            activation_phase: ActivationPhase::Publishing,
            ..state
        },
    )
    .await
    .expect("journal the interrupted activation");
    manifest.generation_id
}

#[tokio::test]
async fn boot_converges_an_interrupted_activation() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    write_file(&ctx, "site/index.html", "v1", None).await;

    // Simulate a crash mid-activation: desired points at a staged generation.
    let staged = insert_staged_generation(&ctx, "v2").await;

    let blocks = activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("converge");
    assert!(
        blocks.is_empty(),
        "the active generation declares no blocks"
    );

    let state = runtime_state::read(&ctx).await.expect("read journal");
    assert_eq!(state.desired_generation_id, None);
    assert_eq!(state.activation_phase, ActivationPhase::Idle);
    assert_eq!(state.active_generation_id.as_deref(), Some(staged.as_str()));
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v2"[..])
    );
}

/// Convergence that cannot succeed must still leave a coherent instance: the
/// previous generation live, its site published, and nothing owed.
#[tokio::test]
async fn a_failed_convergence_restores_the_previous_generation() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let active = g1["generation"]["id"].as_str().expect("id").to_string();

    let staged = insert_staged_generation(&ctx, "v2").await;
    // Collect the blob the staged generation names, so it can never activate.
    let (sha, _) = blobs::put(&ctx, b"v2").await.expect("hash");
    blobs::delete(&ctx, &sha).await.expect("collect");

    let blocks = activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("a failed convergence is not a failed boot");
    assert!(blocks.is_empty());

    let state = runtime_state::read(&ctx).await.expect("read journal");
    assert_eq!(state.desired_generation_id, None);
    assert_eq!(state.activation_phase, ActivationPhase::Idle);
    assert_eq!(state.active_generation_id.as_deref(), Some(active.as_str()));
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v1"[..])
    );

    let detail =
        output_json(dev_get(&ctx, &format!("/b/dev/api/generations/{staged}")).await).await;
    assert_eq!(detail["summary"]["status"], "failed");
}

/// Nothing in flight: convergence is a read that reports the block set the
/// host should build with.
#[tokio::test]
async fn boot_reports_the_active_block_set_when_nothing_is_owed() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    write_file(&ctx, "site/index.html", "v1", None).await;
    activation::request(
        &ctx,
        &ctx.dev_shared(),
        GenerationCause::BlockCompile,
        compile_of(&ctx, &["hello"]).await,
    )
    .await
    .expect("activate a generation with a block");

    let blocks = activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("converge");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].name, "site/hello");
    // Convergence with nothing owed must not rebuild: the host is about to
    // build its runtime from what this returned.
    assert_eq!(control.rebuilds().len(), 1, "only the activation rebuilt");
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// A generation that was staged and never activated — the row a crash between
/// the insert and the swap leaves behind, and the row boot convergence would
/// re-run. Retention keeps it however old it gets.
async fn stage_only(ctx: &TestContext) -> String {
    let id = repo::new_id();
    let manifest = {
        let mut manifest = GenerationManifest::staged(SiteManifest::default(), Vec::new());
        manifest.identify(id.clone(), None);
        manifest
    };
    generations::insert(
        ctx,
        &NewGeneration {
            id: id.clone(),
            parent_id: None,
            cause: GenerationCause::BlockCompile,
            site_manifest_json: generation::canonical_text(&manifest.site).expect("canonical"),
            block_manifest_json: generation::canonical_text(&manifest.blocks).expect("canonical"),
            manifest_sha256: generation::manifest_sha256(&manifest).expect("hash"),
        },
    )
    .await
    .expect("stage");
    id
}

/// The status of `id`, read back through the API, or `None` once retention
/// has deleted the row — which the endpoint answers as a `404`.
///
/// Two requests: an `OutputStream` is consumed by reading it, so the status
/// code and the body cannot both come off one response.
async fn status_of_generation(ctx: &TestContext, id: &str) -> Option<String> {
    let path = format!("/b/dev/api/generations/{id}");
    if output_http_status(dev_get(ctx, &path).await).await == 404 {
        return None;
    }
    let detail = output_json(dev_get(ctx, &path).await).await;
    Some(
        detail["summary"]["status"]
            .as_str()
            .unwrap_or_else(|| panic!("no status for {id}: {detail}"))
            .to_string(),
    )
}

/// `GET /b/dev/api/generations?limit=n`, with the query where the HTTP
/// boundary puts it.
async fn list_generations(ctx: &TestContext, limit: Option<u32>) -> serde_json::Value {
    let mut msg = admin_msg("retrieve", "/b/dev/api/generations");
    if let Some(limit) = limit {
        msg.set_meta("req.query.limit", limit.to_string());
    }
    output_json(ctx.dispatch_resolved(msg).await).await
}

/// Write `site/index.html` `count` times from version `from`, chaining the
/// hashes, and return the last one.
async fn write_repeatedly(
    ctx: &TestContext,
    count: usize,
    from: usize,
    mut sha: Option<String>,
) -> Option<String> {
    for i in from..(from + count) {
        let w = write_file(ctx, "site/index.html", &format!("v{i}"), sha.as_deref()).await;
        sha = Some(w["sha256"].as_str().expect("sha256").to_string());
    }
    sha
}

/// Retention deletes the rows that fall out of the window — it does not
/// relabel them. Rewritten from the labelling version this replaces: nothing
/// rewrites a row's status because it got old, so the observable effect is a
/// row that is *gone*, and the one thing that must never go is the generation
/// that is serving.
#[tokio::test]
async fn only_the_last_twenty_generations_are_retained() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;

    // Ten writes, ten generations: everything is still inside the window.
    let sha = write_repeatedly(&ctx, 10, 0, None).await;
    let inside = list_generations(&ctx, Some(100)).await;
    let oldest = inside["generations"][9]["id"]
        .as_str()
        .expect("the tenth-newest id")
        .to_string();
    assert_eq!(
        status_of_generation(&ctx, &oldest).await.as_deref(),
        Some("superseded"),
        "a replaced generation is superseded by the activation that replaced it",
    );

    // 24 writes: the four oldest fall out of the window and are deleted.
    write_repeatedly(&ctx, 14, 10, sha).await;
    assert_eq!(
        status_of_generation(&ctx, &oldest).await,
        None,
        "a generation past the window is deleted, not relabelled",
    );

    let l = list_generations(&ctx, Some(100)).await;
    let statuses: Vec<&str> = l["generations"]
        .as_array()
        .expect("generations")
        .iter()
        .map(|g| g["status"].as_str().expect("status"))
        .collect();
    assert_eq!(
        statuses.len(),
        20,
        "24 published, 20 kept — the ledger is bounded: {statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == "active").count(),
        1,
        "exactly one generation is serving: {statuses:?}"
    );
    // The default page size is the retention window, so the default listing
    // is exactly the set that can still be rolled back to.
    assert_eq!(
        list_generations(&ctx, None).await["generations"]
            .as_array()
            .expect("generations")
            .len(),
        20
    );
}

/// A generation that never activated is a recovery target, not an old row: the
/// journal may name it and boot convergence re-runs it (design §7.3), so
/// retention keeps it however far down the ledger it falls.
#[tokio::test]
async fn a_staged_generation_survives_past_the_retention_window() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let staged = stage_only(&ctx).await;

    // 24 activations on top of it, so it is well outside the newest twenty.
    write_repeatedly(&ctx, 24, 0, None).await;

    assert_eq!(
        status_of_generation(&ctx, &staged).await.as_deref(),
        Some("staged"),
        "an in-flight generation is not retention's to delete",
    );
    // It is kept *in addition to* the window, not instead of one of its rows.
    assert_eq!(
        list_generations(&ctx, Some(100)).await["generations"]
            .as_array()
            .expect("generations")
            .len(),
        21,
    );
}

/// The oldest generation the window keeps has a parent that retention has
/// deleted. Its detail view still answers — a diff against nothing, exactly as
/// the first generation's is — rather than 404-ing over a row that is
/// perfectly readable.
#[tokio::test]
async fn the_oldest_retained_generation_reads_back_without_its_parent() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    write_repeatedly(&ctx, 24, 0, None).await;

    let listed = list_generations(&ctx, Some(100)).await;
    let oldest = &listed["generations"][19];
    let id = oldest["id"].as_str().expect("id");
    assert!(
        oldest["parent_id"].is_string(),
        "the fixture needs a generation whose parent is gone: {oldest}"
    );

    let detail = output_json(dev_get(&ctx, &format!("/b/dev/api/generations/{id}")).await).await;
    assert_eq!(detail["summary"]["id"], json!(id));
    assert_eq!(
        detail["diff_from_parent"]["added_paths"],
        json!(["index.html"]),
        "with no parent to diff against, the generation adds everything it holds: {detail}",
    );
}

// ---------------------------------------------------------------------------
// Coalescing
// ---------------------------------------------------------------------------

/// Requests that arrive while an activation is running collapse into one, and
/// every waiter resolves with the generation that carries its change.
///
/// Two writes to *different* paths, because the point is that both survive:
/// the queue keeps one pending slot, so a coalesced site publish has to be
/// composed from the persisted workspace rather than from whatever either
/// caller happened to be holding.
#[tokio::test]
async fn site_writes_that_arrive_during_an_activation_coalesce_and_both_publish() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    let shared = ctx.dev_shared();
    let compile = compile_of(&ctx, &["hello"]).await;

    // The driver parks inside `rebuild` — the one place an activation can be
    // held open — so the two writes are admitted while it is in flight. The
    // gate opens only once BOTH writes are folded into the pending slot:
    // `join!` polls in order, but a write path that yields on any of its
    // awaits hands control back before it has enqueued, and a gate released
    // on the first poll round then lets the driver finish ahead of the writes
    // — which is exactly the interleaving this test is not about.
    let release = control.gate_next_rebuild();
    let (driver, first, second, ()) = tokio::join!(
        activation::request(&ctx, &shared, GenerationCause::BlockCompile, compile),
        write_file(&ctx, "site/a.css", "a{}", None),
        write_file(&ctx, "site/b.css", "b{}", None),
        async {
            while shared.activation.pending_waiters() < 2 {
                tokio::task::yield_now().await;
            }
            let _ = release.send(());
        },
    );
    let driver = driver.expect("the driver activates its own intent");

    // Both writes resolved with the same generation — the one composed from
    // the workspace after both had saved — and it is not the driver's.
    assert_eq!(first["generation"]["id"], second["generation"]["id"]);
    assert_ne!(first["generation"]["id"], json!(driver.generation.id));
    assert_eq!(first["generation"]["site_files"], 2);
    assert_eq!(
        served(&ctx, "a.css").await.as_deref(),
        Some(&b"a{}"[..]),
        "the displaced writer's file must still be published"
    );
    assert_eq!(served(&ctx, "b.css").await.as_deref(), Some(&b"b{}"[..]));

    // Two publishes for three requests, and one rebuild: the site writes
    // changed no block set.
    let l = list_generations(&ctx, Some(100)).await;
    assert_eq!(l["generations"].as_array().expect("generations").len(), 2);
    assert_eq!(control.rebuilds().len(), 1);
}

/// The regression the intent design exists for: a site write admitted while a
/// block activation is in flight must not be composed against the *previous*
/// block set. Composed at request time it would carry the pre-compile blocks
/// and, at dequeue, rebuild the runtime back to them — tearing out the block
/// that had just been published.
#[tokio::test]
async fn a_site_write_during_a_block_activation_keeps_the_block_set() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    let shared = ctx.dev_shared();
    write_file(&ctx, "site/index.html", "v1", None).await;

    // One block is already live, so the in-flight activation is a change from
    // a non-empty set to a different non-empty set — a site write composed
    // from either would be wrong in a visible way.
    activation::request(
        &ctx,
        &shared,
        GenerationCause::BlockCompile,
        compile_of(&ctx, &["hello"]).await,
    )
    .await
    .expect("activate the first block");
    assert_eq!(active_block_names(&ctx).await, vec!["site/hello"]);

    let second = compile_of(&ctx, &["hello", "goodbye"]).await;
    let v1 = sha_of("v1");
    let release = control.gate_next_rebuild();
    let (compile, write, ()) = tokio::join!(
        activation::request(&ctx, &shared, GenerationCause::BlockCompile, second),
        write_file(&ctx, "site/index.html", "v2", Some(&v1)),
        async {
            let _ = release.send(());
        },
    );
    compile.expect("the second compile activates");

    // The generation the write published still carries both blocks...
    assert_eq!(write["generation"]["blocks"], 2);
    assert_eq!(
        active_block_names(&ctx).await,
        vec!["site/goodbye", "site/hello"]
    );
    // ...and the write itself rebuilt nothing: two compiles, two rebuilds.
    assert_eq!(control.rebuilds().len(), 2);
    assert_eq!(control.rebuilds()[1].len(), 2);
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v2"[..])
    );
}

/// Liveness: a driver that is cancelled mid-activation must release the queue
/// and fail the requests folded into it, not wedge every later request on a
/// oneshot nobody will send.
#[tokio::test]
async fn a_cancelled_driver_releases_the_queue_and_fails_its_waiters() {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context as TaskContext, Poll, Waker},
    };

    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    let shared = ctx.dev_shared();
    let compile = compile_of(&ctx, &["hello"]).await;

    // The gate's sender is held for the whole test, so the driver can never
    // finish: cancelling it is the only way it ends.
    let _never_released = control.gate_next_rebuild();
    let mut cx = TaskContext::from_waker(Waker::noop());

    let mut driver = Box::pin(activation::request(
        &ctx,
        &shared,
        GenerationCause::BlockCompile,
        compile,
    ));
    assert!(
        driver.as_mut().poll(&mut cx).is_pending(),
        "the driver must park inside rebuild"
    );

    let mut waiter = Box::pin(activation::request(
        &ctx,
        &shared,
        GenerationCause::SiteWrite,
        ActivationIntent::SiteOnly,
    ));
    assert!(
        waiter.as_mut().poll(&mut cx).is_pending(),
        "the second request must queue behind the driver"
    );

    // Cancel the driver.
    drop(driver);

    match Pin::new(&mut waiter).poll(&mut cx) {
        Poll::Ready(Err(ActivationError::Runtime(message))) => {
            assert!(message.contains("abandoned"), "{message}");
        }
        other => panic!("the orphaned waiter must be failed: {other:?}"),
    }

    // And the queue is usable again.
    let after = write_file(&ctx, "site/index.html", "v1", None).await;
    assert_eq!(after["generation"]["status"], "active");
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v1"[..])
    );
}

// ---------------------------------------------------------------------------
// The generations API
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_ledger_publishes_each_generation_with_its_manifest_and_diff() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let sha1 = g1["sha256"].as_str().expect("sha256").to_string();
    let g2 = write_file(&ctx, "site/index.html", "v2", Some(&sha1)).await;

    let id1 = g1["generation"]["id"].as_str().expect("id").to_string();
    let id2 = g2["generation"]["id"].as_str().expect("id").to_string();

    let l = list_generations(&ctx, Some(10)).await;
    let ids: Vec<&str> = l["generations"]
        .as_array()
        .expect("generations")
        .iter()
        .map(|g| g["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, vec![id2.as_str(), id1.as_str()], "newest first");
    assert_eq!(l["generations"][0]["status"], "active");
    assert_eq!(l["generations"][1]["status"], "superseded");
    assert_eq!(
        list_generations(&ctx, Some(1)).await["generations"]
            .as_array()
            .expect("generations")
            .len(),
        1
    );

    let first = output_json(dev_get(&ctx, &format!("/b/dev/api/generations/{id1}")).await).await;
    assert_eq!(first["summary"]["id"], json!(id1));
    assert_eq!(first["manifest"]["schema_version"], 1);
    assert_eq!(first["manifest"]["generation_id"], json!(id1));
    assert_eq!(first["manifest"]["site"]["files"][0]["path"], "index.html");
    assert_eq!(first["manifest"]["blocks"], json!([]));
    // No parent: the first generation adds everything it holds.
    assert_eq!(
        first["diff_from_parent"]["added_paths"],
        json!(["index.html"])
    );

    let second = output_json(dev_get(&ctx, &format!("/b/dev/api/generations/{id2}")).await).await;
    assert_eq!(second["summary"]["parent_id"], json!(id1));
    assert_eq!(
        second["diff_from_parent"]["changed_paths"],
        json!(["index.html"])
    );
    assert_eq!(second["diff_from_parent"]["added_paths"], json!([]));

    // The stored hash covers the manifest the API publishes.
    let manifest: GenerationManifest =
        serde_json::from_value(second["manifest"].clone()).expect("manifest round-trips");
    assert_eq!(
        generation::manifest_sha256(&manifest).expect("hash"),
        generations::get(&ctx, &id2)
            .await
            .expect("row")
            .manifest_sha256
    );

    assert_eq!(
        output_http_status(dev_get(&ctx, "/b/dev/api/generations/nope").await).await,
        404
    );
    assert_eq!(
        output_http_status(dev_post(&ctx, "/b/dev/api/generations/nope/rollback", json!({})).await)
            .await,
        404
    );
}

#[tokio::test]
async fn the_generations_api_is_admin_only() {
    let ctx = dev_with_accounts(FakeControl::new()).await;
    let member = signed_in_as(&ctx, "user").await;
    // No identity is told to sign in; a member is refused the admin tier.
    for (msg, status) in [
        (anon_msg("retrieve", "/b/dev/api/generations"), 401),
        (
            member.bearer(anon_msg("retrieve", "/b/dev/api/generations")),
            403,
        ),
        (anon_msg("retrieve", "/b/dev/api/generations/g1"), 401),
        (
            member.bearer(anon_msg("retrieve", "/b/dev/api/generations/g1")),
            403,
        ),
        (
            anon_msg("create", "/b/dev/api/generations/g1/rollback"),
            401,
        ),
        (
            member.bearer(anon_msg("create", "/b/dev/api/generations/g1/rollback")),
            403,
        ),
    ] {
        let path = msg.path().to_string();
        assert_eq!(
            output_http_status(ctx.request(msg).await).await,
            status,
            "{path}"
        );
    }
}

#[tokio::test]
async fn every_generations_response_is_never_cached() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let sha1 = g1["sha256"].as_str().expect("sha256").to_string();
    write_file(&ctx, "site/index.html", "v2", Some(&sha1)).await;
    blobs::delete(&ctx, &sha1).await.expect("collect the blob");
    let id1 = g1["generation"]["id"].as_str().expect("id").to_string();

    // One request per assertion: reading an `OutputStream` consumes it.
    for (label, out) in [
        (
            "a 200 listing",
            dev_get(&ctx, "/b/dev/api/generations").await,
        ),
        (
            "a 200 detail",
            dev_get(&ctx, &format!("/b/dev/api/generations/{id1}")).await,
        ),
        ("a 404", dev_get(&ctx, "/b/dev/api/generations/nope").await),
        (
            "a 422 refusal",
            dev_post(
                &ctx,
                &format!("/b/dev/api/generations/{id1}/rollback"),
                json!({}),
            )
            .await,
        ),
    ] {
        assert_eq!(
            output_http_header(out, "Cache-Control").await.as_deref(),
            Some("no-store"),
            "{label}"
        );
    }
}

/// The dev block's own handler for `msg`, run by a block the deployment
/// grants nothing.
///
/// The dev block itself always reaches its ledger — the table is in its own
/// namespace — so a refused read cannot be staged by routing to it: the
/// runtime would run the handler as `impresspress/dev`. A block with no
/// grant running the same handler is what makes `wrap::check_access` refuse.
async fn as_ungranted_block(
    ctx: &TestContext,
    msg: wafer_run::Message,
    body: serde_json::Value,
) -> wafer_run::OutputStream {
    let dev = ctx
        .blocks
        .lock()
        .expect("blocks mutex")
        .get(impresspress_core::blocks::dev::BLOCK_NAME)
        .cloned()
        .expect("the fixture registers the dev block");
    dev.handle(
        &ctx.clone().running_as("test/ungranted"),
        msg,
        wafer_run::InputStream::from_bytes(serde_json::to_vec(&body).expect("encode body")),
    )
    .await
}

/// The three ledger reads the two tests below refuse.
async fn denied_ledger_reads(ctx: &TestContext) -> Vec<(&'static str, wafer_run::OutputStream)> {
    vec![
        (
            "the listing",
            as_ungranted_block(
                ctx,
                admin_msg("retrieve", "/b/dev/api/generations"),
                json!({}),
            )
            .await,
        ),
        (
            "one generation",
            as_ungranted_block(
                ctx,
                admin_msg("retrieve", "/b/dev/api/generations/g1"),
                json!({}),
            )
            .await,
        ),
        (
            "a rollback",
            as_ungranted_block(
                ctx,
                admin_msg("create", "/b/dev/api/generations/g1/rollback"),
                json!({}),
            )
            .await,
        ),
    ]
}

/// A WRAP refusal reading the ledger is a **403**, not the 500 the
/// hand-written `NotFound`/`err_internal` pair produced.
///
/// The ledger is `impresspress__dev__generations`, so a caller reaching it
/// without a grant is refused by `wrap::check_access`. Both handlers
/// answered `no_store_error(NotFound, …)` for a missing generation and
/// `err_internal` for everything else, so a refused read looked like an
/// outage.
#[tokio::test]
async fn a_denied_generations_read_is_403_not_500() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    for (label, out) in denied_ledger_reads(&ctx).await {
        assert_eq!(output_http_status(out).await, 403, "{label}");
    }
}

/// …and the refusal still carries the block-wide `Cache-Control: no-store`,
/// which is why the conversion needed a no-store form of `crud::db_error`
/// rather than the plain one. Design §12 admits exactly one exception, the
/// sanitized 500 from `err_internal`; a 403 is not it.
#[tokio::test]
async fn a_denied_generations_read_is_still_never_cached() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    for (label, out) in denied_ledger_reads(&ctx).await {
        assert_eq!(
            output_http_header(out, "Cache-Control").await.as_deref(),
            Some("no-store"),
            "{label}"
        );
    }
}

/// The one failure that has to be unwound rather than merely refused: the
/// runtime has already been swapped when the publish fails (design §7.3).
#[tokio::test]
async fn a_publish_that_fails_after_the_swap_restores_the_previous_runtime_and_site() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let active = g1["generation"]["id"].as_str().expect("id").to_string();

    // The intent names its own site, different from what is published: a
    // publish only writes what changed, so an activation whose site matched
    // the live one would reach the commit without ever calling `put`.
    let site = site_of(&ctx, "v2").await;
    let intent = ActivationIntent::BlockSet {
        site: Some(site),
        blocks: vec![block_spec(&ctx, "hello").await],
    };
    ctx.fail_next_storage_put("disk is on fire");
    let err = activation::request(
        &ctx,
        &ctx.dev_shared(),
        GenerationCause::BlockCompile,
        intent,
    )
    .await
    .expect_err("a publish that fails must fail the activation");
    assert!(
        matches!(&err, ActivationError::Storage(m) if m.contains("disk is on fire")),
        "{err:?}"
    );
    assert_eq!(err.status(), 500);

    // The runtime was rebuilt with the new block set exactly once, and the
    // swap was then UNDONE — the retained runtime put back, not a second
    // runtime built from the previous block set (design §7.3 step 4).
    let rebuilds = control.rebuilds();
    assert_eq!(rebuilds.len(), 1, "the swap must not be redone");
    assert_eq!(rebuilds[0].len(), 1);
    assert_eq!(control.restores(), 1, "the swap must be unwound");
    assert_eq!(
        control.live_blocks(),
        None,
        "the runtime that was live before the rebuild is live again"
    );

    // The previous generation is still live, still serving its own content.
    let status = dev_status(&ctx).await;
    assert_eq!(status["active_generation"]["id"], json!(active));
    assert_eq!(status["activation"], serde_json::Value::Null);
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v1"[..])
    );

    let l = list_generations(&ctx, Some(10)).await;
    assert_eq!(l["generations"][0]["status"], "failed");
}

/// The same unwind with a block already live, which is what separates the two
/// possible implementations: a `rebuild(previous_blocks)` would show up as a
/// second forward rebuild and would produce a *different* runtime carrying the
/// same set, while restoring the retained handle shows up as a restore and
/// carries the runtime that was actually serving (design §7.3 step 4).
#[tokio::test]
async fn a_failed_publish_restores_the_retained_runtime_instead_of_rebuilding_it() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;

    let first = compile_of(&ctx, &["hello"]).await;
    activation::request(
        &ctx,
        &ctx.dev_shared(),
        GenerationCause::BlockCompile,
        first,
    )
    .await
    .expect("the first activation goes live");
    assert_eq!(control.rebuilds().len(), 1);

    // A second activation that adds a block and then fails while publishing.
    let site = site_of(&ctx, "v2").await;
    let intent = ActivationIntent::BlockSet {
        site: Some(site),
        blocks: vec![
            block_spec(&ctx, "hello").await,
            block_spec(&ctx, "second").await,
        ],
    };
    ctx.fail_next_storage_put("disk is on fire");
    activation::request(
        &ctx,
        &ctx.dev_shared(),
        GenerationCause::BlockCompile,
        intent,
    )
    .await
    .expect_err("a publish that fails must fail the activation");

    assert_eq!(
        control.rebuilds().len(),
        2,
        "the unwind must not be a third rebuild"
    );
    assert_eq!(control.restores(), 1, "the retained runtime went back");
    let live: Vec<String> = control
        .live_blocks()
        .expect("a block set is live")
        .iter()
        .map(|spec| spec.name.clone())
        .collect();
    assert_eq!(live, vec!["site/hello".to_string()]);
    assert_eq!(
        active_block_names(&ctx).await,
        vec!["site/hello".to_string()]
    );
}

/// A restore that itself fails is still reported — the sandbox says both what
/// went wrong and that it could not undo it, rather than claiming a clean
/// rollback it did not perform.
#[tokio::test]
async fn a_restore_that_fails_is_reported_alongside_the_publish_failure() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;

    let site = site_of(&ctx, "v2").await;
    let intent = ActivationIntent::BlockSet {
        site: Some(site),
        blocks: vec![block_spec(&ctx, "hello").await],
    };
    ctx.fail_next_storage_put("disk is on fire");
    control.fail_next_restore("the runtime slot is empty");
    let err = activation::request(
        &ctx,
        &ctx.dev_shared(),
        GenerationCause::BlockCompile,
        intent,
    )
    .await
    .expect_err("a publish that fails must fail the activation");

    let message = err.to_string();
    assert!(message.contains("disk is on fire"), "{message}");
    assert!(
        message.contains("restoring the previous runtime also failed")
            && message.contains("the runtime slot is empty"),
        "{message}"
    );
    assert_eq!(control.restores(), 0, "nothing was restored");
}

/// A journal whose `desired` already names the live generation must converge
/// to a no-op, not supersede the very generation it activates.
#[tokio::test]
async fn converging_on_the_generation_that_is_already_live_leaves_it_live() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let active = g1["generation"]["id"].as_str().expect("id").to_string();

    let state = runtime_state::read(&ctx).await.expect("read journal");
    runtime_state::write(
        &ctx,
        &RuntimeState {
            desired_generation_id: Some(active.clone()),
            activation_phase: ActivationPhase::Publishing,
            ..state
        },
    )
    .await
    .expect("journal");

    activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("converge");

    let after = runtime_state::read(&ctx).await.expect("read journal");
    assert_eq!(after.active_generation_id.as_deref(), Some(active.as_str()));
    assert_eq!(after.desired_generation_id, None);
    assert_eq!(
        status_of_generation(&ctx, &active).await.as_deref(),
        Some("active")
    );
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v1"[..])
    );
}

// ---------------------------------------------------------------------------
// A journal that points nowhere useful
// ---------------------------------------------------------------------------

/// Point the journal at `desired`, mid-publish, leaving `active` alone.
async fn journal_desired(ctx: &TestContext, desired: &str) {
    let state = runtime_state::read(ctx).await.expect("read journal");
    runtime_state::write(
        ctx,
        &RuntimeState {
            desired_generation_id: Some(desired.to_string()),
            activation_phase: ActivationPhase::Publishing,
            ..state
        },
    )
    .await
    .expect("journal");
}

/// The journal is persistent, so a `desired` that cannot be loaded would fail
/// every boot identically and the instance would never serve again. Boot has
/// to get past it.
#[tokio::test]
async fn boot_clears_a_journal_that_names_a_generation_that_is_gone() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let active = g1["generation"]["id"].as_str().expect("id").to_string();

    journal_desired(&ctx, "a-generation-that-never-existed").await;

    let blocks = activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("a dangling journal must not fail the boot");
    assert!(blocks.is_empty());

    let state = runtime_state::read(&ctx).await.expect("read journal");
    assert_eq!(state.desired_generation_id, None);
    assert_eq!(state.activation_phase, ActivationPhase::Idle);
    assert_eq!(state.active_generation_id.as_deref(), Some(active.as_str()));
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v1"[..])
    );

    // And it stays cleared: a second boot is an ordinary one.
    activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("the second boot has nothing owed");
    assert_eq!(
        status_of_generation(&ctx, &active).await.as_deref(),
        Some("active")
    );
}

/// The same, for a row that exists but cannot be read: its manifest columns do
/// not parse. Here there *is* a row, so the refusal is recorded on it.
#[tokio::test]
async fn boot_clears_a_journal_that_names_an_unreadable_generation() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let g1 = write_file(&ctx, "site/index.html", "v1", None).await;
    let active = g1["generation"]["id"].as_str().expect("id").to_string();

    let corrupt = repo::new_id();
    generations::insert(
        &ctx,
        &NewGeneration {
            id: corrupt.clone(),
            parent_id: Some(active.clone()),
            cause: GenerationCause::BlockCompile,
            site_manifest_json: r#"{"files":[]}"#.to_string(),
            // Written by a build this one cannot read.
            block_manifest_json: "not-json-at-all".to_string(),
            manifest_sha256: "cc".to_string(),
        },
    )
    .await
    .expect("stage a corrupt generation");
    journal_desired(&ctx, &corrupt).await;

    activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("an unreadable generation must not fail the boot");

    let state = runtime_state::read(&ctx).await.expect("read journal");
    assert_eq!(state.desired_generation_id, None);
    assert_eq!(state.activation_phase, ActivationPhase::Idle);
    assert_eq!(state.active_generation_id.as_deref(), Some(active.as_str()));
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v1"[..])
    );

    // The row that could not be read says so, rather than sitting `staged`
    // forever with nothing explaining why it never activated.
    let row = generations::get(&ctx, &corrupt).await.expect("row");
    assert_eq!(row.status, GenerationStatus::Failed);
    assert!(
        row.failure_message
            .as_deref()
            .is_some_and(|m| m.contains("block_manifest_json")),
        "{:?}",
        row.failure_message
    );
}

/// The symmetric hole. `desired` is not the only pointer the persistent
/// journal holds: an `active` that cannot be loaded would make *every* boot
/// return `Err` from here on, and the `/b/dev` page that could fix it is
/// served by the runtime that boot never builds.
#[tokio::test]
async fn boot_clears_an_active_pointer_to_a_generation_that_is_gone() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    write_file(&ctx, "site/index.html", "v1", None).await;

    let state = runtime_state::read(&ctx).await.expect("read journal");
    runtime_state::write(
        &ctx,
        &RuntimeState {
            active_generation_id: Some("a-generation-that-never-existed".to_string()),
            ..state
        },
    )
    .await
    .expect("journal");

    let blocks = activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("an unloadable active generation must not fail the boot");
    assert!(blocks.is_empty(), "nothing dynamic is served");

    let state = runtime_state::read(&ctx).await.expect("read journal");
    assert_eq!(state.active_generation_id, None);
    assert_eq!(state.desired_generation_id, None);
    assert_eq!(state.activation_phase, ActivationPhase::Idle);

    // The instance is usable again: the next write publishes on top of the
    // cleared journal instead of failing on the pointer.
    let next = write_file(&ctx, "site/index.html", "v2", Some(&sha_of("v1"))).await;
    assert_eq!(next["generation"]["status"], "active");
    assert_eq!(
        served(&ctx, "index.html").await.as_deref(),
        Some(&b"v2"[..])
    );
}

/// The same, for an `active` row that exists but whose manifest columns do not
/// parse. Here there is a row, so the refusal is recorded on it.
#[tokio::test]
async fn boot_clears_an_active_pointer_to_an_unreadable_generation() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;

    let corrupt = repo::new_id();
    generations::insert(
        &ctx,
        &NewGeneration {
            id: corrupt.clone(),
            parent_id: None,
            cause: GenerationCause::Seed,
            // Written by a build this one cannot read.
            site_manifest_json: "not-json-at-all".to_string(),
            block_manifest_json: "[]".to_string(),
            manifest_sha256: "cc".to_string(),
        },
    )
    .await
    .expect("stage a corrupt generation");
    let state = runtime_state::read(&ctx).await.expect("read journal");
    runtime_state::write(
        &ctx,
        &RuntimeState {
            active_generation_id: Some(corrupt.clone()),
            ..state
        },
    )
    .await
    .expect("journal");

    activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("an unreadable active generation must not fail the boot");

    let state = runtime_state::read(&ctx).await.expect("read journal");
    assert_eq!(state.active_generation_id, None);

    let row = generations::get(&ctx, &corrupt).await.expect("row");
    assert_eq!(row.status, GenerationStatus::Failed);
    assert!(
        row.failure_message
            .as_deref()
            .is_some_and(|m| m.contains("site_manifest_json")),
        "{:?}",
        row.failure_message
    );
}

/// Clearing an unreadable `active` must not throw away a `desired` that loads
/// perfectly well. The interrupted activation is the best answer available to
/// "what should this instance serve?", and converging on it is what leaves the
/// ledger and the published folder describing the same thing again.
#[tokio::test]
async fn an_unreadable_active_pointer_still_converges_on_a_loadable_desired() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;

    // A real, loadable generation, staged but never activated.
    let desired = repo::new_id();
    let site = serde_json::json!({"files": []}).to_string();
    generations::insert(
        &ctx,
        &NewGeneration {
            id: desired.clone(),
            parent_id: None,
            cause: GenerationCause::Seed,
            site_manifest_json: site,
            block_manifest_json: "[]".to_string(),
            manifest_sha256: "dd".to_string(),
        },
    )
    .await
    .expect("stage the desired generation");

    // ...behind an `active` pointer nothing can load.
    let state = runtime_state::read(&ctx).await.expect("read journal");
    runtime_state::write(
        &ctx,
        &RuntimeState {
            active_generation_id: Some("a-generation-that-never-existed".to_string()),
            desired_generation_id: Some(desired.clone()),
            activation_phase: ActivationPhase::Publishing,
            ..state
        },
    )
    .await
    .expect("journal");

    let blocks = activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("boot must not fail");
    assert!(blocks.is_empty());

    let state = runtime_state::read(&ctx).await.expect("read journal");
    assert_eq!(
        state.active_generation_id.as_deref(),
        Some(desired.as_str()),
        "the loadable desired generation is what the instance ends up serving"
    );
    assert_eq!(state.desired_generation_id, None);
    assert_eq!(state.activation_phase, ActivationPhase::Idle);
    assert_eq!(
        status_of_generation(&ctx, &desired).await.as_deref(),
        Some("active")
    );
}
