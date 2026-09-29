//! `impresspress/fastembed` — native ONNX embedding block.
//!
//! Wraps a [`FastembedService`] from `wafer-block-fastembed` and exposes it
//! as a WAFER block speaking the `embedding@v1` service protocol. App blocks
//! (notably `impresspress/vector`) dispatch to it via
//! `ctx.call_block("impresspress/fastembed", ...)` whenever they need to embed
//! text with a locally-hosted model.
//!
//! This block is feature-gated behind `block-fastembed` (which
//! `native-embedding` implies) because the underlying fastembed-rs crate
//! pulls in ONNX Runtime (~100 MB of native deps on most platforms).
//! Consumers that only need remote embedding providers — or the browser
//! runtime where ONNX isn't applicable — should not pay the build cost, so
//! the module is compiled (`blocks::mod`) and registered
//! (`builder::registration`) only under that feature.
//!
//! ## Lazy service construction
//!
//! `FastembedService::default_model(cache_dir)` triggers ONNX model download +
//! load (tens to hundreds of MB) — not cheap. The `info()` path in
//! `blocks::all_block_infos()` constructs every block just to read its
//! metadata, so we must *not* eagerly load the model in the constructor.
//! The service is built lazily on the first `handle()` call and cached for
//! the lifetime of the singleton.
//!
//! The load is synchronous (a blocking download plus an ONNX session build),
//! so it runs on a dedicated thread and `handle()` awaits its result: the
//! async worker that took the first request keeps serving every other task
//! meanwhile. The in-flight load is owned by the block, not by the request
//! that started it: every request awaits the same load, and a request that
//! is dropped mid-load leaves it running for the others. A failed or
//! panicked load is not cached, so a later request starts a fresh one.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

use futures::{
    channel::oneshot,
    future::{BoxFuture, FutureExt, Shared},
};
use wafer_block_fastembed::FastembedService;
use wafer_core::interfaces::vector::{
    handler::handle_embedding_message, service::EmbeddingService,
};
use wafer_run::{
    context::Context, Block, BlockInfo, InputStream, InstanceMode, Message, OutputStream,
};

use crate::http::err_internal;

/// Builds the embedding service. Synchronous and slow — it is only ever run
/// on a dedicated thread (see [`FastembedBlock::get_service`]).
type Loader = dyn Fn() -> Result<Arc<dyn EmbeddingService>, String> + Send + Sync;

/// One load in flight: the loader thread's result, awaitable by any number
/// of requests at once.
type Load = Shared<BoxFuture<'static, Result<Arc<dyn EmbeddingService>, String>>>;

/// Native ONNX embedding block.
///
/// Singleton. The wrapped `FastembedService` is initialized on the first
/// `handle()` call — construction is free, no model weights are loaded
/// until someone asks to embed something. An init error surfaces to the
/// caller as an `Internal` error on that request.
pub struct FastembedBlock {
    loader: Arc<Loader>,
    service: OnceLock<Arc<dyn EmbeddingService>>,
    /// The load in flight, if any. Owned here rather than by the request
    /// that started it, so dropping that request neither cancels the load
    /// nor orphans its result: the next request awaits the same load
    /// instead of starting a second download and ONNX session.
    in_flight: Mutex<Option<Load>>,
}

impl FastembedBlock {
    /// Build a `FastembedBlock` with a lazy service whose model weights are
    /// cached under `cache_dir` (downloaded there on first use) — the
    /// directory the embedder hands
    /// [`ImpresspressBuilder`](crate::builder::ImpresspressBuilder)'s
    /// `model_cache_dir`.
    ///
    /// The model is loaded on first embed, not here — this is cheap enough
    /// to call from `blocks::all_block_infos()` without triggering an ONNX
    /// download.
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        let cache_dir = cache_dir.into();
        Self::with_loader(move || {
            FastembedService::default_model(cache_dir.clone())
                .map(|svc| Arc::new(svc) as Arc<dyn EmbeddingService>)
                .map_err(|e| format!("fastembed init failed: {e}"))
        })
    }

    /// A block whose service comes from `loader` instead of the catalog's
    /// default ONNX model — the seam that lets a test observe how the load
    /// is scheduled without downloading a model.
    pub(crate) fn with_loader(
        loader: impl Fn() -> Result<Arc<dyn EmbeddingService>, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            loader: Arc::new(loader),
            service: OnceLock::new(),
            in_flight: Mutex::new(None),
        }
    }

    /// The service, loading it on first use.
    ///
    /// The loader runs on its own thread, so the executor is never blocked
    /// by the download or the ONNX session build. Every request awaits the
    /// block-owned [`Load`], and whichever request sees it finish clears the
    /// slot: on success the service now lives in `service` alone, on failure
    /// a later request retries instead of inheriting the failure forever.
    async fn get_service(&self) -> Result<&dyn EmbeddingService, String> {
        if let Some(svc) = self.service.get() {
            return Ok(svc.as_ref());
        }
        let load = self.in_flight_load()?;
        let result = load.clone().await;
        self.clear_in_flight(&load);
        result.map(|built| self.service.get_or_init(|| built).as_ref())
    }

    /// Empty the slot if it still holds `load` — never a newer load another
    /// request has started since.
    fn clear_in_flight(&self, load: &Load) {
        let mut slot = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
        if slot.as_ref().is_some_and(|l| l.ptr_eq(load)) {
            *slot = None;
        }
    }

    /// The load in flight, starting one if there is none.
    ///
    /// A load in the slot that has already FAILED is replaced, not joined: it
    /// failed after every request waiting on it had gone away, so nobody
    /// cleared it. `peek` alone cannot see that — a `Shared` only records its
    /// output once something polls it — so the stale load is polled once,
    /// without waiting, to find out.
    fn in_flight_load(&self) -> Result<Load, String> {
        let mut slot = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(load) = slot.as_ref() {
            if !matches!(load.clone().now_or_never(), Some(Err(_))) {
                return Ok(load.clone());
            }
        }
        let loader = self.loader.clone();
        let (tx, rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("fastembed-load".to_string())
            .spawn(move || {
                // The receiver lives in the block-owned `Load`, so it is
                // still there however many requests have gone away.
                let _ = tx.send(loader());
            })
            .map_err(|e| format!("fastembed: could not start the model loader: {e}"))?;
        let load = async move {
            // A dropped sender means the loader panicked.
            rx.await
                .map_err(|_| "fastembed: the model loader exited without a result".to_string())?
        }
        .boxed()
        .shared();
        *slot = Some(load.clone());
        Ok(load)
    }
}

impl FastembedBlock {
    /// The block's registered name.
    pub const BLOCK_NAME: &'static str = "impresspress/fastembed";
}

#[wafer_block::wafer_async_trait]
impl Block for FastembedBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            Self::BLOCK_NAME,
            "0.0.1",
            "embedding@v1",
            "Native ONNX text embedding via fastembed-rs",
        )
        // Singleton: the wrapped `FastembedService` lazily loads one ONNX
        // model and caches it for the block's lifetime —
        // a per-node/per-flow instance would reload the model needlessly.
        // Kept deliberately in lockstep with `TransformersEmbedBlock`.
        .instance_mode(InstanceMode::Singleton)
        .category(wafer_run::BlockCategory::Service)
        // Declared here rather than from the service: the service loads
        // lazily on the first call, after `info()` has been read.
        .grants(vec![super::embedding_grant(Self::BLOCK_NAME)])
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let body = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        let svc = match self.get_service().await {
            Ok(s) => s,
            Err(e) => return err_internal("fastembed service unavailable", e),
        };
        // Op validation (EMBEDDING_EMBED / EMBEDDING_COUNT_TOKENS, plus an
        // `Unimplemented` terminal for anything else) lives in
        // `handle_embedding_message`. Both embedding wrappers delegate the
        // whole message here — neither carries its own `ServiceOp` check.
        handle_embedding_message(svc, ctx, Self::BLOCK_NAME, &msg, &body).await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    use wafer_block::{
        codec,
        wire::vector::{CountTokensRequest, CountTokensResponse},
        ServiceOp,
    };
    use wafer_core::interfaces::vector::service::{EmbeddingService, VectorError};
    use wafer_run::{context::Context as _, Block as _, InputStream, Message};

    use super::FastembedBlock;
    use crate::test_support::TestContext;

    /// How long the stand-in model takes to "load" — long enough that a
    /// starved executor or a second load is unmistakable.
    const LOAD: Duration = Duration::from_millis(300);

    /// A service that answers `count_tokens` with a whitespace count.
    struct Stub;

    #[wafer_block::wafer_async_trait]
    impl EmbeddingService for Stub {
        fn model(&self) -> &str {
            "stub"
        }
        fn dimensions(&self) -> u32 {
            1
        }
        async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, VectorError> {
            Ok(texts.iter().map(|_| vec![0.0]).collect())
        }
    }

    /// A block whose loader blocks its thread for [`LOAD`], as the real
    /// download + ONNX build does, and counts how often it ran.
    fn slow_block() -> (Arc<FastembedBlock>, Arc<AtomicUsize>) {
        let loads = Arc::new(AtomicUsize::new(0));
        let counter = loads.clone();
        let block = FastembedBlock::with_loader(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(LOAD);
            Ok(Arc::new(Stub) as Arc<dyn EmbeddingService>)
        });
        (Arc::new(block), loads)
    }

    /// A deployment holding `block`, running as the block its declared grant
    /// admits (`impresspress/vector`): the caller production has.
    async fn called_by_vector(block: &Arc<FastembedBlock>) -> TestContext {
        let grantee = block
            .info()
            .grants
            .first()
            .expect("the block grants its caller")
            .grantee
            .clone();
        let mut ctx = TestContext::new().await;
        ctx.register_block(FastembedBlock::BLOCK_NAME, block.clone());
        ctx.running_as(&grantee)
    }

    /// `op` with `body`, through `call_block` from the vector block's frame,
    /// so the handler authorizes it as the runtime does.
    async fn call(ctx: &TestContext, op: &str, body: Vec<u8>) -> wafer_run::OutputStream {
        ctx.call_block(
            FastembedBlock::BLOCK_NAME,
            Message::new(op),
            InputStream::from_bytes(body),
        )
        .await
    }

    /// One `embedding.count_tokens`, as the vector block sends it.
    async fn count_tokens(ctx: &TestContext) -> u64 {
        let body = codec::encode(&CountTokensRequest {
            text: "two words".into(),
        })
        .expect("encode");
        let out = call(ctx, ServiceOp::EMBEDDING_COUNT_TOKENS, body)
            .await
            .collect_buffered()
            .await
            .map_err(wafer_block::WaferError::from)
            .expect("count_tokens answers once the model is loaded");
        codec::decode::<CountTokensResponse>(&out.body)
            .expect("decode")
            .tokens
    }

    /// The first request's model load must not run on the async worker that
    /// took the request. On a single-threaded runtime a load run inline
    /// blocks the ONLY worker, so every other task — here a 10 ms ticker —
    /// stops for the whole load. Two concurrent first requests also share
    /// the one load.
    #[tokio::test(flavor = "current_thread")]
    async fn the_model_loads_off_the_executor() {
        let (block, loads) = slow_block();
        let ctx = called_by_vector(&block).await;
        let done = AtomicBool::new(false);
        let ticks = AtomicUsize::new(0);

        let requests = async {
            let answers = futures::join!(count_tokens(&ctx), count_tokens(&ctx));
            done.store(true, Ordering::SeqCst);
            answers
        };
        let ticker = async {
            while !done.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
                ticks.fetch_add(1, Ordering::SeqCst);
            }
        };
        let (answers, ()) = futures::join!(requests, ticker);

        assert_eq!(answers, (2, 2));
        assert_eq!(loads.load(Ordering::SeqCst), 1, "one load serves both");
        // 300 ms of load at a 10 ms tick is ~30 ticks when the worker is
        // free; an inline load leaves it at most one or two.
        let ticked = ticks.load(Ordering::SeqCst);
        assert!(
            ticked >= 10,
            "the executor was starved during the model load: {ticked} ticks in {LOAD:?}"
        );
    }

    /// Concurrent first requests on DIFFERENT workers load the model once.
    /// Each ONNX session is hundreds of MB, so a second load is a second
    /// copy of the model in memory for the duration, not just wasted time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_first_requests_load_the_model_once() {
        let (block, loads) = slow_block();
        let ctx = called_by_vector(&block).await;

        let first = tokio::spawn({
            let ctx = ctx.clone();
            async move { count_tokens(&ctx).await }
        });
        let second = tokio::spawn({
            let ctx = ctx.clone();
            async move { count_tokens(&ctx).await }
        });
        assert_eq!(first.await.expect("first request"), 2);
        assert_eq!(second.await.expect("second request"), 2);

        assert_eq!(loads.load(Ordering::SeqCst), 1);
        // And the loaded service is kept: a later request does not load.
        assert_eq!(count_tokens(&ctx).await, 2);
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    /// A failed load is reported to the request and NOT cached: the next
    /// request tries again, so a transient download failure is not
    /// permanent for the life of the process.
    #[tokio::test]
    async fn a_failed_load_is_retried_by_the_next_request() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let block = Arc::new(FastembedBlock::with_loader(move || {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                Err("offline".to_string())
            } else {
                Ok(Arc::new(Stub) as Arc<dyn EmbeddingService>)
            }
        }));
        let ctx = called_by_vector(&block).await;

        let err = call(&ctx, ServiceOp::EMBEDDING_COUNT_TOKENS, Vec::new())
            .await
            .collect_buffered()
            .await
            .map_err(wafer_block::WaferError::from)
            .expect_err("the first load fails");
        assert_eq!(err.code, wafer_run::ErrorCode::Internal);

        assert_eq!(count_tokens(&ctx).await, 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// A request dropped mid-load — a client disconnect, a timeout — must not
    /// take the load with it. The load belongs to the block: the requests
    /// that follow await the same one, so the model is downloaded and built
    /// exactly once.
    #[tokio::test(flavor = "current_thread")]
    async fn a_request_dropped_mid_load_does_not_cause_a_second_load() {
        let (block, loads) = slow_block();
        let ctx = called_by_vector(&block).await;

        // Start the first request and abandon it a third of the way through.
        let abandoned = tokio::time::timeout(Duration::from_millis(100), count_tokens(&ctx)).await;
        assert!(
            abandoned.is_err(),
            "the first request must still be loading"
        );
        assert_eq!(loads.load(Ordering::SeqCst), 1);

        let answers = futures::join!(count_tokens(&ctx), count_tokens(&ctx));
        assert_eq!(answers, (2, 2));
        assert_eq!(count_tokens(&ctx).await, 2);
        assert_eq!(
            loads.load(Ordering::SeqCst),
            1,
            "the dropped request's load serves every later request"
        );
    }

    /// A loader that panics is a failed load, not a wedged block: the
    /// request is answered with an error and a later request loads again.
    #[tokio::test]
    async fn a_panicked_load_is_retried_by_the_next_request() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let block = Arc::new(FastembedBlock::with_loader(move || {
            assert!(
                counter.fetch_add(1, Ordering::SeqCst) != 0,
                "first load panics"
            );
            Ok(Arc::new(Stub) as Arc<dyn EmbeddingService>)
        }));
        let ctx = called_by_vector(&block).await;

        let err = call(&ctx, ServiceOp::EMBEDDING_COUNT_TOKENS, Vec::new())
            .await
            .collect_buffered()
            .await
            .map_err(wafer_block::WaferError::from)
            .expect_err("the first load panics");
        assert_eq!(err.code, wafer_run::ErrorCode::Internal);

        assert_eq!(count_tokens(&ctx).await, 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// A load that fails after every request waiting on it has been dropped
    /// is not handed to the next request as a stale error: that request
    /// starts a fresh load and gets the service.
    #[tokio::test(flavor = "current_thread")]
    async fn a_load_that_fails_with_nobody_waiting_is_retried() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let block = Arc::new(FastembedBlock::with_loader(move || {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                std::thread::sleep(Duration::from_millis(100));
                Err("offline".to_string())
            } else {
                Ok(Arc::new(Stub) as Arc<dyn EmbeddingService>)
            }
        }));
        let ctx = called_by_vector(&block).await;

        let abandoned = tokio::time::timeout(Duration::from_millis(20), count_tokens(&ctx)).await;
        assert!(abandoned.is_err(), "the only waiter is dropped mid-load");
        // Let the orphaned load fail with nobody listening.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        assert_eq!(count_tokens(&ctx).await, 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// A finished load leaves the slot empty: the service is held once, by
    /// the block, not a second time by the spent load.
    #[tokio::test]
    async fn a_successful_load_leaves_no_load_in_flight() {
        let (block, _loads) = slow_block();
        let ctx = called_by_vector(&block).await;
        assert_eq!(count_tokens(&ctx).await, 2);
        assert!(block
            .in_flight
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_none());
    }

    /// The cache directory is the embedder's to choose: a build that turns
    /// this block on without one is refused, naming the builder call that
    /// supplies it, rather than falling back to a directory nobody picked.
    #[test]
    fn a_build_without_a_model_cache_dir_is_refused() {
        let err = crate::builder::required_model_cache_dir(None, "block-fastembed")
            .expect_err("no directory must be refused");
        let text = err.to_string();
        assert!(text.contains(".model_cache_dir("), "{text}");
        assert!(text.contains("block-fastembed"), "{text}");

        let dir = std::path::Path::new("/var/cache/models");
        assert_eq!(
            crate::builder::required_model_cache_dir(Some(dir), "block-fastembed")
                .expect("a directory is used as given"),
            dir
        );
    }
}
