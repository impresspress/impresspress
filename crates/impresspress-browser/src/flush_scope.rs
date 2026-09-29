//! One flush per request.
//!
//! The browser database persists by exporting the whole sql.js database to
//! OPFS (`bridge.js` `dbFlush`), which costs the same whether one row or nine
//! changed. A request that activates a generation makes about nine
//! mutations; this scope lets them share one export. The request path opens
//! it around a request with [`run`]; `database::with_flush_mapped` asks
//! [`note_mutation`] whether a scope is current and, if so, records that a
//! flush is owed instead of exporting; [`run`] exports once when the scoped
//! future has finished — BEFORE it hands the future's output back, so a
//! reply built from that output still means the change is durable. A scope
//! in which nothing mutated exports nothing. Outside every scope nothing
//! changes: each logical mutation flushes itself.
//!
//! ## Interleaved requests
//!
//! The service worker handles several fetch events on one thread, so a
//! thread-local "current scope" set when a request starts and cleared when it
//! ends would let a status poll that finishes while a write is suspended
//! close the write's scope, and would credit the write's mutations to
//! whichever request happened to be current. The owed flag is therefore made
//! current on every poll of the scoped future and the previous one restored
//! when that poll returns (the shape of
//! `impresspress_core::after_response::Scoped`), so a mutation always marks
//! the flag of the request whose code made it, and each request flushes once,
//! at its own end, if it mutated.
//!
//! The restore is a drop guard, so it also runs when the inner poll unwinds:
//! a panic in one request's future cannot leave its flag current for the
//! next poll of another. (`wasm32-unknown-unknown` aborts on panic today, so
//! this is for correctness under any panic strategy, not a live path.)
//!
//! ## Nesting
//!
//! A scope opened while another is current joins it: the inner [`run`]
//! marks the outer flag and leaves the flush to the outer one, so an
//! activation inside a write inside a request is one scope and one export.

use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

thread_local! {
    /// The owed-flush flag of the scope whose future is being polled right
    /// now, if any.
    static CURRENT: RefCell<Option<Rc<Cell<bool>>>> = const { RefCell::new(None) };
}

fn current() -> Option<Rc<Cell<bool>>> {
    CURRENT.with(|slot| slot.borrow().clone())
}

/// Record that the scope being polled owes a flush: `true` when one is
/// current. `false` when no scope is current — the caller must flush itself.
pub(crate) fn note_mutation() -> bool {
    match current() {
        Some(owed) => {
            owed.set(true);
            true
        }
        None => false,
    }
}

/// Restores the previously current flag when a poll ends, including when the
/// inner poll unwinds.
struct ScopeGuard {
    previous: Option<Rc<Cell<bool>>>,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        CURRENT.with(|slot| slot.replace(self.previous.take()));
    }
}

/// A future that makes its scope's owed flag current on every poll. See
/// [`run`].
struct Scoped<F> {
    owed: Rc<Cell<bool>>,
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _guard = ScopeGuard {
            previous: CURRENT.with(|slot| slot.replace(Some(Rc::clone(&self.owed)))),
        };
        self.inner.as_mut().poll(cx)
    }
}

/// Run `future` as one flush scope, then flush the database once if anything
/// inside it mutated; returns the future's output and that flush's result.
///
/// The flush result is the request's durability verdict: an `Err` means a
/// mutation the output may report as done is in memory only, and the caller
/// must not answer as if it were durable. A scope opened inside another one
/// joins it and always returns `Ok(())` here — the outer scope's flush is the
/// one that counts.
pub async fn run<F: Future>(future: F) -> (F::Output, Result<(), String>) {
    let outer = current();
    let owed = outer.clone().unwrap_or_else(|| Rc::new(Cell::new(false)));
    let output = Scoped {
        owed: Rc::clone(&owed),
        inner: Box::pin(future),
    }
    .await;
    if outer.is_some() {
        return (output, Ok(()));
    }
    let flush = if owed.get() {
        crate::database::flush_through_bridge().await
    } else {
        Ok(())
    };
    (output, flush)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    };

    use wafer_core::interfaces::database::service::DatabaseService;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;
    use crate::database::{
        test_support::{fresh_db, opfs_writes},
        BrowserDatabaseService,
    };

    const TABLE: &str = "flush_scope_t";

    /// A fresh database holding one empty table, created (and flushed) before
    /// the caller starts counting writes.
    async fn fresh() -> BrowserDatabaseService {
        let db = fresh_db().await;
        db.exec_raw(
            &format!("CREATE TABLE {TABLE} (id TEXT PRIMARY KEY, name TEXT)"),
            &[],
        )
        .await
        .expect("create table");
        db
    }

    fn row(id: &str) -> HashMap<String, serde_json::Value> {
        HashMap::from([
            ("id".to_string(), serde_json::json!(id)),
            ("name".to_string(), serde_json::json!(format!("row {id}"))),
        ])
    }

    async fn mutate(db: &BrowserDatabaseService, id: &str) {
        db.create(TABLE, row(id)).await.expect("create");
    }

    async fn row_count(db: &BrowserDatabaseService) -> i64 {
        db.count(TABLE, &[]).await.expect("count")
    }

    /// Pending once, waking itself, then ready: a yield point that needs no
    /// timer, so a `join!` of two futures interleaves them deterministically.
    struct YieldNow(bool);

    impl Future for YieldNow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                return Poll::Ready(());
            }
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    fn yield_now() -> YieldNow {
        YieldNow(false)
    }

    /// Three mutations inside one scope are one export, and that one export
    /// holds all three rows.
    #[wasm_bindgen_test]
    async fn a_scope_with_three_mutations_flushes_once() {
        let db = fresh().await;
        let before = opfs_writes();
        let (_, flush) = run(async {
            for i in 0..3 {
                mutate(&db, &format!("r{i}")).await;
            }
            assert_eq!(opfs_writes(), before, "no export inside the scope");
        })
        .await;
        flush.expect("the scope's flush");
        assert_eq!(opfs_writes() - before, 1, "three mutations, one export");

        crate::db_init().await.expect("reopen from OPFS");
        assert_eq!(row_count(&db).await, 3, "the one export holds every row");
    }

    /// A scope that only reads writes nothing.
    #[wasm_bindgen_test]
    async fn a_scope_with_no_mutation_flushes_nothing() {
        let db = fresh().await;
        let before = opfs_writes();
        let (count, flush) = run(row_count(&db)).await;
        flush.expect("nothing to flush");
        assert_eq!(count, 0);
        assert_eq!(opfs_writes(), before, "a read-only scope exports nothing");
    }

    /// Nested scopes are one scope: the inner one leaves the flush it owes to
    /// the outer one, which flushes once at its own end.
    #[wasm_bindgen_test]
    async fn nested_scopes_flush_once() {
        let db = fresh().await;
        let before = opfs_writes();
        let (_, flush) = run(async {
            let (_, inner) = run(mutate(&db, "inner")).await;
            inner.expect("the inner scope reports no flush of its own");
            assert_eq!(opfs_writes(), before, "the inner scope does not flush");
            mutate(&db, "outer").await;
        })
        .await;
        flush.expect("the outer scope's flush");
        assert_eq!(opfs_writes() - before, 1, "nested scopes, one export");
        assert!(current().is_none(), "no scope is left current");

        crate::db_init().await.expect("reopen from OPFS");
        assert_eq!(row_count(&db).await, 2);
    }

    /// Two requests interleaved on one thread each flush once, at their own
    /// end. `join!` polls A, then B, each round:
    ///
    /// 1. A opens its scope and yields; B opens its scope, mutates, yields.
    /// 2. A mutates — while B's scope is open and suspended — then waits,
    ///    inside its scope, until B has finished; B completes and flushes.
    /// 3. A completes and flushes.
    ///
    /// A scope that is current from when its request starts until it ends,
    /// rather than on each poll, fails here: A's mutation in round 2 lands
    /// on B's flag, B flushes it, and A — the request that made it — flushes
    /// zero times. A scope that does not restore the previous flag after a
    /// poll fails the last assertion.
    #[wasm_bindgen_test]
    async fn interleaved_scopes_each_flush_once() {
        let db = fresh().await;
        let before = opfs_writes();
        let (b_done_tx, b_done_rx) = futures::channel::oneshot::channel::<()>();

        let a = run(async {
            yield_now().await;
            mutate(&db, "a").await;
            b_done_rx.await.expect("B finishes");
            opfs_writes()
        });
        let b = async {
            let (seen_by_b, flush) = run(async {
                mutate(&db, "b").await;
                yield_now().await;
                opfs_writes()
            })
            .await;
            b_done_tx.send(()).expect("A is waiting");
            (seen_by_b, flush)
        };
        let ((seen_by_a, a_flush), (seen_by_b, b_flush)) = futures::join!(a, b);

        a_flush.expect("A's flush");
        b_flush.expect("B's flush");
        assert_eq!(
            seen_by_b, before,
            "neither A's nor B's mutation exported before B's scope ended"
        );
        assert_eq!(
            seen_by_a - before,
            1,
            "B flushed at its own end, before A completed, and only once"
        );
        assert_eq!(opfs_writes() - before, 2, "one export per request");
        assert!(current().is_none(), "no scope is left current");
    }

    /// Outside any scope the old contract holds: one flush per mutation.
    #[wasm_bindgen_test]
    async fn without_a_scope_every_mutation_flushes() {
        let db = fresh().await;
        let before = opfs_writes();
        mutate(&db, "one").await;
        mutate(&db, "two").await;
        assert_eq!(
            opfs_writes() - before,
            2,
            "no scope, an export per mutation"
        );
    }
}
