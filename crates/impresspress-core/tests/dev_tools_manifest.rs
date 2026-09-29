//! `GET /b/dev/api/tools.json` — the page-scoped WebMCP manifest, `dev_*` and
//! `shop_*` projected from the dev and products blocks' typed contracts.
//!
//! The whole file is gated on `block-dev`, like the rest of the dev block's
//! integration tests: the block does not exist in a default-feature build.
#![cfg(feature = "block-dev")]

use impresspress_core::{
    blocks::dev::test_support::FakeControl,
    test_support::{admin_msg, discovery_json_as, output_json, TestContext},
};

/// Host passed to [`discovery_json_as`] — arbitrary, but shared with the
/// other discovery-document tests (`openapi_document`,
/// `pipeline.rs`'s discovery tests) so a failure's context matches theirs.
const HOST: &str = "impresspress.example.com";

#[tokio::test]
async fn tools_json_publishes_every_selection_with_zero_refusals() {
    let ctx = TestContext::with_products()
        .await
        .with_dev_added(FakeControl::new())
        .await;
    let doc = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/tools.json"))
            .await,
    )
    .await;
    let mut names: Vec<String> = doc["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    let mut expected: Vec<String> = impresspress_core::blocks::dev::tools::SELECTIONS
        .iter()
        .map(|s| s.3.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        names, expected,
        "every curated tool is published — a missing one means a refusal"
    );
    for tool in doc["tools"].as_array().unwrap() {
        assert!(tool["inputSchema"].is_object(), "{}", tool["name"]);
        assert!(!tool["description"].as_str().unwrap().is_empty());
    }
}

/// `shop_create_offer` merges two sources into one flat `inputSchema`: the
/// path template's `{product_id}` and the `POST` body
/// (`offer_definition_schema`, `products/mod.rs:386`). Both halves must
/// survive the merge intact — a client that lost either could not build a
/// working call.
///
/// This project's brief drafted this test to check `$defs.is_object()`
/// instead, on the premise that `offer_definition_schema` already carries a
/// `$defs`-closed recursive `Condition`. That premise does not hold: the
/// schema at `products/mod.rs:386` is hand-written and deliberately flat —
/// `components`/`checkout` stay generic `{"type": "object"}` rather than
/// reaching the real (recursive) `Condition` type, precisely so nothing here
/// needs `$defs` (see the comment at `products/mod.rs:378-384` — a derived,
/// `$ref`/`$defs`-carrying schema would embed a pointer that resolves
/// against the wrong root once it is copied into an OpenAPI document).
/// Generating `/b/dev/api/tools.json` from the real `SELECTIONS` confirms
/// it: `$defs` appears in none of the 23 published tools (checked against
/// `tests/snapshots/dev.tools.json`). Asserting `$defs.is_object()` here
/// would therefore either fail against a correct implementation or have to
/// be satisfied by giving `offer_definition_schema` a real `$defs` table —
/// reversing that documented, deliberate design choice — which is out of
/// this endpoint's scope. So this checks the property the merge actually
/// has to get right for these two sources instead.
#[tokio::test]
async fn shop_create_offer_merges_its_path_and_body_schemas() {
    let ctx = TestContext::with_products()
        .await
        .with_dev_added(FakeControl::new())
        .await;
    let doc = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/tools.json"))
            .await,
    )
    .await;
    let create = doc["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "shop_create_offer")
        .unwrap();
    let input = &create["inputSchema"];
    assert_eq!(input["type"], "object", "{create}");
    // From the path template's `{product_id}` placeholder.
    assert_eq!(
        input["properties"]["product_id"]["type"], "string",
        "{create}"
    );
    // From the POST body (`offer_definition_schema`).
    assert_eq!(input["properties"]["name"]["type"], "string", "{create}");
    let required: Vec<&str> = input["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"product_id"), "{create}");
    assert!(required.contains(&"name"), "{create}");
}

#[tokio::test]
async fn no_dev_or_shop_tool_leaks_into_the_global_manifest() {
    let ctx = TestContext::with_products()
        .await
        .with_dev_added(FakeControl::new())
        .await;
    let doc = discovery_json_as(&ctx, "/b/webmcp/manifest.json", HOST, Some(&["admin"])).await;
    for tool in doc["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert!(
            !name.starts_with("dev_") && !name.starts_with("shop_"),
            "{name} leaked"
        );
    }
}

#[tokio::test]
async fn tools_json_matches_its_snapshot() {
    // Same discipline as /openapi.json: UPDATE_DEV_TOOLS_SNAPSHOT=1
    // regenerates; read every changed line.
    let ctx = TestContext::with_products()
        .await
        .with_dev_added(FakeControl::new())
        .await;
    let doc = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/tools.json"))
            .await,
    )
    .await;
    let rendered = serde_json::to_string_pretty(&doc).unwrap() + "\n";
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/snapshots/dev.tools.json"
    );
    if std::env::var_os("UPDATE_DEV_TOOLS_SNAPSHOT").is_some() {
        std::fs::write(path, &rendered).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("missing snapshot {path}: {e} — run with UPDATE_DEV_TOOLS_SNAPSHOT=1 once, then read it")
    });
    assert_eq!(
        rendered, expected,
        "tools.json changed — every changed line is a decision; regenerate deliberately with \
         UPDATE_DEV_TOOLS_SNAPSHOT=1"
    );
}
