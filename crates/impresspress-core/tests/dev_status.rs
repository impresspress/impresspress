//! `GET /b/dev/api/status` — the sandbox control plane's read model.
//!
//! The whole file is gated on `block-dev`: the block does not exist in a
//! default-feature build, so these tests must not even compile there (that
//! absence is itself asserted by `cargo test -p impresspress-core` passing
//! with default features).
#![cfg(feature = "block-dev")]

use impresspress_core::{
    blocks::dev::{
        contracts::StatusResponse,
        control::{DynamicBlockSpec, DynamicRoute, RouteAccessKind},
        repo::{
            self,
            generations::{self, GenerationCause, GenerationStatus, NewGeneration},
            runtime_state::{self, ActivationPhase, RuntimeState},
        },
        test_support::{dev_status, dev_with_accounts, signed_in_as, FakeControl, FakeShell},
        DevBlock, DevShared, RuntimeControl, ROUTES, WAFER_GUEST_VERSION,
    },
    test_support::{
        admin_msg, anon_msg, output_http_header, output_http_status, output_status, TestContext,
    },
};
use wafer_run::Block as _;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The one site file the fixture generation publishes, in the canonical form
/// design §11.3 mandates (sorted keys, no whitespace — the bytes
/// `manifest_sha256` is a hash over).
const SITE_MANIFEST: &str = r#"{"files":[{"content_type":"text/html; charset=utf-8","path":"index.html","sha256":"aa","size":5}]}"#;

/// The fixture's block manifest, built from the real [`DynamicBlockSpec`]
/// rather than a hand-written string.
///
/// That is the load-bearing part: the manifest's block entry and the spec the
/// runtime is rebuilt from are declared to be the *same* type, so a test that
/// hand-wrote the JSON could not catch the two drifting apart.
fn block_manifest() -> String {
    let specs = vec![DynamicBlockSpec {
        name: "site/newsletter".to_string(),
        artifact_sha256: "bb".to_string(),
        routes: vec![DynamicRoute {
            prefix: "/b/newsletter/".to_string(),
            access: RouteAccessKind::Public,
        }],
        capabilities: wafer_block::BlockCapabilities::default(),
        wafer_guest_version: WAFER_GUEST_VERSION,
    }];
    // Through `Value` so object keys come out sorted — canonical JSON, as the
    // stored manifest must be.
    serde_json::to_value(&specs)
        .expect("serialize block manifest")
        .to_string()
}

/// A context whose journal points at one `Active` generation carrying one site
/// file and one block, with `control` already having rebuilt `rebuilds` times.
async fn active_generation_ctx(rebuilds: u64) -> (TestContext, String) {
    let control = FakeControl::new();
    for _ in 0..rebuilds {
        control.rebuild(&[]).await.expect("rebuild");
    }

    let ctx = TestContext::with_dev(control).await;
    let generation = generations::insert(
        &ctx,
        &NewGeneration {
            id: repo::new_id(),
            parent_id: None,
            cause: GenerationCause::BlockCompile,
            site_manifest_json: SITE_MANIFEST.to_string(),
            block_manifest_json: block_manifest(),
            manifest_sha256: "cc".to_string(),
        },
    )
    .await
    .expect("insert generation");

    generations::set_status(
        &ctx,
        &generation.id,
        GenerationStatus::Active,
        None,
        Some("2026-09-03T00:00:00Z"),
    )
    .await
    .expect("activate generation");

    runtime_state::write(
        &ctx,
        &RuntimeState {
            active_generation_id: Some(generation.id.clone()),
            desired_generation_id: None,
            activation_phase: ActivationPhase::Idle,
            generation: 3,
        },
    )
    .await
    .expect("journal");

    (ctx, generation.id)
}

// ---------------------------------------------------------------------------
// Fresh instance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_reports_no_generation_on_a_fresh_instance() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let out = ctx
        .dispatch_resolved(admin_msg("retrieve", "/b/dev/api/status"))
        .await;
    assert_eq!(output_status(out).await, 200);

    let body = dev_status(&ctx).await;
    assert_eq!(body["active_generation"], serde_json::Value::Null);
    assert_eq!(body["runtime_generation"], 0);
    assert_eq!(body["blocks"], serde_json::json!([]));
    assert_eq!(body["activation"], serde_json::Value::Null);
    assert_eq!(body["wafer_guest_version"], WAFER_GUEST_VERSION);
}

// ---------------------------------------------------------------------------
// The non-empty read path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_projects_the_active_generation_and_its_blocks() {
    let (ctx, generation_id) = active_generation_ctx(0).await;
    let body = dev_status(&ctx).await;

    let active = &body["active_generation"];
    assert_eq!(active["id"], serde_json::json!(generation_id));
    assert_eq!(active["parent_id"], serde_json::Value::Null);
    assert_eq!(active["cause"], "block_compile");
    assert_eq!(active["status"], "active");
    assert_eq!(active["activated_at"], "2026-09-03T00:00:00Z");
    // Both counts are decoded from the stored manifests, not from a column —
    // reading either with `str_field` instead of `repo::json_text` yields an
    // empty string on SQLite and fails the whole request.
    assert_eq!(active["site_files"], 1);
    assert_eq!(active["blocks"], 1);

    assert_eq!(body["blocks"].as_array().expect("blocks array").len(), 1);
    let block = &body["blocks"][0];
    assert_eq!(block["name"], "site/newsletter");
    assert_eq!(block["artifact_sha256"], "bb");
    assert_eq!(block["routes"][0]["prefix"], "/b/newsletter/");
    assert_eq!(block["routes"][0]["access"], "Public");

    // `ActiveBlockView` is a deliberate projection of `DynamicBlockSpec`: the
    // capabilities a guest runs under and its guest-ABI version are internal
    // and must not reach an HTTP caller.
    let fields: Vec<&String> = block.as_object().expect("block object").keys().collect();
    assert_eq!(fields, vec!["artifact_sha256", "name", "routes"]);

    // The whole body must also satisfy the published contract, which is
    // `deny_unknown_fields` — so this fails if a field is renamed or added
    // without the snapshot being regenerated.
    let typed: StatusResponse = serde_json::from_value(body).expect("body matches StatusResponse");
    assert_eq!(typed.blocks.len(), 1);
    assert!(typed.activation.is_none());
}

#[tokio::test]
async fn status_reports_an_activation_in_flight_from_the_journal() {
    let (ctx, generation_id) = active_generation_ctx(0).await;
    runtime_state::write(
        &ctx,
        &RuntimeState {
            active_generation_id: Some(generation_id.clone()),
            desired_generation_id: Some("gen-next".to_string()),
            activation_phase: ActivationPhase::BuildingRuntime,
            generation: 4,
        },
    )
    .await
    .expect("journal");

    let body = dev_status(&ctx).await;
    assert_eq!(body["activation"]["generation_id"], "gen-next");
    assert_eq!(body["activation"]["phase"], "building_runtime");
    // The previous generation is still what is serving until the swap lands.
    assert_eq!(
        body["active_generation"]["id"],
        serde_json::json!(generation_id)
    );
}

/// `runtime_generation` is the *control's* counter, not the journal's column.
/// The journal is written with `generation: 3` above precisely so a handler
/// that read the row instead of asking the runtime would report 3 here.
#[tokio::test]
async fn status_reports_the_runtimes_generation_not_the_journals() {
    let (ctx, _) = active_generation_ctx(2).await;
    let body = dev_status(&ctx).await;
    assert_eq!(body["runtime_generation"], 2);
    assert_eq!(body["wafer_guest_version"], WAFER_GUEST_VERSION);
}

#[tokio::test]
async fn status_follows_the_runtime_generation_as_it_is_bumped() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    assert_eq!(dev_status(&ctx).await["runtime_generation"], 0);

    control.rebuild(&[]).await.expect("rebuild");
    assert_eq!(dev_status(&ctx).await["runtime_generation"], 1);

    control.rebuild(&[]).await.expect("rebuild");
    assert_eq!(dev_status(&ctx).await["runtime_generation"], 2);
    assert_eq!(control.rebuilds().len(), 2);
}

// ---------------------------------------------------------------------------
// Access and caching
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_is_admin_only() {
    let ctx = dev_with_accounts(FakeControl::new()).await;
    let member = signed_in_as(&ctx, "user").await;
    assert_eq!(
        output_http_status(ctx.request(anon_msg("retrieve", "/b/dev/api/status")).await).await,
        401
    );
    assert_eq!(
        output_http_status(
            ctx.request(member.bearer(anon_msg("retrieve", "/b/dev/api/status")))
                .await
        )
        .await,
        403
    );
}

/// Design §12: *every* `/b/dev` response is `no-store` — the 404 as much as
/// the 200. A cacheable 404 under this prefix would let a stale negative
/// answer outlive the generation that changed it.
#[tokio::test]
async fn every_response_is_never_cached() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;

    // An `OutputStream` is consumed by reading it, so each assertion needs
    // its own request. The status is asserted alongside the header so the
    // header claim cannot be satisfied by an unexpected response shape.
    for (path, expected_status) in [("/b/dev/api/status", 200), ("/b/dev/nope", 404)] {
        assert_eq!(
            output_http_status(ctx.dispatch_resolved(admin_msg("retrieve", path)).await).await,
            expected_status,
            "{path}"
        );
        assert_eq!(
            output_http_header(
                ctx.dispatch_resolved(admin_msg("retrieve", path)).await,
                "Cache-Control"
            )
            .await
            .as_deref(),
            Some("no-store"),
            "{path} must be no-store"
        );
    }
}

// ---------------------------------------------------------------------------
// Declaration integrity
// ---------------------------------------------------------------------------

/// The dispatch table and the declared endpoint list must be the *same* set,
/// in both directions.
///
/// A one-way check only catches half the drift: a route with no declared
/// endpoint falls back to the router's fail-closed tier and never gets an
/// `AuthLevel`, and a declared endpoint with no route is published in
/// `/openapi.json` (and registered as an agent tool) while 404-ing.
#[test]
fn routes_and_endpoints_stay_in_lockstep() {
    let info = DevBlock::with_workspace(DevShared::new(
        FakeControl::new(),
        std::sync::Arc::new(FakeShell::new()),
    ))
    .info();

    assert_eq!(ROUTES.len(), info.endpoints.len());

    for route in ROUTES {
        assert!(
            info.endpoints
                .iter()
                .any(|e| e.method == route.method && e.path == route.template),
            "route missing from BlockInfo: {:?} {}",
            route.method,
            route.template,
        );
    }
    for endpoint in &info.endpoints {
        assert!(
            ROUTES
                .iter()
                .any(|r| r.method == endpoint.method && r.template == endpoint.path),
            "declared endpoint has no dispatch route: {:?} {}",
            endpoint.method,
            endpoint.path,
        );
    }

    let mut seen: Vec<(wafer_run::HttpMethod, &str)> =
        ROUTES.iter().map(|r| (r.method, r.template)).collect();
    let before = seen.len();
    seen.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    seen.dedup();
    assert_eq!(before, seen.len(), "duplicate route in the dispatch table");

    // Every `/b/dev` route is Admin (design §13); the router is the sole gate,
    // so a route that slipped in at a weaker tier would be reachable.
    for endpoint in &info.endpoints {
        assert_eq!(
            endpoint.auth,
            wafer_run::AuthLevel::Admin,
            "{} must be Admin",
            endpoint.path
        );
        assert!(
            endpoint.path.starts_with("/b/dev"),
            "{} escapes the block's prefix",
            endpoint.path
        );
    }
}

/// The same invariant one level up: an EXPORTED bundle registers this block
/// without routing it, so it must declare no HTTP surface at all.
///
/// `routes_and_endpoints_stay_in_lockstep` above compares the dispatch table
/// against the declarations and cannot see this — in `Exported` the dispatch
/// table is still there, it is the *route* that is absent, and a `BlockInfo`
/// is published for every registered block whether or not one exists. A
/// declared endpoint would then be a 404 in `/openapi.json`, and `admin_url`
/// an "Open" button on `/b/admin/blocks` pointing at a page the exported site
/// does not serve.
#[test]
fn an_exported_bundle_declares_no_surface_it_does_not_route() {
    let shared = DevShared::new(FakeControl::new(), std::sync::Arc::new(FakeShell::new()));
    let exported = DevBlock::runtime_only(std::sync::Arc::clone(&shared)).info();

    assert!(
        exported.endpoints.is_empty(),
        "an unrouted block must publish no endpoints: {:?}",
        exported
            .endpoints
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        exported.admin_url.is_empty(),
        "an unrouted block must offer no admin link: {}",
        exported.admin_url
    );

    // Everything that is true in both modes stays: the block still owns its
    // ledger tables, and its migrations still have to run.
    let workspace = DevBlock::with_workspace(shared).info();
    assert_eq!(exported.name, workspace.name);
    assert_eq!(exported.collections.len(), workspace.collections.len());
    assert!(!exported.collections.is_empty());
    assert!(!exported.can_disable);
    assert!(!workspace.endpoints.is_empty());
    assert!(!workspace.admin_url.is_empty());
}
