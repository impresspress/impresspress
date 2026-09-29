//! Work a handler hands off to run after its response has gone out.
//!
//! A handler that must answer every caller alike — the public auth endpoints
//! that may not reveal whether an address has an account — cannot also do
//! slow work on only some of its paths: the reply would be the same bytes,
//! but it would arrive later for a registered address than for an unknown
//! one. [`defer`] takes that work off the response path. The handler returns
//! at once, on every path, and the work runs afterwards.
//!
//! Where "afterwards" runs depends on the host, so the host picks the mode:
//!
//! - [`DeferMode::Spawn`] (the default; native and the browser runtime): the
//!   task is spawned on the current executor with
//!   [`wafer_block::spawn_producer`] — `tokio::spawn` on native, where the
//!   process outlives any one request, and `spawn_local` in the browser.
//! - [`DeferMode::Queued`] (Cloudflare): the task is queued on the request
//!   being polled ([`crate::after_response`]), and the platform entry takes
//!   that request's tasks after its dispatch and hands them to
//!   `ctx.wait_until`, inside that request's own service bindings. A task
//!   spawned any other way on Workers is not tied to an event and is
//!   cancelled once the response is sent, which is why this mode exists. The
//!   queue is the request's, not the isolate's, so a task runs under the D1
//!   budget of the request that deferred it and no other request can drain
//!   or starve it (see [`crate::after_response`]).
//!
//! Tests select [`DeferMode::Queued`] too (through `queue_for_test`) and run
//! what the handler deferred, which makes "this path deferred a send"
//! observable without a clock.
//!
//! A deferred task has no response to report into, so it logs its own
//! failures. Anything whose failure the caller must hear about does not
//! belong here.
//!
//! A task can also be dropped before it finishes: a native process shutting
//! down drops every task its runtime still holds, a browser tab closing drops
//! its `spawn_local` tasks, and a task deferred under [`DeferMode::Queued`]
//! outside any request scope has nowhere to wait and is dropped at once. Each task carries a guard that logs a warning when that happens
//! ([`Unfinished`]), so a mail lost to a restart leaves a line. A Cloudflare
//! hard stop is the exception: it runs no destructors at all.

use std::{cell::Cell, future::Future, pin::Pin};

use wafer_block::MaybeSend;

/// A task queued by [`defer`] under [`DeferMode::Queued`].
pub type DeferredTask = Pin<Box<dyn Future<Output = ()>>>;

/// How [`defer`] runs a task on this thread (isolate). See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferMode {
    Spawn,
    Queued,
}

thread_local! {
    static MODE: Cell<DeferMode> = const { Cell::new(DeferMode::Spawn) };
}

/// Select how [`defer`] runs tasks on this thread. The Cloudflare entry sets
/// [`DeferMode::Queued`] once per isolate; native never calls it.
pub fn set_mode(mode: DeferMode) {
    MODE.with(|m| m.set(mode));
}

/// Run `task` after the current response, as this thread's [`DeferMode`]
/// says.
pub fn defer<F>(task: F)
where
    F: Future<Output = ()> + MaybeSend + 'static,
{
    let unfinished = Unfinished;
    let task = async move {
        task.await;
        std::mem::forget(unfinished);
    };
    match MODE.with(Cell::get) {
        DeferMode::Spawn => wafer_block::spawn_producer(task),
        DeferMode::Queued => {
            if crate::after_response::queue_task(Box::pin(task)).is_err() {
                // Dropping it runs the task's `Unfinished` warning too.
                tracing::error!(
                    "a task was deferred outside any request scope; nothing would run it"
                );
            }
        }
    }
}

/// Select [`DeferMode::Queued`] and give this test thread a request scope
/// that lasts for the rest of the test, so a handler called directly queues
/// what it defers where [`crate::after_response::current_for_test`] finds it.
#[cfg(test)]
pub(crate) fn queue_for_test() {
    set_mode(DeferMode::Queued);
    if crate::after_response::current_for_test().is_none() {
        crate::after_response::enter_for_test(crate::after_response::AfterResponse::new());
    }
}

/// Take every task deferred on this test thread's scope.
#[cfg(test)]
pub(crate) fn take_for_test() -> Vec<DeferredTask> {
    crate::after_response::current_for_test()
        .map(|after| after.take_tasks())
        .unwrap_or_default()
}

/// Travels inside every deferred task and is forgotten when the task
/// completes, so its `Drop` runs only for a task dropped unfinished.
struct Unfinished;

impl Drop for Unfinished {
    fn drop(&mut self) {
        tracing::warn!(
            "a deferred task was dropped before it finished (runtime shutdown, closed page or \
             a task deferred outside a request scope); work it carried, such as an auth mail, did not happen"
        );
        #[cfg(test)]
        DROPPED_UNFINISHED.with(|n| n.set(n.get() + 1));
    }
}

#[cfg(test)]
thread_local! {
    static DROPPED_UNFINISHED: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::*;

    #[tokio::test]
    async fn queued_tasks_wait_for_their_request_and_are_taken_once() {
        queue_for_test();
        let ran = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let ran = Arc::clone(&ran);
            defer(async move {
                ran.fetch_add(1, Ordering::SeqCst);
            });
        }
        assert_eq!(ran.load(Ordering::SeqCst), 0, "queued, not run");

        let tasks = take_for_test();
        assert_eq!(tasks.len(), 2);
        assert!(take_for_test().is_empty(), "taking must clear the queue");
        for task in tasks {
            task.await;
        }
        assert_eq!(ran.load(Ordering::SeqCst), 2);
        set_mode(DeferMode::Spawn);
    }

    #[tokio::test]
    async fn spawned_tasks_run_without_a_drain() {
        set_mode(DeferMode::Spawn);
        let (tx, rx) = tokio::sync::oneshot::channel();
        defer(async move {
            tx.send(()).expect("receiver alive");
        });
        assert!(take_for_test().is_empty(), "a spawned task is never queued");
        rx.await.expect("the spawned task ran");
    }

    #[tokio::test]
    async fn a_task_dropped_unfinished_is_reported_and_a_finished_one_is_not() {
        queue_for_test();
        let before = DROPPED_UNFINISHED.with(Cell::get);

        defer(async {});
        for task in take_for_test() {
            task.await;
        }
        assert_eq!(
            DROPPED_UNFINISHED.with(Cell::get),
            before,
            "a finished task reports nothing"
        );

        defer(std::future::pending());
        drop(take_for_test());
        assert_eq!(
            DROPPED_UNFINISHED.with(Cell::get),
            before + 1,
            "a task dropped before it finished is reported"
        );
        set_mode(DeferMode::Spawn);
    }

    /// Under [`DeferMode::Queued`] with no request in scope the task has
    /// nowhere to wait: it is dropped at once, and says so.
    #[test]
    fn a_task_queued_outside_a_request_is_dropped_and_reported() {
        set_mode(DeferMode::Queued);
        let before = DROPPED_UNFINISHED.with(Cell::get);
        defer(async {});
        assert_eq!(DROPPED_UNFINISHED.with(Cell::get), before + 1);
        set_mode(DeferMode::Spawn);
    }
}
