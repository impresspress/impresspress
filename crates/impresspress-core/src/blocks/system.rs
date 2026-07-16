use wafer_run::{BlockEndpoint, BlockInfo, InstanceMode};

use crate::{
    http::{err_not_found, ok_json, ResponseBuilder},
    ui,
};

crate::impresspress_feature_block! {
    /// System health checks and embedded static assets (`impresspress/system`).
    pub struct SystemBlock;
    name: "impresspress/system",
    info: |_this| {
        // Base set: assets served regardless of feature flags. The LLM
        // (marked/purify/llm-chat) and Files (files-browser) assets are
        // conditionally appended below — they're feature-gated in
        // `ui::assets` behind `block-llm`/`block-files` respectively (those
        // blocks are their only consumers), so a build without the block
        // doesn't advertise an endpoint it can't serve.
        #[allow(unused_mut)]
        let mut endpoints = vec![
            BlockEndpoint::get("/health").summary("Health check"),
            BlockEndpoint::get("/b/static/app-{hash}.css").summary("Embedded CSS"),
            BlockEndpoint::get("/b/static/htmx-{hash}.min.js").summary("Embedded htmx JS"),
            BlockEndpoint::get("/b/static/itim-latin-{hash}.woff2").summary("Embedded Itim font (latin)"),
            BlockEndpoint::get("/b/static/itim-latin-ext-{hash}.woff2").summary("Embedded Itim font (latin-ext)"),
            BlockEndpoint::get("/b/static/impresspress-logo-{hash}.png").summary("Embedded Impresspress square logo"),
            BlockEndpoint::get("/b/static/impresspress-logo-long-{hash}.png").summary("Embedded Impresspress wordmark logo"),
            BlockEndpoint::get("/b/static/favicon-{hash}.ico").summary("Embedded Impresspress favicon"),
        ];
        #[cfg(feature = "block-llm")]
        endpoints.extend([
            BlockEndpoint::get("/b/static/marked-{hash}.min.js").summary("Embedded marked.js"),
            BlockEndpoint::get("/b/static/purify-{hash}.js").summary("Embedded DOMPurify JS"),
            BlockEndpoint::get("/b/static/llm-chat-{hash}.js").summary("Embedded LLM chat JS"),
        ]);
        #[cfg(feature = "block-files")]
        endpoints.push(
            BlockEndpoint::get("/b/static/files-browser-{hash}.js")
                .summary("Embedded files-browser JS"),
        );
        BlockInfo::new("impresspress/system", "0.0.1", "http-handler@v1", "System health and embedded static assets")
            .instance_mode(InstanceMode::Singleton)
            .category(wafer_run::BlockCategory::Infrastructure)
            .description("Core system services including health checks and embedded static assets (CSS, JavaScript).")
            .endpoints(endpoints)
    },
    handle: |_this, _ctx, msg, _input| {
        let path = msg.path();

        if path == "/health" {
            return ok_json(&serde_json::json!({"status": "ok"}));
        }

        // Embedded static assets (CSS, JS, fonts) with content-hash URLs for
        // cache busting. The dispatch table replaces a stack of
        // `_ if path.starts_with(...) && path.ends_with(...)` arms — order
        // matters in that form (`latin-ext` must precede `latin`), and a
        // table makes the order explicit and lookup uniform.
        //
        // Each entry's bytes-fn returns `&'static [u8]` directly — every
        // asset is either an `include_str!`/`include_bytes!` literal or a
        // `OnceLock`-cached `String`/`Vec`, so the reference is genuinely
        // 'static and the lookup itself allocates nothing. The one
        // unavoidable copy is `.to_vec()` at the call site: `ResponseBuilder
        // ::body` (wafer-run's streaming-response protocol) takes ownership
        // of a `Vec<u8>` chunk, so *some* buffer has to be handed across that
        // boundary per request — there is no zero-copy response path in the
        // current wafer-run `OutputStream`/`StreamEvent::Chunk` API. Fixing
        // that fully (e.g. an `Arc<[u8]>`/`bytes::Bytes`-backed chunk so
        // concurrent requests can share one buffer via refcount bump instead
        // of a fresh copy) is a wafer-run API change, out of scope here.
        //
        // Split into three tables (core / block-llm / block-files) instead
        // of one, so the LLM and Files assets — themselves feature-gated in
        // `ui::assets` — don't need a stub/panic branch when their feature
        // is off; the table for a disabled group simply doesn't exist.
        type BytesFn = fn() -> &'static [u8];
        const CORE_TABLE: &[(&str, &str, &str, BytesFn)] = &[
            ("/b/static/app-", ".css", "text/css; charset=utf-8", || {
                ui::assets::css().as_bytes()
            }),
            (
                "/b/static/htmx-",
                ".min.js",
                "application/javascript; charset=utf-8",
                || ui::assets::htmx_js().as_bytes(),
            ),
            // `latin-ext` must come before `latin` so the longer prefix
            // wins. The table is scanned in order.
            ("/b/static/itim-latin-ext-", ".woff2", "font/woff2", || {
                ui::assets::itim_latin_ext_woff2()
            }),
            ("/b/static/itim-latin-", ".woff2", "font/woff2", || {
                ui::assets::itim_latin_woff2()
            }),
            // `impresspress-logo-long-` must come before `impresspress-logo-` so the
            // longer prefix wins (same pattern as `itim-latin-ext-` above).
            ("/b/static/impresspress-logo-long-", ".png", "image/png", || {
                ui::assets::logo_long_png()
            }),
            ("/b/static/impresspress-logo-", ".png", "image/png", || {
                ui::assets::logo_icon_png()
            }),
            ("/b/static/favicon-", ".ico", "image/x-icon", || {
                ui::assets::favicon_ico()
            }),
        ];
        for (prefix, suffix, content_type, bytes_fn) in CORE_TABLE {
            if path.starts_with(prefix) && path.ends_with(suffix) {
                return ResponseBuilder::new()
                    .set_header("Cache-Control", "public, max-age=31536000, immutable")
                    .body(bytes_fn().to_vec(), content_type);
            }
        }

        #[cfg(feature = "block-llm")]
        {
            const LLM_TABLE: &[(&str, &str, &str, BytesFn)] = &[
                (
                    "/b/static/marked-",
                    ".min.js",
                    "application/javascript; charset=utf-8",
                    || ui::assets::marked_js().as_bytes(),
                ),
                (
                    "/b/static/purify-",
                    ".js",
                    "application/javascript; charset=utf-8",
                    || ui::assets::purify_js().as_bytes(),
                ),
                (
                    "/b/static/llm-chat-",
                    ".js",
                    "application/javascript; charset=utf-8",
                    || ui::assets::llm_chat_js().as_bytes(),
                ),
            ];
            for (prefix, suffix, content_type, bytes_fn) in LLM_TABLE {
                if path.starts_with(prefix) && path.ends_with(suffix) {
                    return ResponseBuilder::new()
                        .set_header("Cache-Control", "public, max-age=31536000, immutable")
                        .body(bytes_fn().to_vec(), content_type);
                }
            }
        }

        #[cfg(feature = "block-files")]
        {
            const FILES_TABLE: &[(&str, &str, &str, BytesFn)] = &[(
                "/b/static/files-browser-",
                ".js",
                "application/javascript; charset=utf-8",
                || ui::assets::files_browser_js().as_bytes(),
            )];
            for (prefix, suffix, content_type, bytes_fn) in FILES_TABLE {
                if path.starts_with(prefix) && path.ends_with(suffix) {
                    return ResponseBuilder::new()
                        .set_header("Cache-Control", "public, max-age=31536000, immutable")
                        .body(bytes_fn().to_vec(), content_type);
                }
            }
        }

        err_not_found("not found")
    },
}

#[cfg(test)]
mod tests {
    use wafer_run::{
        context::Context, Block, InputStream, Message, OutputStream, META_RESP_CONTENT_TYPE,
    };

    use super::*;
    #[cfg(any(feature = "block-llm", feature = "block-files"))]
    use crate::ui::assets;

    #[derive(Clone)]
    struct NopCtx;
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
    #[cfg(feature = "block-llm")]
    async fn system_handle_serves_llm_chat_js() {
        let block = SystemBlock::new();
        let url = assets::llm_chat_js_url();
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
    #[cfg(feature = "block-files")]
    async fn system_handle_serves_files_browser_js() {
        let block = SystemBlock::new();
        let url = assets::files_browser_js_url();
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
    #[cfg(feature = "block-llm")]
    async fn system_handle_serves_marked_js() {
        let block = SystemBlock::new();
        let url = assets::marked_js_url();
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
    #[cfg(feature = "block-llm")]
    async fn system_handle_serves_purify_js() {
        let block = SystemBlock::new();
        let url = assets::purify_js_url();
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
}
