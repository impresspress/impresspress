//! The runtime half of `impresspress/dev` on its own: a build with
//! `site-runtime` and without `block-dev`.
//!
//! That build is the half a site exported from the sandbox needs — the
//! generation ledger, seed import with its `data.json`, activation and the
//! published site — and none of the workspace: no `/b/dev` page, no JSON
//! API, no export. These tests prove both halves of that sentence on the
//! build itself. The seed path is driven the way the browser runtime drives
//! it on a cold boot (`impresspress-web`'s `seed_on_boot`: `seed::is_fresh`,
//! `seed::import`, then `activation::request` with the `Seed` intent), and
//! the absence of the workspace is checked where a caller would meet it: the
//! block's declaration, the published `/openapi.json`, the router, the
//! block's own `handle`, and the seed import, which refuses a sandbox's seed
//! by name.
//!
//! The gate is the point of the file, not a convenience: `cargo test
//! --workspace` turns `block-dev` on for this crate (the `impresspress`
//! crate's dev-dependency asks for it, and features unify), so these tests
//! compile only in their own `-p impresspress-core --features
//! site-runtime,test-support` invocation. CI runs that invocation and fails
//! when it reports no passing test, so this file cannot be compiled out
//! silently.
#![cfg(all(feature = "site-runtime", not(feature = "block-dev")))]

use std::sync::Arc;

use impresspress_core::{
    blocks::dev::{
        self,
        activation::{self, ActivationIntent, Maintenance},
        blobs,
        control::{DynamicBlockSpec, DynamicRoute, RouteAccessKind},
        data_snapshot::{self, DataSnapshot},
        repo::generations::GenerationCause,
        seed::{self, SandboxSeed, SeedBlock, SeedManifest},
        test_support::{fake_bypass_rules, hello_info, seed_file, FakeControl, MapFetch},
        DevBlock,
    },
    test_support::{admin_msg, anon_msg, openapi_document, output_http_status, TestContext},
    util::json_map,
};
use serde_json::json;
use wafer_core::clients::database as db;
use wafer_run::Block as _;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// `impresspress-core` keeps the products table name `pub(crate)`; restated
/// here as `tests/dev_data_snapshot.rs` does.
const PRODUCTS_TABLE: &str = "impresspress__products__products";

const INDEX: &[u8] = b"<h1>the shop</h1>";
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

/// One product row, as an export writes it: `db::upsert` (the path the
/// products table's `Mode::Upsert` writes through) inserts exactly what it is
/// given, so the timestamps a real exported row carries are here too.
fn data_json() -> Vec<u8> {
    let mut tables = std::collections::BTreeMap::new();
    tables.insert(
        PRODUCTS_TABLE.to_string(),
        vec![json_map(json!({
            "id": "prod_seeded",
            "name": "Seeded Widget",
            "status": "active",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }))
        .into_iter()
        .collect()],
    );
    serde_json::to_vec(&DataSnapshot {
        schema_version: data_snapshot::SCHEMA_VERSION,
        tables,
    })
    .expect("serialize the snapshot")
}

/// A seed with one site file, one compiled block and a `data.json` — every
/// kind of content an export carries — and the bundle that serves it.
fn seed_bundle() -> (SeedManifest, MapFetch) {
    let data = data_json();
    let manifest = SeedManifest {
        schema_version: seed::SCHEMA_VERSION,
        source_generation: Some("gen-exported".to_string()),
        site: vec![seed_file("index.html", INDEX)],
        blocks: vec![SeedBlock {
            spec: hello_spec(),
            sources: vec![seed_file("src/lib.rs", LIB_RS)],
        }],
        data: Some(seed_file("data.json", &data)),
        sandbox: None,
    };
    let fetch = MapFetch::default()
        .with(&seed::site_url("index.html"), INDEX)
        .with(&seed::artifact_url("hello"), ARTIFACT)
        .with(&seed::source_url("hello", "src/lib.rs"), LIB_RS)
        .with(&seed::data_url("data.json"), &data);
    (manifest, fetch)
}

/// A site-runtime fixture with the products block's tables (the seed's
/// `data.json` writes one), and the control it was built over — whose
/// `inspect` reports the `hello` guest the seed carries, so the import's
/// static rules accept it.
async fn fixture() -> (TestContext, Arc<FakeControl>) {
    let control = FakeControl::new();
    control.set_validated_info(hello_info("site/hello"));
    let ctx = TestContext::with_products()
        .await
        .with_site_runtime_added(control.clone())
        .await;
    (ctx, control)
}

/// A [`seed::SeedFetch`] that serves no file and counts every request.
#[derive(Default)]
struct CountingFetch(std::sync::atomic::AtomicUsize);

impl CountingFetch {
    fn asked(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl seed::SeedFetch for CountingFetch {
    fn get<'a>(&'a self, url: &'a str) -> seed::FetchFuture<'a> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { Err(format!("{url}: this bundle serves nothing")) })
    }
}

// ---------------------------------------------------------------------------
// What the runtime half does
// ---------------------------------------------------------------------------

/// A cold boot, as the browser runtime runs it: the seed is imported, its
/// generation activates — the block set is built and the site published —
/// and its data lands, all without a line of the workspace compiled in.
#[tokio::test]
async fn a_seed_boots_into_a_live_site_with_its_block_and_data() {
    let (ctx, control) = fixture().await;
    let (manifest, fetch) = seed_bundle();
    assert!(seed::is_fresh(&ctx).await.expect("read the ledger"));

    let generation = seed::import(
        &ctx,
        control.as_ref(),
        &fake_bypass_rules(),
        &manifest,
        &fetch,
    )
    .await
    .expect("import")
    .expect("a fresh instance imports");
    let outcome = activation::request(
        &ctx,
        &ctx.dev_shared(),
        GenerationCause::Seed,
        ActivationIntent::Seed {
            manifest: generation,
        },
        Maintenance::Inline,
    )
    .await
    .expect("activate the seed");

    assert_eq!(outcome.generation.site_files, Some(1));
    assert_eq!(outcome.generation.blocks, Some(1));
    // The runtime was rebuilt with the seeded block, through the host seam.
    assert_eq!(control.live_blocks(), Some(vec![hello_spec()]));
    // The site is in the folder `wafer-run/web` serves `/` from.
    assert_eq!(
        ctx.storage_get("wafer-run/web", "site", "index.html")
            .await
            .expect("the published index"),
        INDEX.to_vec()
    );
    // The seed's data is in the products block's own table.
    let products = db::list_all(&ctx.fixture(), PRODUCTS_TABLE, Vec::new())
        .await
        .expect("list products");
    assert_eq!(
        products.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ["prod_seeded"]
    );
    // And the instance is no longer fresh: the next boot converges instead of
    // importing again.
    assert!(!seed::is_fresh(&ctx).await.expect("read the ledger"));
}

// ---------------------------------------------------------------------------
// What the runtime half does not have
// ---------------------------------------------------------------------------

/// The block a site build registers declares no HTTP surface: no endpoint,
/// no admin page, and a summary of what it is in this build.
#[tokio::test]
async fn the_block_declares_no_workspace_surface() {
    let info = DevBlock::new().info();

    assert_eq!(info.name, dev::BLOCK_NAME);
    assert!(
        info.endpoints.is_empty(),
        "{:?}",
        info.endpoints.iter().map(|e| &e.path).collect::<Vec<_>>()
    );
    assert!(info.admin_url.is_empty(), "{}", info.admin_url);
    assert_eq!(info.summary, "Site generation ledger and seed import");
    // What is true in both builds stays: it owns its ledger tables, and an
    // admin cannot switch it off.
    assert!(!info.collections.is_empty());
    assert!(!info.can_disable);
}

/// `/openapi.json` — built from the blocks this build registers, the dev
/// block among them — publishes no `/b/dev` path.
#[tokio::test]
async fn openapi_publishes_no_workspace_path() {
    let (ctx, _control) = fixture().await;
    let document = openapi_document(&ctx).await;
    let paths = document["paths"].as_object().expect("an openapi paths map");
    assert!(!paths.is_empty(), "the document lists no path at all");
    let dev_paths: Vec<&String> = paths
        .keys()
        .filter(|path| path.starts_with(dev::ROUTE_PREFIX))
        .collect();
    assert!(dev_paths.is_empty(), "{dev_paths:?}");
}

/// No workspace path answers: not through the router, for an admin or an
/// anonymous visitor, and not from the block itself if something hands it
/// the request directly. The entry page — the one `Public` row of a
/// workspace, which hands out a session — is among them.
///
/// The two router legs show only that this fixture registered no route
/// (`TestContext::with_site_runtime_added` passes none); they do not test
/// production's wiring, which is `impresspress-web`'s `runtime_factory` and
/// is pinned by that crate's const assertions. The direct leg is the one
/// that tests this build: its `handle` has no dispatch table to look a path
/// up in.
#[tokio::test]
async fn no_workspace_path_answers() {
    let (ctx, _control) = fixture().await;
    let block = DevBlock::new();
    for path in [
        "/b/dev",
        "/b/dev/enter",
        "/b/dev/api/status",
        "/b/dev/api/files",
        "/b/dev/api/export",
    ] {
        let as_admin = ctx.dispatch_resolved(admin_msg("retrieve", path)).await;
        assert_eq!(output_http_status(as_admin).await, 404, "admin {path}");

        let anonymous = ctx.request(anon_msg("retrieve", path)).await;
        assert_eq!(output_http_status(anonymous).await, 404, "anonymous {path}");

        let direct = block
            .handle(
                &ctx,
                admin_msg("retrieve", path),
                wafer_run::InputStream::empty(),
            )
            .await;
        assert_eq!(output_http_status(direct).await, 404, "direct {path}");
    }
}

/// A seed that carries a `sandbox` section (a sandbox's guide and
/// `llms.txt`) is refused by name on a build without the workspace, before
/// anything is fetched, inspected or stored — not imported with the section dropped,
/// and not refused as serde's "unknown field" (decision D2). The refusal is
/// recorded where the site's admin can read it, like any other, and the
/// instance stays fresh.
#[tokio::test]
async fn a_seed_with_a_sandbox_section_is_refused_naming_it() {
    let (ctx, control) = fixture().await;
    let (mut manifest, _bundle) = seed_bundle();
    manifest.sandbox = Some(SandboxSeed {
        template: "blank".to_string(),
        suggested_prompt: "Build me a shop.".to_string(),
        guide: seed_file("guide.md", b"# Guide\n"),
        llms: seed_file(seed::LLMS_PATH, b"# The sandbox\n"),
    });
    // A bundle that serves nothing and counts what it is asked for: any fetch
    // before the refusal would be counted, and would fail the import with a
    // different message than the refusal's.
    let fetch = CountingFetch::default();

    let refused = seed::import(
        &ctx,
        control.as_ref(),
        &fake_bypass_rules(),
        &manifest,
        &fetch,
    )
    .await
    .expect_err("a sandbox's seed is refused");
    assert_eq!(refused, seed::SANDBOX_SEED_REFUSAL);
    assert_eq!(fetch.asked(), 0, "nothing is fetched before the refusal");
    assert!(
        refused.contains("`sandbox` section") && refused.contains("development sandbox runtime"),
        "{refused}"
    );

    // Nor was the artifact inspected or anything stored.
    assert_eq!(control.inspections(), 0);
    assert!(ctx
        .storage_get("wafer-run/web", "site", "index.html")
        .await
        .is_err());
    assert!(seed::is_fresh(&ctx).await.expect("read the ledger"));
    // Recorded for the site's admin, as every refused import is.
    let rows = db::list_all(&ctx.fixture(), "impresspress__admin__variables", Vec::new())
        .await
        .expect("list variables");
    let recorded = rows
        .iter()
        .find(|row| row.data.get("key").and_then(|v| v.as_str()) == Some(seed::SEED_ERROR_KEY))
        .and_then(|row| row.data.get("value").and_then(|v| v.as_str()));
    assert_eq!(recorded, Some(seed::SANDBOX_SEED_REFUSAL));
}
