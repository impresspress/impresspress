//! Service-Worker-side Wafer runtime storage and dispatch.
//!
//! The active runtime is an `Rc<Wafer>`. Every dispatch clones the `Rc`
//! before its first `.await`, so a `replace_wafer` that lands while a
//! request is in flight leaves that request on the runtime it started on
//! and routes every later request to the new one. wasm32 is
//! single-threaded, so the thread_local needs no Send/Sync.

use std::{cell::RefCell, rc::Rc};

use impresspress_core::after_response::{self, AfterResponse};
use wasm_bindgen::prelude::*;

use crate::convert;

thread_local! {
    pub(crate) static RUNTIME: RefCell<Option<Rc<wafer_run::Wafer>>> = const { RefCell::new(None) };
}

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    AlreadyInitialized,
    NotInitialized,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyInitialized => f.write_str("store_wafer: runtime already initialized"),
            Self::NotInitialized => f.write_str("replace_wafer: runtime not initialized"),
        }
    }
}

impl std::error::Error for StoreError {}

/// True if a runtime is currently stored (via `store_wafer` or `replace_wafer`).
pub fn is_initialized() -> bool {
    RUNTIME.with(|r| r.borrow().is_some())
}

/// Clone a handle to the currently active runtime, if any.
pub fn current_wafer() -> Option<Rc<wafer_run::Wafer>> {
    RUNTIME.with(|r| r.borrow().clone())
}

/// Install the first runtime. Cold initialization only — a second call is an
/// error so an accidental double `initialize()` cannot swap runtimes silently.
pub fn store_wafer(wafer: wafer_run::Wafer) -> Result<(), StoreError> {
    RUNTIME.with(|r| {
        let mut slot = r.borrow_mut();
        if slot.is_some() {
            return Err(StoreError::AlreadyInitialized);
        }
        *slot = Some(Rc::new(wafer));
        Ok(())
    })
}

/// Swap in a rebuilt runtime and hand back the one that was active.
///
/// The returned handle is what makes the swap reversible: an activation
/// rebuilds the runtime *before* it publishes the site, so a publish that
/// fails afterwards has to put the previous runtime back. Hold the returned
/// `Rc` across the rest of the activation and pass it to [`restore_wafer`] on
/// that path; drop it once the activation has committed. A caller that
/// discards it cannot undo the swap — the old runtime is gone as soon as the
/// last handle to it is.
///
/// The caller that does this is `impresspress-web`'s `BrowserRuntimeControl`:
/// it parks the handle in its `retained` slot on every successful `rebuild`
/// and hands it to [`restore_wafer`] from `RuntimeControl::restore_previous`,
/// which is the rollback half of design §7.3.
pub fn replace_wafer(wafer: wafer_run::Wafer) -> Result<Rc<wafer_run::Wafer>, StoreError> {
    RUNTIME.with(|r| {
        let mut slot = r.borrow_mut();
        let previous = slot.take().ok_or(StoreError::NotInitialized)?;
        *slot = Some(Rc::new(wafer));
        Ok(previous)
    })
}

/// Restore a runtime handed back by [`replace_wafer`].
///
/// Infallible by construction, and that is the point of restoring the value
/// rather than rebuilding one: the caller is already on a failure path, and
/// the runtime it wants back is the one it is holding.
pub fn restore_wafer(previous: Rc<wafer_run::Wafer>) {
    RUNTIME.with(|r| *r.borrow_mut() = Some(previous));
}

/// Convert a browser `Request` into a WAFER `Message`, dispatch through
/// the currently active `Wafer`'s `site-main` flow, and return a browser
/// `Response` together with a promise for the work the request left to run
/// after it. Answers 503 if called before `store_wafer`; internal errors
/// answer 500. A request body over
/// `impresspress_core::streaming::MAX_REQUEST_BODY_BYTES` is marked by
/// `convert::request_to_message` and answered 413 by the flow, like any
/// other refusal.
///
/// The `Rc` is cloned synchronously (before the first `.await`), so a
/// `replace_wafer` that lands mid-dispatch does not affect this call — it
/// keeps running against the runtime it started on.
///
/// ## Durability and the work after the reply
///
/// The request runs as one [`flush_scope`](crate::flush_scope): its
/// mutations share one database export, and by the epoch rule
/// (`flush_scope`'s module doc) this function does not return until every
/// mutation completed before the request's end, whichever request made it,
/// is in an export that has been written — so a response that reports a
/// change done means the change is durable, even when the change was
/// written from another request's poll (a coalesced activation). When that
/// export fails the response is a 500, whatever the flow answered, because
/// the reply must not claim a durability it does not have. The change is not
/// undone, though: it is already live in the in-memory database, every later
/// read sees it, and the next export that succeeds persists it — possibly
/// this request's own after-response flush. A caller that retries on the 500 therefore repeats a
/// change that did happen (a retried `dev_write_file` creates another
/// generation). The failure is reported only as this `tracing::error` in the
/// console: the request's audit row was queued with the flow's own status,
/// so under the browser's default `errors` policy no `request_logs` row
/// records it at all, and under `all` the row carries that status, not 500.
///
/// The request also runs inside its own
/// [`AfterResponse`](impresspress_core::after_response::AfterResponse)
/// scope (the one Cloudflare's entry opens), so its `request_logs` audit row
/// and the tasks its handlers [`defer`](impresspress_core::deferred::defer)
/// are queued on it instead of being written or spawned on the response
/// path. The returned promise runs that work — the audit row first, then
/// the tasks together — in a flush scope of its own, so it too exports at
/// most once. It starts on the next task of the event loop, once the
/// response has been handed back, and its failures are logged: there is no
/// response left to carry them. The service worker must pass the promise to
/// `event.waitUntil`: nothing else keeps the worker alive until it settles,
/// and a worker stopped before then loses the audit row and the deferred
/// tasks — never the request's own changes, which were written above.
///
/// Both scopes are re-entered on every poll, so requests interleaved on the
/// service worker's one thread each flush their own mutations at their own
/// end and each queue their own after-response work.
pub async fn dispatch_request(
    request: web_sys::Request,
) -> Result<(web_sys::Response, js_sys::Promise), JsValue> {
    let Some(wafer) = current_wafer() else {
        let response = build_error_response(
            503,
            "impresspress-browser: runtime not initialized — call store_wafer() first",
        )?;
        return Ok((response, js_sys::Promise::resolve(&JsValue::UNDEFINED)));
    };
    let after = AfterResponse::new();
    let (result, flush) =
        crate::flush_scope::run(after_response::scope(Rc::clone(&after), async move {
            let (msg, input) = convert::request_to_message(&request).await?;
            let output = wafer.run("site-main", msg, input).await;
            convert::output_to_response(output).await
        }))
        .await;
    let work = after_response_work(&after);
    if let Err(error) = flush {
        tracing::error!(
            %error,
            "the request's changes were not written to OPFS; they stay live in memory \
             and the next successful export persists them"
        );
        let response = build_error_response(
            500,
            "impresspress-browser: the change is live but could not be saved to browser \
             storage yet; the next successful save persists it, so retrying repeats it",
        )?;
        return Ok((response, work));
    }
    Ok((result?, work))
}

/// [`dispatch_request`]'s answer as the object a service worker's fetch
/// handler reads: `{ response, after }`. The handler returns `response` to
/// `respondWith` and passes `after` to `event.waitUntil` — see
/// [`dispatch_request`] for why the second is not optional. The shape a
/// consumer's `#[wasm_bindgen] handle_request` export resolves to.
pub async fn dispatch_fetch(request: web_sys::Request) -> Result<JsValue, JsValue> {
    let (response, after) = dispatch_request(request).await?;
    let answer = js_sys::Object::new();
    js_sys::Reflect::set(&answer, &JsValue::from_str("response"), &response)?;
    js_sys::Reflect::set(&answer, &JsValue::from_str("after"), &after)?;
    Ok(answer.into())
}

/// The promise for the work `after` holds: its audit row, then its deferred
/// tasks together, in one flush scope. See [`dispatch_request`].
///
/// There is no statement reservation to release between the two, as
/// Cloudflare's `after_response_work` does: the browser database has no
/// per-request statement limit, so nothing was held back for the row.
fn after_response_work(after: &AfterResponse) -> js_sys::Promise {
    let audit_row = after.take_audit_row();
    let tasks = after.take_tasks();
    wasm_bindgen_futures::future_to_promise(async move {
        if audit_row.is_none() && tasks.is_empty() {
            return Ok(JsValue::UNDEFINED);
        }
        next_task().await;
        let ((), flush) = crate::flush_scope::run(async move {
            if let Some(row) = audit_row {
                let db = crate::make_database_service();
                if let Err(failure) = after_response::persist_audit_row(db.as_ref(), row).await {
                    tracing::warn!(
                        table = failure.table,
                        error = %failure.error,
                        "request audit row not written"
                    );
                }
            }
            futures::future::join_all(tasks).await;
        })
        .await;
        if let Err(error) = flush {
            tracing::warn!(%error, "after-response work was not written to OPFS");
        }
        Ok(JsValue::UNDEFINED)
    })
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(callback: &js_sys::Function, delay_ms: i32) -> JsValue;
}

/// Resolve on the event loop's next task. Everything a response goes through
/// on its way out of the service worker — this crate's future resolving,
/// `handle_request`'s promise, `respondWith` taking the response — happens
/// in microtasks, so work that waits for the next task cannot put its
/// synchronous sql.js statements and database export ahead of the reply.
async fn next_task() {
    let tick = js_sys::Promise::new(&mut |resolve, _reject| {
        set_timeout(&resolve, 0);
    });
    // `setTimeout` never rejects the promise it resolves.
    let _ = wasm_bindgen_futures::JsFuture::from(tick).await;
}

fn build_error_response(status: u16, body: &str) -> Result<web_sys::Response, JsValue> {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    web_sys::Response::new_with_opt_str_and_init(Some(body), &init)
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use wasm_bindgen_test::*;

    use super::*;

    fn empty_wafer() -> wafer_run::Wafer {
        let cfg: std::sync::Arc<dyn wafer_run::ConfigSource> =
            std::sync::Arc::new(wafer_run::StaticConfigSource::default());
        wafer_run::Wafer::new(cfg).expect("wafer")
    }

    fn reset() {
        RUNTIME.with(|r| *r.borrow_mut() = None);
    }

    #[wasm_bindgen_test]
    fn first_store_succeeds_and_second_cold_store_fails() {
        reset();
        assert!(!is_initialized());
        store_wafer(empty_wafer()).expect("first store");
        assert!(is_initialized());
        assert!(
            store_wafer(empty_wafer()).is_err(),
            "store_wafer is single-shot"
        );
    }

    #[wasm_bindgen_test]
    fn replace_returns_the_previous_runtime_and_keeps_it_alive() {
        reset();
        store_wafer(empty_wafer()).unwrap();
        let held = current_wafer().expect("current");
        let previous = replace_wafer(empty_wafer()).expect("replace");
        assert!(
            Rc::ptr_eq(&held, &previous),
            "replace hands back the runtime that was active"
        );
        assert_eq!(
            Rc::strong_count(&previous),
            2,
            "an in-flight holder keeps the old runtime alive"
        );
        let now = current_wafer().unwrap();
        assert!(!Rc::ptr_eq(&now, &previous));
    }

    #[wasm_bindgen_test]
    fn replace_before_store_is_an_error() {
        reset();
        assert!(replace_wafer(empty_wafer()).is_err());
    }

    /// **Fails on the pre-fix tree**, where an over-cap body left
    /// `request_to_message` as a `JsValue` error: `dispatch_request` returns
    /// `Err`, the Service Worker's `respondWith` rejects, and the uploader's
    /// `fetch` fails with no status at all. The body is now dropped and the
    /// message marked, and the flow answers 413 — the assertion of the status
    /// itself lives with the code that builds it
    /// (`impresspress_core::pipeline`'s `oversized_body_tests`), because an
    /// empty `Wafer` has no `site-main` flow to answer through.
    #[wasm_bindgen_test]
    async fn an_over_cap_request_body_is_marked_and_dropped() {
        let body = js_sys::Uint8Array::new_with_length(
            (impresspress_core::streaming::MAX_REQUEST_BODY_BYTES + 1) as u32,
        );
        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_body(&body);
        let request = web_sys::Request::new_with_str_and_init(
            "https://dev.impresspress.org/b/storage/api/buckets/photos/objects?key=big.bin",
            &init,
        )
        .expect("build request");

        let (msg, input) = convert::request_to_message(&request)
            .await
            .expect("conversion must not fail the fetch");

        assert!(
            impresspress_core::streaming::body_too_large(&msg),
            "the marker the pipeline refuses on"
        );
        let forwarded = input
            .collect_to_bytes()
            .await
            .expect("an in-memory body does not fail");
        assert!(
            forwarded.is_empty(),
            "an oversized body must not reach a block"
        );
    }

    // ─── dispatch_request: the request's flush, then the work after it ─────

    /// A request through the real `site-main` pipeline
    /// (`impresspress_core::handle_request`, via the router block the
    /// browser runtime registers) to one test block, which writes a row on
    /// `/b/a2test/write` and fails with a 500 on `/b/a2test/fail`. The
    /// pipeline queues the request's audit row on the scope
    /// `dispatch_request` opens, exactly as for any browser request.
    mod after_response_order {
        use std::{collections::HashMap, sync::Arc};

        use impresspress_core::routing::{ExtraRoute, RouteAccess};
        use wafer_core::interfaces::database::service::DatabaseService;
        use wafer_run::{
            context::Context, Block, BlockInfo, InputStream, LifecycleEvent, Message, OutputStream,
            WaferError,
        };

        use super::*;
        use crate::database::{
            test_support::{fail_next_opfs_write, fresh_db, opfs_writes},
            BrowserDatabaseService,
        };

        const WRITES: &str = "a2_writes";
        const LOGS: &str = impresspress_core::platform_state::request_logs::TABLE;
        const BLOCK: &str = "a2test/mutator";

        struct Mutator;

        #[wafer_block::wafer_async_trait]
        impl Block for Mutator {
            fn info(&self) -> BlockInfo {
                BlockInfo::new(
                    BLOCK,
                    "0.0.1",
                    "http-handler@v1",
                    "writes a row per request",
                )
            }

            async fn handle(
                &self,
                _ctx: &dyn Context,
                msg: Message,
                _input: InputStream,
            ) -> OutputStream {
                if msg.path().ends_with("/fail") {
                    return impresspress_core::http::err_internal_no_cause("a2test: failing");
                }
                // Two mutations, so a request that is not one flush scope
                // shows as two exports.
                for n in 0..2 {
                    let row = HashMap::from([(
                        "id".to_string(),
                        serde_json::json!(format!("{}#{n}", msg.path())),
                    )]);
                    if let Err(e) = BrowserDatabaseService.create(WRITES, row).await {
                        return impresspress_core::http::err_internal("a2test: write", e);
                    }
                }
                impresspress_core::http::ok_json(&serde_json::json!({ "ok": true }))
            }

            async fn lifecycle(
                &self,
                _ctx: &dyn Context,
                _event: LifecycleEvent,
            ) -> Result<(), WaferError> {
                Ok(())
            }
        }

        /// Let every queued microtask run: a JS promise resolved now is
        /// awaited, which settles only after the microtasks queued before it.
        async fn drain_microtasks() {
            for _ in 0..8 {
                wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::UNDEFINED))
                    .await
                    .expect("a resolved promise");
            }
        }

        /// A fresh database holding the test table and `request_logs`, and a
        /// runtime whose `site-main` flow is the pipeline, with request-log
        /// policy `policy`.
        async fn install(policy: &str) -> BrowserDatabaseService {
            let db = fresh_db().await;
            db.exec_raw(&format!("CREATE TABLE {WRITES} (id TEXT PRIMARY KEY)"), &[])
                .await
                .expect("create writes table");
            db.exec_raw(
                &format!(
                    "CREATE TABLE {LOGS} (id TEXT PRIMARY KEY, method TEXT, path TEXT, \
                     status TEXT, status_code INTEGER, duration_ms INTEGER, \
                     error_message TEXT, client_ip TEXT, user_id TEXT, \
                     created_at TEXT, updated_at TEXT)"
                ),
                &[],
            )
            .await
            .expect("create request_logs");

            let cfg: Arc<dyn wafer_run::ConfigSource> =
                Arc::new(wafer_run::StaticConfigSource::default());
            let mut wafer = wafer_run::Wafer::new(cfg).expect("wafer");
            let mutator: Arc<dyn Block> = Arc::new(Mutator);
            let infos = vec![mutator.info()];
            wafer
                .register_block(BLOCK, mutator)
                .expect("register mutator");
            #[expect(
                clippy::arc_with_non_send_sync,
                reason = "`register_block` takes an `Arc<dyn Block>`; on this \
                          single-threaded target the router's `Arc` makes no \
                          cross-thread claim"
            )]
            let router: Arc<dyn Block> = Arc::new(
                impresspress_core::blocks::router::ImpresspressRouterBlock::with_extra_routes(
                    Arc::new(std::sync::RwLock::new(String::new())),
                    Arc::new(impresspress_core::features::AllEnabled),
                    infos,
                    vec![ExtraRoute::new("/b/a2test/", BLOCK, RouteAccess::Public)],
                ),
            );
            wafer
                .register_block(impresspress_core::blocks::router::ROUTER_BLOCK_ID, router)
                .expect("register router");
            wafer
                .add_flow_json(
                    r#"{ "id": "site-main", "name": "Site Main", "version": "0.1.0",
                         "description": "test",
                         "steps": [ { "id": "router", "block": "impresspress/router" } ],
                         "config": { "on_error": "stop" } }"#,
                )
                .expect("site-main");
            wafer.set_config_snapshot(HashMap::from([(
                impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY.to_string(),
                policy.to_string(),
            )]));
            wafer.seal().await.expect("seal");
            reset();
            store_wafer(wafer).expect("store");
            db
        }

        fn post(path: &str) -> web_sys::Request {
            let init = web_sys::RequestInit::new();
            init.set_method("POST");
            web_sys::Request::new_with_str_and_init(
                &format!("https://dev.impresspress.org{path}"),
                &init,
            )
            .expect("build request")
        }

        async fn logged(db: &BrowserDatabaseService) -> i64 {
            db.count(LOGS, &[]).await.expect("count request_logs")
        }

        async fn written(db: &BrowserDatabaseService) -> i64 {
            db.count(WRITES, &[]).await.expect("count writes")
        }

        /// **The order a reply promises**: a request that mutates twice is
        /// answered after exactly one export, which already holds its
        /// changes, and before its audit row exists — even once every
        /// microtask queued behind the reply has run, which is how the reply
        /// travels to `respondWith`. The audit row is written by the `after`
        /// promise, in an export of its own. Fails if the request is not one
        /// flush scope (two exports), if its flush is left to run after the
        /// reply (none), if the audit row is written on the response path, or
        /// if the after-response work starts before the reply has left.
        #[wasm_bindgen_test]
        async fn the_reply_is_durable_and_the_audit_row_comes_after_it() {
            let db = install("all").await;
            let before = opfs_writes();

            let (response, after) = dispatch_request(post("/b/a2test/write"))
                .await
                .expect("dispatch");

            assert_eq!(response.status(), 200);
            assert_eq!(opfs_writes() - before, 1, "one export, before the reply");
            assert_eq!(logged(&db).await, 0, "no audit row on the response path");
            drain_microtasks().await;
            assert_eq!(
                logged(&db).await,
                0,
                "the after-response work waits for the next task"
            );

            wasm_bindgen_futures::JsFuture::from(after)
                .await
                .expect("after-response work");
            assert_eq!(logged(&db).await, 1, "the audit row, after the reply");
            assert_eq!(
                opfs_writes() - before,
                2,
                "the after-response work exports once, on its own"
            );

            crate::db_init().await.expect("reopen from OPFS");
            assert_eq!(written(&db).await, 2, "the request's rows are on disk");
            assert_eq!(logged(&db).await, 1, "and so is the audit row");
        }

        /// **A reply never claims durability it does not have**: a mutating
        /// request whose one flush fails answers 500 in place of the flow's
        /// 200, and its after-response work still runs — its export persists
        /// the request's rows, which stayed live in memory. Fails if the
        /// flush result is ignored (the 200 goes out), or if a failed flush
        /// drops the after-response work.
        #[wasm_bindgen_test]
        async fn a_failed_request_flush_answers_500_and_the_next_flush_persists_it() {
            let db = install("all").await;
            let before = opfs_writes();

            fail_next_opfs_write();
            let (response, after) = dispatch_request(post("/b/a2test/write"))
                .await
                .expect("dispatch");

            assert_eq!(response.status(), 500, "the flow's 200 is replaced");
            assert_eq!(opfs_writes(), before, "the request's export failed");
            assert_eq!(written(&db).await, 2, "the change is live in memory");

            wasm_bindgen_futures::JsFuture::from(after)
                .await
                .expect("after-response work");
            assert_eq!(opfs_writes() - before, 1, "the after-response flush wrote");

            crate::db_init().await.expect("reopen from OPFS");
            assert_eq!(
                written(&db).await,
                2,
                "the next successful flush persisted the request's rows"
            );
            assert_eq!(logged(&db).await, 1, "and the audit row");
        }

        /// Under the browser default `errors`, a 200 leaves no row and costs
        /// no export after the reply; a 500 is logged.
        #[wasm_bindgen_test]
        async fn under_errors_only_a_server_error_is_logged() {
            let db = install("errors").await;
            let before = opfs_writes();

            let (ok, after) = dispatch_request(post("/b/a2test/write"))
                .await
                .expect("dispatch");
            wasm_bindgen_futures::JsFuture::from(after)
                .await
                .expect("after-response work");
            assert_eq!(ok.status(), 200);
            assert_eq!(logged(&db).await, 0, "a 200 is not logged under errors");
            assert_eq!(
                opfs_writes() - before,
                1,
                "and nothing is exported after it"
            );

            let (failed, after) = dispatch_request(post("/b/a2test/fail"))
                .await
                .expect("dispatch");
            assert_eq!(failed.status(), 500);
            wasm_bindgen_futures::JsFuture::from(after)
                .await
                .expect("after-response work");
            assert_eq!(logged(&db).await, 1, "a 500 is logged");
        }
    }
}
