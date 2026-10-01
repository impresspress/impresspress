//! Every seed's `suggested_prompt` names only tools the workspace page has.
//!
//! The prompt `/b/dev` suggests comes from the seed's `sandbox.json`
//! (`examples/dev-sandbox/seeds/<name>/sandbox.json`), not from this crate, so
//! nothing else ties a tool it names to one the page publishes. A renamed or
//! dropped tool would leave the agent a prompt that tells it to call
//! something that is not there.
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
/// punctuation around a tool name in prose is not part of it.
fn tool_tokens(text: &str) -> BTreeSet<&str> {
    text.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
        .filter(|word| word.starts_with("dev_") || word.starts_with("shop_"))
        .collect()
}

#[test]
fn every_seed_prompt_names_only_tools_the_page_has() {
    let published: BTreeSet<&str> = SELECTIONS
        .iter()
        .map(|(_, _, _, name, _)| *name)
        .chain(PAGE_LOCAL_TOOLS.iter().copied())
        .collect();

    let dir = seeds_dir();
    let mut checked = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let sandbox_path = entry.expect("seed dir entry").path().join("sandbox.json");
        if !sandbox_path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&sandbox_path)
            .unwrap_or_else(|e| panic!("{}: {e}", sandbox_path.display()));
        let sandbox: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", sandbox_path.display()));
        let prompt = sandbox["suggested_prompt"]
            .as_str()
            .unwrap_or_else(|| panic!("{}: no suggested_prompt", sandbox_path.display()));
        for tool in tool_tokens(prompt) {
            assert!(
                published.contains(tool),
                "{}: suggested_prompt names {tool}, which the workspace page does not publish",
                sandbox_path.display()
            );
        }
        checked.push(sandbox_path);
    }
    // A moved seeds directory must not pass by finding nothing to check.
    assert!(
        !checked.is_empty(),
        "no seeds/*/sandbox.json under {}",
        dir.display()
    );
}

#[test]
fn tool_tokens_splits_names_out_of_prose() {
    let found = tool_tokens("create three with shop_create_product, then dev_status. Done");
    assert_eq!(found, BTreeSet::from(["dev_status", "shop_create_product"]));
}
