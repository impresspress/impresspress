//! Root and nested request scopes (`request_services`'s module doc), on the
//! scopes themselves and at every root call site.
//!
//! A request Workers stops mid-poll runs no destructor, so its scope guard
//! never puts its bundle back and the dead request stays current. These tests
//! stand in for that stop with a guard that is never dropped
//! (`request_services::strand_for_test`,
//! `impresspress_core::after_response::enter_for_test`), then poll the scope
//! under test and read what it left current and what it logged.
//!
//! The call-site tests drive the real `dispatch`, a real deferred task, the
//! real scheduled sweep and the real spawn wrapper, each with both request
//! cells stranded. Each fails if its site takes a nested constructor in
//! either cell: a nested scope puts the dead request back. A root does not
//! free the stopped request (its task's future still owns it); these tests
//! check the slot, which is what later polls read.

use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use impresspress_core::after_response::{self, AfterResponse};
use js_sys::{Array, Function, Object, Reflect};
use wafer_run::{Block, BlockInfo, InputStream, Message, OutputStream};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;

use crate::{
    request_services::{self, RequestServices},
    BoxedTask,
};

/// Captures `console.log`, which `worker::console_log!` writes to and the
/// Workers tail reads, until dropped. The original is put back on drop, so a
/// failing assertion cannot leave the global replaced.
struct ConsoleCapture {
    console: Object,
    original: JsValue,
    lines: Array,
}

impl ConsoleCapture {
    fn start() -> Self {
        let console: Object = Reflect::get(&js_sys::global(), &"console".into())
            .expect("console")
            .dyn_into()
            .expect("console is an object");
        let original = Reflect::get(&console, &"log".into()).expect("console.log");
        let lines = Array::new();
        let push = Function::new_with_args("line", "this.push(String(line))").bind0(&lines);
        Reflect::set(&console, &"log".into(), &push).expect("replace console.log");
        Self {
            console,
            original,
            lines,
        }
    }

    /// The `ip-root-found-stale` lines logged since [`Self::start`], sorted,
    /// without their last field, the instance. That field is checked here:
    /// it is this instance's traced identity once an earlier test in this
    /// binary ran a traced request, and `-` before.
    fn found_stale(&self) -> Vec<String> {
        let instance = crate::executor_trace::traced_instance().unwrap_or_else(|| "-".into());
        let mut lines: Vec<String> = self
            .lines
            .iter()
            .filter_map(|line| line.as_string())
            .filter(|line| line.starts_with("ip-root-found-stale"))
            .map(|line| {
                let (rest, last) = line.rsplit_once(' ').expect("an instance field");
                assert_eq!(last, instance, "the instance field of {line:?}");
                rest.to_string()
            })
            .collect();
        lines.sort();
        lines
    }
}

impl Drop for ConsoleCapture {
    fn drop(&mut self) {
        let _ = Reflect::set(&self.console, &"log".into(), &self.original);
    }
}

/// Pending once, so the scope under test is polled twice.
async fn yield_once() {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await;
}

/// The request whose after-response work is current, if any.
fn after_response_current() -> Option<Rc<AfterResponse>> {
    after_response::current_for_test()
}

// --- The scopes ---

/// A root's poll runs under its own bundle, logs the dead one once (only the
/// first poll finds it), and leaves nothing current.
#[wasm_bindgen_test]
async fn a_root_entered_with_a_stale_bundle_clears_it_and_logs_it() {
    let console = ConsoleCapture::start();
    request_services::strand_for_test(RequestServices::marker(91));

    let seen = Rc::new(Cell::new((None, None)));
    let observed = Rc::clone(&seen);
    request_services::scope_root(RequestServices::marker(5), async move {
        let first = request_services::current_marker();
        yield_once().await;
        observed.set((first, request_services::current_marker()));
    })
    .await;

    assert_eq!(
        seen.get(),
        (Some(5), Some(5)),
        "each poll ran under its own bundle"
    );
    assert_eq!(
        request_services::current_marker(),
        None,
        "the dead bundle is gone"
    );
    assert_eq!(
        console.found_stale(),
        ["ip-root-found-stale request-services 91"],
        "logged once, with the dead bundle's marker"
    );
}

/// A root that finds nothing current logs nothing.
#[wasm_bindgen_test]
async fn a_root_on_a_clean_slot_logs_nothing() {
    request_services::clear_for_test();
    let console = ConsoleCapture::start();
    request_services::scope_root(RequestServices::marker(5), yield_once()).await;
    assert_eq!(request_services::current_marker(), None);
    assert!(console.found_stale().is_empty());
}

/// A nested scope puts back what it found, so a scope inside another
/// request's scope leaves that request current. It neither clears nor logs
/// a dead bundle: that is the root's job.
#[wasm_bindgen_test]
async fn a_nested_scope_restores_the_previous_bundle() {
    let console = ConsoleCapture::start();
    request_services::strand_for_test(RequestServices::marker(92));

    request_services::scope(RequestServices::marker(6), async {
        assert_eq!(request_services::current_marker(), Some(6));
        yield_once().await;
    })
    .await;

    assert_eq!(request_services::current_marker(), Some(92));
    assert!(console.found_stale().is_empty());
    request_services::clear_for_test();
}

/// The after-response cell's root logs through the adapter's hook, with no
/// marker: an `AfterResponse` carries none.
#[wasm_bindgen_test]
async fn an_after_response_root_clears_and_logs_a_stale_request() {
    let console = ConsoleCapture::start();
    let dead = AfterResponse::new();
    after_response::enter_for_test(Rc::clone(&dead));

    after_response::scope_root(
        AfterResponse::new(),
        yield_once(),
        request_services::after_response_found_stale,
    )
    .await;

    assert!(after_response_current().is_none());
    assert_eq!(
        console.found_stale(),
        ["ip-root-found-stale after-response -"]
    );
    assert_eq!(
        Rc::strong_count(&dead),
        1,
        "the slot's reference was dropped (a stopped task's own reference is not)"
    );
}

// --- The call sites ---

/// Records the request-services marker its `handle` ran under, then answers
/// 200. With `deferred`, it first defers a task that records the marker the
/// task ran under and then defers again: outside its request's dispatch
/// nothing would run that second task, so it must be refused, not queued
/// into whichever request the after-response slot holds.
struct Records {
    handled: Arc<AtomicUsize>,
    deferred: Option<Arc<AtomicUsize>>,
}

#[wafer_block::wafer_async_trait]
impl Block for Records {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/records",
            "0.0.1",
            "http-handler@v1",
            "records its scope",
        )
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_run::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        self.handled.store(
            request_services::current_marker().unwrap_or(0),
            Ordering::SeqCst,
        );
        if let Some(seen) = self.deferred.clone() {
            impresspress_core::deferred::defer(async move {
                seen.store(
                    request_services::current_marker().unwrap_or(0),
                    Ordering::SeqCst,
                );
                impresspress_core::deferred::defer(async {});
            });
        }
        impresspress_core::http::ok_json(&serde_json::json!({ "ok": true }))
    }
}

async fn wafer_with(block: Records) -> wafer_run::Wafer {
    let mut wafer = wafer_run::Wafer::new(Arc::new(wafer_run::StaticConfigSource::default()))
        .expect("Wafer::new");
    wafer
        .register_block("test/records", Arc::new(block))
        .expect("register");
    wafer
        .add_flow_json(
            r#"{"id":"site-main","name":"t","version":"0.1.0","description":"t",
                "steps":[{"id":"records","block":"test/records"}],
                "config":{"on_error":"stop"}}"#,
        )
        .expect("flow");
    wafer.seal().await.expect("seal");
    wafer
}

fn get() -> worker::Request {
    worker::Request::new("https://example.test/anything", worker::Method::Get).expect("request")
}

/// Stand in for a request stopped mid-poll in both cells: its bundle, marked
/// `marker`, and its after-response work stay current. Returns the work, so
/// a test can show nothing more reached it.
fn strand_both(marker: usize) -> Rc<AfterResponse> {
    request_services::strand_for_test(RequestServices::marker(marker));
    let dead = AfterResponse::new();
    after_response::enter_for_test(Rc::clone(&dead));
    dead
}

/// Both cells empty, as every root leaves them.
fn assert_both_cleared() {
    assert_eq!(request_services::current_marker(), None, "request-services");
    assert!(after_response_current().is_none(), "after-response");
}

fn both_reported(marker: usize) -> [String; 2] {
    [
        "ip-root-found-stale after-response -".to_string(),
        format!("ip-root-found-stale request-services {marker}"),
    ]
}

/// The `fetch` future's roots: `dispatch` enters both cells as roots, so a
/// request dispatched after a hard stop runs under its own bundle and
/// clears both of the dead request's.
#[wasm_bindgen_test]
async fn dispatch_is_the_root_of_both_cells() {
    crate::init_isolate();
    let handled = Arc::new(AtomicUsize::new(0));
    let wafer = wafer_with(Records {
        handled: Arc::clone(&handled),
        deferred: None,
    })
    .await;
    let console = ConsoleCapture::start();
    strand_both(81);

    let response = crate::dispatch(
        &wafer,
        get(),
        RequestServices::marker(7),
        &AfterResponse::new(),
        &|_| {},
    )
    .await
    .expect("dispatch");

    assert_eq!(response.status_code(), 200);
    assert_eq!(
        handled.load(Ordering::SeqCst),
        7,
        "handled under its own bundle"
    );
    assert_both_cleared();
    assert_eq!(console.found_stale(), both_reported(81));
}

/// A deferred task is the root of both cells in the `wait_until` future it
/// runs in. A request stopped after this one's dispatch, before its task
/// ran, is cleared by the task, and the task's own `defer` is refused
/// rather than queued into the stopped request.
#[wasm_bindgen_test]
async fn a_deferred_task_is_a_root() {
    crate::init_isolate();
    let deferred = Arc::new(AtomicUsize::new(0));
    let wafer = wafer_with(Records {
        handled: Arc::new(AtomicUsize::new(0)),
        deferred: Some(Arc::clone(&deferred)),
    })
    .await;
    let handed: Rc<std::cell::RefCell<Vec<BoxedTask>>> = Rc::default();
    let sink = Rc::clone(&handed);
    crate::dispatch(
        &wafer,
        get(),
        RequestServices::marker(7),
        &AfterResponse::new(),
        &move |task| sink.borrow_mut().push(task),
    )
    .await
    .expect("dispatch");
    let tasks = std::mem::take(&mut *handed.borrow_mut());
    assert_eq!(tasks.len(), 1, "the handler deferred one task");

    let console = ConsoleCapture::start();
    let dead = strand_both(82);
    for task in tasks {
        task.await;
    }

    assert_eq!(
        deferred.load(Ordering::SeqCst),
        7,
        "it ran under its request's bundle"
    );
    assert!(
        dead.take_tasks().is_empty(),
        "the task's own defer did not land in the stopped request"
    );
    assert_both_cleared();
    assert_eq!(console.found_stale(), both_reported(82));
}

/// The `scheduled` future's roots: the sweep's scopes. The bare `Wafer` has
/// no auth-ui block, so the sweep itself fails; its scopes are what is under
/// test.
#[wasm_bindgen_test]
async fn the_scheduled_sweep_is_a_root() {
    let wafer = wafer_run::Wafer::new(Arc::new(wafer_run::StaticConfigSource::default()))
        .expect("Wafer::new");
    let console = ConsoleCapture::start();
    strand_both(83);

    let swept = crate::run_sweep(&wafer, RequestServices::marker(9)).await;

    assert!(swept.is_err(), "no auth-ui block to sweep with");
    assert_both_cleared();
    assert_eq!(console.found_stale(), both_reported(83));
}

/// A spawned producer is a task of its own, so its scopes are roots: it runs
/// under the bundle current when it was spawned and outside every request's
/// after-response work, and clears a request stopped since. Its final-write
/// guard is a no-op and its `defer` is refused, as `wrap_spawned` documents,
/// rather than landing in the stopped request.
#[wasm_bindgen_test]
async fn a_spawned_producer_is_a_root() {
    crate::init_isolate();
    let seen = Rc::new(Cell::new(None));
    let observed = Rc::clone(&seen);
    let task: wafer_block::spawn::SpawnedTask = Box::pin(async move {
        yield_once().await;
        observed.set(request_services::current_marker());
        drop(after_response::final_write());
        impresspress_core::deferred::defer(async {});
    });
    let wrapped = request_services::scope_sync(RequestServices::marker(10), || {
        request_services::wrap_spawned(task)
    });
    let console = ConsoleCapture::start();
    let dead = strand_both(84);

    wrapped.await;

    assert_eq!(
        seen.get(),
        Some(10),
        "it ran under its spawning request's bundle"
    );
    assert!(
        dead.take_final_writes().is_empty(),
        "no guard on the stopped request"
    );
    assert!(
        dead.take_tasks().is_empty(),
        "no task queued into the stopped request"
    );
    assert_both_cleared();
    assert_eq!(console.found_stale(), both_reported(84));
}
