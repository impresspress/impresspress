//! A bounded in-memory cache of objects the browser `StorageService` has
//! recently read or written.
//!
//! **Why it cannot go stale.** The Service Worker is the only writer of the
//! OPFS `storage` directory — the page never opens it, and `loader.js`'s wipe
//! runs only after the workers are unregistered — and every write the worker
//! makes goes through `BrowserStorageService`, which reports each one here
//! (`put`, `put_streaming`, `delete`, `delete_folder`). There is one cache per
//! worker (see `storage::READ_CACHE`), not one per service instance, because
//! the runtime builds a fresh service for every runtime it builds while the
//! OPFS they all write is the same.
//!
//! **What it serves.** The dev sandbox re-reading `workspace.json` several
//! times per site write, the published blobs it reads back while publishing,
//! and every site file the web block serves (`wafer-run/web/site/**`, one
//! buffered `get` per request) — each of which was otherwise an OPFS directory
//! walk, a file read and a metadata-sidecar read.
//!
//! **Bounds.** At most [`BUDGET_BYTES`] of object bytes in total, least
//! recently used evicted first, and no object over [`MAX_ENTRY_BYTES`] is held
//! at all, so a site larger than the budget degrades to reading OPFS exactly
//! as it did before the cache, never worse.
//!
//! **Keys are object paths.** OPFS stores `(folder "a", key "b/c")` and
//! `(folder "a/b", key "c")` as the same file, so the cache keys an object by
//! its joined path `"{folder}/{key}"`: two names for one file are one entry,
//! and a folder's entries are exactly the paths under `"{folder}/"`.
//!
//! **Concurrent requests.** The worker interleaves requests at every `await`,
//! so a read that started before a write finished may carry the bytes the
//! write replaced. Every write therefore advances an epoch when it starts and
//! when it ends, and a read inserts what it fetched only if the epoch it saw
//! before its OPFS read is still current ([`ReadCache::insert_read`]). A write
//! inserts its own bytes only if no other write overlapped it
//! ([`ReadCache::insert_written`]); otherwise OPFS's final state is not known
//! here and the key is left uncached.

use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
};

use wafer_core::interfaces::storage::service::ObjectInfo;

/// Total object bytes the cache holds before it evicts.
pub(crate) const BUDGET_BYTES: usize = 16 * 1024 * 1024;

/// Largest object the cache holds; anything bigger is always read from OPFS.
pub(crate) const MAX_ENTRY_BYTES: usize = 1024 * 1024;

/// One cached object: its bytes, shared rather than copied until a caller
/// needs a `Vec`, and the `ObjectInfo` a read of it reports.
#[derive(Clone)]
pub(crate) struct Cached {
    pub(crate) data: Rc<[u8]>,
    pub(crate) info: ObjectInfo,
}

/// The epoch a read or write observed when it started; see the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ticket(u64);

struct Slot {
    cached: Cached,
    /// The recency stamp of this entry's live record in `order`.
    stamp: u64,
}

pub(crate) struct ReadCache {
    entries: HashMap<String, Slot>,
    /// Recency records, least recent first. A `get` appends a new record
    /// rather than moving the old one (which would be a linear scan), so a
    /// record whose stamp no longer matches its entry's is dead; dead records
    /// are skipped by eviction and dropped by [`Self::compact`], which keeps
    /// this at most twice the number of entries.
    order: VecDeque<(String, u64)>,
    bytes: usize,
    budget: usize,
    max_entry: usize,
    next_stamp: u64,
    epoch: u64,
}

fn object_path(folder: &str, key: &str) -> String {
    format!("{folder}/{key}")
}

impl ReadCache {
    pub(crate) fn new() -> Self {
        Self::with_bounds(BUDGET_BYTES, MAX_ENTRY_BYTES)
    }

    /// A cache with its own bounds — how the tests exercise eviction without
    /// allocating the production budget.
    pub(crate) fn with_bounds(budget: usize, max_entry: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            budget,
            max_entry,
            next_stamp: 0,
            epoch: 0,
        }
    }

    /// The cached object, marked most recently used.
    pub(crate) fn get(&mut self, folder: &str, key: &str) -> Option<Cached> {
        let path = object_path(folder, key);
        let stamp = self.next_stamp;
        let slot = self.entries.get_mut(&path)?;
        slot.stamp = stamp;
        let cached = slot.cached.clone();
        self.next_stamp += 1;
        self.order.push_back((path, stamp));
        self.compact();
        Some(cached)
    }

    /// The ticket a read takes before it goes to OPFS.
    pub(crate) fn ticket(&self) -> Ticket {
        Ticket(self.epoch)
    }

    /// Cache what a read fetched from OPFS — unless a write started or
    /// finished since the read took `ticket`, in which case the bytes may be
    /// the ones that write replaced.
    pub(crate) fn insert_read(
        &mut self,
        ticket: Ticket,
        folder: &str,
        key: &str,
        data: &[u8],
        info: &ObjectInfo,
    ) {
        if ticket == self.ticket() {
            self.insert(folder, key, data, info);
        }
    }

    /// A write to `folder/key` is starting: forget the object and void every
    /// read in flight. The returned ticket is what [`Self::insert_written`]
    /// checks at the end of the write.
    pub(crate) fn invalidate(&mut self, folder: &str, key: &str) -> Ticket {
        self.remove(folder, key);
        self.epoch += 1;
        self.ticket()
    }

    /// [`Self::invalidate`] for every object under `folder`, sub-folders
    /// included.
    pub(crate) fn invalidate_folder(&mut self, folder: &str) -> Ticket {
        self.remove_folder(folder);
        self.epoch += 1;
        self.ticket()
    }

    /// A `put` that took `ticket` from [`Self::invalidate`] has written
    /// `data`: cache it, unless another write overlapped this one — OPFS's
    /// final bytes are then whichever write's `close()` ran last, which is
    /// not known here, so the key is left uncached instead. Either way the
    /// reads in flight are voided, since they may have fetched the bytes this
    /// write replaced.
    pub(crate) fn insert_written(
        &mut self,
        ticket: Ticket,
        folder: &str,
        key: &str,
        data: &[u8],
        info: &ObjectInfo,
    ) {
        let undisturbed = ticket == self.ticket();
        self.invalidate(folder, key);
        if undisturbed {
            self.insert(folder, key, data, info);
        }
    }

    /// Hold `data` as `folder/key`, replacing any previous entry, then evict
    /// least recently used entries until the total is within the budget. An
    /// object over the per-entry cap is not held (and is not copied).
    pub(crate) fn insert(&mut self, folder: &str, key: &str, data: &[u8], info: &ObjectInfo) {
        if data.len() > self.max_entry || data.len() > self.budget {
            self.remove(folder, key);
            return;
        }
        let path = object_path(folder, key);
        self.remove_path(&path);
        let stamp = self.next_stamp;
        self.next_stamp += 1;
        self.bytes += data.len();
        self.entries.insert(
            path.clone(),
            Slot {
                cached: Cached {
                    data: Rc::from(data),
                    info: info.clone(),
                },
                stamp,
            },
        );
        self.order.push_back((path, stamp));
        while self.bytes > self.budget {
            self.evict_one();
        }
        self.compact();
    }

    pub(crate) fn remove(&mut self, folder: &str, key: &str) {
        self.remove_path(&object_path(folder, key));
    }

    /// Forget every object under `folder`, sub-folders included.
    pub(crate) fn remove_folder(&mut self, folder: &str) {
        let prefix = format!("{folder}/");
        let doomed: Vec<String> = self
            .entries
            .keys()
            .filter(|path| path.starts_with(&prefix))
            .cloned()
            .collect();
        for path in doomed {
            self.remove_path(&path);
        }
    }

    /// Its record in `order` becomes dead and is dropped by eviction or
    /// [`Self::compact`].
    fn remove_path(&mut self, path: &str) {
        if let Some(slot) = self.entries.remove(path) {
            self.bytes -= slot.cached.data.len();
        }
        self.compact();
    }

    fn evict_one(&mut self) {
        while let Some((path, stamp)) = self.order.pop_front() {
            let live = self
                .entries
                .get(&path)
                .is_some_and(|slot| slot.stamp == stamp);
            if live {
                if let Some(slot) = self.entries.remove(&path) {
                    self.bytes -= slot.cached.data.len();
                }
                return;
            }
        }
    }

    /// Drop dead recency records once they outnumber the live ones, so
    /// repeated hits cannot grow `order` without bound. Amortised O(1): a
    /// compaction halves `order` at least, and happens only after as many
    /// appends as there are entries.
    fn compact(&mut self) {
        if self.order.len() <= 2 * self.entries.len() + 16 {
            return;
        }
        let entries = &self.entries;
        self.order
            .retain(|(path, stamp)| entries.get(path).is_some_and(|slot| slot.stamp == *stamp));
    }

    #[cfg(test)]
    fn order_len(&self) -> usize {
        self.order.len()
    }

    #[cfg(test)]
    fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn info(key: &str, size: usize) -> ObjectInfo {
        ObjectInfo {
            key: key.to_string(),
            size: size as i64,
            content_type: "text/plain".to_string(),
            last_modified: Utc::now(),
        }
    }

    fn put(cache: &mut ReadCache, folder: &str, key: &str, len: usize) {
        cache.insert(folder, key, &vec![7u8; len], &info(key, len));
    }

    fn held(cache: &mut ReadCache, folder: &str, key: &str) -> bool {
        cache.get(folder, key).is_some()
    }

    #[test]
    fn a_hit_returns_the_bytes_and_info_inserted() {
        let mut cache = ReadCache::new();
        cache.insert("f", "k", b"hello", &info("k", 5));
        let hit = cache.get("f", "k").expect("cached");
        assert_eq!(&*hit.data, b"hello");
        assert_eq!(hit.info.key, "k");
        assert_eq!(hit.info.size, 5);
        assert!(cache.get("f", "other").is_none());
    }

    #[test]
    fn inserting_past_the_budget_evicts_the_least_recently_used() {
        let mut cache = ReadCache::with_bounds(30, 20);
        put(&mut cache, "f", "a", 10);
        put(&mut cache, "f", "b", 10);
        put(&mut cache, "f", "c", 10);
        // `a` is now the most recently used, so `b` is the eviction victim.
        assert!(held(&mut cache, "f", "a"));
        put(&mut cache, "f", "d", 10);

        assert!(!held(&mut cache, "f", "b"), "the LRU entry must go");
        assert!(held(&mut cache, "f", "a"), "the MRU entry must stay");
        assert!(held(&mut cache, "f", "c"));
        assert!(held(&mut cache, "f", "d"));
        assert_eq!(cache.bytes(), 30);
    }

    #[test]
    fn one_insert_evicts_as_many_entries_as_the_budget_needs() {
        let mut cache = ReadCache::with_bounds(30, 30);
        put(&mut cache, "f", "a", 10);
        put(&mut cache, "f", "b", 10);
        put(&mut cache, "f", "c", 10);
        put(&mut cache, "f", "big", 25);

        assert!(!held(&mut cache, "f", "a"));
        assert!(!held(&mut cache, "f", "b"));
        assert!(!held(&mut cache, "f", "c"));
        assert!(held(&mut cache, "f", "big"));
        assert_eq!(cache.bytes(), 25);
    }

    #[test]
    fn an_object_over_the_entry_cap_is_not_held() {
        let mut cache = ReadCache::with_bounds(100, 10);
        put(&mut cache, "f", "small", 10);
        put(&mut cache, "f", "big", 11);
        assert!(!held(&mut cache, "f", "big"));
        assert!(
            held(&mut cache, "f", "small"),
            "a skipped insert evicts nothing"
        );
        assert_eq!(cache.bytes(), 10);
    }

    #[test]
    fn an_over_size_rewrite_forgets_the_old_bytes() {
        let mut cache = ReadCache::with_bounds(100, 10);
        put(&mut cache, "f", "k", 5);
        put(&mut cache, "f", "k", 11);
        assert!(
            !held(&mut cache, "f", "k"),
            "the previous bytes are stale now"
        );
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn production_bounds_are_sixteen_mib_and_one_mib() {
        assert_eq!(BUDGET_BYTES, 16 * 1024 * 1024);
        assert_eq!(MAX_ENTRY_BYTES, 1024 * 1024);
        let mut cache = ReadCache::new();
        put(&mut cache, "f", "at-cap", MAX_ENTRY_BYTES);
        put(&mut cache, "f", "over-cap", MAX_ENTRY_BYTES + 1);
        assert!(held(&mut cache, "f", "at-cap"));
        assert!(!held(&mut cache, "f", "over-cap"));
    }

    #[test]
    fn replacing_an_entry_accounts_its_bytes_once() {
        let mut cache = ReadCache::with_bounds(100, 100);
        put(&mut cache, "f", "k", 40);
        put(&mut cache, "f", "k", 30);
        assert_eq!(cache.bytes(), 30);
        assert_eq!(cache.get("f", "k").map(|c| c.data.len()), Some(30));
    }

    #[test]
    fn remove_forgets_one_key() {
        let mut cache = ReadCache::new();
        put(&mut cache, "f", "a", 3);
        put(&mut cache, "f", "b", 3);
        cache.remove("f", "a");
        assert!(!held(&mut cache, "f", "a"));
        assert!(held(&mut cache, "f", "b"));
        assert_eq!(cache.bytes(), 3);
    }

    #[test]
    fn remove_folder_forgets_nested_keys_and_sub_folders_only() {
        let mut cache = ReadCache::new();
        put(&mut cache, "site", "index.html", 3);
        put(&mut cache, "site", "blog/post.html", 3);
        put(&mut cache, "site/assets", "app.css", 3);
        put(&mut cache, "site2", "index.html", 3);
        put(&mut cache, "sit", "e", 3);

        cache.remove_folder("site");

        assert!(!held(&mut cache, "site", "index.html"));
        assert!(!held(&mut cache, "site", "blog/post.html"));
        assert!(!held(&mut cache, "site/assets", "app.css"));
        assert!(
            held(&mut cache, "site2", "index.html"),
            "a sibling sharing a prefix stays"
        );
        assert!(held(&mut cache, "sit", "e"));
        assert_eq!(cache.bytes(), 6);
    }

    /// OPFS stores `("a", "b/c")` and `("a/b", "c")` as one file, so a write
    /// through either name must forget what a read through the other cached.
    #[test]
    fn two_names_for_one_file_are_one_entry() {
        let mut cache = ReadCache::new();
        put(&mut cache, "a", "b/c", 3);
        cache.remove("a/b", "c");
        assert!(!held(&mut cache, "a", "b/c"));
    }

    #[test]
    fn a_read_that_overlapped_a_write_is_not_cached() {
        let mut cache = ReadCache::new();
        let read = cache.ticket();
        // A delete starts and finishes while the read awaits OPFS.
        cache.invalidate("f", "k");
        cache.invalidate("f", "k");
        cache.insert_read(read, "f", "k", b"old", &info("k", 3));
        assert!(
            !held(&mut cache, "f", "k"),
            "the read may carry deleted bytes"
        );

        let fresh = cache.ticket();
        cache.insert_read(fresh, "f", "k", b"new", &info("k", 3));
        assert!(held(&mut cache, "f", "k"));
    }

    #[test]
    fn a_read_that_overlapped_a_folder_delete_is_not_cached() {
        let mut cache = ReadCache::new();
        let read = cache.ticket();
        cache.invalidate_folder("f");
        cache.insert_read(read, "f", "k", b"old", &info("k", 3));
        assert!(!held(&mut cache, "f", "k"));
    }

    #[test]
    fn a_read_that_started_inside_a_put_is_not_cached_after_it() {
        let mut cache = ReadCache::new();
        let write = cache.invalidate("f", "k");
        // A read starts while the put awaits OPFS and may fetch the old bytes.
        let read = cache.ticket();
        cache.insert_written(write, "f", "k", b"new", &info("k", 3));
        cache.insert_read(read, "f", "k", b"old", &info("k", 3));
        assert_eq!(
            cache.get("f", "k").map(|c| c.data.to_vec()),
            Some(b"new".to_vec())
        );
    }

    #[test]
    fn overlapping_writes_leave_the_key_uncached() {
        let mut cache = ReadCache::new();
        let first = cache.invalidate("f", "k");
        let second = cache.invalidate("f", "k");
        cache.insert_written(first, "f", "k", b"one", &info("k", 3));
        cache.insert_written(second, "f", "k", b"two", &info("k", 3));
        assert!(
            !held(&mut cache, "f", "k"),
            "which write's bytes OPFS kept is not known here"
        );
    }

    #[test]
    fn an_undisturbed_write_is_cached() {
        let mut cache = ReadCache::new();
        let write = cache.invalidate("f", "k");
        cache.insert_written(write, "f", "k", b"one", &info("k", 3));
        assert!(held(&mut cache, "f", "k"));
    }

    #[test]
    fn repeated_hits_do_not_grow_the_recency_queue() {
        let mut cache = ReadCache::new();
        put(&mut cache, "f", "a", 1);
        put(&mut cache, "f", "b", 1);
        for _ in 0..10_000 {
            assert!(held(&mut cache, "f", "a"));
            assert!(held(&mut cache, "f", "b"));
        }
        assert!(cache.order_len() <= 2 * 2 + 16, "{}", cache.order_len());
    }

    #[test]
    fn repeated_removals_do_not_grow_the_recency_queue() {
        let mut cache = ReadCache::new();
        for i in 0..10_000 {
            put(&mut cache, "f", &format!("k{i}"), 1);
            cache.remove("f", &format!("k{i}"));
        }
        assert!(cache.order_len() <= 16, "{}", cache.order_len());
    }
}
