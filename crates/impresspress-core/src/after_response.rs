//! The work one request leaves to run after its response: its
//! `request_logs` audit row and the tasks its handlers deferred
//! ([`crate::deferred`]).
//!
//! A host that must not pay for that work on the response path (the
//! Cloudflare Worker) runs each request's dispatch inside [`scope`] with a
//! fresh [`AfterResponse`], and afterwards takes the audit row and the tasks
//! out of it and runs them under that same request's services. Outside a
//! scope, the pipeline inserts the audit row inline and [`crate::deferred`]
//! spawns (native, browser).
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

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context as TaskContext, Poll},
};

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

/// Restores the previously current scope when a poll ends, including when
/// the inner poll unwinds.
struct ScopeGuard {
    previous: Option<Rc<AfterResponse>>,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        CURRENT.with(|slot| slot.replace(self.previous.take()));
    }
}

/// A future that makes its request's [`AfterResponse`] current on every
/// poll. See [`scope`].
pub struct Scoped<F> {
    after: Rc<AfterResponse>,
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let _guard = ScopeGuard {
            previous: CURRENT.with(|slot| slot.replace(Some(Rc::clone(&self.after)))),
        };
        self.inner.as_mut().poll(cx)
    }
}

/// Run `future` with the audit row and deferred tasks it produces queued
/// into `after` rather than inserted or spawned.
pub fn scope<F: Future>(after: Rc<AfterResponse>, future: F) -> Scoped<F> {
    Scoped {
        after,
        inner: Box::pin(future),
    }
}

/// Make `after` current for the rest of this thread, for a test that calls a
/// handler directly and then runs what it deferred.
#[cfg(test)]
pub(crate) fn enter_for_test(after: Rc<AfterResponse>) {
    CURRENT.with(|slot| slot.set(after));
}

/// The scope [`enter_for_test`] installed, if any.
#[cfg(test)]
pub(crate) fn current_for_test() -> Option<Rc<AfterResponse>> {
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
