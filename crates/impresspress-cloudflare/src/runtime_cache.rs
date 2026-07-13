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

use std::{cell::RefCell, rc::Rc, sync::Arc};

use impresspress_core::cache_key::CONFIG_VERSION_KEY;
use wafer_core::interfaces::database::service::DatabaseService;

pub(crate) struct ReadyRuntime {
    pub wafer: wafer_run::Wafer,
    pub db: Arc<dyn DatabaseService>,
    /// KV backend this runtime was built with. Held so the config-version
    /// probe on the request hot path reuses it instead of constructing a fresh
    /// `KvStore` handle from `env` on every request.
    pub kv: Arc<dyn impresspress_core::kv::KvBackend>,
    pub version: String,
}

thread_local! {
    static RUNTIME: RefCell<Option<Rc<ReadyRuntime>>> = const { RefCell::new(None) };
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
            if let Err(e) = impresspress_core::kv::put_version_stamp_with_retry(kv.as_ref(), &v).await {
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
    F: FnOnce(crate::ImpresspressBuilder) -> Result<crate::ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn wafer_core::interfaces::storage::service::StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
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
        let version = current_version(&rt.kv).await;
        if rt.version == version {
            return Ok(rt);
        }
        tracing::info!(old = %rt.version, new = %version, "config version moved; rebuilding runtime");
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
    });
    store(rt.clone());
    Ok(rt)
}
