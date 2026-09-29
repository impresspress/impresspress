//! One flush per request.
//!
//! The browser database persists by exporting the whole sql.js database to
//! OPFS (`bridge.js` `dbFlush`), which costs the same whether one row or nine
//! changed. A request that activates a generation makes about nine
//! mutations; this scope lets them share one export. The request path
//! ([`crate::runtime::dispatch_request`]) opens it around a request with
//! [`run`], and again around the work the request leaves for after its
//! reply; `database::with_flush_mapped` asks
//! [`note_mutation`] whether a scope is current and, if so, records that a
//! flush is owed instead of exporting; [`run`] exports once when the scoped
//! future has finished — BEFORE it hands the future's output back, so a
//! reply built from that output still means the change is durable. Outside
//! every scope nothing changes: each logical mutation flushes itself.
//!
//! ## The epoch rule
//!
//! A scope does not return until every mutation completed before its end —
//! its own or another scope's — is in an export that has been written. A
//! mutation is not always made by the request it belongs to: a write that
//! arrives while another write's activation is running is coalesced into
//! it, so its generation's rows are written inside the OTHER request's
//! poll and mark that request's scope; the waiting write's own scope owes
//! nothing, yet its reply reports the generation active. So the crate keeps
//! a mutation epoch (bumped when each logical mutation completes, in or out
//! of a scope) and an exported-through epoch (the epoch every written
//! export is known to hold), and a scope that ends while the first is ahead
//! of the second flushes although it mutated nothing. It first looks for an
//! export already under way that covers the epoch — one called at or after
//! it; `bridge.js` serializes exports, so an export holds every mutation
//! completed before its call — and awaits that one instead of adding its
//! own. The cost: a request that mutated nothing (a status poll) but ends
//! while another request's mutations are unexported pays for, or waits on,
//! one export. A scope that mutated nothing and ends when everything is
//! exported writes nothing.
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
//! `impresspress_core::after_response::Scoped`), so a mutation marks the
//! flag of the request whose code made it, and a request that mutated
//! flushes at its own end. Its export holds whatever else was in memory at
//! the time, other requests' mutations included; keeping the flag per
//! request is what lets a request that owes nothing, with nothing
//! unexported, skip the export — a preference, not a rule, since the epoch
//! rule above makes such a request export (or wait) whenever another
//! request's mutations are still unexported.
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

use futures::future::{FutureExt, LocalBoxFuture, Shared};

thread_local! {
    /// The owed-flush flag of the scope whose future is being polled right
    /// now, if any.
    static CURRENT: RefCell<Option<Rc<Cell<bool>>>> = const { RefCell::new(None) };
}

fn current() -> Option<Rc<Cell<bool>>> {
    CURRENT.with(|slot| slot.borrow().clone())
}

/// One export, shareable between every scope that waits on it. The output
/// is [`crate::database::flush_through_bridge`]'s.
type SharedFlush = Shared<LocalBoxFuture<'static, Result<(), String>>>;

/// The most recently started export, while it is still running: the
/// mutation epoch when it was called, its identity, and the export itself.
struct InFlight {
    covers: u64,
    id: u64,
    flush: SharedFlush,
}

thread_local! {
    /// How many logical mutations have completed on the one sql.js database.
    static MUTATION_EPOCH: Cell<u64> = const { Cell::new(0) };
    /// The mutation epoch every written export is known to hold.
    static EXPORTED_THROUGH: Cell<u64> = const { Cell::new(0) };
    /// See [`InFlight`].
    static IN_FLIGHT: RefCell<Option<InFlight>> = const { RefCell::new(None) };
    /// The id the next export started by [`flush_covering_now`] takes.
    static NEXT_FLUSH_ID: Cell<u64> = const { Cell::new(0) };
}

/// Record that a logical mutation has completed — whatever it returned,
/// since a failed one may have applied some of its statements. Called after
/// the mutation's statements have run, never before: an export called
/// between the two must not be credited with them.
pub(crate) fn mutation_done() {
    MUTATION_EPOCH.with(|epoch| epoch.set(epoch.get() + 1));
}

/// Record that the in-memory database was just loaded from OPFS
/// (`db_init`): it now matches what is written, whatever was unexported
/// before.
pub(crate) fn loaded_from_disk() {
    let epoch = MUTATION_EPOCH.with(Cell::get);
    EXPORTED_THROUGH.with(|through| through.set(epoch));
}

/// An export that will hold every mutation completed so far: the one in
/// flight when it was called after the last of them, or else a new one.
///
/// `bridge.js` runs exports one at a time, each after every earlier call
/// has finished, so an export's snapshot is taken no earlier than its call
/// and holds every mutation that had completed by then. The epoch is
/// therefore captured at the call — a lower bound for what the export will
/// hold, which can only cost a later scope an export, never let one return
/// early. When it is written, the exported-through epoch moves up to it.
pub(crate) fn flush_covering_now() -> SharedFlush {
    let epoch = MUTATION_EPOCH.with(Cell::get);
    let running = IN_FLIGHT.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|in_flight| in_flight.covers >= epoch)
            .map(|in_flight| in_flight.flush.clone())
    });
    if let Some(flush) = running {
        return flush;
    }
    let id = NEXT_FLUSH_ID.with(|next| next.replace(next.get() + 1));
    let flush = async move {
        let result = crate::database::flush_through_bridge().await;
        if result.is_ok() {
            EXPORTED_THROUGH.with(|through| through.set(through.get().max(epoch)));
        }
        IN_FLIGHT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|in_flight| in_flight.id == id) {
                *slot = None;
            }
        });
        result
    }
    .boxed_local()
    .shared();
    IN_FLIGHT.with(|slot| {
        *slot.borrow_mut() = Some(InFlight {
            covers: epoch,
            id,
            flush: flush.clone(),
        })
    });
    flush
}

/// Whether a mutation has completed that no written export is known to hold.
fn unexported_mutations() -> bool {
    MUTATION_EPOCH.with(Cell::get) > EXPORTED_THROUGH.with(Cell::get)
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
/// inside it mutated, or if any mutation completed so far is not yet
/// exported (the epoch rule in the module doc); returns the future's output
/// and that flush's result.
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
    let flush = if owed.get() || unexported_mutations() {
        flush_covering_now().await
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

    /// **A coalesced waiter.** Scope A mutates and wakes scope B, which
    /// mutated nothing, then stays open until B has returned — the shape of
    /// a write whose activation, driven from another write's poll, marks the
    /// other write's scope while its own owes nothing. B must not return
    /// (and so its request must not reply "durable") until an export holding
    /// A's row has been written. Fails if a scope that owes nothing returns
    /// without looking at mutations other scopes made.
    #[wasm_bindgen_test]
    async fn a_scope_that_wakes_after_another_scopes_mutation_flushes_before_returning() {
        let db = fresh().await;
        let before = opfs_writes();
        let (wake_b, b_woken) = futures::channel::oneshot::channel::<()>();
        let (b_returned, wait_for_b) = futures::channel::oneshot::channel::<()>();

        let a = run(async {
            mutate(&db, "a").await;
            wake_b.send(()).expect("B is waiting");
            wait_for_b.await.expect("B returns");
        });
        let b = async {
            let ((), flush) = run(async {
                b_woken.await.expect("A wakes B");
            })
            .await;
            flush.expect("B's flush");
            let exported = opfs_writes() - before;
            crate::db_init().await.expect("reopen from OPFS");
            let durable = row_count(&db).await;
            b_returned.send(()).expect("A is waiting");
            (exported, durable)
        };
        let (((), a_flush), (exported, durable)) = futures::join!(a, b);

        a_flush.expect("A's flush");
        assert_eq!(exported, 1, "B returned only after an export");
        assert_eq!(durable, 1, "and that export holds A's row");
    }

    /// The cheap half of the same rule: a scope that ends while another
    /// scope's export is in flight, and whose unexported mutations that
    /// export already covers, awaits it rather than adding one. A mutates,
    /// wakes B and ends (its export starts); B, which mutated nothing, ends
    /// in the same pass and returns once A's export is written. One export.
    #[wasm_bindgen_test]
    async fn a_scope_ending_during_a_covering_export_awaits_it() {
        let db = fresh().await;
        let before = opfs_writes();
        let (wake_b, b_woken) = futures::channel::oneshot::channel::<()>();

        let a = run(async {
            mutate(&db, "a").await;
            wake_b.send(()).expect("B is waiting");
        });
        let b = async {
            let ((), flush) = run(async {
                b_woken.await.expect("A wakes B");
            })
            .await;
            (flush, opfs_writes() - before)
        };
        let (((), a_flush), (b_flush, seen_by_b)) = futures::join!(a, b);

        a_flush.expect("A's flush");
        b_flush.expect("B awaited A's flush");
        assert_eq!(seen_by_b, 1, "B returned only once A's export was written");
        assert_eq!(opfs_writes() - before, 1, "and added none of its own");
    }

    /// Two requests interleaved on one thread each flush once, at their own
    /// end. `join!` polls A, then B, each round:
    ///
    /// 1. A opens its scope and yields; B opens its scope, mutates, yields.
    /// 2. A mutates — while B's scope is open and suspended — then waits,
    ///    inside its scope, until B has finished; B completes and flushes.
    /// 3. A completes and flushes.
    ///
    /// A scope that is made current once, when its request starts, and
    /// cleared when it ends, rather than on each poll, fails here: B's `run`
    /// first runs while A's flag is still current, so it joins A's scope as
    /// if nested, and B — a separate request — never flushes at its own end
    /// (A sees no export before it completes). A scope that installs its
    /// flag per poll but does not restore the previous one after the poll
    /// fails the same way, since A's flag is left current when B starts.
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
