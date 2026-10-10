//! `GET /b/dev/api/export` and `GET /b/dev/api/export/manifest` — the sandbox
//! as a folder anyone can serve.
//!
//! Gated on `block-dev` like every other dev-sandbox integration test: the
//! block does not exist in a default-feature build.
//!
//! # What is under test
//!
//! Three claims, and the third is the one the design rests on:
//!
//!  1. the archive carries the runtime shell **with development mode off**,
//!     the site, every compiled block with its source, and a data snapshot;
//!  2. the manifest endpoint describes exactly that archive without producing
//!     it;
//!  3. the `seed/` half of the archive is not an export format at all — it is
//!     the SAME format `seed::import` reads on a cold boot, so an export from
//!     one instance boots as generation 0 in another, shop and all.
//!
//! The archive is read back with the real `zip` crate (a dev-dependency),
//! never with the writer's own parser: `blocks::dev::zip` writing something
//! only it can read would satisfy a round trip through itself and nothing
//! else.
#![cfg(feature = "block-dev")]

use std::{collections::HashMap, io::Read as _};

use impresspress_core::{
    blocks::dev::{
        activation::{self, ActivationIntent},
        blobs,
        contracts::ExportManifest,
        data_snapshot::DataSnapshot,
        export, generation,
        repo::{
            generations::{self, GenerationCause},
            seed_info::{self, SeedInfo},
        },
        seed::{self, SeedManifest},
        stored_types,
        test_support::{dev_get, dev_post, fake_bypass_rules, hello_info, FakeControl, FakeShell},
        workspace, BypassRules, DevShared, WAFER_GUEST_VERSION,
    },
    platform_state::variables,
    test_support::{
        admin_msg, anon_msg, output_body, output_http_header, output_http_status, output_json,
        TestContext,
    },
};
use serde_json::json;
use wafer_core::clients::database as db;

// ---------------------------------------------------------------------------
// Table names this crate keeps private, restated here
// ---------------------------------------------------------------------------
//
// Same reason `tests/dev_data_snapshot.rs` restates them: `impresspress-core`
// keeps the products table names `pub(crate)` (and that block's own door
// tests refuse a re-export reachable from outside `repo::products`), and this
// is a separate compilation unit.

const PRODUCTS_TABLE: &str = "impresspress__products__products";

/// A minimal wasm header. Nothing here parses it; the bytes only have to be
/// stable so their sha256 is.
const ARTIFACT: &[u8] = b"\0asm\x01\0\0\0";

/// The page the agent wrote.
const SHOP_HTML: &[u8] = b"<h1>shop</h1>";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Standard base64 with padding — how an artifact travels in JSON.
fn b64(bytes: &[u8]) -> String {
    use base64ct::{Base64, Encoding as _};
    Base64::encode_string(bytes)
}

/// Stock the shop the way an agent does — through the products admin API,
/// not by writing rows.
///
/// The difference is not cosmetic. A hand-seeded row carries the handful of
/// columns the test bothered to name; a real create/price/publish leaves rows
/// across `products`, `offers` and `offer_components` with every column the
/// schema declares, populated the way production populates them. The data
/// snapshot exports THOSE, and a round trip that only ever moved a bare
/// product row would pass while a real export failed to import — which is
/// exactly what happened: the browser's export carried a real offer and its
/// component, and importing them was where it broke.
async fn seed_shop(ctx: &TestContext) {
    let product = output_json(
        ctx.dispatch_resolved_json(
            admin_msg("create", "/b/products/api/admin/products"),
            &json!({
                "name": "Custom print",
                "slug": "custom-print",
                "description": "Made to order, priced by the page.",
                "currency": "nzd",
                "fulfillment_kind": "manual",
            }),
        )
        .await,
    )
    .await;
    let product_id = product["id"]
        .as_str()
        .unwrap_or_else(|| panic!("create product: {product}"))
        .to_string();

    // A components offer with one typed input, the same shape
    // `tests/e2e/fixtures/shop-fixture.ts` uses — a flat price would exercise
    // neither `offer_components` nor the typed-variable columns.
    let offer = output_json(
        ctx.dispatch_resolved_json(
            admin_msg(
                "create",
                &format!("/b/products/api/admin/products/{product_id}/offers"),
            ),
            &json!({
                "name": "Custom print",
                "mode": "payment",
                "currency": "nzd",
                "pricing_model": "components",
                "usage_type": "licensed",
                "billing_scheme": "per_unit",
                "tax_behavior": "exclusive",
                "variables": [{
                    "key": "pages", "kind": "integer", "label": "Pages",
                    "required": true, "minimum": "1", "maximum": "20",
                    "step": "1", "sort_order": 0,
                }],
                "components": [{
                    "key": "pages", "label": "Printed pages", "sort_order": 0,
                    "required": true,
                    "amount": { "type": "per_unit", "input": "pages", "unit_amount_minor": 1500 },
                }],
                "checkout": {},
            }),
        )
        .await,
    )
    .await;
    let offer_id = offer["offer"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("create offer: {offer}"))
        .to_string();

    let published = output_json(
        ctx.dispatch_resolved_json(
            admin_msg(
                "create",
                &format!("/b/products/api/admin/products/{product_id}/offers/{offer_id}/publish"),
            ),
            &json!({}),
        )
        .await,
    )
    .await;
    assert_eq!(published["status"], "active", "{published}");

    let live = output_json(
        ctx.dispatch_resolved_json(
            admin_msg(
                "update",
                &format!("/b/products/api/admin/products/{product_id}"),
            ),
            &json!({ "status": "active" }),
        )
        .await,
    )
    .await;
    assert_eq!(live["status"], "active", "{live}");
}

/// Every entry of an archive, by path.
fn entries(bytes: Vec<u8>) -> HashMap<String, Vec<u8>> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("the export is a readable zip");
    let mut out = HashMap::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).expect("entry");
        let name = file.name().to_string();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("read entry");
        out.insert(name, buf);
    }
    out
}

/// One entry's content as text.
fn text(entries: &HashMap<String, Vec<u8>>, path: &str) -> String {
    let bytes = entries
        .get(path)
        .unwrap_or_else(|| panic!("{path} is not in the archive: {:?}", sorted(entries)));
    String::from_utf8(bytes.clone()).unwrap_or_else(|_| panic!("{path} is not utf8"))
}

fn sorted(entries: &HashMap<String, Vec<u8>>) -> Vec<String> {
    let mut names: Vec<String> = entries.keys().cloned().collect();
    names.sort();
    names
}

/// A sandbox with the products block, a shop page, one compiled block and one
/// product — the state the scenario in design §16 leaves behind.
async fn shop_instance(control: &std::sync::Arc<FakeControl>) -> TestContext {
    shop_instance_with_shell(control, std::sync::Arc::new(FakeShell::new())).await
}

/// [`shop_instance`] over a shell the caller keeps a handle to.
async fn shop_instance_with_shell(
    control: &std::sync::Arc<FakeControl>,
    shell: std::sync::Arc<FakeShell>,
) -> TestContext {
    control.set_validated_info(hello_info("site/hello"));
    // `with_auth_added`: the data snapshot's allowlist spans products, admin
    // AND auth (`users`, `local_credentials`, `user_roles` — the visitor's own
    // accounts, `Mode::Replace`d as a set). A fixture without auth's tables
    // exercises the export and import of every table EXCEPT those, which is
    // exactly the half a real browser has and a weaker fixture would not —
    // and `Mode::Replace` is the half that can fail.
    let ctx = TestContext::with_products()
        .await
        .with_auth_added()
        .await
        .with_dev_added_and_shell(control.clone(), shell)
        .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "<h1>shop</h1>", "expected_sha256": null}),
    )
    .await;
    dev_post(
        &ctx,
        "/b/dev/api/blocks",
        json!({"name": "hello", "template": "hello"}),
    )
    .await;
    let staged = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/builds/stage",
            json!({
                "block_name": "hello",
                "artifact_base64": b64(ARTIFACT),
                "compiler_version": "t",
                "diagnostics": [],
                "wafer_guest_version": WAFER_GUEST_VERSION,
            }),
        )
        .await,
    )
    .await;
    assert_eq!(staged["success"], true, "{staged}");
    seed_shop(&ctx).await;
    ctx
}

// ---------------------------------------------------------------------------
// The archive
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_zip_contains_shell_seed_sources_and_data_with_dev_off() {
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;

    // Read through `http_codec`, so these are the header and body a client
    // actually receives — `Content-Type` travels as `resp.content_type` meta,
    // not as a `resp.header.*` entry.
    assert_eq!(
        output_http_header(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
            "content-type"
        )
        .await
        .as_deref(),
        Some("application/zip")
    );
    assert!(output_http_header(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
            .await,
        "content-disposition"
    )
    .await
    .expect("Content-Disposition")
    .starts_with("attachment; filename=\"impresspress-site-"));

    let declared: u64 = output_http_header(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
            .await,
        "X-Export-Bytes",
    )
    .await
    .expect("X-Export-Bytes")
    .parse()
    .expect("a byte count");
    let bytes = output_body(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
            .await,
    )
    .await;
    assert_eq!(
        declared,
        bytes.len() as u64,
        "X-Export-Bytes is the archive"
    );

    let entries = entries(bytes);
    for expected in [
        "README.md",
        // The shell, at the paths `/asset-manifest.json` listed.
        "index.html",
        "sw.js",
        "loader.js",
        "impresspress_web-abc123.js",
        "impresspress_web_bg-abc123.wasm",
        "vendor/sql-wasm.wasm",
        // The seed, in exactly the layout `seed::import` reads.
        "seed/manifest.json",
        "seed/site/index.html",
        "seed/blocks/hello.wasm",
        "seed/blocks/hello/src/lib.rs",
        "seed/wafer_guest/Cargo.toml",
        "seed/wafer_guest/src/lib.rs",
        "seed/data.json",
    ] {
        assert!(
            entries.contains_key(expected),
            "missing {expected} in {:?}",
            sorted(&entries)
        );
    }
    // The crate is beside the blocks, where `path = "../../wafer_guest"` finds
    // it from `seed/blocks/<name>/`, and it is not a seed entry: an import must
    // not mistake it for a block.
    assert_eq!(
        entries["seed/wafer_guest/src/lib.rs"],
        impresspress_core::blocks::dev::scaffold::GUEST_LIB_RS.as_bytes()
    );
    assert_eq!(
        entries["seed/wafer_guest/Cargo.toml"],
        impresspress_core::blocks::dev::scaffold::GUEST_CARGO_TOML.as_bytes()
    );
    // Checked on the paths the seed manifest lists, not on its text: every
    // block spec carries a `wafer_guest_version` field, so the string
    // `wafer_guest` is in any manifest that has a block.
    let seed_manifest: SeedManifest =
        serde_json::from_slice(&entries["seed/manifest.json"]).expect("seed manifest");
    assert_eq!(
        seed_manifest.blocks.len(),
        1,
        "the guest crate is not a block"
    );
    let listed: Vec<&str> = seed_manifest
        .site
        .iter()
        .map(|file| file.path.as_str())
        .chain(
            seed_manifest
                .blocks
                .iter()
                .flat_map(|block| block.sources.iter().map(|file| file.path.as_str())),
        )
        .chain(seed_manifest.data.iter().map(|file| file.path.as_str()))
        .collect();
    assert!(
        !listed.iter().any(|path| path.contains("wafer_guest")),
        "the guest crate is not a seed entry: {listed:?}"
    );

    // Development mode is OFF, and the ONE line that says so is the one the
    // bundler renders (`impresspress-bundle`'s `sw.js.tmpl`): the isolation
    // passthrough reads the same constant, so it is off too.
    let sw = text(&entries, "sw.js");
    assert!(sw.contains("const DEV_ENABLED = false;"), "{sw}");
    assert!(!sw.contains("const DEV_ENABLED = true;"), "{sw}");
    assert!(
        sw.contains(
            "initialize({ dev: DEV_ENABLED, bypass: BYPASS_RULES, pageEngines: PAGE_ENGINES })"
        ),
        "{sw}"
    );
    assert!(
        sw.contains("if (DEV_ENABLED && url.pathname !== '/sw.js')"),
        "{sw}"
    );
    // …and `/seed/` is still bypassed, or the exported folder could never
    // import the seed the archive ships beside it.
    assert!(sw.contains("url.pathname.startsWith('/seed/')"), "{sw}");

    // The seed manifest describes the seed half of the archive.
    let manifest: SeedManifest =
        serde_json::from_str(&text(&entries, "seed/manifest.json")).expect("a seed manifest");
    assert_eq!(manifest.schema_version, seed::SCHEMA_VERSION);
    assert!(manifest.source_generation.is_some());
    assert_eq!(manifest.blocks.len(), 1);
    assert_eq!(manifest.blocks[0].spec.name, "site/hello");
    assert_eq!(
        manifest.blocks[0].spec.artifact_sha256,
        blobs::sha256_hex(ARTIFACT)
    );
    // Every referenced file's hash is the exporter's own (amendment 17): the
    // data snapshot is a `SeedFile` like the rest, not a bare path.
    let data = manifest.data.as_ref().expect("the bundle carries data");
    assert_eq!(data.path, "data.json");
    let data_bytes = entries.get("seed/data.json").expect("seed/data.json");
    assert_eq!(data.sha256, blobs::sha256_hex(data_bytes));
    assert_eq!(data.size, data_bytes.len() as u64);
    let site = manifest
        .site
        .iter()
        .find(|f| f.path == "index.html")
        .expect("index.html");
    assert_eq!(site.sha256, blobs::sha256_hex(SHOP_HTML));

    let snapshot: DataSnapshot = serde_json::from_slice(data_bytes).expect("a data snapshot");
    assert_eq!(snapshot.tables[PRODUCTS_TABLE].len(), 1);

    // The README carries this export's own numbers, not a template's.
    let readme = text(&entries, "README.md");
    assert!(
        readme.contains(&manifest.source_generation.clone().unwrap()),
        "{readme}"
    );
    assert!(readme.contains("password hashes"), "{readme}");
    assert!(!readme.contains("{{"), "unsubstituted hole in {readme}");

    // The compiler tree and the exporting deployment's own `seed/` are never
    // copied: the archive writes its own `seed/`, and the 72 MiB toolchain is
    // for a `/b/dev` the exported site does not have.
    assert!(
        !entries
            .keys()
            .any(|path| path.starts_with("__impresspress_dev/")),
        "{:?}",
        sorted(&entries)
    );
}

/// The deployment's own overlays are excluded by rule, not by luck: a shell
/// that DOES list them (a bundler that ran after the overlays, say) still
/// exports without them.
#[tokio::test]
async fn the_compiler_tree_and_the_deployments_own_seed_are_never_copied() {
    let control = FakeControl::new();
    control.set_validated_info(hello_info("site/hello"));
    let shell = FakeShell::new()
        .with("__impresspress_dev/compiler/manifest.json", b"{}")
        .with("seed/manifest.json", b"{\"schema_version\":1}")
        .with("seed/site/index.html", b"<h1>welcome</h1>");
    let ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_shell(control, std::sync::Arc::new(shell))
        .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "<h1>shop</h1>", "expected_sha256": null}),
    )
    .await;

    let bytes = output_body(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
            .await,
    )
    .await;
    let entries = entries(bytes);
    assert!(
        !entries
            .keys()
            .any(|path| path.starts_with("__impresspress_dev/")),
        "{:?}",
        sorted(&entries)
    );
    // The archive's own `seed/site/index.html` is the exported site, NOT the
    // deployment's welcome page that the shell also listed.
    assert_eq!(
        entries.get("seed/site/index.html").map(Vec::as_slice),
        Some(SHOP_HTML)
    );
}

/// The one edit the export makes to a shell file has to be verifiable, so a
/// shell whose `sw.js` does not carry the marker is a 500 — never a silent
/// pass-through of a service worker that would come up as a second sandbox.
#[tokio::test]
async fn a_shell_whose_sw_js_has_no_dev_marker_is_refused() {
    let control = FakeControl::new();
    let shell = FakeShell::new().with("sw.js", b"await initialize({ dev: true });");
    let ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_shell(control, std::sync::Arc::new(shell))
        .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "x", "expected_sha256": null}),
    )
    .await;

    let status = output_http_status(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
            .await,
    )
    .await;
    assert_eq!(status, 500);
}

/// And a shell that cannot be listed at all: an export with no runtime in it
/// is a folder that cannot be served, so it must fail rather than produce one.
#[tokio::test]
async fn a_shell_that_cannot_be_listed_is_refused() {
    let control = FakeControl::new();
    let shell = FakeShell::new().failing_to_list("/asset-manifest.json: HTTP 404");
    let ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_shell(control, std::sync::Arc::new(shell))
        .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "x", "expected_sha256": null}),
    )
    .await;

    let status = output_http_status(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
            .await,
    )
    .await;
    assert_eq!(status, 500);
}

/// A site file the exported worker would shadow refuses the export, on both
/// surfaces, by path and rule — the exported runtime's seed import would
/// refuse the whole bundle over it and the exported site would come up empty.
///
/// The sandbox refuses the write itself, so such a file can only predate the
/// check: here it is written through a worker that handed over no rules, and
/// then exported by one that hands over a dev-sandbox worker's.
#[tokio::test]
async fn a_site_file_the_exported_worker_would_shadow_refuses_the_export() {
    let shell = std::sync::Arc::new(FakeShell::new());
    let ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_bypass(FakeControl::new(), shell.clone(), BypassRules::default())
        .await;
    for path in ["site/index.html", "site/manifest.json"] {
        dev_post(
            &ctx,
            "/b/dev/api/files/write",
            json!({"path": path, "content": "x", "expected_sha256": null}),
        )
        .await;
    }
    // Under the rules it was written with, the workspace exports: the check
    // is the rules', not a refusal of every export.
    export::build(&ctx, &ctx.dev_shared())
        .await
        .expect("no rules, nothing shadowed");

    // The same workspace behind a worker that does hand its rules over.
    let current = DevShared::new(FakeControl::new(), shell.clone(), fake_bypass_rules());
    let fetched = shell.fetches();
    for error in [
        export::build(&ctx, &current).await.expect_err("refused"),
        export::manifest_preview(&ctx, &current)
            .await
            .expect_err("refused"),
    ] {
        assert_eq!(error.code, wafer_run::ErrorCode::FailedPrecondition);
        let message = &error.message;
        assert!(message.contains("\"site/manifest.json\""), "{message}");
        assert!(message.contains("\"/manifest.json\""), "{message}");
        assert!(message.contains("the exact path"), "{message}");
        assert!(message.contains("Delete the file or move it"), "{message}");
    }
    // Refused before the runtime was read.
    assert_eq!(shell.fetches(), fetched);
}

/// What a caller of the two ROUTES is told about that refusal — the page's
/// Export button, the `dev_export` tool and the `dev_export_manifest` tool
/// all go through them, and none of them reaches `export::build`.
///
/// A 400 whose body names the file, the URL and the rule, and says what to do.
/// Not the sanitized 500 an unclassified failure becomes: the person can fix
/// this one, and only if they are told which file it is.
///
/// The body is compared with a file the page's own tests read
/// (`assets/test/dev_export.test.mjs`), which hold the tools to passing this
/// message on whole. So what the handler says and what the agent is shown are
/// one text, checked from both ends.
///
/// The workspace is the one the test above builds: the file is written
/// through a worker that handed over no rules, and the routes are then asked
/// by the block a newer worker registers over the same workspace.
#[tokio::test]
async fn the_export_routes_answer_a_shadowed_site_file_with_a_400_that_names_it() {
    use impresspress_core::blocks::dev::{DevBlock, BLOCK_NAME};

    const ROUTES: [&str; 2] = ["/b/dev/api/export", "/b/dev/api/export/manifest"];

    let shell = std::sync::Arc::new(FakeShell::new());
    let mut ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_bypass(FakeControl::new(), shell.clone(), BypassRules::default())
        .await;
    for path in ["site/index.html", "site/manifest.json"] {
        dev_post(
            &ctx,
            "/b/dev/api/files/write",
            json!({"path": path, "content": "x", "expected_sha256": null}),
        )
        .await;
    }
    // The routes themselves are not what refuses: under the rules the file
    // was written with, both answer.
    for route in ROUTES {
        assert_eq!(
            output_http_status(ctx.dispatch_resolved(admin_msg("retrieve", route)).await).await,
            200,
            "{route}"
        );
    }

    // The same workspace, now behind a worker that hands its rules over.
    ctx.register_block(
        BLOCK_NAME,
        std::sync::Arc::new(DevBlock::with_workspace(DevShared::new(
            FakeControl::new(),
            shell.clone(),
            fake_bypass_rules(),
        ))),
    );
    let fetched = shell.fetches();

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/blocks/dev/assets/test/fixtures/export-shadowed-site-file.json");
    let expected: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&fixture).unwrap_or_else(|e| panic!("{}: {e}", fixture.display())),
    )
    .expect("the fixture is JSON");

    for route in ROUTES {
        let parts = wafer_block::http_codec::collect_http_response(
            ctx.dispatch_resolved(admin_msg("retrieve", route)).await,
        )
        .await;
        assert_eq!(parts.status, 400, "{route}");
        let header = |name: &str| {
            parts
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("content-type"), Some("application/json"), "{route}");
        assert_eq!(header("cache-control"), Some("no-store"), "{route}");
        // Never a download: a refusal saved as `impresspress-site-….zip`
        // would be a broken archive with the explanation inside it.
        assert_eq!(header("content-disposition"), None, "{route}");

        let body: serde_json::Value =
            serde_json::from_slice(&parts.body).unwrap_or_else(|e| panic!("{route}: {e}"));
        let message = body["message"].as_str().expect("message");
        // The file, the URL the worker keeps from the runtime, the rule, and
        // the way out.
        assert!(message.contains("\"site/manifest.json\""), "{message}");
        assert!(message.contains("\"/manifest.json\""), "{message}");
        assert!(message.contains("the exact path"), "{message}");
        assert!(message.contains("Delete the file or move it"), "{message}");
        // And nothing an internal error would say instead.
        assert!(!message.contains("Internal server error"), "{message}");
        assert_eq!(
            body,
            expected,
            "{route}: the answer and {} have drifted apart; the page's tests read that file",
            fixture.display()
        );
    }
    // Both routes refused before the runtime shell was read.
    assert_eq!(shell.fetches(), fetched);
}

/// The compiler prefix is NOT one of the exported worker's rules — the export
/// strips it from the exported `sw.js` — so a site file under it does not
/// refuse the export, however it got there.
#[tokio::test]
async fn the_compiler_prefix_is_not_an_exported_rule() {
    let shell = std::sync::Arc::new(FakeShell::new());
    let ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_bypass(FakeControl::new(), shell.clone(), BypassRules::default())
        .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/__impresspress_dev/compiler/notes.txt", "content": "x", "expected_sha256": null}),
    )
    .await;
    let current = DevShared::new(FakeControl::new(), shell, fake_bypass_rules());
    export::build(&ctx, &current).await.expect("exports");
}

/// The export removes the compiler's bypass from two renderings of one fact
/// in `sw.js`: the fetch condition's clause and the `BYPASS_RULES` data. A
/// worker whose data lists the prefix while its condition lacks the clause
/// the export recognises (or the reverse) cannot be stripped consistently,
/// and exporting it anyway would ship a worker that says one thing and does
/// another — so it is an export error.
#[tokio::test]
async fn an_sw_whose_condition_and_rules_disagree_about_the_compiler_is_refused() {
    let listed_only = "const DEV_ENABLED = true;\n\
        const BYPASS_RULES = {\"exact\":[\"/sw.js\"],\"prefixes\":[\"/__impresspress_dev/compiler/\",\"/seed/\"]};\n\
        if (url.pathname === '/sw.js' || url.pathname.startsWith('/__impresspress_dev/compiler/') || url.pathname.startsWith('/seed/')) { return; }\n";
    let clause_only = "const DEV_ENABLED = true;\n\
        const BYPASS_RULES = {\"exact\":[\"/sw.js\"],\"prefixes\":[\"/seed/\"]};\n\
        if (url.pathname === '/sw.js' ||\n        url.pathname.startsWith('/__impresspress_dev/compiler/') ||\n        url.pathname.startsWith('/seed/')) { return; }\n";
    for sw in [listed_only, clause_only] {
        let shell = FakeShell::new().with("sw.js", sw.as_bytes());
        let ctx = TestContext::with_admin()
            .await
            .with_dev_added_and_shell(FakeControl::new(), std::sync::Arc::new(shell))
            .await;
        dev_post(
            &ctx,
            "/b/dev/api/files/write",
            json!({"path": "site/index.html", "content": "x", "expected_sha256": null}),
        )
        .await;

        let error = export::build(&ctx, &ctx.dev_shared())
            .await
            .expect_err("a worker that disagrees with itself is not exported");
        assert_eq!(error.code, wafer_run::ErrorCode::Internal, "{sw}");
        assert!(
            error.message.contains("disagrees with itself"),
            "{}",
            error.message
        );
        let status = output_http_status(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await;
        assert_eq!(status, 500, "{sw}");
    }
}

/// Two exports of one generation are byte-identical — the WHOLE archive,
/// README included.
///
/// `ZipWriter` fixes every entry's timestamp, `assemble` fixes the entry
/// order, and the README is dated from the ACTIVE GENERATION's `created_at`
/// rather than the wall clock. That last one is the point: an export is a
/// function of what is live, and a README carrying the download's own
/// timestamp would have made the one entry that differs between two otherwise
/// identical exports the one entry nobody diffing them cares about.
#[tokio::test]
async fn two_exports_of_the_same_generation_are_identical() {
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;
    let first = output_body(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
            .await,
    )
    .await;
    let second = output_body(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
            .await,
    )
    .await;
    assert_eq!(
        first, second,
        "two exports of one generation must be identical, README included"
    );
}

/// And the date the README carries is the generation's own, so it says when
/// the site came to be rather than when someone pressed the button.
#[tokio::test]
async fn the_readme_is_dated_by_the_generation_not_the_download() {
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;
    let archive = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    let status = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/status"))
            .await,
    )
    .await;
    let created_at = status["active_generation"]["created_at"]
        .as_str()
        .expect("created_at")
        .to_string();
    assert!(
        text(&archive, "README.md").contains(&created_at),
        "the README must carry the generation's own timestamp"
    );
}

/// The exported `sw.js` keeps `/seed/` on the bypass list — without it the
/// folder could never import the seed shipped beside it — and DROPS the
/// compiler's, because the export copies none of those assets. A bypass for a
/// tree that is not there waves every request under the prefix past the
/// runtime to a 404 from the static host.
#[tokio::test]
async fn the_exported_sw_drops_the_compiler_bypass_and_keeps_the_seed_one() {
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;
    let archive = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    let sw = text(&archive, "sw.js");

    assert!(
        !sw.contains("__impresspress_dev"),
        "the compiler bypass must be gone; {sw}"
    );
    assert!(sw.contains("url.pathname.startsWith('/seed/')"), "{sw}");
    // Only that one clause: the app's other bypasses are untouched, and the
    // expression still reads the way the bundler would have rendered it for a
    // bundle that never asked for the compiler.
    assert!(sw.contains("url.pathname.startsWith('/snippets/')"), "{sw}");
    assert!(
        sw.contains(
            "if (url.pathname.startsWith('/snippets/') ||\n        \
             url.pathname.startsWith('/cdn-cgi/') ||\n        \
             url.pathname.startsWith('/seed/')) { return; }"
        ),
        "the remaining expression must be exactly what a compiler-less bundle renders; {sw}"
    );

    // And the rules the exported worker hands its runtime say the same: that
    // runtime's seed import refuses a site file at any path they list, and a
    // prefix the exported fetch handler no longer bypasses would refuse paths
    // that site can serve. Every other rule is kept, in order.
    let mut expected = fake_bypass_rules();
    expected
        .prefixes
        .retain(|prefix| prefix != "/__impresspress_dev/compiler/");
    let declaration = format!(
        "const BYPASS_RULES = {};\n",
        serde_json::to_string(&expected).unwrap()
    );
    assert_eq!(sw.matches(&declaration).count(), 1, "{sw}");
    assert!(
        sw.contains("await initialize({ dev: DEV_ENABLED, bypass: BYPASS_RULES, pageEngines: PAGE_ENGINES });"),
        "{sw}"
    );
}

/// A shell with no compiler bypass to begin with is left alone — CI's
/// foundations bundle ships no compiler, and its absence is an ordinary
/// build rather than a mismatched one.
#[tokio::test]
async fn a_shell_with_no_compiler_bypass_is_exported_unchanged() {
    let control = FakeControl::new();
    control.set_validated_info(hello_info("site/hello"));
    let plain_sw = "const DEV_ENABLED = true;\n\
                    if (url.pathname.startsWith('/seed/')) { return; }\n";
    let shell = FakeShell::new().with("sw.js", plain_sw.as_bytes());
    let ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_shell(control, std::sync::Arc::new(shell))
        .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "x", "expected_sha256": null}),
    )
    .await;

    let archive = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    assert_eq!(
        text(&archive, "sw.js"),
        plain_sw.replace("= true;", "= false;"),
        "only the dev flag may change on a shell with no compiler bypass"
    );
}

// ---------------------------------------------------------------------------
// Source provenance
// ---------------------------------------------------------------------------

/// The artifact comes from the live generation and the sources come from the
/// workspace as it stands, so the two CAN disagree — an agent that edited
/// `blocks/hello/src/lib.rs` and did not recompile leaves an export whose
/// `.wasm` and `src/` describe different programs. The README says which,
/// per block, so it is never silent.
#[tokio::test]
async fn the_readme_says_whether_each_blocks_sources_match_its_artifact() {
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;

    // `shop_instance` staged without a `source_manifest_sha256`, so the build
    // row records none and the verdict is honestly "unknown" rather than a
    // guess in either direction.
    let archive = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    let readme = text(&archive, "README.md");
    assert!(
        readme.contains("site/hello: no source digest recorded"),
        "{readme}"
    );
}

/// And when the compile DID record a digest, the verdict is a real
/// comparison: matching sources read "current", and one edited byte reads
/// "SOURCES DIFFER".
#[tokio::test]
async fn a_recorded_source_digest_is_compared_against_the_workspace() {
    let control = FakeControl::new();
    control.set_validated_info(hello_info("site/hello"));
    let ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_shell(control.clone(), std::sync::Arc::new(FakeShell::new()))
        .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "<h1>shop</h1>", "expected_sha256": null}),
    )
    .await;
    dev_post(
        &ctx,
        "/b/dev/api/blocks",
        json!({"name": "hello", "template": "hello"}),
    )
    .await;

    // The digest the page computes: sorted `"<crate-relative path>\0<sha>\n"`
    // lines over the block's sources, exactly as `dev.js`'s `snapshotBlock`
    // builds it. Restated here rather than reached for, because the whole
    // point of the check is that two independent computations of it agree.
    let listed = output_json(
        ctx.dispatch_resolved({
            let mut msg = admin_msg("retrieve", "/b/dev/api/files");
            msg.set_meta("req.query.prefix", "blocks/hello/");
            msg
        })
        .await,
    )
    .await;
    let mut lines: Vec<String> = listed["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| {
            format!(
                "{}\0{}\n",
                f["path"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("blocks/hello/"),
                f["sha256"].as_str().unwrap()
            )
        })
        .collect();
    lines.sort();
    let digest = impresspress_core::blocks::dev::blobs::sha256_hex(lines.concat().as_bytes());

    let staged = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/builds/stage",
            json!({
                "block_name": "hello",
                "artifact_base64": b64(ARTIFACT),
                "source_manifest_sha256": digest,
                "compiler_version": "t",
                "diagnostics": [],
                "wafer_guest_version": WAFER_GUEST_VERSION,
            }),
        )
        .await,
    )
    .await;
    assert_eq!(staged["success"], true, "{staged}");

    let readme = text(
        &entries(
            output_body(
                ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                    .await,
            )
            .await,
        ),
        "README.md",
    );
    assert!(
        readme.contains("site/hello: sources match the compiled artifact"),
        "{readme}"
    );

    // Now edit a source without recompiling. The artifact in the generation is
    // unchanged; the workspace is not.
    let read = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "blocks/hello/src/lib.rs"}),
        )
        .await,
    )
    .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({
            "path": "blocks/hello/src/lib.rs",
            "content": format!("{}\n// edited\n", read["content"].as_str().unwrap()),
            "expected_sha256": read["sha256"],
        }),
    )
    .await;

    let readme = text(
        &entries(
            output_body(
                ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                    .await,
            )
            .await,
        ),
        "README.md",
    );
    assert!(readme.contains("site/hello: SOURCES DIFFER"), "{readme}");
}

// ---------------------------------------------------------------------------
// The manifest
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_manifest_previews_the_archive_without_building_it() {
    let ctx = TestContext::with_dev_added_and_shell(
        TestContext::with_admin().await,
        FakeControl::new(),
        std::sync::Arc::new(FakeShell::new()),
    )
    .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "x", "expected_sha256": null}),
    )
    .await;

    let m = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export/manifest"))
            .await,
    )
    .await;
    assert_eq!(m["site_files"], 1);
    assert_eq!(m["blocks"], 0);
    assert_eq!(m["shell_files"], 7, "{m}");
    assert!(m["total_bytes"].as_u64().expect("total_bytes") > 0);
    assert!(!m["generation_id"]
        .as_str()
        .expect("generation_id")
        .is_empty());
}

/// The guest crate is there for the blocks' sake, so a generation with no
/// block exports without it — an archive of a static site carries no Rust.
#[tokio::test]
async fn a_generation_with_no_block_exports_no_guest_crate() {
    let ctx = TestContext::with_dev_added_and_shell(
        TestContext::with_admin().await,
        FakeControl::new(),
        std::sync::Arc::new(FakeShell::new()),
    )
    .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "x", "expected_sha256": null}),
    )
    .await;

    let entries = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    // The export itself happened: the site is in it.
    assert!(
        entries.contains_key("seed/site/index.html"),
        "{:?}",
        sorted(&entries)
    );
    assert!(
        !entries
            .keys()
            .any(|path| path.starts_with("seed/wafer_guest/")),
        "{:?}",
        sorted(&entries)
    );
}

/// The manifest is not a second derivation of what an export contains — it is
/// a summary of the same assembled entry list, so every path and size it
/// publishes is in the archive with exactly that size.
#[tokio::test]
async fn the_manifest_describes_the_archive_entry_for_entry() {
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;

    let manifest: ExportManifest = serde_json::from_value(
        output_json(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export/manifest"))
                .await,
        )
        .await,
    )
    .expect("an ExportManifest");
    let entries = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );

    let mut listed: Vec<String> = manifest.files.iter().map(|f| f.path.clone()).collect();
    listed.sort();
    assert_eq!(listed, sorted(&entries));
    // The guest crate is an archive entry like a block's source, so the
    // preview lists it too.
    for guest in ["seed/wafer_guest/Cargo.toml", "seed/wafer_guest/src/lib.rs"] {
        assert!(
            listed.iter().any(|path| path == guest),
            "{guest} in {listed:?}"
        );
    }
    for file in &manifest.files {
        // The README is the one entry whose size can move between two calls
        // (it carries the wall-clock date), so it is compared for presence
        // rather than for length.
        if file.path == "README.md" {
            continue;
        }
        assert_eq!(
            entries[&file.path].len() as u64,
            file.bytes,
            "{} is a different size in the archive",
            file.path
        );
    }
    assert_eq!(manifest.blocks, 1);
    assert_eq!(manifest.site_files, 1);
    assert_eq!(manifest.tables[PRODUCTS_TABLE], 1);
    // Every allowlisted table is reported, empty ones included — "no
    // products" and "no products table in this build" must not read the same.
    assert!(manifest.tables.contains_key(variables::TABLE));
}

/// The archive is compressed: the runtime wasm, which is nine tenths of a
/// real export, goes in DEFLATEd and comes back out byte for byte, and the
/// download is much smaller than the content it carries. The manifest still
/// reports each entry's own size, which is what the archive unpacks to.
#[tokio::test]
async fn the_archive_deflates_the_runtime_and_the_manifest_reports_content_sizes() {
    // Shaped like a real runtime in the one respect that matters here: it
    // compresses (a real one shrinks by about two thirds).
    let runtime: Vec<u8> = b"\0asm\x01\0\0\0"
        .iter()
        .copied()
        .chain((0..256 * 1024).map(|i| (i % 251) as u8 & 0x3f))
        .collect();
    let control = FakeControl::new();
    let ctx = shop_instance_with_shell(
        &control,
        std::sync::Arc::new(FakeShell::new().with("impresspress_web_bg-abc123.wasm", &runtime)),
    )
    .await;

    let manifest: ExportManifest = serde_json::from_value(
        output_json(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export/manifest"))
                .await,
        )
        .await,
    )
    .expect("an ExportManifest");
    let archive = output_body(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
            .await,
    )
    .await;

    let listed = manifest
        .files
        .iter()
        .find(|f| f.path == "impresspress_web_bg-abc123.wasm")
        .expect("the runtime is in the manifest");
    assert_eq!(
        listed.bytes,
        runtime.len() as u64,
        "the content size, not the compressed one"
    );
    assert!(
        (archive.len() as u64) < manifest.total_bytes / 2,
        "a {}-byte archive of {} content bytes is not compressed",
        archive.len(),
        manifest.total_bytes
    );

    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).expect("a readable zip");
    let mut entry = zip
        .by_name("impresspress_web_bg-abc123.wasm")
        .expect("the runtime is in the archive");
    assert_eq!(entry.compression(), zip::CompressionMethod::Deflated);
    let mut unpacked = Vec::new();
    entry
        .read_to_end(&mut unpacked)
        .expect("inflate the runtime");
    assert!(
        unpacked == runtime,
        "the runtime does not unpack to the bytes that went in"
    );
}

/// Nothing published, nothing to export — and the refusal says what to do
/// about it rather than 500ing on an absent generation.
#[tokio::test]
async fn exporting_a_fresh_instance_is_refused() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    assert_eq!(
        output_http_status(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await
        )
        .await,
        400
    );
    assert_eq!(
        output_http_status(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export/manifest"))
                .await
        )
        .await,
        400
    );
}

/// A blob the manifest names that the store no longer has is the export
/// LOSING A RACE, not an internal fault.
///
/// `assemble` reads the site manifest and then each blob with nothing held —
/// deliberately, because a 10 MB read under the workspace mutex would block
/// editing for the length of an export. What that admits is a `blocks/`-source
/// delete landing in between and collecting the blob underneath the read. The
/// answer has to say so and say what to do about it: a generic 500 reads like
/// a bug in the exporter, and the remedy — try again — is not one anybody
/// would guess from it.
#[tokio::test]
async fn a_blob_freed_mid_export_is_a_named_refusal_rather_than_a_500() {
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;
    // Exactly what the race produces: the manifest still names the blob, the
    // store no longer has it.
    blobs::delete(&ctx, &blobs::sha256_hex(SHOP_HTML))
        .await
        .expect("free the blob the site manifest names");

    let out = ctx
        .dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
        .await;
    assert_eq!(output_http_status(out).await, 409);

    // The same answer on the manifest endpoint, and on the non-HTTP callers:
    // one wording, so an agent that retries on one retries on the other.
    let out = ctx
        .dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export/manifest"))
        .await;
    assert_eq!(output_http_status(out).await, 409);
    let error = impresspress_core::blocks::dev::export::build(&ctx, ctx.dev_shared().as_ref())
        .await
        .expect_err("a freed blob must refuse the export");
    assert!(
        error.message.contains("the workspace changed") && error.message.contains("try again"),
        "{}",
        error.message
    );
}

/// Both routes are `/b/dev`'s, so both are admin-only at the router.
#[tokio::test]
async fn the_export_routes_are_admin_only() {
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;
    for path in ["/b/dev/api/export", "/b/dev/api/export/manifest"] {
        let status = output_http_status(ctx.request(anon_msg("retrieve", path)).await).await;
        assert!(
            status == 401 || status == 403,
            "{path} answered an anonymous caller with {status}"
        );
    }
}

// ---------------------------------------------------------------------------
// The round trip
// ---------------------------------------------------------------------------

/// The claim the whole format rests on: an export is a seed bundle. Export
/// from A, feed the archive's `seed/` entries to B's importer, and B serves
/// the same shop with the same product.
#[tokio::test]
async fn an_exported_seed_imports_into_a_fresh_instance() {
    let a_control = FakeControl::new();
    let a = shop_instance(&a_control).await;
    let archive = entries(
        output_body(
            a.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );

    let manifest: SeedManifest =
        serde_json::from_slice(&archive["seed/manifest.json"]).expect("a seed manifest");
    // The archive carries the guest crate beside the blocks, and the import
    // below is of that same archive: an entry the seed manifest does not list
    // is one the importer never reads.
    assert!(archive.contains_key("seed/wafer_guest/src/lib.rs"));
    // The importer fetches by URL under `/seed/`; the archive holds the same
    // paths without the leading slash. That correspondence IS the format.
    let fetch = ArchiveFetch { archive };

    let b_control = FakeControl::new();
    b_control.set_validated_info(hello_info("site/hello"));
    let b = TestContext::with_products()
        .await
        .with_auth_added()
        .await
        .with_dev_added_and_shell(b_control.clone(), std::sync::Arc::new(FakeShell::new()))
        .await;
    let generation = seed::import(
        &b,
        b_control.as_ref(),
        &fake_bypass_rules(),
        &manifest,
        &fetch,
    )
    .await
    .expect("import")
    .expect("a fresh instance imports");
    activation::request(
        &b,
        &b.dev_shared(),
        GenerationCause::Seed,
        ActivationIntent::Seed {
            manifest: generation,
        },
        activation::Maintenance::Inline,
    )
    .await
    .expect("activate the imported generation");

    // The shop is being served.
    assert_eq!(
        b.storage_get("wafer-run/web", "site", "index.html")
            .await
            .expect("published index"),
        SHOP_HTML.to_vec()
    );
    // The block is live, with its source alongside it.
    let live = b_control.live_blocks().expect("a live set");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].name, "site/hello");
    // And the data came with it.
    let products = db::list_all(&b, PRODUCTS_TABLE, Vec::new())
        .await
        .expect("products");
    assert_eq!(products.len(), 1);
    assert_eq!(products[0].data["slug"], json!("custom-print"));
}

/// A [`seed::SeedFetch`] over the archive's own entries, keyed the way the
/// importer asks for them.
struct ArchiveFetch {
    archive: HashMap<String, Vec<u8>>,
}

impl seed::SeedFetch for ArchiveFetch {
    fn get<'a>(&'a self, url: &'a str) -> seed::FetchFuture<'a> {
        Box::pin(async move {
            let path = url.trim_start_matches('/');
            self.archive
                .get(path)
                .cloned()
                .ok_or_else(|| format!("{url}: not in the archive"))
        })
    }
}

// ---------------------------------------------------------------------------
// The data snapshot's size
// ---------------------------------------------------------------------------

/// The ordinary site-config variable the size tests grow.
const NOTES_KEY: &str = "WAFER_RUN_SHARED__SHOP_NOTES";

/// Every timestamp the notes row is written with.
const NOTES_WRITTEN_AT: &str = "2026-01-01T00:00:00.123456789+00:00";

/// Replace the notes variable with one holding `len` bytes, and nothing else
/// about the row different from the last one.
///
/// The row's `created_at` and `updated_at` are pinned because their width is
/// not fixed: `now_rfc3339` (chrono's `to_rfc3339`) writes the fraction of a
/// second with 9, 6, 3 or 0 digits as the instant does or does not fall on a
/// whole microsecond, millisecond or second. Left to the clock, a notes row
/// written one byte longer can serialize shorter than the last one, and the
/// size these tests step to the byte is off by up to 20.
async fn set_notes(ctx: &TestContext, len: usize) {
    variables::delete_by_key(ctx, NOTES_KEY)
        .await
        .expect("clear the notes");
    let row = variables::insert(
        ctx,
        variables::NewVariable {
            key: NOTES_KEY.to_string(),
            value: "n".repeat(len),
            name: String::new(),
            description: String::new(),
            warning: String::new(),
            sensitive: false,
            updated_by: String::new(),
            block: variables::block_for_key(NOTES_KEY),
        },
    )
    .await
    .expect("store the notes");
    db::update(
        ctx,
        variables::TABLE,
        &row.id,
        HashMap::from([
            ("created_at".to_string(), json!(NOTES_WRITTEN_AT)),
            ("updated_at".to_string(), json!(NOTES_WRITTEN_AT)),
        ]),
    )
    .await
    .expect("pin the notes' timestamps");
}

/// The size `seed/data.json` has in an export of `ctx` right now.
async fn data_json_len(ctx: &TestContext) -> usize {
    let archive = entries(
        impresspress_core::blocks::dev::export::build(ctx, ctx.dev_shared().as_ref())
            .await
            .expect("export"),
    );
    archive["seed/data.json"].len()
}

/// A shop whose snapshot would not fit the importer is refused at export,
/// with the reason and the limit, rather than exported as a bundle whose own
/// cold boot then refuses it and serves an empty site.
///
/// Exactly at the limit, the same shop exports and imports: the bound the
/// exporter enforces is the importer's, not a stricter or looser copy of it.
#[tokio::test]
async fn a_data_snapshot_over_the_import_limit_is_refused_at_export_and_one_at_it_round_trips() {
    let a_control = FakeControl::new();
    let a_shell = std::sync::Arc::new(FakeShell::new());
    let a = shop_instance_with_shell(&a_control, a_shell.clone()).await;

    // Grow the notes until `data.json` is exactly the limit. The value is
    // plain ASCII, so a byte of value is a byte of JSON; the loop only has to
    // absorb whatever the row's own columns add.
    let mut len = 1;
    set_notes(&a, len).await;
    for _ in 0..4 {
        let size = data_json_len(&a).await;
        if size == seed::MAX_DATA_BYTES {
            break;
        }
        len = (len + seed::MAX_DATA_BYTES)
            .checked_sub(size)
            .expect("room");
        set_notes(&a, len).await;
    }
    assert_eq!(data_json_len(&a).await, seed::MAX_DATA_BYTES);

    // At the limit: exported, and imported by a fresh instance.
    let archive = entries(
        output_body(
            a.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    let manifest: SeedManifest =
        serde_json::from_slice(&archive["seed/manifest.json"]).expect("a seed manifest");
    let b_control = FakeControl::new();
    b_control.set_validated_info(hello_info("site/hello"));
    let b = TestContext::with_products()
        .await
        .with_auth_added()
        .await
        .with_dev_added_and_shell(b_control.clone(), std::sync::Arc::new(FakeShell::new()))
        .await;
    seed::import(
        &b,
        b_control.as_ref(),
        &fake_bypass_rules(),
        &manifest,
        &ArchiveFetch { archive },
    )
    .await
    .expect("a bundle at the limit imports")
    .expect("a fresh instance imports");
    let notes = variables::get_by_key(&b, NOTES_KEY)
        .await
        .expect("read")
        .expect("the notes travelled");
    assert_eq!(notes.value.len(), len);

    // One byte over: refused, on both routes and to the non-HTTP caller.
    set_notes(&a, len + 1).await;
    for path in ["/b/dev/api/export", "/b/dev/api/export/manifest"] {
        let before = a.storage_reads().len();
        let shell_before = a_shell.fetches();
        let refused = wafer_block::http_codec::collect_http_response(
            a.dispatch_resolved(admin_msg("retrieve", path)).await,
        )
        .await;
        // Refused before the runtime or any stored content was read: the
        // snapshot is built and measured first.
        assert_eq!(a_shell.fetches(), shell_before, "{path} fetched the shell");
        let reads = a.storage_reads()[before..].to_vec();
        assert!(
            !reads
                .iter()
                .any(|read| read.contains("impresspress/dev/blobs/")
                    || read.contains("impresspress/dev/artifacts/")),
            "{path} read content it was about to refuse: {reads:#?}"
        );
        assert_eq!(refused.status, 413, "{path}");
        let body: serde_json::Value = serde_json::from_slice(&refused.body).expect("json");
        let message = body["message"].as_str().expect("message");
        assert!(
            message.contains(&format!("{} bytes", seed::MAX_DATA_BYTES + 1))
                && message.contains(&seed::MAX_DATA_BYTES.to_string())
                && message.contains("could not be imported"),
            "{path}: {message}"
        );
    }
    let error = impresspress_core::blocks::dev::export::build(&a, a.dev_shared().as_ref())
        .await
        .expect_err("an oversized snapshot refuses the export");
    assert_eq!(error.code, wafer_run::ErrorCode::ResourceExhausted);
}

// ---------------------------------------------------------------------------
// What the sandbox says about itself stays in the sandbox
// ---------------------------------------------------------------------------

/// The sandbox's own `llms.txt`, as a seed import records it.
const SANDBOX_LLMS: &str = "# ImpressPress build sandbox\n\nBuild a website here.\n";

/// The sandbox deployment's own boot shell, as built from `seed`: the shipped
/// templates, rendered by the bundler with `examples/dev-sandbox`'s `[app]`
/// name, that seed's title and the real boot notice, development mode on and
/// the compiler's bypass — what `build.sh --seed <seed>` produces, minus the
/// wasm. Returned as a [`FakeShell`] over the three files the export edits or
/// a visitor reads text from, with the title it was rendered under.
fn sandbox_shell(seed: &str) -> (FakeShell, String) {
    let sandbox =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/dev-sandbox");
    let config = std::fs::read_to_string(sandbox.join("impresspress.toml")).expect("toml");
    // The name and the title, read from where the deployment keeps them so
    // this cannot go on testing strings the sandbox no longer has: the name
    // from its configuration, the title from the seed's `sandbox.json` —
    // which is where `build.sh` takes it from (`[app] title_file`).
    let name = config
        .lines()
        .find_map(|line| line.strip_prefix("name = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("impresspress.toml has no [app] name")
        .to_string();
    let seed_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(sandbox.join("seeds").join(seed).join("sandbox.json"))
            .expect("sandbox.json"),
    )
    .expect("sandbox.json is JSON");
    let title = seed_json["title"]
        .as_str()
        .unwrap_or_else(|| panic!("seeds/{seed}/sandbox.json has no title"))
        .to_string();
    assert!(name.contains("sandbox") && title.contains("sandbox"));
    let notice = std::fs::read_to_string(sandbox.join("boot-notice.html")).expect("notice");

    let dir = tempfile::tempdir().expect("tempdir");
    impresspress_bundle::assets::write_to(dir.path()).expect("shell assets");
    impresspress_bundle::bundle::run(
        dir.path(),
        dir.path(),
        impresspress_bundle::bundle::AppConfig {
            app_name: Some(name),
            app_title: Some(title.clone()),
            boot_notice_html: Some(notice),
            dev_enabled: true,
            extra_bypass_prefix: vec!["/__impresspress_dev/compiler/".to_string()],
            ..Default::default()
        },
    )
    .expect("render the shell");
    let mut shell = FakeShell::new();
    for file in ["index.html", "loader.js", "sw.js"] {
        let bytes = std::fs::read(dir.path().join(file)).expect("rendered file");
        if file == "index.html" {
            // What the export is about to be asked to take out is really
            // there. (No committed title has a character the bundler escapes.)
            assert!(
                String::from_utf8_lossy(&bytes).contains(&format!("<title>{title}</title>")),
                "the {seed} shell is not titled {title:?}"
            );
        }
        shell = shell.with(file, &bytes);
    }
    (shell, title)
}

/// Every string literal and every piece of markup text in `source`, minus
/// the lines a visitor never sees: comments and `console.*` calls (which
/// keep the build's own name as their prefix, for whoever built it).
fn visible_lines(source: &str) -> Vec<&str> {
    source
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//") && !line.starts_with("console."))
        .collect()
}

async fn record_sandbox_llms(ctx: &TestContext) {
    seed_info::write(
        ctx,
        &SeedInfo {
            template: "blank".to_string(),
            suggested_prompt: String::new(),
            guide_markdown: String::new(),
            llms_text: Some(SANDBOX_LLMS.to_string()),
        },
    )
    .await
    .expect("seed info");
}

/// Name the site, the way an admin does on the settings page.
async fn name_the_site(ctx: &TestContext, name: &str) {
    variables::upsert_by_key(
        ctx,
        impresspress_core::config_vars::APP_NAME_KEY,
        variables::VariablePatch {
            value: Some(name.to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("app name");
}

/// The exported site is not the sandbox, and nothing the sandbox says about
/// itself goes with it: not its `llms.txt` (which the runtime IS publishing
/// for this site, and which is in no manifest the export reads), and not a
/// word a visitor could read on its boot shell — the page's notice is gone,
/// its title is the site's, and the loader names the app by that title.
///
/// Checked on the shell the bundler really renders for the sandbox
/// ([`sandbox_shell`]), so a new place the templates show the deployment's
/// name or wording fails here.
#[tokio::test]
async fn nothing_the_sandbox_says_about_itself_is_exported() {
    // Each seed heads its boot page with a title of its own, so each is
    // exported: the one that names its template has more to leave behind.
    for seed in ["blank", "bootstrap"] {
        nothing_a_sandbox_built_from_this_seed_says_is_exported(seed).await;
    }
}

async fn nothing_a_sandbox_built_from_this_seed_says_is_exported(seed: &str) {
    let (shell, sandbox_title) = sandbox_shell(seed);
    let ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_shell(FakeControl::new(), std::sync::Arc::new(shell))
        .await;
    record_sandbox_llms(&ctx).await;
    name_the_site(&ctx, "Kiln & Co").await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "<h1>shop</h1>", "expected_sha256": null}),
    )
    .await;
    // The sandbox IS answering `/llms.txt` for this site, which has none…
    assert_eq!(
        ctx.storage_get("wafer-run/web", "site", "llms.txt")
            .await
            .expect("the sandbox's llms.txt, published"),
        SANDBOX_LLMS.as_bytes()
    );

    let entries = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    // …and none of it is in the archive, at the root or under the seed.
    assert!(
        !entries.keys().any(|path| path.ends_with("llms.txt")),
        "{:?}",
        sorted(&entries)
    );
    let manifest: SeedManifest =
        serde_json::from_slice(&entries["seed/manifest.json"]).expect("a seed manifest");
    assert!(manifest.sandbox.is_none());
    assert_eq!(manifest.site.len(), 1);

    // The boot page is the exported site's: its own name everywhere the
    // sandbox's title was (escaped, as the bundler escapes one), an empty
    // notice region, and not a word about a sandbox anywhere in the file.
    let index = text(&entries, "index.html");
    assert!(index.contains("<title>Kiln &amp; Co</title>"), "{index}");
    assert_eq!(
        index
            .matches("<span data-app-title>Kiln &amp; Co</span>")
            .count(),
        2,
        "{index}"
    );
    assert!(
        index.contains("<!--boot-notice--><!--/boot-notice-->"),
        "{index}"
    );
    for word in ["sandbox", "llms.txt", "/b/dev", sandbox_title.as_str()] {
        assert!(
            !index.to_lowercase().contains(&word.to_lowercase()),
            "the exported boot page of the {seed} sandbox says {word:?}: {index}"
        );
    }
    // The loader is copied as it is, and that is safe because nothing in it
    // that a visitor reads names the deployment: it takes the app's name
    // from the page above.
    let loader = text(&entries, "loader.js");
    assert!(
        loader.contains("[data-app-title]"),
        "the loader reads the page"
    );
    for line in visible_lines(&loader) {
        assert!(
            !line.to_lowercase().contains("sandbox"),
            "the exported loader shows: {line}"
        );
    }
    // The README is headed with the same name.
    assert!(text(&entries, "README.md").contains("Kiln & Co"));
}

/// A deployment that is NOT the sandbox may bundle an `llms.txt` of its own
/// in its shell — listed in its asset manifest like any shell file. The
/// export copies it: there is no rule about the name, only about what the
/// export reads. (The sandbox's own is an overlay the listing never names.)
#[tokio::test]
async fn a_shell_file_named_llms_txt_is_exported_like_any_other() {
    let bundled = b"# An app that ships its own\n";
    let shell = FakeShell::new().with("llms.txt", bundled);
    let ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_shell(FakeControl::new(), std::sync::Arc::new(shell))
        .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "x", "expected_sha256": null}),
    )
    .await;
    let exported = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    assert_eq!(
        exported.get("llms.txt").map(Vec::as_slice),
        Some(&bundled[..])
    );
    assert!(!exported.contains_key("seed/site/llms.txt"));

    // Once the site has its own, that is the one the static host is given:
    // the worker will serve the site's, and the two must say the same thing.
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/llms.txt", "content": "# The site\n", "expected_sha256": null}),
    )
    .await;
    let preview: ExportManifest = serde_json::from_value(
        output_json(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export/manifest"))
                .await,
        )
        .await,
    )
    .expect("manifest");
    let named: Vec<&str> = preview
        .files
        .iter()
        .map(|file| file.path.as_str())
        .filter(|path| path.ends_with("llms.txt"))
        .collect();
    assert_eq!(named, ["llms.txt", "seed/site/llms.txt"], "one root copy");
    let exported = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    assert_eq!(text(&exported, "llms.txt"), "# The site\n");
}

/// A site's OWN `llms.txt` is a site file like any other: writable (the path
/// is not one the service worker keeps from the runtime), exported under
/// `seed/site/` — and once more at the root, for readers with no worker —
/// and what the instance seeded from the export serves.
#[tokio::test]
async fn a_sites_own_llms_txt_is_exported_and_served_by_the_imported_instance() {
    const OWN: &str = "# Kiln & Co\n\nHandmade ceramics.\n";
    let a_control = FakeControl::new();
    let a = shop_instance(&a_control).await;
    record_sandbox_llms(&a).await;
    let written = output_json(
        dev_post(
            &a,
            "/b/dev/api/files/write",
            json!({"path": "site/llms.txt", "content": OWN, "expected_sha256": null}),
        )
        .await,
    )
    .await;
    assert_eq!(written["path"], "site/llms.txt", "{written}");
    assert_eq!(
        a.storage_get("wafer-run/web", "site", "llms.txt")
            .await
            .expect("published llms.txt"),
        OWN.as_bytes(),
        "the site's file is what the sandbox serves once it exists"
    );

    let archive = entries(
        output_body(
            a.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    // Twice, and the same bytes: under the seed, for the exported runtime to
    // import and serve once its worker controls the page; and at the root,
    // for the static host to serve a reader that never gets a worker.
    assert_eq!(text(&archive, "seed/site/llms.txt"), OWN);
    assert_eq!(archive["llms.txt"], archive["seed/site/llms.txt"]);
    // The root copy shadows nothing: `/llms.txt` is not a path the exported
    // worker leaves to the static host, so the runtime's copy is the one a
    // controlled page gets.
    assert!(!text(&archive, "sw.js").contains("llms.txt"));

    let manifest: SeedManifest =
        serde_json::from_slice(&archive["seed/manifest.json"]).expect("a seed manifest");
    let fetch = ArchiveFetch { archive };
    let b_control = FakeControl::new();
    b_control.set_validated_info(hello_info("site/hello"));
    let b = TestContext::with_products()
        .await
        .with_auth_added()
        .await
        .with_dev_added_and_shell(b_control.clone(), std::sync::Arc::new(FakeShell::new()))
        .await;
    let generation = seed::import(
        &b,
        b_control.as_ref(),
        &fake_bypass_rules(),
        &manifest,
        &fetch,
    )
    .await
    .expect("import")
    .expect("a fresh instance imports");
    activation::request(
        &b,
        &b.dev_shared(),
        GenerationCause::Seed,
        ActivationIntent::Seed {
            manifest: generation,
        },
        activation::Maintenance::Inline,
    )
    .await
    .expect("activate the imported generation");
    assert_eq!(
        b.storage_get("wafer-run/web", "site", "llms.txt")
            .await
            .expect("published llms.txt"),
        OWN.as_bytes()
    );
}

/// What a build before the type was derived stored for each file:
/// `(site path, content, the type that build stored, the table's type)`.
const STORED_THE_OLD_WAY: [(&str, &str, &str, &str); 4] = [
    (
        "notes.md",
        "# Caf\u{e9}\n",
        "text/plain; charset=utf-8",
        "text/markdown; charset=utf-8",
    ),
    (
        "data.json",
        "{\"name\":\"caf\u{e9}\"}\n",
        "application/json",
        "application/json; charset=utf-8",
    ),
    (
        "feed.xml",
        "<feed>caf\u{e9}</feed>\n",
        "application/octet-stream",
        "application/xml; charset=utf-8",
    ),
    (
        "data.csv",
        "name\ncaf\u{e9}\n",
        "application/octet-stream",
        "text/csv; charset=utf-8",
    ),
];

/// Put back what an earlier build left: a `content_type` on every stored
/// entry (the generation rows and `workspace.json`) and every published file
/// stored with that type.
async fn store_the_old_way(ctx: &TestContext) {
    let old_type = |path: &str| {
        STORED_THE_OLD_WAY
            .iter()
            .find(|(name, ..)| path.ends_with(name))
            .map_or("text/html; charset=utf-8", |(_, _, old, _)| old)
    };
    let with_types = |files: &mut Vec<serde_json::Value>| {
        for entry in files {
            let path = entry["path"].as_str().expect("path").to_string();
            entry["content_type"] = json!(old_type(&path));
        }
    };

    for row in generations::list_recent(ctx, 200).await.expect("rows") {
        let mut site: serde_json::Value =
            serde_json::from_str(&row.site_manifest_json).expect("site manifest");
        with_types(site["files"].as_array_mut().expect("files"));
        generations::replace_site_manifest(ctx, &row.id, &site.to_string(), &row.manifest_sha256)
            .await
            .expect("store the row the old way");
    }

    let ws = workspace::load(ctx).await.expect("workspace");
    let mut value = serde_json::to_value(&ws).expect("workspace json");
    for entry in value["files"].as_object_mut().expect("files").values_mut() {
        let path = entry["path"].as_str().expect("path").to_string();
        entry["content_type"] = json!(old_type(&path));
    }
    ctx.storage_put(
        "impresspress/dev",
        "",
        workspace::KEY,
        value.to_string().as_bytes(),
        "application/json",
    )
    .await
    .expect("store the workspace the old way");

    for (name, content, old, _) in STORED_THE_OLD_WAY {
        ctx.storage_put("wafer-run/web", "site", name, content.as_bytes(), old)
            .await
            .expect("publish the file the old way");
    }
}

/// A sandbox an earlier build stored types for. Its next boot's upgrade
/// serves every file with the table's type though no content changed, leaves
/// no stored type behind, and the sandbox then exports a bundle that a fresh
/// instance imports and publishes with the table's types.
#[tokio::test]
async fn a_sandbox_that_stored_old_types_serves_exports_and_imports_the_tables() {
    let a_control = FakeControl::new();
    let a = shop_instance(&a_control).await;
    for (name, content, ..) in STORED_THE_OLD_WAY {
        let written = output_json(
            dev_post(
                &a,
                "/b/dev/api/files/write",
                json!({"path": format!("site/{name}"), "content": content, "expected_sha256": null}),
            )
            .await,
        )
        .await;
        assert_eq!(written["path"], format!("site/{name}"), "{written}");
    }
    store_the_old_way(&a).await;
    workspace::load(&a)
        .await
        .expect("a stored type is read and dropped");

    let upgrade = stored_types::upgrade(&a).await.expect("upgrade");
    assert!(upgrade.republished, "{upgrade:?}");
    assert!(upgrade.workspace, "{upgrade:?}");
    assert!(upgrade.generations > 0, "{upgrade:?}");
    for (name, content, _, table) in STORED_THE_OLD_WAY {
        assert_eq!(
            a.storage_content_type("wafer-run/web", "site", name)
                .await
                .expect("published"),
            table,
            "{name} is served with the table's type"
        );
        assert_eq!(
            a.storage_get("wafer-run/web", "site", name)
                .await
                .expect("published"),
            content.as_bytes(),
            "{name} keeps its content"
        );
    }
    workspace::load(&a).await.expect("the workspace loads");
    for row in generations::list_recent(&a, 200).await.expect("rows") {
        assert!(
            !row.site_manifest_json.contains("content_type"),
            "{}",
            row.id
        );
        let manifest = generation::from_row(&row).expect("the row loads");
        assert_eq!(
            generation::manifest_sha256(&manifest).expect("hash"),
            row.manifest_sha256,
            "the stored hash covers the manifest the row now holds"
        );
    }
    assert!(
        !stored_types::upgrade(&a)
            .await
            .expect("again")
            .changed_anything(),
        "a second boot has nothing to upgrade"
    );

    let archive = entries(
        output_body(
            a.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    assert!(
        !text(&archive, "seed/manifest.json").contains("content_type"),
        "a bundle declares no content type"
    );
    let manifest: SeedManifest =
        serde_json::from_slice(&archive["seed/manifest.json"]).expect("a seed manifest");
    let fetch = ArchiveFetch { archive };
    let b_control = FakeControl::new();
    b_control.set_validated_info(hello_info("site/hello"));
    let b = TestContext::with_products()
        .await
        .with_auth_added()
        .await
        .with_dev_added_and_shell(b_control.clone(), std::sync::Arc::new(FakeShell::new()))
        .await;
    let generation = seed::import(
        &b,
        b_control.as_ref(),
        &fake_bypass_rules(),
        &manifest,
        &fetch,
    )
    .await
    .expect("import")
    .expect("a fresh instance imports");
    activation::request(
        &b,
        &b.dev_shared(),
        GenerationCause::Seed,
        ActivationIntent::Seed {
            manifest: generation,
        },
        activation::Maintenance::Inline,
    )
    .await
    .expect("activate the imported generation");
    for (name, _, _, table) in STORED_THE_OLD_WAY {
        assert_eq!(
            b.storage_content_type("wafer-run/web", "site", name)
                .await
                .expect("published by the importer"),
            table,
            "{name} is published with the table's type after import"
        );
    }
}

/// The republish can fail (here, a blob the active site names is gone).
/// The rest of that boot (the journal convergence that decides what is
/// active) must keep the active generation and its blocks, whose row still
/// stores types; the workspace must load, so the files API works; and every
/// other row is upgraded. Only the active row keeps its stored types, so
/// the next boot publishes again, and once the blob is back that boot
/// finishes the upgrade.
#[tokio::test]
async fn a_failed_republish_leaves_the_workspace_usable_and_retries_next_boot() {
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;
    for (name, content, ..) in STORED_THE_OLD_WAY {
        let written = output_json(
            dev_post(
                &ctx,
                "/b/dev/api/files/write",
                json!({"path": format!("site/{name}"), "content": content, "expected_sha256": null}),
            )
            .await,
        )
        .await;
        assert_eq!(written["path"], format!("site/{name}"), "{written}");
    }
    let before = impresspress_core::blocks::dev::repo::runtime_state::read(&ctx)
        .await
        .expect("journal");
    let active_id = before
        .active_generation_id
        .clone()
        .expect("an active generation");
    let active_blocks: Vec<String> = generation::load(&ctx, &active_id)
        .await
        .expect("the active generation")
        .1
        .blocks
        .into_iter()
        .map(|block| block.name)
        .collect();
    assert!(!active_blocks.is_empty(), "the shop serves a block");
    store_the_old_way(&ctx).await;
    let (name, content, old, table) = STORED_THE_OLD_WAY[0];
    let sha = blobs::sha256_hex(content.as_bytes());
    ctx.storage_delete("impresspress/dev", blobs::FOLDER, &sha)
        .await
        .expect("lose the blob");

    stored_types::upgrade(&ctx)
        .await
        .expect_err("the publish cannot read the lost blob");
    // The rest of this boot: the journal is converged on, and the active
    // generation, whose row still stores types, stays active with its blocks.
    let blocks: Vec<String> = activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("the boot converges")
        .into_iter()
        .map(|block| block.name)
        .collect();
    assert_eq!(blocks, active_blocks, "the boot keeps the active blocks");
    workspace::load(&ctx)
        .await
        .expect("the workspace loads whatever the publish did");
    let listed = output_json(dev_get(&ctx, "/b/dev/api/files").await).await;
    assert!(
        listed["files"]
            .as_array()
            .expect("files")
            .iter()
            .any(|f| f["path"] == format!("site/{name}")),
        "{listed}"
    );
    let left: Vec<String> = generations::list_with_stored_content_types(&ctx)
        .await
        .expect("rows")
        .into_iter()
        .map(|row| row.id)
        .collect();
    let state = impresspress_core::blocks::dev::repo::runtime_state::read(&ctx)
        .await
        .expect("journal");
    assert_eq!(
        state.active_generation_id.as_deref(),
        Some(active_id.as_str()),
        "the generation stays active"
    );
    assert_eq!(
        left,
        vec![active_id.clone()],
        "only the active row keeps its stored types"
    );
    assert_eq!(
        ctx.storage_content_type("wafer-run/web", "site", name)
            .await
            .expect("still published"),
        old,
        "the site keeps serving what it served"
    );

    blobs::put(&ctx, content.as_bytes())
        .await
        .expect("the blob is back");
    let upgrade = stored_types::upgrade(&ctx).await.expect("the next boot");
    assert!(upgrade.republished, "{upgrade:?}");
    assert_eq!(upgrade.generations, 1, "{upgrade:?}");
    assert_eq!(
        ctx.storage_content_type("wafer-run/web", "site", name)
            .await
            .expect("published"),
        table
    );
    assert!(generations::list_with_stored_content_types(&ctx)
        .await
        .expect("rows")
        .is_empty());
    let blocks: Vec<String> = activation::converge_on_boot(&ctx, &ctx.dev_shared())
        .await
        .expect("the boot converges")
        .into_iter()
        .map(|block| block.name)
        .collect();
    assert_eq!(blocks, active_blocks, "still the same blocks");
    assert_eq!(
        impresspress_core::blocks::dev::repo::runtime_state::read(&ctx)
            .await
            .expect("journal")
            .active_generation_id
            .as_deref(),
        Some(active_id.as_str())
    );
}

/// UTF-8's byte order mark, as the archive's root text files carry it.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// A site's `llms.txt` in any language. The static host serves the root copy
/// with whatever type its own table gives `.txt` — `text/plain` with no
/// charset on Cloudflare's asset server and `python3 -m http.server` — and a
/// browser decodes such a file as windows-1252 unless it starts with a byte
/// order mark. So the ROOT copy carries one; the seed copy is the site's file
/// byte for byte, because the importer verifies its hash and the exported
/// runtime serves it with `charset=utf-8`.
#[tokio::test]
async fn a_non_ascii_llms_txt_is_exported_with_a_byte_order_mark_at_the_root() {
    const OWN: &str = "# Töpferei Kiln & Co — 窯\n\n> Handgemachte Keramik…\n";
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/llms.txt", "content": OWN, "expected_sha256": null}),
    )
    .await;
    let archive = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    assert_eq!(archive["seed/site/llms.txt"], OWN.as_bytes());
    assert_eq!(archive["llms.txt"], [UTF8_BOM, OWN.as_bytes()].concat());

    // The preview counts the bytes the archive carries.
    let preview: ExportManifest = serde_json::from_value(
        output_json(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export/manifest"))
                .await,
        )
        .await,
    )
    .expect("manifest");
    let root = preview
        .files
        .iter()
        .find(|file| file.path == "llms.txt")
        .expect("the root copy is listed");
    assert_eq!(root.bytes, (UTF8_BOM.len() + OWN.len()) as u64);
}

/// The README is the archive's other root text file — the static host serves
/// it too — and it is never ASCII: the template's own dashes, and the site's
/// name and admin address, which can be in any language.
#[tokio::test]
async fn the_readme_starts_with_a_byte_order_mark() {
    // The mark is added only to a file that is not ASCII, so this test means
    // something only while the template is not — as its dashes make it.
    assert!(
        !include_str!("../src/blocks/dev/templates/export-readme.md").is_ascii(),
        "the README template is ASCII now: this test no longer pins the BOM path"
    );
    let control = FakeControl::new();
    let ctx = shop_instance(&control).await;
    let archive = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    let readme = &archive["README.md"];
    assert!(
        readme.starts_with(UTF8_BOM),
        "the README must start with a BOM"
    );
    assert!(
        !readme[UTF8_BOM.len()..].starts_with(UTF8_BOM),
        "and with exactly one"
    );
}

/// A boot page with no notice region has nothing to remove and is exported
/// byte for byte — a deployment may overlay its own.
#[tokio::test]
async fn a_boot_page_with_no_notice_region_is_exported_unchanged() {
    let page = b"<!doctype html><h1>My app</h1>";
    let shell = FakeShell::new().with("index.html", page);
    let ctx = TestContext::with_admin()
        .await
        .with_dev_added_and_shell(FakeControl::new(), std::sync::Arc::new(shell))
        .await;
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "x", "expected_sha256": null}),
    )
    .await;
    let entries = entries(
        output_body(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await,
    );
    assert_eq!(
        entries.get("index.html").map(Vec::as_slice),
        Some(&page[..])
    );
}

/// A region the export cannot delimit is a 500, never a guess at where the
/// sandbox's text ends.
#[tokio::test]
async fn a_boot_page_whose_notice_region_is_malformed_is_refused() {
    for page in [
        "<!--boot-notice--><p>sandbox</p>",
        "<p>sandbox</p><!--/boot-notice-->",
        "<!--/boot-notice--><p>sandbox</p><!--boot-notice-->",
        "<!--boot-notice-->a<!--/boot-notice--><!--boot-notice-->b<!--/boot-notice-->",
    ] {
        let shell = FakeShell::new().with("index.html", page.as_bytes());
        let ctx = TestContext::with_admin()
            .await
            .with_dev_added_and_shell(FakeControl::new(), std::sync::Arc::new(shell))
            .await;
        dev_post(
            &ctx,
            "/b/dev/api/files/write",
            json!({"path": "site/index.html", "content": "x", "expected_sha256": null}),
        )
        .await;
        let status = output_http_status(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/export"))
                .await,
        )
        .await;
        assert_eq!(status, 500, "{page}");
    }
}
