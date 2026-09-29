//! Scaffolding a block and reading the authoring reference —
//! `POST /b/dev/api/blocks` and `GET /b/dev/api/reference`.
//!
//! Gated on `block-dev` for the same reason the other `dev_*.rs` files are:
//! the block does not exist in a default-feature build, so these tests must
//! not compile there.
#![cfg(feature = "block-dev")]

use base64ct::{Base64, Encoding};
use impresspress_core::{
    blocks::dev::{
        blobs, paths,
        scaffold::Template,
        test_support::{dev_post, hello_info, FakeControl},
        workspace, RuntimeControl, WAFER_GUEST_VERSION,
    },
    test_support::{admin_msg, output_http_status, output_json, TestContext},
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read one workspace file's content back through the files API.
async fn read_file(ctx: &TestContext, path: &str) -> String {
    let body =
        output_json(dev_post(ctx, "/b/dev/api/files/read", json!({"path": path})).await).await;
    body["content"]
        .as_str()
        .unwrap_or_else(|| panic!("read {path} returned no content: {body}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// POST /b/dev/api/blocks
// ---------------------------------------------------------------------------

/// The endpoint writes three files: the crate manifest, the instantiated
/// template, and the vendored support module verbatim.
#[tokio::test]
async fn create_block_writes_the_template_and_the_module() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let created = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/blocks",
            json!({"name": "newsletter", "template": "table"}),
        )
        .await,
    )
    .await;

    assert_eq!(created["name"], "newsletter");
    let paths: Vec<&str> = created["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| f["path"].as_str().expect("path"))
        .collect();
    assert_eq!(
        paths,
        vec![
            "blocks/newsletter/Cargo.toml",
            "blocks/newsletter/src/lib.rs",
            "blocks/newsletter/src/wafer_guest.rs",
        ]
    );

    let lib = read_file(&ctx, "blocks/newsletter/src/lib.rs").await;
    assert!(
        lib.contains("site/newsletter"),
        "the template is instantiated with the block name"
    );
    assert!(lib.contains("site__newsletter__subscribers"));
    assert!(read_file(&ctx, "blocks/newsletter/Cargo.toml")
        .await
        .contains(r#"name = "newsletter""#));
    // The support module is the canonical bytes, not a rendering of them.
    assert_eq!(
        read_file(&ctx, "blocks/newsletter/src/wafer_guest.rs").await,
        Template::WAFER_GUEST
    );

    // A second create over the same directory is a conflict, whichever
    // template it names — overwriting two of three files would leave a crate
    // that is neither what the author wrote nor what the template is.
    let again = dev_post(
        &ctx,
        "/b/dev/api/blocks",
        json!({"name": "newsletter", "template": "hello"}),
    )
    .await;
    assert_eq!(output_http_status(again).await, 409);

    let bad = dev_post(
        &ctx,
        "/b/dev/api/blocks",
        json!({"name": "Bad-Name", "template": "hello"}),
    )
    .await;
    assert_eq!(output_http_status(bad).await, 400);
}

/// A block name is a directory, a crate name and half a block id, so the
/// instantiation has to reach all three — and stop there.
#[tokio::test]
async fn a_hyphenated_name_reaches_every_place_the_name_is_load_bearing() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    output_json(
        dev_post(
            &ctx,
            "/b/dev/api/blocks",
            json!({"name": "my-shop", "template": "table"}),
        )
        .await,
    )
    .await;

    let lib = read_file(&ctx, "blocks/my-shop/src/lib.rs").await;
    assert!(lib.contains(r#"Block::new("site/my-shop""#), "{lib}");
    assert!(lib.contains("/b/my-shop/subscribe"), "{lib}");
    // The collection is spelled the way the runtime spells the block's
    // namespace, a hyphen as `_`: the one spelling validation accepts.
    assert!(lib.contains("site__my_shop__subscribers"), "{lib}");
    assert!(!lib.contains("site__my-shop__"), "{lib}");
    assert!(!lib.contains("site/newsletter"), "no stale block id: {lib}");
    assert!(
        !lib.contains("site__newsletter__"),
        "no stale collection: {lib}"
    );
    // The handler names are not the block's name and must survive: a blanket
    // replace would have produced `fn subscribe_my-shop`.
    assert!(lib.contains("fn subscribe("), "{lib}");
    assert!(read_file(&ctx, "blocks/my-shop/Cargo.toml")
        .await
        .contains(r#"name = "my-shop""#));
}

/// The `hello` template is the other starting point, and it claims nothing.
#[tokio::test]
async fn the_hello_template_is_a_block_that_claims_nothing() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    output_json(
        dev_post(
            &ctx,
            "/b/dev/api/blocks",
            json!({"name": "greeter", "template": "hello"}),
        )
        .await,
    )
    .await;
    let lib = read_file(&ctx, "blocks/greeter/src/lib.rs").await;
    assert!(lib.contains(r#"Block::new("site/greeter""#), "{lib}");
    assert!(lib.contains(r#""/b/greeter/", hello"#), "{lib}");
    assert!(!lib.contains(".collection("), "{lib}");
}

/// A template name outside the two is a `400` from serde, not an empty block.
#[tokio::test]
async fn an_unknown_template_is_refused() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let refused = dev_post(
        &ctx,
        "/b/dev/api/blocks",
        json!({"name": "thing", "template": "kitchen-sink"}),
    )
    .await;
    assert_eq!(output_http_status(refused).await, 400);
    // Nothing was written.
    let listed = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/files"))
            .await,
    )
    .await;
    assert!(listed["files"].as_array().expect("files").is_empty());
}

/// A create that runs out of quota part-way stores nothing at all.
///
/// The endpoint writes three files, and the quota is a running total: a
/// workspace with room for the first and not the second used to store the
/// first blob and then return without saving, so the bytes it had just put in
/// the store were never charged for. `check_quotas` bounds on `blob_bytes`
/// exactly because an unreferenced blob still occupies the author's storage,
/// so every such refusal left a permanent hole — and a caller sitting on the
/// limit could retry its way past `MAX_WORKSPACE_BYTES`.
#[tokio::test]
async fn a_create_that_runs_out_of_quota_part_way_stores_nothing() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let files = Template::Hello.files("cramped");
    let (first_path, first_content) = &files[0];
    let headroom = first_content.len() as u64;

    // Room for the FIRST template file exactly, and for nothing after it.
    // Staged rather than written, for the reason `dev_files.rs` stages the
    // same shape: the accumulation itself is covered there, and this pins the
    // refusal without spending 64 MB.
    let ws = workspace::Workspace {
        blob_bytes: paths::MAX_WORKSPACE_BYTES - headroom,
        ..Default::default()
    };
    workspace::save(&ctx, &ws)
        .await
        .expect("stage a nearly full workspace");

    let refused = dev_post(
        &ctx,
        "/b/dev/api/blocks",
        json!({"name": "cramped", "template": "hello"}),
    )
    .await;
    assert_eq!(output_http_status(refused).await, 413);

    let after = workspace::load(&ctx).await.expect("load workspace");
    assert!(after.files.is_empty(), "no entry survives the refusal");
    assert_eq!(
        after.blob_bytes,
        paths::MAX_WORKSPACE_BYTES - headroom,
        "and the accounting is exactly where it was"
    );
    assert_eq!(after.blob_count, 0);
    // The real check: the file the old code had already put in the store
    // before it looked at the second one is not there.
    assert!(
        !blobs::exists(&ctx, &blobs::sha256_hex(first_content.as_bytes()))
            .await
            .expect("probe the blob store"),
        "{first_path} was stored despite the refusal, so its bytes are unaccounted for"
    );

    // Which is what keeps the limit a limit: retrying gets the same refusal
    // and never eats into the headroom.
    let again = dev_post(
        &ctx,
        "/b/dev/api/blocks",
        json!({"name": "cramped", "template": "hello"}),
    )
    .await;
    assert_eq!(output_http_status(again).await, 413);
    assert_eq!(
        workspace::load(&ctx).await.expect("reload").blob_bytes,
        paths::MAX_WORKSPACE_BYTES - headroom
    );
}

/// Scaffolding does not publish: block source reaches the runtime only
/// through a compile, so nothing was activated.
#[tokio::test]
async fn scaffolding_activates_nothing() {
    let control = FakeControl::new();
    let ctx = TestContext::with_dev(control.clone()).await;
    output_json(
        dev_post(
            &ctx,
            "/b/dev/api/blocks",
            json!({"name": "greeter", "template": "hello"}),
        )
        .await,
    )
    .await;
    assert!(control.rebuilds().is_empty());
    assert_eq!(control.runtime_generation(), 0);
}

// ---------------------------------------------------------------------------
// GET /b/dev/api/reference
// ---------------------------------------------------------------------------

/// The reference is the one artifact an agent has to read before writing
/// Rust, so it has to actually carry the rules.
#[tokio::test]
async fn reference_returns_the_authoring_guide() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let body = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/reference"))
            .await,
    )
    .await;
    assert_eq!(body["wafer_guest_version"], WAFER_GUEST_VERSION);
    // The module a block built against an older copy writes over its own:
    // the stale-module diagnostic points here.
    assert_eq!(
        body["wafer_guest_module"].as_str(),
        Some(Template::WAFER_GUEST)
    );

    let markdown = body["markdown"].as_str().expect("markdown");
    for needle in [
        "Block::new",
        "db::ensure_table",
        "agent_tool",
        "site__<name>__",
        "wasm32-wasip1",
        "no dependencies",
    ] {
        assert!(
            markdown.contains(needle),
            "the reference must cover {needle}"
        );
    }
    // The two long samples ARE the templates, spliced in at render time, so
    // the guide and the scaffolder cannot drift.
    assert!(markdown.contains(Template::Hello.lib_rs().trim_end()));
    assert!(markdown.contains(Template::Table.lib_rs().trim_end()));
}

// ---------------------------------------------------------------------------
// The guest-module version gate
// ---------------------------------------------------------------------------

/// A block compiled against an older `wafer_guest.rs` is refused with a coded
/// diagnostic, before the module is loaded.
#[tokio::test]
async fn staging_with_a_stale_module_version_is_a_diagnostic() {
    let control = FakeControl::new();
    control.set_validated_info(hello_info("site/hello"));
    let ctx = TestContext::with_dev(control.clone()).await;
    let body = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/builds/stage",
            json!({
                "block_name": "hello",
                "artifact_base64": Base64::encode_string(b"\0asm"),
                "compiler_version": "t",
                "diagnostics": [],
                "wafer_guest_version": 0,
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["success"], false);
    assert_eq!(body["diagnostics"][0]["code"], "wafer-guest-version");
    // Refused before the artifact was executed: nothing was inspected and
    // nothing was activated.
    assert_eq!(control.inspections(), 0);
    assert!(control.rebuilds().is_empty());
}

/// The current version is accepted and recorded on the activated spec.
#[tokio::test]
async fn staging_records_the_module_version_it_was_built_against() {
    let control = FakeControl::new();
    control.set_validated_info(hello_info("site/hello"));
    let ctx = TestContext::with_dev(control.clone()).await;
    let body = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/builds/stage",
            json!({
                "block_name": "hello",
                "artifact_base64": Base64::encode_string(b"\0asm\x01\0\0\0"),
                "compiler_version": "t",
                "diagnostics": [],
                "wafer_guest_version": WAFER_GUEST_VERSION,
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["success"], true, "{body}");
    let rebuilt = control.rebuilds();
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(rebuilt[0][0].wafer_guest_version, WAFER_GUEST_VERSION);
}

/// A compiler that could not read the file reports nothing, and nothing is
/// checked — the spec records `0`, "unknown".
#[tokio::test]
async fn an_unreported_module_version_is_recorded_as_unknown() {
    let control = FakeControl::new();
    control.set_validated_info(hello_info("site/hello"));
    let ctx = TestContext::with_dev(control.clone()).await;
    let body = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/builds/stage",
            json!({
                "block_name": "hello",
                "artifact_base64": Base64::encode_string(b"\0asm\x01\0\0\0"),
                "compiler_version": "t",
                "diagnostics": [],
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["success"], true, "{body}");
    assert_eq!(control.rebuilds()[0][0].wafer_guest_version, 0);
}
