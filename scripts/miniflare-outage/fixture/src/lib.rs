//! The Worker the Miniflare D1-outage harness (`scripts/miniflare-outage/`)
//! deploys and drives.
//!
//! It is impresspress and nothing else, in one of two shapes picked by the
//! [`SHAPE_VAR`] Worker var, so that one wasm build serves every scenario:
//!
//! - **`wafer-site`** — shaped as the wafer.run site deploys on Cloudflare
//!   (`site/src/lib.rs`, `register_post_build_for_site`): a content block
//!   serving the release's `dist/` folder straight from the platform storage
//!   service, with no config read, and the router's route table replaced so
//!   `/b/**` and `/health` reach impresspress and everything else reaches the
//!   content block. During a D1 outage its pages keep serving.
//! - **`impresspress-site`** — impresspress as an ordinary site deploys it, its
//!   landing switch a stored-config read that fails closed during an outage.
//! - **`wafer-site-init-reader`** — `wafer-site` plus one block,
//!   [`InitReader`], whose `Init` sends D1 a statement of its own. No block
//!   impresspress ships does: each reads its config from the one variables
//!   snapshot the runtime build takes. A consumer's block can (D1 outage plan
//!   I2g: a block whose own `Init` makes D1 reset), and this one stands for it,
//!   so the harness can fail that block's statement alone.
//!
//! The var is read here, before `run_with_config`, and is not part of the
//! runtime's identity: a harness scenario deploys one shape for its whole
//! life, onto its own local D1, KV and R2.

#[cfg(feature = "target-cloudflare")]
mod worker_entry {
    use std::sync::Arc;

    use impresspress_core::builder::ImpresspressBuilder;
    use wafer_core::{clients::database as db, interfaces::storage::service::StorageService};
    use wafer_run::{
        Block, BlockInfo, Context, ErrorCode, InputStream, LifecycleEvent, LifecycleType, Message,
        MetaEntry, OutputStream, Wafer, WaferError, META_RESP_CONTENT_TYPE,
    };

    /// The Worker var naming the shape: `wafer-site`, `wafer-site-init-reader`
    /// or `impresspress-site`.
    pub const SHAPE_VAR: &str = "MINIFLARE_FIXTURE_SHAPE";

    /// The release folder the content block serves, as wafer-site's
    /// Cloudflare target does: `impresspress deploy` stages `dist/` under
    /// that name.
    const CONTENT_FOLDER: &str = "dist";

    /// The site's static content block, as wafer-site's `wafer-site/content`
    /// is on Cloudflare: every path read straight from the platform storage
    /// service, `/` as `index.html`, no config read.
    struct SiteContent(Arc<dyn StorageService>);

    impl SiteContent {
        const NAME: &'static str = "wafer-site/content";
    }

    #[wafer_block::wafer_async_trait]
    impl Block for SiteContent {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(Self::NAME, "0.0.1", "http-handler@v1", "site content")
        }

        async fn handle(
            &self,
            _ctx: &dyn Context,
            msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            let path = msg.path().trim_start_matches('/');
            let key = if path.is_empty() { "index.html" } else { path };
            match self.0.get(CONTENT_FOLDER, key).await {
                Ok((data, _)) => OutputStream::respond_with_meta(
                    data,
                    vec![MetaEntry {
                        key: META_RESP_CONTENT_TYPE.to_string(),
                        value: wafer_core::mime::mime_for_ext(std::path::Path::new(key))
                            .to_string(),
                    }],
                ),
                Err(_) => OutputStream::error(WaferError::new(ErrorCode::NotFound, "Not found")),
            }
        }

        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// A block whose `Init` sends D1 one statement of its own: whether its own
    /// table exists (a read under its `{org}__{block}__` prefix, which WRAP
    /// admits without a grant). Its `Init` succeeds whatever the answer, and
    /// fails when the statement does, classified as the platform classifies
    /// the statement's error. It serves nothing: every request is `NotFound`.
    struct InitReader;

    impl InitReader {
        const NAME: &'static str = "miniflare/init-reader";
        /// The harness's D1 wrapper fails the statements that name this
        /// (`workers/d1-control.mjs`, mode `init-reset`).
        const TABLE: &'static str = "miniflare__init_reader__state";
    }

    #[wafer_block::wafer_async_trait]
    impl Block for InitReader {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(Self::NAME, "0.0.1", "http-handler@v1", "reads D1 at Init")
        }

        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::error(WaferError::new(ErrorCode::NotFound, "Not found"))
        }

        async fn lifecycle(
            &self,
            ctx: &dyn Context,
            event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            if event.event_type == LifecycleType::Init {
                db::table_exists(ctx, Self::TABLE).await?;
            }
            Ok(())
        }
    }

    /// Add [`InitReader`] to the runtime, before `build()`.
    fn with_init_reader(
        builder: ImpresspressBuilder,
    ) -> Result<ImpresspressBuilder, Box<dyn std::error::Error>> {
        Ok(builder.extra_block(InitReader::NAME, Arc::new(InitReader)))
    }

    /// Make `wafer` wafer-site-shaped, after `build()`: the content block over
    /// the platform storage, and the router's route table replaced by the
    /// site's.
    fn make_site_shaped(
        wafer: &mut Wafer,
        storage: Arc<dyn StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // `register_block` takes `Arc<dyn Block>`, and on wasm32 every wafer
        // block and service trait object is `!Send + !Sync` by construction
        // (`impresspress-cloudflare/src/lib.rs` says why at length).
        #[expect(
            clippy::arc_with_non_send_sync,
            reason = "the `Arc` is forced by `register_block`'s signature, not chosen over an `Rc`"
        )]
        let block = Arc::new(SiteContent(storage));
        wafer.register_block(SiteContent::NAME, block)?;
        wafer.add_block_config(
            "wafer-run/router",
            serde_json::json!({ "routes": [
                { "path": "/b/**",   "block": "impresspress/router" },
                { "path": "/health", "block": "impresspress/router" },
                { "path": "/**",     "block": SiteContent::NAME },
            ] }),
        );
        Ok(())
    }

    /// Cloudflare Worker `fetch` entrypoint: [`impresspress_cloudflare::run`]
    /// with this fixture's shape.
    #[worker::event(fetch)]
    async fn fetch_main(
        req: worker::Request,
        env: worker::Env,
        ctx: worker::Context,
    ) -> worker::Result<worker::Response> {
        let shape = env.var(SHAPE_VAR).map(|v| v.to_string()).map_err(|_| {
            worker::Error::RustError(format!("the {SHAPE_VAR} Worker var is not set"))
        })?;
        match shape.as_str() {
            "wafer-site" => impresspress_cloudflare::run(req, env, ctx, Ok, make_site_shaped).await,
            "wafer-site-init-reader" => {
                impresspress_cloudflare::run(req, env, ctx, with_init_reader, make_site_shaped)
                    .await
            }
            "impresspress-site" => {
                impresspress_cloudflare::run(req, env, ctx, Ok, |_wafer, _storage| Ok(())).await
            }
            other => Err(worker::Error::RustError(format!(
                "{SHAPE_VAR} is {other:?}; expected \"wafer-site\", \"wafer-site-init-reader\" \
                 or \"impresspress-site\""
            ))),
        }
    }

    /// One-time isolate initialization before the first fetch event.
    #[worker::event(start)]
    fn start() {
        impresspress_cloudflare::init_isolate();
    }
}
