//! Per-isolate runtime cache. Builds the Wafer once per isolate (sealed, no
//! boot funnel — migrations/seeds happen at deploy via `/_deploy/init`),
//! stores it in a thread_local, and rebuilds when the KV config-version
//! stamp moves. Mirrors impresspress-browser/src/runtime.rs's thread_local
//! pattern; `Rc` handles (not raw pointers) keep an in-flight request's
//! runtime alive across a swap. wasm32 is single-threaded, so the RefCell
//! borrows are never contended — but they are still never held across an
//! `.await` (interleaved fetch events resume at await points).
//!
//! Concurrent first requests may race to build; last store wins. A build
//! is pure CPU plus one KV-cached block_settings read, so a duplicate
//! build is wasteful-but-correct and only possible in the first instants
//! of an isolate's life. (YAGNI: no async build-guard until measurement
//! says otherwise.)

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use impresspress_core::cache_key::CONFIG_VERSION_KEY;
use wafer_core::interfaces::database::service::DatabaseService;

/// Floor of the isolate-local warm-hit probe window (ms) — see
/// [`next_probe_deadline_ms`].
const PROBE_INTERVAL_FLOOR_MS: u64 = 30_000;
/// Width of the jitter added on top of the floor (ms) — see
/// [`next_probe_deadline_ms`].
const PROBE_INTERVAL_JITTER_MS: u64 = 30_000;

pub(crate) struct ReadyRuntime {
    pub wafer: wafer_run::Wafer,
    pub db: Arc<dyn DatabaseService>,
    /// KV backend this runtime was built with. Held so the config-version
    /// probe on the request hot path reuses it instead of constructing a fresh
    /// `KvStore` handle from `env` on every request.
    pub kv: Arc<dyn impresspress_core::kv::KvBackend>,
    pub version: String,
    /// Absolute wall-clock deadline (ms since epoch, `now_millis()`-scale)
    /// after which the next request in this isolate re-probes the KV
    /// config-version stamp instead of trusting this cached runtime
    /// outright. Reset to a fresh jittered window after every probe (hit
    /// or rebuild). See "Remove the KV read from nearly every warm
    /// request" — Cloudflare KV is already eventually consistent (changes
    /// can take 60s+ to propagate), so probing more often than this floor
    /// buys no real freshness.
    probe_deadline_ms: Cell<u64>,
}

thread_local! {
    static RUNTIME: RefCell<Option<Rc<ReadyRuntime>>> = const { RefCell::new(None) };
    /// Set by `KvCachedD1DatabaseService::bump_config_version` /
    /// `force_bump_config_version` (kv_cached_db.rs) immediately after a
    /// LOCAL write to a config-version-bumping table (variables /
    /// block_settings / wrap_grants) in THIS isolate. Forces the next
    /// `get_or_build` call to probe (and rebuild) regardless of the
    /// jittered deadline below — a request that just wrote new config must
    /// not keep serving the pre-write runtime for up to a minute just
    /// because the deadline hasn't elapsed yet. Consumed (cleared) by the
    /// next `get_or_build` call, whether or not that call ends up
    /// rebuilding.
    static DIRTY: Cell<bool> = const { Cell::new(false) };
}

/// Mark the per-isolate runtime dirty: the next [`get_or_build`] call in
/// this isolate probes the KV config-version stamp — and rebuilds
/// unconditionally, regardless of what that probe reads back — rather than
/// trusting the jittered deadline. See the `DIRTY` thread_local's doc.
pub(crate) fn mark_dirty() {
    DIRTY.with(|d| d.set(true));
}

/// Read and clear the dirty flag.
fn take_dirty() -> bool {
    DIRTY.with(|d| d.replace(false))
}

/// A fresh probe deadline: `now` plus a jittered 30-60s window. Jitter
/// avoids every isolate that warmed at the same instant re-probing KV in
/// lockstep after exactly the same interval.
fn next_probe_deadline_ms(now: u64) -> u64 {
    let mut buf = [0u8; 2];
    let jitter_ms = if getrandom::getrandom(&mut buf).is_ok() {
        u64::from(u16::from_le_bytes(buf)) % PROBE_INTERVAL_JITTER_MS
    } else {
        0
    };
    now + PROBE_INTERVAL_FLOOR_MS + jitter_ms
}

fn cached() -> Option<Rc<ReadyRuntime>> {
    RUNTIME.with(|r| r.borrow().clone())
}

fn store(rt: Rc<ReadyRuntime>) {
    RUNTIME.with(|r| *r.borrow_mut() = Some(rt));
}

/// Read-only peek at the currently-cached runtime, if any. Used by `run` to
/// drain queued request-log rows through the cached runtime's DB handle in a
/// `waitUntil` without forcing a build.
pub(crate) fn peek() -> Option<Rc<ReadyRuntime>> {
    cached()
}

/// Current KV config-version stamp. Missing key ⇒ stamp a fresh one so all
/// isolates converge on the same generation.
async fn current_version(kv: &Arc<dyn impresspress_core::kv::KvBackend>) -> String {
    match kv.get(CONFIG_VERSION_KEY).await {
        Ok(Some(v)) => v,
        _ => {
            let v = crate::kv_cached_db::new_version_stamp();
            if let Err(e) =
                impresspress_core::kv::put_version_stamp_with_retry(kv.as_ref(), &v).await
            {
                tracing::warn!(error = %e, "config-version stamp persist failed; runtime tagged with local stamp only (KV unstamped; re-mints until a put lands)");
            }
            v
        }
    }
}

/// Return the per-isolate cached runtime, rebuilding it if the KV
/// config-version stamp has moved (or if nothing is cached yet).
///
/// The `register_blocks` / `register_post_build` hooks are `FnOnce` and are
/// consumed only on the build path; on a cache hit they are dropped unused.
pub(crate) async fn get_or_build<F, G>(
    env: &worker::Env,
    register_blocks: F,
    register_post_build: G,
) -> Result<Rc<ReadyRuntime>, Box<dyn std::error::Error>>
where
    F: FnOnce(
        crate::ImpresspressBuilder,
    ) -> Result<crate::ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn wafer_core::interfaces::storage::service::StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    let now = impresspress_core::util::now_millis();

    // Every path probes the config-version BEFORE building, so the stored
    // ReadyRuntime is always tagged with a version no newer than the config
    // generation it was actually built from. Version stamps are monotonic,
    // so a pre-build probe is safe: worst case the stamp moves again between
    // the probe and the build, and the next request pays one harmless extra
    // rebuild. Probing AFTER the build would risk the opposite — stamping a
    // runtime built from stale config with a fresh version, which would
    // never self-heal until the next bump.
    //
    // Hit / mismatch path: probe through the CACHED runtime's own KV
    // backend — no fresh `KvStore` construction on the request hot path.
    //
    // The flag threaded alongside the version is `read_through`: version-
    // mismatch rebuilds bypass the KV row cache (the rebuild exists BECAUSE
    // config just changed, which is exactly when cross-PoP KV lag or a
    // failed row-invalidate would bake stale rows under the new stamp).
    // Cold builds keep the cache (cold-start latency is its remaining value).
    let (probed_version, read_through) = if let Some(rt) = cached() {
        let dirty = take_dirty();

        // Fast path: no local write since the runtime was built, and the
        // jittered probe window hasn't elapsed yet — skip the KV read
        // entirely. This is the case for nearly every warm request.
        if !dirty && now < rt.probe_deadline_ms.get() {
            return Ok(rt);
        }

        let version = current_version(&rt.kv).await;

        // A pure deadline-elapsed probe (not dirty) that finds the version
        // unchanged just extends the window — no rebuild needed. A LOCAL
        // write (`dirty`) always falls through to a rebuild below, even if
        // this same read happens to still report the old version: KV is
        // eventually consistent even for the writer, so trusting this read
        // alone would reintroduce the exact staleness window `mark_dirty`
        // exists to close.
        if !dirty && rt.version == version {
            rt.probe_deadline_ms.set(next_probe_deadline_ms(now));
            return Ok(rt);
        }
        tracing::info!(old = %rt.version, new = %version, dirty, "config version moved or local write pending; rebuilding runtime");
        (version, true)
    } else {
        // Cold isolate: nothing cached to probe through. Construct a
        // standalone KV backend (cold path only — the hit/mismatch branch
        // above never constructs one) and probe it now, before
        // `build_runtime` runs. `build_runtime` constructs its own KV
        // backend internally (`built.kv`, used for the D1 read-cache), but
        // that handle isn't available yet and isn't reused for this probe —
        // the whole point is to probe before the build starts. Either way
        // this is exactly one KV `get` for the version stamp.
        let kv = crate::make_kv_backend(env, crate::runner::KV_BINDING)?;
        (current_version(&kv).await, false)
    };

    let mut built = crate::build_runtime(
        env,
        register_blocks,
        register_post_build,
        false,
        crate::kv_cached_db::CacheMode {
            read_through,
            bump_on_write: true,
        },
    )
    .await?;

    // Dynamic WRAP grants must be registered before seal.
    crate::apply_db_wrap_grants(&mut built).await;

    built.wafer.seal().await.map_err(|e| format!("seal: {e}"))?;
    impresspress_core::builder::post_start(&built.wafer, &built.storage_block);

    let rt = Rc::new(ReadyRuntime {
        wafer: built.wafer,
        db: built.db,
        kv: built.kv,
        version: probed_version,
        probe_deadline_ms: Cell::new(next_probe_deadline_ms(now)),
    });
    store(rt.clone());
    Ok(rt)
}
