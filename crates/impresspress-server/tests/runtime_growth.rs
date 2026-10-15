//! Building, booting and dropping runtimes one after another does not grow
//! the heap.
//!
//! `runtime_drop.rs` proves the auth block is freed with its runtime. This
//! checks the whole heap instead, so it also catches a leak that bypasses
//! that block: a `mem::forget`, a `Box::leak`, a static cache keyed per
//! runtime. Each iteration builds and boots the runtime the server binary
//! does (`build_native_runtime` + `boot_native`) over a fresh SQLite file,
//! then drops it. Live heap bytes are read after each drop. The runtime
//! carries every block `impresspress-core` enables by default — this crate's
//! dev-dependency turns them on, whatever else `cargo` builds alongside —
//! and is about 20 MB of live heap once booted.
//!
//! The baseline is the reading after the FIRST iteration, not before it: the
//! first build fills one-time state that is never freed by design (tracing
//! callsites, `LazyLock` statics, the first SQLite open, the tokio blocking
//! pool), and a later build reuses it. With the stored `Init` context put
//! back into `AuthServiceImpl`, every iteration retains its whole runtime:
//! this test then reads about 19.8 MB more live heap per cycle, linearly.
//!
//! What it does not see: memory the bundled SQLite allocates from the C
//! heap (the counter wraps only Rust's global allocator), and anything a
//! served request would retain — it builds and boots, and serves nothing.
//!
//! A `#[global_allocator]` is per binary, which is why this is a test file of
//! its own.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::HashMap,
    sync::atomic::{AtomicIsize, Ordering},
};

use impresspress_native::{InfraConfig, ListenerEnv};
use impresspress_server::{boot_native, build_native_runtime, AppHooks};

/// The system allocator, counting the bytes it has handed out and not had
/// back.
struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);

// SAFETY: every method forwards to `System` with the same arguments and only
// adds bookkeeping on the side, so `System`'s guarantees carry over.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds the contract.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; the caller upholds the contract.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds the contract.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Builds and boots the native runtime over a fresh SQLite file in `dir`,
/// then drops it.
async fn build_boot_drop(dir: &std::path::Path) {
    let storage_root = dir.join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let infra = InfraConfig {
        listen: "127.0.0.1:0".to_string(),
        db_type: "sqlite".to_string(),
        db_path: dir
            .join("growth.sqlite3")
            .to_str()
            .expect("UTF-8 path")
            .to_string(),
        db_url: None,
        storage_type: "local".to_string(),
        storage_root: storage_root.to_str().expect("UTF-8 path").to_string(),
        model_cache_dir: "data/models".to_string(),
        listener: ListenerEnv::default(),
    };
    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    let mut wafer = build_native_runtime(
        &infra,
        database,
        &HashMap::new(),
        Default::default(),
        AppHooks::none(),
    )
    .await
    .expect("build the native runtime");
    let report = boot_native(&mut wafer)
        .await
        .expect("boot the native runtime");
    assert!(report.ok, "every block initializes: {report:?}");
    drop(wafer);
}

/// Iterations after the baseline one.
const ITERATIONS: usize = 8;

/// Live bytes the last reading may exceed the baseline by. Measured at
/// 0 bytes after the first build (one transient reading of about 1 KB in
/// three runs), while one leaked runtime is about 19.8 MB here; the slack
/// only absorbs allocator noise in long-lived pools.
const SLACK_BYTES: isize = 64 * 1024;

#[tokio::test]
async fn building_and_dropping_runtimes_does_not_grow_the_heap() {
    let mut readings = Vec::with_capacity(ITERATIONS + 1);
    for _ in 0..=ITERATIONS {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_boot_drop(tmp.path()).await;
        drop(tmp);
        readings.push(LIVE.load(Ordering::SeqCst));
    }
    let baseline = readings[0];
    let last = readings[ITERATIONS];
    let growth: Vec<isize> = readings.iter().map(|r| r - baseline).collect();
    assert!(
        last - baseline <= SLACK_BYTES,
        "live heap grew by {} bytes over {ITERATIONS} build/boot/drop cycles after the \
         first (slack {SLACK_BYTES}); growth per reading: {growth:?}",
        last - baseline,
    );
}
