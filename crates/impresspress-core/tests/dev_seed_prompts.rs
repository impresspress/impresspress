//! Every seed's `suggested_prompt` and site guide name only tools the
//! workspace page has.
//!
//! The prompt `/b/dev` suggests comes from the seed's `sandbox.json`
//! (`examples/dev-sandbox/seeds/<name>/sandbox.json`), and the guide
//! `dev_read_reference` serves from its `guide.md` — neither from this crate,
//! so nothing else ties a tool they name to one the page publishes. A renamed
//! or dropped tool would leave the agent a prompt or a guide that tells it to
//! call something that is not there.
//!
//! Gated on `block-dev` like the other dev tests: `blocks::dev` does not exist
//! in a default-feature build.
#![cfg(feature = "block-dev")]

use std::{collections::BTreeSet, path::PathBuf};

use impresspress_core::blocks::dev::tools::SELECTIONS;

/// Tools the workspace page registers itself rather than projecting from
/// `/b/dev/api/tools.json`: they run in the page (the in-browser compiler,
/// the bundle download), so they have no `SELECTIONS` row.
const PAGE_LOCAL_TOOLS: &[&str] = &["dev_compile_block", "dev_export"];

fn seeds_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/dev-sandbox/seeds")
}

/// Every `dev_*` / `shop_*` word in `text`: a run of `[a-z0-9_]`, so the
/// punctuation around a tool name in prose is not part of it. A word ending
/// in `_` is a family, not a tool (a guide may say `shop_*`), and is skipped.
fn tool_tokens(text: &str) -> BTreeSet<&str> {
    text.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
        .filter(|word| word.starts_with("dev_") || word.starts_with("shop_"))
        .filter(|word| !word.ends_with('_'))
        .collect()
}

/// Every tool the workspace page publishes.
fn published() -> BTreeSet<&'static str> {
    SELECTIONS
        .iter()
        .map(|(_, _, _, name, _)| *name)
        .chain(PAGE_LOCAL_TOOLS.iter().copied())
        .collect()
}

/// Every `seeds/*/<file>` that exists, with its text.
fn seed_files(file: &str) -> Vec<(PathBuf, String)> {
    let dir = seeds_dir();
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.expect("seed dir entry").path().join(file);
        if !path.is_file() {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        found.push((path, text));
    }
    // A moved seeds directory must not pass by finding nothing to check.
    assert!(
        !found.is_empty(),
        "no seeds/*/{file} under {}",
        dir.display()
    );
    found
}

#[test]
fn every_seed_prompt_names_only_tools_the_page_has() {
    let published = published();
    for (path, text) in seed_files("sandbox.json") {
        let sandbox: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let prompt = sandbox["suggested_prompt"]
            .as_str()
            .unwrap_or_else(|| panic!("{}: no suggested_prompt", path.display()));
        for tool in tool_tokens(prompt) {
            assert!(
                published.contains(tool),
                "{}: suggested_prompt names {tool}, which the workspace page does not publish",
                path.display()
            );
        }
    }
}

#[test]
fn every_seed_guide_names_only_tools_the_page_has() {
    let published = published();
    for (path, text) in seed_files("guide.md") {
        for tool in tool_tokens(&text) {
            assert!(
                published.contains(tool),
                "{}: names {tool}, which the workspace page does not publish",
                path.display()
            );
        }
    }
}

/// Every seed's `llms.txt` — what the static host serves at `/llms.txt` and
/// the importer records — is the shared preamble naming the seed's template,
/// then the seed's guide, and the committed manifest declares exactly those
/// bytes.
///
/// `seeds/seedlib.py` generates it and `check-seeds.py` checks the manifest
/// against the generator; this is the same statement made from the side that
/// reads it, so the two cannot agree with each other and disagree with the
/// importer (`seed::SeedManifest`, `seed::LLMS_PATH`, the content type and
/// the size limit). It also holds the preamble to the rule the guides are
/// held to: it names no tool the workspace page does not publish.
#[test]
fn every_seed_llms_txt_is_the_preamble_then_the_guide_as_the_manifest_declares() {
    use impresspress_core::blocks::dev::{blobs, seed};

    let preamble_path = seeds_dir().join("llms-preamble.md");
    let preamble = std::fs::read_to_string(&preamble_path)
        .unwrap_or_else(|e| panic!("{}: {e}", preamble_path.display()));
    let published = published();
    for tool in tool_tokens(&preamble) {
        assert!(
            published.contains(tool),
            "{}: names {tool}, which the workspace page does not publish",
            preamble_path.display()
        );
    }
    // The preamble's list of `dev_*` tools is the WHOLE set, not a sample: an
    // agent with only the Tool console has no other way to learn that it can
    // list, delete or roll back. Compared against what the page publishes, so
    // a tool added to the manifest fails here until the preamble names it.
    let dev_tools: BTreeSet<&str> = published
        .iter()
        .copied()
        .filter(|tool| tool.starts_with("dev_"))
        .collect();
    let named: BTreeSet<&str> = tool_tokens(&preamble)
        .into_iter()
        .filter(|tool| tool.starts_with("dev_"))
        .collect();
    assert_eq!(
        named,
        dev_tools,
        "{}: its tool list is not the workspace page's dev_* tools",
        preamble_path.display()
    );
    assert!(preamble.contains("`shop_*`"), "{}", preamble_path.display());

    // What a reader must be told before it can get in, whichever seed.
    for needle in [
        "/b/dev/enter",
        "no credentials to",
        "WebMCP",
        "Tool console",
        "dev_write_files",
        "dev_export",
        "JavaScript",
    ] {
        assert!(
            preamble.contains(needle),
            "{}: does not mention {needle:?}",
            preamble_path.display()
        );
    }

    for (path, text) in seed_files("manifest.json") {
        let manifest: seed::SeedManifest =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let sandbox = manifest
            .sandbox
            .unwrap_or_else(|| panic!("{}: no sandbox block", path.display()));
        let guide_path = path.with_file_name("guide.md");
        let guide = std::fs::read_to_string(&guide_path)
            .unwrap_or_else(|e| panic!("{}: {e}", guide_path.display()));
        let llms = format!(
            "{}\n{guide}",
            preamble.replace("{template}", &sandbox.template)
        );
        assert!(
            llms.contains(&format!("the `{}` template", sandbox.template)),
            "{}: the preamble does not name the template",
            path.display()
        );
        assert_eq!(sandbox.llms.path, seed::LLMS_PATH, "{}", path.display());
        assert_eq!(
            sandbox.llms.content_type,
            seed::LLMS_CONTENT_TYPE,
            "{}",
            path.display()
        );
        assert!(llms.len() <= seed::MAX_LLMS_BYTES, "{}", path.display());
        assert_eq!(sandbox.llms.size, llms.len() as u64, "{}", path.display());
        assert_eq!(
            sandbox.llms.sha256,
            blobs::sha256_hex(llms.as_bytes()),
            "{}: sandbox.llms is not the preamble plus this seed's guide — \
             run seeds/write-manifest.py for every seed",
            path.display()
        );
    }
}

#[test]
fn tool_tokens_splits_names_out_of_prose() {
    let found =
        tool_tokens("create three with shop_create_product, then dev_status. Any shop_* tool");
    assert_eq!(found, BTreeSet::from(["dev_status", "shop_create_product"]));
}

/// Every seed guide's "Pricing an offer" example is an argument the create-
/// offer endpoint accepts: `product_id` goes to the path, and the rest
/// deserializes into the handler's own type, `deny_unknown_fields` and all.
#[cfg(feature = "block-products")]
#[test]
fn every_guide_offer_example_is_a_valid_create_offer_argument() {
    use impresspress_core::blocks::products::contracts::OfferDefinitionRequest;
    for (path, text) in seed_files("guide.md") {
        let section = text
            .split("## Pricing an offer")
            .nth(1)
            .unwrap_or_else(|| panic!("{}: no \"## Pricing an offer\"", path.display()));
        let json = section
            .split("```json")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .unwrap_or_else(|| panic!("{}: no json block under the heading", path.display()));
        let mut argument: serde_json::Value =
            serde_json::from_str(json).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(
            argument
                .as_object_mut()
                .unwrap()
                .remove("product_id")
                .is_some(),
            "{}: the example must say where product_id goes",
            path.display()
        );
        let parsed: OfferDefinitionRequest =
            serde_json::from_value(argument).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(parsed.components.len(), 1, "{}", path.display());
    }
}
