//! TTL-based per-instance cache.
//!
//! Used by impresspress-cloudflare to memoize D1 reads across requests within
//! a Worker isolate's lifetime. Generic so the unit tests live here on the
//! native target — the cloudflare crate has no `cargo test` surface.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub struct TtlCache<T> {
    state: Mutex<Option<(Arc<T>, Option<Instant>)>>,
    ttl: Duration,
}

impl<T> TtlCache<T> {
    pub const fn new(ttl: Duration) -> Self {
        Self {
            state: Mutex::new(None),
            ttl,
        }
    }

    /// Returns the cached value if fresh, otherwise runs `fetcher` and caches the result.
    ///
    /// `Instant::now()` and `Instant::elapsed()` panic on `wasm32-unknown-unknown`
    /// (no `time` syscall). Callers targeting wasm pass `Duration::MAX` as the TTL —
    /// the cache then never calls into `Instant`, on either the read or write side.
    pub async fn get_or_load<F, Fut>(&self, fetcher: F) -> Arc<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        // Mutex poisoning here means some _other_ fetcher panicked while
        // holding the lock; the cached value itself is unchanged and still
        // safe to reuse. Recover via `into_inner()` instead of panicking —
        // a single fetcher panic shouldn't permanently brick the isolate's
        // cache.
        if let Some((value, loaded_at)) = &*self.state.lock().unwrap_or_else(|e| e.into_inner()) {
            if self.ttl == Duration::MAX || loaded_at.is_some_and(|t| t.elapsed() < self.ttl) {
                return Arc::clone(value);
            }
        }
        let fresh = fetcher().await;
        let arc = Arc::new(fresh);
        let loaded_at = if self.ttl == Duration::MAX {
            None
        } else {
            Some(Instant::now())
        };
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some((Arc::clone(&arc), loaded_at));
        arc
    }

    /// Force the next call to refetch. Used by admin-write paths.
    pub fn invalidate(&self) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test]
    async fn first_call_runs_fetcher() {
        let cache: TtlCache<u32> = TtlCache::new(Duration::from_secs(60));
        let calls = AtomicUsize::new(0);
        let v = cache
            .get_or_load(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                42
            })
            .await;
        assert_eq!(*v, 42);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn second_call_within_ttl_does_not_refetch() {
        let cache: TtlCache<u32> = TtlCache::new(Duration::from_secs(60));
        let calls = AtomicUsize::new(0);
        let _ = cache
            .get_or_load(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                7
            })
            .await;
        let v = cache
            .get_or_load(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                99
            })
            .await;
        assert_eq!(*v, 7, "should return cached value, not new fetcher result");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "fetcher should not run twice"
        );
    }

    #[tokio::test]
    async fn refetch_after_ttl_expires() {
        let cache: TtlCache<u32> = TtlCache::new(Duration::from_millis(10));
        let calls = AtomicUsize::new(0);

        let _ = cache
            .get_or_load(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                1
            })
            .await;

        tokio::time::sleep(Duration::from_millis(50)).await;

        let v = cache
            .get_or_load(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                2
            })
            .await;

        assert_eq!(*v, 2, "should refetch after TTL");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn invalidate_forces_refetch() {
        let cache: TtlCache<u32> = TtlCache::new(Duration::from_secs(60));
        let calls = AtomicUsize::new(0);

        let _ = cache
            .get_or_load(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                1
            })
            .await;

        cache.invalidate();

        let v = cache
            .get_or_load(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                2
            })
            .await;

        assert_eq!(*v, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn duration_max_ttl_never_expires_and_returns_cached() {
        let cache: TtlCache<u32> = TtlCache::new(Duration::MAX);
        let calls = AtomicUsize::new(0);

        let v1 = cache
            .get_or_load(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                5
            })
            .await;

        // Even after a sleep that would exceed any reasonable finite TTL,
        // Duration::MAX means we should still hit the cache without ever
        // calling `loaded_at.elapsed()`.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let v2 = cache
            .get_or_load(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                999
            })
            .await;

        assert_eq!(*v2, 5);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            Arc::ptr_eq(&v1, &v2),
            "same Arc should be returned for cache hits"
        );
    }
}
