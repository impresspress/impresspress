//! Per-isolate runtime cache. Builds the Wafer once per isolate (sealed, no
//! boot funnel — migrations/seeds happen at deploy via `/_deploy/init`),
//! stores it in a thread_local, and rebuilds when the KV config-version
//! stamp moves. Mirrors impresspress-browser/src/runtime.rs's thread_local
//! pattern; `Rc` handles (not raw pointers) keep an in-flight request's
//! runtime alive across a swap. Every cell here is a `Cell`/`IsolateCell`,
//! never a `RefCell`: wasm32 is single-threaded so a borrow would never be
//! *contended*, but Cloudflare can hard-stop a request without running its
//! destructors, and a borrow flag stranded that way wedges the isolate for
//! good (see the `thread_local!` block's comment).
//!
//! Runtime construction is single-flight per isolate. A prepared-plan request
//! that arrives while another request is building first re-checks the cache
//! after its own KV read, then hydrates a request-local runtime if the owner is
//! still busy. It deliberately does not await a shared future: the build
//! performs D1/KV I/O, and a future created by one Workers request must not be
//! polled on behalf of another request.

use std::{
    cell::Cell,
    rc::{Rc, Weak},
    sync::Arc,
};

use impresspress_core::{cache_key::CONFIG_VERSION_KEY, metrics::CacheOutcome, IsolateCell};

/// Floor of the isolate-local warm-hit probe window (ms) — see
/// [`next_probe_deadline_ms`]. The default; a Worker can set its own with
/// [`PROBE_INTERVAL_VAR`].
const PROBE_INTERVAL_FLOOR_MS: u64 = 30_000;
/// Width of the jitter added on top of the floor (ms) — see
/// [`next_probe_deadline_ms`].
const PROBE_INTERVAL_JITTER_MS: u64 = 30_000;

/// Worker var: seconds between config-version probes per isolate (clamped to
/// 30..=3600). Each probe is one KV read, and on Workers Free KV reads are
/// capped at 100k/day account-wide, so a site that rarely edits config can
/// trade freshness for reads. An admin edit is still seen at once by the
/// isolate that made it (`mark_dirty`); other isolates see it within one
/// interval. Unset keeps the 30-60s default.
pub(crate) const PROBE_INTERVAL_VAR: &str = impresspress_core::config_vars::CONFIG_PROBE_INTERVAL_KEY;

/// The probe window this isolate uses, as (floor, jitter) in ms. Set from the
/// Worker's env on each request entry ([`configure_probe_interval`]); env vars
/// are fixed per deployment, so this only ever moves on a new version.
static PROBE_FLOOR_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(PROBE_INTERVAL_FLOOR_MS);
static PROBE_JITTER_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(PROBE_INTERVAL_JITTER_MS);

/// (floor, jitter) in ms for a [`PROBE_INTERVAL_VAR`] value. None or an
/// unparsable value keeps the default; a set value gets 10% jitter.
fn probe_window_ms(value: Option<&str>) -> (u64, u64) {
    match impresspress_core::config_vars::config_probe_interval_secs(value) {
        Some(secs) => {
            let floor = secs * 1000;
            (floor, floor / 10)
        }
        None => (PROBE_INTERVAL_FLOOR_MS, PROBE_INTERVAL_JITTER_MS),
    }
}

fn configure_probe_interval(env: &worker::Env) {
    let value = env.var(PROBE_INTERVAL_VAR).ok().map(|v| v.to_string());
    let (floor, jitter) = probe_window_ms(value.as_deref());
    PROBE_FLOOR_MS.store(floor, std::sync::atomic::Ordering::Relaxed);
    PROBE_JITTER_MS.store(jitter.max(1), std::sync::atomic::Ordering::Relaxed);
}
/// Maximum time an isolate-local build slot may remain owned.
///
/// Normal Rust cancellation drops [`BuildGuard`], but Cloudflare can hard-stop
/// a request after it exceeds its CPU allowance. That termination does not
/// guarantee Rust destructors run, so a plain boolean can remain set forever.
/// A later request reclaims the slot after this lease and forces a rebuild.
const BUILD_LEASE_MS: u64 = 5_000;
/// Absolute build-slot lease, even when a hard-stopped request leaves its
/// strong liveness token in the isolate heap. Healthy prepared builds finish
/// well inside this window; after it, availability is safer than trusting a
/// token whose owning request may no longer exist.
const BUILD_HARD_LEASE_MS: u64 = 10_000;

pub(crate) struct ReadyRuntime {
    pub wafer: wafer_run::Wafer,
    // Deliberately no D1/KV/R2/network/config handles here. The Wafer's six
    // service blocks contain stateless request-scoped proxies; concrete
    // services are selected by `request_services::scope` for each dispatch.
    pub version: String,
    /// Config-version observed when a packaged plan was hydrated. `None`
    /// identifies the ordinary dynamic runtime path, whose `version` already
    /// is the config-version itself.
    config_version: Option<String>,
    /// SHA-256 identity of the request-current Worker version and every Env
    /// value captured by runtime construction. Checked before the zero-await
    /// cache hit so binding/secret-only changes cannot retain stale services
    /// in a reused isolate.
    environment_identity: String,
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

// Every cell below is `Cell`/`IsolateCell`, never `RefCell`, and that is a
// requirement rather than a preference. `BUILD_LEASE_MS`'s doc already states
// the premise: Cloudflare can hard-stop a request without running its
// destructors. A `RefCell` borrow stranded by such a stop stays set for the
// life of the isolate, and — under this workspace's `panic = "abort"` wasm
// profile — every later request that touches the same cell traps inside
// `poll`, leaving a response promise that is never settled. Cloudflare reports
// that as "your Worker's code had hung and would never generate a response",
// at ~zero recorded CPU. `IsolateCell` has no borrow flag to strand; the worst
// an interrupted holder can do is drop the cached value, which is
// indistinguishable from a cold isolate and self-heals. See
// `impresspress_core::isolate_cell`.
thread_local! {
    static RUNTIME: IsolateCell<Rc<ReadyRuntime>> = const { IsolateCell::new() };
    /// Per-isolate cumulative count of runtime builds (cold + rebuild).
    /// Surfaced via `Server-Timing` (`CacheOutcome::build_ordinal`) as a
    /// zero-plumbing proxy for "D1 statements per logical request" — see
    /// `impresspress_core::metrics`'s module doc. A plain `Cell<u32>`
    /// increment; costs nothing on the far more common hit/probed-fresh
    /// paths, which never touch it.
    static BUILD_COUNT: Cell<u32> = const { Cell::new(0) };
    /// True while one request in this isolate is probing/rebuilding the
    /// runtime. Workers can interleave fetch events at `.await` points even
    /// though wasm32 is single-threaded, so a plain thread-local `Cell` is the
    /// correct atomicity boundary: check-and-set contains no await/yield.
    static BUILDING: Cell<bool> = const { Cell::new(false) };
    /// Wall-clock acquisition time for [`BUILDING`]. This makes the slot
    /// recoverable when Cloudflare hard-terminates its owning request before
    /// [`BuildGuard::drop`] can clear it.
    static BUILD_STARTED_MS: Cell<u64> = const { Cell::new(0) };
    /// Monotonic ownership token for the build slot. A reclaimed builder gets
    /// a new token so the expired builder's eventual `Drop` or completion
    /// cannot clear or overwrite its successor (the classic ABA race).
    static BUILD_OWNER: Cell<u64> = const { Cell::new(0) };
    static NEXT_BUILD_OWNER: Cell<u64> = const { Cell::new(0) };
    /// Weak liveness token for the request future that owns the slot. A
    /// legitimately suspended build retains the strong token in its guard,
    /// so elapsed wall time alone can never reclaim it. Cancellation drops
    /// the token; if a platform interruption leaves the scalar slot behind,
    /// a later request may reclaim it after the lease grace period.
    static BUILD_LIVENESS: IsolateCell<Weak<()>> = const { IsolateCell::new() };
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
    /// Once mutable admin state changes, a packaged plan is no longer an
    /// authoritative description of this deployment. Keep that exact
    /// plan/environment pair on the dynamic path for the isolate; a new
    /// Worker version or plan hash gets a fresh chance to use its new plan.
    static PREPARED_BYPASS: IsolateCell<String> = const { IsolateCell::new() };
}

/// RAII ownership of the isolate's runtime-build slot. Dropping the future on
/// an error/cancellation also drops this guard, allowing the next request to
/// retry instead of leaving the isolate permanently wedged.
struct BuildGuard {
    owner: u64,
    _liveness: Rc<()>,
}

/// A cold request arrived while another request in the same isolate owns the
/// runtime-build slot. Callers turn this into a short, retryable 503 rather
/// than awaiting request-owned work or leaving a response future pending.
#[derive(Debug)]
pub(crate) struct RuntimeBuildBusy;

impl std::fmt::Display for RuntimeBuildBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("runtime build already in progress")
    }
}

impl std::error::Error for RuntimeBuildBusy {}

impl BuildGuard {
    fn try_acquire(now: u64) -> Option<Self> {
        let owner_alive = owner_liveness_token_alive();
        let stale = BUILDING.with(|building| {
            building.get()
                && BUILD_STARTED_MS.with(|started| {
                    let acquired_at = started.get();
                    acquired_at == 0
                        || now.saturating_sub(acquired_at) >= BUILD_HARD_LEASE_MS
                        || (!owner_alive && now.saturating_sub(acquired_at) >= BUILD_LEASE_MS)
                })
        });
        if stale {
            tracing::warn!(
                lease_ms = BUILD_LEASE_MS,
                "reclaiming stale runtime-build slot after an interrupted builder"
            );
            BUILDING.with(|building| building.set(false));
            BUILD_STARTED_MS.with(|started| started.set(0));
            BUILD_OWNER.with(|owner| owner.set(0));
            BUILD_LIVENESS.with(IsolateCell::clear);
            // If a previous runtime exists, its dirty flag may already have
            // been consumed by the interrupted builder. Force the recovered
            // owner to rebuild instead of accepting that old runtime.
            if cached().is_some() {
                DIRTY.with(|dirty| dirty.set(true));
            }
        }

        BUILDING.with(|building| {
            if building.get() {
                return None;
            }
            building.set(true);
            BUILD_STARTED_MS.with(|started| started.set(now));
            let owner = NEXT_BUILD_OWNER.with(|next| {
                let owner = next.get().wrapping_add(1).max(1);
                next.set(owner);
                owner
            });
            BUILD_OWNER.with(|current| current.set(owner));
            let liveness = Rc::new(());
            BUILD_LIVENESS.with(|token| token.set(Rc::downgrade(&liveness)));
            Some(Self {
                owner,
                _liveness: liveness,
            })
        })
    }

    /// True while this guard still owns the slot. Lease age only permits a
    /// competing request to reclaim; it does not invalidate a long-running
    /// build until such a reclaim actually assigns a new owner token.
    fn is_current(&self) -> bool {
        BUILDING.with(Cell::get) && BUILD_OWNER.with(Cell::get) == self.owner
    }
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        // An expired owner may resume after another request reclaimed the
        // slot. Only the current token is allowed to release ownership.
        if BUILD_OWNER.with(Cell::get) == self.owner {
            BUILDING.with(|building| building.set(false));
            BUILD_STARTED_MS.with(|started| started.set(0));
            BUILD_OWNER.with(|owner| owner.set(0));
            BUILD_LIVENESS.with(IsolateCell::clear);
        }
    }
}

/// True while the request that owns the build slot still holds its strong
/// liveness token.
///
/// A cleared cell reads as "no live owner", which is also the state a
/// platform hard-stop inside [`IsolateCell`]'s critical section leaves. That
/// is the safe direction: it lets the lease reclaim the slot rather than
/// protecting a builder that may no longer exist.
fn owner_liveness_token_alive() -> bool {
    BUILD_LIVENESS.with(|token| token.get().is_some_and(|weak| weak.upgrade().is_some()))
}

fn build_slot_active(now: u64) -> bool {
    BUILDING.with(|building| {
        building.get()
            && BUILD_STARTED_MS.with(|started| {
                let acquired_at = started.get();
                acquired_at != 0
                    && now.saturating_sub(acquired_at) < BUILD_HARD_LEASE_MS
                    && (owner_liveness_token_alive()
                        || now.saturating_sub(acquired_at) < BUILD_LEASE_MS)
            })
    })
}

/// A request must not await a future or timer whose progress depends on a
/// different Workers request. While a healthy builder owns the slot, warm
/// requests can safely use the last complete runtime; cold requests receive a
/// retryable [`RuntimeBuildBusy`]. A stale slot falls through and is reclaimed
/// by [`BuildGuard::try_acquire`].
fn runtime_while_building(
    now: u64,
    environment_identity: &str,
) -> Result<Option<Rc<ReadyRuntime>>, RuntimeBuildBusy> {
    if !build_slot_active(now) {
        return Ok(None);
    }
    if let Some(rt) = cached() {
        if rt.environment_identity == environment_identity && rt.config_version.is_none() {
            return Ok(Some(rt));
        }
    }
    Err(RuntimeBuildBusy)
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

fn prepared_cache_identity(plan_hash: &str, environment_identity: &str) -> String {
    format!("{plan_hash}\n{environment_identity}")
}

/// Return true only for the exact plan/environment pair previously bypassed.
/// Encountering a new identity clears stale isolate-local bypass state.
fn prepared_is_bypassed(identity: &str) -> bool {
    // `retain_if_eq` is where the clear-on-mismatch semantics live, and it is
    // held by tests that actually run — this crate is wasm32-only, so its own
    // unit tests are compiled but never executed.
    PREPARED_BYPASS.with(|slot| slot.retain_if_eq(identity))
}

fn bypass_prepared(identity: String) {
    PREPARED_BYPASS.with(|slot| slot.set(identity));
}

/// Did the config change? An UNREADABLE version is not a change.
///
/// [`current_version`] returns [`VERSION_UNAVAILABLE`] on a KV read error,
/// meaning to hold the current generation. Comparing it like a real stamp did
/// the opposite: it never matches, so every probe during a KV failure (on
/// Workers Free that includes the daily 100k-read cap running out) abandoned
/// the runtime for the multi-second dynamic build, which cannot fit the CPU
/// budget. One KV quota day took the whole site down (2026-11-05/06).
fn config_version_changed(cached: &str, observed: &str) -> bool {
    observed != VERSION_UNAVAILABLE && cached != observed
}

fn prepared_probe_requires_fallback(
    dirty: bool,
    cached_config_version: &str,
    observed_config_version: &str,
) -> bool {
    dirty || config_version_changed(cached_config_version, observed_config_version)
}

/// Is the packaged plan's generation current? With KV unreadable there is
/// nothing newer to compare against, so a BOUND plan is trusted (it was
/// current when deployed) rather than bypassed for the dynamic build.
fn prepared_generation_matches(plan_generation: &str, observed_generation: &str) -> bool {
    plan_generation != impresspress_core::UNBOUND_CONFIG_GENERATION
        && (observed_generation == VERSION_UNAVAILABLE || plan_generation == observed_generation)
}

/// A fresh probe deadline: `now` plus a jittered window (30-60s by default,
/// see [`PROBE_INTERVAL_VAR`]). Jitter avoids every isolate that warmed at the
/// same instant re-probing KV in lockstep after exactly the same interval.
fn next_probe_deadline_ms(now: u64) -> u64 {
    let floor = PROBE_FLOOR_MS.load(std::sync::atomic::Ordering::Relaxed);
    let jitter_width = PROBE_JITTER_MS.load(std::sync::atomic::Ordering::Relaxed).max(1);
    let mut buf = [0u8; 4];
    let jitter_ms = if getrandom::getrandom(&mut buf).is_ok() {
        u64::from(u32::from_le_bytes(buf)) % jitter_width
    } else {
        0
    };
    now + floor + jitter_ms
}

fn cached() -> Option<Rc<ReadyRuntime>> {
    RUNTIME.with(IsolateCell::get)
}

fn store(rt: Rc<ReadyRuntime>) {
    // `IsolateCell::set` installs the new runtime BEFORE dropping the old
    // one. That matters here: the displaced `Rc<ReadyRuntime>` may be the
    // last handle to an entire Wafer, so its destructor is long enough to be
    // interrupted — and `*slot.borrow_mut() = Some(rt)` would have run that
    // destructor with the borrow held.
    RUNTIME.with(|r| r.set(rt));
}

fn store_if_current(guard: &BuildGuard, rt: Rc<ReadyRuntime>) -> bool {
    if !guard.is_current() {
        return false;
    }
    store(rt);
    true
}

/// Returned when the config-version key cannot be READ.
///
/// A constant, deliberately: every isolate that hits a KV error converges on
/// the same value, so a KV outage costs at most one rebuild per isolate rather
/// than one per probe.
const VERSION_UNAVAILABLE: &str = "<kv-unavailable>";

/// Current KV config-version stamp. Missing key ⇒ stamp a fresh one so all
/// isolates converge on the same generation.
///
/// A read ERROR is NOT a missing key. Minting on error — which is what `_`
/// used to do — hands back a fresh random stamp, and the caller compares it
/// against `rt.version` to decide whether to rebuild. They never match, so one
/// transient KV failure forced EVERY isolate onto the multi-second dynamic
/// build path, on every probe, until a `put` finally landed. The doc comment
/// above described the intended behaviour; the match arm was wider than the
/// comment.
async fn current_version(kv: &Arc<dyn impresspress_core::kv::KvBackend>) -> String {
    match kv.get(CONFIG_VERSION_KEY).await {
        Ok(Some(v)) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "config-version read failed; holding the current generation rather than re-minting"
            );
            VERSION_UNAVAILABLE.to_string()
        }
        Ok(None) => {
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
/// config-version stamp has moved (or if nothing is cached yet), alongside
/// the [`CacheOutcome`] this call resolved to — a free byproduct of the
/// branches below, consumed by `lib.rs::run` to build the `Server-Timing`
/// response header.
///
/// The `register_blocks` / `register_post_build` hooks are `FnOnce` and are
/// consumed only on the build path; on a cache hit they are dropped unused.
pub(crate) async fn get_or_build<F, G>(
    env: &worker::Env,
    request_config: &std::collections::HashMap<String, String>,
    register_blocks: F,
    register_post_build: G,
) -> Result<(Rc<ReadyRuntime>, CacheOutcome), Box<dyn std::error::Error>>
where
    F: FnOnce(
        crate::ImpresspressBuilder,
    ) -> Result<crate::ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn wafer_core::interfaces::storage::service::StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    configure_probe_interval(env);
    // The `IMPRESSPRESS_PREPARED_PLAN_DISABLED` escape hatch that used to sit
    // here is GONE, and should not come back. Its own comment said to remove it
    // "once the hang is understood"; it now is.
    //
    // It existed on the theory that possession of a packaged plan caused the
    // 2026-07-31 cold-isolate failures. Deploying with it set disproved that:
    // the deploy pipeline's concurrency gate rejected 121/160 requests, because
    // a cold DYNAMIC build ran up to 8.4s while concurrent cold requests were
    // refused in ~26ms. Serving those requests instead then hit the real
    // ceiling — Cloudflare error 1102, per-request CPU exhausted, because a full
    // dynamic build does not fit in a request's CPU budget alongside its own
    // work. Prepared hydration is ~132us. That gap is why the plan exists.
    if let Some(plan) = crate::packaged_prepared_runtime_plan(env)? {
        let environment_identity = crate::runtime_environment_identity(env, request_config);
        let prepared_identity = prepared_cache_identity(&plan.plan_hash, &environment_identity);
        if !prepared_is_bypassed(&prepared_identity) {
            return get_or_build_prepared(
                env,
                request_config,
                plan,
                register_blocks,
                register_post_build,
            )
            .await;
        }
    }

    let environment_identity = crate::runtime_environment_identity(env, request_config);

    // Hooks are FnOnce because only the request that acquires the build slot
    // consumes them. Waiters retain their own hooks while sleeping, then drop
    // them unused when the completed runtime is visible.
    let mut register_blocks = Some(register_blocks);
    let mut register_post_build = Some(register_post_build);

    let (probed_version, read_through, is_cold, built_at, build_guard) = loop {
        let now = impresspress_core::util::now_millis();

        // Preserve the zero-await warm path. A dirty or probe-due runtime
        // falls through to the build slot so only one request probes KV and,
        // if needed, rebuilds.
        // Do not take the ordinary hit while any slot is marked owned. A
        // healthy owner is handled as stale-while-revalidate below; an
        // expired owner must reach `try_acquire` so it can be reclaimed and
        // force a rebuild even if the interrupted builder consumed DIRTY.
        if !BUILDING.with(Cell::get) {
            if let Some(rt) = cached() {
                let dirty = DIRTY.with(Cell::get);
                if !dirty
                    && rt.config_version.is_none()
                    && rt.environment_identity == environment_identity
                    && now < rt.probe_deadline_ms.get()
                {
                    return Ok((rt, CacheOutcome::Hit));
                }
            }
        }

        match runtime_while_building(now, &environment_identity) {
            // A healthy owner is rebuilding and the last complete runtime is
            // still serviceable: stale-while-revalidate.
            Ok(Some(rt)) => return Ok((rt, CacheOutcome::Hit)),
            Ok(None) => {}
            // The slot is owned and this request has nothing usable cached —
            // the cold case. Serve it from a runtime of its own instead of
            // refusing it with a 503; it cannot await the owner's build.
            Err(RuntimeBuildBusy) => {
                return hydrate_transient_dynamic_runtime(
                    env,
                    request_config,
                    register_blocks
                        .take()
                        .expect("build hooks are consumed by at most one build attempt"),
                    register_post_build
                        .take()
                        .expect("build hooks are consumed by at most one build attempt"),
                    environment_identity.clone(),
                    now,
                )
                .await;
            }
        }

        // Another request took the slot between the check above and here. Same
        // reasoning: build request-locally rather than refuse.
        let Some(build_guard) = BuildGuard::try_acquire(now) else {
            return hydrate_transient_dynamic_runtime(
                env,
                request_config,
                register_blocks
                    .take()
                    .expect("build hooks are consumed by at most one build attempt"),
                register_post_build
                    .take()
                    .expect("build hooks are consumed by at most one build attempt"),
                environment_identity.clone(),
                now,
            )
            .await;
        };

        // Re-check under ownership of the slot. Another request may have
        // completed a build while this one was waiting on its timer.
        let resolution = if let Some(rt) = cached() {
            let dirty = take_dirty();
            let environment_changed = rt.environment_identity != environment_identity;

            if !dirty
                && rt.config_version.is_none()
                && !environment_changed
                && now < rt.probe_deadline_ms.get()
            {
                return Ok((rt, CacheOutcome::Hit));
            }

            // Always derive the probe handle from THIS request's Env. The
            // immutable cached runtime intentionally retains no KV binding.
            let probe_kv = crate::make_kv_backend(env, crate::runner::KV_BINDING)?;
            let version = current_version(&probe_kv).await;

            // A pure deadline-elapsed probe (not dirty) that finds the
            // version unchanged just extends the window — no rebuild needed.
            // A LOCAL write (`dirty`) always rebuilds even if KV still reports
            // the old version because KV is eventually consistent.
            if !dirty
                && rt.config_version.is_none()
                && !environment_changed
                && !config_version_changed(&rt.version, &version)
            {
                rt.probe_deadline_ms.set(next_probe_deadline_ms(now));
                return Ok((rt, CacheOutcome::ProbedFresh));
            }
            tracing::info!(old = %rt.version, new = %version, dirty, environment_changed, "config version, Worker environment, or local state changed; rebuilding runtime");
            (version, true, false, now, build_guard)
        } else {
            // Cold isolate: probe before build so the finished runtime is
            // tagged with a version no newer than the config it loaded.
            let kv = crate::make_kv_backend(env, crate::runner::KV_BINDING)?;
            (current_version(&kv).await, false, true, now, build_guard)
        };
        break resolution;
    };

    let mut built = crate::build_runtime(
        env,
        request_config,
        None,
        register_blocks
            .take()
            .expect("build hooks are consumed by at most one build attempt"),
        register_post_build
            .take()
            .expect("build hooks are consumed by at most one build attempt"),
        false,
        crate::kv_cached_db::CacheMode {
            read_through,
            bump_on_write: true,
        },
    )
    .await?;

    // Dynamic WRAP grants must be registered before seal. Strictly initialize
    // every slot under the build owner's concrete services before publishing
    // the Wafer: Workers requests must never wait on another request's shared
    // lazy-init mutex/future. The concrete services are dropped instead of
    // entering ReadyRuntime.
    crate::request_services::scope(built.services.clone(), async {
        crate::apply_db_wrap_grants(&mut built).await;
        built.wafer.seal().await.map_err(|e| format!("seal: {e}"))?;
        impresspress_core::builder::strict_init_all_blocks(&built.wafer)
            .await
            .map_err(|error| format!("strict cached-runtime Init: {error}"))
    })
    .await?;
    crate::request_services::scope_sync(built.services.clone(), || {
        impresspress_core::builder::post_start(&built.wafer, &built.storage_block);
    });

    let build_ordinal = BUILD_COUNT.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        n
    });
    let duration_ms = impresspress_core::util::now_millis().saturating_sub(built_at);

    let rt = Rc::new(ReadyRuntime {
        wafer: built.wafer,
        version: probed_version,
        config_version: None,
        environment_identity,
        probe_deadline_ms: Cell::new(next_probe_deadline_ms(built_at)),
    });
    if !store_if_current(&build_guard, rt.clone()) {
        // The runtime is complete and internally consistent; only the right to
        // PUBLISH it was lost, because the guard expired or a newer owner
        // superseded it. Discarding it to return a 503 throws away a build this
        // request already paid for in full — and on Workers that build is
        // charged against the per-request CPU budget, so it is the expensive
        // part. Serve it request-locally; the winning owner's runtime becomes
        // the cached one.
        tracing::warn!(
            build_ordinal,
            "serving un-storable runtime request-locally; a newer owner superseded this build"
        );
    }
    tracing::info!(
        build_ordinal,
        duration_ms,
        cold = is_cold,
        "runtime build complete"
    );
    // `build_guard` remains alive through `store`, so waiters cannot observe
    // BUILDING=false before the completed runtime is visible.
    let outcome = if is_cold {
        CacheOutcome::ColdBuilt {
            build_ordinal,
            duration_ms,
        }
    } else {
        CacheOutcome::Rebuilt {
            build_ordinal,
            duration_ms,
        }
    };
    Ok((rt, outcome))
}

/// Hydrate a packaged immutable plan without D1 settings or WRAP-grant reads.
/// One KV config-version read tags the cold hydration, followed by the same
/// bounded 30–60s probes used by dynamic runtimes. A local dirty signal or a
/// moved version permanently bypasses this plan/environment pair in the
/// isolate and restores dynamic hydration, preserving admin mutation
/// semantics without putting D1 structural reads back on the prepared cold
/// path.
async fn hydrate_prepared_runtime<F, G>(
    env: &worker::Env,
    request_config: &std::collections::HashMap<String, String>,
    plan: &impresspress_core::PreparedRuntimePlan,
    register_blocks: F,
    register_post_build: G,
    environment_identity: String,
    started_at: u64,
) -> Result<(Rc<ReadyRuntime>, u32, u64), Box<dyn std::error::Error>>
where
    F: FnOnce(
        crate::ImpresspressBuilder,
    ) -> Result<crate::ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn wafer_core::interfaces::storage::service::StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    let mut built = crate::build_runtime(
        env,
        request_config,
        Some(plan),
        register_blocks,
        register_post_build,
        false,
        crate::kv_cached_db::CacheMode::default(),
    )
    .await?;

    // Grants and settings were imported from the verified plan. Seal and
    // strictly initialize every slot under this request's services before the
    // Wafer can be dispatched or published. ConfigSource may still perform
    // per-block reads; keeping them here prevents cross-request lazy-init
    // waiters.
    crate::request_services::scope(built.services.clone(), async {
        built.wafer.seal().await.map_err(|e| format!("seal: {e}"))?;
        impresspress_core::builder::strict_init_all_blocks(&built.wafer)
            .await
            .map_err(|error| format!("strict prepared-runtime Init: {error}"))
    })
    .await?;
    crate::request_services::scope_sync(built.services.clone(), || {
        impresspress_core::builder::post_start(&built.wafer, &built.storage_block);
    });

    let build_ordinal = BUILD_COUNT.with(|count| {
        let next = count.get() + 1;
        count.set(next);
        next
    });
    let duration_ms = impresspress_core::util::now_millis().saturating_sub(started_at);
    let rt = Rc::new(ReadyRuntime {
        wafer: built.wafer,
        version: plan.plan_hash.clone(),
        config_version: Some(plan.config_generation.clone()),
        environment_identity,
        probe_deadline_ms: Cell::new(next_probe_deadline_ms(started_at)),
    });
    Ok((rt, build_ordinal, duration_ms))
}

/// Build a complete runtime for THIS request only, without touching the build
/// slot or replacing the isolate cache.
///
/// A cold request must not wait for the owner's build: Workers I/O futures are
/// bound to the request that created them, so awaiting another request's future
/// is not a valid execution model on this platform (see
/// [`runtime_while_building`]). The prepared path has always resolved that by
/// hydrating a request-scoped runtime; the dynamic path had no equivalent and
/// refused the request with [`RuntimeBuildBusy`] → HTTP 503 instead.
///
/// That asymmetry is what made a plan-disabled Worker unshippable. Measured
/// 2026-08-01 against a candidate preview under the deploy pipeline's own
/// 160-request / 32-in-flight gate: the slot owner's build ran up to **8.4 s**
/// while every concurrent cold request was refused in **~26 ms** — 121/160
/// rejected. The gate refused promotion, correctly.
///
/// It matters beyond the plan-disabled case: a deploy bumps the config
/// generation, which makes `get_or_build_prepared` bypass the now-stale plan and
/// route every isolate down this same dynamic path, exactly while post-deploy
/// verification is probing.
///
/// Cost is one extra build for the width of the owner's build window — the same
/// price the prepared path already pays. The KV read before it gives the owner a
/// chance to finish first, and the cache is re-checked so a request that arrives
/// late pays nothing.
///
/// WATCH THE SUBREQUEST CEILING. A refused request used to cost ~nothing; one
/// served this way pays a full dynamic build AND its page render on the same
/// request, against Cloudflare's ~50-subrequest limit. A dynamic build is
/// heavier than prepared hydration precisely because it does the D1 structural
/// reads the plan exists to avoid. Post-change measurements put `/destinations/`
/// at 33 subrequests and `/search` at 39, so the headroom is real but not large.
/// Exceeding it surfaces as `Too many subrequests`, NOT as a hang or a 503 —
/// distinct enough to diagnose from the deploy gate's output.
async fn hydrate_transient_dynamic_runtime<F, G>(
    env: &worker::Env,
    request_config: &std::collections::HashMap<String, String>,
    register_blocks: F,
    register_post_build: G,
    environment_identity: String,
    started_at: u64,
) -> Result<(Rc<ReadyRuntime>, CacheOutcome), Box<dyn std::error::Error>>
where
    F: FnOnce(
        crate::ImpresspressBuilder,
    ) -> Result<crate::ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn wafer_core::interfaces::storage::service::StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    // Read KV with THIS request's binding — never the owner's — then re-check
    // the cache before paying for a build the owner may have just finished.
    let kv = crate::make_kv_backend(env, crate::runner::KV_BINDING)?;
    let probed_version = current_version(&kv).await;
    if let Some(rt) = cached() {
        if rt.environment_identity == environment_identity
            && rt.config_version.is_none()
            && rt.version == probed_version
        {
            return Ok((rt, CacheOutcome::Hit));
        }
    }

    let mut built = crate::build_runtime(
        env,
        request_config,
        None,
        register_blocks,
        register_post_build,
        false,
        crate::kv_cached_db::CacheMode {
            // Read through the KV row cache. This runtime is tagged with the
            // version just probed above, and one of the ways this path is
            // reached is a config-generation bump — precisely the
            // version-mismatch case where a stale row cache must not be baked
            // into a runtime carrying the new stamp. Cost is near-neutral: it
            // replaces a KV row-cache read with a D1 query rather than adding
            // one, which matters given the subrequest ceiling noted above.
            read_through: true,
            bump_on_write: true,
        },
    )
    .await?;

    // Same ordering the stored dynamic build uses: WRAP grants before seal, then
    // strictly initialize every slot so no request can wait on another's
    // lazy-init future.
    crate::request_services::scope(built.services.clone(), async {
        crate::apply_db_wrap_grants(&mut built).await;
        built.wafer.seal().await.map_err(|e| format!("seal: {e}"))?;
        impresspress_core::builder::strict_init_all_blocks(&built.wafer)
            .await
            .map_err(|error| format!("strict transient-runtime Init: {error}"))
    })
    .await?;
    crate::request_services::scope_sync(built.services.clone(), || {
        impresspress_core::builder::post_start(&built.wafer, &built.storage_block);
    });

    let build_ordinal = BUILD_COUNT.with(|count| {
        let next = count.get() + 1;
        count.set(next);
        next
    });
    let duration_ms = impresspress_core::util::now_millis().saturating_sub(started_at);
    let rt = Rc::new(ReadyRuntime {
        wafer: built.wafer,
        version: probed_version,
        config_version: None,
        environment_identity,
        probe_deadline_ms: Cell::new(next_probe_deadline_ms(started_at)),
    });
    tracing::info!(
        build_ordinal,
        duration_ms,
        transient = true,
        "request-local dynamic runtime build complete"
    );
    Ok((
        rt,
        CacheOutcome::ColdBuilt {
            build_ordinal,
            duration_ms,
        },
    ))
}

async fn get_or_build_prepared<F, G>(
    env: &worker::Env,
    request_config: &std::collections::HashMap<String, String>,
    plan: Rc<impresspress_core::PreparedRuntimePlan>,
    register_blocks: F,
    register_post_build: G,
) -> Result<(Rc<ReadyRuntime>, CacheOutcome), Box<dyn std::error::Error>>
where
    F: FnOnce(
        crate::ImpresspressBuilder,
    ) -> Result<crate::ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn wafer_core::interfaces::storage::service::StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    configure_probe_interval(env);
    let environment_identity = crate::runtime_environment_identity(env, request_config);
    let plan_generation = plan.plan_hash.clone();
    let prepared_identity = prepared_cache_identity(&plan_generation, &environment_identity);
    let now = impresspress_core::util::now_millis();

    if !BUILDING.with(Cell::get) {
        if let Some(rt) = cached() {
            let dirty = DIRTY.with(Cell::get);
            if !dirty
                && rt.environment_identity == environment_identity
                && rt.version == plan_generation
                && rt.config_version.is_some()
                && now < rt.probe_deadline_ms.get()
            {
                return Ok((rt, CacheOutcome::Hit));
            }
        }
    }

    if build_slot_active(now) {
        if let Some(rt) = cached() {
            if rt.environment_identity == environment_identity
                && rt.version == plan_generation
                && rt.config_version.is_some()
            {
                return Ok((rt, CacheOutcome::Hit));
            }
        }

        // Do not await the owner request: Workers I/O futures are bound to the
        // request that created them. Reading KV with this request gives the
        // owner a chance to finish; then re-check the cache before paying for
        // a transient prepared runtime. A transient runtime is complete and
        // request-scoped, so it can serve safely without touching the owner's
        // build slot or replacing the isolate cache.
        let kv = crate::make_kv_backend(env, crate::runner::KV_BINDING)?;
        let config_version = current_version(&kv).await;
        if !prepared_generation_matches(&plan.config_generation, &config_version) {
            // The plan is stale AND the slot is busy — the post-deploy window,
            // where a config-generation bump invalidates the packaged plan for
            // every isolate at once. The non-busy path below reacts by bypassing
            // the plan and rebuilding dynamically; do the same request-locally
            // rather than refusing. Isolate-wide bypass state is left to the
            // owner, which reaches the same check once it holds the slot.
            return hydrate_transient_dynamic_runtime(
                env,
                request_config,
                register_blocks,
                register_post_build,
                environment_identity,
                now,
            )
            .await;
        }
        if let Some(rt) = cached() {
            if rt.environment_identity == environment_identity
                && rt.version == plan_generation
                && rt.config_version.as_deref() == Some(config_version.as_str())
            {
                return Ok((rt, CacheOutcome::Hit));
            }
        }

        let (rt, build_ordinal, duration_ms) = hydrate_prepared_runtime(
            env,
            request_config,
            plan.as_ref(),
            register_blocks,
            register_post_build,
            environment_identity,
            now,
        )
        .await?;
        tracing::info!(
            build_ordinal,
            duration_ms,
            prepared = true,
            transient = true,
            "request-local prepared runtime hydration complete"
        );
        return Ok((
            rt,
            CacheOutcome::ColdBuilt {
                build_ordinal,
                duration_ms,
            },
        ));
    }

    // Losing this race must not cost the request a 503. The plan is verified and
    // already in hand, so hydrate it request-locally — the same escape hatch the
    // busy branch above takes. Prepared hydration is ~132 µs rather than a
    // multi-second dynamic build, so unlike the dynamic path's fallback it
    // carries no per-request CPU (Cloudflare 1102) risk.
    //
    // PR #88 gave the DYNAMIC path this fallback and left this copy untouched.
    // That omission is the most likely source of the `/gdsf/admin/geo/tree` 503
    // that rolled back the pre-#80 with-plan deploy.
    let Some(build_guard) = BuildGuard::try_acquire(now) else {
        let (rt, build_ordinal, duration_ms) = hydrate_prepared_runtime(
            env,
            request_config,
            plan.as_ref(),
            register_blocks,
            register_post_build,
            environment_identity,
            now,
        )
        .await?;
        tracing::info!(
            build_ordinal,
            duration_ms,
            prepared = true,
            transient = true,
            "request-local prepared hydration complete (lost the build-slot race)"
        );
        return Ok((
            rt,
            CacheOutcome::ColdBuilt {
                build_ordinal,
                duration_ms,
            },
        ));
    };
    if let Some(rt) = cached() {
        if rt.environment_identity == environment_identity && rt.version == plan_generation {
            if let Some(cached_config_version) = rt.config_version.as_deref() {
                let dirty = take_dirty();
                if !dirty && now < rt.probe_deadline_ms.get() {
                    return Ok((rt, CacheOutcome::Hit));
                }

                let probe_kv = crate::make_kv_backend(env, crate::runner::KV_BINDING)?;
                let observed_config_version = current_version(&probe_kv).await;
                if !prepared_probe_requires_fallback(
                    dirty,
                    cached_config_version,
                    &observed_config_version,
                ) {
                    rt.probe_deadline_ms.set(next_probe_deadline_ms(now));
                    return Ok((rt, CacheOutcome::ProbedFresh));
                }

                tracing::info!(
                    plan_hash = %plan_generation,
                    old_config_version = %cached_config_version,
                    new_config_version = %observed_config_version,
                    dirty,
                    "mutable admin state changed; bypassing packaged plan for this isolate"
                );
                bypass_prepared(prepared_identity);
                drop(build_guard);
                // Force the dynamic path to replace the currently cached prepared
                // runtime even when an eventually-consistent KV read still
                // returns the old stamp after a local write.
                mark_dirty();
                return Box::pin(get_or_build(
                    env,
                    request_config,
                    register_blocks,
                    register_post_build,
                ))
                .await;
            }
        }
    }
    let is_cold = cached().is_none();
    // A plan/Worker identity change supersedes dirty state belonging to the
    // prior runtime. The new plan is tagged with the current KV generation.
    let _ = take_dirty();
    let kv = crate::make_kv_backend(env, crate::runner::KV_BINDING)?;
    let config_version = current_version(&kv).await;
    if !prepared_generation_matches(&plan.config_generation, &config_version) {
        tracing::info!(
            plan_hash = %plan_generation,
            plan_config_generation = %plan.config_generation,
            observed_config_generation = %config_version,
            "prepared plan generation is stale; bypassing it for this isolate"
        );
        bypass_prepared(prepared_identity);
        drop(build_guard);
        return Box::pin(get_or_build(
            env,
            request_config,
            register_blocks,
            register_post_build,
        ))
        .await;
    }

    let (rt, build_ordinal, duration_ms) = hydrate_prepared_runtime(
        env,
        request_config,
        plan.as_ref(),
        register_blocks,
        register_post_build,
        environment_identity,
        now,
    )
    .await?;
    if !store_if_current(&build_guard, rt.clone()) {
        // Same reasoning as the dynamic path: the hydrated runtime is complete,
        // only the right to publish it was lost. Serve it request-locally rather
        // than converting a superseded owner into a user-visible 503.
        tracing::warn!(
            build_ordinal,
            prepared = true,
            "serving un-storable runtime request-locally; a newer owner superseded this build"
        );
    }
    tracing::info!(
        build_ordinal,
        duration_ms,
        cold = is_cold,
        prepared = true,
        "prepared runtime hydration complete"
    );
    let outcome = if is_cold {
        CacheOutcome::ColdBuilt {
            build_ordinal,
            duration_ms,
        }
    } else {
        CacheOutcome::Rebuilt {
            build_ordinal,
            duration_ms,
        }
    };
    Ok((rt, outcome))
}

#[cfg(test)]
mod tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    fn reset_build_slot() {
        BUILDING.with(|value| value.set(false));
        BUILD_STARTED_MS.with(|value| value.set(0));
        BUILD_OWNER.with(|value| value.set(0));
        NEXT_BUILD_OWNER.with(|value| value.set(0));
        BUILD_LIVENESS.with(IsolateCell::clear);
        DIRTY.with(|value| value.set(false));
    }

    /// The NEGATIVE side of the build slot, which nothing else asserted.
    ///
    /// Every existing slot test checks that `build_slot_active` is TRUE while a
    /// guard is held. None checked that it is FALSE otherwise, so
    /// `build_slot_active` could be replaced with `|_| true` and the whole
    /// module stayed green — verified by doing exactly that. An always-active
    /// slot means every cold request is told a build is in flight, which is the
    /// difference between serving a request and refusing it.
    #[wasm_bindgen_test]
    fn build_slot_is_inactive_until_a_guard_is_acquired_and_after_it_drops() {
        reset_build_slot();
        assert!(
            !build_slot_active(100),
            "no builder owns the slot, so nothing should be reported active"
        );
        let owner = BuildGuard::try_acquire(100).unwrap();
        assert!(build_slot_active(100), "the acquired guard owns the slot");
        drop(owner);
        assert!(
            !build_slot_active(100),
            "the slot must clear when its owner drops, or every later cold \
             request is refused forever"
        );
    }

    /// `runtime_while_building` is the branch that decides whether a cold
    /// request is served or refused, and both directions matter.
    ///
    /// `Ok(None)` means "no build in flight, go acquire the slot yourself".
    /// `Err(RuntimeBuildBusy)` means "another request owns the build and you
    /// have nothing cached to fall back on" — on Workers a request cannot await
    /// another request's future, so this is a real fork, not an optimisation.
    #[wasm_bindgen_test]
    fn cold_request_is_told_to_back_off_only_while_a_build_is_active() {
        reset_build_slot();
        assert!(
            matches!(runtime_while_building(100, "env-a"), Ok(None)),
            "with no builder the caller must be free to build"
        );

        let owner = BuildGuard::try_acquire(100).unwrap();
        assert!(
            runtime_while_building(100, "env-a").is_err(),
            "a cold request cannot wait on another request's in-flight build"
        );

        drop(owner);
        assert!(
            matches!(runtime_while_building(100, "env-a"), Ok(None)),
            "once the builder is gone the next caller must be free again"
        );
    }

    #[wasm_bindgen_test]
    fn expired_owner_cannot_clear_reclaimer_slot_or_store() {
        reset_build_slot();
        let owner_a = BuildGuard::try_acquire(100).unwrap();
        // Simulate a platform interruption that orphaned scalar slot state,
        // while retaining the old guard to exercise an eventual late resume.
        BUILD_LIVENESS.with(IsolateCell::clear);
        let owner_b = BuildGuard::try_acquire(100 + BUILD_LEASE_MS).unwrap();

        assert!(!owner_a.is_current());
        assert!(owner_b.is_current());
        drop(owner_a);
        assert!(BUILDING.with(Cell::get));
        assert!(owner_b.is_current());

        drop(owner_b);
        assert!(!BUILDING.with(Cell::get));
    }

    #[wasm_bindgen_test]
    fn live_build_is_protected_until_absolute_lease_then_reclaimed() {
        reset_build_slot();
        let owner = BuildGuard::try_acquire(100).unwrap();
        // The soft threshold may pass while the live request token protects
        // its owner. The absolute lease still recovers a token stranded by a
        // platform hard stop, whose destructor never ran.
        assert!(owner.is_current());
        assert!(build_slot_active(100 + BUILD_LEASE_MS - 1));
        assert!(build_slot_active(100 + BUILD_LEASE_MS));
        assert!(BuildGuard::try_acquire(100 + BUILD_LEASE_MS).is_none());
        assert!(BuildGuard::try_acquire(100 + BUILD_HARD_LEASE_MS - 1).is_none());
        let recovered = BuildGuard::try_acquire(100 + BUILD_HARD_LEASE_MS).unwrap();
        assert!(!owner.is_current());
        assert!(recovered.is_current());
        drop(owner);
        assert!(BUILDING.with(Cell::get));
        drop(recovered);
        assert!(!BUILDING.with(Cell::get));
    }

    #[wasm_bindgen_test]
    fn orphaned_slot_waits_for_grace_then_recovers() {
        reset_build_slot();
        BUILDING.with(|value| value.set(true));
        BUILD_STARTED_MS.with(|value| value.set(100));
        BUILD_OWNER.with(|value| value.set(7));
        BUILD_LIVENESS.with(IsolateCell::clear);

        assert!(BuildGuard::try_acquire(100 + BUILD_LEASE_MS - 1).is_none());
        let recovered = BuildGuard::try_acquire(100 + BUILD_LEASE_MS).unwrap();
        assert!(recovered.is_current());
    }

    #[wasm_bindgen_test]
    fn local_dirty_forces_prepared_fallback_even_before_kv_converges() {
        assert!(prepared_probe_requires_fallback(true, "v1", "v1"));
        assert!(!prepared_probe_requires_fallback(false, "v1", "v1"));
    }

    #[wasm_bindgen_test]
    fn moved_config_version_forces_prepared_fallback() {
        assert!(prepared_probe_requires_fallback(false, "v1", "v2"));
    }

    #[wasm_bindgen_test]
    fn unreadable_config_version_holds_the_current_runtime() {
        // A KV read error (on Workers Free, the daily read cap) is not a
        // config change: hold the runtime instead of falling back to the
        // dynamic build on every probe.
        assert!(!config_version_changed("v1", VERSION_UNAVAILABLE));
        assert!(!prepared_probe_requires_fallback(false, "v1", VERSION_UNAVAILABLE));
        // A local write still forces the rebuild.
        assert!(prepared_probe_requires_fallback(true, "v1", VERSION_UNAVAILABLE));
        // A bound plan is trusted when KV cannot say otherwise; an unbound
        // one never is.
        let v1 = "1".repeat(32);
        assert!(prepared_generation_matches(&v1, VERSION_UNAVAILABLE));
        assert!(!prepared_generation_matches(
            impresspress_core::UNBOUND_CONFIG_GENERATION,
            VERSION_UNAVAILABLE,
        ));
    }

    #[wasm_bindgen_test]
    fn probe_window_defaults_and_clamps() {
        assert_eq!(probe_window_ms(None), (PROBE_INTERVAL_FLOOR_MS, PROBE_INTERVAL_JITTER_MS));
        assert_eq!(probe_window_ms(Some("not a number")), (PROBE_INTERVAL_FLOOR_MS, PROBE_INTERVAL_JITTER_MS));
        assert_eq!(probe_window_ms(Some("600")), (600_000, 60_000));
        assert_eq!(probe_window_ms(Some(" 600 ")), (600_000, 60_000));
        assert_eq!(probe_window_ms(Some("5")), (30_000, 3_000));
        assert_eq!(probe_window_ms(Some("999999")), (3_600_000, 360_000));
    }

    #[wasm_bindgen_test]
    fn second_isolate_rejects_v1_plan_after_generation_moves_to_v2() {
        let v1 = "1".repeat(32);
        let v2 = "2".repeat(32);
        // Isolate A started while the candidate's generation was current.
        assert!(prepared_generation_matches(&v1, &v1));
        // A later admin/deploy mutation moves KV. A fresh isolate must not
        // hydrate the older packaged v1 structure.
        assert!(!prepared_generation_matches(&v1, &v2));
        // The replacement Worker plan is accepted by another fresh isolate.
        assert!(prepared_generation_matches(&v2, &v2));
        assert!(!prepared_generation_matches(
            impresspress_core::UNBOUND_CONFIG_GENERATION,
            impresspress_core::UNBOUND_CONFIG_GENERATION,
        ));
    }

    #[wasm_bindgen_test]
    fn bypass_is_scoped_to_exact_plan_and_environment_identity() {
        PREPARED_BYPASS.with(IsolateCell::clear);
        let old = prepared_cache_identity("plan-a", "worker-a");
        bypass_prepared(old.clone());
        assert!(prepared_is_bypassed(&old));

        let replacement = prepared_cache_identity("plan-b", "worker-a");
        assert!(!prepared_is_bypassed(&replacement));
        assert!(!prepared_is_bypassed(&old));
    }

    #[wasm_bindgen_test]
    fn environment_change_produces_a_distinct_prepared_cache_identity() {
        let original = prepared_cache_identity("plan-a", "worker-env-v1");
        let changed = prepared_cache_identity("plan-a", "worker-env-v2");
        assert_ne!(original, changed);

        PREPARED_BYPASS.with(IsolateCell::clear);
        bypass_prepared(original.clone());
        assert!(prepared_is_bypassed(&original));
        assert!(!prepared_is_bypassed(&changed));
    }
}
