use wafer_run::{BlockInfo, HttpMethod, InstanceMode, OutputStream};

// `ResponseBuilder` serves an asset's bytes, which only exist when the
// assets are compiled in; without the feature the block still declares and
// routes `/b/static/{filename}` and answers 404.
#[cfg(feature = "embed-assets")]
use crate::http::ResponseBuilder;
use crate::{
    endpoint_match::{self, EndpointRoute},
    http::{err_not_found, ok_json},
};

/// Resolve a hashed filename (the part after `/b/static/`) to its bytes and
/// content type. Exact-match against the build-time manifest — no prefix
/// scanning, so no ordering hazard between `itim-latin-` and `itim-latin-ext-`.
#[cfg(feature = "embed-assets")]
pub(crate) fn static_asset(filename: &str) -> Option<(&'static [u8], &'static str)> {
    let e = crate::ui::assets::ASSETS
        .iter()
        .find(|e| e.filename == filename)?;
    Some((crate::ui::assets::bytes(e.logical)?, e.content_type))
}

#[derive(Clone, Copy)]
enum Route {
    Health,
    /// One embedded asset, addressed by its content-hashed filename.
    Asset,
}

/// The block's HTTP surface. Every embedded asset (CSS, htmx, the WebMCP
/// script, fonts, logos, favicon) is served from the one `{filename}` row:
/// filenames are content-hashed (`app-{hash}.css`), the lookup is by exact
/// filename against the build-time manifest, and a stale hash is therefore a
/// 404. One row rather than one per asset keeps `itim-latin-` /
/// `itim-latin-ext-` (and the two logo sizes) from depending on table order,
/// which a per-asset `{hash}` template would reintroduce.
const ROUTES: &[EndpointRoute<Route>] = &[
    EndpointRoute::public(HttpMethod::Get, "/health", Route::Health).summary("Health check"),
    EndpointRoute::public(HttpMethod::Get, "/b/static/{filename}", Route::Asset)
        .summary("Embedded static asset (content-hashed filename)"),
];

/// Serve the manifest entry named `filename`, or 404.
fn serve_asset(filename: &str) -> OutputStream {
    #[cfg(feature = "embed-assets")]
    if let Some((body, content_type)) = static_asset(filename) {
        return ResponseBuilder::new()
            .set_header("Cache-Control", "public, max-age=31536000, immutable")
            .body(body.to_vec(), content_type);
    }
    // Either the filename is not in the manifest (a stale or made-up hash),
    // or assets were not compiled in. In the second case the deployer is
    // responsible for publishing them and pointing IMPRESSPRESS_ASSET_BASE_URL
    // at them; reaching this arm means that did not happen.
    #[cfg(not(feature = "embed-assets"))]
    let _ = filename;
    err_not_found("not found")
}

crate::impresspress_feature_block! {
    /// System health checks and embedded static assets (`impresspress/system`).
    pub struct SystemBlock;
    name: "impresspress/system",
    info: |_this| {
        BlockInfo::new("impresspress/system", "0.0.1", "http-handler@v1", "System health and embedded static assets")
            .instance_mode(InstanceMode::Singleton)
            .category(wafer_run::BlockCategory::Infrastructure)
            .description("Core system services including health checks and embedded static assets (CSS, JavaScript).")
            .endpoints(endpoint_match::declare(ROUTES))
    },
    handle: |_this, _ctx, mut msg, _input| {
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return err_not_found("not found");
        };
        match route {
            Route::Health => ok_json(&serde_json::json!({"status": "ok"})),
            Route::Asset => serve_asset(msg.var("filename")),
        }
    },
}

#[cfg(test)]
mod tests {
    // Read only by the asset-serving tests below (and by the `NopCtx` those
    // tests call `handle` with), and `serve_asset` answers 404 without
    // `embed-assets`, so those tests are gated on it.
    #[cfg(feature = "embed-assets")]
    use wafer_run::{context::Context, InputStream, OutputStream, META_RESP_CONTENT_TYPE};
    use wafer_run::{Block, Message};

    use super::*;
    #[cfg(feature = "embed-assets")]
    use crate::ui::assets;

    // The only thing that ever passes a context to `SystemBlock::handle` is an
    // asset-serving test, and those are gated on `embed-assets`.
    #[cfg(feature = "embed-assets")]
    #[derive(Clone)]
    struct NopCtx;
    #[cfg(feature = "embed-assets")]
    #[async_trait::async_trait]
    impl Context for NopCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            panic!("call_block not used");
        }
        /// Admits nothing, as the fail-closed `check_resource_access` default
        /// this context keeps does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_run::ResourceType,
            _access: wafer_block::ResourceAccess,
        ) -> bool {
            false
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    #[tokio::test]
    #[cfg(all(feature = "block-llm", feature = "embed-assets"))]
    async fn system_handle_serves_llm_chat_js() {
        let block = SystemBlock::new();
        let url = crate::blocks::llm::assets::llm_chat_js_url();
        let mut msg = Message::new(format!("retrieve:{url}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, url);

        let out = block.handle(&NopCtx, msg, InputStream::empty()).await;
        let buffered = out.collect_buffered().await.expect("response");
        let content_type = buffered
            .meta
            .iter()
            .find(|m| m.key == META_RESP_CONTENT_TYPE)
            .map(|m| m.value.as_str());
        assert_eq!(
            content_type,
            Some("application/javascript; charset=utf-8"),
            "wrong content type"
        );
        let body = std::str::from_utf8(&buffered.body).unwrap();
        assert!(
            body.contains("impresspressLlmChat"),
            "body should contain the JS module"
        );
    }

    #[tokio::test]
    #[cfg(all(feature = "block-files", feature = "embed-assets"))]
    async fn system_handle_serves_files_browser_js() {
        let block = SystemBlock::new();
        let url = crate::blocks::files::assets::files_browser_js_url();
        let mut msg = Message::new(format!("retrieve:{url}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, url);

        let out = block.handle(&NopCtx, msg, InputStream::empty()).await;
        let buffered = out.collect_buffered().await.expect("response");
        let content_type = buffered
            .meta
            .iter()
            .find(|m| m.key == META_RESP_CONTENT_TYPE)
            .map(|m| m.value.as_str());
        assert_eq!(
            content_type,
            Some("application/javascript; charset=utf-8"),
            "wrong content type"
        );
        let body = std::str::from_utf8(&buffered.body).unwrap();
        assert!(
            body.starts_with("// impresspress files-browser"),
            "body should start with the placeholder comment"
        );
    }

    #[tokio::test]
    #[cfg(all(feature = "block-llm", feature = "embed-assets"))]
    async fn system_handle_serves_marked_js() {
        let block = SystemBlock::new();
        let url = crate::blocks::llm::assets::marked_js_url();
        let mut msg = Message::new(format!("retrieve:{url}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, url);

        let out = block.handle(&NopCtx, msg, InputStream::empty()).await;
        let buffered = out.collect_buffered().await.expect("response");
        let content_type = buffered
            .meta
            .iter()
            .find(|m| m.key == META_RESP_CONTENT_TYPE)
            .map(|m| m.value.as_str());
        assert_eq!(
            content_type,
            Some("application/javascript; charset=utf-8"),
            "wrong content type"
        );
        let body = std::str::from_utf8(&buffered.body).unwrap();
        assert!(
            body.contains("marked"),
            "body should be the vendored marked.js"
        );
    }

    #[tokio::test]
    #[cfg(all(feature = "block-llm", feature = "embed-assets"))]
    async fn system_handle_serves_purify_js() {
        let block = SystemBlock::new();
        let url = crate::blocks::llm::assets::purify_js_url();
        let mut msg = Message::new(format!("retrieve:{url}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, url);

        let out = block.handle(&NopCtx, msg, InputStream::empty()).await;
        let buffered = out.collect_buffered().await.expect("response");
        let content_type = buffered
            .meta
            .iter()
            .find(|m| m.key == META_RESP_CONTENT_TYPE)
            .map(|m| m.value.as_str());
        assert_eq!(
            content_type,
            Some("application/javascript; charset=utf-8"),
            "wrong content type"
        );
        let body = std::str::from_utf8(&buffered.body).unwrap();
        assert!(
            body.contains("DOMPurify"),
            "body should be the vendored DOMPurify build"
        );
    }

    #[tokio::test]
    #[cfg(feature = "embed-assets")]
    async fn system_handle_serves_webmcp_js() {
        let block = SystemBlock::new();
        let url = assets::webmcp_js_url();
        let mut msg = Message::new(format!("retrieve:{url}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, url);

        let out = block.handle(&NopCtx, msg, InputStream::empty()).await;
        let buffered = out.collect_buffered().await.expect("response");
        let content_type = buffered
            .meta
            .iter()
            .find(|m| m.key == META_RESP_CONTENT_TYPE)
            .map(|m| m.value.as_str());
        assert_eq!(
            content_type,
            Some("application/javascript; charset=utf-8"),
            "wrong content type"
        );
        let body = std::str::from_utf8(&buffered.body).unwrap();
        assert!(
            body.contains("registerTool"),
            "body should be the WebMCP registration script"
        );
    }

    /// `declared_access` (`routing.rs`) fails closed to `AuthLevel::
    /// Authenticated` for any path a block does not declare as a
    /// `BlockEndpoint` — the one asset row in `ROUTES` is that declaration,
    /// and it is what admits an anonymous asset request: the router carries
    /// no per-path entry for `/b/static/` (proved end-to-end by
    /// `routing::tests::webmcp_script_asset_is_publicly_reachable`, which
    /// drives the real request through `route_to_block` with this block's
    /// `info()`). Checked here via `effective_access`, the resolver that
    /// mirrors exactly what the router enforces (the same one `pipeline.rs`
    /// plugs into the WebMCP manifest generator).
    #[test]
    fn webmcp_script_asset_is_publicly_reachable() {
        let info = SystemBlock::new().info();
        let ep = info
            .endpoints
            .iter()
            .find(|e| e.path == "/b/static/{filename}")
            .expect("static asset endpoint not declared in SystemBlock::info()");
        assert_eq!(
            crate::routing::effective_access(&info, ep, &[]),
            wafer_run::AuthLevel::Public,
            "the WebMCP script must load for anonymous visitors — an \
             effective auth level above Public would silently disable tools \
             on the public storefront"
        );
    }

    /// `info().endpoints` is generated from `ROUTES`.
    #[test]
    fn info_endpoints_come_from_the_table() {
        let declared = SystemBlock::new().info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }

    /// The asset row's template is the same literal the URL builders use.
    #[test]
    fn asset_row_sits_under_the_static_prefix() {
        let row = ROUTES
            .iter()
            .find(|r| matches!(r.handler, Route::Asset))
            .expect("asset row");
        assert_eq!(row.method, wafer_run::HttpMethod::Get);
        assert_eq!(row.template, "/b/static/{filename}");
        assert!(row.template.starts_with(crate::routing::STATIC_PREFIX));
        assert_eq!(row.auth, wafer_run::AuthLevel::Public);
    }

    /// Every file in the build-time manifest resolves to the asset row with
    /// its exact filename bound, whatever the hash happens to be.
    #[test]
    fn every_manifest_asset_dispatches_to_the_asset_row() {
        for entry in crate::ui::assets::ASSETS {
            let url = format!("{}{}", crate::routing::STATIC_PREFIX, entry.filename);
            let mut msg = Message::new(format!("retrieve:{url}"));
            msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
            msg.set_meta(wafer_run::META_REQ_RESOURCE, &url);
            let route = crate::endpoint_match::dispatch(&mut msg, ROUTES);
            assert!(matches!(route, Some(Route::Asset)), "{url}");
            assert_eq!(msg.var("filename"), entry.filename);
        }
    }

    /// A URL with a stale hash names a file that is not in the manifest and
    /// must 404, never receive the current bytes under an `immutable` header.
    #[tokio::test]
    #[cfg(feature = "embed-assets")]
    async fn a_stale_hash_is_not_found() {
        let block = SystemBlock::new();
        let url = "/b/static/app-0000000000000000.css";
        let mut msg = Message::new(format!("retrieve:{url}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, url);
        let out = block.handle(&NopCtx, msg, InputStream::empty()).await;
        assert!(crate::test_support::output_is_error(out, "NotFound").await);
    }

    /// A literal nested path has more than one segment after the prefix, so
    /// no row matches it and it 404s before any lookup.
    #[test]
    fn a_nested_static_path_matches_no_row() {
        let url = "/b/static/../../etc/passwd";
        let mut msg = Message::new(format!("retrieve:{url}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, url);
        assert!(crate::endpoint_match::dispatch(&mut msg, ROUTES).is_none());
    }

    /// A percent-encoded traversal is one segment on the wire, so it DOES
    /// match the `{filename}` row and reaches `serve_asset` decoded. What
    /// keeps it harmless is not the matcher but the lookup: `static_asset`
    /// is an exact-match allowlist over the build-time manifest and never
    /// touches a filesystem, so the decoded name finds nothing and 404s.
    #[tokio::test]
    #[cfg(feature = "embed-assets")]
    async fn an_encoded_traversal_binds_but_finds_no_manifest_entry() {
        let url = "/b/static/..%2F..%2Fetc%2Fpasswd";
        let mut msg = Message::new(format!("retrieve:{url}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, url);
        assert!(matches!(
            crate::endpoint_match::dispatch(&mut msg, ROUTES),
            Some(Route::Asset)
        ));
        assert_eq!(msg.var("filename"), "../../etc/passwd");

        let out = SystemBlock::new()
            .handle(&NopCtx, msg, InputStream::empty())
            .await;
        assert!(crate::test_support::output_is_error(out, "NotFound").await);
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn static_lookup_matches_exact_hashed_filename_without_ordering_hazard() {
        // The prefix-table version needed `itim-latin-ext-` to be scanned before
        // `itim-latin-`. Exact-filename matching makes that class of bug impossible.
        let ext = crate::ui::assets::entry("itim-latin-ext.woff2");
        let base = crate::ui::assets::entry("itim-latin.woff2");
        assert_ne!(ext.filename, base.filename);
        assert_eq!(super::static_asset(ext.filename).unwrap().1, "font/woff2");
        assert_eq!(super::static_asset(base.filename).unwrap().1, "font/woff2");
    }

    #[cfg(feature = "embed-assets")]
    #[test]
    fn static_lookup_rejects_unknown_and_traversal_paths() {
        assert!(super::static_asset("nope.css").is_none());
        assert!(super::static_asset("../../etc/passwd").is_none());
    }
}
