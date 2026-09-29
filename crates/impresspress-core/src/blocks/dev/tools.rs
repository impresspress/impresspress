//! `GET /b/dev/api/tools.json` — the page-scoped WebMCP manifest.
//!
//! `/b/webmcp/manifest.json` (`pipeline.rs`) publishes every endpoint any
//! block opted into with `.agent_tool(..)`, deployment-wide. The `/b/dev`
//! page wants something narrower and page-specific instead: a curated
//! allowlist of the dev block's own control-plane endpoints (`dev_*`) plus
//! the products admin endpoints an agent needs to build a storefront
//! (`shop_*`) — so the in-page agent sees exactly the tools this page's
//! workflow is about, not the deployment's whole surface.
//!
//! [`SELECTIONS`] is that allowlist, and [`handle`] is what projects it: both
//! are [`wafer_core::discovery::generate_webmcp_selected`], which builds this
//! exact manifest shape from a caller-chosen `(block, method, path)` list
//! rather than from `agent_tool` annotations on the selected endpoints (the
//! selected products endpoints carry none — the manifest below is the only
//! place they become tools at all).

use wafer_core::discovery::{
    generate_webmcp_selected, ToolSelection, WebMcpRefusal, WebMcpRefusalReport,
};
use wafer_run::{context::Context, AuthLevel, BlockInfo, HttpMethod, OutputStream};

/// The curated tool list this manifest publishes: `(block, method, path,
/// tool name, description)`.
///
/// `block`/`method`/`path` name an endpoint exactly as its owning block
/// declared it in `BlockInfo::endpoints` — a typo here is a
/// [`wafer_core::discovery::WebMcpRefusal::SelectionNotFound`] refusal, not a
/// compile error, which is why `tools_json_publishes_every_selection_with_zero_refusals`
/// (`tests/dev_tools_manifest.rs`) asserts the published tool names equal
/// this list's names exactly: a silently dropped row would otherwise only
/// show up as one fewer tool on the page.
///
/// Every mutating tool's description names its side effect — the agent
/// deciding whether to call it has to be able to tell from the description
/// alone, the same way a human would from a button's label.
pub const SELECTIONS: &[(&str, HttpMethod, &str, &str, &str)] = &[
    // -- dev_* : the sandbox control plane -----------------------------
    (
        "impresspress/dev",
        HttpMethod::Get,
        "/b/dev/api/status",
        "dev_status",
        "Read the sandbox state: active generation, runtime generation, active blocks, any \
         activation in progress. Call this first.",
    ),
    (
        "impresspress/dev",
        HttpMethod::Get,
        "/b/dev/api/files",
        "dev_list_files",
        "List files in the sandbox workspace, optionally filtered to those whose path starts \
         with `prefix` (e.g. `site/` or `blocks/hello/`).",
    ),
    (
        "impresspress/dev",
        HttpMethod::Post,
        "/b/dev/api/files/read",
        "dev_read_file",
        "Read one workspace file's content and its SHA-256. Pass the hash back as \
         `expected_sha256` on `dev_write_file` or `dev_delete_file` to avoid overwriting an \
         edit you have not seen.",
    ),
    (
        "impresspress/dev",
        HttpMethod::Post,
        "/b/dev/api/files/write",
        "dev_write_file",
        "Write a workspace file under `site/` or `blocks/<name>/`. Writing under `site/` \
         publishes a new generation immediately, live on the sandbox; writing under `blocks/` \
         only stages source — nothing serves it until the block is compiled and staged.",
    ),
    (
        "impresspress/dev",
        HttpMethod::Post,
        "/b/dev/api/files/delete",
        "dev_delete_file",
        "Delete a workspace file. Requires the file's current SHA-256 as `expected_sha256`; a \
         mismatch is refused so you never delete over an edit you have not seen. Deleting under \
         `site/` publishes a new generation; deleting under `blocks/` does not.",
    ),
    (
        "impresspress/dev",
        HttpMethod::Get,
        "/b/dev/api/generations",
        "dev_list_generations",
        "List the publication ledger, newest first: every generation the sandbox has created.",
    ),
    (
        "impresspress/dev",
        HttpMethod::Get,
        "/b/dev/api/generations/{id}",
        "dev_get_generation",
        "Read one generation's full manifest — its site files and its blocks — and what it \
         changed relative to the generation it was derived from.",
    ),
    (
        "impresspress/dev",
        HttpMethod::Post,
        "/b/dev/api/generations/{id}/rollback",
        "dev_rollback",
        "Republish an earlier generation as the active one, reverting every site and block \
         change made since. Call `dev_list_generations` first to find the `id` to roll back to.",
    ),
    (
        "impresspress/dev",
        HttpMethod::Get,
        "/b/dev/api/reference",
        "dev_read_reference",
        "The authoring reference for backend blocks: API, host services, limits, and the two \
         templates. Read it before writing Rust.",
    ),
    (
        "impresspress/dev",
        HttpMethod::Post,
        "/b/dev/api/blocks",
        "dev_create_block",
        "Scaffold a new backend block from a template (`hello` or `table`) under \
         blocks/<name>/. Compile it with dev_compile_block.",
    ),
    (
        "impresspress/dev",
        HttpMethod::Post,
        "/b/dev/api/blocks/{name}/remove",
        "dev_remove_block",
        "Remove a compiled block from the running site and activate the resulting generation. \
         Its source under `blocks/<name>/` is untouched — this only takes the compiled block out \
         of what serves.",
    ),
    // `dev_export` itself is NOT here: its result is a file the browser
    // downloads, not a tool result, so `dev.js` registers it page-locally
    // (there is no HTTP tool call whose answer is a 15 MB zip). Its manifest
    // is a different matter — it is small JSON, it is a read, and it is the
    // only way an agent can see what an export would contain before asking
    // for one.
    (
        "impresspress/dev",
        HttpMethod::Get,
        "/b/dev/api/export/manifest",
        "dev_export_manifest",
        "What exporting the site right now would produce: every file of the bundle with its \
         size, the totals, and the rows of each data table. A read — use `dev_export` to \
         actually download it.",
    ),
    // -- shop_* : the products admin API ------------------------------
    (
        "impresspress/products",
        HttpMethod::Get,
        "/b/products/api/admin/products",
        "shop_list_products",
        "List products with their status (`draft`, `active`, `archived`) and moderation state.",
    ),
    (
        "impresspress/products",
        HttpMethod::Post,
        "/b/products/api/admin/products",
        "shop_create_product",
        "Create a product. It starts in `draft` status and is invisible to shoppers until \
         `shop_update_product` sets `status: \"active\"`.",
    ),
    (
        "impresspress/products",
        HttpMethod::Patch,
        "/b/products/api/admin/products/{id}",
        "shop_update_product",
        "Update a product's fields, including `status`. Set `status: \"active\"` to make a \
         draft product visible to shoppers, or `\"archived\"` to retire it without deleting it.",
    ),
    (
        "impresspress/products",
        HttpMethod::Delete,
        "/b/products/api/admin/products/{id}",
        "shop_delete_product",
        "Soft-delete a product, hiding it from shoppers immediately. Call \
         `shop_restore_product` to undo.",
    ),
    (
        "impresspress/products",
        HttpMethod::Post,
        "/b/products/api/admin/products/{id}/restore",
        "shop_restore_product",
        "Restore a soft-deleted product, undoing `shop_delete_product` and clearing \
         `deleted_at`. The product is not editable through `shop_update_product` until this \
         call succeeds.",
    ),
    (
        "impresspress/products",
        HttpMethod::Get,
        "/b/products/api/admin/groups",
        "shop_list_groups",
        "List the product groups used to organize the catalog.",
    ),
    (
        "impresspress/products",
        HttpMethod::Post,
        "/b/products/api/admin/groups",
        "shop_create_group",
        "Create a product group. Assign a product to it by setting that product's `group_id` \
         with `shop_update_product`.",
    ),
    (
        "impresspress/products",
        HttpMethod::Get,
        "/b/products/api/admin/products/{product_id}/offers",
        "shop_list_offers",
        "List a product's pricing offers, draft and published.",
    ),
    (
        "impresspress/products",
        HttpMethod::Post,
        "/b/products/api/admin/products/{product_id}/offers",
        "shop_create_offer",
        "Create a pricing offer for a product. It starts in `draft` status, unpurchasable until \
         `shop_publish_offer` publishes it.",
    ),
    (
        "impresspress/products",
        HttpMethod::Patch,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}",
        "shop_update_offer",
        "Update a draft offer's pricing definition. Only a `draft` offer is editable this way — \
         a published offer must be archived with `shop_archive_offer` and recreated with \
         `shop_create_offer` to change its pricing.",
    ),
    (
        "impresspress/products",
        HttpMethod::Post,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/publish",
        "shop_publish_offer",
        "Publish a draft offer, making it purchasable by shoppers.",
    ),
    (
        "impresspress/products",
        HttpMethod::Delete,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}",
        "shop_archive_offer",
        "Archive an offer, removing it from sale immediately without deleting its history.",
    ),
];

/// [`SELECTIONS`] projected against `blocks`: the manifest to serve, and
/// every row that could not be projected.
///
/// The one place the projection is expressed, so the document [`handle`]
/// serves and the diagnostic [`log_selection_refusals`] reports are derived
/// from the same call rather than two that could drift apart.
///
/// `|_b, ep| ep.auth` (rather than a router-aware resolver like
/// `routing::effective_access`) is correct here specifically because every
/// row in [`SELECTIONS`] names an `Admin`-declared endpoint mounted under an
/// `Admin`-only prefix (`/b/dev`'s own routes, and `/b/products/api/admin/*`)
/// — there is no tier this route's mount could raise the ceiling to that the
/// declaration does not already claim. See `pipeline.rs`'s
/// `/b/webmcp/manifest.json` handler for the case where that is not true.
fn generate(blocks: &[BlockInfo]) -> (serde_json::Value, Vec<WebMcpRefusalReport>) {
    let selections: Vec<ToolSelection> = SELECTIONS
        .iter()
        .map(|(block, method, path, name, description)| ToolSelection {
            block: (*block).into(),
            method: *method,
            path: (*path).into(),
            name: (*name).into(),
            description: (*description).into(),
        })
        .collect();
    generate_webmcp_selected(blocks, AuthLevel::Admin, |_b, ep| ep.auth, &selections)
}

/// Serve the manifest: [`SELECTIONS`] projected against the runtime's
/// actually registered blocks.
///
/// Refusals are discarded on purpose. They are a property of the BUILD — a
/// `SELECTIONS` row naming a block this build did not compile in, or a typo
/// in a row — identical for every call, and `/b/dev/api/tools.json` is
/// re-derived on every GET (it is `no-store`, and the page fetches it on
/// load and again behind "Refresh tools"). Logging them here would emit the
/// same N lines per request, at the mercy of whoever is clicking: the exact
/// amplification `pipeline.rs`'s manifest handler documents and avoids. They
/// are computed and logged once instead, at runtime construction, by
/// [`log_selection_refusals`] — the diagnostic moved, it was not dropped.
pub async fn handle(ctx: &dyn Context) -> OutputStream {
    let (doc, _refused) = generate(ctx.registered_blocks());
    super::no_store().json(&doc)
}

/// Log what [`SELECTIONS`] could not project, once, from the block set the
/// runtime was built with.
///
/// Called from `builder::registration::build()`, alongside the deployment-
/// wide WebMCP refusal pass it mirrors — the one place refusals are turned
/// into operator-visible warnings. `build()` runs once per `Wafer`
/// construction (on Cloudflare Workers, once per isolate), so this is
/// bounded by deploys and sandbox activations rather than by requests.
///
/// A build that compiled the dev block in but did not REGISTER it never
/// serves [`handle`], and every `SELECTIONS` row would refuse for the same
/// uninteresting reason. Returning early keeps those 21 lines out of the
/// logs of every runtime that simply is not a sandbox.
pub(crate) fn log_selection_refusals(blocks: &[BlockInfo]) {
    if !blocks.iter().any(|b| b.name == super::BLOCK_NAME) {
        return;
    }
    let (_doc, refused) = generate(blocks);
    for r in &refused {
        match r.reason {
            // Expected whenever this build was compiled without
            // `block-products`: the products rows in `SELECTIONS` name a
            // block that simply is not registered, which is a build
            // configuration, not a defect in this manifest's own
            // declarations.
            WebMcpRefusal::SelectionNotFound => tracing::warn!(
                block = %r.block,
                path = %r.path,
                tool = %r.tool_name,
                reason = %r.reason,
                "dev tools.json: selection not found — its block is likely absent from this build",
            ),
            // Every other reason is a defect in this file's own
            // declarations (a schema wall the selected endpoint's contract
            // hits) and should never fire against the endpoints
            // `SELECTIONS` actually names.
            _ => tracing::error!(
                block = %r.block,
                path = %r.path,
                tool = %r.tool_name,
                reason = %r.reason,
                "dev tools.json refusal",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::dev::test_support::FakeControl,
        test_support::{MessageCapture, TestContext},
    };

    /// A substring of the `SelectionNotFound` warning, matched rather than
    /// the whole line so these tests do not depend on the wording staying
    /// byte-for-byte in sync with production.
    const NOT_FOUND_WARNING: &str = "dev tools.json: selection not found";

    /// A fixture where `SELECTIONS` really does refuse: the dev block is
    /// registered, `impresspress/products` is not, so every `shop_*` row
    /// names a block that is not there. Without that asymmetry the tests
    /// below would pass vacuously against zero refusals.
    async fn dev_without_products() -> TestContext {
        TestContext::with_dev(FakeControl::new()).await
    }

    /// How many `SELECTIONS` rows name `impresspress/products` — the exact
    /// number of refusals the fixture above must produce, derived from the
    /// list rather than hard-coded so adding a `shop_*` row cannot silently
    /// weaken the assertions.
    fn products_rows() -> usize {
        SELECTIONS
            .iter()
            .filter(|(block, ..)| *block == "impresspress/products")
            .count()
    }

    #[tokio::test]
    async fn serving_the_manifest_logs_nothing_however_many_times_it_is_asked() {
        let ctx = dev_without_products().await;

        // Precondition: this fixture genuinely refuses.
        let (_doc, refused) = generate(ctx.registered_blocks());
        assert_eq!(
            refused.len(),
            products_rows(),
            "precondition: every products row must refuse when that block is absent: {refused:?}"
        );

        let capture = MessageCapture::default();
        let guard = tracing::subscriber::set_default(capture.clone());
        // More than once: the defect is per-REQUEST amplification, so a
        // single silent call would be weak evidence. The page fetches this
        // on load and again on every "Refresh tools" click.
        let _first = handle(&ctx).await;
        let _second = handle(&ctx).await;
        let _third = handle(&ctx).await;
        drop(guard);

        assert_eq!(
            capture.count_containing(NOT_FOUND_WARNING),
            0,
            "the per-request path must log no refusals — they are a property of the build, \
             identical for every call, and logged once at runtime construction instead"
        );
    }

    #[tokio::test]
    async fn the_build_time_pass_reports_each_refusal_once() {
        let ctx = dev_without_products().await;

        let capture = MessageCapture::default();
        let guard = tracing::subscriber::set_default(capture.clone());
        log_selection_refusals(ctx.registered_blocks());
        drop(guard);

        assert_eq!(
            capture.count_containing(NOT_FOUND_WARNING),
            products_rows(),
            "the diagnostic moved to runtime construction — it must not have been dropped: an \
             operator wondering why the page is missing its shop_* tools still has to be able \
             to find out"
        );
    }

    #[tokio::test]
    async fn a_runtime_without_the_dev_block_reports_nothing() {
        // Compiling `block-dev` in is not the same as registering the block.
        // Every deployment that is not a sandbox is in this state, never
        // serves `/b/dev/api/tools.json`, and must not pay a warn line per
        // `SELECTIONS` row for a manifest it does not publish.
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::dev::BLOCK_NAME);
        assert!(
            !ctx.registered_blocks()
                .iter()
                .any(|b| b.name == crate::blocks::dev::BLOCK_NAME),
            "precondition: the dev block must be absent from this fixture"
        );

        let capture = MessageCapture::default();
        let guard = tracing::subscriber::set_default(capture.clone());
        log_selection_refusals(ctx.registered_blocks());
        drop(guard);

        assert_eq!(capture.count_containing(NOT_FOUND_WARNING), 0);
    }

    /// Moving the logging must not have changed a byte of what the page
    /// receives.
    #[tokio::test]
    async fn refusals_do_not_change_what_is_published() {
        let ctx = dev_without_products().await;
        let (doc, _refused) = generate(ctx.registered_blocks());
        let names: Vec<&str> = doc["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().expect("tool name"))
            .collect();
        assert!(
            names.iter().all(|n| n.starts_with("dev_")),
            "a build without the products block publishes exactly its dev_* rows: {names:?}"
        );
        assert_eq!(names.len(), SELECTIONS.len() - products_rows());
    }
}
