//! The work one request leaves to run after its response: its
//! `request_logs` audit row, the tasks its handlers deferred
//! ([`crate::deferred`]), and the final writes its producers still owe
//! ([`final_write`]).
//!
//! A host that must not pay for that work on the response path (the
//! Cloudflare Worker, the browser's service worker) runs each request's
//! dispatch inside [`scope_root`] or [`scope`] with a fresh [`AfterResponse`],
//! and afterwards takes the audit row and the tasks out of it and runs them
//! under that same request's services. Outside a scope, the pipeline inserts the audit row
//! inline and [`crate::deferred`] spawns (native).
//!
//! The scope is selected per POLL, not per isolate: one Workers isolate
//! interleaves concurrent requests whenever a future returns `Pending`, and
//! each request is its own invocation with its own D1 query limit. Work kept
//! in an isolate-wide queue would be run by whichever request drained it,
//! under that request's budget: one request would pay for another's audit
//! row, and a password-reset mail could be skipped because a stranger's
//! request had spent its limit. Here each request's work belongs to that
//! request and nothing else can take it.
//!
//! # The audit row always fits
//!
//! The pipeline writes at most ONE audit row per request, and the slot here
//! holds at most one, so the statements it costs are known before the
//! request starts: [`AUDIT_ROW_STATEMENTS`]. The Cloudflare adapter holds
//! that many statements of the invocation's D1 limit back from the request's
//! own services — handlers and deferred tasks alike are refused before they
//! reach them — and writes the row with a handle that may use them. However
//! the request spends its budget, its audit row still fits.
//!
//! When the request-log policy is `off` no row is written, and
//! [`audit_row_reservation`] reserves nothing.
//!
//! What a deferred task needs is not known up front (a mail send reads the
//! email block's settings and writes its log), so nothing is reserved for
//! it. A task runs under its own request's services, after the audit row is
//! written and the reservation released, on everything the request and its
//! row left — and no other request can spend that. How much that is depends
//! on the handler: forgot-password and resend-verification defer after one
//! account lookup; signup defers after its settings reads and the
//! multi-statement account insert, so its verification mail runs on what
//! those left, which is still most of the limit.
//!
//! # A producer's one write after its consumer left
//!
//! A streamed response's producer runs on a task of its own, and the rule in
//! `wafer_block::spawn` is that it finishes its work before its terminal:
//! once its stream has ended, nothing keeps the request alive for it. A
//! producer cancelled because its consumer left has no terminal, yet some
//! still owe one bounded write: the storage block's access row for a read the
//! client abandoned, the part of a chat reply already handed to the
//! transport. [`final_write`] is the opt-in exception the wafer-run rule
//! allows ("unless the host extends the request for it").
//!
//! - The guard is taken in a block's `handle`, which runs while the request
//!   is dispatched, so it exists before the consumer could leave. From
//!   inside a producer's own poll it is a no-op: the producer is polled from
//!   its own task, outside this scope (Cloudflare re-enters only the
//!   request's *services* there), and a guard taken that late could race the
//!   head.
//! - The producer drops it once the write is done, or at its terminal. A
//!   panic or an early `return` drops it too.
//! - The host takes [`AfterResponse::take_final_writes`] once the dispatch
//!   has returned and keeps the request alive until [`FinalWrites::released`]
//!   resolves: Cloudflare in a `ctx.wait_until` of its own, the browser in
//!   the promise it hands `event.waitUntil`. The platform bounds that: on
//!   Workers, at most 30 s after the client disconnects or the body ends. A
//!   stream still being read has no such bound, and needs none.
//! - Natively there is no scope, the guard is a no-op, and the producer's
//!   tokio task carries the write while the process is up.
//!
//! The write runs in the producer under its own request's services, so on
//! Cloudflare it counts against that invocation's D1 statements exactly as a
//! handler's write does. Nothing is reserved for it: it is one or two
//! statements against a per-invocation limit of 1,000 (the Cloudflare
//! adapter's `database` module). It is waited on apart from the audit row
//! and the deferred tasks, never joined into their ordering. On Cloudflare a
//! request that queued neither (every SSE stream without the `resp.stream`
//! marker) runs no after-response work at all, so its
//! [`AUDIT_ROW_STATEMENTS`] stay reserved for the whole stream. That is
//! four of a thousand, harmless and older than final writes; it is not a
//! reason to route final writes through that work.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context as TaskContext, Poll},
};

use futures::channel::oneshot;
use wafer_core::interfaces::database::service::DatabaseService;

use crate::{deferred::DeferredTask, IsolateCell};

/// The most D1 statements writing one audit row with `create_many` sends:
/// the id-policy probe (the row carries no `id`), the column list the lazy
/// column-add reads when STRICT_SCHEMA is off, the column list the JSON
/// column lookup reads, and the insert. The two probes and the column list
/// are cached per isolate after their first answer, so a warm isolate sends
/// only the insert. A column the table lacks would add an `ALTER` each; the
/// table's migration creates every column the row names, so none is counted.
pub const AUDIT_ROW_STATEMENTS: u64 = 4;

/// The statements to reserve for a request's audit row under the
/// request-log policy `policy` (the raw
/// [`crate::config_vars::REQUEST_LOG_CONFIG_KEY`] value): none when the policy
/// writes no rows, [`AUDIT_ROW_STATEMENTS`] otherwise.
pub fn audit_row_reservation(policy: Option<&str>) -> u64 {
    if crate::pipeline::RequestLogPolicy::parse(policy).writes_rows() {
        AUDIT_ROW_STATEMENTS
    } else {
        0
    }
}

/// One queued audit row (table + column map), ready for
/// `DatabaseService::create_many`.
pub struct QueuedRequestLog {
    pub table: &'static str,
    pub data: HashMap<String, serde_json::Value>,
}

/// What one request leaves to run after its response.
///
/// [`IsolateCell`] rather than `RefCell` for the reason
/// [`crate::isolate_cell`] gives: a push can reallocate, and a Cloudflare hard
/// stop inside a borrow would strand its flag.
#[derive(Default)]
pub struct AfterResponse {
    audit_row: IsolateCell<QueuedRequestLog>,
    tasks: IsolateCell<Vec<DeferredTask>>,
    /// One receiver per [`FinalWrite`] taken in this request. `take`n, not
    /// read: [`IsolateCell::get`] needs `T: Clone`.
    final_writes: IsolateCell<Vec<oneshot::Receiver<()>>>,
}

impl AfterResponse {
    /// Nothing queued yet, for one request.
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Take the request's audit row, if it queued one.
    pub fn take_audit_row(&self) -> Option<QueuedRequestLog> {
        self.audit_row.take()
    }

    /// Take every task the request deferred, in the order it deferred them.
    pub fn take_tasks(&self) -> Vec<DeferredTask> {
        self.tasks.take().unwrap_or_default()
    }

    /// Take the final writes the request's producers still owe. See
    /// [`final_write`].
    ///
    /// A guard already released is left out: a buffered response's producer
    /// writes its row and drops its guard before the dispatch returns, so
    /// the host has nothing to wait on and registers nothing for it.
    pub fn take_final_writes(&self) -> FinalWrites {
        let mut writes = self.final_writes.take().unwrap_or_default();
        // `Ok(None)`: still held. `Err(Canceled)`: dropped. A guard is never
        // sent on, so `Ok(Some(()))` cannot occur.
        writes.retain_mut(|released| matches!(released.try_recv(), Ok(None)));
        FinalWrites(writes)
    }
}

/// A promise to the host that this request still has one bounded write to
/// make, possibly after its response has ended. Taken with [`final_write`]
/// while the request is dispatched; released by dropping it once the write
/// is done.
#[must_use = "dropping the guard releases it at once; hold it until the write is done"]
#[derive(Debug)]
pub struct FinalWrite {
    /// `None` outside any request scope: nothing waits on this guard. Held
    /// for its `Drop`, which is the release: it is never sent on.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "held for its Drop, which releases the host")
    )]
    done: Option<oneshot::Sender<()>>,
}

#[cfg(test)]
impl FinalWrite {
    /// Whether a request's host waits for this guard.
    pub(crate) fn is_held(&self) -> bool {
        self.done.is_some()
    }
}

/// Take a [`FinalWrite`] guard for the request being polled — or a no-op
/// guard when no request is in scope: natively, in a test, or inside a
/// producer's own poll. See the module doc's section on final writes.
pub fn final_write() -> FinalWrite {
    let Some(after) = current() else {
        return FinalWrite { done: None };
    };
    let (done, released) = oneshot::channel();
    let mut final_writes = after.final_writes.take().unwrap_or_default();
    final_writes.push(released);
    after.final_writes.set(final_writes);
    FinalWrite { done: Some(done) }
}

/// The final writes one request's producers still owe, taken from its
/// [`AfterResponse`] by the host.
#[must_use]
pub struct FinalWrites(Vec<oneshot::Receiver<()>>);

impl FinalWrites {
    /// Whether no producer of the request took a guard.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many guards the request's producers took.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Resolves once every guard has been released. A guard is only ever
    /// dropped, never sent on, so each receiver ends `Canceled`: that is the
    /// release.
    pub async fn released(self) {
        futures::future::join_all(self.0).await;
    }
}

thread_local! {
    /// The work of the request whose future is being polled, if any.
    static CURRENT: IsolateCell<Rc<AfterResponse>> = const { IsolateCell::new() };
}

fn current() -> Option<Rc<AfterResponse>> {
    CURRENT.with(IsolateCell::get)
}

/// Queue `row` as the audit row of the request being polled. Hands the row
/// back when no request is in scope — or, since the slot holds the one row
/// [`AUDIT_ROW_STATEMENTS`] reserves room for, when the request already
/// queued its row — so the caller inserts it itself.
pub(crate) fn queue_audit_row(row: QueuedRequestLog) -> Result<(), QueuedRequestLog> {
    let Some(after) = current() else {
        return Err(row);
    };
    match after.audit_row.take() {
        None => {
            after.audit_row.set(row);
            Ok(())
        }
        Some(queued) => {
            after.audit_row.set(queued);
            Err(row)
        }
    }
}

/// Queue `task` for the request being polled. Hands it back when no request
/// is in scope.
pub(crate) fn queue_task(task: DeferredTask) -> Result<(), DeferredTask> {
    let Some(after) = current() else {
        return Err(task);
    };
    let mut tasks = after.tasks.take().unwrap_or_default();
    tasks.push(task);
    after.tasks.set(tasks);
    Ok(())
}

/// What a [`Scoped`] future's poll puts back when it ends, including when
/// the inner poll unwinds.
struct ScopeGuard {
    restore: Option<Rc<AfterResponse>>,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        CURRENT.with(|slot| slot.replace(self.restore.take()));
    }
}

/// Which constructor made a [`Scoped`] future, and so what its polls put
/// back. Chosen at the call site, never at run time: see [`scope_root`].
#[derive(Clone, Copy)]
enum ScopeKind {
    /// [`scope`]: restore whatever the poll found.
    Nested,
    /// [`scope_root`]: restore nothing, and report a request found current
    /// on entry through `found_stale`.
    Root { found_stale: fn() },
}

/// A future that makes its request's [`AfterResponse`] current on every
/// poll. See [`scope`] and [`scope_root`].
pub struct Scoped<F> {
    after: Rc<AfterResponse>,
    kind: ScopeKind,
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let found = CURRENT.with(|slot| slot.replace(Some(Rc::clone(&self.after))));
        let _guard = match self.kind {
            ScopeKind::Nested => ScopeGuard { restore: found },
            ScopeKind::Root { found_stale } => {
                // Nothing a root's poll can be nested in has a request
                // current, so `found` is a request whose own guard never
                // ran: it was hard-stopped mid-poll. Dropping it here frees
                // that request's work.
                if found.is_some() {
                    found_stale();
                }
                ScopeGuard { restore: None }
            }
        };
        self.inner.as_mut().poll(cx)
    }
}

/// Run `future` with the audit row and deferred tasks it produces queued
/// into `after` rather than inserted or spawned, and the [`final_write`]
/// guards it takes registered on `after` for the host to wait on.
///
/// Each poll restores what it found current, so this nests inside another
/// request scope. A host's outermost scope, on a future it hands its
/// executor, is [`scope_root`] instead.
pub fn scope<F: Future>(after: Rc<AfterResponse>, future: F) -> Scoped<F> {
    Scoped {
        after,
        kind: ScopeKind::Nested,
        inner: Box::pin(future),
    }
}

/// [`scope`] for the outermost after-response scope of a future the host
/// hands its executor (the Cloudflare adapter's `dispatch`, polled from the
/// Worker's `fetch` future). Each poll leaves no request current when it
/// ends, rather than restoring what it found.
///
/// # Why roots exist
///
/// A host that can stop a guest mid-poll (Cloudflare Workers' CPU limit)
/// skips every destructor, the scope guard's included, so the request it
/// stopped stays current for the life of the isolate. A nested scope
/// restores what it found, so every later request would put the dead
/// request's work back at the end of each poll, and anything polled outside
/// a scope — a producer's own task, a host task — would queue its audit row,
/// deferred tasks and final writes into it instead of being told no request
/// is in scope.
///
/// The executor never polls one task inside another's poll, so nothing a
/// root's poll can be nested in has a request current. A root therefore
/// restores nothing, which clears a dead request at the end of the next
/// root's first poll. Nesting is decided by the call site, not counted at
/// run time: a depth counter would be stranded by the same hard stop.
///
/// A root that finds a request current on entry calls `found_stale`: a
/// request was hard-stopped in this isolate. The host owns the log line,
/// since only it knows where its logs go.
pub fn scope_root<F: Future>(after: Rc<AfterResponse>, future: F, found_stale: fn()) -> Scoped<F> {
    Scoped {
        after,
        kind: ScopeKind::Root { found_stale },
        inner: Box::pin(future),
    }
}

/// Make `after` current for the rest of this thread, as a request whose
/// guard never ran leaves it: for a test that calls a handler directly and
/// then runs what it deferred, or one that stands in for a hard-stopped
/// request.
#[cfg(any(test, feature = "test-hooks"))]
pub fn enter_for_test(after: Rc<AfterResponse>) {
    CURRENT.with(|slot| slot.set(after));
}

/// The request current on this thread, if any.
#[cfg(any(test, feature = "test-hooks"))]
pub fn current_for_test() -> Option<Rc<AfterResponse>> {
    current()
}

/// A write of an audit row that failed.
#[derive(Debug)]
pub struct PersistFailure {
    pub table: &'static str,
    pub error: String,
}

/// Write `row` — one request's audit row — through `db`, a handle that may
/// use the [`AUDIT_ROW_STATEMENTS`] the request's own services were held
/// back from. A failure is returned for the caller to log: there is no
/// response left to carry it.
pub async fn persist_audit_row(
    db: &dyn DatabaseService,
    row: QueuedRequestLog,
) -> Result<(), PersistFailure> {
    let table = row.table;
    db.create_many(table, vec![row.data])
        .await
        .map(|_| ())
        .map_err(|error| PersistFailure {
            table,
            error: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        sync::{Arc, Mutex},
    };

    use serde_json::json;
    use wafer_core::interfaces::database::service::{DatabaseError, StatementBudget};

    use super::*;

    const TABLE: &str = "test__audit__rows";

    fn row(label: &str) -> QueuedRequestLog {
        let mut data = HashMap::new();
        data.insert("path".to_string(), json!(label));
        QueuedRequestLog { table: TABLE, data }
    }

    fn label(row: &QueuedRequestLog) -> String {
        row.data["path"].as_str().unwrap().to_string()
    }

    /// Yield once, so a `join!` of two requests polls the other one before
    /// this one continues.
    async fn yield_once() {
        let mut yielded = false;
        std::future::poll_fn(|cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }

    thread_local! {
        /// How often a root reported a request left current, on this test's
        /// thread. A `found_stale` hook is a `fn`, so it cannot capture.
        static FOUND_STALE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn count_found_stale() {
        FOUND_STALE.with(|count| count.set(count.get() + 1));
    }

    fn found_stale() -> usize {
        FOUND_STALE.with(std::cell::Cell::get)
    }

    /// A request hard-stopped mid-poll leaves its work current: its scope
    /// guard never ran. The next root's poll runs under its own request,
    /// reports the dead one, and leaves nothing current, so a row queued
    /// outside every scope afterwards comes back to its caller.
    #[tokio::test]
    async fn a_root_clears_a_request_a_hard_stop_left_current_and_reports_it() {
        let dead = AfterResponse::new();
        enter_for_test(Rc::clone(&dead));
        let live = AfterResponse::new();
        let reported = found_stale();

        scope_root(
            Rc::clone(&live),
            async {
                assert!(queue_audit_row(row("live")).is_ok());
                yield_once().await;
            },
            count_found_stale,
        )
        .await;

        assert_eq!(
            found_stale() - reported,
            1,
            "reported on the first poll only"
        );
        assert!(current_for_test().is_none(), "the dead request is cleared");
        assert!(queue_audit_row(row("outside")).is_err());
        assert_eq!(label(&live.take_audit_row().unwrap()), "live");
        assert!(
            dead.take_audit_row().is_none(),
            "nothing reached the dead request"
        );
        assert_eq!(
            Rc::strong_count(&dead),
            1,
            "the slot's reference was dropped"
        );
    }

    /// A root that finds nothing current reports nothing.
    #[tokio::test]
    async fn a_root_on_a_clean_slot_reports_nothing() {
        CURRENT.with(IsolateCell::clear);
        let reported = found_stale();
        scope_root(AfterResponse::new(), yield_once(), count_found_stale).await;
        assert_eq!(found_stale(), reported);
        assert!(current_for_test().is_none());
    }

    /// A nested scope still restores what it found, so it stays correct
    /// inside another request scope; it does not clear a dead request.
    #[tokio::test]
    async fn a_nested_scope_restores_what_it_found() {
        let outer = AfterResponse::new();
        enter_for_test(Rc::clone(&outer));

        scope(AfterResponse::new(), yield_once()).await;

        assert!(
            current_for_test().is_some_and(|after| Rc::ptr_eq(&after, &outer)),
            "the nested scope put back what it found"
        );
        CURRENT.with(IsolateCell::clear);
    }

    /// Two requests interleaved in one isolate, each queueing its audit row
    /// and a deferred task on either side of a yield: each request's scope
    /// holds its own row and task and no other's, and nothing is queued
    /// outside a request.
    #[tokio::test]
    async fn interleaved_requests_each_keep_only_their_own_row_and_tasks() {
        let after_a = AfterResponse::new();
        let after_b = AfterResponse::new();
        let order = RefCell::new(Vec::new());
        let ran = Rc::new(RefCell::new(Vec::new()));
        let request = |name: &'static str| {
            let order = &order;
            let ran = Rc::clone(&ran);
            async move {
                assert!(queue_audit_row(row(name)).is_ok(), "in scope");
                order.borrow_mut().push(name);
                yield_once().await;
                let task: DeferredTask = Box::pin(async move { ran.borrow_mut().push(name) });
                assert!(queue_task(task).is_ok(), "in scope");
                order.borrow_mut().push(name);
            }
        };
        tokio::join!(
            scope(Rc::clone(&after_a), request("a")),
            scope(Rc::clone(&after_b), request("b")),
        );

        let mut first_halves = order.borrow()[..2].to_vec();
        first_halves.sort_unstable();
        assert_eq!(
            first_halves,
            ["a", "b"],
            "both requests must queue before either continues, or this test is not \
             interleaving them: {:?}",
            order.borrow()
        );
        assert_eq!(label(&after_a.take_audit_row().unwrap()), "a");
        assert_eq!(label(&after_b.take_audit_row().unwrap()), "b");
        for task in after_b.take_tasks() {
            task.await;
        }
        assert_eq!(*ran.borrow(), ["b"], "b's scope holds b's task only");
        for task in after_a.take_tasks() {
            task.await;
        }
        assert_eq!(*ran.borrow(), ["b", "a"]);
        assert!(
            queue_audit_row(row("outside")).is_err(),
            "outside a request the row comes back to be inserted"
        );
    }

    /// A guard taken while a request is polled is that request's: its host
    /// sees one final write, and waiting on it lasts until the guard is
    /// dropped, not until the request's future ended.
    #[tokio::test]
    async fn a_guard_taken_in_a_scope_holds_its_requests_final_writes_until_dropped() {
        let after = AfterResponse::new();
        let other = AfterResponse::new();
        let guard = scope(Rc::clone(&after), async { final_write() }).await;
        assert!(guard.is_held(), "a guard taken in a scope is waited on");

        let writes = after.take_final_writes();
        assert_eq!(writes.len(), 1, "the request holds its producer's write");
        assert!(
            other.take_final_writes().is_empty(),
            "and no other request does"
        );
        let mut released = Box::pin(writes.released());
        assert!(
            futures::poll!(released.as_mut()).is_pending(),
            "the host waits while the guard is held, after the request's future ended"
        );
        drop(guard);
        assert!(
            futures::poll!(released.as_mut()).is_ready(),
            "dropping the guard releases the host"
        );
        assert!(
            after.take_final_writes().is_empty(),
            "the host takes the writes once"
        );
    }

    /// A guard released before the host takes the writes — a buffered
    /// response's, whose row was written inside the dispatch — leaves
    /// nothing to wait on, so the host registers nothing for it; one still
    /// held is kept.
    #[tokio::test]
    async fn a_guard_released_before_the_host_takes_the_writes_is_left_out() {
        let after = AfterResponse::new();
        let (released, held) =
            scope(Rc::clone(&after), async { (final_write(), final_write()) }).await;
        drop(released);
        let writes = after.take_final_writes();
        assert_eq!(writes.len(), 1, "only the guard still held is waited on");
        drop(held);
        writes.released().await;

        let after = AfterResponse::new();
        drop(scope(Rc::clone(&after), async { final_write() }).await);
        assert!(
            after.take_final_writes().is_empty(),
            "a request whose every guard is released has no final write"
        );
    }

    /// Outside any request scope — natively, or inside a producer's own
    /// poll — the guard is a no-op: nothing is registered and nothing waits.
    #[tokio::test]
    async fn a_guard_taken_outside_any_scope_is_a_no_op() {
        let after = AfterResponse::new();
        let guard = final_write();
        assert!(
            !guard.is_held(),
            "no host waits on a guard taken outside a scope"
        );
        drop(guard);
        assert!(after.take_final_writes().is_empty());
    }

    /// A request whose policy writes no rows reserves nothing; every other
    /// policy reserves the row's statements.
    #[test]
    fn only_a_policy_that_writes_rows_reserves_for_one() {
        assert_eq!(audit_row_reservation(Some("off")), 0);
        assert_eq!(audit_row_reservation(Some(" off ")), 0);
        for policy in [None, Some("all"), Some("errors"), Some("typo")] {
            assert_eq!(
                audit_row_reservation(policy),
                AUDIT_ROW_STATEMENTS,
                "{policy:?}"
            );
        }
    }

    /// The slot holds the one row the reservation covers: a second row from
    /// the same request comes back to be inserted through the request's own
    /// services instead of silently taking reserved room.
    #[tokio::test]
    async fn a_request_queues_at_most_one_audit_row() {
        let after = AfterResponse::new();
        scope(Rc::clone(&after), async {
            assert!(queue_audit_row(row("first")).is_ok());
            assert!(queue_audit_row(row("second")).is_err());
        })
        .await;
        assert_eq!(label(&after.take_audit_row().unwrap()), "first");
        assert!(after.take_audit_row().is_none());
    }

    /// A database whose `statement_budget` always reports room, and whose
    /// `create_many` refuses anyway — D1 answering with its own limit, which
    /// the budget did not see coming. Every other operation is the in-memory
    /// SQLite service's and is never called.
    struct RefusingDb {
        inner: Arc<dyn DatabaseService>,
        attempts: Mutex<usize>,
    }

    impl RefusingDb {
        fn inner_service(&self) -> &dyn DatabaseService {
            self.inner.as_ref()
        }
    }

    wafer_core::forward_database_service! {
        impl DatabaseService for RefusingDb {
            forward_to inner_service();

            ops {
                get: forward,
                list: forward,
                create: forward,
                create_many: custom,
                update: forward,
                delete: forward,
                count: forward,
                sum: forward,
                query_raw: forward,
                exec_raw: forward,
                delete_where: forward,
                delete_where_count: forward,
                take_where: forward,
                update_where: forward,
                update_where_count: forward,
                increment_field_where: forward,
                upsert: forward,
                aggregate: forward,
                batch: forward,
                insert_guarded: forward,
                update_guarded: forward,
                ensure_schema_table: forward,
                ensure_schema_tables: forward,
                schema_table_exists: forward,
                schema_columns: forward,
                schema_drop_table: forward,
                schema_add_column: forward,
                set_strict_schema: forward,
                statement_budget: custom,
            }

            async fn create_many(
                &self,
                _collection: &str,
                _rows: Vec<HashMap<String, serde_json::Value>>,
            ) -> Result<i64, DatabaseError> {
                *self.attempts.lock().unwrap() += 1;
                Err(DatabaseError::ResourceExhausted(
                    "D1 refused: too many queries".into(),
                ))
            }

            fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
                Ok(StatementBudget::Limited { limit: 1000, used: 0 })
            }
        }
    }

    /// A write refused although the budget reported room is attempted once
    /// and handed back as a failure naming the table and the refusal, for the
    /// caller to log — never swallowed.
    #[tokio::test]
    async fn a_refused_write_is_reported_not_swallowed() {
        let db = RefusingDb {
            inner: Arc::new(
                wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                    .expect("in-memory sqlite"),
            ),
            attempts: Mutex::new(0),
        };
        let failure = persist_audit_row(&db, row("a"))
            .await
            .expect_err("the refusal is reported");
        assert_eq!(failure.table, TABLE);
        assert!(failure.error.contains("too many queries"), "{failure:?}");
        assert_eq!(*db.attempts.lock().unwrap(), 1);
    }
}
