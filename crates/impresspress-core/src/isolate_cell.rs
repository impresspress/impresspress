//! Interrupt-safe containers for isolate-lifetime state.
//!
//! # Why this exists
//!
//! On a request-isolated platform (Cloudflare Workers, and any other host
//! that can stop a guest mid-execution) a request can be **hard-terminated**
//! — for exceeding its CPU allowance, or because the host tore down its I/O
//! context. That termination is not a Rust unwind: **no destructor runs.**
//! `runtime_cache`'s `BUILD_LEASE_MS` already encodes this fact for a
//! `Cell<bool>`:
//!
//! > Normal Rust cancellation drops `BuildGuard`, but Cloudflare can
//! > hard-stop a request after it exceeds its CPU allowance. That
//! > termination does not guarantee Rust destructors run, so a plain boolean
//! > can remain set forever.
//!
//! A [`RefCell`](std::cell::RefCell) is the same hazard with a far worse
//! failure mode. Its borrow flag is released by `Ref`/`RefMut::drop`. If a
//! request is hard-stopped between `borrow_mut()` and the end of that
//! borrow's scope, **the flag stays set for the life of the wasm instance**,
//! and the next request that touches the same cell panics. Under
//! `panic = "abort"` (which every wasm32 build of this workspace uses) a
//! panic is a `unreachable` trap taken *inside* the future's `poll`. The
//! host's response promise is then never settled and never rejected, which
//! Cloudflare reports as:
//!
//! ```text
//! The Workers runtime canceled this request because it detected that your
//! Worker's code had hung and would never generate a response.
//! ```
//!
//! with almost no recorded CPU, because the victim request had barely
//! started.
//!
//! What becomes of the instance after that is up to worker-build's JS shim,
//! not this crate. The shim registers a Rust panic hook (worker's
//! `setPanicHook`) that marks the instance critical, and its next call into
//! the instance re-instantiates the wasm module (`__wbg_reset_state`): fresh
//! linear memory, the stranded cell and every cached runtime gone. That is
//! the code's account. No reset on Cloudflare has been confirmed by a trace,
//! but the 2026-10-15 analytics have failing rows whose memory figures look
//! like re-instantiated instances. A trap inside a poll that is not a Rust
//! panic reaches the same mark only through the shim's global `error`
//! listener (polls run in later microtasks, outside the `try` around each
//! handler call), and whether Workers delivers such a trap there is not
//! known. So one stranded borrow costs at least one more failed request, and
//! then either the instance is replaced, losing its cached runtime, or every
//! request that touches the cell fails for as long as it is not.
//!
//! The executor that polls every request was the other isolate-wide state a
//! hard stop could strand: js-sys 0.3.99's task queue, which the workspace
//! `Cargo.toml` patches (its `[patch.crates-io]` entry says why).
//!
//! # The rule
//!
//! Isolate-lifetime state reachable from the request path must not sit
//! behind a fallible borrow flag, and its critical section must contain only
//! infallible O(1) moves — never a caller-supplied closure, never
//! deserialization, never hashing.
//!
//! [`IsolateCell`] enforces both. It is built on [`Cell`], which has no
//! borrow state at all, so there is nothing an interrupted holder can
//! strand. Its critical sections are `take` → move → `set`; the worst a
//! hard stop inside one can do is *lose the cached value*, which every
//! caller already handles (it is indistinguishable from a cold isolate) and
//! which self-heals on the next successful store.
//!
//! [`IdentityCache`] layers the one shared usage pattern on top: a
//! single-entry, content-keyed cache whose expensive producer runs
//! **outside** the critical section.

use std::{cell::Cell, rc::Rc};

/// Isolate-lifetime state that cannot be wedged by an interrupted holder.
///
/// Every method's critical section is a `take`/`set` pair around infallible
/// moves. There is no borrow flag, so no method can panic and no hard stop
/// can leave the cell unusable — only empty.
pub struct IsolateCell<T> {
    slot: Cell<Option<T>>,
}

impl<T> std::fmt::Debug for IsolateCell<T> {
    /// Reports only occupancy. Reading the value out to format it would put
    /// caller-supplied `Debug` code inside the critical section, which is the
    /// one thing this type exists to prevent.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IsolateCell")
            .field("is_set", &self.is_set())
            .finish()
    }
}

impl<T> Default for IsolateCell<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> IsolateCell<T> {
    /// An empty cell. `const` so callers can use `thread_local! { … = const
    /// { … } }` and skip lazy initialization.
    pub const fn new() -> Self {
        Self {
            slot: Cell::new(None),
        }
    }

    /// Store `value`, discarding whatever was held.
    ///
    /// The previous value is dropped **after** the new one is installed
    /// ([`Cell::set`]'s contract), so a hard stop while the old value's
    /// destructor runs still leaves the cell holding the new value. That is
    /// strictly safer than `*slot.borrow_mut() = Some(value)`, which runs
    /// the old value's `Drop` while the borrow is held.
    pub fn set(&self, value: T) {
        self.slot.set(Some(value));
    }

    /// Remove and return the held value, if any.
    pub fn take(&self) -> Option<T> {
        self.slot.take()
    }

    /// Install `value` and return what was held.
    pub fn replace(&self, value: Option<T>) -> Option<T> {
        self.slot.replace(value)
    }

    /// Drop any held value.
    ///
    /// This is also the exact state a platform hard-stop inside one of this
    /// type's critical sections leaves behind — the value has been taken and
    /// not yet put back — which is what makes such an interruption
    /// survivable rather than fatal.
    pub fn clear(&self) {
        self.slot.set(None);
    }

    /// Keep the held value only if it equals `expected`, and report whether
    /// it did. A value that does not match is dropped.
    ///
    /// This is the "is this exact identity still current?" check, which every
    /// isolate-scoped decision keyed on an identity string needs (`is the
    /// prepared plan bypassed for THIS plan/environment pair?`). Folding it
    /// into the cell keeps the compare inside one `take`/`set` pair instead
    /// of forcing callers back onto a borrow, and keeps the semantics
    /// host-testable — the Cloudflare crate that uses it is wasm32-only and
    /// its own unit tests never execute anywhere.
    ///
    /// The comparison is a plain equality on borrowed data, not a
    /// caller-supplied closure: nothing that can panic, allocate, or await
    /// enters the critical section.
    pub fn retain_if_eq<Q>(&self, expected: &Q) -> bool
    where
        T: std::borrow::Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        match self.slot.take() {
            Some(held) if held.borrow() == expected => {
                self.slot.set(Some(held));
                true
            }
            // A stale value is deliberately not put back: encountering a new
            // identity is what clears the old one.
            _ => false,
        }
    }

    /// True while a value is held.
    pub fn is_set(&self) -> bool {
        let held = self.slot.take();
        let present = held.is_some();
        self.slot.set(held);
        present
    }
}

impl<T: Clone> IsolateCell<T> {
    /// Clone out the held value, leaving it in place.
    pub fn get(&self) -> Option<T> {
        let held = self.slot.take();
        let copy = held.clone();
        self.slot.set(held);
        copy
    }
}

/// A single-entry, content-keyed isolate cache.
///
/// `key` is the caller's identity string for the cached value (a Worker
/// version, a digest, a plan hash — whatever fully determines it). A miss
/// replaces the entry rather than growing the cache, which is what makes the
/// memory footprint independent of how many identities an isolate sees.
///
/// The value is an [`Rc`] so a cache hit hands out a handle without cloning
/// the payload.
#[derive(Debug)]
pub struct IdentityCache<T> {
    entry: IsolateCell<(String, Rc<T>)>,
}

// `IsolateCell<T>: Debug` for every `T`, so `IdentityCache`'s derive is fine.

impl<T> Default for IdentityCache<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> IdentityCache<T> {
    /// An empty cache. `const` for `thread_local! { … = const { … } }`.
    pub const fn new() -> Self {
        Self {
            entry: IsolateCell::new(),
        }
    }

    /// The cached value for `key`, or `None` on a miss.
    ///
    /// A non-matching entry is left in place; the caller decides whether to
    /// replace it via [`store`](Self::store).
    pub fn get(&self, key: &str) -> Option<Rc<T>> {
        let held = self.entry.take();
        let hit = held
            .as_ref()
            .filter(|(cached_key, _)| cached_key == key)
            .map(|(_, value)| Rc::clone(value));
        self.entry.replace(held);
        hit
    }

    /// Install `value` under `key`, replacing any existing entry.
    pub fn store(&self, key: String, value: Rc<T>) {
        self.entry.set((key, value));
    }

    /// Drop the entry. Also the state an interrupted critical section
    /// leaves — see [`IsolateCell::clear`].
    pub fn clear(&self) {
        self.entry.clear();
    }

    /// Return the value cached under `key`, producing it with `load` on a
    /// miss.
    ///
    /// **`load` runs with no critical section held.** That is the whole
    /// point of this method: the producers this cache exists for (plan
    /// decoding, digest verification, manifest parsing) are the most
    /// expensive synchronous work on a cold request, and therefore the
    /// likeliest place for the platform to hard-stop a request. Running them
    /// inside a lock would let that stop strand the lock — see the module
    /// documentation.
    ///
    /// A `load` that itself consults this cache is therefore well-defined
    /// (it observes a miss), and the outer store still wins. That
    /// re-entrancy is the observable proof that `load` is outside the
    /// critical section.
    pub fn get_or_try_insert_with<E>(
        &self,
        key: String,
        load: impl FnOnce() -> Result<T, E>,
    ) -> Result<Rc<T>, E> {
        if let Some(hit) = self.get(&key) {
            return Ok(hit);
        }
        let value = Rc::new(load()?);
        self.store(key, Rc::clone(&value));
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn cell_round_trips_and_survives_an_interrupted_critical_section() {
        let cell: IsolateCell<String> = IsolateCell::new();
        assert!(!cell.is_set());
        cell.set("v1".to_string());
        assert_eq!(cell.get().as_deref(), Some("v1"));
        // `get` must leave the value in place, not consume it.
        assert_eq!(cell.get().as_deref(), Some("v1"));

        // The exact state a hard stop between `take` and `set` leaves.
        cell.clear();
        assert_eq!(cell.get(), None);
        // …and the cell is still usable, which is the property that keeps a
        // terminated request from wedging every later one.
        cell.set("v2".to_string());
        assert_eq!(cell.get().as_deref(), Some("v2"));
    }

    #[test]
    fn set_installs_the_new_value_before_dropping_the_old() {
        thread_local! {
            static DROPPED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        }
        struct Noisy(&'static str);
        impl Drop for Noisy {
            fn drop(&mut self) {
                DROPPED.with(|log| log.borrow_mut().push(self.0));
            }
        }

        let cell: IsolateCell<Rc<Noisy>> = IsolateCell::new();
        cell.set(Rc::new(Noisy("old")));
        cell.set(Rc::new(Noisy("new")));
        // The old value is gone, so its destructor ran during the second
        // `set` — and the cell nevertheless holds the new value.
        assert_eq!(DROPPED.with(|log| log.borrow().clone()), vec!["old"]);
        assert!(cell.is_set());
    }

    #[test]
    fn identity_cache_hits_only_the_matching_key() {
        let cache: IdentityCache<u32> = IdentityCache::new();
        assert!(cache.get("a").is_none());
        cache.store("a".to_string(), Rc::new(1));
        assert_eq!(cache.get("a").as_deref(), Some(&1));
        assert!(cache.get("b").is_none());
        cache.store("b".to_string(), Rc::new(2));
        assert_eq!(cache.get("b").as_deref(), Some(&2));
        // Single entry: the replaced identity is gone, not retained.
        assert!(cache.get("a").is_none());
    }

    #[test]
    fn loader_runs_outside_the_critical_section() {
        let cache: IdentityCache<u32> = IdentityCache::new();
        let observed = cache
            .get_or_try_insert_with::<()>("k".to_string(), || {
                // A `RefCell`-based cache that held its borrow across the
                // loader would panic here instead of returning a miss.
                assert!(cache.get("k").is_none());
                Ok(7)
            })
            .unwrap();
        assert_eq!(*observed, 7);
        assert_eq!(cache.get("k").as_deref(), Some(&7));
    }

    #[test]
    fn a_failed_load_caches_nothing() {
        let cache: IdentityCache<u32> = IdentityCache::new();
        let error = cache.get_or_try_insert_with::<&str>("k".to_string(), || Err("boom"));
        assert_eq!(error.err(), Some("boom"));
        assert!(cache.get("k").is_none());
    }
}
