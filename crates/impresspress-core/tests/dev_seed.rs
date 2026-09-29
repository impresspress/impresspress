//! Seed-on-boot: importing generation 0 from a bundle the origin serves.
//!
//! Gated on `block-dev` for the same reason `dev_activation.rs` is: the block
//! does not exist in a default-feature build, so these tests must not compile
//! there.
//!
//! The fetch seam is what makes this host-testable at all — the real source is
//! the service worker's own `fetch`, and `MapFetch` (from `dev::test_support`)
//! is the same contract backed by a `BTreeMap`, shared with
//! `tests/dev_data_snapshot.rs`.
#![cfg(feature = "block-dev")]

use std::{collections::BTreeSet, sync::Arc};

use impresspress_core::{
    blocks::dev::{
        activation::{self, ActivationIntent},
        artifacts, blobs,
        control::{DynamicBlockSpec, DynamicRoute, RouteAccessKind},
        repo::{self, generations::GenerationCause, runtime_state},
        seed::{self, SeedBlock, SeedManifest},
        test_support::{hello_info, seed_file as file, FakeControl, MapFetch},
        validation, workspace,
    },
    test_support::TestContext,
};
use wafer_block::{Allowlist, BlockCapabilities};
use wafer_core::clients::database as db;
use wafer_run::{AuthLevel, BlockEndpoint, BlockInfo};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const INDEX: &[u8] = b"<h1>welcome</h1>";
const APP_JS: &[u8] = b"console.log('hi');\n";
const LIB_RS: &[u8] = b"// a guest\n";
const ARTIFACT: &[u8] = b"\0asm\x01\0\0\0";

fn hello_spec() -> DynamicBlockSpec {
    DynamicBlockSpec {
        name: "site/hello".to_string(),
        artifact_sha256: blobs::sha256_hex(ARTIFACT),
        routes: vec![DynamicRoute {
            prefix: "/b/hello/".to_string(),
            access: RouteAccessKind::Public,
        }],
        capabilities: wafer_block::BlockCapabilities::none(),
        wafer_guest_version: 0,
    }
}

/// A control whose `inspect` reports the shared `hello` guest — what the
/// bundle fixture below describes.
///
/// A seed import runs the four rules that read the guest's own report (name,
/// endpoints inside the route prefix, agent tool names, capabilities against
/// `requires`), so a fixture whose control reported nothing useful would
/// refuse every bundle below for a reason none of them is about. The declared
/// spec has to be exactly what those rules produce from `hello_info`, which is
/// why the endpoint is under `/b/hello/` and the name is `site/hello`.
fn hello_control() -> Arc<FakeControl> {
    let control = FakeControl::new();
    control.set_validated_info(hello_info("site/hello"));
    control
}

/// The standard fixture: a dev context and the control it was built over.
///
/// Both halves are needed because `seed::import` takes the control
/// explicitly — it is the runtime seam, and the context has no way to hand
/// one back. Binding them here keeps a test from importing through a control
/// that is not the one the block would rebuild through.
async fn fixture() -> (TestContext, Arc<FakeControl>) {
    let control = hello_control();
    let ctx = TestContext::with_dev(control.clone()).await;
    (ctx, control)
}

/// A two-file site plus one block with one source file.
fn manifest() -> SeedManifest {
    SeedManifest {
        schema_version: seed::SCHEMA_VERSION,
        source_generation: Some("gen-from-elsewhere".to_string()),
        site: vec![file("index.html", INDEX), file("assets/app.js", APP_JS)],
        blocks: vec![SeedBlock {
            spec: hello_spec(),
            sources: vec![file("src/lib.rs", LIB_RS)],
        }],
        data: None,
    }
}

fn bundle() -> MapFetch {
    MapFetch::default()
        .with(&seed::site_url("index.html"), INDEX)
        .with(&seed::site_url("assets/app.js"), APP_JS)
        .with(&seed::artifact_url("hello"), ARTIFACT)
        .with(&seed::source_url("hello", "src/lib.rs"), LIB_RS)
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_seed_bundle_becomes_the_workspace_and_generation_zero() {
    let (ctx, control) = fixture().await;

    let generation = seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect("import")
        .expect("a fresh instance imports");

    // The workspace holds every file, at its workspace-relative path.
    let ws = workspace::load(&ctx).await.expect("workspace");
    let paths: Vec<&str> = ws.files.keys().map(String::as_str).collect();
    assert_eq!(
        paths,
        vec![
            "blocks/hello/src/lib.rs",
            "site/assets/app.js",
            "site/index.html",
        ]
    );
    // Content types are derived by the workspace, not copied from the bundle.
    assert_eq!(
        ws.get("site/index.html").expect("entry").content_type,
        "text/html; charset=utf-8"
    );
    assert_eq!(ws.blob_count, 3);
    assert_eq!(
        ws.blob_bytes,
        (INDEX.len() + APP_JS.len() + LIB_RS.len()) as u64
    );

    // Every blob and the artifact are stored under the hash the manifest named.
    assert_eq!(
        blobs::get(&ctx, &blobs::sha256_hex(INDEX))
            .await
            .expect("blob"),
        INDEX.to_vec()
    );
    assert_eq!(
        artifacts::get(&ctx, &blobs::sha256_hex(ARTIFACT))
            .await
            .expect("artifact"),
        ARTIFACT.to_vec()
    );

    // The returned manifest is staged — the site half is the workspace's
    // `site/` entries with the prefix stripped, the block half is the spec.
    assert_eq!(generation.generation_id, "");
    assert_eq!(generation.parent_id, None);
    let site: Vec<&str> = generation
        .site
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(site, vec!["assets/app.js", "index.html"]);
    assert_eq!(generation.blocks.len(), 1);
    assert_eq!(generation.blocks[0].name, "site/hello");

    // A seeded artifact is recorded in the builds table like a compiled one.
    // That table is the index `dev_status` reports the artifact store from and
    // the one the collector deletes a row from when it reclaims the bytes, so
    // an artifact missing from it is an artifact the sandbox cannot see.
    let build = repo::builds::latest_valid_for_artifact(&ctx, &blobs::sha256_hex(ARTIFACT))
        .await
        .expect("lookup")
        .expect("the seeded artifact has an accepted build row");
    assert_eq!(build.block_name, "site/hello");
    assert_eq!(build.artifact_bytes, ARTIFACT.len() as u64);
    // Never left staged: `staged` is the collector's "a compile is still
    // coming" marker, and none is.
    assert!(repo::builds::list_in_flight(&ctx)
        .await
        .expect("list")
        .is_empty());
}

/// The whole point of the import: the manifest it returns activates through
/// the ordinary queue as generation 0, publishing the site.
#[tokio::test]
async fn the_imported_generation_activates_as_generation_zero() {
    let (ctx, control) = fixture().await;
    let shared = ctx.dev_shared();

    let generation = seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect("import")
        .expect("fresh");
    let outcome = activation::request(
        &ctx,
        &shared,
        GenerationCause::Seed,
        ActivationIntent::Seed {
            manifest: generation,
        },
    )
    .await
    .expect("activate the seed");

    assert_eq!(
        outcome.generation.parent_id, None,
        "generation 0 has no parent"
    );
    assert_eq!(outcome.generation.site_files, 2);
    assert_eq!(outcome.generation.blocks, 1);
    assert_eq!(
        ctx.storage_get("wafer-run/web", "site", "index.html")
            .await
            .expect("published index"),
        INDEX.to_vec()
    );
    // The block set reached the host exactly once, as the runtime rebuild.
    let rebuilds = control.rebuilds();
    assert_eq!(rebuilds.len(), 1);
    assert_eq!(rebuilds[0].len(), 1);
    assert_eq!(rebuilds[0][0].name, "site/hello");

    // And the instance is no longer fresh, so a re-import is a no-op.
    assert!(!seed::is_fresh(&ctx).await.expect("is_fresh"));
    assert!(seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect("second import")
        .is_none());
}

// ---------------------------------------------------------------------------
// Freshness
// ---------------------------------------------------------------------------

/// A seed must never overwrite a workspace someone has edited. An instance
/// that has published anything is not fresh, and the import is a no-op — not
/// an error, because this runs on every boot.
#[tokio::test]
async fn a_second_import_on_a_non_fresh_instance_is_a_no_op() {
    let (ctx, control) = fixture().await;
    seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect("first import")
        .expect("fresh");
    // The first import wrote the workspace but staged nothing, so freshness
    // is still decided by the ledger — publish, then re-check.
    activation::request(
        &ctx,
        &ctx.dev_shared(),
        GenerationCause::SiteWrite,
        ActivationIntent::SiteOnly,
    )
    .await
    .expect("publish");

    assert!(seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect("second import")
        .is_none());
}

/// A generation that only ever *failed* still means "something has been staged
/// here", so the instance is not fresh — re-seeding over a workspace whose
/// first publish failed would delete whatever the user wrote next.
#[tokio::test]
async fn a_failed_generation_still_makes_an_instance_non_fresh() {
    let (ctx, control) = fixture().await;
    repo::generations::insert(
        &ctx,
        &repo::generations::NewGeneration {
            id: repo::new_id(),
            parent_id: None,
            cause: GenerationCause::SiteWrite,
            site_manifest_json: r#"{"files":[]}"#.to_string(),
            block_manifest_json: "[]".to_string(),
            manifest_sha256: "aa".to_string(),
        },
    )
    .await
    .expect("stage");

    assert_eq!(
        runtime_state::read(&ctx)
            .await
            .expect("journal")
            .active_generation_id,
        None,
        "nothing is active — freshness must not rest on that alone"
    );
    assert!(!seed::is_fresh(&ctx).await.expect("is_fresh"));
    assert!(seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect("import")
        .is_none());
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn content_that_does_not_match_its_declared_hash_is_refused() {
    let (ctx, control) = fixture().await;
    let tampered = bundle().with(&seed::site_url("assets/app.js"), b"alert('gotcha')");

    let error = seed::import(&ctx, control.as_ref(), &manifest(), &tampered)
        .await
        .expect_err("a hash mismatch must refuse the import");
    assert!(
        error.contains("assets/app.js") && error.contains("hashes to"),
        "{error}"
    );
    assert!(
        workspace::load(&ctx)
            .await
            .expect("workspace")
            .files
            .is_empty(),
        "a refused import leaves no workspace behind"
    );
}

#[tokio::test]
async fn an_artifact_that_does_not_match_its_declared_hash_is_refused() {
    let (ctx, control) = fixture().await;
    let tampered = bundle().with(&seed::artifact_url("hello"), b"\0asm\x01\0\0\0different");

    let error = seed::import(&ctx, control.as_ref(), &manifest(), &tampered)
        .await
        .expect_err("a hash mismatch must refuse the import");
    assert!(
        error.contains("/seed/blocks/hello.wasm") && error.contains("site/hello"),
        "{error}"
    );
}

/// The artifact size limit exists to bound what the artifact store holds, and
/// a bundle that arrived over the network is no more entitled to exceed it
/// than one that arrived over `POST /b/dev/api/builds/stage`.
#[tokio::test]
async fn an_oversized_artifact_is_refused_and_never_stored() {
    let (ctx, control) = fixture().await;
    let huge = vec![0u8; validation::MAX_ARTIFACT_BYTES + 1];
    let mut manifest = manifest();
    manifest.blocks[0].spec.artifact_sha256 = blobs::sha256_hex(&huge);
    let bundle = bundle().with(&seed::artifact_url("hello"), &huge);

    let error = seed::import(&ctx, control.as_ref(), &manifest, &bundle)
        .await
        .expect_err("an oversized artifact must refuse the import");
    assert!(
        error.contains("artifact is at least") && error.contains("/seed/blocks/hello.wasm"),
        "{error}"
    );
    // Refused beside the hash check and before the `put`: the store never saw
    // it, and neither did the workspace.
    assert!(!artifacts::exists(&ctx, &blobs::sha256_hex(&huge))
        .await
        .expect("exists"));
    assert!(workspace::load(&ctx)
        .await
        .expect("workspace")
        .files
        .is_empty());
}

#[tokio::test]
async fn a_size_that_does_not_match_the_content_is_refused() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    manifest.site[0].size += 1;

    let error = seed::import(&ctx, control.as_ref(), &manifest, &bundle())
        .await
        .expect_err("a size mismatch must refuse the import");
    assert!(
        error.contains("index.html") && error.contains("bytes"),
        "{error}"
    );
}

/// The served type is derived from the path, so a bundle claiming a different
/// one was produced by an exporter that does not agree with this build about
/// how the file is served.
#[tokio::test]
async fn a_content_type_that_is_not_what_the_path_is_served_as_is_refused() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    manifest.site[0].content_type = "text/plain".to_string();

    let error = seed::import(&ctx, control.as_ref(), &manifest, &bundle())
        .await
        .expect_err("a content-type mismatch must refuse the import");
    assert!(error.contains("content type"), "{error}");
}

/// The path check runs before the fetch, so a traversing entry is refused for
/// *being* a traversal rather than merely for being unfetchable — the bundle
/// here carries the file, so the only thing that can refuse it is the rule.
#[tokio::test]
async fn a_path_that_escapes_the_workspace_is_refused() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    manifest.site[0].path = "../../elsewhere.html".to_string();
    let bundle = bundle().with(&seed::site_url("../../elsewhere.html"), INDEX);

    let error = seed::import(&ctx, control.as_ref(), &manifest, &bundle)
        .await
        .expect_err("a traversing path must refuse the import");
    assert!(
        error.contains("site/../../elsewhere.html") && error.contains(r#"segment "..""#),
        "{error}"
    );
}

#[tokio::test]
async fn a_file_the_bundle_does_not_carry_is_refused() {
    let (ctx, control) = fixture().await;
    // Everything the manifest names except one site file. Built up rather
    // than derived from `bundle()` because the point is precisely which entry
    // is missing: the artifacts are fetched and inspected before any site
    // file is stored, so a bundle missing the artifact would be refused for
    // the artifact and this test would no longer be about a site file at all.
    let incomplete = MapFetch::default()
        .with(&seed::site_url("index.html"), INDEX)
        .with(&seed::artifact_url("hello"), ARTIFACT)
        .with(&seed::source_url("hello", "src/lib.rs"), LIB_RS);

    let error = seed::import(&ctx, control.as_ref(), &manifest(), &incomplete)
        .await
        .expect_err("a missing file must refuse the import");
    assert!(error.contains("assets/app.js"), "{error}");
}

/// The other half of the same rule, now that the artifact is fetched first: a
/// bundle whose manifest names a block but does not carry its module is
/// refused for the module, by name.
#[tokio::test]
async fn a_block_artifact_the_bundle_does_not_carry_is_refused() {
    let (ctx, control) = fixture().await;
    let incomplete = MapFetch::default()
        .with(&seed::site_url("index.html"), INDEX)
        .with(&seed::site_url("assets/app.js"), APP_JS);

    let error = seed::import(&ctx, control.as_ref(), &manifest(), &incomplete)
        .await
        .expect_err("a missing artifact must refuse the import");
    assert!(error.contains("/seed/blocks/hello.wasm"), "{error}");
    // And nothing was stored: the artifacts are fetched before the first
    // site file lands in the workspace.
    assert!(workspace::load(&ctx)
        .await
        .expect("workspace")
        .files
        .is_empty());
}

#[tokio::test]
async fn a_bundle_from_another_schema_version_is_refused() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    manifest.schema_version = seed::SCHEMA_VERSION + 1;

    let error = seed::import(&ctx, control.as_ref(), &manifest, &bundle())
        .await
        .expect_err("an unknown schema version must refuse the import");
    assert!(error.contains("schema_version"), "{error}");
}

/// wafer-run refuses `_` in a block-name segment, so a bundle carrying one
/// describes a block this runtime could never register. The bundle here is
/// complete under the offending name, so the *only* thing that can refuse it
/// is the name rule.
#[tokio::test]
async fn a_block_name_that_cannot_be_registered_is_refused() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    manifest.blocks[0].spec.name = "site/my_shop".to_string();
    let bundle = bundle()
        .with(&seed::artifact_url("my_shop"), ARTIFACT)
        .with(&seed::source_url("my_shop", "src/lib.rs"), LIB_RS);

    let error = seed::import(&ctx, control.as_ref(), &manifest, &bundle)
        .await
        .expect_err("an unregisterable block name must refuse the import");
    assert!(
        error.contains("site/my_shop") && error.contains("cannot be registered"),
        "{error}"
    );
    assert!(error.contains("name-format"), "{error}");
}

/// A seed's capabilities are granted verbatim by `load_guest`, so §6.5 has to
/// hold on this entry point exactly as it does on staging. `/seed/` is a
/// service-worker *bypass* prefix and §10.1 makes exports deliberately
/// re-importable, so the bundle is not necessarily one this instance wrote.
#[tokio::test]
async fn a_seeded_block_reaching_outside_its_namespace_is_refused() {
    for (label, capabilities, code) in [
        (
            "a collection in someone else's namespace",
            BlockCapabilities {
                collections: Allowlist::Only(BTreeSet::from([
                    "impresspress__auth__users".to_string()
                ])),
                ..BlockCapabilities::none()
            },
            "cap-collection",
        ),
        (
            "raw SQL",
            BlockCapabilities {
                raw_sql: true,
                ..BlockCapabilities::none()
            },
            "cap-raw-sql",
        ),
        (
            "an unrestricted storage allowlist",
            BlockCapabilities {
                storage_folders: Allowlist::Any,
                ..BlockCapabilities::none()
            },
            "cap-folder",
        ),
        (
            "network access",
            BlockCapabilities {
                network: Allowlist::Only(BTreeSet::from(["evil.example".to_string()])),
                ..BlockCapabilities::none()
            },
            "cap-network",
        ),
    ] {
        let (ctx, control) = fixture().await;
        let mut manifest = manifest();
        manifest.blocks[0].spec.capabilities = capabilities;

        let Err(error) = seed::import(&ctx, control.as_ref(), &manifest, &bundle()).await else {
            panic!("{label} must refuse the import");
        };
        assert!(error.contains("site/hello"), "{label}: {error}");
        assert!(error.contains(code), "{label}: {error}");

        // Refused before anything was fetched: the workspace must not carry
        // half a bundle whose block was never going to be admitted.
        assert!(
            workspace::load(&ctx)
                .await
                .expect("workspace")
                .files
                .is_empty(),
            "{label}: nothing may be stored by a refused import"
        );
    }
}

/// The route half of the same gate: a seeded spec may not claim a prefix its
/// name does not produce, and two seeded blocks may not claim one prefix.
#[tokio::test]
async fn a_seeded_block_claiming_someone_elses_route_is_refused() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    manifest.blocks[0].spec.routes = vec![DynamicRoute {
        prefix: "/admin/".to_string(),
        access: RouteAccessKind::Public,
    }];

    let error = seed::import(&ctx, control.as_ref(), &manifest, &bundle())
        .await
        .expect_err("a route claim outside the block's own prefix must refuse the import");
    assert!(error.contains("route-prefix"), "{error}");
}

/// Two paths carrying the same asset cost one blob and are charged once — the
/// same rule the files API follows, and the one the workspace quota rests on.
#[tokio::test]
async fn identical_content_at_two_paths_is_stored_once() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    manifest.site.push(file("copy.html", INDEX));
    let bundle = bundle().with(&seed::site_url("copy.html"), INDEX);

    seed::import(&ctx, control.as_ref(), &manifest, &bundle)
        .await
        .expect("import")
        .expect("fresh");

    let ws = workspace::load(&ctx).await.expect("workspace");
    assert_eq!(ws.files.len(), 4);
    assert_eq!(ws.blob_count, 3, "the duplicate is not a second blob");
    assert_eq!(
        ws.blob_bytes,
        (INDEX.len() + APP_JS.len() + LIB_RS.len()) as u64
    );
}

/// The URL layout is design §10.1's, and both halves of the bundle read it
/// from here — a layout spelled at two sites is one that can be spelled two
/// different ways.
#[test]
fn the_bundle_layout_is_stated_once() {
    assert_eq!(seed::MANIFEST_URL, "/seed/manifest.json");
    assert_eq!(seed::site_url("assets/app.js"), "/seed/site/assets/app.js");
    assert_eq!(seed::artifact_url("hello"), "/seed/blocks/hello.wasm");
    assert_eq!(
        seed::source_url("hello", "src/lib.rs"),
        "/seed/blocks/hello/src/lib.rs"
    );
    assert_eq!(seed::short_name("site/hello"), "hello");
    assert_eq!(seed::short_name("hello"), "hello");
}

// ---------------------------------------------------------------------------
// The rules that need the guest's own report
// ---------------------------------------------------------------------------

/// A seeded artifact is INSPECTED, and the four rules that read the guest's
/// own `BlockInfo` are applied to what it reports.
///
/// The bundle here is otherwise perfect — the manifest declares `site/hello`,
/// the artifact hashes as declared, every file is carried — and the only
/// thing wrong is that the module calls itself something else. Nothing in
/// `validate_spec` can see that: the declared spec is self-consistent, and
/// the mismatch exists only between the manifest and the module. Before the
/// importer had a `RuntimeControl` this bundle imported cleanly and the
/// runtime then registered a guest under a name it does not answer to.
#[tokio::test]
async fn a_seeded_block_whose_module_reports_another_name_is_refused() {
    let control = FakeControl::new();
    control.set_validated_info(BlockInfo::new(
        "site/imposter",
        "0.1.0",
        "http-handler@v1",
        "not hello",
    ));
    let ctx = TestContext::with_dev(control.clone()).await;

    let error = seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect_err("a guest that reports another name must refuse the import");
    assert!(error.contains("name-mismatch"), "{error}");
    assert!(error.contains("site/imposter"), "{error}");
    // Refused before anything was stored: the artifacts are fetched and
    // inspected ahead of the first workspace write.
    assert!(workspace::load(&ctx)
        .await
        .expect("workspace")
        .files
        .is_empty());
    assert!(!artifacts::exists(&ctx, &blobs::sha256_hex(ARTIFACT))
        .await
        .expect("exists"));
}

/// The endpoint rule, from the same report: a guest may only declare
/// endpoints under the one prefix its name produces.
#[tokio::test]
async fn a_seeded_block_declaring_an_endpoint_outside_its_prefix_is_refused() {
    let control = FakeControl::new();
    control.set_validated_info(
        BlockInfo::new("site/hello", "0.1.0", "http-handler@v1", "hello").endpoints(vec![
            BlockEndpoint::get("/b/admin/api/users")
                .auth(AuthLevel::Public)
                .summary("not mine"),
        ]),
    );
    let ctx = TestContext::with_dev(control.clone()).await;

    let error = seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect_err("an endpoint outside the block's prefix must refuse the import");
    assert!(error.contains("endpoint-outside-routes"), "{error}");
}

/// The manifest is a description of its own artifact, not a second, separate
/// grant. A bundle whose spec asks for capabilities the module does not
/// declare is refused even though BOTH halves would pass their own rules in
/// isolation — `collections: Only(["site__hello__notes"])` is inside the
/// block's own namespace, so `validate_spec` accepts it; the module simply
/// never asked for it.
#[tokio::test]
async fn a_seeded_spec_that_grants_more_than_the_module_asks_for_is_refused() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    manifest.blocks[0].spec.capabilities = BlockCapabilities {
        collections: Allowlist::Only(BTreeSet::from(["site__hello__notes".to_string()])),
        ..BlockCapabilities::none()
    };

    let error = seed::import(&ctx, control.as_ref(), &manifest, &bundle())
        .await
        .expect_err("a spec the module does not report must refuse the import");
    assert!(
        error.contains("does not report") && error.contains("site/hello"),
        "{error}"
    );
    assert!(error.contains("site__hello__notes"), "{error}");
}

/// The other direction of the same equality: the module declares a
/// capability the manifest does not carry. Refused too — the check is that
/// the two describe one block, not only that the manifest asks for no more.
#[tokio::test]
async fn a_module_that_asks_for_more_than_the_seeded_spec_is_refused() {
    let mut info = hello_info("site/hello");
    info.capabilities = Some(BlockCapabilities {
        collections: Allowlist::Only(BTreeSet::from(["site__hello__notes".to_string()])),
        ..BlockCapabilities::none()
    });
    let control = FakeControl::new();
    control.set_validated_info(info);
    let ctx = TestContext::with_dev(control.clone()).await;

    let error = seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect_err("a module declaring more than its spec must refuse the import");
    assert!(
        error.contains("does not report") && error.contains("site/hello"),
        "{error}"
    );
    assert!(error.contains("site__hello__notes"), "{error}");
}

/// A bundle exported with a hyphenated block that still claims the hyphen
/// spelling (`site__my-shop__*`) is refused on import: that spelling is read
/// and written as `site__myshop__*`, another block's tables.
#[tokio::test]
async fn a_seeded_hyphenated_block_claiming_the_hyphen_spelling_is_refused() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    let spec = &mut manifest.blocks[0].spec;
    spec.name = "site/my-shop".to_string();
    spec.routes[0].prefix = "/b/my-shop/".to_string();
    spec.capabilities = BlockCapabilities {
        collections: Allowlist::Only(BTreeSet::from(["site__my-shop__notes".to_string()])),
        ..BlockCapabilities::none()
    };
    let bundle = bundle()
        .with(&seed::artifact_url("my-shop"), ARTIFACT)
        .with(&seed::source_url("my-shop", "src/lib.rs"), LIB_RS);

    let error = seed::import(&ctx, control.as_ref(), &manifest, &bundle)
        .await
        .expect_err("the hyphen spelling must refuse the import");
    assert!(error.contains("cap-collection"), "{error}");
    assert!(error.contains("site__my-shop__notes"), "{error}");
}

/// The seeded build row records the guest's own `BlockInfo`, exactly as
/// `blocks_api::stage` does.
///
/// Not bookkeeping: `blocks_api::claimed_tool_names` reads the stored
/// `BlockInfo` of every block in the active generation and refuses the whole
/// stage when one cannot be read. A `"null"` here — which is what a seed
/// wrote before the importer had a control — meant a seeded instance could
/// never compile a block of its own. `tests/dev_blocks.rs` drives that end to
/// end; this pins the row the rule reads.
#[tokio::test]
async fn a_seeded_build_row_records_the_block_info_the_guest_reported() {
    let (ctx, control) = fixture().await;

    seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect("import")
        .expect("fresh");

    let build = repo::builds::latest_valid_for_artifact(&ctx, &blobs::sha256_hex(ARTIFACT))
        .await
        .expect("lookup")
        .expect("row");
    let stored: BlockInfo =
        serde_json::from_str(&build.block_info_json).expect("the row carries a readable BlockInfo");
    assert_eq!(stored.name, "site/hello");
    assert_eq!(stored.endpoints.len(), 1);
    assert_eq!(stored.endpoints[0].path, "/b/hello/");
    // And it was read out of the module rather than invented: the fixture's
    // control was asked once per seeded block.
    assert_eq!(control.inspections(), 1);
}

/// The staging path refuses a module built against a different
/// `wafer_guest.rs` before it executes it (`blocks_api::handle_stage`), and a
/// seed is the other way a module reaches this runtime. The failure the gate
/// prevents is a trap inside wasmi during the boot activation — the one boot
/// that can least explain itself — and §10.1 makes exports deliberately
/// re-importable by someone who did not write them, so the two paths have to
/// apply the same rule.
#[tokio::test]
async fn a_block_built_against_a_different_guest_version_is_refused() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    manifest.blocks[0].spec.wafer_guest_version = 99;

    let error = seed::import(&ctx, control.as_ref(), &manifest, &bundle())
        .await
        .expect_err("a stale guest module must refuse the import");
    // The staging path's own diagnostic, code and remedy included.
    assert!(error.contains("wafer-guest-version"), "{error}");
    assert!(error.contains("version 99"), "{error}");
    assert!(error.contains("site/hello"), "{error}");
    // Refused before anything was fetched, let alone stored: the version is
    // knowable from the manifest.
    assert_eq!(control.inspections(), 0);
    assert!(workspace::load(&ctx)
        .await
        .expect("workspace")
        .files
        .is_empty());
}

/// Zero is "no version reported" on both paths — a compiler that could not
/// read the file records `0`, and so does a bundle exported before the field
/// existed. Neither is a mismatch, and refusing them would make every such
/// bundle unimportable.
#[tokio::test]
async fn a_block_that_reports_no_guest_version_still_imports() {
    let (ctx, control) = fixture().await;
    let mut manifest = manifest();
    manifest.blocks[0].spec.wafer_guest_version = 0;

    seed::import(&ctx, control.as_ref(), &manifest, &bundle())
        .await
        .expect("a bundle that reports no version must still import")
        .expect("fresh");
}

// ---------------------------------------------------------------------------
// A refused import, where the site's own admin can see it
// ---------------------------------------------------------------------------

/// The variables row the refusal wrote, if there is one.
async fn recorded_seed_error_row(
    ctx: &TestContext,
) -> Option<wafer_core::clients::database::Record> {
    let rows = db::list_all(ctx, "impresspress__admin__variables", vec![])
        .await
        .expect("list variables");
    rows.into_iter()
        .find(|row| row.data.get("key").and_then(|v| v.as_str()) == Some(seed::SEED_ERROR_KEY))
}

/// The message the variables row carries, if there is one.
async fn recorded_seed_error(ctx: &TestContext) -> Option<String> {
    recorded_seed_error_row(ctx).await.map(|row| {
        row.data
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    })
}

/// A refused seed leaves an empty site and, until this, nothing but a console
/// line to say why — which an EXPORTED bundle's owner cannot reach at all:
/// there is no `/b/dev` there. The admin variables page is the surface every
/// instance has, so the refusal is written where it shows up.
#[tokio::test]
async fn a_refused_import_records_its_reason_for_the_admin() {
    let (ctx, control) = fixture().await;
    let tampered = bundle().with(&seed::site_url("assets/app.js"), b"alert('gotcha')");

    let error = seed::import(&ctx, control.as_ref(), &manifest(), &tampered)
        .await
        .expect_err("a hash mismatch must refuse the import");

    let row = recorded_seed_error_row(&ctx)
        .await
        .expect("the refusal is recorded in the admin variables table");
    assert_eq!(
        row.data.get("value").and_then(|v| v.as_str()),
        Some(error.as_str()),
        "the row carries the refusal verbatim"
    );
    // Addressable, or the page that exists to show this cannot edit or delete
    // it: `id` is the table's PRIMARY KEY and `db::upsert` writes `data`
    // verbatim, so the caller has to supply one. SQLite takes a NULL there
    // without complaint, which is exactly why this is asserted rather than
    // left to the backend.
    assert!(
        row.data
            .get("id")
            .and_then(|v| v.as_str())
            .is_some_and(|id| !id.is_empty()),
        "the recorded refusal must carry a primary key: {:?}",
        row.data.get("id")
    );
    // …and the status endpoint reads the same row, so the workspace half sees
    // it without an admin having to go looking.
    assert_eq!(
        seed::last_failure(&ctx).await.expect("read"),
        Some(error.clone())
    );
    // Never exportable: the key is `IMPRESSPRESS_`-prefixed, so one instance's
    // seed failure can never travel into another instance's bundle.
    assert!(
        !impresspress_core::blocks::dev::data_snapshot::variable_is_exportable(
            &serde_json::json!({ "key": seed::SEED_ERROR_KEY, "sensitive": 0 })
                .as_object()
                .expect("object")
                .clone()
        )
    );
}

/// An instance that is not fresh makes no attempt, so it records nothing —
/// and an import that works clears what an earlier refusal left, or a fixed
/// bundle would boot into a stale complaint.
#[tokio::test]
async fn an_import_that_works_clears_an_earlier_refusal() {
    let (ctx, control) = fixture().await;
    seed::import(
        &ctx,
        control.as_ref(),
        &manifest(),
        &bundle().with(&seed::site_url("assets/app.js"), b"alert('gotcha')"),
    )
    .await
    .expect_err("the tampered bundle is refused");
    assert!(recorded_seed_error(&ctx).await.is_some());

    seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect("the intact bundle imports")
        .expect("still fresh — the refusal stored nothing");

    assert_eq!(recorded_seed_error(&ctx).await, None);
    assert_eq!(seed::last_failure(&ctx).await.expect("read"), None);
}
