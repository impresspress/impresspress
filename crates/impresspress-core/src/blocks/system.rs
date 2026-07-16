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
        BlockInfo::new("impresspress/system", "0.0.1", "http-handler@v1", "System health and embedded static assets")
            .instance_mode(InstanceMode::Singleton)
            .category(wafer_run::BlockCategory::Infrastructure)
            .description("Core system services including health checks and embedded static assets (CSS, JavaScript).")
            .endpoints(vec![
                BlockEndpoint::get("/health").summary("Health check"),
                BlockEndpoint::get("/b/static/app-{hash}.css").summary("Embedded CSS"),
                BlockEndpoint::get("/b/static/htmx-{hash}.min.js").summary("Embedded htmx JS"),
                BlockEndpoint::get("/b/static/marked-{hash}.min.js").summary("Embedded marked.js"),
                BlockEndpoint::get("/b/static/purify-{hash}.js").summary("Embedded DOMPurify JS"),
                BlockEndpoint::get("/b/static/llm-chat-{hash}.js").summary("Embedded LLM chat JS"),
                BlockEndpoint::get("/b/static/files-browser-{hash}.js").summary("Embedded files-browser JS"),
                BlockEndpoint::get("/b/static/itim-latin-{hash}.woff2").summary("Embedded Itim font (latin)"),
                BlockEndpoint::get("/b/static/itim-latin-ext-{hash}.woff2").summary("Embedded Itim font (latin-ext)"),
                BlockEndpoint::get("/b/static/impresspress-logo-{hash}.png").summary("Embedded Impresspress square logo"),
                BlockEndpoint::get("/b/static/impresspress-logo-long-{hash}.png").summary("Embedded Impresspress wordmark logo"),
                BlockEndpoint::get("/b/static/favicon-{hash}.ico").summary("Embedded Impresspress favicon"),
            ])
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
        // Each entry: (prefix, suffix, content_type, bytes-fn).
        type Bytes = std::borrow::Cow<'static, [u8]>;
        let table: &[(&str, &str, &str, fn() -> Bytes)] = &[
            ("/b/static/app-", ".css", "text/css; charset=utf-8", || {
                Bytes::Owned(ui::assets::css().as_bytes().to_vec())
            }),
            (
                "/b/static/htmx-",
                ".min.js",
                "application/javascript; charset=utf-8",
                || Bytes::Owned(ui::assets::htmx_js().as_bytes().to_vec()),
            ),
            (
                "/b/static/marked-",
                ".min.js",
                "application/javascript; charset=utf-8",
                || Bytes::Owned(ui::assets::marked_js().as_bytes().to_vec()),
            ),
            (
                "/b/static/purify-",
                ".js",
                "application/javascript; charset=utf-8",
                || Bytes::Owned(ui::assets::purify_js().as_bytes().to_vec()),
            ),
            (
                "/b/static/llm-chat-",
                ".js",
                "application/javascript; charset=utf-8",
                || Bytes::Owned(ui::assets::llm_chat_js().as_bytes().to_vec()),
            ),
            (
                "/b/static/files-browser-",
                ".js",
                "application/javascript; charset=utf-8",
                || Bytes::Owned(ui::assets::files_browser_js().as_bytes().to_vec()),
            ),
            // `latin-ext` must come before `latin` so the longer prefix
            // wins. The table is scanned in order.
            ("/b/static/itim-latin-ext-", ".woff2", "font/woff2", || {
                Bytes::Owned(ui::assets::itim_latin_ext_woff2().to_vec())
            }),
            ("/b/static/itim-latin-", ".woff2", "font/woff2", || {
                Bytes::Owned(ui::assets::itim_latin_woff2().to_vec())
            }),
            // `impresspress-logo-long-` must come before `impresspress-logo-` so the
            // longer prefix wins (same pattern as `itim-latin-ext-` above).
            ("/b/static/impresspress-logo-long-", ".png", "image/png", || {
                Bytes::Owned(ui::assets::logo_long_png().to_vec())
            }),
            ("/b/static/impresspress-logo-", ".png", "image/png", || {
                Bytes::Owned(ui::assets::logo_icon_png().to_vec())
            }),
            ("/b/static/favicon-", ".ico", "image/x-icon", || {
                Bytes::Owned(ui::assets::favicon_ico().to_vec())
            }),
        ];

        for (prefix, suffix, content_type, bytes_fn) in table {
            if path.starts_with(prefix) && path.ends_with(suffix) {
                return ResponseBuilder::new()
                    .set_header("Cache-Control", "public, max-age=31536000, immutable")
                    .body(bytes_fn().into_owned(), content_type);
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
