//! Per-isolate runtime cache. Builds the Wafer once per isolate (sealed, no
//! boot funnel — migrations/seeds happen at deploy via `/_deploy/init`),
//! stores it in a thread_local, and rebuilds when the KV config-version
//! stamp moves. Mirrors impresspress-browser/src/runtime.rs's thread_local
//! pattern; `Rc` handles (not raw pointers) keep an in-flight request's
//! runtime alive across a swap. Every cell here is a `Cell`/`IsolateCell`,
//! never a `RefCell`: wasm32 is single-threaded so a borrow would never be
//! *contended*, but Cloudflare can hard-stop a request without running its
//! destructors, and a borrow flag stranded that way traps the next request
//! that touches it (see the `thread_local!` block's comment).
//!
//! Request-path runtime construction is single-flight per isolate. One
//! request owns the build slot ([`BuildGuard`]); a cold request that finds the
//! slot owned and nothing it can serve WAITS for that build instead of
//! building a runtime of its own ([`admit_cold`] / [`wait_for_owner`]). Each
//! runtime is 13–20 MB of Wasm memory against a 128 MB isolate, and a cold
//! request that builds one measured about 400–670 ms of CPU on Cloudflare, so
//! k concurrent cold requests each building their own is what killed an
//! isolate in the 2026-10-14 deploy smoke.
//!
//! Two builds happen outside that rule, and both are bounded:
//!
//! - The deploy funnel (`/_deploy/init`, `/_deploy/prepare`,
//!   `deploy_endpoints.rs`) builds a fresh runtime without the slot, because
//!   it must not use the cache. It is token-gated and operator-driven: at most
//!   one extra runtime, during a deploy.
//! - An owner still building past [`BUILD_HARD_LEASE_MS`] keeps building after
//!   a waiter reclaims the slot. Its late publish is refused and it serves its
//!   runtime request-locally ([`publish_runtime`]); its failure is not
//!   recorded against the new owner ([`BuildGuard::record_failure`]).
//!
//! The wait never awaits the owner's future and is never woken by the owner:
//! the build performs D1/KV I/O, a future created by one Workers request must
//! not be polled on behalf of another, and a waker the owner fired would run
//! the waiter's continuation inside the owner's I/O context. A waiter sleeps
//! on a timer of its own and re-checks the isolate cells — plain data — on
//! each tick, bounded by the build leases. When the wait ends without a
//! runtime it re-enters [`get_or_build`] once; a second miss is a retryable
//! [`RuntimeBuildBusy`] 503. A failed owner is recorded in
//! `BUILD_LAST_FAILURE`, so its waiters answer 503 rather than rebuilding one
//! after another.
//!
//! # The retry window (D1 outage plan I5)
//!
//! A runtime is retried by being rebuilt, never by running a block's `Init`
//! in a request (the request path boots `Serving` over an `InitWait::Refuse`
//! runtime). One isolate cell, `RETRY_WINDOW`, paces that: a build that
//! published degraded, failed transiently or failed permanently arms it, 1 s
//! doubling to 60 s, jittered, and a healthy publish clears it. While it is
//! armed it is checked first, before `DIRTY`, the probe deadline and
//! [`admit_cold`]: a request is served the cached runtime of its environment,
//! however stale or degraded ([`CacheOutcome::Stale`]), and nothing is built;
//! with nothing cached, a cold isolate hydrates this version's packaged plan
//! and serves it stale (the user's decision U-B1), or answers 503 with the
//! window's `Retry-After`. After it, the first request retries under the
//! slot, and a degraded or stale runtime never counts as a warm hit. A
//! packaged plan is bypassed for the isolate only once a dynamic runtime for
//! the moved config is stored, so a failed dynamic build leaves the plan's
//! runtime serving.
//!
//! ## The gate
//!
//! A retry asks the backend before it builds anything. A build is about
//! 320 ms of CPU on Cloudflare against the Free plan's 10 ms per request, and
//! more than 70 % of it comes before any block reads D1, so a retry that
//! found out by building paid a whole runtime per window: on the 2026-11-03
//! live outage check that repetition had Cloudflare throttle the isolates and
//! kill a quarter of all requests, static files included. So the slot owner
//! first reads the variables table — the read every degraded block failed
//! on — through the D1 handle the build would use
//! ([`runtime_build::read_variables`](crate::runtime_build::read_variables)),
//! and only builds once it answers, handing the build the snapshot it filled
//! (the table is still read once per attempt, the read moved, not added):
//!
//! - **A runtime is cached and the read fails transiently**: nothing is built.
//!   The window doubles and the runtime serves on as it was
//!   ([`WindowCause::GateFailed`], [`CacheOutcome::Gated`]); after a hand-off,
//!   whose kept runtime is of older config, it is marked stale
//!   ([`WindowCause::RebuildFailed`]) and `DIRTY` stays set.
//! - **Nothing is cached** (a cold isolate): the prepared path builds anyway
//!   over the failed snapshot, the degraded publish that serves static pages
//!   (Decision 1) and the isolate's one overrun, as any cold start's. The
//!   dynamic path does not: its boot reads the WRAP grants before anything
//!   else and cannot publish past a failed read, so it answers 503 without
//!   assembling, and the next request takes U-B1's hydrate.
//! - **A permanent failure**: nothing is built; the window is armed as a
//!   `Fault` and this request answered the 500, as a failed build is.
//! - **The read answers**: the full rebuild runs, as before the gate.
//!
//! A runtime whose blocks all initialized and whose only defect is a failed
//! variables read one of its later requests left behind is healed by the
//! read alone, with no rebuild ([`heals_in_place`]).

use std::{
    cell::{Cell, OnceCell},
    rc::{Rc, Weak},
    sync::Arc,
};

use impresspress_core::{
    cache_key::CONFIG_VERSION_KEY,
    config_source::VariablesSnapshot,
    metrics::CacheOutcome,
    runtime_health::{PlatformHealth, RetryEpisode, RetryStreaks},
    IsolateCell,
};
use wafer_core::interfaces::database::service::DatabaseService;

use crate::runtime_build::BuildSource;

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
/// active isolates x ~1,920 probes/day). Cold builds still probe once each,
/// deliberately: a build with no usable cached runtime has to resolve a
/// version somehow, and their volume is bounded by isolate churn rather than
/// by wall-clock cadence. A request that waits for another's build probes
/// nothing; it serves the owner's freshly probed runtime. The counter
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
/// How often a waiting cold request re-checks the cache, on its own timer.
///
/// A tick is a handful of `Cell` reads, so the cost is the latency it adds
/// after the owner publishes (at most one tick) — small beside a cold build's
/// 0.9–1.5 s of wall time on Cloudflare.
const BUILD_WAIT_POLL_MS: u64 = 25;
/// Longest one wait lasts, from when it began. A single owner's slot already
/// ends by [`BUILD_HARD_LEASE_MS`]; this also bounds a wait that spans
/// successive owners (a prepared owner that bypasses its stale plan hands the
/// slot to a dynamic build in the same poll).
const BUILD_WAIT_MAX_MS: u64 = BUILD_HARD_LEASE_MS;

pub(crate) struct ReadyRuntime {
    pub wafer: wafer_run::Wafer,
    // Deliberately no D1/KV/R2/network/config handles here. The Wafer's six
    // service blocks contain stateless request-scoped proxies; concrete
    // services are selected by `request_services::scope` for each dispatch.
    /// The runtime's read of the `variables` table — data and memo, no
    /// handle. Every warm request's config source reads through it
    /// (`runtime_build::warm_request_services`), as the runtime's config block
    /// does, so a warm request resolves config without a query; a rebuilt
    /// runtime gets a new one.
    pub variables_snapshot: std::sync::Arc<impresspress_core::config_source::VariablesSnapshot>,
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
    /// Whether this runtime is known not to be current: published without a
    /// block (degraded), or kept, or hydrated, from config older than the
    /// database's (stale). Never a warm hit while it is not healthy, so the
    /// first request after the retry window rebuilds it. `/health` reports
    /// it ([`platform_health`]).
    pub health: Cell<RuntimeHealth>,
    /// Which blocks were retrying their `Init` when this runtime was stored,
    /// carried on from the runtime it replaced ([`RetryStreaks::carried`]),
    /// so `/health` can tell a block that has failed the same way across
    /// rebuilds from one that failed once. Set once, by [`publish_runtime`]
    /// when it stores the runtime; a `OnceCell`, which has no borrow flag to
    /// strand (see the `thread_local!` block's comment).
    retry_streaks: OnceCell<RetryStreaks>,
}

impl ReadyRuntime {
    /// The `kind` field every runtime-lifecycle log line carries, derived from
    /// the runtime itself rather than typed at each site.
    ///
    /// Both paths announce a finished runtime through [`publish_runtime`],
    /// and a failed boot names its path with the same word
    /// ([`RuntimeKind::lifecycle`]), so one dashboard filter on `kind` sees a
    /// path's builds and its failures. `config_version.is_some()` is what the
    /// rest of this file switches on to tell the two paths apart.
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
// destructors: a hard stop can strand state, and a `RefCell`'s borrow flag is
// one such state. Under this workspace's `panic = "abort"` wasm profile a
// stranded `RefCell` traps the next request that touches it inside `poll`,
// leaving a response promise that is never settled. Cloudflare reports that
// as "your Worker's code had hung and would never generate a response", at
// ~zero recorded CPU. The wasm instance is then re-instantiated, the cached
// runtime with it, by worker 0.8's panic hook and worker-build's shim, or by
// wasm-bindgen's call guard (`impresspress_core::isolate_cell` has both
// paths). No reset on Cloudflare has been confirmed by a trace, but the
// 2026-10-15 analytics have failing rows whose memory figures look like
// re-instantiated instances. The executor's own task queue was the other
// isolate-wide state a hard stop could strand; the workspace `Cargo.toml`
// patches js-sys for it.
// `IsolateCell` has no borrow flag to strand; the worst an interrupted holder
// can do is drop the cached value, which is indistinguishable from a cold
// isolate and self-heals. See `impresspress_core::isolate_cell`.
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
    /// The most recent slot owner whose build failed. A request waiting on
    /// that owner (or a later one) answers 503 instead of re-entering, so one
    /// persistent `Init` failure costs one build per cold window rather than
    /// one per waiter. The next request that is NOT a waiter is the retry.
    static BUILD_LAST_FAILURE: Cell<Option<BuildFailure>> = const { Cell::new(None) };
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
    /// The isolate's retry window (see the module docs): `Some` from a build
    /// that published degraded or failed until a healthy publish. A `Copy`
    /// value in a plain `Cell`, like `BUILD_LAST_FAILURE`.
    static RETRY_WINDOW: Cell<Option<RetryWindow>> = const { Cell::new(None) };
}

/// RAII ownership of the isolate's runtime-build slot. Dropping the future on
/// an error/cancellation also drops this guard, allowing the next request to
/// retry instead of leaving the isolate permanently wedged.
struct BuildGuard {
    owner: u64,
    _liveness: Rc<()>,
}

/// A cold request could not get a runtime without building a second one
/// alongside another request's build. `lib.rs` answers it with a 503 and a
/// `Retry-After` of [`RuntimeBuildBusy::retry_after_secs`], rather than
/// awaiting request-owned work or leaving a response future pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeBuildBusy {
    /// This request already waited once and re-entered, and the slot is owned
    /// again.
    SlotBusy,
    /// The owner this request waited on failed its build. The next cold
    /// request that is not a waiter retries it.
    LeaderFailed,
}

impl RuntimeBuildBusy {
    /// Seconds a client should wait before retrying. One: a successful owner
    /// publishes within a cold build's wall time, and a failed one is retried
    /// by the next cold request.
    pub(crate) fn retry_after_secs(self) -> u32 {
        1
    }
}

impl std::fmt::Display for RuntimeBuildBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SlotBusy => "runtime build still in progress after one wait",
            Self::LeaderFailed => "the runtime build this request waited for failed",
        })
    }
}

impl std::error::Error for RuntimeBuildBusy {}

/// Why the request path has no runtime to serve, typed for the edge
/// (`lib.rs`, D1 outage plan I4).
#[derive(Debug)]
pub(crate) enum RuntimeBuildError {
    /// A later attempt may succeed: a backend outage met by the build, the
    /// build slot busy, or the retry window. A 503 with `Retry-After:
    /// retry_after_secs`; `cause` is logged, never sent.
    Unavailable {
        cause: String,
        retry_after_secs: u32,
    },
    /// Anything else: the opaque 500, with this logged.
    Fault(String),
}

impl RuntimeBuildError {
    pub(crate) fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    /// This error with a `Retry-After` no earlier than the isolate's retry
    /// window, where one is armed: a waiter answered `LeaderFailed` or
    /// `SlotBusy` (one second) inside a just-armed window would otherwise be
    /// told to come back before anything may be built (#275 review n6).
    fn at_least_the_window(self, now: u64) -> Self {
        match self {
            Self::Unavailable {
                cause,
                retry_after_secs,
            } => Self::Unavailable {
                cause,
                retry_after_secs: retry_after_secs
                    .max(armed_window(now).map_or(0, |window| window.remaining_secs(now))),
            },
            fault => fault,
        }
    }

    /// An outage-class failure (one second to retry; the window, where one is
    /// armed, replaces it), or a fault.
    pub(crate) fn classified(transient: bool, cause: String) -> Self {
        if transient {
            Self::Unavailable {
                cause,
                retry_after_secs: 1,
            }
        } else {
            Self::Fault(cause)
        }
    }
}

impl std::fmt::Display for RuntimeBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable { cause, .. } => write!(f, "unavailable: {cause}"),
            Self::Fault(cause) => f.write_str(cause),
        }
    }
}

impl std::error::Error for RuntimeBuildError {}

impl From<RuntimeBuildBusy> for RuntimeBuildError {
    fn from(busy: RuntimeBuildBusy) -> Self {
        Self::Unavailable {
            cause: busy.to_string(),
            retry_after_secs: busy.retry_after_secs(),
        }
    }
}

/// What armed the retry window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowCause {
    /// A runtime was published without a block whose `Init` failed
    /// transiently, or its variables snapshot is remembering a failed read.
    Degraded,
    /// A build failed transiently with nothing cached.
    BuildUnavailable,
    /// A rebuild failed transiently; the runtime it was replacing serves on,
    /// marked stale. Also a failed gate on a hand-off, whose kept runtime is
    /// of config older than the database's.
    RebuildFailed,
    /// The window ended and the gate's read of the variables table failed
    /// transiently, so nothing was built: the runtime of the current config
    /// serves on as it was, degraded and not stale (see the module docs).
    GateFailed,
    /// The stale-plan hydrate (U-B1 (b)) failed transiently with nothing
    /// cached: no further hydrate until the window ends.
    HydrateFailed,
    /// A build failed permanently.
    Fault,
}

/// The isolate's retry window. `Copy`, so its cell stays a plain `Cell`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetryWindow {
    /// Consecutive armings, by a build or by a failed gate (at least 1); the
    /// delay doubles with each. `/health` reports it as `retries`.
    failures: u32,
    /// When the first of them armed it: what a runtime served stale through
    /// it is stale since.
    armed_at_ms: u64,
    /// Nothing is built before this.
    retry_at_ms: u64,
    pub(crate) cause: WindowCause,
}

impl RetryWindow {
    /// Whether this arming is its episode's
    /// [`PERSISTENT_RETRIES`](impresspress_core::runtime_health::PERSISTENT_RETRIES)th:
    /// true for exactly one arming of an episode, the one [`arm_window`] logs
    /// at `error!`.
    fn is_newly_persistent(&self) -> bool {
        self.failures == impresspress_core::runtime_health::PERSISTENT_RETRIES
    }

    /// Whole seconds until it ends, at least 1 (a `Retry-After: 0` invites
    /// an immediate hammering retry).
    pub(crate) fn remaining_secs(&self, now: u64) -> u32 {
        let ms = self.retry_at_ms.saturating_sub(now);
        u32::try_from(ms.div_ceil(1_000)).unwrap_or(u32::MAX).max(1)
    }

    /// This window's answer to a request it has no runtime for: a 503 with
    /// its remaining time, or — after a permanent failure — the 500.
    pub(crate) fn error(&self, now: u64) -> RuntimeBuildError {
        match self.cause {
            WindowCause::Fault => RuntimeBuildError::Fault(
                "a permanent runtime-build failure (logged with its own reference when it \
                 happened); the next build runs when the retry window ends"
                    .to_string(),
            ),
            _ => RuntimeBuildError::Unavailable {
                cause: format!("no runtime while the retry window lasts ({:?})", self.cause),
                retry_after_secs: self.remaining_secs(now),
            },
        }
    }
}

/// Retry delay after the first arming build: 1 s.
const RETRY_WINDOW_BASE_MS: u64 = 1_000;
/// Longest retry delay: 60 s, so an isolate recovers within a minute of its
/// backend coming back. A constant to tune with the live `d1_cost` data.
const RETRY_WINDOW_MAX_MS: u64 = 60_000;

/// The delay after `failures` consecutive arming builds, before jitter.
fn retry_delay_ms(failures: u32) -> u64 {
    RETRY_WINDOW_BASE_MS
        .checked_shl(failures.saturating_sub(1).min(16))
        .unwrap_or(RETRY_WINDOW_MAX_MS)
        .min(RETRY_WINDOW_MAX_MS)
}

/// Up to a quarter of `delay_ms`, at random: isolates that failed together
/// do not all retry in the same millisecond. Scaled to the delay rather than
/// the probe window's 5-minute jitter ([`random_probe_jitter_ms`]), which
/// would swamp a one-second retry.
fn retry_jitter_ms(delay_ms: u64) -> u64 {
    let mut buf = [0u8; 4];
    if getrandom::getrandom(&mut buf).is_ok() {
        u64::from(u32::from_le_bytes(buf)) % (delay_ms / 4 + 1)
    } else {
        0
    }
}

/// Arm the retry window (or double it, if armed), for `cause`. Returns it.
///
/// The [`PERSISTENT_RETRIES`]th arming in a row is logged at `error!`, once
/// per episode: by then the backend has failed for about two minutes of
/// windows, a lasting outage. It is the line that outlives the isolate
/// (Workers Logs) when nothing is rebuilt per window, which is how a
/// database-wide outage goes once the gate stops builds; a block's own
/// [`RetryStreaks`] line needs a runtime published per window.
///
/// [`PERSISTENT_RETRIES`]: impresspress_core::runtime_health::PERSISTENT_RETRIES
fn arm_window(now: u64, cause: WindowCause) -> RetryWindow {
    let previous = RETRY_WINDOW.with(Cell::get);
    let failures = previous.map_or(1, |w| w.failures.saturating_add(1));
    let delay = retry_delay_ms(failures);
    let window = RetryWindow {
        failures,
        armed_at_ms: previous.map_or(now, |w| w.armed_at_ms),
        retry_at_ms: now + delay + retry_jitter_ms(delay),
        cause,
    };
    tracing::warn!(
        ?cause,
        failures,
        retry_in_ms = window.retry_at_ms.saturating_sub(now),
        "runtime retry window armed"
    );
    if window.is_newly_persistent() {
        tracing::error!(
            ?cause,
            retries = failures,
            since_ms = window.armed_at_ms,
            "the runtime retry window has been armed again and again for minutes: \
             a lasting failure (see `cause`), not a passing one"
        );
    }
    RETRY_WINDOW.with(|cell| cell.set(Some(window)));
    window
}

/// A healthy runtime is published: no retry pending.
fn clear_window() {
    RETRY_WINDOW.with(|cell| cell.set(None));
}

/// The retry window, while it has not ended.
fn armed_window(now: u64) -> Option<RetryWindow> {
    RETRY_WINDOW
        .with(Cell::get)
        .filter(|window| now < window.retry_at_ms)
}

/// The remaining retry window in whole seconds, for a 503 the edge sends
/// without one of its own; 1 when none is armed.
pub(crate) fn retry_after_secs(now: u64) -> u32 {
    armed_window(now).map_or(1, |window| window.remaining_secs(now))
}

/// A runtime's health ([`ReadyRuntime::health`]): since when it has been
/// degraded and since when stale, if it is. Both can hold at once (a stale
/// plan hydrated during an outage is usually both). `Copy`, for a `Cell`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeHealth {
    /// Published without a block whose `Init` failed transiently, or its
    /// variables snapshot is remembering a failed read.
    pub degraded_since_ms: Option<u64>,
    /// Kept after a rebuild failed, or hydrated from a plan older than the
    /// database's config: serving config that may be out of date.
    pub stale_since_ms: Option<u64>,
}

impl RuntimeHealth {
    pub fn is_healthy(&self) -> bool {
        self.degraded_since_ms.is_none() && self.stale_since_ms.is_none()
    }

    fn degraded_since(now: u64) -> Self {
        Self {
            degraded_since_ms: Some(now),
            stale_since_ms: None,
        }
    }
}

/// One failed build, as `BUILD_LAST_FAILURE` records it. `Copy`, so the cell
/// stays a plain `Cell` (see the `thread_local!` block's comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BuildFailure {
    /// Wall-clock time of the failure, for the log line.
    at_ms: u64,
    /// The failed owner's token. Tokens only grow, so "a failure newer than
    /// the owner a waiter started waiting on" is `owner >= waited_on`, with no
    /// clock comparison to get wrong.
    owner: u64,
}

fn last_build_failure() -> Option<BuildFailure> {
    BUILD_LAST_FAILURE.with(Cell::get)
}

/// True when the owner `waited_on`, or any owner after it, failed its build.
fn build_failed_since(waited_on: u64) -> bool {
    last_build_failure().is_some_and(|failure| failure.owner >= waited_on)
}

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

    /// Record that this owner's build failed, so the requests waiting on it
    /// answer 503 instead of each re-entering to rebuild. A superseded owner
    /// records nothing: its slot already belongs to someone else, whose
    /// waiters are not waiting on it.
    fn record_failure(&self, now: u64) {
        if self.is_current() {
            BUILD_LAST_FAILURE.with(|failure| {
                failure.set(Some(BuildFailure {
                    at_ms: now,
                    owner: self.owner,
                }));
            });
        }
    }
}

/// Run the build this request owns, recording a failure for its waiters.
///
/// Every owner region goes through here, so a failure path cannot forget to
/// record itself — an early `?` inside `build` is still an `Err` here.
async fn as_owner<T, E>(
    guard: &BuildGuard,
    timer: &dyn WaitTimer,
    build: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let result = build.await;
    if result.is_err() {
        guard.record_failure(timer.now_ms());
    }
    result
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

/// Stale-while-revalidate for the dynamic path: while a healthy builder owns
/// the slot, a request may serve the last complete dynamic runtime for its
/// environment. `None` when no build is active (go take the slot) or nothing
/// is servable (a cold request: [`admit_cold`] makes it wait). A stale slot
/// reads as inactive and is reclaimed by [`BuildGuard::try_acquire`].
fn dynamic_runtime_while_building(
    now: u64,
    environment_identity: &str,
) -> Option<Rc<ReadyRuntime>> {
    if !build_slot_active(now) {
        return None;
    }
    cached()
        .filter(|rt| rt.environment_identity == environment_identity && rt.config_version.is_none())
}

/// The zero-await warm hit for a DYNAMIC runtime: not dirty, healthy, built
/// for this environment, inside its probe window. The warm path and a waiter accept a
/// runtime by this one predicate, so a waiter never serves something the
/// next warm request would have refused.
fn dynamic_runtime_is_warm(rt: &ReadyRuntime, environment_identity: &str, now: u64) -> bool {
    !DIRTY.with(Cell::get)
        && rt.health.get().is_healthy()
        && rt.config_version.is_none()
        && rt.environment_identity == environment_identity
        && now < rt.probe_deadline_ms.get()
}

/// The zero-await warm hit for a PREPARED runtime: not dirty, this plan, this
/// environment, inside its probe window. Shared by the warm path and waiters,
/// as [`dynamic_runtime_is_warm`] is.
fn prepared_runtime_is_warm(
    rt: &ReadyRuntime,
    environment_identity: &str,
    plan_generation: &str,
    now: u64,
) -> bool {
    !DIRTY.with(Cell::get)
        && rt.health.get().is_healthy()
        && rt.environment_identity == environment_identity
        && rt.version == plan_generation
        && rt.config_version.is_some()
        && now < rt.probe_deadline_ms.get()
}

/// The clock and the timer a waiting request uses. Both are its own: the
/// timer is armed by the waiting request, so its wake-up runs in that
/// request's context, never in the owner's.
trait WaitTimer {
    fn now_ms(&self) -> u64;
    fn sleep(&self, ms: u64) -> futures::future::LocalBoxFuture<'static, ()>;
}

/// The Workers clock and a `setTimeout` owned by the calling request.
struct WorkerTimer;

impl WaitTimer for WorkerTimer {
    fn now_ms(&self) -> u64 {
        impresspress_core::util::now_millis()
    }

    fn sleep(&self, ms: u64) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(worker::Delay::from(std::time::Duration::from_millis(ms)))
    }
}

/// Which pass through [`get_or_build`] this is. A request that waited and
/// still found nothing re-enters once, as `Reentry`; a `Reentry` never waits
/// again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    First,
    Reentry,
}

/// What a request with nothing servable cached does next.
enum ColdAdmission {
    /// It owns the build slot and builds.
    Build(BuildGuard),
    /// The owner it waited on published a runtime it accepts.
    Serve {
        rt: Rc<ReadyRuntime>,
        waited_ms: u64,
    },
    /// The wait ended with nothing acceptable and no failure: re-enter
    /// [`get_or_build`] from the top, once. Re-entry re-decides everything —
    /// reclaim, a bypassed plan, a moved config version — rather than this
    /// function guessing which applies.
    Reenter,
}

/// Admit a request that has nothing servable cached: own the build, or wait
/// for the request that does.
///
/// This is where concurrent cold requests stop building a runtime each.
/// `accept` is the warm-hit predicate of the caller's path
/// ([`dynamic_runtime_is_warm`] / [`prepared_runtime_is_warm`]).
async fn admit_cold(
    attempt: Attempt,
    timer: &dyn WaitTimer,
    accept: impl Fn(&ReadyRuntime, u64) -> bool,
) -> Result<ColdAdmission, RuntimeBuildBusy> {
    if let Some(guard) = BuildGuard::try_acquire(timer.now_ms()) {
        return Ok(ColdAdmission::Build(guard));
    }
    if attempt == Attempt::Reentry {
        return Err(RuntimeBuildBusy::SlotBusy);
    }
    wait_for_owner(timer, accept).await
}

/// Wait for the slot owner's build on this request's own timer.
///
/// Each tick reads isolate cells and nothing else: no KV probe (the owner
/// probed, and its runtime carries the result in its probe window), no I/O,
/// no handle from the owner's request. It ends when
///
/// - the cache holds a runtime `accept` takes → serve it;
/// - the owner waited on, or a later one, recorded a failure → 503;
/// - the slot is no longer active (released, or past a lease — a hard-stopped
///   owner included) or [`BUILD_WAIT_MAX_MS`] passed → re-enter once.
async fn wait_for_owner(
    timer: &dyn WaitTimer,
    accept: impl Fn(&ReadyRuntime, u64) -> bool,
) -> Result<ColdAdmission, RuntimeBuildBusy> {
    let started = timer.now_ms();
    let waited_on = BUILD_OWNER.with(Cell::get);
    loop {
        timer.sleep(BUILD_WAIT_POLL_MS).await;
        let now = timer.now_ms();
        let waited_ms = now.saturating_sub(started);
        if let Some(rt) = cached().filter(|rt| accept(rt, now)) {
            return Ok(ColdAdmission::Serve { rt, waited_ms });
        }
        if build_failed_since(waited_on) {
            if let Some(failure) = last_build_failure() {
                tracing::warn!(
                    owner = failure.owner,
                    failed_at_ms = failure.at_ms,
                    waited_ms,
                    "the runtime build this request waited for failed; answering 503"
                );
            }
            return Err(RuntimeBuildBusy::LeaderFailed);
        }
        if !build_slot_active(now) || waited_ms >= BUILD_WAIT_MAX_MS {
            return Ok(ColdAdmission::Reenter);
        }
    }
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
    // `retain_if_eq` is where the clear-on-mismatch semantics live.
    // `bypass_is_scoped_to_exact_plan_and_environment_identity` below holds
    // them through this function; the `cloudflare-wasm-test` job runs it.
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
    // one. That matters here: the displaced `Rc<ReadyRuntime>` is often the
    // last handle to an entire Wafer, and dropping it then frees that
    // runtime, so its destructor is long enough to be interrupted — and
    // `*slot.borrow_mut() = Some(rt)` would have run that destructor with the
    // borrow held.
    //
    // "Frees" holds because no block or service stores a context of the
    // runtime: `scripts/grep-guard-stored-context.sh` checks the source, and
    // the `Weak` probes in `impresspress-server` (native) and
    // `impresspress-browser` (wasm32) check a booted runtime — neither runs
    // this Worker build. The one known holder past the drop is a stream
    // producer whose consumer left: on Workers it is never polled again and
    // keeps the runtime it captured until the isolate dies (the spawn
    // lifetime rule in `wafer_block::spawn`).
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
    /// A dynamically built runtime, built by the request that owns the
    /// isolate's build slot. `version` is the KV config-version stamp probed
    /// before the build, so a runtime is never tagged with a version newer
    /// than the config it loaded.
    Dynamic { version: String },
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
    /// How a *boot failure* names its path. It is the same word the `kind`
    /// log field uses ([`ReadyRuntime::kind_label`]), which is derived from a
    /// finished runtime and so cannot name a build that never finished.
    ///
    /// On the request path every build is the slot owner's, so the path is
    /// the whole distinction; the owner's failure is recorded for its waiters
    /// in `BUILD_LAST_FAILURE`. The two builds outside the slot are named in
    /// the module doc: the deploy funnel, which boots through its own funnel
    /// and never reaches this, and an owner that outlives the hard lease,
    /// which still names its path here.
    fn lifecycle(&self) -> &'static str {
        match self {
            Self::Dynamic { .. } => "dynamic",
            Self::Prepared { .. } => "prepared",
        }
    }

    fn version_and_config_version(self) -> (String, Option<String>) {
        match self {
            Self::Dynamic { version } => (version, None),
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
/// Both paths that produce a `ReadyRuntime` — the dynamic build and
/// `hydrate_prepared_runtime` — share this tail: pick a boot funnel, mint the
/// `BUILD_COUNT` ordinal, measure against `started_at`, and construct the
/// struct with a fresh jittered probe deadline and a zero failure streak. A
/// copy of a tail is how two paths drift; one measuring against
/// `now_millis()` instead of `started_at`, or arming its probe window
/// differently, would be invisible.
async fn finish_runtime(
    built: crate::runtime_build::BuiltRuntime,
    kind: RuntimeKind,
    environment_identity: String,
    started_at: u64,
) -> Result<Finished, RuntimeBuildError> {
    let mut built = built;
    let lifecycle = kind.lifecycle();
    let build = match &kind {
        RuntimeKind::Dynamic { .. } => impresspress_core::builder::RequestBuild::Database,
        RuntimeKind::Prepared { .. } => impresspress_core::builder::RequestBuild::Prepared,
    };
    // Classified by the boot's own rule (`BootError::is_transient`): an
    // outage is `Unavailable` (503, retried by the window), anything else a
    // fault (500).
    let report = crate::runtime_build::boot_request_runtime(&mut built, build)
        .await
        .map_err(|error| {
            RuntimeBuildError::classified(
                error.is_transient(),
                format!("{lifecycle}-runtime boot: {error}"),
            )
        })?;

    let build_ordinal = next_build_ordinal();
    let duration_ms = impresspress_core::util::now_millis().saturating_sub(started_at);
    let tag = kind.tag(environment_identity, started_at);
    let rt = Rc::new(ReadyRuntime {
        wafer: built.wafer,
        variables_snapshot: built.variables_snapshot,
        version: tag.version,
        config_version: tag.config_version,
        environment_identity: tag.environment_identity,
        probe_deadline_ms: Cell::new(tag.probe_deadline_ms),
        probe_failures: Cell::new(0),
        health: Cell::new(RuntimeHealth::default()),
        retry_streaks: OnceCell::new(),
    });
    if !report.degraded.is_empty() {
        tracing::warn!(
            kind = lifecycle,
            degraded = ?report
                .degraded
                .iter()
                .map(|block| block.block.as_str())
                .collect::<Vec<_>>(),
            "runtime published without blocks whose Init failed transiently"
        );
    }
    Ok(Finished {
        rt,
        build_ordinal,
        duration_ms,
        degraded: !report.degraded.is_empty(),
    })
}

/// Publish a finished runtime as this isolate's cached one, and report the
/// cache outcome and whether it was stored — the second half both
/// *slot-owning* paths share. Only a stored runtime may move isolate state
/// (the retry window, the prepared bypass).
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
) -> (CacheOutcome, bool) {
    let label = rt.kind_label();
    let replaced = cached();
    let stored = store_if_current(guard, rt.clone());
    if stored {
        let streaks = replaced
            .as_deref()
            .and_then(|previous| previous.retry_streaks.get())
            .cloned()
            .unwrap_or_default()
            .carried(
                &rt.wafer.init_states(),
                impresspress_core::util::now_millis(),
            );
        // A runtime is stored once; a second store of the same one keeps the
        // streaks of its first.
        let _ = rt.retry_streaks.set(streaks);
    }
    if !stored {
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
    (outcome, stored)
}

/// What every build on the request path reads from its own request: the Env,
/// the captured environment, the D1 call count and the per-request
/// config. Borrowed together so the path's functions take one argument for
/// them rather than four.
#[derive(Clone, Copy)]
struct RequestInputs<'a> {
    env: &'a worker::Env,
    environment: &'a crate::environment::CfEnvironment,
    calls: &'a crate::database::D1CallCount,
    request_config: &'a std::collections::HashMap<String, String>,
}

/// A build, finished and booted, as a [`BuildIo`] hands it back.
pub(crate) struct Finished {
    rt: Rc<ReadyRuntime>,
    build_ordinal: u32,
    duration_ms: u64,
    /// The boot published without at least one block whose `Init` failed
    /// transiently (`BootReport::degraded`).
    degraded: bool,
}

/// The I/O a request-path pass performs — the KV version probe, the reads a
/// build makes before it assembles anything (the gate among them), and the
/// two builds — behind one seam, so the cache's decisions (the retry window,
/// the gate, the bypass rule, single flight) run in tests over fakes.
/// Production's is [`WorkerBuildIo`]; every decision around these calls is
/// shared.
#[async_trait::async_trait(?Send)]
trait BuildIo {
    /// The request's environment identity (`CfEnvironment::identity`).
    fn environment_identity(&self) -> String;
    /// This Worker version's verified packaged plan, if it ships one.
    fn packaged_plan(
        &self,
    ) -> Result<Option<Rc<impresspress_core::PreparedRuntimePlan>>, RuntimeBuildError>;
    /// Probe the KV config-version stamp ([`probe_version`]).
    async fn probe_version(&self) -> Result<VersionProbe, RuntimeBuildError>;
    /// This request's D1 handle for one attempt
    /// ([`runtime_build::open_build_database`](crate::runtime_build::open_build_database)):
    /// opened once by the slot owner, and every read of the attempt — the
    /// gate's and the build's — goes through it. The invocation's count is
    /// noted [`BuildKind::Gate`](crate::database::BuildKind::Gate) first: an
    /// attempt has assembled nothing until its build starts and notes itself.
    fn open_database(
        &self,
        cache_mode: crate::kv_cached_db::CacheMode,
    ) -> Result<Arc<dyn DatabaseService>, RuntimeBuildError>;
    /// The dynamic path's `block_settings` read
    /// ([`runtime_build::read_block_settings`](crate::runtime_build::read_block_settings)).
    async fn read_block_settings(
        &self,
        db: &Arc<dyn DatabaseService>,
    ) -> Result<impresspress_core::features::BlockSettings, RuntimeBuildError>;
    /// The gate: read the variables table into `variables` now
    /// ([`runtime_build::read_variables`](crate::runtime_build::read_variables)).
    async fn read_variables(
        &self,
        db: &Arc<dyn DatabaseService>,
        variables: &Arc<VariablesSnapshot>,
    ) -> Result<(), RuntimeBuildError>;
    /// Build and boot a dynamic runtime tagged `version`, over `source`.
    async fn build_dynamic(
        &self,
        source: BuildSource,
        version: String,
        environment_identity: String,
        started_at: u64,
    ) -> Result<Finished, RuntimeBuildError>;
    /// Hydrate and boot `plan`, over `source`.
    async fn hydrate_prepared(
        &self,
        source: BuildSource,
        plan: &impresspress_core::PreparedRuntimePlan,
        environment_identity: String,
        started_at: u64,
    ) -> Result<Finished, RuntimeBuildError>;
}

/// [`BuildIo`] over a Worker request: its Env, captured environment, D1
/// call count and request config, and the consumer's registration hooks —
/// consumed by the one build a request may run.
struct WorkerBuildIo<'a, F, G> {
    inputs: RequestInputs<'a>,
    hooks: Cell<Option<(F, G)>>,
}

impl<F, G> WorkerBuildIo<'_, F, G> {
    fn take_hooks(&self) -> Result<(F, G), RuntimeBuildError> {
        self.hooks.take().ok_or_else(|| {
            RuntimeBuildError::Fault("a request ran a second runtime build".to_string())
        })
    }
}

#[async_trait::async_trait(?Send)]
impl<F, G> BuildIo for WorkerBuildIo<'_, F, G>
where
    F: FnOnce(
        crate::ImpresspressBuilder,
    ) -> Result<crate::ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn wafer_core::interfaces::storage::service::StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    fn environment_identity(&self) -> String {
        self.inputs.environment.identity(self.inputs.request_config)
    }

    fn packaged_plan(
        &self,
    ) -> Result<Option<Rc<impresspress_core::PreparedRuntimePlan>>, RuntimeBuildError> {
        crate::environment::packaged_prepared_runtime_plan(self.inputs.environment)
            .map_err(|e| RuntimeBuildError::Fault(format!("read the packaged plan: {e}")))
    }

    async fn probe_version(&self) -> Result<VersionProbe, RuntimeBuildError> {
        // Always derived from THIS request's Env: a cached runtime retains no
        // KV binding.
        let kv = crate::services::make_kv_backend(self.inputs.env, crate::runner::KV_BINDING)
            .map_err(|e| RuntimeBuildError::Fault(format!("KV binding: {e}")))?;
        Ok(probe_version(&kv).await)
    }

    fn open_database(
        &self,
        cache_mode: crate::kv_cached_db::CacheMode,
    ) -> Result<Arc<dyn DatabaseService>, RuntimeBuildError> {
        // `build=gate` on this invocation's `d1_cost` line, unless a build
        // starts and notes itself (`BuildKind`'s doc): whatever this attempt
        // reads before then, it reads without assembling a runtime.
        self.inputs
            .calls
            .note_build(crate::database::BuildKind::Gate);
        crate::runtime_build::open_build_database(
            self.inputs.env,
            self.inputs.environment,
            self.inputs.calls,
            cache_mode,
        )
    }

    async fn read_block_settings(
        &self,
        db: &Arc<dyn DatabaseService>,
    ) -> Result<impresspress_core::features::BlockSettings, RuntimeBuildError> {
        crate::runtime_build::read_block_settings(db).await
    }

    async fn read_variables(
        &self,
        db: &Arc<dyn DatabaseService>,
        variables: &Arc<VariablesSnapshot>,
    ) -> Result<(), RuntimeBuildError> {
        crate::runtime_build::read_variables(db, variables).await
    }

    async fn build_dynamic(
        &self,
        source: BuildSource,
        version: String,
        environment_identity: String,
        started_at: u64,
    ) -> Result<Finished, RuntimeBuildError> {
        let (register_blocks, register_post_build) = self.take_hooks()?;
        // This invocation's `d1_cost` line names the build its rows were read
        // for, a failed one included.
        self.inputs
            .calls
            .note_build(crate::database::BuildKind::Dynamic);
        let built = crate::runtime_build::build_runtime(
            self.inputs.env,
            self.inputs.environment,
            self.inputs.request_config,
            None,
            register_blocks,
            register_post_build,
            false,
            source,
        )?;
        // One lifecycle: D1 grants before seal, seal, admin first, the request
        // path's read-only hook, every remaining slot, then the grants into
        // the storage block, under the request path's `Serving` policy: a
        // block whose Init failed transiently is published unsettled and
        // answers `Unavailable` (the runtime is `InitWait::Refuse`, so no
        // request runs its Init); anything permanent fails the build. The
        // concrete services are dropped instead of entering ReadyRuntime.
        finish_runtime(
            built,
            RuntimeKind::Dynamic { version },
            environment_identity,
            started_at,
        )
        .await
    }

    async fn hydrate_prepared(
        &self,
        source: BuildSource,
        plan: &impresspress_core::PreparedRuntimePlan,
        environment_identity: String,
        started_at: u64,
    ) -> Result<Finished, RuntimeBuildError> {
        let (register_blocks, register_post_build) = self.take_hooks()?;
        hydrate_prepared_runtime(
            self.inputs,
            source,
            plan,
            register_blocks,
            register_post_build,
            environment_identity,
            started_at,
        )
        .await
    }
}

/// Return the per-isolate cached runtime, rebuilding it if the KV
/// config-version stamp has moved (or if nothing is cached yet), alongside
/// the [`CacheOutcome`] this call resolved to — a free byproduct of the
/// branches below, consumed by `lib.rs::run` to build the `Server-Timing`
/// response header.
///
/// The `register_blocks` / `register_post_build` hooks are `FnOnce` and are
/// consumed only on the build path; on a cache hit, or by a request that
/// waits for another request's build, they are dropped unused.
///
/// An `Err` is typed ([`RuntimeBuildError`]): `Unavailable` (a 503 with
/// `Retry-After` at the edge) for anything a later attempt may cure — a
/// backend outage, the build slot busy, the retry window — and `Fault` (the
/// opaque 500) for the rest.
pub(crate) async fn get_or_build<F, G>(
    env: &worker::Env,
    environment: &crate::environment::CfEnvironment,
    calls: &crate::database::D1CallCount,
    request_config: &std::collections::HashMap<String, String>,
    register_blocks: F,
    register_post_build: G,
) -> Result<(Rc<ReadyRuntime>, CacheOutcome), RuntimeBuildError>
where
    F: FnOnce(
        crate::ImpresspressBuilder,
    ) -> Result<crate::ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn wafer_core::interfaces::storage::service::StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    let io = WorkerBuildIo {
        inputs: RequestInputs {
            env,
            environment,
            calls,
            request_config,
        },
        hooks: Cell::new(Some((register_blocks, register_post_build))),
    };
    get_or_build_with(&io, &WorkerTimer).await
}

/// [`get_or_build`] over any [`BuildIo`] and clock: every decision the
/// request path makes, with the I/O passed in.
async fn get_or_build_with(
    io: &dyn BuildIo,
    timer: &dyn WaitTimer,
) -> Result<(Rc<ReadyRuntime>, CacheOutcome), RuntimeBuildError> {
    let served = with_one_reentry(|attempt| get_or_build_attempt(io, timer, attempt))
        .await
        .map_err(|error| error.at_least_the_window(timer.now_ms()))?;
    observe_snapshot(&served.0, timer.now_ms());
    Ok(served)
}

/// How one pass through the cache ended.
enum Pass<T> {
    Done(T),
    /// The request waited and found nothing it accepts: pass again, once.
    Reenter,
}

/// The one place a request's passes are sequenced: an [`Attempt::First`]
/// pass, and at most one [`Attempt::Reentry`] pass after it. A `Reentry`
/// pass never waits ([`admit_cold`] answers `SlotBusy` instead), so no request
/// waits more than once; a second `Reenter` is answered the same way.
async fn with_one_reentry<T, E, Fut>(mut pass: impl FnMut(Attempt) -> Fut) -> Result<T, E>
where
    E: From<RuntimeBuildBusy>,
    Fut: std::future::Future<Output = Result<Pass<T>, E>>,
{
    if let Pass::Done(done) = pass(Attempt::First).await? {
        return Ok(done);
    }
    match pass(Attempt::Reentry).await? {
        Pass::Done(done) => Ok(done),
        Pass::Reenter => Err(RuntimeBuildBusy::SlotBusy.into()),
    }
}

/// What a pass through the cache produces: a runtime, or a re-entry.
type PassResult = Result<Pass<(Rc<ReadyRuntime>, CacheOutcome)>, RuntimeBuildError>;

/// One pass through the cache. A request that waited and found nothing comes
/// back here once as [`Attempt::Reentry`], through [`with_one_reentry`].
async fn get_or_build_attempt(
    io: &dyn BuildIo,
    timer: &dyn WaitTimer,
    attempt: Attempt,
) -> PassResult {
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
    // work.
    //
    // What the plan removes is the D1 structural reads: their round trips and
    // subrequests. It does not make a build cheap. A cold prepared build
    // measured 170–245 ms of CPU locally; a cold request that builds one
    // measured about 400–670 ms of CPU on Cloudflare, its own work included
    // (733 ms was the deploy funnel's `/_deploy/prepare`). A built runtime
    // holds 13–20 MB of Wasm memory against the isolate's 128 MB
    // (CF-RESOURCE-LIMITS, 2026-10-15). That is why a cold request that finds
    // another request building waits for that build ([`admit_cold`]) instead
    // of hydrating one of its own.
    let now = timer.now_ms();
    let environment_identity = io.environment_identity();
    let plan = io.packaged_plan()?.filter(|plan| {
        !prepared_is_bypassed(&prepared_cache_identity(
            &plan.plan_hash,
            &environment_identity,
        ))
    });

    // The retry window comes first, before `DIRTY`, the probe deadline and
    // `admit_cold` (I5b): inside it this isolate builds nothing on a
    // request's behalf, and `DIRTY` is left for the rebuild after it.
    if let Some(window) = armed_window(now) {
        if let Some(rt) = cached().filter(|rt| rt.environment_identity == environment_identity) {
            let outcome = if rt.health.get().is_healthy() {
                CacheOutcome::Hit
            } else {
                CacheOutcome::Stale
            };
            return Ok(Pass::Done((rt, outcome)));
        }
        if cached().is_none() {
            return serve_without_runtime(io, timer, attempt, window, plan, environment_identity)
                .await;
        }
        // A runtime is cached for another environment identity (a new
        // secret, a new version): it is not this request's, and nothing is
        // built inside the window.
        return Err(window.error(now));
    }

    match plan {
        Some(plan) => {
            get_or_build_prepared(io, timer, plan, environment_identity, now, attempt).await
        }
        None => get_or_build_dynamic(io, timer, environment_identity, now, attempt, None).await,
    }
}

/// I5b's "window armed, nothing cached": the user's decision U-B1 (b). A cold
/// isolate whose dynamic build just failed transiently — its packaged plan is
/// older than the database's config, so it went dynamic — hydrates that plan
/// and serves it marked stale, rather than answering 503 on every route.
///
/// - With a plan for this version and a window whose cause is neither
///   [`WindowCause::Fault`] nor [`WindowCause::HydrateFailed`]: take the slot
///   and hydrate (a waiter accepts whatever runtime is published for its
///   identity), publish it stale ([`RuntimeHealth::stale_since_ms`]) since
///   the window was armed, and leave the window as it is, whatever the boot's
///   outcome (addendum §4.2): the plan is known to be older than the
///   database, so even a healthy hydrate is retried after the window, through
///   the prepared owner's probe and the dynamic hand-off. `PREPARED_BYPASS`
///   stays unset and `DIRTY` untouched. A failed hydrate paces itself like any
///   failed build: `HydrateFailed` (doubled) or `Fault`, so at most one
///   hydrate runs per window.
/// - Without a plan, after a permanent failure, or once this window's
///   hydrate has failed: the window's own answer.
async fn serve_without_runtime(
    io: &dyn BuildIo,
    timer: &dyn WaitTimer,
    attempt: Attempt,
    window: RetryWindow,
    plan: Option<Rc<impresspress_core::PreparedRuntimePlan>>,
    environment_identity: String,
) -> PassResult {
    let now = timer.now_ms();
    // One hydrate per window: not after a permanent failure, and not again
    // once this window's hydrate has failed (#275 review M1).
    let hydrates = !matches!(
        window.cause,
        WindowCause::Fault | WindowCause::HydrateFailed
    );
    let Some(plan) = plan.filter(|_| hydrates) else {
        return Err(window.error(now));
    };
    let identity = environment_identity.clone();
    let admission = admit_cold(attempt, timer, |rt, _now| {
        rt.environment_identity == identity
    })
    .await?;
    let build_guard = match admission {
        ColdAdmission::Build(guard) => guard,
        ColdAdmission::Serve { rt, waited_ms } => {
            return Ok(Pass::Done((rt, CacheOutcome::Waited { waited_ms })));
        }
        ColdAdmission::Reenter => return Ok(Pass::Reenter),
    };
    // Nothing is cached, so the hydrate is gated as a cold isolate's: a
    // transient failure of the gate's read hydrates anyway, degraded.
    let finished = match as_owner(
        &build_guard,
        timer,
        gated_hydrate(io, &plan, true, environment_identity, now),
    )
    .await
    {
        Ok(finished) => finished,
        // A failed hydrate paces itself like every failed build: a transient
        // one doubles the window, a permanent one re-arms it as a `Fault`, so
        // this isolate builds at most once per window (#275 review M1).
        Err(unbuilt) => {
            return match rebuild_failed(
                &build_guard,
                timer,
                unbuilt.into_error(),
                None,
                FailedBuild::Hydrate,
            ) {
                AfterFailure::Answer(error) => Err(error),
                AfterFailure::Serve(rt) => Ok(Pass::Done((rt, CacheOutcome::Stale))),
            };
        }
    };
    finished.rt.health.set(RuntimeHealth {
        stale_since_ms: Some(window.armed_at_ms),
        degraded_since_ms: finished.degraded.then_some(now),
    });
    tracing::warn!(
        plan_hash = %plan.plan_hash,
        degraded = finished.degraded,
        "serving this version's packaged plan, stale, while the retry window lasts"
    );
    // The outcome names the build this request ran (`ColdBuilt`), so
    // `Server-Timing` carries it; the runtime's health says it is stale.
    let (outcome, _stored) = publish_runtime(
        &build_guard,
        &finished.rt,
        finished.build_ordinal,
        finished.duration_ms,
        true,
    );
    Ok(Pass::Done((finished.rt, outcome)))
}

async fn get_or_build_dynamic(
    io: &dyn BuildIo,
    timer: &dyn WaitTimer,
    environment_identity: String,
    now: u64,
    attempt: Attempt,
    commit_bypass: Option<String>,
) -> PassResult {
    let served = dynamic_pass(io, timer, environment_identity.clone(), now, attempt).await;
    // A hand-off commits the plan's bypass once this isolate's cached runtime
    // is a dynamic one for this environment — the meaning `PREPARED_BYPASS`
    // has — however the pass got there: built and stored by this request, or
    // found cached (a waiter's owner published it, or a warm hit). Never for a
    // runtime a superseded owner served request-locally, which is not the
    // cached one (addendum §4.1; #275 review n9, n4).
    if let (Some(prepared_identity), Ok(Pass::Done((rt, _)))) = (commit_bypass, &served) {
        let is_cached = cached().is_some_and(|cached| Rc::ptr_eq(&cached, rt));
        if is_cached
            && rt.config_version.is_none()
            && rt.environment_identity == environment_identity
        {
            bypass_prepared(prepared_identity);
        }
    }
    served
}

/// The dynamic path's pass, without the hand-off's bypass commit.
async fn dynamic_pass(
    io: &dyn BuildIo,
    timer: &dyn WaitTimer,
    environment_identity: String,
    now: u64,
    attempt: Attempt,
) -> PassResult {
    // Zero-await warm hit (dominant path once warm). Skipped while another
    // request owns the build slot: it may be replacing this runtime with
    // a newer one, and the stale-while-revalidate branch below serves it
    // without the probe-deadline check.
    if !BUILDING.with(Cell::get) {
        if let Some(rt) =
            cached().filter(|rt| dynamic_runtime_is_warm(rt, &environment_identity, now))
        {
            return Ok(Pass::Done((rt, CacheOutcome::Hit)));
        }
    }

    // A healthy owner is building and a dynamic runtime for this environment
    // is cached: stale-while-revalidate.
    if let Some(rt) = dynamic_runtime_while_building(now, &environment_identity) {
        return Ok(Pass::Done((rt, CacheOutcome::Hit)));
    }

    let admission = admit_cold(attempt, timer, |rt, now| {
        dynamic_runtime_is_warm(rt, &environment_identity, now)
    })
    .await?;
    let build_guard = match admission {
        ColdAdmission::Build(guard) => guard,
        ColdAdmission::Serve { rt, waited_ms } => {
            return Ok(Pass::Done((rt, CacheOutcome::Waited { waited_ms })));
        }
        ColdAdmission::Reenter => return Ok(Pass::Reenter),
    };
    let served = as_owner(
        &build_guard,
        timer,
        build_dynamic_as_owner(&build_guard, io, timer, environment_identity, now),
    )
    .await?;
    Ok(Pass::Done(served))
}

async fn build_dynamic_as_owner(
    build_guard: &BuildGuard,
    io: &dyn BuildIo,
    timer: &dyn WaitTimer,
    environment_identity: String,
    now: u64,
) -> Result<(Rc<ReadyRuntime>, CacheOutcome), RuntimeBuildError> {
    // Whether this request's build attempt CONSUMED the isolate dirty flag.
    // Only a consumed signal may be re-marked if the build then fails: the
    // cold branch never takes DIRTY, and re-marking there would force the
    // NEXT successful runtime into an immediate full dynamic rebuild
    // (read_through, D1 structural reads — the multi-second path this file's
    // own comments flag for Cloudflare 1102 risk).
    let mut dirty_consumed = false;
    let mut kept_is_current = false;
    let previous = cached();

    // Re-check under ownership of the slot: a request that waited and
    // re-entered may find a runtime here that its wait's predicate refused.
    let (probed_version, read_through, is_cold) = if let Some(rt) = previous.clone() {
        let dirty = take_dirty();
        dirty_consumed = dirty;
        let environment_changed = rt.environment_identity != environment_identity;
        let healthy = rt.health.get().is_healthy();

        if !dirty
            && healthy
            && rt.config_version.is_none()
            && !environment_changed
            && now < rt.probe_deadline_ms.get()
        {
            return Ok((rt, CacheOutcome::Hit));
        }

        let probe = match io.probe_version().await {
            Ok(probe) => probe,
            Err(e) => {
                // Same reasoning as the build-failure paths below: this
                // attempt already took the flag, so propagating without
                // restoring it strands the isolate on pre-write state.
                if dirty {
                    mark_dirty();
                }
                return Err(e);
            }
        };
        // Retried for its health alone: nothing says the config moved and
        // nothing has marked it stale, so the runtime kept after a failed
        // gate is of the current config (degraded, not stale) and may be
        // healed in place.
        kept_is_current = !dirty
            && rt.health.get().stale_since_ms.is_none()
            && rt.config_version.is_none()
            && !dynamic_probe_requires_rebuild(dirty, environment_changed, &rt.version, &probe);

        // A pure deadline-elapsed probe (not dirty) that finds the version
        // unchanged just extends the window — no rebuild needed — for a
        // healthy runtime. A degraded or stale one is rebuilt: it is what the
        // retry window exists to retry. A LOCAL write (`dirty`) always
        // rebuilds even if KV still reports the old version because KV is
        // eventually consistent. An unavailable probe keeps the last valid
        // runtime with a widened window: rebuilding over a read error is
        // exactly the amplification the three-way probe exists to prevent.
        if healthy
            && rt.config_version.is_none()
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
        if kept_is_current && heals_in_place(&rt) {
            return heal_or_gate(build_guard, io, timer, rt, &probe, now).await;
        }
        tracing::info!(old = %rt.version, new = %probe.observed(), dirty, environment_changed, healthy, "config version, Worker environment, local state or health changed; rebuilding runtime");
        (probe.into_dynamic_version(), true, false)
    } else {
        // Cold isolate: probe before build so the finished runtime is
        // tagged with a version no newer than the config it loaded.
        (
            io.probe_version().await?.into_dynamic_version(),
            false,
            true,
        )
    };

    // This build already CONSUMED the dirty flag (`take_dirty` above). If it
    // now fails, that local-write signal must not die with it: the isolate
    // would keep serving the pre-write runtime as zero-await hits until the
    // probe deadline elapses — 5-10 minutes, and up to the backoff cap when
    // probes are failing too. Gated on `dirty_consumed` so a cold build
    // failure (which never took the flag) cannot manufacture a dirty signal
    // and charge the next runtime a full dynamic rebuild it does not need.
    //
    // A failed gate takes this same branch (CPU-GATE review m1): it re-marks
    // the flag, keeps the runtime, and commits no bypass, only with no build
    // having run.
    let finished = gated_dynamic_build(
        io,
        read_through,
        probed_version,
        environment_identity.clone(),
        now,
    )
    .await;
    let finished = match finished {
        Ok(finished) => finished,
        Err(unbuilt) => {
            if dirty_consumed {
                mark_dirty();
            }
            // The cached runtime for this environment, if any, a failed
            // rebuild leaves serving (I5d, Decision 1): a dynamic one, or the
            // prepared one a hand-off was replacing. With none — a cold
            // isolate whose gate failed — the request answers 503 without
            // assembling a runtime (CPU-GATE review M1).
            let keep = previous.filter(|rt| rt.environment_identity == environment_identity);
            let failed = unbuilt.failed_build(kept_is_current);
            return match rebuild_failed(build_guard, timer, unbuilt.into_error(), keep, failed) {
                AfterFailure::Serve(rt) => Ok((rt, failed.served_outcome())),
                AfterFailure::Answer(error) => Err(error),
            };
        }
    };

    // `build_guard` remains alive through `store`, so waiters cannot observe
    // BUILDING=false before the completed runtime is visible.
    if finished.degraded {
        finished.rt.health.set(RuntimeHealth::degraded_since(now));
    }
    let (outcome, stored) = publish_runtime(
        build_guard,
        &finished.rt,
        finished.build_ordinal,
        finished.duration_ms,
        is_cold,
    );
    if stored {
        settle_window(build_guard, timer.now_ms(), finished.degraded);
    }
    Ok((finished.rt, outcome))
}

/// Which build failed ([`rebuild_failed`]): the stale-plan hydrate inside a
/// window, a gate that let nothing be built, or any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedBuild {
    Rebuild,
    Hydrate,
    /// The gate's read failed and nothing was built. `kept_is_current`: the
    /// runtime kept, if any, is of the config the database holds — a current
    /// plan, or a dynamic runtime whose stamp has not moved — and was retried
    /// for its health alone, so it serves on as it was. A hand-off's kept
    /// runtime is of older config, and is marked stale like any a failed
    /// rebuild keeps.
    Gate {
        kept_is_current: bool,
    },
}

impl FailedBuild {
    /// The outcome of a request served the kept runtime after this failure:
    /// [`CacheOutcome::Gated`] when it checked the backend and built nothing.
    fn served_outcome(self) -> CacheOutcome {
        match self {
            Self::Gate { .. } => CacheOutcome::Gated,
            Self::Rebuild | Self::Hydrate => CacheOutcome::Stale,
        }
    }
}

/// How an owner's attempt ended without a finished runtime.
enum Unbuilt {
    /// The gate's read failed, and nothing was built.
    Gated(RuntimeBuildError),
    /// A read before the gate, or the build itself, failed.
    Failed(RuntimeBuildError),
}

impl Unbuilt {
    fn into_error(self) -> RuntimeBuildError {
        match self {
            Self::Gated(error) | Self::Failed(error) => error,
        }
    }

    /// [`FailedBuild`] for a rebuild that ended like this.
    fn failed_build(&self, kept_is_current: bool) -> FailedBuild {
        match self {
            Self::Gated(_) => FailedBuild::Gate { kept_is_current },
            Self::Failed(_) => FailedBuild::Rebuild,
        }
    }
}

/// The prepared path's attempt: the D1 handle, the gate, the hydrate.
///
/// `cold`: nothing is cached for this request's identity. A transient failure
/// of the gate then hydrates anyway, over the failed snapshot: the degraded
/// publish that serves the static site (Decision 1), and the isolate's one
/// build, as any cold start's. Otherwise — a runtime to keep serving, or a
/// permanent failure, which no build survives — a failed gate builds nothing.
async fn gated_hydrate(
    io: &dyn BuildIo,
    plan: &impresspress_core::PreparedRuntimePlan,
    cold: bool,
    environment_identity: String,
    started_at: u64,
) -> Result<Finished, Unbuilt> {
    let db = io
        .open_database(crate::kv_cached_db::CacheMode::default())
        .map_err(Unbuilt::Failed)?;
    let variables = crate::runtime_build::fresh_variables_snapshot();
    if let Err(error) = io.read_variables(&db, &variables).await {
        if !(cold && error.is_unavailable()) {
            return Err(Unbuilt::Gated(error));
        }
    }
    let source = BuildSource {
        db,
        block_settings: crate::runtime_build::plan_block_settings(plan),
        variables,
    };
    io.hydrate_prepared(source, plan, environment_identity, started_at)
        .await
        .map_err(Unbuilt::Failed)
}

/// The dynamic path's attempt: the D1 handle, the gate, `block_settings`, the
/// build.
///
/// The gate is the attempt's first D1 read. A warm retry reads
/// `block_settings` through to D1 (`read_through`, past the KV row cache),
/// so with that read first a database-wide outage failed there, before the
/// gate, and a runtime of the current config was marked stale for a check
/// that built nothing (PR #282 review M1). After a gate that passed, a failed
/// `block_settings` read is a backend failing in part: a failed rebuild.
///
/// Any failed gate builds nothing, a cold isolate's included (CPU-GATE review
/// M1): a dynamic boot reads the WRAP grants first (`GrantSource::Database`,
/// never KV-cached) and cannot publish past a failed read, so a build over a
/// backend that just failed the variables read would spend a whole runtime to
/// reach the same 503.
async fn gated_dynamic_build(
    io: &dyn BuildIo,
    read_through: bool,
    version: String,
    environment_identity: String,
    started_at: u64,
) -> Result<Finished, Unbuilt> {
    let db = io
        .open_database(crate::kv_cached_db::CacheMode {
            read_through,
            bump_on_write: true,
        })
        .map_err(Unbuilt::Failed)?;
    let variables = crate::runtime_build::fresh_variables_snapshot();
    io.read_variables(&db, &variables)
        .await
        .map_err(Unbuilt::Gated)?;
    let block_settings = io.read_block_settings(&db).await.map_err(Unbuilt::Failed)?;
    let source = BuildSource {
        db,
        block_settings,
        variables,
    };
    io.build_dynamic(source, version, environment_identity, started_at)
        .await
        .map_err(Unbuilt::Failed)
}

/// Whether `rt`'s only defect is its variables snapshot remembering a failed
/// read (CPU-GATE review m3): every block initialized, and a later request's
/// read failed — a passing D1 error, or one request's spent call budget —
/// which [`observe_snapshot`] marked degraded. Not stale: config older than
/// the database's needs the rebuild.
///
/// Such a runtime needs one successful read, not a rebuild, so its owner
/// gates on the runtime's own snapshot ([`heal_or_gate`]). A runtime with a
/// block still retrying its `Init` needs a fresh snapshot and the rebuild.
fn heals_in_place(rt: &ReadyRuntime) -> bool {
    rt.health.get().stale_since_ms.is_none()
        && rt.variables_snapshot.remembers_a_failure()
        && rt
            .wafer
            .init_states()
            .values()
            .all(wafer_run::BlockInitState::is_ready)
}

/// The gate, read into `rt`'s own snapshot ([`heals_in_place`]). Answered,
/// the runtime is healed — no longer degraded, the window cleared, the
/// owner's `probe` of the config stamp recorded as a healthy runtime's is —
/// and served; failed, it is a failed gate like any other.
async fn heal_or_gate(
    guard: &BuildGuard,
    io: &dyn BuildIo,
    timer: &dyn WaitTimer,
    rt: Rc<ReadyRuntime>,
    probe: &VersionProbe,
    now: u64,
) -> Result<(Rc<ReadyRuntime>, CacheOutcome), RuntimeBuildError> {
    let read = match io.open_database(crate::kv_cached_db::CacheMode::default()) {
        Ok(db) => io.read_variables(&db, &rt.variables_snapshot).await,
        Err(error) => Err(error),
    };
    match read {
        Ok(()) => {
            let mut health = rt.health.get();
            health.degraded_since_ms = None;
            rt.health.set(health);
            if guard.is_current() {
                clear_window();
            }
            tracing::info!(
                kind = rt.kind_label(),
                "the variables read answered; runtime healed in place, nothing rebuilt"
            );
            // The owner probed the config stamp before healing: record it
            // as a healthy runtime's probe is, so the next request does not
            // probe KV again and an unavailable probe widens the window.
            let outcome = if matches!(probe, VersionProbe::Unavailable) {
                rt.note_probe_failure(now);
                CacheOutcome::ProbeFailed
            } else {
                rt.note_probe_success(now);
                CacheOutcome::ProbedFresh
            };
            Ok((rt, outcome))
        }
        Err(error) => {
            let failed = FailedBuild::Gate {
                kept_is_current: true,
            };
            match rebuild_failed(guard, timer, error, Some(rt), failed) {
                AfterFailure::Serve(rt) => Ok((rt, failed.served_outcome())),
                AfterFailure::Answer(error) => Err(error),
            }
        }
    }
}

/// What a failed build leaves this request with ([`rebuild_failed`]).
enum AfterFailure {
    /// Serve the runtime the build was replacing, marked stale.
    Serve(Rc<ReadyRuntime>),
    /// Answer this error.
    Answer(RuntimeBuildError),
}

/// A failed build, as the slot owner `guard` answers it (I5d), given the
/// cached runtime it was replacing, if any. A superseded owner arms nothing.
///
/// - transient, a runtime cached → it keeps serving, marked stale; the window
///   is armed (or doubled) and this request is served from it. After a gate
///   whose kept runtime is current ([`FailedBuild::Gate`]) it serves on as it
///   was, under [`WindowCause::GateFailed`]: nothing was built, and nothing
///   says its config is old;
/// - transient, nothing cached → `Unavailable` with the window's time;
/// - permanent → a `Fault` for this request (its cause logged by the edge);
///   the window is armed so the isolate builds at most once per window, and a
///   cached runtime keeps serving everyone else inside it (Decision 1).
fn rebuild_failed(
    guard: &BuildGuard,
    timer: &dyn WaitTimer,
    error: RuntimeBuildError,
    keep: Option<Rc<ReadyRuntime>>,
    failed: FailedBuild,
) -> AfterFailure {
    let now = timer.now_ms();
    let transient = error.is_unavailable();
    let cause = match (transient, keep.is_some(), failed) {
        (false, _, _) => WindowCause::Fault,
        (
            true,
            true,
            FailedBuild::Gate {
                kept_is_current: true,
            },
        ) => WindowCause::GateFailed,
        (true, true, _) => WindowCause::RebuildFailed,
        (true, false, FailedBuild::Hydrate) => WindowCause::HydrateFailed,
        (true, false, FailedBuild::Rebuild | FailedBuild::Gate { .. }) => {
            WindowCause::BuildUnavailable
        }
    };
    let window = if guard.is_current() {
        Some(arm_window(now, cause))
    } else {
        armed_window(now)
    };
    match (transient, keep) {
        (true, Some(rt)) => {
            if cause == WindowCause::GateFailed {
                // Kept as it was. The gate's failure is already on the
                // snapshot's own `warn!` and the window's (CPU-GATE review
                // n7: a check logs no third line of its own).
                return AfterFailure::Serve(rt);
            }
            if !matches!(failed, FailedBuild::Gate { .. }) {
                tracing::warn!(error = %error, "rebuild failed transiently; serving the runtime it was replacing, stale");
            }
            let mut health = rt.health.get();
            health.stale_since_ms.get_or_insert(now);
            rt.health.set(health);
            AfterFailure::Serve(rt)
        }
        (true, None) => AfterFailure::Answer(match error {
            RuntimeBuildError::Unavailable { cause, .. } => RuntimeBuildError::Unavailable {
                cause,
                retry_after_secs: window.map_or(1, |w| w.remaining_secs(now)),
            },
            fault => fault,
        }),
        (false, _) => AfterFailure::Answer(error),
    }
}

/// A runtime the slot owner stored: a healthy one clears the retry window, a
/// degraded one arms (or doubles) it, so the isolate retries it by
/// rebuilding. A superseded owner moves nothing.
fn settle_window(guard: &BuildGuard, now: u64, degraded: bool) {
    if !guard.is_current() {
        return;
    }
    if degraded {
        arm_window(now, WindowCause::Degraded);
    } else {
        clear_window();
    }
}

/// Hydrate a packaged immutable plan without D1 settings or WRAP-grant reads.
/// One KV config-version read tags the cold hydration, followed by the same
/// bounded jittered probes used by dynamic runtimes. A local dirty signal or a
/// moved version hands the plan/environment pair to the dynamic path, which
/// bypasses the plan for the isolate only once it has stored a dynamic
/// runtime for the moved config — preserving admin mutation semantics without
/// putting D1 structural reads back on the prepared cold path.
async fn hydrate_prepared_runtime<F, G>(
    inputs: RequestInputs<'_>,
    source: BuildSource,
    plan: &impresspress_core::PreparedRuntimePlan,
    register_blocks: F,
    register_post_build: G,
    environment_identity: String,
    started_at: u64,
) -> Result<Finished, RuntimeBuildError>
where
    F: FnOnce(
        crate::ImpresspressBuilder,
    ) -> Result<crate::ImpresspressBuilder, Box<dyn std::error::Error>>,
    G: FnOnce(
        &mut wafer_run::Wafer,
        Arc<dyn wafer_core::interfaces::storage::service::StorageService>,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    // This invocation's `d1_cost` line names the build its rows were read
    // for, a failed one included.
    inputs
        .calls
        .note_build(crate::database::BuildKind::Prepared);
    let built = crate::runtime_build::build_runtime(
        inputs.env,
        inputs.environment,
        inputs.request_config,
        Some(plan),
        register_blocks,
        register_post_build,
        false,
        source,
    )?;

    // Grants and settings were imported from the verified plan, so this is the
    // one path whose `GrantSource` is `PreInstalled` and whose seed hook is a
    // written no-op — see `impresspress_core::builder::boot_request_runtime`'s
    // `RequestBuild::Prepared`, which `RuntimeKind::Prepared` selects.
    // Everything else is the shared ordering, under this request's services:
    // ConfigSource may still perform per-block reads, and keeping them here
    // prevents cross-request lazy-init waiters.
    finish_runtime(
        built,
        RuntimeKind::Prepared {
            plan_hash: plan.plan_hash.clone(),
            config_generation: plan.config_generation.clone(),
        },
        environment_identity,
        started_at,
    )
    .await
}

async fn get_or_build_prepared(
    io: &dyn BuildIo,
    timer: &dyn WaitTimer,
    plan: Rc<impresspress_core::PreparedRuntimePlan>,
    environment_identity: String,
    now: u64,
    attempt: Attempt,
) -> PassResult {
    let plan_generation = plan.plan_hash.clone();

    if !BUILDING.with(Cell::get) {
        if let Some(rt) = cached()
            .filter(|rt| prepared_runtime_is_warm(rt, &environment_identity, &plan_generation, now))
        {
            return Ok(Pass::Done((rt, CacheOutcome::Hit)));
        }
    }

    // A healthy owner is building and this plan's runtime is cached:
    // stale-while-revalidate, as on the dynamic path.
    if build_slot_active(now) {
        if let Some(rt) = cached().filter(|rt| {
            rt.environment_identity == environment_identity
                && rt.version == plan_generation
                && rt.config_version.is_some()
        }) {
            return Ok(Pass::Done((rt, CacheOutcome::Hit)));
        }
    }

    let admission = admit_cold(attempt, timer, |rt, now| {
        prepared_runtime_is_warm(rt, &environment_identity, &plan_generation, now)
    })
    .await?;
    let build_guard = match admission {
        ColdAdmission::Build(guard) => guard,
        ColdAdmission::Serve { rt, waited_ms } => {
            return Ok(Pass::Done((rt, CacheOutcome::Waited { waited_ms })));
        }
        // Re-entry also covers an owner that handed this plan to the dynamic
        // path while this request waited: the runtime it published is dynamic,
        // which this path's predicate refuses, and re-entry routes to the
        // dynamic path once the bypass is committed.
        ColdAdmission::Reenter => return Ok(Pass::Reenter),
    };
    let owned = as_owner(
        &build_guard,
        timer,
        build_prepared_as_owner(
            &build_guard,
            io,
            timer,
            &plan,
            environment_identity.clone(),
            now,
        ),
    )
    .await?;
    match owned {
        PreparedOwned::Ready(rt, outcome) => Ok(Pass::Done((rt, outcome))),
        PreparedOwned::HandOff { prepared_identity } => {
            // The slot is released and taken again by the dynamic path in the
            // same poll — `get_or_build_dynamic` awaits nothing before
            // `admit_cold`'s synchronous `try_acquire` — so this request's
            // waiters keep waiting through it. Directly, not through
            // `get_or_build_attempt`: the bypass is not committed yet, so that
            // would route straight back here. Under `as_owner`, so a failed
            // hand-off is recorded for its waiters (addendum §4.1, n13).
            drop(build_guard);
            Box::pin(get_or_build_dynamic(
                io,
                timer,
                environment_identity,
                now,
                attempt,
                Some(prepared_identity),
            ))
            .await
        }
    }
}

/// How the prepared owner's turn ended.
enum PreparedOwned {
    Ready(Rc<ReadyRuntime>, CacheOutcome),
    /// The plan no longer describes this deployment: the dynamic path takes
    /// over, and bypasses the plan for this pair only once it stores a
    /// dynamic runtime ([`build_dynamic_as_owner`]).
    HandOff {
        prepared_identity: String,
    },
}

/// The prepared build, run by the request that owns the slot.
async fn build_prepared_as_owner(
    build_guard: &BuildGuard,
    io: &dyn BuildIo,
    timer: &dyn WaitTimer,
    plan: &impresspress_core::PreparedRuntimePlan,
    environment_identity: String,
    now: u64,
) -> Result<PreparedOwned, RuntimeBuildError> {
    let plan_generation = &plan.plan_hash;
    let prepared_identity = prepared_cache_identity(plan_generation, &environment_identity);
    let mut known_probe = None;
    let previous = cached().filter(|rt| {
        rt.environment_identity == environment_identity
            && rt.version == *plan_generation
            && rt.config_version.is_some()
    });
    if let Some(rt) = previous.clone() {
        let cached_config_version = rt.config_version.clone().unwrap_or_default();
        let dirty = take_dirty();
        let healthy = rt.health.get().is_healthy();
        if !dirty && healthy && now < rt.probe_deadline_ms.get() {
            return Ok(PreparedOwned::Ready(rt, CacheOutcome::Hit));
        }

        let probe = match io.probe_version().await {
            Ok(probe) => probe,
            Err(e) => {
                // `dirty` was consumed above; losing it here would strand the
                // isolate on pre-write state for a full probe window.
                if dirty {
                    mark_dirty();
                }
                return Err(e);
            }
        };
        if !dirty
            && !healthy
            && rt.health.get().stale_since_ms.is_some()
            && matches!(probe, VersionProbe::Unavailable)
        {
            // This isolate knows the plan is older than the database's config
            // (a failed rebuild kept it, or U-B1's hydrate served it), and with
            // no local write pending an unavailable probe gives no reason to
            // retry it now: keep serving it stale and retry after a doubled
            // window — never re-hydrate and republish it as current (#275
            // review m1). A local write (`dirty`) does give one: KV is not
            // needed to know this isolate's config moved, so it falls through
            // to the hand-off below, which rebuilds from D1 and keeps `DIRTY`
            // set for it (#275 re-review m6).
            rt.note_probe_failure(now);
            if build_guard.is_current() {
                arm_window(now, WindowCause::RebuildFailed);
            }
            return Ok(PreparedOwned::Ready(rt, CacheOutcome::Stale));
        }
        if prepared_probe_requires_fallback(dirty, &cached_config_version, &probe) {
            tracing::info!(
                plan_hash = %plan_generation,
                old_config_version = %cached_config_version,
                new_config_version = %probe.observed(),
                dirty,
                "mutable admin state changed; handing the packaged plan to the dynamic path"
            );
            // Force the dynamic path to replace the currently cached prepared
            // runtime even when an eventually-consistent KV read still returns
            // the old stamp after a local write. It survives a failed attempt:
            // the dynamic owner re-marks a flag it consumed (I5d).
            mark_dirty();
            return Ok(PreparedOwned::HandOff { prepared_identity });
        }
        if healthy {
            if matches!(probe, VersionProbe::Unavailable) {
                rt.note_probe_failure(now);
                return Ok(PreparedOwned::Ready(rt, CacheOutcome::ProbeFailed));
            }
            rt.note_probe_success(now);
            return Ok(PreparedOwned::Ready(rt, CacheOutcome::ProbedFresh));
        }
        // A runtime of a current plan whose only defect is a failed
        // variables read: one read heals it (CPU-GATE review m3).
        if !dirty && heals_in_place(&rt) {
            let (rt, outcome) = heal_or_gate(build_guard, io, timer, rt, &probe, now).await?;
            return Ok(PreparedOwned::Ready(rt, outcome));
        }
        // A degraded runtime of a current plan: hydrate it again, the retry
        // the window was armed for, once the gate's read answers.
        known_probe = Some(probe);
    }
    let is_cold = previous.is_none();
    // A plan/Worker identity change supersedes dirty state belonging to the
    // prior runtime. The new plan is tagged with the current KV generation.
    let _ = take_dirty();
    let probe = match known_probe {
        Some(probe) => probe,
        None => io.probe_version().await?,
    };
    if !prepared_generation_matches(&plan.config_generation, &probe) {
        tracing::info!(
            plan_hash = %plan_generation,
            plan_config_generation = %plan.config_generation,
            observed_config_generation = %probe.observed(),
            "prepared plan generation is stale; handing it to the dynamic path"
        );
        return Ok(PreparedOwned::HandOff { prepared_identity });
    }

    // Gated: with a runtime of this plan cached, a failed read builds
    // nothing and keeps it as it was (`kept_is_current`: the probe above
    // found the plan current); a cold isolate builds anyway, degraded.
    let finished = match gated_hydrate(io, plan, is_cold, environment_identity, now).await {
        Ok(finished) => finished,
        Err(unbuilt) => {
            let failed = unbuilt.failed_build(true);
            return match rebuild_failed(build_guard, timer, unbuilt.into_error(), previous, failed)
            {
                AfterFailure::Serve(rt) => Ok(PreparedOwned::Ready(rt, failed.served_outcome())),
                AfterFailure::Answer(error) => Err(error),
            };
        }
    };
    if finished.degraded {
        finished.rt.health.set(RuntimeHealth::degraded_since(now));
    }
    // Same publish-and-report tail as the dynamic path, including the reason a
    // lost publish is served request-locally rather than turned into a 503.
    let (outcome, stored) = publish_runtime(
        build_guard,
        &finished.rt,
        finished.build_ordinal,
        finished.duration_ms,
        is_cold,
    );
    if stored {
        settle_window(build_guard, timer.now_ms(), finished.degraded);
    }
    Ok(PreparedOwned::Ready(finished.rt, outcome))
}

/// What `/health` reports beyond the runtime's slots (D1 outage plan I6),
/// read from this isolate: the cached runtime's [`RuntimeHealth`] and
/// [`RetryStreaks`], and the retry window. The Cloudflare entry registers it
/// as the isolate's [`impresspress_core::runtime_health`] source. Reads
/// isolate state only.
///
/// The cached runtime is the one a request is served from, but for a
/// superseded owner's runtime, which serves its own request only.
pub(crate) fn platform_health() -> PlatformHealth {
    let episode = RETRY_WINDOW.with(Cell::get).map(|window| RetryEpisode {
        since_ms: window.armed_at_ms,
        retry_at_ms: window.retry_at_ms,
        retries: window.failures,
        failed: window.cause == WindowCause::Fault,
    });
    let Some(rt) = cached() else {
        return PlatformHealth {
            episode,
            ..PlatformHealth::default()
        };
    };
    let health = rt.health.get();
    PlatformHealth {
        episode,
        degraded_since_ms: health.degraded_since_ms,
        stale_since_ms: health.stale_since_ms,
        retry_streaks: rt.retry_streaks.get().cloned().unwrap_or_default(),
    }
}

/// Before this request is dispatched, on the runtime it will be dispatched
/// to: a variables snapshot that a previous request's dispatch left
/// remembering a failed read answers every stored-config read `Unavailable`
/// until the window's owner reads it again — under `RetryMode::Rebuild`
/// nothing else retries it: in place when that is the runtime's only defect
/// ([`heals_in_place`]), else by rebuilding — so the runtime is marked
/// degraded and the retry window armed, if it is not already (I5a). The
/// failure is noticed one request late, by the first request after it. Reads
/// isolate state only.
fn observe_snapshot(rt: &ReadyRuntime, now: u64) {
    if !rt.variables_snapshot.remembers_a_failure() {
        return;
    }
    let mut health = rt.health.get();
    if health.degraded_since_ms.is_none() {
        health.degraded_since_ms = Some(now);
        rt.health.set(health);
    }
    // Armed once: a window already armed (or elapsed and awaiting the
    // rebuild the next request runs) is the same retry.
    if RETRY_WINDOW.with(Cell::get).is_none() {
        arm_window(now, WindowCause::Degraded);
    }
}

#[cfg(test)]
pub(crate) mod tests {
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

    /// A cold isolate: no slot owner, no runtime, no recorded failure.
    pub(crate) fn reset_isolate() {
        reset_build_slot();
        RUNTIME.with(IsolateCell::clear);
        BUILD_LAST_FAILURE.with(|value| value.set(None));
        RETRY_WINDOW.with(|value| value.set(None));
        PREPARED_BYPASS.with(IsolateCell::clear);
    }

    /// A Worker `Env` with a `CONFIG_CACHE` KV namespace whose every call
    /// rejects (so the version probe reads as unavailable and nothing is
    /// written) and no D1 binding, so a build gets past the probe and fails
    /// at its first D1 handle — before any statement is sent.
    pub(crate) fn env_whose_build_fails_at_d1() -> worker::Env {
        let env = js_sys::Function::new_no_args(
            "const down = () => Promise.reject(new Error('kv unavailable in this test'));
             return { CONFIG_CACHE: {
                 get: down, getWithMetadata: down, put: down, list: down, delete: down,
             } };",
        )
        .call0(&wasm_bindgen::JsValue::NULL)
        .expect("the fake env builds");
        wasm_bindgen::JsCast::unchecked_into::<worker::Env>(env)
    }

    /// **An attempt that assembles nothing is `build=gate`**: a cold dynamic
    /// attempt that fails at its D1 binding, before any read, sent nothing
    /// and built nothing, so its `d1_cost` line must not count it among the
    /// builds an outage costs. Driven through `get_or_build`, the request
    /// path's one entry.
    #[wasm_bindgen_test]
    async fn an_attempt_that_assembles_nothing_notes_the_gate() {
        reset_isolate();
        let env = env_whose_build_fails_at_d1();
        let environment = crate::environment::CfEnvironment::capture(&env);
        let calls = crate::database::D1CallCount::new();
        let built = get_or_build(
            &env,
            &environment,
            &calls,
            &std::collections::HashMap::new(),
            Ok,
            |_, _| Ok(()),
        )
        .await;
        reset_isolate();

        assert!(built.is_err(), "the attempt failed at its D1 binding");
        let cost = calls.cost();
        assert_eq!(cost.built, Some(crate::database::BuildKind::Gate));
        assert_eq!(cost.statements, 0, "it failed before sending anything");
    }

    /// **A dynamic build names itself in its invocation's cost**, failed or
    /// not: the `d1_cost` line's `build=dynamic` comes from here, after the
    /// attempt's gate noted itself.
    #[wasm_bindgen_test]
    async fn a_dynamic_build_notes_its_kind_on_the_invocations_count() {
        let env = env_whose_build_fails_at_d1();
        let environment = crate::environment::CfEnvironment::capture(&env);
        let calls = crate::database::D1CallCount::new();
        let request_config = std::collections::HashMap::new();
        let io = WorkerBuildIo {
            inputs: RequestInputs {
                env: &env,
                environment: &environment,
                calls: &calls,
                request_config: &request_config,
            },
            hooks: Cell::new(Some((
                Ok::<crate::ImpresspressBuilder, Box<dyn std::error::Error>>,
                |_: &mut wafer_run::Wafer, _| Ok(()),
            ))),
        };
        calls.note_build(crate::database::BuildKind::Gate);
        // No D1 binding: the source's handle is a fake, and the build fails
        // at the next binding it needs.
        let source = BuildSource {
            db: Arc::new(crate::test_fakes::VariablesDb::default()),
            block_settings: impresspress_core::features::BlockSettings::default(),
            variables: crate::runtime_build::fresh_variables_snapshot(),
        };
        let built = io
            .build_dynamic(source, "v".to_string(), "env".to_string(), 0)
            .await;

        assert!(built.is_err(), "the build failed at a binding");
        let cost = calls.cost();
        assert_eq!(cost.built, Some(crate::database::BuildKind::Dynamic));
        assert_eq!(cost.statements, 0);
    }

    /// **A prepared hydration names itself in its invocation's cost**, failed
    /// or not: the line's `build=prepared` comes from here.
    #[wasm_bindgen_test]
    async fn a_prepared_hydration_notes_its_kind_on_the_invocations_count() {
        let env = env_whose_build_fails_at_d1();
        let environment = crate::environment::CfEnvironment::capture(&env);
        let calls = crate::database::D1CallCount::new();
        let request_config = std::collections::HashMap::new();
        let plan = impresspress_core::PreparedRuntimePlan::new_with_config_generation(
            "example",
            format!("sha256:{}", "a".repeat(64)),
            "1".repeat(32),
            impresspress_core::WaferLockIdentity::absent(),
            impresspress_core::PreparedReleaseAssets::absent(),
            impresspress_core::PreparedRuntimeStructure {
                application_blocks: Vec::new(),
                routes: Vec::new(),
                built_in_route_count: 0,
                block_settings: Default::default(),
                block_configs: Default::default(),
                final_block_configs: Default::default(),
                wrap_grants: Vec::new(),
                deployment_wrap_grants: Vec::new(),
            },
        )
        .expect("a plan");
        // No D1 binding: the source's handle is a fake, and the build fails
        // at the next binding it needs.
        let source = BuildSource {
            db: Arc::new(crate::test_fakes::VariablesDb::default()),
            block_settings: crate::runtime_build::plan_block_settings(&plan),
            variables: crate::runtime_build::fresh_variables_snapshot(),
        };
        let hydrated = hydrate_prepared_runtime(
            RequestInputs {
                env: &env,
                environment: &environment,
                calls: &calls,
                request_config: &request_config,
            },
            source,
            &plan,
            Ok,
            |_, _| Ok(()),
            "env".to_string(),
            0,
        )
        .await;

        assert!(hydrated.is_err(), "the build failed at a binding");
        let cost = calls.cost();
        assert_eq!(cost.built, Some(crate::database::BuildKind::Prepared));
        assert_eq!(cost.statements, 0);
    }

    /// A bare runtime, enough to publish: `store` only moves the `Rc`.
    fn ready_runtime() -> Rc<ReadyRuntime> {
        ready_runtime_until(0)
    }

    /// A bare dynamic runtime for `env-test` whose probe window ends at
    /// `probe_deadline_ms`.
    fn ready_runtime_until(probe_deadline_ms: u64) -> Rc<ReadyRuntime> {
        Rc::new(ReadyRuntime {
            wafer: wafer_run::Wafer::new(std::sync::Arc::new(
                wafer_run::StaticConfigSource::default(),
            ))
            .expect("a bare runtime"),
            variables_snapshot: std::sync::Arc::default(),
            version: "v-test".to_string(),
            config_version: None,
            environment_identity: "env-test".to_string(),
            probe_deadline_ms: Cell::new(probe_deadline_ms),
            probe_failures: Cell::new(0),
            health: Cell::new(RuntimeHealth::default()),
            retry_streaks: OnceCell::new(),
        })
    }

    /// Pending once, then ready: one turn of the executor, standing in for a
    /// build's I/O await.
    struct YieldOnce(bool);

    impl std::future::Future for YieldOnce {
        type Output = ();

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<()> {
            if self.0 {
                std::task::Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }
    }

    /// A clock the test owns. `sleep` advances it by the requested time and
    /// yields one turn, so every request in a `join_all` is polled between
    /// ticks. A wait that outlives every bound fails the test instead of
    /// hanging it.
    struct FakeTimer {
        now: Cell<u64>,
        sleeps: Cell<u32>,
    }

    impl FakeTimer {
        /// Far more ticks than any bounded wait in these tests takes.
        const MAX_SLEEPS: u32 = 10_000;

        fn starting_at(now: u64) -> Self {
            Self {
                now: Cell::new(now),
                sleeps: Cell::new(0),
            }
        }
    }

    impl WaitTimer for FakeTimer {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }

        fn sleep(&self, ms: u64) -> futures::future::LocalBoxFuture<'static, ()> {
            let sleeps = self.sleeps.get() + 1;
            self.sleeps.set(sleeps);
            assert!(
                sleeps <= Self::MAX_SLEEPS,
                "a waiting request outlived every bound on its wait"
            );
            self.now.set(self.now.get() + ms);
            Box::pin(YieldOnce(false))
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Served {
        Built,
        Waited,
        BuildFailed,
        Refused(RuntimeBuildBusy),
    }

    /// One cold request on the dynamic path. Its passes are sequenced by the
    /// production [`with_one_reentry`] — the loop `get_or_build` runs — and
    /// each pass goes through the production admission ([`admit_cold`]),
    /// owner wrapper ([`as_owner`]) and publish ([`publish_runtime`]), with a
    /// fake build of `build_ticks` I/O turns.
    async fn cold_request(
        timer: &FakeTimer,
        builds: &Cell<u32>,
        build_ticks: u32,
        build_fails: bool,
    ) -> Served {
        let served: Result<Served, RuntimeBuildBusy> = with_one_reentry(|attempt| async move {
            let admission = admit_cold(attempt, timer, |rt, now| {
                dynamic_runtime_is_warm(rt, "env-test", now)
            })
            .await?;
            let guard = match admission {
                ColdAdmission::Serve { .. } => return Ok(Pass::Done(Served::Waited)),
                ColdAdmission::Reenter => return Ok(Pass::Reenter),
                ColdAdmission::Build(guard) => guard,
            };
            let built = as_owner(&guard, timer, async {
                builds.set(builds.get() + 1);
                for _ in 0..build_ticks {
                    YieldOnce(false).await;
                }
                if build_fails {
                    Err("admin Init failed")
                } else {
                    Ok(ready_runtime_until(
                        timer.now_ms() + PROBE_INTERVAL_FLOOR_MS,
                    ))
                }
            })
            .await;
            Ok(Pass::Done(match built {
                Ok(rt) => {
                    publish_runtime(&guard, &rt, builds.get(), 0, true);
                    Served::Built
                }
                Err(_) => Served::BuildFailed,
            }))
        })
        .await;
        match served {
            Ok(served) => served,
            Err(busy) => Served::Refused(busy),
        }
    }

    /// A prepared owner that finds its plan stale releases the slot and the
    /// dynamic path takes it again in the same poll
    /// (`get_or_build_prepared` → `PreparedOwned::HandOff`). This owner
    /// does exactly that between its two halves, then publishes.
    async fn handing_off_owner(timer: &FakeTimer, builds: &Cell<u32>) -> Served {
        let Ok(ColdAdmission::Build(prepared)) =
            admit_cold(Attempt::First, timer, |_, _| false).await
        else {
            panic!("the first request owns the slot");
        };
        for _ in 0..3 {
            YieldOnce(false).await;
        }
        drop(prepared);
        let dynamic = BuildGuard::try_acquire(timer.now_ms())
            .expect("the released slot is free in the same poll");
        builds.set(builds.get() + 1);
        for _ in 0..3 {
            YieldOnce(false).await;
        }
        let rt = ready_runtime_until(timer.now_ms() + PROBE_INTERVAL_FLOOR_MS);
        publish_runtime(&dynamic, &rt, builds.get(), 0, true);
        Served::Built
    }

    fn count(outcomes: &[Served], wanted: Served) -> usize {
        outcomes.iter().filter(|served| **served == wanted).count()
    }

    /// k concurrent cold requests in one isolate build ONE runtime. Each
    /// runtime is 13–20 MB of Wasm memory; eight built side by side are what
    /// took a 128 MB isolate down in the 2026-10-14 deploy smoke.
    #[wasm_bindgen_test]
    async fn concurrent_cold_requests_share_one_build() {
        reset_isolate();
        let timer = FakeTimer::starting_at(1_000);
        let builds = Cell::new(0);

        let outcomes =
            futures::future::join_all((0..8).map(|_| cold_request(&timer, &builds, 3, false)))
                .await;

        assert_eq!(builds.get(), 1, "one build for the isolate: {outcomes:?}");
        assert_eq!(count(&outcomes, Served::Built), 1, "{outcomes:?}");
        assert_eq!(
            count(&outcomes, Served::Waited),
            7,
            "every other request serves the owner's runtime: {outcomes:?}"
        );
        assert!(cached().is_some(), "the owner's runtime is the isolate's");
        assert!(!BUILDING.with(Cell::get));
    }

    /// A failed owner costs one build, not one per waiter: its waiters answer
    /// a retryable 503 and the failure is recorded once. The next request
    /// that is not a waiter is the retry, and an old failure does not refuse
    /// the waiters of a later owner.
    #[wasm_bindgen_test]
    async fn a_failed_owner_answers_its_waiters_503_and_is_retried_once() {
        reset_isolate();
        let timer = FakeTimer::starting_at(1_000);
        let builds = Cell::new(0);

        let outcomes =
            futures::future::join_all((0..8).map(|_| cold_request(&timer, &builds, 3, true))).await;

        assert_eq!(
            builds.get(),
            1,
            "the waiters must not rebuild: {outcomes:?}"
        );
        assert_eq!(count(&outcomes, Served::BuildFailed), 1, "{outcomes:?}");
        assert_eq!(
            count(&outcomes, Served::Refused(RuntimeBuildBusy::LeaderFailed)),
            7,
            "{outcomes:?}"
        );
        let failure = last_build_failure().expect("the owner's failure is recorded");
        assert_eq!(failure.owner, 1, "recorded by the one owner");
        assert!(cached().is_none());
        assert!(!BUILDING.with(Cell::get));

        let retry =
            futures::future::join_all((0..2).map(|_| cold_request(&timer, &builds, 3, false)))
                .await;
        assert_eq!(builds.get(), 2, "one retry: {retry:?}");
        assert_eq!(
            retry,
            vec![Served::Built, Served::Waited],
            "the earlier failure does not refuse a later owner's waiter"
        );
    }

    /// A wait is bounded by the build leases, a request re-enters once, and a
    /// re-entered request that finds the slot owned again answers the
    /// retryable 503 at once — it never waits a second time. Driven through
    /// [`with_one_reentry`], so a production loop whose second pass waited
    /// again would hold the request for another full lease.
    #[wasm_bindgen_test]
    async fn a_wait_is_bounded_and_a_second_miss_is_a_retryable_503() {
        reset_isolate();
        let timer = FakeTimer::starting_at(1_000);
        let builds = Cell::new(0);
        // A live owner that never publishes: its request is suspended, not
        // stopped, so its liveness token protects it until the hard lease.
        let stuck = BuildGuard::try_acquire(timer.now_ms()).unwrap();
        let waiter_done = Cell::new(false);
        // Another request that takes the slot the moment the lease frees it
        // (polled first each turn), and holds it while the waiter re-enters.
        let reclaimer = async {
            loop {
                timer.sleep(BUILD_WAIT_POLL_MS).await;
                if let Some(guard) = BuildGuard::try_acquire(timer.now_ms()) {
                    while !waiter_done.get() {
                        timer.sleep(BUILD_WAIT_POLL_MS).await;
                    }
                    drop(guard);
                    return;
                }
            }
        };
        let waiter = async {
            let served = cold_request(&timer, &builds, 1, false).await;
            waiter_done.set(true);
            served
        };

        let ((), served) = futures::future::join(reclaimer, waiter).await;

        assert_eq!(served, Served::Refused(RuntimeBuildBusy::SlotBusy));
        assert_eq!(builds.get(), 0);
        let elapsed = timer.now_ms() - 1_000;
        assert!(
            elapsed < BUILD_HARD_LEASE_MS + 4 * BUILD_WAIT_POLL_MS,
            "one bounded wait, then 503 — not a second wait ({elapsed} ms)"
        );
        assert_eq!(
            RuntimeBuildBusy::SlotBusy.retry_after_secs(),
            1,
            "lib.rs sends this as Retry-After"
        );
        drop(stuck);
    }

    /// A prepared owner that bypasses its stale plan hands the slot to the
    /// dynamic build in the same poll. Its waiters must wait through the
    /// handoff and be served — a wait that ended when the owner token changed
    /// would re-enter into the dynamic owner's build and answer 503, every
    /// waiter in the post-deploy window.
    #[wasm_bindgen_test]
    async fn waiters_wait_through_the_prepared_to_dynamic_handoff() {
        reset_isolate();
        let timer = FakeTimer::starting_at(1_000);
        let builds = Cell::new(0);

        let (owner, waiters) = futures::future::join(
            handing_off_owner(&timer, &builds),
            futures::future::join_all((0..7).map(|_| cold_request(&timer, &builds, 3, false))),
        )
        .await;

        assert_eq!(owner, Served::Built);
        assert_eq!(builds.get(), 1, "{waiters:?}");
        assert_eq!(
            count(&waiters, Served::Waited),
            7,
            "every waiter is served by the dynamic owner: {waiters:?}"
        );
    }

    /// A hard stop runs no destructor, so the owner's guard and its liveness
    /// token stay in the heap. Its waiter rides the existing lease reclaim:
    /// the wait ends at the hard lease, the re-entry reclaims the slot, and
    /// the runtime it builds becomes the isolate's. An owner whose token is
    /// gone is reclaimed at the soft lease.
    #[wasm_bindgen_test]
    async fn a_hard_stopped_owner_is_reclaimed_by_its_waiter() {
        reset_isolate();
        let timer = FakeTimer::starting_at(1_000);
        let builds = Cell::new(0);
        std::mem::forget(BuildGuard::try_acquire(timer.now_ms()).unwrap());
        let stopped_at = timer.now_ms();

        let served = cold_request(&timer, &builds, 1, false).await;

        assert_eq!(served, Served::Built);
        assert_eq!(builds.get(), 1);
        assert!(
            timer.now_ms() - stopped_at >= BUILD_HARD_LEASE_MS,
            "a live token is reclaimed only after the hard lease"
        );
        assert!(
            cached().is_some(),
            "the reclaimer's runtime is the isolate's"
        );
        assert!(!BUILDING.with(Cell::get), "and it released the slot");

        // The same, with the token gone: the soft lease applies.
        reset_isolate();
        let timer = FakeTimer::starting_at(1_000);
        let builds = Cell::new(0);
        std::mem::forget(BuildGuard::try_acquire(timer.now_ms()).unwrap());
        BUILD_LIVENESS.with(IsolateCell::clear);

        let served = cold_request(&timer, &builds, 1, false).await;

        assert_eq!(served, Served::Built);
        let reclaimed_after = timer.now_ms() - 1_000;
        assert!(
            (BUILD_LEASE_MS..BUILD_HARD_LEASE_MS).contains(&reclaimed_after),
            "an orphaned slot is reclaimed at the soft lease, after {reclaimed_after} ms"
        );
        assert!(cached().is_some());
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

    /// Stale-while-revalidate serves the last dynamic runtime only while a
    /// build is active, and never a runtime built for another environment or
    /// a prepared one. With nothing servable it answers `None`, which sends a
    /// cold request to [`admit_cold`].
    #[wasm_bindgen_test]
    fn stale_while_revalidate_serves_only_a_matching_dynamic_runtime_while_building() {
        reset_isolate();
        store(ready_runtime_until(0));
        assert!(
            dynamic_runtime_while_building(100, "env-test").is_none(),
            "with no builder the caller takes the slot itself"
        );

        let owner = BuildGuard::try_acquire(100).unwrap();
        assert!(dynamic_runtime_while_building(100, "env-test").is_some());
        assert!(
            dynamic_runtime_while_building(100, "env-other").is_none(),
            "a runtime built for another environment is never served"
        );
        drop(owner);
        RUNTIME.with(IsolateCell::clear);
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
    /// so its message is what names the path that failed. It must use the
    /// same word as the `kind` field a finished runtime logs
    /// (`ReadyRuntime::kind_label`, which switches on the tag's
    /// `config_version`), so one filter finds a path's builds and its
    /// failures.
    #[wasm_bindgen_test]
    fn a_boot_failure_names_its_path_as_the_kind_field_does() {
        let dynamic = RuntimeKind::Dynamic {
            version: "cfg-v1".to_string(),
        };
        let prepared = RuntimeKind::Prepared {
            plan_hash: "plan-hash".to_string(),
            config_generation: "cfg-v1".to_string(),
        };

        assert_eq!(dynamic.lifecycle(), "dynamic");
        assert_eq!(prepared.lifecycle(), "prepared");
        assert!(dynamic.tag("env".to_string(), 0).config_version.is_none());
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

    // -----------------------------------------------------------------------
    // The retry window, the bypass rule and the stale-plan hydrate (D1 outage
    // plan I5, addendum §4), through `get_or_build_with` over a fake
    // `BuildIo`: every decision is production's; only the I/O is faked.
    // -----------------------------------------------------------------------

    /// For assertion messages over a served `Result`: what tells two runtimes
    /// apart, without the `Wafer`.
    impl std::fmt::Debug for ReadyRuntime {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ReadyRuntime")
                .field("version", &self.version)
                .field("config_version", &self.config_version)
                .field("environment_identity", &self.environment_identity)
                .field("health", &self.health.get())
                .finish_non_exhaustive()
        }
    }

    const IDENTITY: &str = "env-outage";
    /// The KV stamp the plan was sealed at (a stamp is 32 lowercase hex).
    const PLAN_GENERATION: &str = "11111111111111111111111111111111";
    /// The stamp after an admin save since the deploy.
    const MOVED_GENERATION: &str = "22222222222222222222222222222222";

    /// How a fake build ends.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Ends {
        Healthy,
        Degraded,
        Unavailable,
        Fault,
    }

    /// A [`BuildIo`] whose probe answers `stamp`, whose dynamic build and
    /// hydrate end as told, and which counts what it was asked to do. Its
    /// gate is the production one (`runtime_build::read_variables`) over a
    /// [`VariablesDb`](crate::test_fakes::VariablesDb) whose table answers as
    /// `variables` says (read by default).
    struct FakeIo {
        plan: Option<Rc<impresspress_core::PreparedRuntimePlan>>,
        stamp: std::cell::RefCell<String>,
        /// The KV probe fails (`VersionProbe::Unavailable`).
        probe_unavailable: Cell<bool>,
        dynamic: Cell<Ends>,
        hydrate: Cell<Ends>,
        /// I/O turns each build takes, so concurrent requests interleave.
        build_ticks: Cell<u32>,
        /// Let a newer owner take the slot while the dynamic build runs, so
        /// this owner's publish is superseded.
        superseded_by: std::cell::RefCell<Option<BuildGuard>>,
        supersede: Cell<bool>,
        probes: Cell<u32>,
        builds: Cell<u32>,
        hydrates: Cell<u32>,
        /// Each runtime built carries [`health_wafer`]'s real slots rather
        /// than none: a degraded one with `test/resets` retrying.
        real_slots: Cell<bool>,
        /// How the variables table answers the gate (`None`: it reads).
        variables: Rc<Cell<Option<crate::test_fakes::VariablesAnswer>>>,
        /// Reads of the variables table that reached the database.
        variables_reads: Rc<Cell<u32>>,
        /// Gate reads asked for, and `block_settings` reads.
        gates: Cell<u32>,
        settings_reads: Cell<u32>,
        /// The snapshot the last gate read into, and the one the last build
        /// was handed.
        gated: std::cell::RefCell<Option<Arc<VariablesSnapshot>>>,
        built_with: std::cell::RefCell<Option<Arc<VariablesSnapshot>>>,
    }

    impl FakeIo {
        fn new(plan: Option<Rc<impresspress_core::PreparedRuntimePlan>>) -> Self {
            Self {
                plan,
                stamp: std::cell::RefCell::new(PLAN_GENERATION.to_string()),
                probe_unavailable: Cell::new(false),
                dynamic: Cell::new(Ends::Healthy),
                hydrate: Cell::new(Ends::Healthy),
                build_ticks: Cell::new(0),
                superseded_by: std::cell::RefCell::new(None),
                supersede: Cell::new(false),
                probes: Cell::new(0),
                builds: Cell::new(0),
                hydrates: Cell::new(0),
                real_slots: Cell::new(false),
                variables: Rc::default(),
                variables_reads: Rc::default(),
                gates: Cell::new(0),
                settings_reads: Cell::new(0),
                gated: std::cell::RefCell::new(None),
                built_with: std::cell::RefCell::new(None),
            }
        }

        /// Answer the gate's read as `answer` from now on (`None`: reads).
        fn variables_answer(&self, answer: Option<crate::test_fakes::VariablesAnswer>) {
            self.variables.set(answer);
        }

        /// Whether the last build was handed the snapshot the last gate read
        /// into: the same `Arc`, not an equal one.
        fn built_over_the_gates_snapshot(&self) -> bool {
            match (&*self.gated.borrow(), &*self.built_with.borrow()) {
                (Some(gated), Some(built)) => Arc::ptr_eq(gated, built),
                _ => false,
            }
        }

        async fn finish(
            &self,
            ends: Ends,
            version: String,
            config_version: Option<String>,
            variables: Arc<VariablesSnapshot>,
        ) -> Result<Finished, RuntimeBuildError> {
            *self.built_with.borrow_mut() = Some(variables.clone());
            match ends {
                Ends::Unavailable => Err(RuntimeBuildError::classified(
                    true,
                    "D1_ERROR: Network connection lost.".to_string(),
                )),
                Ends::Fault => Err(RuntimeBuildError::classified(
                    false,
                    "D1_ERROR: no such column".to_string(),
                )),
                Ends::Healthy | Ends::Degraded => {
                    let mut rt = if self.real_slots.get() {
                        runtime_of(
                            health_wafer(ends == Ends::Degraded).await,
                            IDENTITY,
                            &version,
                            config_version,
                            u64::MAX,
                        )
                    } else {
                        runtime(IDENTITY, &version, config_version, u64::MAX)
                    };
                    // The runtime keeps the snapshot its build was handed, as
                    // `finish_runtime` does.
                    Rc::get_mut(&mut rt)
                        .expect("a runtime just built has one handle")
                        .variables_snapshot = variables;
                    Ok(Finished {
                        rt,
                        build_ordinal: 1,
                        duration_ms: 0,
                        degraded: ends == Ends::Degraded,
                    })
                }
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl BuildIo for FakeIo {
        fn environment_identity(&self) -> String {
            IDENTITY.to_string()
        }

        fn packaged_plan(
            &self,
        ) -> Result<Option<Rc<impresspress_core::PreparedRuntimePlan>>, RuntimeBuildError> {
            Ok(self.plan.clone())
        }

        async fn probe_version(&self) -> Result<VersionProbe, RuntimeBuildError> {
            self.probes.set(self.probes.get() + 1);
            if self.probe_unavailable.get() {
                return Ok(VersionProbe::Unavailable);
            }
            Ok(VersionProbe::Stamped(self.stamp.borrow().clone()))
        }

        fn open_database(
            &self,
            _cache_mode: crate::kv_cached_db::CacheMode,
        ) -> Result<Arc<dyn DatabaseService>, RuntimeBuildError> {
            Ok(Arc::new(crate::test_fakes::VariablesDb::new(
                self.variables.clone(),
                self.variables_reads.clone(),
            )))
        }

        async fn read_block_settings(
            &self,
            _db: &Arc<dyn DatabaseService>,
        ) -> Result<impresspress_core::features::BlockSettings, RuntimeBuildError> {
            self.settings_reads.set(self.settings_reads.get() + 1);
            // The same database: an outage fails this read as it fails the
            // variables read (a warm retry reads it through to D1).
            match self.variables.get() {
                Some(crate::test_fakes::VariablesAnswer::Unavailable) => Err(
                    RuntimeBuildError::classified(true, "read block_settings: D1 down".into()),
                ),
                Some(crate::test_fakes::VariablesAnswer::Fault) => Err(
                    RuntimeBuildError::classified(false, "read block_settings: fault".into()),
                ),
                _ => Ok(impresspress_core::features::BlockSettings::default()),
            }
        }

        async fn read_variables(
            &self,
            db: &Arc<dyn DatabaseService>,
            variables: &Arc<VariablesSnapshot>,
        ) -> Result<(), RuntimeBuildError> {
            self.gates.set(self.gates.get() + 1);
            *self.gated.borrow_mut() = Some(variables.clone());
            crate::runtime_build::read_variables(db, variables).await
        }

        async fn build_dynamic(
            &self,
            source: BuildSource,
            version: String,
            _environment_identity: String,
            started_at: u64,
        ) -> Result<Finished, RuntimeBuildError> {
            self.builds.set(self.builds.get() + 1);
            for _ in 0..self.build_ticks.get() {
                YieldOnce(false).await;
            }
            if self.supersede.get() {
                // A waiter reclaims the slot past the hard lease.
                *self.superseded_by.borrow_mut() =
                    BuildGuard::try_acquire(started_at + BUILD_HARD_LEASE_MS + 1);
            }
            self.finish(self.dynamic.get(), version, None, source.variables)
                .await
        }

        async fn hydrate_prepared(
            &self,
            source: BuildSource,
            plan: &impresspress_core::PreparedRuntimePlan,
            _environment_identity: String,
            _started_at: u64,
        ) -> Result<Finished, RuntimeBuildError> {
            self.hydrates.set(self.hydrates.get() + 1);
            for _ in 0..self.build_ticks.get() {
                YieldOnce(false).await;
            }
            self.finish(
                self.hydrate.get(),
                plan.plan_hash.clone(),
                Some(plan.config_generation.clone()),
                source.variables,
            )
            .await
        }
    }

    /// A bare runtime of `identity`, tagged as given.
    fn runtime(
        identity: &str,
        version: &str,
        config_version: Option<String>,
        probe_deadline_ms: u64,
    ) -> Rc<ReadyRuntime> {
        runtime_of(
            wafer_run::Wafer::new(std::sync::Arc::new(wafer_run::StaticConfigSource::default()))
                .expect("a bare runtime"),
            identity,
            version,
            config_version,
            probe_deadline_ms,
        )
    }

    /// A runtime of `wafer` for `identity`, tagged as given.
    fn runtime_of(
        wafer: wafer_run::Wafer,
        identity: &str,
        version: &str,
        config_version: Option<String>,
        probe_deadline_ms: u64,
    ) -> Rc<ReadyRuntime> {
        Rc::new(ReadyRuntime {
            wafer,
            variables_snapshot: std::sync::Arc::default(),
            version: version.to_string(),
            config_version,
            environment_identity: identity.to_string(),
            probe_deadline_ms: Cell::new(probe_deadline_ms),
            probe_failures: Cell::new(0),
            health: Cell::new(RuntimeHealth::default()),
            retry_streaks: OnceCell::new(),
        })
    }

    /// This Worker version's packaged plan, sealed at [`PLAN_GENERATION`].
    fn packaged_plan() -> Rc<impresspress_core::PreparedRuntimePlan> {
        Rc::new(
            impresspress_core::PreparedRuntimePlan::new_with_config_generation(
                "example",
                format!("sha256:{}", "a".repeat(64)),
                PLAN_GENERATION,
                impresspress_core::WaferLockIdentity::absent(),
                impresspress_core::PreparedReleaseAssets::absent(),
                impresspress_core::PreparedRuntimeStructure {
                    application_blocks: Vec::new(),
                    routes: Vec::new(),
                    built_in_route_count: 0,
                    block_settings: Default::default(),
                    block_configs: Default::default(),
                    final_block_configs: Default::default(),
                    wrap_grants: Vec::new(),
                    deployment_wrap_grants: Vec::new(),
                },
            )
            .expect("a plan"),
        )
    }

    fn bypassed(plan: &impresspress_core::PreparedRuntimePlan) -> bool {
        prepared_is_bypassed(&prepared_cache_identity(&plan.plan_hash, IDENTITY))
    }

    fn window() -> Option<RetryWindow> {
        RETRY_WINDOW.with(Cell::get)
    }

    const START: u64 = 1_000_000;

    /// **A cold isolate whose plan is stale serves it during an outage**
    /// (U-B1 (b), addendum §4.3 tests 1, 2, 4, 6). Request 1's dynamic build
    /// fails transiently: a 503 with the window's `Retry-After`, the window
    /// armed and the plan NOT bypassed (M2). Request 2 hydrates the plan and
    /// serves it stale without moving the window; request 3 is served with
    /// no build and no probe. After the window, with D1 back, the dynamic
    /// build publishes and only then commits the bypass.
    #[wasm_bindgen_test]
    async fn a_cold_isolate_serves_its_stale_plan_during_an_outage() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let plan = packaged_plan();
        let io = FakeIo::new(Some(plan.clone()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        io.dynamic.set(Ends::Unavailable);
        io.hydrate.set(Ends::Degraded);

        let first = get_or_build_with(&io, &timer).await;
        assert!(
            matches!(&first, Err(RuntimeBuildError::Unavailable { retry_after_secs, .. }) if *retry_after_secs >= 1),
            "{first:?}"
        );
        assert_eq!((io.builds.get(), io.hydrates.get()), (1, 0));
        let armed = window().expect("the failed build armed the window");
        assert_eq!(armed.cause, WindowCause::BuildUnavailable);
        assert!(!bypassed(&plan), "request 1 leaves the plan un-bypassed");

        let (rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("request 2 is served");
        assert!(
            matches!(outcome, CacheOutcome::ColdBuilt { .. }),
            "the hydrate is this request's build: {outcome:?}"
        );
        assert_eq!(rt.config_version.as_deref(), Some(PLAN_GENERATION));
        assert_eq!(rt.health.get().stale_since_ms, Some(armed.armed_at_ms));
        assert_eq!((io.builds.get(), io.hydrates.get()), (1, 1));
        assert_eq!(
            window(),
            Some(armed),
            "the hydrate leaves the window as it was"
        );
        assert!(!bypassed(&plan));

        let probes = io.probes.get();
        let (_rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("request 3 is served");
        assert_eq!(outcome, CacheOutcome::Stale);
        assert_eq!(
            (io.builds.get(), io.hydrates.get(), io.probes.get()),
            (1, 1, probes),
            "inside the window: no build, no probe"
        );

        // After the window, D1 is back: the prepared owner probes, sees the
        // moved stamp, and hands the plan to the dynamic path.
        timer.now.set(armed.retry_at_ms);
        io.dynamic.set(Ends::Healthy);
        let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("rebuilt");
        assert!(
            matches!(outcome, CacheOutcome::Rebuilt { .. }),
            "{outcome:?}"
        );
        assert_eq!(rt.config_version, None, "a dynamic runtime");
        assert!(rt.health.get().is_healthy());
        assert!(
            bypassed(&plan),
            "the bypass is committed with the stored dynamic runtime"
        );
        assert_eq!(window(), None, "a healthy publish clears the window");
        reset_isolate();
    }

    /// After the window with D1 still down, the dynamic attempt fails again:
    /// the stale prepared runtime keeps serving, the window doubles, and the
    /// plan stays un-bypassed (addendum §4.3 test 6, second half).
    #[wasm_bindgen_test]
    async fn a_failed_retry_keeps_the_stale_plan_and_doubles_the_window() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let plan = packaged_plan();
        let io = FakeIo::new(Some(plan.clone()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        io.dynamic.set(Ends::Unavailable);
        let _ = get_or_build_with(&io, &timer).await;
        get_or_build_with(&io, &timer)
            .await
            .expect("the stale plan is served");
        let armed = window().expect("armed");

        timer.now.set(armed.retry_at_ms);
        let (rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("the stale runtime keeps serving");
        assert_eq!(outcome, CacheOutcome::Stale);
        assert_eq!(rt.config_version.as_deref(), Some(PLAN_GENERATION));
        let doubled = window().expect("still armed");
        assert_eq!(doubled.failures, armed.failures + 1);
        assert_eq!(doubled.cause, WindowCause::RebuildFailed);
        assert!(!bypassed(&plan));
        reset_isolate();
    }

    /// **A healthy hydrate does not clear the window** (addendum §4.3 test
    /// 5): the plan is known to be older than the database, so it is
    /// retried after the ≤ 60 s window, not at the 5–10 minute probe.
    #[wasm_bindgen_test]
    async fn a_healthy_hydrate_of_a_stale_plan_leaves_the_window() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let plan = packaged_plan();
        let io = FakeIo::new(Some(plan.clone()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        io.dynamic.set(Ends::Unavailable);
        let _ = get_or_build_with(&io, &timer).await;
        let armed = window().expect("armed");

        io.hydrate.set(Ends::Healthy);
        let (rt, _outcome) = get_or_build_with(&io, &timer).await.expect("served");
        assert!(rt.health.get().stale_since_ms.is_some());
        assert_eq!(window(), Some(armed), "neither cleared nor doubled");
        reset_isolate();
    }

    /// **A permanent failure does not hydrate** (addendum §4.3 test 7): a
    /// config bug is the operator's to see, so a `Fault` window answers the
    /// 500 and nothing is built inside it.
    #[wasm_bindgen_test]
    async fn a_permanent_failure_window_answers_the_500_without_hydrating() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(Some(packaged_plan()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        io.dynamic.set(Ends::Fault);
        let first = get_or_build_with(&io, &timer).await;
        assert!(
            matches!(first, Err(RuntimeBuildError::Fault(_))),
            "{first:?}"
        );
        assert_eq!(window().map(|w| w.cause), Some(WindowCause::Fault));

        let second = get_or_build_with(&io, &timer).await;
        assert!(
            matches!(second, Err(RuntimeBuildError::Fault(_))),
            "{second:?}"
        );
        assert_eq!((io.builds.get(), io.hydrates.get()), (1, 0));
        reset_isolate();
    }

    /// **Another identity's runtime is not replaced inside the window**
    /// (addendum §4.3 test 10, first half): a request whose environment
    /// identity changed answers `Unavailable` with the window's time, and
    /// nothing is hydrated over the cached runtime.
    #[wasm_bindgen_test]
    async fn another_identitys_runtime_is_not_replaced_inside_the_window() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(Some(packaged_plan()));
        store(runtime("env-before-a-secret-rotation", "v", None, u64::MAX));
        arm_window(START, WindowCause::RebuildFailed);

        let served = get_or_build_with(&io, &timer).await;
        assert!(
            matches!(served, Err(RuntimeBuildError::Unavailable { .. })),
            "{served:?}"
        );
        assert_eq!(io.hydrates.get(), 0);
        assert_eq!(
            cached()
                .map(|rt| rt.environment_identity.clone())
                .as_deref(),
            Some("env-before-a-secret-rotation")
        );
        reset_isolate();
    }

    /// **A `Degraded` window with nothing cached hydrates** (addendum §4.3
    /// test 10, second half): an interrupted holder can drop `RUNTIME`, so
    /// any non-`Fault` cause with a cold cache takes the (b) arm.
    #[wasm_bindgen_test]
    async fn a_degraded_window_with_nothing_cached_hydrates() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(Some(packaged_plan()));
        arm_window(START, WindowCause::Degraded);

        let (rt, _outcome) = get_or_build_with(&io, &timer).await.expect("hydrated");
        assert!(rt.health.get().stale_since_ms.is_some());
        assert_eq!(io.hydrates.get(), 1);
        reset_isolate();
    }

    /// **The window comes first** (I5b): inside it, with `DIRTY` set and the
    /// probe deadline passed, the cached runtime is served, nothing is probed
    /// or built, and `DIRTY` is left for the rebuild after the window.
    #[wasm_bindgen_test]
    async fn inside_the_window_dirty_and_the_deadline_wait() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(None);
        store(runtime(IDENTITY, "v", None, START - 1));
        arm_window(START, WindowCause::Degraded);
        mark_dirty();

        let (_rt, _outcome) = get_or_build_with(&io, &timer).await.expect("served");
        assert_eq!((io.probes.get(), io.builds.get()), (0, 0));
        assert!(
            DIRTY.with(Cell::get),
            "DIRTY is not consumed inside the window"
        );
        reset_isolate();
    }

    /// **A degraded publish arms the window, and the degraded runtime is
    /// rebuilt after it** — never a warm hit, however long its probe window:
    /// the retry is the rebuild. A healthy rebuild then clears the window.
    #[wasm_bindgen_test]
    async fn a_degraded_runtime_is_rebuilt_after_the_window() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(None);
        io.dynamic.set(Ends::Degraded);
        let (rt, _outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("published degraded");
        assert!(rt.health.get().degraded_since_ms.is_some());
        let armed = window().expect("a degraded publish arms the window");
        assert_eq!(armed.cause, WindowCause::Degraded);

        let (_rt, outcome) = get_or_build_with(&io, &timer).await.expect("served inside");
        assert_eq!(outcome, CacheOutcome::Stale);
        assert_eq!(io.builds.get(), 1);

        timer.now.set(armed.retry_at_ms);
        io.dynamic.set(Ends::Healthy);
        let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("rebuilt");
        assert!(
            matches!(outcome, CacheOutcome::Rebuilt { .. }),
            "{outcome:?}"
        );
        assert!(rt.health.get().is_healthy());
        assert_eq!(io.builds.get(), 2);
        assert_eq!(window(), None);
        reset_isolate();
    }

    /// **A request that arrives during the hydrate waits for it** (addendum
    /// §4.3 test 3): it is served the hydrated runtime, with no second
    /// hydrate and no 503 for the slot being busy.
    #[wasm_bindgen_test]
    async fn a_request_during_the_hydrate_waits_for_it() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(Some(packaged_plan()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        io.dynamic.set(Ends::Unavailable);
        let _ = get_or_build_with(&io, &timer).await;

        io.build_ticks.set(5);
        let (a, b) = futures::join!(
            get_or_build_with(&io, &timer),
            get_or_build_with(&io, &timer)
        );
        let outcomes = [a.expect("served").1, b.expect("served").1];
        assert_eq!(io.hydrates.get(), 1, "one hydrate for both");
        assert!(
            outcomes
                .iter()
                .any(|o| matches!(o, CacheOutcome::ColdBuilt { .. }))
                && outcomes
                    .iter()
                    .any(|o| matches!(o, CacheOutcome::Waited { .. })),
            "{outcomes:?}"
        );
        reset_isolate();
    }

    /// **A superseded hand-off commits no bypass** (addendum §4.3 test 9,
    /// re-review n9): the dynamic owner whose publish loses to a newer owner
    /// serves its runtime request-locally and leaves the plan un-bypassed.
    #[wasm_bindgen_test]
    async fn a_superseded_hand_off_commits_no_bypass() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let plan = packaged_plan();
        let io = FakeIo::new(Some(plan.clone()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        io.supersede.set(true);

        let (rt, _outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("served request-locally");
        assert_eq!(rt.config_version, None, "the dynamic runtime it built");
        assert!(!bypassed(&plan), "nothing stored, nothing committed");
        drop(io.superseded_by.borrow_mut().take());
        reset_isolate();
    }

    /// **A failed hand-off's waiter answers 503** (addendum §4.3 test 11,
    /// re-review n13): the hand-off stays under `as_owner`, so the request
    /// waiting on it is told the build failed instead of building again.
    #[wasm_bindgen_test]
    async fn a_failed_hand_offs_waiter_answers_503_without_building() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(Some(packaged_plan()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        io.dynamic.set(Ends::Unavailable);
        io.build_ticks.set(5);

        let (a, b) = futures::join!(
            get_or_build_with(&io, &timer),
            get_or_build_with(&io, &timer)
        );
        assert!(
            matches!(a, Err(RuntimeBuildError::Unavailable { .. })),
            "{a:?}"
        );
        assert!(
            matches!(b, Err(RuntimeBuildError::Unavailable { .. })),
            "{b:?}"
        );
        assert_eq!(io.builds.get(), 1, "the waiter did not build again");
        reset_isolate();
    }

    /// The retry delay doubles from 1 s to a 60 s cap, a window's remaining
    /// time rounds up to whole seconds and is never 0, and the jitter stays
    /// within a quarter of the delay.
    #[wasm_bindgen_test]
    fn the_retry_window_doubles_to_its_cap() {
        assert_eq!(retry_delay_ms(1), 1_000);
        assert_eq!(retry_delay_ms(2), 2_000);
        assert_eq!(retry_delay_ms(7), RETRY_WINDOW_MAX_MS);
        assert_eq!(retry_delay_ms(u32::MAX), RETRY_WINDOW_MAX_MS);
        for _ in 0..64 {
            assert!(retry_jitter_ms(4_000) <= 1_000);
        }
        let window = RetryWindow {
            failures: 1,
            armed_at_ms: 0,
            retry_at_ms: 2_500,
            cause: WindowCause::BuildUnavailable,
        };
        assert_eq!(window.remaining_secs(0), 3);
        assert_eq!(window.remaining_secs(2_400), 1);
        assert_eq!(window.remaining_secs(9_000), 1, "never Retry-After: 0");
    }

    /// **A hand-off that ends on the isolate's cached dynamic runtime
    /// commits the bypass** (#275 review n4): the bypass means "this isolate
    /// has a dynamic runtime for the moved config", which it does, so later
    /// requests go straight to the dynamic path instead of probing KV and
    /// handing off every time.
    #[wasm_bindgen_test]
    async fn a_hand_off_to_a_cached_dynamic_runtime_commits_the_bypass() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let plan = packaged_plan();
        let io = FakeIo::new(Some(plan.clone()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        store(runtime(IDENTITY, MOVED_GENERATION, None, u64::MAX));

        let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("served");
        assert_eq!(outcome, CacheOutcome::Hit, "the cached dynamic runtime");
        assert_eq!(rt.config_version, None);
        assert_eq!(io.builds.get(), 0);
        assert!(bypassed(&plan), "the bypass is committed");
        reset_isolate();
    }

    /// **A failed hydrate paces itself** (#275 review M1): it doubles the
    /// window (transient) or re-arms it as a `Fault` (permanent), so a third
    /// request inside the new window builds nothing — one build per window,
    /// not one per request.
    #[wasm_bindgen_test]
    async fn a_failed_hydrate_builds_at_most_once_per_window() {
        for (fails_with, cause) in [
            (Ends::Unavailable, WindowCause::HydrateFailed),
            (Ends::Fault, WindowCause::Fault),
        ] {
            reset_isolate();
            let timer = FakeTimer::starting_at(START);
            let io = FakeIo::new(Some(packaged_plan()));
            *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
            io.dynamic.set(Ends::Unavailable);
            io.hydrate.set(fails_with);
            let _ = get_or_build_with(&io, &timer).await;
            let first = window().expect("armed by the dynamic build");

            let second = get_or_build_with(&io, &timer).await;
            assert!(second.is_err(), "{fails_with:?}: {second:?}");
            let paced = window().expect("still armed");
            assert_eq!(paced.cause, cause, "{fails_with:?}");
            assert_eq!(paced.failures, first.failures + 1, "{fails_with:?}");

            let third = get_or_build_with(&io, &timer).await;
            assert!(third.is_err(), "{fails_with:?}: {third:?}");
            assert_eq!(
                (io.builds.get(), io.hydrates.get()),
                (1, 1),
                "{fails_with:?}: nothing is built inside the paced window"
            );
        }
        reset_isolate();
    }

    /// **A stale plan is never republished as current on an unavailable
    /// probe** (#275 review m1): after the window, a KV probe that fails
    /// cannot tell the isolate the plan is current, so the stale runtime keeps
    /// serving, stale since when it was, and the window doubles — no hydrate,
    /// no `Healthy`, no cleared window.
    #[wasm_bindgen_test]
    async fn an_unavailable_probe_keeps_a_stale_plan_stale() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(Some(packaged_plan()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        io.dynamic.set(Ends::Unavailable);
        let _ = get_or_build_with(&io, &timer).await;
        let (rt, _) = get_or_build_with(&io, &timer)
            .await
            .expect("the stale plan");
        let stale_since = rt.health.get().stale_since_ms;
        let armed = window().expect("armed");

        timer.now.set(armed.retry_at_ms);
        io.probe_unavailable.set(true);
        io.hydrate.set(Ends::Healthy);
        let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("served");
        assert_eq!(outcome, CacheOutcome::Stale);
        assert_eq!(rt.health.get().stale_since_ms, stale_since);
        assert_eq!(io.hydrates.get(), 1, "not re-hydrated as current");
        let doubled = window().expect("the window is not cleared");
        assert_eq!(doubled.failures, armed.failures + 1);
        reset_isolate();
    }

    /// **A local write is not lost behind a stale plan and a failed probe**
    /// (#275 re-review m6): with the window over, KV down and an admin save
    /// in this isolate (`DIRTY`), the request hands off to the dynamic path
    /// and rebuilds from D1 rather than serving the stale plan until a probe
    /// succeeds.
    #[wasm_bindgen_test]
    async fn a_dirty_isolate_with_a_stale_plan_rebuilds_despite_a_failed_probe() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let plan = packaged_plan();
        let io = FakeIo::new(Some(plan.clone()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        io.dynamic.set(Ends::Unavailable);
        let _ = get_or_build_with(&io, &timer).await;
        get_or_build_with(&io, &timer)
            .await
            .expect("the stale plan");
        let armed = window().expect("armed");

        timer.now.set(armed.retry_at_ms);
        io.probe_unavailable.set(true);
        io.dynamic.set(Ends::Healthy);
        mark_dirty();
        let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("rebuilt");
        assert!(
            matches!(outcome, CacheOutcome::Rebuilt { .. }),
            "{outcome:?}"
        );
        assert_eq!(rt.config_version, None, "the dynamic runtime, from D1");
        assert_eq!(io.builds.get(), 2);
        assert!(bypassed(&plan));
        reset_isolate();
    }

    /// The same with D1 still down (#275 final check n9): the dirty rebuild
    /// fails, so the stale plan keeps serving, un-bypassed, and `DIRTY`
    /// survives the failed attempt for the rebuild after the next window.
    #[wasm_bindgen_test]
    async fn a_dirty_rebuild_that_fails_keeps_dirty_and_the_plan() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let plan = packaged_plan();
        let io = FakeIo::new(Some(plan.clone()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        io.dynamic.set(Ends::Unavailable);
        let _ = get_or_build_with(&io, &timer).await;
        get_or_build_with(&io, &timer)
            .await
            .expect("the stale plan");
        let armed = window().expect("armed");

        timer.now.set(armed.retry_at_ms);
        io.probe_unavailable.set(true);
        mark_dirty();
        let (rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("the stale plan keeps serving");
        assert_eq!(outcome, CacheOutcome::Stale);
        assert_eq!(rt.config_version.as_deref(), Some(PLAN_GENERATION));
        assert_eq!(io.builds.get(), 2, "the dirty rebuild was attempted");
        assert!(DIRTY.with(Cell::get), "DIRTY survives the failed attempt");
        assert!(!bypassed(&plan), "nothing dynamic was stored");
        reset_isolate();
    }

    /// A D1 binding that rejects every statement as an outage would.
    fn d1_that_is_down() -> worker::D1Database {
        let d1 = js_sys::Function::new_no_args(
            "const fail = () => Promise.reject(new Error('D1_ERROR: Network connection lost.'));
             return {
                 prepare() {
                     const stmt = { bind() { return stmt; }, all: fail, run: fail, raw: fail,
                                    first: fail };
                     return stmt;
                 },
                 batch: fail,
             };",
        )
        .call0(&wasm_bindgen::JsValue::NULL)
        .expect("the fake binding builds");
        wasm_bindgen::JsCast::unchecked_into::<worker::D1Database>(d1)
    }

    /// **A published runtime whose variables snapshot remembers a failed
    /// read arms the window** (I5a; #275 review m2): under
    /// `RetryMode::Rebuild` that arming is the only retry its stored config
    /// gets. The next request marks it degraded and arms `Degraded`, so the
    /// runtime is rebuilt within the window rather than the probe's 5–10
    /// minutes.
    #[wasm_bindgen_test]
    async fn a_runtime_whose_snapshot_failed_arms_the_window() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(None);
        let snapshot =
            std::sync::Arc::new(impresspress_core::config_source::VariablesSnapshot::new(
                impresspress_core::config_source::RetryMode::Rebuild,
            ));
        let db: std::sync::Arc<dyn wafer_core::interfaces::database::service::DatabaseService> =
            std::sync::Arc::new(crate::database::D1DatabaseService::new(
                d1_that_is_down(),
                "DB",
                crate::database::D1CallCount::new(),
                1000,
            ));
        crate::database::forget_isolate_schema();
        snapshot
            .read(&db)
            .await
            .expect_err("the outage fails the read");
        assert!(snapshot.remembers_a_failure());

        let rt = Rc::new(ReadyRuntime {
            wafer: wafer_run::Wafer::new(std::sync::Arc::new(
                wafer_run::StaticConfigSource::default(),
            ))
            .expect("a bare runtime"),
            variables_snapshot: snapshot,
            version: "v".to_string(),
            config_version: None,
            environment_identity: IDENTITY.to_string(),
            probe_deadline_ms: Cell::new(u64::MAX),
            probe_failures: Cell::new(0),
            health: Cell::new(RuntimeHealth::default()),
            retry_streaks: OnceCell::new(),
        });
        store(rt);
        assert_eq!(window(), None);

        let (rt, _) = get_or_build_with(&io, &timer).await.expect("served");
        assert!(rt.health.get().degraded_since_ms.is_some());
        assert_eq!(window().map(|w| w.cause), Some(WindowCause::Degraded));
        reset_isolate();
    }

    // -----------------------------------------------------------------------
    // The gate (see the module docs): a retry reads the variables table
    // before it builds anything.
    // -----------------------------------------------------------------------

    use crate::test_fakes::VariablesAnswer;

    /// The answer of the gate's read from now on.
    fn backend(io: &FakeIo, answer: VariablesAnswer) {
        io.variables_answer(Some(answer));
    }

    /// A cold prepared isolate that published degraded, the window armed
    /// `Degraded`, and the clock at the window's end.
    async fn degraded_prepared_isolate(io: &FakeIo, timer: &FakeTimer) -> RetryWindow {
        io.hydrate.set(Ends::Degraded);
        get_or_build_with(io, timer)
            .await
            .expect("the cold build publishes degraded");
        let armed = window().expect("a degraded publish arms the window");
        assert_eq!(armed.cause, WindowCause::Degraded);
        timer.now.set(armed.retry_at_ms);
        armed
    }

    /// **A failed gate builds nothing and keeps the degraded runtime as it
    /// was** (acceptance 1): after the window, with the backend still down,
    /// the prepared owner of a current plan reads the variables table, fails,
    /// and hydrates nothing. The window doubles under `GateFailed`, the
    /// request is served the cached runtime as `Gated`, still degraded and
    /// not stale, and a request inside the new window checks nothing.
    #[wasm_bindgen_test]
    async fn a_failed_gate_builds_nothing_and_keeps_the_degraded_runtime() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(Some(packaged_plan()));
        let armed = degraded_prepared_isolate(&io, &timer).await;
        let cached_before = cached().expect("the degraded runtime");
        let health_before = cached_before.health.get();
        assert_eq!((io.hydrates.get(), io.gates.get()), (1, 1));

        backend(&io, VariablesAnswer::Unavailable);
        let (rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("the cached runtime serves on");

        assert_eq!(outcome, CacheOutcome::Gated);
        assert_eq!(io.hydrates.get(), 1, "the failed gate hydrated nothing");
        assert_eq!(io.gates.get(), 2, "the retry checked the backend");
        assert!(Rc::ptr_eq(&rt, &cached_before), "the same runtime");
        assert_eq!(rt.health.get(), health_before, "degraded, not stale");
        let doubled = window().expect("re-armed");
        assert_eq!(doubled.cause, WindowCause::GateFailed);
        assert_eq!(doubled.failures, armed.failures + 1);
        assert_eq!(doubled.armed_at_ms, armed.armed_at_ms, "one episode");
        assert!(!platform_health().episode.expect("an episode").failed);

        let gates = io.gates.get();
        let (_rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("inside the new window");
        assert_eq!(outcome, CacheOutcome::Stale);
        assert_eq!(io.gates.get(), gates, "inside a window nothing is checked");
        reset_isolate();
    }

    /// **A passed gate hands its snapshot to exactly one hydrate**
    /// (acceptance 2): the build reads the variables table through the
    /// gate's read (the fake checks it is the same `Arc`), and its outcome
    /// moves the window as a build's always has: a healthy publish clears
    /// it, a degraded one doubles it.
    #[wasm_bindgen_test]
    async fn a_passed_gate_hands_its_snapshot_to_one_hydrate() {
        for (ends, cleared) in [(Ends::Healthy, true), (Ends::Degraded, false)] {
            reset_isolate();
            let timer = FakeTimer::starting_at(START);
            let io = FakeIo::new(Some(packaged_plan()));
            let armed = degraded_prepared_isolate(&io, &timer).await;

            io.hydrate.set(ends);
            let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("rebuilt");

            assert!(
                matches!(outcome, CacheOutcome::Rebuilt { .. }),
                "{ends:?}: {outcome:?}"
            );
            assert_eq!(io.hydrates.get(), 2, "{ends:?}: one hydrate");
            assert!(
                io.built_over_the_gates_snapshot(),
                "{ends:?}: the hydrate read through the gate's snapshot"
            );
            assert!(Arc::ptr_eq(
                &rt.variables_snapshot,
                io.gated.borrow().as_ref().expect("gated")
            ));
            if cleared {
                assert_eq!(window(), None, "a healthy publish clears the window");
            } else {
                let doubled = window().expect("still armed");
                assert_eq!(doubled.cause, WindowCause::Degraded);
                assert_eq!(doubled.failures, armed.failures + 1);
            }
        }
        reset_isolate();
    }

    /// **A cold prepared isolate whose gate fails builds anyway** (acceptance
    /// 3, Decision 1): with nothing cached there is no runtime to keep, so it
    /// hydrates once over the failed snapshot and publishes degraded, which
    /// serves the static site. A permanent failure builds nothing: no boot
    /// survives it, so the request is the 500 and the window a `Fault`.
    #[wasm_bindgen_test]
    async fn a_cold_prepared_isolate_whose_gate_fails_builds_once_degraded() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(Some(packaged_plan()));
        backend(&io, VariablesAnswer::Unavailable);
        io.hydrate.set(Ends::Degraded);

        let (rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("published degraded");

        assert!(
            matches!(outcome, CacheOutcome::ColdBuilt { .. }),
            "{outcome:?}"
        );
        assert_eq!((io.gates.get(), io.hydrates.get()), (1, 1));
        assert!(io.built_over_the_gates_snapshot());
        assert!(
            rt.variables_snapshot.remembers_a_failure(),
            "the build was handed the failed read, not a fresh one"
        );
        assert_eq!(io.variables_reads.get(), 1, "the table was read once");
        assert_eq!(window().map(|w| w.cause), Some(WindowCause::Degraded));

        reset_isolate();
        let io = FakeIo::new(Some(packaged_plan()));
        backend(&io, VariablesAnswer::Fault);
        let first = get_or_build_with(&io, &timer).await;
        assert!(
            matches!(first, Err(RuntimeBuildError::Fault(_))),
            "{first:?}"
        );
        assert_eq!(io.hydrates.get(), 0, "nothing is built over a fault");
        assert_eq!(window().map(|w| w.cause), Some(WindowCause::Fault));
        let second = get_or_build_with(&io, &timer).await;
        assert!(
            matches!(second, Err(RuntimeBuildError::Fault(_))),
            "a cold isolate inside a Fault window answers the 500: {second:?}"
        );
        assert_eq!(io.gates.get(), 1, "and checks nothing inside it");
        reset_isolate();
    }

    /// **A cold dynamic isolate whose gate fails answers 503 without
    /// building** (acceptance 4, CPU-GATE review M1): a dynamic boot reads
    /// the WRAP grants first and cannot publish past a failed read, so
    /// building would only spend the CPU. Request 2, inside the window, takes
    /// U-B1's hydrate of the stale plan: the isolate's one build. Without a
    /// plan, the window's own answer.
    #[wasm_bindgen_test]
    async fn a_cold_dynamic_isolate_whose_gate_fails_answers_503_without_building() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let plan = packaged_plan();
        let io = FakeIo::new(Some(plan.clone()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        backend(&io, VariablesAnswer::Unavailable);
        io.hydrate.set(Ends::Degraded);

        let first = get_or_build_with(&io, &timer).await;
        let armed = window().expect("the failed gate armed the window");
        assert!(
            matches!(&first, Err(RuntimeBuildError::Unavailable { retry_after_secs, .. })
                if *retry_after_secs == armed.remaining_secs(timer.now_ms())),
            "{first:?}"
        );
        assert_eq!(armed.cause, WindowCause::BuildUnavailable);
        assert_eq!(
            (io.settings_reads.get(), io.gates.get()),
            (0, 1),
            "the gate is the first read, and its failure ends the attempt"
        );
        assert_eq!(
            (io.builds.get(), io.hydrates.get()),
            (0, 0),
            "nothing built"
        );
        assert!(!bypassed(&plan));

        let (rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("request 2 is served the stale plan");
        assert!(
            matches!(outcome, CacheOutcome::ColdBuilt { .. }),
            "{outcome:?}"
        );
        assert_eq!((io.builds.get(), io.hydrates.get()), (0, 1));
        assert_eq!(rt.health.get().stale_since_ms, Some(armed.armed_at_ms));
        assert!(
            rt.variables_snapshot.remembers_a_failure(),
            "the hydrate was gated too, and built over the failed read"
        );

        reset_isolate();
        let io = FakeIo::new(None);
        backend(&io, VariablesAnswer::Unavailable);
        let first = get_or_build_with(&io, &timer).await;
        assert!(
            matches!(first, Err(RuntimeBuildError::Unavailable { .. })),
            "{first:?}"
        );
        assert_eq!(io.builds.get(), 0);
        let second = get_or_build_with(&io, &timer).await;
        assert!(
            matches!(second, Err(RuntimeBuildError::Unavailable { .. })),
            "no plan: the window's answer: {second:?}"
        );
        assert_eq!((io.builds.get(), io.gates.get()), (0, 1));
        reset_isolate();
    }

    /// **A warm degraded dynamic runtime whose retry meets the outage stays
    /// degraded, not stale** (PR #282 review M1): the gate is the attempt's
    /// first D1 read, so a database-wide outage fails it rather than the
    /// read-through `block_settings` read, and the retry is a failed gate —
    /// `Gated`, `GateFailed`, the runtime kept as it was — not a failed
    /// rebuild that would mark a runtime of the current config stale.
    #[wasm_bindgen_test]
    async fn a_warm_degraded_dynamic_runtime_whose_gate_fails_stays_degraded() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(None);
        io.dynamic.set(Ends::Degraded);
        let (rt, _) = get_or_build_with(&io, &timer)
            .await
            .expect("published degraded");
        let health = rt.health.get();
        let armed = window().expect("armed");
        timer.now.set(armed.retry_at_ms);

        backend(&io, VariablesAnswer::Unavailable);
        let (kept, outcome) = get_or_build_with(&io, &timer).await.expect("kept");

        assert_eq!(outcome, CacheOutcome::Gated);
        assert!(Rc::ptr_eq(&kept, &rt));
        assert_eq!(kept.health.get(), health, "degraded, not stale");
        assert_eq!(window().map(|w| w.cause), Some(WindowCause::GateFailed));
        assert_eq!(io.builds.get(), 1, "nothing built");
        assert_eq!(
            io.settings_reads.get(),
            1,
            "only the cold build read block_settings"
        );
        reset_isolate();
    }

    /// **A hand-off whose gate fails goes through the failure branch**
    /// (acceptance 5, CPU-GATE review m1): nothing is built, the prepared
    /// runtime is kept and marked stale (its config is older than the
    /// database's), `DIRTY` — which the prepared owner set for the hand-off
    /// and the dynamic owner consumed — is set again, no bypass is
    /// committed, and the window doubles as `RebuildFailed`.
    #[wasm_bindgen_test]
    async fn a_hand_off_whose_gate_fails_keeps_dirty_and_the_stale_plan() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let plan = packaged_plan();
        let io = FakeIo::new(Some(plan.clone()));
        // A current plan, published degraded; then an admin save elsewhere
        // moves the stamp.
        let armed = degraded_prepared_isolate(&io, &timer).await;
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        backend(&io, VariablesAnswer::Unavailable);

        let (rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("the prepared runtime serves on");

        assert_eq!(outcome, CacheOutcome::Gated);
        assert_eq!(rt.config_version.as_deref(), Some(PLAN_GENERATION));
        assert_eq!(
            (io.builds.get(), io.hydrates.get()),
            (0, 1),
            "nothing built"
        );
        assert_eq!(
            io.settings_reads.get(),
            0,
            "the dynamic owner read nothing past its failed gate"
        );
        assert!(rt.health.get().stale_since_ms.is_some(), "kept, and stale");
        assert!(DIRTY.with(Cell::get), "DIRTY survives the failed gate");
        assert!(!bypassed(&plan), "nothing dynamic was stored");
        let doubled = window().expect("re-armed");
        assert_eq!(doubled.cause, WindowCause::RebuildFailed);
        assert_eq!(doubled.failures, armed.failures + 1);
        reset_isolate();
    }

    /// The same for a local write on an isolate serving a stale plan, with
    /// the probe failing too (#275 final check n9, through the gate): the
    /// dirty rebuild is gated, so it builds nothing, and `DIRTY` survives.
    #[wasm_bindgen_test]
    async fn a_dirty_rebuild_whose_gate_fails_keeps_dirty_and_the_plan() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let plan = packaged_plan();
        let io = FakeIo::new(Some(plan.clone()));
        *io.stamp.borrow_mut() = MOVED_GENERATION.to_string();
        backend(&io, VariablesAnswer::Unavailable);
        let _ = get_or_build_with(&io, &timer).await;
        get_or_build_with(&io, &timer)
            .await
            .expect("the stale plan");
        let armed = window().expect("armed");

        timer.now.set(armed.retry_at_ms);
        io.probe_unavailable.set(true);
        mark_dirty();
        let (rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("the stale plan keeps serving");
        assert_eq!(outcome, CacheOutcome::Gated);
        assert_eq!(rt.config_version.as_deref(), Some(PLAN_GENERATION));
        assert_eq!(io.builds.get(), 0, "the gate stopped the dirty rebuild");
        assert_eq!(io.gates.get(), 3, "request 1, the hydrate, the dirty retry");
        assert!(DIRTY.with(Cell::get), "DIRTY survives the failed gate");
        assert!(!bypassed(&plan));
        reset_isolate();
    }

    /// **The gate's semantics the review spelled out** (CPU-GATE review m2):
    /// an absent table passes (a fresh database is not a failing one); a
    /// permanent failure answers the gating request the 500 and arms a
    /// `Fault` window while the cached runtime serves the next request; and
    /// a `GateFailed` window whose runtime an interrupted holder dropped
    /// hydrates like any non-`Fault` window.
    #[wasm_bindgen_test]
    async fn the_gate_passes_an_absent_table_and_faults_a_permanent_failure() {
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(Some(packaged_plan()));
        degraded_prepared_isolate(&io, &timer).await;
        backend(&io, VariablesAnswer::Absent);
        io.hydrate.set(Ends::Healthy);
        let (_rt, outcome) = get_or_build_with(&io, &timer).await.expect("rebuilt");
        assert!(
            matches!(outcome, CacheOutcome::Rebuilt { .. }),
            "{outcome:?}"
        );
        assert_eq!(io.hydrates.get(), 2, "an absent table passed the gate");

        reset_isolate();
        let io = FakeIo::new(Some(packaged_plan()));
        let armed = degraded_prepared_isolate(&io, &timer).await;
        backend(&io, VariablesAnswer::Fault);
        let gating = get_or_build_with(&io, &timer).await;
        assert!(
            matches!(gating, Err(RuntimeBuildError::Fault(_))),
            "{gating:?}"
        );
        assert_eq!(io.hydrates.get(), 1, "nothing built");
        let faulted = window().expect("armed");
        assert_eq!(faulted.cause, WindowCause::Fault);
        assert_eq!(faulted.failures, armed.failures + 1);
        assert!(platform_health().episode.expect("an episode").failed);
        let (_rt, outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("the cached runtime serves everyone else");
        assert_eq!(outcome, CacheOutcome::Stale);

        reset_isolate();
        let io = FakeIo::new(Some(packaged_plan()));
        let armed = arm_window(timer.now_ms(), WindowCause::GateFailed);
        let (rt, _outcome) = get_or_build_with(&io, &timer)
            .await
            .expect("a GateFailed window with nothing cached hydrates");
        assert_eq!(io.hydrates.get(), 1);
        assert_eq!(
            rt.health.get().stale_since_ms,
            Some(armed.armed_at_ms),
            "U-B1's hydrate, inside the window"
        );
        reset_isolate();
    }

    /// A runtime of `kind` whose blocks all initialized and whose variables
    /// snapshot remembers a failed read, stored as the isolate's.
    async fn a_runtime_whose_only_defect_is_a_failed_read(plan: Option<&str>) {
        let snapshot = crate::runtime_build::fresh_variables_snapshot();
        let down: Arc<dyn DatabaseService> = Arc::new(crate::test_fakes::VariablesDb::new(
            Rc::new(Cell::new(Some(VariablesAnswer::Unavailable))),
            Rc::default(),
        ));
        snapshot.read(&down).await.expect_err("the outage fails it");
        let (version, config_version) = match plan {
            Some(plan_hash) => (plan_hash.to_string(), Some(PLAN_GENERATION.to_string())),
            None => (PLAN_GENERATION.to_string(), None),
        };
        let mut rt = runtime(IDENTITY, &version, config_version, u64::MAX);
        Rc::get_mut(&mut rt).expect("one handle").variables_snapshot = snapshot;
        store(rt);
    }

    /// **A runtime whose only defect is a failed variables read is healed by
    /// the read alone** (CPU-GATE review m3): every block initialized, so the
    /// window's owner reads the runtime's own snapshot again instead of
    /// rebuilding. Still down, the window doubles as `GateFailed`; back, the
    /// runtime is no longer degraded, the window is cleared, and nothing was
    /// built either time. Both paths.
    #[wasm_bindgen_test]
    async fn a_runtime_whose_only_defect_is_its_snapshot_heals_in_place() {
        let plan = packaged_plan();
        for prepared in [false, true] {
            reset_isolate();
            let timer = FakeTimer::starting_at(START);
            let io = FakeIo::new(prepared.then(|| plan.clone()));
            a_runtime_whose_only_defect_is_a_failed_read(
                prepared.then_some(plan.plan_hash.as_str()),
            )
            .await;
            let (rt, _) = get_or_build_with(&io, &timer).await.expect("served");
            assert!(rt.health.get().degraded_since_ms.is_some(), "{prepared}");
            let armed = window().expect("observe_snapshot armed it");

            timer.now.set(armed.retry_at_ms);
            backend(&io, VariablesAnswer::Unavailable);
            let (_rt, outcome) = get_or_build_with(&io, &timer).await.expect("kept");
            assert_eq!(outcome, CacheOutcome::Gated, "{prepared}");
            let doubled = window().expect("re-armed");
            assert_eq!(doubled.cause, WindowCause::GateFailed, "{prepared}");

            timer.now.set(doubled.retry_at_ms);
            backend(&io, VariablesAnswer::Rows);
            let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("healed");
            assert_eq!(outcome, CacheOutcome::ProbedFresh, "{prepared}");
            assert!(rt.health.get().is_healthy(), "{prepared}");
            assert!(!rt.variables_snapshot.remembers_a_failure(), "{prepared}");
            assert_eq!(window(), None, "{prepared}: the heal clears the window");
            assert_eq!(
                (io.builds.get(), io.hydrates.get(), io.gates.get()),
                (0, 0, 2),
                "{prepared}: two reads, no build"
            );
            assert!(
                rt.probe_deadline_ms.get() < u64::MAX,
                "{prepared}: the owner's probe is recorded, a fresh window from now"
            );
            let probes = io.probes.get();
            let (_rt, outcome) = get_or_build_with(&io, &timer).await.expect("warm");
            assert_eq!(outcome, CacheOutcome::Hit, "{prepared}");
            assert_eq!(io.probes.get(), probes, "{prepared}: no second probe");
        }

        // A heal whose probe could not reach KV says so, and backs the next
        // probe off, as for a healthy runtime.
        reset_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(None);
        a_runtime_whose_only_defect_is_a_failed_read(None).await;
        get_or_build_with(&io, &timer).await.expect("served");
        timer.now.set(window().expect("armed").retry_at_ms);
        io.probe_unavailable.set(true);
        let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("healed");
        assert_eq!(outcome, CacheOutcome::ProbeFailed);
        assert!(rt.health.get().is_healthy());
        assert_eq!(rt.probe_failures.get(), 1);
        reset_isolate();
    }

    /// **`/health` through gated windows** (acceptance 7, CPU-GATE review
    /// M3): `retries` counts every arming, builds and failed gates alike; the
    /// episode's `since` stays; a block's run (`runtimes`, `since`) moves only
    /// when a runtime is published; the episode is newly persistent at
    /// exactly the [`PERSISTENT_RETRIES`]th arming, the one `arm_window` logs
    /// at `error!`; and once the gate passes, a block that fails again still
    /// grows its run.
    ///
    /// [`PERSISTENT_RETRIES`]: impresspress_core::runtime_health::PERSISTENT_RETRIES
    #[wasm_bindgen_test]
    async fn health_counts_retries_through_gated_windows() {
        reset_isolate();
        crate::init_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(None);
        io.real_slots.set(true);
        io.dynamic.set(Ends::Degraded);
        let (rt, _) = get_or_build_with(&io, &timer).await.expect("published");
        let (_, _, body) = health_of(&rt).await;
        assert_eq!(body["retries"], 1, "{body}");
        let since = body["since"].clone();
        let run_since = body["unavailable"][0]["since"].clone();

        backend(&io, VariablesAnswer::Unavailable);
        let mut persistent = Vec::new();
        for retries in 2..=impresspress_core::runtime_health::PERSISTENT_RETRIES + 2 {
            timer.now.set(window().expect("armed").retry_at_ms);
            let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("kept");
            assert_eq!(outcome, CacheOutcome::Gated);
            let armed = window().expect("armed");
            if armed.is_newly_persistent() {
                persistent.push(armed.failures);
            }
            let (status, _, body) = health_of(&rt).await;
            assert_eq!(status, 503);
            assert_eq!(body["status"], "degraded", "{body}");
            assert_eq!(body["retries"], retries, "{body}");
            assert_eq!(body["since"], since, "the episode keeps its start: {body}");
            let entry = &body["unavailable"][0];
            assert_eq!(entry["runtimes"], 1, "no runtime published: {body}");
            assert_eq!(entry["since"], run_since, "{body}");
        }
        assert_eq!(
            persistent,
            [impresspress_core::runtime_health::PERSISTENT_RETRIES],
            "logged once, at the threshold"
        );
        assert_eq!(io.builds.get(), 1, "nothing built through the outage");

        backend(&io, VariablesAnswer::Rows);
        timer.now.set(window().expect("armed").retry_at_ms);
        let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("rebuilt");
        assert!(
            matches!(outcome, CacheOutcome::Rebuilt { .. }),
            "{outcome:?}"
        );
        let (_, _, body) = health_of(&rt).await;
        assert_eq!(body["unavailable"][0]["runtimes"], 2, "{body}");
        assert_eq!(body["unavailable"][0]["since"], run_since, "{body}");
        reset_isolate();
    }

    // -----------------------------------------------------------------------
    // `/health` (D1 outage plan I6): the system block's report, from a served
    // runtime's real slots and this isolate's state, through the source the
    // Cloudflare entry registers.
    // -----------------------------------------------------------------------

    /// D1's text for a reset its own query caused (plan I2g).
    const RESET: &str = "D1_ERROR: D1 DB exceeded its CPU time limit and was reset.";

    /// Answers nothing; its `Init` fails as D1 does after a reset, or not.
    struct Resets(bool);

    #[wafer_block::wafer_async_trait]
    impl wafer_run::Block for Resets {
        fn info(&self) -> wafer_run::BlockInfo {
            wafer_run::BlockInfo::new("test/resets", "0.0.1", "http-handler@v1", "resets D1")
        }
        async fn handle(
            &self,
            _ctx: &dyn wafer_run::context::Context,
            _msg: wafer_run::Message,
            _input: wafer_run::InputStream,
        ) -> wafer_run::OutputStream {
            impresspress_core::http::err_not_found("nothing here")
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn wafer_run::context::Context,
            event: wafer_run::LifecycleEvent,
        ) -> Result<(), wafer_run::WaferError> {
            if self.0 && matches!(event.event_type, wafer_run::LifecycleType::Init) {
                return Err(wafer_run::WaferError::new(
                    wafer_run::ErrorCode::Unavailable,
                    RESET,
                ));
            }
            Ok(())
        }
    }

    /// A sealed runtime of the system block and `test/resets`, both `Init`ed,
    /// as a boot leaves them: `test/resets` retrying when `resets`.
    async fn health_wafer(resets: bool) -> wafer_run::Wafer {
        let mut wafer =
            wafer_run::Wafer::new(std::sync::Arc::new(wafer_run::StaticConfigSource::default()))
                .expect("Wafer::new");
        wafer
            .register_block(
                "impresspress/system",
                std::sync::Arc::new(impresspress_core::blocks::system::SystemBlock::new()),
            )
            .expect("register the system block");
        wafer
            .register_block("test/resets", std::sync::Arc::new(Resets(resets)))
            .expect("register test/resets");
        wafer.seal().await.expect("seal");
        wafer
            .init_block("impresspress/system")
            .await
            .expect("the system block initializes");
        let resets_init = wafer.init_block("test/resets").await;
        assert_eq!(resets_init.is_err(), resets, "{resets_init:?}");
        wafer
    }

    /// `GET /health` on `rt`: status, `Retry-After`, body.
    async fn health_of(rt: &ReadyRuntime) -> (u16, Option<String>, serde_json::Value) {
        let mut msg = wafer_run::Message::new("retrieve:/health");
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, "/health");
        let response = wafer_block::http_codec::collect_http_response(
            rt.wafer
                .run_block("impresspress/system", msg, wafer_run::InputStream::empty())
                .await,
        )
        .await;
        let retry_after = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
            .map(|(_, value)| value.clone());
        let body = serde_json::from_slice(&response.body).expect("a JSON body");
        (response.status, retry_after, body)
    }

    /// **`/health` reports each state the retry window moves an isolate
    /// through** (plan §6 I6), from the served runtime's slots and the
    /// isolate's state as `crate::init_isolate` registers them: degraded with
    /// the retrying block, which carries its run across rebuilds (I2g); stale
    /// when a rebuild fails transiently; failed when one fails permanently;
    /// ok once a healthy runtime is published. Every 503 carries the window's
    /// `Retry-After`.
    #[wasm_bindgen_test]
    async fn health_reports_each_state_of_the_retry_window() {
        reset_isolate();
        crate::init_isolate();
        let timer = FakeTimer::starting_at(START);
        let io = FakeIo::new(None);
        io.real_slots.set(true);

        // A degraded publish: `test/resets` retries on the reset.
        io.dynamic.set(Ends::Degraded);
        let (rt, _) = get_or_build_with(&io, &timer).await.expect("published");
        let armed = window().expect("armed");
        let (status, retry_after, body) = health_of(&rt).await;
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["status"], "degraded", "{body}");
        assert_eq!(body["retries"], 1, "{body}");
        assert_eq!(
            retry_after.as_deref(),
            Some(body["retry_in"].to_string().as_str()),
            "Retry-After is the report's retry_in"
        );
        let entry = &body["unavailable"][0];
        assert_eq!(entry["block"], "test/resets", "{body}");
        assert_eq!(entry["class"], "retrying", "{body}");
        assert_eq!(entry["runtimes"], 1, "{body}");
        let since = entry["since"].clone();
        assert!(since.is_string(), "{body}");
        assert_eq!(body["unavailable"].as_array().map(Vec::len), Some(1));
        assert!(!body.to_string().contains("CPU"), "no D1 text: {body}");

        // After the window, rebuilt degraded the same way: the same run, one
        // runtime longer; the episode keeps its start.
        timer.now.set(armed.retry_at_ms);
        let (rt, _) = get_or_build_with(&io, &timer).await.expect("rebuilt");
        assert_eq!(io.builds.get(), 2);
        let (status, _, body) = health_of(&rt).await;
        assert_eq!(status, 503);
        assert_eq!(body["unavailable"][0]["runtimes"], 2, "{body}");
        assert_eq!(body["unavailable"][0]["since"], since, "{body}");
        assert_eq!(body["retries"], 2, "{body}");

        // A transient rebuild failure keeps it serving, stale.
        timer.now.set(window().expect("armed").retry_at_ms);
        io.dynamic.set(Ends::Unavailable);
        let (rt, outcome) = get_or_build_with(&io, &timer).await.expect("kept");
        assert_eq!(outcome, CacheOutcome::Stale);
        let (status, retry_after, body) = health_of(&rt).await;
        assert_eq!(status, 503);
        assert!(retry_after.is_some());
        assert_eq!(body["status"], "stale", "{body}");
        assert_eq!(body["unavailable"][0]["block"], "test/resets", "{body}");
        assert_ne!(body["since"], serde_json::Value::Null, "{body}");

        // A permanent one answers its own request 500; the runtime serves on
        // and reports `failed`.
        timer.now.set(window().expect("armed").retry_at_ms);
        io.dynamic.set(Ends::Fault);
        get_or_build_with(&io, &timer)
            .await
            .expect_err("the failed rebuild's request");
        let (rt, _) = get_or_build_with(&io, &timer).await.expect("served inside");
        let (status, retry_after, body) = health_of(&rt).await;
        assert_eq!(status, 503);
        assert!(retry_after.is_some());
        assert_eq!(body["status"], "failed", "{body}");

        // A healthy publish: 200, nothing else.
        timer.now.set(window().expect("armed").retry_at_ms);
        io.dynamic.set(Ends::Healthy);
        let (rt, _) = get_or_build_with(&io, &timer).await.expect("healthy");
        let (status, retry_after, body) = health_of(&rt).await;
        assert_eq!((status, retry_after), (200, None), "{body}");
        assert_eq!(body, serde_json::json!({"status": "ok"}));
        reset_isolate();
    }
}
