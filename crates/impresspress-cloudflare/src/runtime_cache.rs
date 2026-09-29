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
/// [`next_probe_deadline_ms`].
///
/// 5 minutes, raised from 30s after the 2026-08-31 KV read-quota exhaustion:
/// at a ~45s average, ~52 continuously active isolates burned the entire
/// 100k/day free-tier read allowance on version probes alone. KV is
/// eventually consistent (60s+ propagation), so the old cadence bought no
/// real freshness. Admin config changes still reach the mutating isolate
/// instantly via the dirty flag; other warm isolates converge within this
/// window.
const PROBE_INTERVAL_FLOOR_MS: u64 = 300_000;
/// Width of the jitter added on top of the floor (ms) — see
/// [`next_probe_deadline_ms`]. Probes land 5–10 minutes apart.
const PROBE_INTERVAL_JITTER_MS: u64 = 300_000;
// The probe window is the 2026-08-31 read-quota fix: ~45s average per isolate
// burned ~1,920 reads/day/isolate against a 100k/day allowance. Pin the window
// to the reviewed 5–10 minute range as a property, not an exact value.
//
// A `const` assertion rather than the `#[wasm_bindgen_test]` this used to be:
// both operands are compile-time, so the check costs nothing at runtime and
// fails every build that breaks it — including the plain `cargo check` and
// `cargo clippy` CI steps, which compile this crate but run no test at all.
// The wasm test needed a `wasm-bindgen-test-runner`, so only the diff-gated
// `cloudflare-wasm-test` job ever evaluated it.
const _: () = assert!(PROBE_INTERVAL_FLOOR_MS >= 300_000);
const _: () = assert!(PROBE_INTERVAL_FLOOR_MS + PROBE_INTERVAL_JITTER_MS <= 600_000);
/// Ceiling on the widened probe window after consecutive probe FAILURES —
/// see [`probe_failure_window_ms`]. Retrying an exhausted daily allowance at
/// full cadence provides no freshness and just manufactures failed
/// operations.
///
/// SCOPE: this backs off the WARM-isolate probe only, which is the read
/// source that actually dominated the 2026-08-31 exhaustion (~52 continuously
/// active isolates x ~1,920 probes/day). Cold, transient and busy-slot
/// hydrations still probe once each, deliberately: a request with no usable
/// cached runtime has to resolve a version somehow, and their volume is
/// bounded by isolate churn rather than by wall-clock cadence. The counter
/// lives on [`ReadyRuntime`], so a rebuild restarts the streak, which is
/// conservative — it can only shorten the window, never extend it past what
/// a fresh runtime would use.
const PROBE_FAILURE_BACKOFF_CAP_MS: u64 = 1_800_000;
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
    /// Consecutive failed version probes. Widens the next probe window
    /// (see [`probe_failure_window_ms`]); reset to zero by any successful
    /// probe.
    probe_failures: Cell<u32>,
}

impl ReadyRuntime {
    /// The `kind` field every runtime-lifecycle log line carries, derived from
    /// the runtime itself rather than typed at each site.
    ///
    /// All four sites that announce a finished runtime — the two that publish
    /// it as the isolate's cached one and the two that serve it request-locally
    /// — spell this field the same way, so one dashboard filter sees them all.
    /// Two of them used to emit `prepared = true` instead, which no filter
    /// matching `kind` could find. `config_version.is_some()` is already what
    /// the rest of this file switches on to tell the two paths apart.
    fn kind_label(&self) -> &'static str {
        if self.config_version.is_some() {
            "prepared"
        } else {
            "dynamic"
        }
    }

    /// A probe reached KV: reset the failure streak and re-arm the normal
    /// jittered window.
    fn note_probe_success(&self, now: u64) {
        self.probe_failures.set(0);
        self.probe_deadline_ms.set(next_probe_deadline_ms(now));
    }

    /// A probe failed (quota/transport): widen the next window with capped
    /// exponential backoff so an exhausted daily allowance is not re-polled
    /// at full cadence by every warm isolate.
    fn note_probe_failure(&self, now: u64) {
        let failures = self.probe_failures.get().saturating_add(1);
        self.probe_failures.set(failures);
        self.probe_deadline_ms.set(probe_failure_deadline_ms(
            now,
            failures,
            random_probe_jitter_ms(),
        ));
    }
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
    /// not keep serving the pre-write runtime for the rest of its 5–10 minute
    /// probe window (`PROBE_INTERVAL_FLOOR_MS` + `PROBE_INTERVAL_JITTER_MS`)
    /// just because the deadline hasn't elapsed yet. This covers the writing
    /// isolate only: every other warm isolate still converges on its own
    /// probe, up to that same window later. Consumed (cleared) by the
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

/// Whether a warm prepared runtime must abandon its packaged plan. A local
/// write (`dirty`) always falls back — D1 is the source of truth and the
/// rebuild reads it directly. An [`VersionProbe::Unavailable`] probe never
/// does: bypassing a valid plan over a transient read error converted one
/// failed GET into dynamic D1 rebuilds for the isolate's whole life.
fn prepared_probe_requires_fallback(
    dirty: bool,
    cached_config_version: &str,
    probe: &VersionProbe,
) -> bool {
    if dirty {
        return true;
    }
    match probe {
        VersionProbe::Unavailable => false,
        VersionProbe::Stamped(v) | VersionProbe::Minted(v) => cached_config_version != v,
    }
}

/// Whether the packaged plan's generation is current. When the probe is
/// [`VersionProbe::Unavailable`] the signed plan is trusted as-is: it is a
/// complete, verified snapshot, and manufacturing an isolate-local generation
/// to compare against it turned KV read errors into full dynamic builds. An
/// UNBOUND generation is never trusted, probe or no probe.
fn prepared_generation_matches(plan_generation: &str, probe: &VersionProbe) -> bool {
    if plan_generation == impresspress_core::UNBOUND_CONFIG_GENERATION {
        return false;
    }
    match probe {
        VersionProbe::Unavailable => true,
        VersionProbe::Stamped(v) | VersionProbe::Minted(v) => plan_generation == v,
    }
}

/// Whether a warm DYNAMIC runtime must rebuild after its probe window
/// elapsed. Mirrors [`prepared_probe_requires_fallback`]: dirty or a changed
/// environment always rebuilds; an unavailable probe keeps the last valid
/// runtime.
fn dynamic_probe_requires_rebuild(
    dirty: bool,
    environment_changed: bool,
    cached_version: &str,
    probe: &VersionProbe,
) -> bool {
    if dirty || environment_changed {
        return true;
    }
    match probe {
        VersionProbe::Unavailable => false,
        VersionProbe::Stamped(v) | VersionProbe::Minted(v) => cached_version != v,
    }
}

/// Whether a cold request that lost the build-slot race can serve the cached
/// runtime instead of building request-locally. With the probe unavailable,
/// freshness cannot be verified either way — serving the last known good
/// runtime beats paying for a dynamic build tagged with an unverifiable
/// stamp.
///
/// Callers that ALREADY probed pass that probe down (see
/// `hydrate_transient_dynamic_runtime`'s `known_probe`) rather than letting
/// this re-probe: a second probe that flakes to `Unavailable` would
/// otherwise discard the first probe's proof that the cached version is
/// stale and serve it as a hit.
fn transient_dynamic_can_serve_cached(cached_version: &str, probe: &VersionProbe) -> bool {
    match probe {
        VersionProbe::Unavailable => true,
        VersionProbe::Stamped(v) | VersionProbe::Minted(v) => cached_version == v,
    }
}

/// Whether a cached PREPARED runtime's config generation is still good for
/// this probe. `Unavailable` serves it: the caller has already trusted the
/// signed plan via [`prepared_generation_matches`], so re-hydrating the same
/// plan would burn a full initialization to reach the identical state. A
/// runtime with no `config_version` is dynamic, never a prepared cache hit.
fn prepared_cached_config_matches(
    cached_config_version: Option<&str>,
    probe: &VersionProbe,
) -> bool {
    let Some(cached) = cached_config_version else {
        return false;
    };
    match probe {
        VersionProbe::Unavailable => true,
        VersionProbe::Stamped(v) | VersionProbe::Minted(v) => cached == v,
    }
}

/// Jitter for one probe deadline, from 32 bits of randomness. Two random
/// bytes were enough for the old 30s width; against a 5-minute width a u16
/// (max 65,535ms) would silently cap the spread at ~65s and re-synchronize
/// isolates that warmed together.
fn probe_jitter_ms(raw: u32) -> u64 {
    u64::from(raw) % PROBE_INTERVAL_JITTER_MS
}

fn random_probe_jitter_ms() -> u64 {
    let mut buf = [0u8; 4];
    if getrandom::getrandom(&mut buf).is_ok() {
        probe_jitter_ms(u32::from_le_bytes(buf))
    } else {
        0
    }
}

/// A fresh probe deadline: `now` plus a jittered 5–10 minute window. Jitter
/// avoids every isolate that warmed at the same instant re-probing KV in
/// lockstep after exactly the same interval.
fn next_probe_deadline_ms(now: u64) -> u64 {
    now + PROBE_INTERVAL_FLOOR_MS + random_probe_jitter_ms()
}

/// Probe window after `consecutive_failures` failed probes: the normal floor
/// doubled per failure, capped at [`PROBE_FAILURE_BACKOFF_CAP_MS`].
fn probe_failure_window_ms(consecutive_failures: u32) -> u64 {
    PROBE_INTERVAL_FLOOR_MS
        .checked_shl(consecutive_failures.min(16))
        .unwrap_or(PROBE_FAILURE_BACKOFF_CAP_MS)
        .min(PROBE_FAILURE_BACKOFF_CAP_MS)
}

/// Absolute deadline for the next probe after `consecutive_failures`
/// failures. The cap bounds the DEADLINE, jitter included — clamping the
/// window and then adding jitter on top would overshoot
/// [`PROBE_FAILURE_BACKOFF_CAP_MS`] by the full jitter width.
fn probe_failure_deadline_ms(now: u64, consecutive_failures: u32, jitter_ms: u64) -> u64 {
    let window = probe_failure_window_ms(consecutive_failures)
        .saturating_add(jitter_ms)
        .min(PROBE_FAILURE_BACKOFF_CAP_MS);
    now + window
}

fn cached() -> Option<Rc<ReadyRuntime>> {
    RUNTIME.with(IsolateCell::get)
}

fn store(rt: Rc<ReadyRuntime>) {
    // A rebuild is this isolate's one signal that the world may have moved:
    // it follows a deploy, a migration run or any config-version bump
    // (`force_bump_config_version` marks every other isolate dirty). Schema
    // facts memoized from before it may describe a schema another isolate has
    // since migrated, so they go with the old runtime. Within an isolate the
    // shared `DbExec` paths invalidate per table as they mutate — see
    // `database::D1DatabaseService::schema_cache`.
    crate::database::forget_isolate_schema();
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

/// Outcome of one KV config-version probe. The three arms are deliberately
/// distinct: collapsing `Err` into the missing-key arm is what turned the
/// 2026-08-30/31 read-quota exhaustion into multi-thousand write-request
/// bursts (every failed probe minted a stamp and PUT it) and permanently
/// bypassed packaged plans on transient read errors.
#[derive(Debug, Clone)]
enum VersionProbe {
    /// KV holds a fleet-wide stamp.
    Stamped(String),
    /// The key was genuinely absent; a fresh stamp was minted and a PUT
    /// attempted so all isolates converge on the same generation.
    Minted(String),
    /// The GET itself failed (quota/transport). Nothing was minted or
    /// written; callers keep their last known good state.
    Unavailable,
}

impl VersionProbe {
    /// The observed stamp, for log lines.
    fn observed(&self) -> &str {
        match self {
            Self::Stamped(v) | Self::Minted(v) => v,
            Self::Unavailable => "<unavailable>",
        }
    }

    /// Version string for tagging a dynamic runtime that must be built right
    /// now. `Unavailable` mints an isolate-local stamp that is deliberately
    /// NOT persisted: it can never equal a later fleet-wide stamp, so the
    /// first successful probe after KV recovers forces one clean rebuild.
    fn into_dynamic_version(self) -> String {
        match self {
            Self::Stamped(v) | Self::Minted(v) => v,
            Self::Unavailable => {
                let v = crate::kv_cached_db::new_version_stamp();
                tracing::warn!(local_stamp = %v, "config-version unavailable; tagging dynamic runtime with an unpersisted local stamp");
                v
            }
        }
    }
}

/// Probe the KV config-version stamp. Missing key ⇒ stamp a fresh one so all
/// isolates converge on the same generation. A failed GET ⇒
/// [`VersionProbe::Unavailable`], with no mint and no write: a quota or
/// availability error must never be treated as a missing key.
async fn probe_version(kv: &Arc<dyn impresspress_core::kv::KvBackend>) -> VersionProbe {
    match kv.get(CONFIG_VERSION_KEY).await {
        Ok(Some(v)) => VersionProbe::Stamped(v),
        Ok(None) => {
            let v = crate::kv_cached_db::new_version_stamp();
            if let Err(e) =
                impresspress_core::kv::put_version_stamp_with_retry(kv.as_ref(), &v).await
            {
                tracing::warn!(error = %e, "config-version stamp persist failed; runtime tagged with local stamp only (KV unstamped; re-mints until a put lands)");
            }
            VersionProbe::Minted(v)
        }
        Err(e) => {
            tracing::warn!(error = %e, "config-version probe failed; treating KV as unavailable (no stamp minted, no write attempted)");
            VersionProbe::Unavailable
        }
    }
}

/// Which of the two request-path lifecycles a finished runtime belongs to, and
/// the version identity that goes with it.
///
/// This is deliberately **not** a `(grants, hooks, policy)` triple a caller
/// composes. Ruling 5.5 (as amended) makes the boot hook a property of the
/// *path*, not an argument: the deploy funnel seeds, a dynamic request build
/// reads and republishes without writing, and a prepared hydration does
/// neither. `runtime_build` expresses that as three named funnels, and this
/// enum is what lets [`finish_runtime`] share everything *around* the funnel
/// without letting a fourth caller invent a fourth combination.
enum RuntimeKind {
    /// A dynamically built runtime — the stored per-isolate build and the
    /// request-local transient one. `version` is the KV config-version stamp
    /// probed before the build, so a runtime is never tagged with a version
    /// newer than the config it loaded.
    ///
    /// `stored` says which of the two: `true` for the build that holds the
    /// isolate's build slot and becomes the cached runtime, `false` for the
    /// request-local one. Nothing about the finished runtime differs — the
    /// flag exists so a *boot failure* can name its blast radius, which is the
    /// one place the two lifecycles are not interchangeable. See
    /// [`RuntimeKind::lifecycle`].
    Dynamic { version: String, stored: bool },
    /// A runtime hydrated from a verified prepared plan. Its `version` is the
    /// plan hash, and it additionally carries the config generation the plan
    /// was sealed at — the `Some`/`None` on `ReadyRuntime::config_version` is
    /// what tells the two paths apart everywhere else in this file.
    Prepared {
        plan_hash: String,
        config_generation: String,
    },
}

impl RuntimeKind {
    /// How a *boot failure* names itself, which is a finer distinction than
    /// the `kind` log field ([`ReadyRuntime::kind_label`]) because the
    /// consequences differ.
    ///
    /// A `cached` failure is the isolate-owning build: every subsequent
    /// request in that isolate keeps failing until a version or config change
    /// forces a rebuild. A `transient` failure is the per-request fallback,
    /// which self-heals on the next request. Both answer an opaque 500, so
    /// this string is the only thing that tells an operator reading Workers
    /// logs a poisoned isolate from a lost race.
    fn lifecycle(&self) -> &'static str {
        match self {
            Self::Dynamic { stored: true, .. } => "cached",
            Self::Dynamic { stored: false, .. } => "transient",
            Self::Prepared { .. } => "prepared",
        }
    }

    fn version_and_config_version(self) -> (String, Option<String>) {
        match self {
            Self::Dynamic { version, .. } => (version, None),
            Self::Prepared {
                plan_hash,
                config_generation,
            } => (plan_hash, Some(config_generation)),
        }
    }

    /// Everything a finished runtime carries except its `Wafer`.
    ///
    /// Split out for the same reason the boot hooks' database halves are:
    /// `ReadyRuntime` holds a `wafer_run::Wafer`, which no wasm unit test can
    /// produce, and what a test needs to see here is that the two request paths
    /// arm an identical probe window from an identical clock reading. A fourth
    /// copy of this tail measuring against `now_millis()` instead of
    /// `started_at` would have been invisible.
    fn tag(self, environment_identity: String, started_at: u64) -> RuntimeTag {
        let (version, config_version) = self.version_and_config_version();
        RuntimeTag {
            version,
            config_version,
            environment_identity,
            probe_deadline_ms: next_probe_deadline_ms(started_at),
        }
    }
}

/// The non-`Wafer` half of a [`ReadyRuntime`], as [`RuntimeKind::tag`] computes
/// it.
struct RuntimeTag {
    version: String,
    config_version: Option<String>,
    environment_identity: String,
    probe_deadline_ms: u64,
}

/// This isolate's next runtime-build ordinal.
///
/// Surfaced via `Server-Timing` (`CacheOutcome::build_ordinal`) as a
/// zero-plumbing proxy for "D1 statements per logical request" — see
/// `impresspress_core::metrics`'s module doc. Written three times verbatim
/// before [`finish_runtime`] took it over.
fn next_build_ordinal() -> u32 {
    BUILD_COUNT.with(|count| {
        let next = count.get() + 1;
        count.set(next);
        next
    })
}

/// Boot a built runtime and wrap it as a [`ReadyRuntime`], with this isolate's
/// next build ordinal and the elapsed build time.
///
/// The three sites that produced a `ReadyRuntime` — the stored dynamic build,
/// the transient dynamic build, and `hydrate_prepared_runtime` (already one
/// function serving three prepared branches) — used to repeat this same tail
/// verbatim: pick a boot funnel, mint the `BUILD_COUNT` ordinal, measure
/// against `started_at`, and construct the struct with a fresh jittered probe
/// deadline and a zero failure streak. Three copies of a tail is how two paths
/// drift; one of them measuring against `now_millis()` instead of
/// `started_at`, or arming its probe window differently, would have been
/// invisible.
async fn finish_runtime(
    built: crate::runtime_build::BuiltRuntime,
    kind: RuntimeKind,
    environment_identity: String,
    started_at: u64,
) -> Result<(Rc<ReadyRuntime>, u32, u64), String> {
    let mut built = built;
    let lifecycle = kind.lifecycle();
    let booted = match &kind {
        RuntimeKind::Dynamic { .. } => {
            crate::runtime_build::boot_dynamic_request_runtime(&mut built).await
        }
        RuntimeKind::Prepared { .. } => {
            crate::runtime_build::boot_prepared_runtime(&mut built).await
        }
    };
    booted.map_err(|error| format!("{lifecycle}-runtime boot: {error}"))?;

    let build_ordinal = next_build_ordinal();
    let duration_ms = impresspress_core::util::now_millis().saturating_sub(started_at);
    let tag = kind.tag(environment_identity, started_at);
    let rt = Rc::new(ReadyRuntime {
        wafer: built.wafer,
        version: tag.version,
        config_version: tag.config_version,
        environment_identity: tag.environment_identity,
        probe_deadline_ms: Cell::new(tag.probe_deadline_ms),
        probe_failures: Cell::new(0),
    });
    Ok((rt, build_ordinal, duration_ms))
}

/// Publish a finished runtime as this isolate's cached one, and report the
/// cache outcome — the second half both *slot-owning* paths share.
///
/// Losing the right to publish is not an error: the runtime is complete and
/// internally consistent, and on Workers the build was charged against this
/// request's CPU budget, so discarding it to answer 503 throws away the
/// expensive part. It is served request-locally instead and the winning owner's
/// runtime becomes the cached one.
fn publish_runtime(
    guard: &BuildGuard,
    rt: &Rc<ReadyRuntime>,
    build_ordinal: u32,
    duration_ms: u64,
    is_cold: bool,
) -> CacheOutcome {
    let label = rt.kind_label();
    if !store_if_current(guard, rt.clone()) {
        tracing::warn!(
            build_ordinal,
            kind = label,
            "serving un-storable runtime request-locally; a newer owner superseded this build"
        );
    }
    tracing::info!(
        build_ordinal,
        duration_ms,
        cold = is_cold,
        kind = label,
        "runtime build complete"
    );
    if is_cold {
        CacheOutcome::ColdBuilt {
            build_ordinal,
            duration_ms,
        }
    } else {
        CacheOutcome::Rebuilt {
            build_ordinal,
            duration_ms,
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
    environment: &crate::environment::CfEnvironment,
    queries: &crate::database::D1QueryCount,
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
    if let Some(plan) = crate::environment::packaged_prepared_runtime_plan(environment)? {
        let environment_identity = environment.identity(request_config);
        let prepared_identity = prepared_cache_identity(&plan.plan_hash, &environment_identity);
        if !prepared_is_bypassed(&prepared_identity) {
            return get_or_build_prepared(
                env,
                environment,
                queries,
                request_config,
                plan,
                register_blocks,
                register_post_build,
            )
            .await;
        }
    }

    let environment_identity = environment.identity(request_config);

    // Hooks are FnOnce because only the request that acquires the build slot
    // consumes them. Waiters retain their own hooks while sleeping, then drop
    // them unused when the completed runtime is visible.
    let mut register_blocks = Some(register_blocks);
    let mut register_post_build = Some(register_post_build);
    // Whether this request's build attempt CONSUMED the isolate dirty flag.
    // Only a consumed signal may be re-marked if the build then fails: the
    // cold branch never takes DIRTY, and re-marking there would force the
    // NEXT successful runtime into an immediate full dynamic rebuild
    // (read_through, D1 structural reads — the multi-second path this file's
    // own comments flag for Cloudflare 1102 risk).
    let mut dirty_consumed = false;

    // A block, not a loop: every path below either returns or produces the
    // resolution. It was written as `loop { … break resolution; }`, which
    // `clippy::never_loop` denies — and did deny, unnoticed, because no CI job
    // runs `clippy --target wasm32-unknown-unknown` for this crate.
    let (probed_version, read_through, is_cold, built_at, build_guard) = {
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
                    environment,
                    queries,
                    request_config,
                    register_blocks
                        .take()
                        .expect("build hooks are consumed by at most one build attempt"),
                    register_post_build
                        .take()
                        .expect("build hooks are consumed by at most one build attempt"),
                    environment_identity.clone(),
                    now,
                    None,
                )
                .await;
            }
        }

        // Another request took the slot between the check above and here. Same
        // reasoning: build request-locally rather than refuse.
        let Some(build_guard) = BuildGuard::try_acquire(now) else {
            return hydrate_transient_dynamic_runtime(
                env,
                environment,
                queries,
                request_config,
                register_blocks
                    .take()
                    .expect("build hooks are consumed by at most one build attempt"),
                register_post_build
                    .take()
                    .expect("build hooks are consumed by at most one build attempt"),
                environment_identity.clone(),
                now,
                None,
            )
            .await;
        };

        // Re-check under ownership of the slot. Another request may have
        // completed a build while this one was waiting on its timer.
        let resolution = if let Some(rt) = cached() {
            let dirty = take_dirty();
            dirty_consumed = dirty;
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
            let probe_kv = match crate::services::make_kv_backend(env, crate::runner::KV_BINDING) {
                Ok(kv) => kv,
                Err(e) => {
                    // Same reasoning as the build-failure paths below: this
                    // attempt already took the flag, so propagating without
                    // restoring it strands the isolate on pre-write state.
                    if dirty {
                        mark_dirty();
                    }
                    return Err(e.into());
                }
            };
            let probe = probe_version(&probe_kv).await;

            // A pure deadline-elapsed probe (not dirty) that finds the
            // version unchanged just extends the window — no rebuild needed.
            // A LOCAL write (`dirty`) always rebuilds even if KV still reports
            // the old version because KV is eventually consistent. An
            // unavailable probe keeps the last valid runtime with a widened
            // window: rebuilding over a read error is exactly the
            // amplification the three-way probe exists to prevent.
            if rt.config_version.is_none()
                && !dynamic_probe_requires_rebuild(dirty, environment_changed, &rt.version, &probe)
            {
                let outcome = if matches!(probe, VersionProbe::Unavailable) {
                    rt.note_probe_failure(now);
                    CacheOutcome::ProbeFailed
                } else {
                    rt.note_probe_success(now);
                    CacheOutcome::ProbedFresh
                };
                return Ok((rt, outcome));
            }
            tracing::info!(old = %rt.version, new = %probe.observed(), dirty, environment_changed, "config version, Worker environment, or local state changed; rebuilding runtime");
            (probe.into_dynamic_version(), true, false, now, build_guard)
        } else {
            // Cold isolate: probe before build so the finished runtime is
            // tagged with a version no newer than the config it loaded.
            let kv = crate::services::make_kv_backend(env, crate::runner::KV_BINDING)?;
            (
                probe_version(&kv).await.into_dynamic_version(),
                false,
                true,
                now,
                build_guard,
            )
        };
        resolution
    };

    // This build already CONSUMED the dirty flag (`take_dirty` above). If it
    // now fails, that local-write signal must not die with it: the isolate
    // would keep serving the pre-write runtime as zero-await hits until the
    // probe deadline elapses — a window this change widened from 30-60s to
    // 5-10 minutes, and up to the backoff cap when probes are failing too.
    // Gated on `dirty_consumed` so a cold build failure (which never took the
    // flag) cannot manufacture a dirty signal and charge the next runtime a
    // full dynamic rebuild it does not need.
    let built = match crate::runtime_build::build_runtime(
        env,
        environment,
        queries,
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
    .await
    {
        Ok(built) => built,
        Err(e) => {
            if dirty_consumed {
                mark_dirty();
            }
            return Err(e);
        }
    };

    // One lifecycle: D1 grants before seal, seal, admin first, the request
    // path's read-only hook, every remaining slot, then the grants into the
    // storage block. Strict, because publishing a Wafer with a failed
    // lazy-init slot would let concurrent requests wait on one another's init
    // future, which is not a valid execution model here. The concrete services
    // are dropped instead of entering ReadyRuntime.
    let finished = finish_runtime(
        built,
        RuntimeKind::Dynamic {
            version: probed_version,
            stored: true,
        },
        environment_identity,
        built_at,
    )
    .await;
    let (rt, build_ordinal, duration_ms) = match finished {
        Ok(finished) => finished,
        Err(e) => {
            if dirty_consumed {
                mark_dirty();
            }
            return Err(e.into());
        }
    };

    // `build_guard` remains alive through `store`, so waiters cannot observe
    // BUILDING=false before the completed runtime is visible.
    let outcome = publish_runtime(&build_guard, &rt, build_ordinal, duration_ms, is_cold);
    Ok((rt, outcome))
}

/// Hydrate a packaged immutable plan without D1 settings or WRAP-grant reads.
/// One KV config-version read tags the cold hydration, followed by the same
/// bounded jittered probes used by dynamic runtimes. A local dirty signal or a
/// moved version permanently bypasses this plan/environment pair in the
/// isolate and restores dynamic hydration, preserving admin mutation
/// semantics without putting D1 structural reads back on the prepared cold
/// path.
// Eight arguments, one over clippy's threshold, and deliberately a parameter
// rather than something the caller sets on the returned handle afterwards.
// Every call site probes (or knowingly skips probing) differently, and a site
// that forgot to record a blind window is precisely the defect this argument
// was added to fix — a compiler-enforced parameter cannot be forgotten, a
// follow-up call can. Bundling the existing seven into a struct would add
// indirection to a private function with four call sites and hide that.
#[expect(
    clippy::too_many_arguments,
    reason = "a blind window a caller forgets to record is the defect the eighth \
              parameter exists to catch; an argument cannot be forgotten"
)]
async fn hydrate_prepared_runtime<F, G>(
    env: &worker::Env,
    environment: &crate::environment::CfEnvironment,
    queries: &crate::database::D1QueryCount,
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
    let built = crate::runtime_build::build_runtime(
        env,
        environment,
        queries,
        request_config,
        Some(plan),
        register_blocks,
        register_post_build,
        false,
        crate::kv_cached_db::CacheMode::default(),
    )
    .await?;

    // Grants and settings were imported from the verified plan, so this is the
    // one path whose `GrantSource` is `PreInstalled` and whose seed hook is a
    // written no-op — see `crate::runtime_build::boot_prepared_runtime`, which
    // `RuntimeKind::Prepared` selects. Everything else is the shared ordering,
    // under this request's services: ConfigSource may still perform per-block
    // reads, and keeping them here prevents cross-request lazy-init waiters.
    Ok(finish_runtime(
        built,
        RuntimeKind::Prepared {
            plan_hash: plan.plan_hash.clone(),
            config_generation: plan.config_generation.clone(),
        },
        environment_identity,
        started_at,
    )
    .await?)
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
// Eight arguments, for the same reason `build_runtime` has eight: the captured
// environment travels rather than being re-read here.
#[expect(
    clippy::too_many_arguments,
    reason = "the captured environment travels as a parameter rather than being \
              re-read here, exactly as in `build_runtime`"
)]
async fn hydrate_transient_dynamic_runtime<F, G>(
    env: &worker::Env,
    environment: &crate::environment::CfEnvironment,
    queries: &crate::database::D1QueryCount,
    request_config: &std::collections::HashMap<String, String>,
    register_blocks: F,
    register_post_build: G,
    environment_identity: String,
    started_at: u64,
    known_probe: Option<VersionProbe>,
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
    // With the probe unavailable a matching cached runtime is served as-is:
    // its freshness cannot be verified either way, and a request-local build
    // would carry an unverifiable stamp at full dynamic-build cost.
    //
    // A caller that already probed passes its result in. That saves the
    // second KV read (two version GETs per request through the post-deploy
    // busy window, against a subrequest ceiling and the read budget this
    // whole change exists to protect) AND keeps its evidence: re-probing
    // could return `Unavailable` and serve a cached version the caller's own
    // probe had just proved stale.
    let probe = match known_probe {
        Some(probe) => probe,
        None => {
            let kv = crate::services::make_kv_backend(env, crate::runner::KV_BINDING)?;
            probe_version(&kv).await
        }
    };
    if let Some(rt) = cached() {
        if rt.environment_identity == environment_identity
            && rt.config_version.is_none()
            && transient_dynamic_can_serve_cached(&rt.version, &probe)
        {
            // Record the failed probe even though this request is served:
            // it widens the next probe window, which is the whole point of
            // the backoff during an outage.
            if matches!(probe, VersionProbe::Unavailable) {
                rt.note_probe_failure(started_at);
            }
            return Ok((rt, CacheOutcome::Hit));
        }
    }
    let probed_version = probe.into_dynamic_version();

    let built = crate::runtime_build::build_runtime(
        env,
        environment,
        queries,
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

    // Literally the same tail the stored dynamic build takes — the two request
    // paths are one decision, not two. It used to be that build's statements
    // copied, and a copy is how two paths drift: both remembered the grant
    // load, neither passed a `BootHooks`, and no reader could tell which of
    // those was a decision. The deploy funnel is the one that differs (it
    // seeds), and it says so by calling `boot_deploy_runtime` instead.
    let (rt, build_ordinal, duration_ms) = finish_runtime(
        built,
        RuntimeKind::Dynamic {
            version: probed_version,
            stored: false,
        },
        environment_identity,
        started_at,
    )
    .await?;
    tracing::info!(
        build_ordinal,
        duration_ms,
        kind = rt.kind_label(),
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
    environment: &crate::environment::CfEnvironment,
    queries: &crate::database::D1QueryCount,
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
    let environment_identity = environment.identity(request_config);
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
        let kv = crate::services::make_kv_backend(env, crate::runner::KV_BINDING)?;
        let probe = probe_version(&kv).await;
        if !prepared_generation_matches(&plan.config_generation, &probe) {
            // The plan is stale AND the slot is busy — the post-deploy window,
            // where a config-generation bump invalidates the packaged plan for
            // every isolate at once. The non-busy path below reacts by bypassing
            // the plan and rebuilding dynamically; do the same request-locally
            // rather than refusing. Isolate-wide bypass state is left to the
            // owner, which reaches the same check once it holds the slot.
            // (An unavailable probe never lands here: the signed plan is
            // trusted rather than replaced with a dynamic build the probe
            // cannot justify.)
            return hydrate_transient_dynamic_runtime(
                env,
                environment,
                queries,
                request_config,
                register_blocks,
                register_post_build,
                environment_identity,
                now,
                // This request already probed, and that probe is what proved
                // the plan stale. Re-probing would cost a second KV read and
                // could flake to `Unavailable`, discarding this evidence.
                Some(probe),
            )
            .await;
        }
        if let Some(rt) = cached() {
            if rt.environment_identity == environment_identity
                && rt.version == plan_generation
                && prepared_cached_config_matches(rt.config_version.as_deref(), &probe)
            {
                // Same as the transient dynamic path: a failed probe still
                // widens this runtime's next probe window.
                if matches!(probe, VersionProbe::Unavailable) {
                    rt.note_probe_failure(now);
                }
                return Ok((rt, CacheOutcome::Hit));
            }
        }

        let (rt, build_ordinal, duration_ms) = hydrate_prepared_runtime(
            env,
            environment,
            queries,
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
            kind = rt.kind_label(),
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
            environment,
            queries,
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
            kind = rt.kind_label(),
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

                let probe_kv =
                    match crate::services::make_kv_backend(env, crate::runner::KV_BINDING) {
                        Ok(kv) => kv,
                        Err(e) => {
                            // `dirty` was consumed above; losing it here would
                            // strand the isolate on pre-write state for a full
                            // probe window.
                            if dirty {
                                mark_dirty();
                            }
                            return Err(e.into());
                        }
                    };
                let probe = probe_version(&probe_kv).await;
                if !prepared_probe_requires_fallback(dirty, cached_config_version, &probe) {
                    if matches!(probe, VersionProbe::Unavailable) {
                        rt.note_probe_failure(now);
                        return Ok((rt, CacheOutcome::ProbeFailed));
                    }
                    rt.note_probe_success(now);
                    return Ok((rt, CacheOutcome::ProbedFresh));
                }

                tracing::info!(
                    plan_hash = %plan_generation,
                    old_config_version = %cached_config_version,
                    new_config_version = %probe.observed(),
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
                    environment,
                    queries,
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
    let kv = crate::services::make_kv_backend(env, crate::runner::KV_BINDING)?;
    let probe = probe_version(&kv).await;
    if !prepared_generation_matches(&plan.config_generation, &probe) {
        tracing::info!(
            plan_hash = %plan_generation,
            plan_config_generation = %plan.config_generation,
            observed_config_generation = %probe.observed(),
            "prepared plan generation is stale; bypassing it for this isolate"
        );
        bypass_prepared(prepared_identity);
        drop(build_guard);
        return Box::pin(get_or_build(
            env,
            environment,
            queries,
            request_config,
            register_blocks,
            register_post_build,
        ))
        .await;
    }

    let (rt, build_ordinal, duration_ms) = hydrate_prepared_runtime(
        env,
        environment,
        queries,
        request_config,
        plan.as_ref(),
        register_blocks,
        register_post_build,
        environment_identity,
        now,
    )
    .await?;
    // Same publish-and-report tail as the dynamic path, including the reason a
    // lost publish is served request-locally rather than turned into a 503.
    let outcome = publish_runtime(&build_guard, &rt, build_ordinal, duration_ms, is_cold);
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

    /// A bare runtime, enough to publish: `store` only moves the `Rc`.
    fn ready_runtime() -> Rc<ReadyRuntime> {
        Rc::new(ReadyRuntime {
            wafer: wafer_run::Wafer::new(std::sync::Arc::new(
                wafer_run::StaticConfigSource::default(),
            ))
            .expect("a bare runtime"),
            version: "v-test".to_string(),
            config_version: None,
            environment_identity: "env-test".to_string(),
            probe_deadline_ms: Cell::new(0),
            probe_failures: Cell::new(0),
        })
    }

    /// Publishing a runtime drops the isolate's memoized schema. That is the
    /// only point at which a schema change made by another isolate — a deploy,
    /// a migration run — can be assumed to have reached this one, and the
    /// wiring is what makes the isolate-scoped cache sound; the cache's own
    /// tests cannot see it.
    #[wasm_bindgen_test]
    fn publishing_a_runtime_drops_the_isolates_schema_cache() {
        let cache = crate::database::isolate_schema_cache("DB");
        cache.set_primary_key_if_gen("published_t", vec!["id".into()], cache.generation());
        assert_eq!(
            crate::database::isolate_schema_cache("DB").primary_key("published_t"),
            Some(vec!["id".to_string()]),
            "the fact is in the isolate's cache before the rebuild"
        );

        store(ready_runtime());

        assert_eq!(
            crate::database::isolate_schema_cache("DB").primary_key("published_t"),
            None,
            "a rebuilt isolate must re-introspect rather than trust a schema \
             another isolate may have migrated"
        );
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

    /// The shared build tail: whatever a finished runtime's kind, the fields
    /// that are not about the kind must come out identical for identical
    /// inputs.
    ///
    /// This is what three verbatim copies of that tail could not guarantee. The
    /// probe deadline is jittered, so the assertion is on the window rather
    /// than the value — a copy that armed it from `now_millis()` instead of the
    /// build's own `started_at`, or from a different floor, lands outside it.
    #[wasm_bindgen_test]
    fn both_request_paths_tag_a_finished_runtime_the_same_way() {
        let started_at = 1_000_000_u64;
        let dynamic = RuntimeKind::Dynamic {
            version: "cfg-v1".to_string(),
            stored: true,
        }
        .tag("env-a".to_string(), started_at);
        let prepared = RuntimeKind::Prepared {
            plan_hash: "plan-hash".to_string(),
            config_generation: "cfg-v1".to_string(),
        }
        .tag("env-a".to_string(), started_at);

        assert_eq!(dynamic.environment_identity, prepared.environment_identity);
        for tag in [&dynamic, &prepared] {
            assert!(
                tag.probe_deadline_ms >= started_at + PROBE_INTERVAL_FLOOR_MS
                    && tag.probe_deadline_ms
                        <= started_at + PROBE_INTERVAL_FLOOR_MS + PROBE_INTERVAL_JITTER_MS,
                "probe window must be armed from this build's own start time",
            );
        }

        // The kind decides exactly two fields, and `config_version` is what the
        // rest of this file switches on to tell a prepared runtime from a
        // dynamic one.
        assert_eq!(dynamic.version, "cfg-v1");
        assert_eq!(dynamic.config_version, None);
        assert_eq!(prepared.version, "plan-hash");
        assert_eq!(prepared.config_version.as_deref(), Some("cfg-v1"));
    }

    /// A boot failure returns before any log line and answers an opaque 500,
    /// so its message is the only thing that separates a poisoned isolate
    /// (the cached build; every later request in that isolate keeps failing)
    /// from a lost race (the transient build, which self-heals). Sharing one
    /// tail must not cost that distinction — the `kind` field, which is about
    /// the hydration mechanism rather than the blast radius, deliberately does
    /// not draw it.
    #[wasm_bindgen_test]
    fn a_boot_failure_names_the_lifecycle_it_poisoned() {
        let cached = RuntimeKind::Dynamic {
            version: "cfg-v1".to_string(),
            stored: true,
        };
        let transient = RuntimeKind::Dynamic {
            version: "cfg-v1".to_string(),
            stored: false,
        };
        let prepared = RuntimeKind::Prepared {
            plan_hash: "plan-hash".to_string(),
            config_generation: "cfg-v1".to_string(),
        };

        assert_eq!(cached.lifecycle(), "cached");
        assert_eq!(transient.lifecycle(), "transient");
        assert_eq!(prepared.lifecycle(), "prepared");
        assert_ne!(
            cached.lifecycle(),
            transient.lifecycle(),
            "a poisoned isolate and a lost race must not read the same"
        );

        // The `kind` log field is coarser and is derived from the finished
        // runtime instead (`ReadyRuntime::kind_label`), so it cannot be typed
        // differently at one of the four sites that emit it. Only the tag it
        // switches on is assertable without a `Wafer`.
        assert_eq!(
            cached.tag("env".to_string(), 0).config_version,
            transient.tag("env".to_string(), 0).config_version
        );
        assert!(prepared.tag("env".to_string(), 0).config_version.is_some());
    }

    /// The build ordinal is a per-isolate counter surfaced through
    /// `Server-Timing`; three copies used to increment it.
    #[wasm_bindgen_test]
    fn every_finished_runtime_takes_the_next_build_ordinal() {
        let first = next_build_ordinal();
        assert_eq!(next_build_ordinal(), first + 1);
        assert_eq!(next_build_ordinal(), first + 2);
    }

    #[wasm_bindgen_test]
    fn local_dirty_forces_prepared_fallback_even_before_kv_converges() {
        let same = VersionProbe::Stamped("v1".to_string());
        assert!(prepared_probe_requires_fallback(true, "v1", &same));
        assert!(!prepared_probe_requires_fallback(false, "v1", &same));
    }

    #[wasm_bindgen_test]
    fn moved_config_version_forces_prepared_fallback() {
        assert!(prepared_probe_requires_fallback(
            false,
            "v1",
            &VersionProbe::Stamped("v2".to_string())
        ));
    }

    #[wasm_bindgen_test]
    fn second_isolate_rejects_v1_plan_after_generation_moves_to_v2() {
        let v1 = "1".repeat(32);
        let v2 = "2".repeat(32);
        // Isolate A started while the candidate's generation was current.
        assert!(prepared_generation_matches(
            &v1,
            &VersionProbe::Stamped(v1.clone())
        ));
        // A later admin/deploy mutation moves KV. A fresh isolate must not
        // hydrate the older packaged v1 structure.
        assert!(!prepared_generation_matches(
            &v1,
            &VersionProbe::Stamped(v2.clone())
        ));
        // The replacement Worker plan is accepted by another fresh isolate.
        assert!(prepared_generation_matches(
            &v2,
            &VersionProbe::Stamped(v2.clone())
        ));
        assert!(!prepared_generation_matches(
            impresspress_core::UNBOUND_CONFIG_GENERATION,
            &VersionProbe::Stamped(impresspress_core::UNBOUND_CONFIG_GENERATION.to_string()),
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

    /// What a probe mock's `get` returns, to drive all three
    /// [`VersionProbe`] arms.
    enum ProbeKvGet {
        Value(&'static str),
        Missing,
        Fail,
    }

    /// In-memory [`impresspress_core::kv::KvBackend`] that scripts `get` and
    /// counts every write-class call. `delete` is unreachable on the probe
    /// path.
    struct ProbeMockKv {
        get: ProbeKvGet,
        writes: Cell<u32>,
    }

    impl ProbeMockKv {
        fn new(get: ProbeKvGet) -> (Arc<dyn impresspress_core::kv::KvBackend>, Arc<Self>) {
            let concrete = Arc::new(Self {
                get,
                writes: Cell::new(0),
            });
            (
                concrete.clone() as Arc<dyn impresspress_core::kv::KvBackend>,
                concrete,
            )
        }
    }

    #[async_trait::async_trait(?Send)]
    impl impresspress_core::kv::KvBackend for ProbeMockKv {
        async fn get(&self, _key: &str) -> Result<Option<String>, String> {
            match self.get {
                ProbeKvGet::Value(v) => Ok(Some(v.to_string())),
                ProbeKvGet::Missing => Ok(None),
                ProbeKvGet::Fail => Err("simulated kv read failure".to_string()),
            }
        }

        async fn put_with_ttl(
            &self,
            _key: &str,
            _value: &str,
            _ttl_secs: u64,
        ) -> Result<(), String> {
            self.writes.set(self.writes.get() + 1);
            Ok(())
        }

        async fn put(&self, _key: &str, _value: &str) -> Result<(), String> {
            self.writes.set(self.writes.get() + 1);
            Ok(())
        }

        async fn delete(&self, _key: &str) -> Result<(), String> {
            unreachable!("the version probe never deletes")
        }
    }

    /// THE August 30/31 write-storm mechanism: a KV read failure must not be
    /// treated as a missing stamp. Minting + PUTting on `Err` converted read
    /// exhaustion into thousands of write requests.
    #[wasm_bindgen_test]
    async fn probe_error_returns_unavailable_and_never_writes() {
        let (kv, mock) = ProbeMockKv::new(ProbeKvGet::Fail);
        let probe = probe_version(&kv).await;
        assert!(
            matches!(probe, VersionProbe::Unavailable),
            "a failed GET is an availability problem, not a missing key"
        );
        assert_eq!(
            mock.writes.get(),
            0,
            "a KV read error must cause zero KV write attempts"
        );
    }

    /// A genuinely absent stamp keeps today's convergence behavior: mint one
    /// and persist it so every isolate lands on the same generation.
    #[wasm_bindgen_test]
    async fn probe_missing_key_still_mints_and_persists_stamp() {
        let (kv, mock) = ProbeMockKv::new(ProbeKvGet::Missing);
        let probe = probe_version(&kv).await;
        match probe {
            VersionProbe::Minted(v) => assert!(!v.is_empty()),
            other => panic!("expected Minted for a missing key, got {other:?}"),
        }
        assert_eq!(mock.writes.get(), 1, "exactly one successful PUT");
    }

    #[wasm_bindgen_test]
    async fn probe_present_key_returns_stamp_without_write() {
        let (kv, mock) = ProbeMockKv::new(ProbeKvGet::Value("stamp-a"));
        let probe = probe_version(&kv).await;
        match probe {
            VersionProbe::Stamped(v) => assert_eq!(v, "stamp-a"),
            other => panic!("expected Stamped, got {other:?}"),
        }
        assert_eq!(mock.writes.get(), 0);
    }

    /// A transient KV error on a warm prepared probe previously minted a
    /// local stamp, mismatched the cached generation, and PERMANENTLY
    /// bypassed the packaged plan for the isolate — converting one failed
    /// read into dynamic D1 rebuilds for the isolate's whole life.
    #[wasm_bindgen_test]
    fn warm_prepared_probe_error_does_not_bypass_plan() {
        assert!(!prepared_probe_requires_fallback(
            false,
            "gen-a",
            &VersionProbe::Unavailable
        ));
    }

    #[wasm_bindgen_test]
    fn dirty_state_forces_prepared_fallback_even_when_probe_fails() {
        // A local admin write must still win: D1 is the source of truth and
        // the rebuild path reads it directly.
        assert!(prepared_probe_requires_fallback(
            true,
            "gen-a",
            &VersionProbe::Unavailable
        ));
    }

    /// Cold prepared hydration with KV unavailable must trust the signed
    /// plan (its own `config_generation`) instead of manufacturing an
    /// isolate-local generation and falling back to a full dynamic build.
    #[wasm_bindgen_test]
    fn prepared_plan_is_trusted_when_probe_fails() {
        assert!(prepared_generation_matches(
            "gen-a",
            &VersionProbe::Unavailable
        ));
    }

    #[wasm_bindgen_test]
    fn unbound_plan_is_never_trusted_even_when_probe_fails() {
        assert!(!prepared_generation_matches(
            impresspress_core::UNBOUND_CONFIG_GENERATION,
            &VersionProbe::Unavailable
        ));
    }

    /// The dynamic warm path mirrors the prepared one: probe error ⇒ keep
    /// serving the last valid runtime; never rebuild on unavailability alone.
    #[wasm_bindgen_test]
    fn dynamic_probe_error_keeps_last_valid_runtime() {
        assert!(!dynamic_probe_requires_rebuild(
            false,
            false,
            "v1",
            &VersionProbe::Unavailable
        ));
        // A genuine version move still rebuilds…
        assert!(dynamic_probe_requires_rebuild(
            false,
            false,
            "v1",
            &VersionProbe::Stamped("v2".to_string())
        ));
        // …a matching stamp does not…
        assert!(!dynamic_probe_requires_rebuild(
            false,
            false,
            "v1",
            &VersionProbe::Stamped("v1".to_string())
        ));
        // …and dirty or environment changes always do, probe or no probe.
        assert!(dynamic_probe_requires_rebuild(
            true,
            false,
            "v1",
            &VersionProbe::Unavailable
        ));
        assert!(dynamic_probe_requires_rebuild(
            false,
            true,
            "v1",
            &VersionProbe::Unavailable
        ));
    }

    /// A cold request that lost the build-slot race and cannot verify
    /// freshness (KV error) serves the cached runtime rather than paying for
    /// a request-local dynamic build it cannot version-tag honestly.
    #[wasm_bindgen_test]
    fn transient_dynamic_serves_cached_runtime_when_probe_fails() {
        assert!(transient_dynamic_can_serve_cached(
            "v1",
            &VersionProbe::Unavailable
        ));
        assert!(transient_dynamic_can_serve_cached(
            "v1",
            &VersionProbe::Stamped("v1".to_string())
        ));
        assert!(!transient_dynamic_can_serve_cached(
            "v1",
            &VersionProbe::Stamped("v2".to_string())
        ));
    }

    // The 5–10 minute probe window itself is pinned by the `const _: () =
    // assert!(..)` pair next to `PROBE_INTERVAL_FLOOR_MS` at the top of this
    // file, which holds on every build rather than only under a wasm test
    // runner.

    /// With a 5-minute jitter width, two random bytes (max 65,535ms) would
    /// silently cap the spread at ~65s and re-synchronize isolates that
    /// warmed together. The jitter source must be at least 32 bits wide.
    #[wasm_bindgen_test]
    fn probe_jitter_uses_more_than_16_bits_of_randomness() {
        let jitter = probe_jitter_ms(16_777_215);
        assert_eq!(jitter, 16_777_215 % PROBE_INTERVAL_JITTER_MS);
        assert!(u64::from(u32::try_from(jitter).unwrap()) > u64::from(u16::MAX));
        assert!(probe_jitter_ms(u32::MAX) < PROBE_INTERVAL_JITTER_MS);
    }

    /// A cached prepared runtime must be servable when the probe is
    /// unavailable: the signed plan is already trusted on that path, so
    /// paying a fresh hydration for the same plan is pure waste.
    #[wasm_bindgen_test]
    fn prepared_cached_config_is_served_when_probe_is_unavailable() {
        assert!(prepared_cached_config_matches(
            Some("gen-a"),
            &VersionProbe::Unavailable
        ));
        assert!(prepared_cached_config_matches(
            Some("gen-a"),
            &VersionProbe::Stamped("gen-a".to_string())
        ));
        assert!(!prepared_cached_config_matches(
            Some("gen-a"),
            &VersionProbe::Stamped("gen-b".to_string())
        ));
        // A dynamic runtime (no config_version) is not a prepared cache hit.
        assert!(!prepared_cached_config_matches(
            None,
            &VersionProbe::Unavailable
        ));
    }

    /// Retrying an exhausted daily allowance at the normal cadence provides
    /// no freshness and just manufactures failed operations: consecutive
    /// probe failures must widen the window, up to a cap.
    /// The cap must bound the DEADLINE, not just the pre-jitter window.
    /// `probe_failure_window_ms` is clamped and then jitter was added on top,
    /// so the real ceiling exceeded the documented one by the full jitter
    /// width — the helper's own test could not see it because it never looked
    /// at the deadline.
    #[wasm_bindgen_test]
    fn probe_failure_deadline_never_exceeds_the_documented_cap() {
        let now = 1_000_000;
        for failures in [1u32, 2, 5, 10, u32::MAX] {
            for jitter in [0, PROBE_INTERVAL_JITTER_MS - 1] {
                let deadline = probe_failure_deadline_ms(now, failures, jitter);
                assert!(
                    deadline > now,
                    "a widened window must still be in the future"
                );
                assert!(
                    deadline <= now + PROBE_FAILURE_BACKOFF_CAP_MS,
                    "failures={failures} jitter={jitter} produced {} past the cap",
                    deadline - now
                );
            }
        }
        // Jitter must still spread isolates apart below the cap.
        assert_ne!(
            probe_failure_deadline_ms(now, 1, 0),
            probe_failure_deadline_ms(now, 1, 60_000)
        );
    }

    #[wasm_bindgen_test]
    fn probe_failure_backoff_doubles_and_caps() {
        assert_eq!(probe_failure_window_ms(0), PROBE_INTERVAL_FLOOR_MS);
        assert_eq!(probe_failure_window_ms(1), 2 * PROBE_INTERVAL_FLOOR_MS);
        assert!(probe_failure_window_ms(2) <= PROBE_FAILURE_BACKOFF_CAP_MS);
        assert_eq!(probe_failure_window_ms(10), PROBE_FAILURE_BACKOFF_CAP_MS);
        // Absurd counts must not overflow the shift.
        assert_eq!(
            probe_failure_window_ms(u32::MAX),
            PROBE_FAILURE_BACKOFF_CAP_MS
        );
    }
}
