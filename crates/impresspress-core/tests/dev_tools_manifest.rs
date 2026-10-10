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
use wafer_run::context::Context;

mod descriptions;
mod dev_page_tools;

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

/// No curated tool names a `server_only` endpoint.
///
/// `/b/webmcp/manifest.json`, `/openapi.json` and the agent card drop
/// `server_only` endpoints in the browser runtime (`pipeline.rs`'s
/// `discoverable_infos`); `tools.json` projects `SELECTIONS` straight from
/// the registered blocks and applies no such filter. That is right only
/// while no selection names one: the dev block is registered by the browser
/// runtime alone (`impresspress-web`), so a `server_only` row would publish a
/// tool whose every call the sandbox refuses. A row like that is an authoring
/// error in `SELECTIONS`, and failing here names it; filtering it out at
/// serve time would only make it one fewer tool on the page.
#[tokio::test]
async fn no_selection_names_a_server_only_endpoint() {
    let ctx = TestContext::with_products()
        .await
        .with_dev_added(FakeControl::new())
        .await;
    let blocks = ctx.registered_blocks();
    for (block, method, path, tool, _) in impresspress_core::blocks::dev::tools::SELECTIONS {
        let endpoint = blocks
            .iter()
            .filter(|b| b.name == *block)
            .flat_map(|b| b.endpoints.iter())
            .find(|ep| ep.method == *method && ep.path == *path)
            .unwrap_or_else(|| panic!("{tool}: no {method:?} {path} declared by {block}"));
        assert!(
            !endpoint.server_only,
            "{tool} selects {method:?} {path}, which is server_only: the browser runtime that \
             serves tools.json refuses every call to it"
        );
    }
}

/// `shop_create_offer` merges two sources into one flat `inputSchema`: the
/// path template's `{product_id}` and the `POST` body, which is
/// `OfferDefinitionRequest`'s derived schema (the create-offer row in
/// `products/routes.rs`). Both halves must survive the merge intact — a
/// client that lost either could not build a working call.
///
/// The body reaches the recursive `Condition` (a component's `condition`
/// can hold `all`/`any`/`not` of further conditions), which no finite
/// inlining expresses, so the projection keeps it as a root-level
/// `$defs.Condition` with `"$ref": "#/$defs/Condition"` back-edges. Chrome
/// accepts that shape at `registerTool` (probed 2026-10-08). The merge must
/// carry the table along: a `$ref` left pointing at a table the merge
/// dropped would be a schema no client can resolve.
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
    // From the POST body (`OfferDefinitionRequest`).
    assert_eq!(input["properties"]["name"]["type"], "string", "{create}");
    let required: Vec<&str> = input["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"product_id"), "{create}");
    assert!(required.contains(&"name"), "{create}");
    // From the body's recursive `Condition`, closed with a root-level table.
    assert!(input["$defs"]["Condition"].is_object(), "{create}");
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

/// `tools.json` is the dev sandbox's agent surface: every tool and parameter
/// description in it is what an agent reads to decide what to send, so the
/// same no-maintainer-notes gate `openapi_snapshot.rs` runs over
/// `/openapi.json` runs here, under the `dev.tools` scope.
///
/// The page registers a few tools of its own beside `tools.json`'s
/// (`dev_compile_block`, `dev_export`), written in `dev.js` rather than
/// projected from a contract. They reach the agent the same way, so their
/// descriptions are checked too, under `dev.page`. Prose in either may name
/// a tool from the other.
#[tokio::test]
async fn tools_json_descriptions_carry_no_maintainer_notes() {
    let ctx = TestContext::with_products()
        .await
        .with_dev_added(FakeControl::new())
        .await;
    let doc = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/tools.json"))
            .await,
    )
    .await;

    let page = dev_page_tools::dev_page_tools();
    let page_tools = dev_page_tools::dev_page_tool_names();
    let failures = descriptions::check(&[
        descriptions::Scope::new("dev.tools", &doc, &[], &page_tools),
        descriptions::Scope::new("dev.page", &page, &[&doc], &[]),
    ]);
    assert!(
        failures.is_empty(),
        "dev tool descriptions carry maintainer notes - keep the caller-facing meaning in \
         `///` (or, for a page tool, in its `description`), move the rest to a `//` comment \
         beside the code:\n{}",
        failures.join("\n---\n")
    );
}

/// The page tools are read off `dev.js` by a small JavaScript reader, so it
/// must be shown to read what the page registers: both tools, each with its
/// description, the input field's description, and no `execute`.
#[tokio::test]
async fn the_page_tool_reader_reads_every_published_part() {
    let page = dev_page_tools::dev_page_tools();
    let tools = page["tools"].as_array().expect("tools");
    let compile = tools
        .iter()
        .find(|tool| tool["name"] == "dev_compile_block")
        .expect("dev_compile_block");
    // A line continuation inside the literal joins its lines with nothing:
    // `the only \` then `dependency` reads as one sentence.
    let description = compile["description"].as_str().expect("description");
    assert!(
        description.contains("the only dependency") && !description.contains('\n'),
        "{description}"
    );
    assert_eq!(
        compile["inputSchema"]["properties"]["name"]["description"],
        "Block name, as used in blocks/<name>/",
        "{compile}"
    );
    assert_eq!(compile["inputSchema"]["required"][0], "name", "{compile}");
    for tool in tools {
        assert!(tool.get("execute").is_none(), "{tool}");
        assert!(
            tool["description"].as_str().is_some_and(|d| !d.is_empty()),
            "{tool}"
        );
    }
}
