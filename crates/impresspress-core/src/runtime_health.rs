//! Runtime health: what `GET /health` reports (D1 outage plan I6).
//!
//! A runtime can serve while part of it cannot: on Cloudflare a boot under
//! `InitPolicy::Serving` publishes without the blocks whose `Init` failed
//! transiently, and the isolate can keep serving a runtime whose config is
//! out of date while its backend is down. `/health` reports that state from
//! two sources, with no query of its own:
//!
//! - the runtime's own slots, [`Context::block_init_states`]: a block that is
//!   not `Ready` does not serve;
//! - what the platform knows about the runtime as a whole
//!   ([`PlatformHealth`]): whether it was published degraded, is serving
//!   stale config, or is serving on after a rebuild failed permanently, the
//!   retry window it will be rebuilt after, and whether its last check of
//!   the backend answered ([`BackendCheck`]). The platform registers where
//!   to read it from with [`set_platform_health_source`], as the Cloudflare
//!   entry does once per isolate; a platform that registers nothing (native,
//!   the browser) is reported from the slots alone.
//!
//! [`HealthReport::of`] combines them. Its status, by precedence:
//!
//! | status     | when                                                            |
//! |------------|-----------------------------------------------------------------|
//! | `failed`   | a block's `Init` failed permanently, or the last rebuild did    |
//! | `stale`    | the runtime serves config older than the database's             |
//! | `degraded` | a block does not serve (its `Init` is retrying), the runtime's variables read failed, or the platform's last check of the backend failed |
//! | `ok`       | none of the above                                               |
//!
//! A runtime that is both stale and degraded (a stale plan hydrated during
//! an outage usually is) reports `stale`, with the blocks that do not serve
//! still listed. Every status but `ok` is a 503 with `Retry-After`.
//!
//! # The backend, named apart from the runtime
//!
//! The status describes the runtime, and a runtime outlives the outage that
//! shaped it: a runtime whose variables read answered and whose block's
//! `Init` then failed on the backend stays `degraded`, listing that block,
//! for as long as the backend is down, because a retry that finds the backend
//! down builds nothing to replace it. The same body is what a block failing
//! its own `Init` while the backend answers gives (a query that makes D1
//! reset, plan I2g). A reader that must tell an outage from a fault of the
//! version — a deploy gate deciding whether to roll back — cannot tell them
//! apart from the status and the blocks.
//!
//! So a platform that rebuilds to retry (Cloudflare) reports what it last
//! saw of its database beside the status, as `backend` ([`BackendReport`],
//! from [`BackendCheck`]). Each retry or build is one attempt, and the
//! request that owns it records one observation when the attempt settles —
//! publishes, or fails — never before, so a body read mid-rebuild shows the
//! last settled attempt:
//!
//! - **`failing`** when the database could not answer the attempt
//!   ([`is_database_outage`]): the gate (the one read of the variables
//!   table every attempt starts with), a read after it (the dynamic path's
//!   settings, the grants), or a block's `Init` whose own database calls
//!   met the outage ([`retrying_on_the_database`]: wafer-run #483 records
//!   what each `Init` attempt's calls saw, whatever error the block then
//!   returned, a wrapped one or a hang cut off by the init budget included);
//! - **`ok`** when the gate answered and none of those failed that way, so a
//!   block still failing its `Init` failed on its own. A gate that failed
//!   on a statement the database ran (a missing table, a page too large: a
//!   fault of the version, which arms a failed episode) or on contention
//!   still answered, so it is `ok` too, and ends a failing run;
//! - **nothing new** when the gate's call was refused before it was sent
//!   (the invocation's call budget, the platform's per-invocation limit):
//!   the database was not reached.
//!
//! A runtime published while the backend is `failing` is published degraded,
//! whatever its blocks say, so a retry window is armed to check the database
//! again.
//!
//! Only the database counts: a config stamp in KV that cannot be read, or a
//! binding that is missing, is not `backend`.
//!
//! A failing backend makes the status at least `degraded`: whatever the
//! runtime holds, its routes that read the database do not serve. `failed`
//! still outranks it: the failure is one retrying will not fix. A platform
//! that does not rebuild to retry (native, the browser) reports no
//! `backend`, and the `ok` body stays `{"status":"ok"}`: an `ok` status
//! implies the last settled attempt, if any, found the database answering.
//!
//! The body names each block's state by its class
//! ([`BlockInitState::as_str`]), never the failure's text, which can quote
//! the backend. A block that has stayed `retrying` on the same failure
//! across rebuilds — a block whose own `Init` makes D1 reset, say, which the
//! transient classifier cannot tell from an outage — carries `since` and
//! `runtimes`, so it reads as the persistent failure it is rather than a
//! blip ([`RetryStreaks`]).
//!
//! Limitations:
//!
//! - A block that swallows its database errors at `Init` reports `ready`.
//!   Only a block whose `Init` fails loudly is listed.
//! - A run ([`RetryStreak`]) and the episode's `retries` live in one
//!   isolate. Cloudflare evicts isolates often, and a new one starts its
//!   runs again at `runtimes: 1`, so a monitor polling across isolates sees
//!   several short runs where the failure is one long one. What outlives the
//!   isolate (Workers Logs) is two `error!` lines: the episode's, once its
//!   window has been armed [`PERSISTENT_RETRIES`] times (a lasting outage,
//!   whether or not anything was rebuilt), and a run's, once it reaches
//!   [`PERSISTENT_RUNTIMES`] (a block failing the same way in every runtime
//!   built, which a gate that passes and a rebuild that degrades again
//!   produce: a block causing its own failure).
//! - `backend` is this isolate's, like the window, and only the slot owner
//!   writes it, once per attempt. It is as fresh as the window: the first
//!   request after the window ends retries before it is answered, so a
//!   `/health` request then reports its own attempt. Inside a window, or
//!   while another request's attempt runs, it reports the last settled one.
//! - Config reads are not attributed to the attempt (wafer-run #483): the
//!   config source reads the database directly, not through the handler. A
//!   block's pre-`Init` config load is classed by the source's own error; a
//!   block that reads config inside `Init` and wraps the error reads as
//!   failing on its own. On Cloudflare that read is the variables snapshot,
//!   which fails only after the gate read has, so `backend` is already
//!   `failing` then.
//! - A block whose own `Init` statement makes D1 fail (a query that makes D1
//!   reset, plan I2g) is answered with D1's outage, so it reads `failing` as
//!   an outage does. Its run's `runtimes` and the `error!` line at
//!   [`PERSISTENT_RUNTIMES`] are what tell it apart, over minutes, not one
//!   answer.
//! - A runtime's runs move only when a runtime is published. During a
//!   database-wide outage Cloudflare's retry window checks the backend with
//!   one read before it rebuilds anything, and a failed check publishes
//!   nothing, so each block's `runtimes` and `since` stay those of the last
//!   runtime built while `retries` counts on.
//! - A failure is identified by the **first** transient text its `Init`
//!   error contains ([`transient_text`](crate::sqlite_text_error::transient_text)),
//!   or by the whole error when it contains none. The backend's text reaches
//!   the error only when the platform itself read the database, as for a
//!   config-declaring block whose variables snapshot failed. On that path,
//!   two different texts can share one identity and extend one run (`D1_ERROR:
//!   Internal error in D1 DB storage caused object to be reset` and `D1_ERROR:
//!   internal error; reference = …` are both `D1_ERROR: internal error`), and
//!   one cause that surfaces as alternating texts restarts its run on each
//!   change.
//! - A block's own `Init` statement, sent through
//!   `wafer_core::clients::database`, sees only the database handler's
//!   scrubbed `database temporarily unavailable` (the D1 text is logged, not
//!   returned). Every transient database failure of that block is therefore
//!   one identity. Alternating texts extend its run, and so does an unrelated
//!   transient cause that follows, such as an outage after a self-inflicted
//!   reset. A run can then span two causes, and its `since` can predate the
//!   current one.

use std::{cell::Cell, collections::BTreeMap};

use serde::Serialize;
use wafer_block::wire::database::DATABASE_UNAVAILABLE;
use wafer_run::{context::Context, BlockInitState};

/// One block's run of consecutive published runtimes in which its `Init` was
/// retrying on the same failure ([`RetryStreaks`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryStreak {
    /// Which failure: its [`transient_text`](crate::sqlite_text_error::transient_text)
    /// when it is a known transient one (so two occurrences that differ only
    /// in a reference id are the same), else its whole text. Compared, never
    /// reported.
    failure: String,
    /// When the first runtime of the run was published.
    pub since_ms: u64,
    /// How many published runtimes the run spans, at least 1.
    pub runtimes: u32,
}

/// Every retrying block's [`RetryStreak`], carried from one published
/// runtime to the next by a platform that retries by rebuilding: its fresh
/// runtime's slots know only their own attempt, so on their own they report a
/// block that has failed the same way for an hour exactly as one that failed
/// once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetryStreaks(BTreeMap<String, RetryStreak>);

impl RetryStreaks {
    /// The streaks of a runtime published at `now_ms` whose slots are
    /// `states`, given the streaks of the runtime it replaces (`self`): a
    /// block retrying on the failure it was retrying on before extends its
    /// run, a block retrying on another failure starts one, and every other
    /// block has none. A run that reaches [`PERSISTENT_RUNTIMES`] is logged at
    /// `error!`, once.
    #[must_use]
    pub fn carried(&self, states: &BTreeMap<String, BlockInitState>, now_ms: u64) -> Self {
        Self(
            states
                .iter()
                .filter_map(|(block, state)| {
                    let BlockInitState::Retrying { failure, .. } = state else {
                        return None;
                    };
                    let failure = failure_identity(&failure.message);
                    let streak = match self.0.get(block) {
                        Some(previous) if previous.failure == failure => RetryStreak {
                            failure,
                            since_ms: previous.since_ms,
                            runtimes: previous.runtimes.saturating_add(1),
                        },
                        _ => RetryStreak {
                            failure,
                            since_ms: now_ms,
                            runtimes: 1,
                        },
                    };
                    if streak.is_newly_persistent() {
                        tracing::error!(
                            block = %block,
                            failure = %streak.failure,
                            runtimes = streak.runtimes,
                            since_ms = streak.since_ms,
                            "a block's Init has failed the same way in every runtime built \
                             for minutes: a lasting failure, possibly caused by the block \
                             itself (a query that makes D1 reset), not a passing outage"
                        );
                    }
                    Some((block.clone(), streak))
                })
                .collect(),
        )
    }

    /// `block`'s run, if it has one.
    #[must_use]
    pub fn get(&self, block: &str) -> Option<&RetryStreak> {
        self.0.get(block)
    }
}

/// How many consecutive published runtimes a block must retry on the same
/// failure in before its run is logged at `error!` ([`RetryStreaks::carried`]).
///
/// 8: the retry window doubles from 1 s and is capped at 60 s, so the 8th
/// runtime is published about 1+2+4+8+16+32+60 ≈ 2 minutes after the first.
/// D1's resets, overloads and lost connections clear within seconds, so a run
/// that long is not a blip. A runtime is published per window only while the
/// backend answers the window's check (Cloudflare's gate read): the block's
/// own `Init` is what keeps failing, the case `warn!` alone hid — a block
/// causing its own failure (plan I2g), such as a query that makes D1 reset.
/// A database-wide outage publishes nothing per window and is logged by the
/// episode instead ([`PERSISTENT_RETRIES`]). Lower, and an ordinary reset
/// storm would log errors; much higher, and a self-inflicted reset goes
/// unflagged for most of an isolate's life.
pub const PERSISTENT_RUNTIMES: u32 = 8;

/// How many times in a row a platform that retries by rebuilding must arm its
/// retry window — after a build that published degraded or failed, or after a
/// check of the backend that failed — before it logs the episode at `error!`,
/// once ([`RetryEpisode::retries`]).
///
/// 8, for [`PERSISTENT_RUNTIMES`]'s reason: about two minutes of windows, far
/// past what D1's passing errors last. It is the lasting-outage line (a quota
/// spent until midnight UTC, a database that will not come back), which a
/// block's run cannot carry when no runtime is published per window.
pub const PERSISTENT_RETRIES: u32 = 8;

impl RetryStreak {
    /// Whether this run has just reached [`PERSISTENT_RUNTIMES`] — true for
    /// exactly one runtime of the run.
    #[must_use]
    pub const fn is_newly_persistent(&self) -> bool {
        self.runtimes == PERSISTENT_RUNTIMES
    }
}

/// Whether a failure's detail code ([`wafer_run::WaferError::detail_code`])
/// says the database itself could not answer: an outage, an overload, a
/// reset, a quota that resets — a backend's `DatabaseError::Unavailable`,
/// whose detail code is [`DATABASE_UNAVAILABLE`]. The one test of "the
/// backend failed" behind `/health`'s `backend`: a statement the database ran
/// and failed (a missing table or column), contention (`database.contended`:
/// the database is up), a refused call (the invocation's budget) and a
/// block's own failure are not it.
#[must_use]
pub fn is_database_outage(detail_code: Option<&str>) -> bool {
    detail_code == Some(DATABASE_UNAVAILABLE)
}

/// Whether `state` is a block retrying because its database could not
/// answer its `Init` ([`is_database_outage`] over the failure's class: what
/// the attempt's own database calls saw, or the error's code when they saw
/// nothing).
#[must_use]
pub fn retrying_on_the_database(state: &BlockInitState) -> bool {
    matches!(state, BlockInitState::Retrying { failure, .. } if is_database_outage(failure.detail_code.as_deref()))
}

/// Which failure `reason` is, for [`RetryStreak::failure`].
fn failure_identity(reason: &str) -> String {
    crate::sqlite_text_error::transient_text(reason)
        .map_or_else(|| reason.to_owned(), str::to_owned)
}

/// The retry window of a platform that retries a runtime by rebuilding it
/// (Cloudflare's isolate retry window).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryEpisode {
    /// When the window was first armed: when this run of failed or degraded
    /// builds began.
    pub since_ms: u64,
    /// When the next retry may run: a check of the backend, and a rebuild if
    /// it answers.
    pub retry_at_ms: u64,
    /// How many times in a row the window has been armed, the first included:
    /// by a build that published degraded or failed (a cold build or a
    /// hydrate as well as a rebuild), or by a check of the backend that failed
    /// and built nothing. At least 1. It counts retries, not runtimes: a
    /// database-wide outage arms it once a window with no build at all.
    pub retries: u32,
    /// The last of them failed permanently: the runtime serving was built
    /// before it, and the next rebuild is likely to fail the same way.
    pub failed: bool,
}

/// A platform's record of what it has seen of its database: whether the
/// last settled attempt found it answering, and since when the attempts have
/// had that outcome (see the module doc). Built only through
/// [`after`](Self::after), so `since_ms <= checked_ms` always holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendCheck {
    answered: bool,
    since_ms: u64,
    checked_ms: u64,
}

impl BackendCheck {
    /// The record after an attempt settled at `at_ms` that found the
    /// database answering or not, given the record before it, if any: the
    /// same outcome extends its run, the other starts one. An observation
    /// older than the record's last one changes nothing: an attempt that
    /// settles late never moves `checked` back or replaces a newer outcome.
    /// Observations at the same instant are taken in the order they are
    /// reported.
    #[must_use]
    pub fn after(previous: Option<Self>, answered: bool, at_ms: u64) -> Self {
        match previous {
            Some(previous) if at_ms < previous.checked_ms => previous,
            Some(previous) if previous.answered == answered => Self {
                answered,
                since_ms: previous.since_ms,
                checked_ms: at_ms,
            },
            _ => Self {
                answered,
                since_ms: at_ms,
                checked_ms: at_ms,
            },
        }
    }

    /// Whether the last settled attempt found the database failing.
    #[must_use]
    pub const fn is_failing(&self) -> bool {
        !self.answered
    }

    /// When the current run of attempts with this outcome began: when the
    /// database began failing, or began answering again.
    #[must_use]
    pub const fn since_ms(&self) -> u64 {
        self.since_ms
    }

    /// When the last attempt settled.
    #[must_use]
    pub const fn checked_ms(&self) -> u64 {
        self.checked_ms
    }
}

/// What the platform knows about the runtime serving this isolate, beyond
/// its slots. [`Default`] is a platform with nothing to add.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlatformHealth {
    /// The retry window, while one is armed or awaiting its rebuild.
    pub episode: Option<RetryEpisode>,
    /// Published without a block whose `Init` failed transiently, or its
    /// variables read failed after it was published.
    pub degraded_since_ms: Option<u64>,
    /// Serving config older than the database's.
    pub stale_since_ms: Option<u64>,
    /// The serving runtime's [`RetryStreaks`].
    pub retry_streaks: RetryStreaks,
    /// What the platform has seen of its backend, if it rebuilds to retry
    /// and has looked.
    pub backend: Option<BackendCheck>,
}

/// Where [`platform_health`] reads from.
pub type PlatformHealthSource = fn() -> PlatformHealth;

thread_local! {
    /// Per thread, which is per isolate on Cloudflare, where the one source
    /// is registered. Native and the browser register none.
    static SOURCE: Cell<Option<PlatformHealthSource>> = const { Cell::new(None) };
}

/// Register where this thread (isolate) reads its [`PlatformHealth`] from,
/// returning the source it replaces (so a test can put it back). The
/// Cloudflare entry registers its runtime cache's once per isolate.
pub fn set_platform_health_source(
    source: Option<PlatformHealthSource>,
) -> Option<PlatformHealthSource> {
    SOURCE.with(|cell| cell.replace(source))
}

/// The registered source's [`PlatformHealth`], or the default when none is
/// registered.
#[must_use]
pub fn platform_health() -> PlatformHealth {
    SOURCE
        .with(Cell::get)
        .map_or_else(PlatformHealth::default, |source| source())
}

/// `/health`'s `status`, in increasing precedence: see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Ok,
    Degraded,
    Stale,
    Failed,
}

/// [`BackendReport::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendStatus {
    /// The last settled attempt found the database answering.
    Ok,
    /// The last settled attempt found it failing.
    Failing,
}

/// What the platform has seen of its backend ([`BackendCheck`]), as
/// `/health` reports it: `backend`, see the module doc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendReport {
    pub status: BackendStatus,
    /// When the run of attempts with this outcome began, RFC 3339: for
    /// `failing`, the first failed attempt of the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// When the last attempt settled, RFC 3339: for `failing`, when the
    /// database last failed one. Never before `since`. The next check runs when
    /// the retry window ends (`retry_in`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<String>,
}

impl BackendReport {
    /// `check` as the body reports it.
    #[must_use]
    pub fn of(check: BackendCheck) -> Self {
        Self {
            status: if check.answered {
                BackendStatus::Ok
            } else {
                BackendStatus::Failing
            },
            since: rfc3339(check.since_ms),
            checked: rfc3339(check.checked_ms),
        }
    }
}

/// A block that does not serve, as `/health` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnavailableBlock {
    pub block: String,
    /// Its [`BlockInitState::as_str`]: `retrying`, `failed`, `initializing`
    /// or `not_initialized`.
    pub class: &'static str,
    /// Whole seconds until its `Init` can run again, for a `retrying` block:
    /// the platform's retry window when one is armed (the rebuild is the
    /// retry), else the slot's own backoff. At least 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_in: Option<u32>,
    /// When the block began retrying on this failure ([`RetryStreak`]),
    /// RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// How many consecutive published runtimes it has been retrying on this
    /// failure in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtimes: Option<u32>,
}

/// The `/health` report. Serializes to its body: `{"status":"ok"}` when
/// healthy, otherwise the status with `since` (when known), `retry_in`,
/// `retries` (on a rebuilding platform), `backend` (on a platform that
/// rebuilds to retry) and `unavailable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthReport {
    pub status: HealthStatus,
    /// RFC 3339. For `degraded` and `failed`, the start of the outage
    /// episode (when the retry window was first armed), which can be before
    /// this status: a `failed` after ten minutes of `degraded` reports the
    /// `degraded` start. For `stale`, when the runtime became stale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// The `Retry-After` seconds, at least 1; `None` when `ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_in: Option<u32>,
    /// [`RetryEpisode::retries`]: how many times in a row the retry window
    /// has been armed, the first included.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    /// What the platform has seen of its backend; `None` when `ok`, or on a
    /// platform that does not rebuild to retry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendReport>,
    /// The blocks that do not serve, by name; `None` when `ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<Vec<UnavailableBlock>>,
}

impl HealthReport {
    /// The report for the runtime behind `ctx`, at `now_ms`: its slots and
    /// the registered [`PlatformHealth`]. Reads no backend.
    #[must_use]
    pub fn for_context(ctx: &dyn Context, now_ms: u64) -> Self {
        Self::of(&ctx.block_init_states(), &platform_health(), now_ms)
    }

    /// The report for slots `states` and `platform`, at `now_ms`.
    #[must_use]
    pub fn of(
        states: &BTreeMap<String, BlockInitState>,
        platform: &PlatformHealth,
        now_ms: u64,
    ) -> Self {
        // While the window is armed its end is when anything is retried: the
        // rebuild is the retry, and the slots' own backoff spaces nothing.
        let window_secs = platform
            .episode
            .filter(|episode| now_ms < episode.retry_at_ms)
            .map(|episode| whole_secs(episode.retry_at_ms - now_ms));

        let unavailable: Vec<UnavailableBlock> = states
            .iter()
            .filter(|(_, state)| !state.is_ready())
            .map(|(block, state)| {
                let retry_in = match state {
                    BlockInitState::Retrying { retry_in, .. } => {
                        Some(window_secs.unwrap_or_else(|| {
                            whole_secs(u64::try_from(retry_in.as_millis()).unwrap_or(u64::MAX))
                        }))
                    }
                    _ => None,
                };
                let streak = matches!(state, BlockInitState::Retrying { .. })
                    .then(|| platform.retry_streaks.get(block))
                    .flatten();
                UnavailableBlock {
                    block: block.clone(),
                    class: state.as_str(),
                    retry_in,
                    since: streak.and_then(|streak| rfc3339(streak.since_ms)),
                    runtimes: streak.map(|streak| streak.runtimes),
                }
            })
            .collect();

        let block_failed = states
            .values()
            .any(|state| matches!(state, BlockInitState::Failed { .. }));
        let rebuild_failed = platform.episode.is_some_and(|episode| episode.failed);
        let backend_failing = platform.backend.filter(BackendCheck::is_failing);
        let (status, since_ms) = if block_failed || rebuild_failed {
            (
                HealthStatus::Failed,
                platform
                    .episode
                    .filter(|episode| episode.failed)
                    .map(|episode| episode.since_ms),
            )
        } else if let Some(stale_since) = platform.stale_since_ms {
            (HealthStatus::Stale, Some(stale_since))
        } else if platform.degraded_since_ms.is_some()
            || backend_failing.is_some()
            || !unavailable.is_empty()
        {
            (
                HealthStatus::Degraded,
                platform
                    .episode
                    .map(|episode| episode.since_ms)
                    .or(platform.degraded_since_ms)
                    .or(backend_failing.map(|check| check.since_ms())),
            )
        } else {
            return Self {
                status: HealthStatus::Ok,
                since: None,
                retry_in: None,
                retries: None,
                backend: None,
                unavailable: None,
            };
        };

        let retry_in = window_secs
            .or_else(|| unavailable.iter().filter_map(|block| block.retry_in).max())
            .unwrap_or(1);
        Self {
            status,
            since: since_ms.and_then(rfc3339),
            retry_in: Some(retry_in),
            retries: platform.episode.map(|episode| episode.retries),
            backend: platform.backend.map(BackendReport::of),
            unavailable: Some(unavailable),
        }
    }

    /// The HTTP status: 200 when `ok`, else 503.
    #[must_use]
    pub const fn http_status(&self) -> u16 {
        match self.status {
            HealthStatus::Ok => 200,
            HealthStatus::Degraded | HealthStatus::Stale | HealthStatus::Failed => 503,
        }
    }
}

/// `ms` in whole seconds, rounded up, at least 1 (a `Retry-After: 0` invites
/// an immediate hammering retry).
fn whole_secs(ms: u64) -> u32 {
    u32::try_from(ms.div_ceil(1_000)).unwrap_or(u32::MAX).max(1)
}

/// `ms` since the epoch as RFC 3339 UTC, to the millisecond.
fn rfc3339(ms: u64) -> Option<String> {
    let ms = i64::try_from(ms).ok()?;
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    const NOW: u64 = 1_800_000_000_000;
    const RESET: &str = "D1_ERROR: D1 DB exceeded its CPU time limit and was reset.";
    const QUOTA: &str = "D1_ERROR: Your account has exceeded D1's free tier daily row read limit.";

    fn retrying(reason: &str, retry_in_ms: u64) -> BlockInitState {
        BlockInitState::Retrying {
            failure: wafer_run::TransientFailure::new(reason),
            retry_in: Duration::from_millis(retry_in_ms),
        }
    }

    fn states(entries: &[(&str, BlockInitState)]) -> BTreeMap<String, BlockInitState> {
        entries
            .iter()
            .map(|(block, state)| ((*block).to_string(), state.clone()))
            .collect()
    }

    fn body(report: &HealthReport) -> serde_json::Value {
        serde_json::to_value(report).expect("serializes")
    }

    #[test]
    fn every_block_ready_is_ok() {
        let report = HealthReport::of(
            &states(&[("a", BlockInitState::Ready), ("b", BlockInitState::Ready)]),
            &PlatformHealth::default(),
            NOW,
        );
        assert_eq!(report.http_status(), 200);
        assert_eq!(body(&report), json!({"status": "ok"}));
    }

    /// A retrying block makes the runtime degraded: 503, the block by its
    /// class, never the failure's text.
    #[test]
    fn a_retrying_block_is_degraded_and_its_reason_is_not_reported() {
        let report = HealthReport::of(
            &states(&[("a", BlockInitState::Ready), ("b", retrying(QUOTA, 2_500))]),
            &PlatformHealth::default(),
            NOW,
        );
        assert_eq!(report.http_status(), 503);
        assert_eq!(
            body(&report),
            json!({
                "status": "degraded",
                "retry_in": 3,
                "unavailable": [{"block": "b", "class": "retrying", "retry_in": 3}],
            })
        );
        assert!(!body(&report).to_string().contains("D1"), "no backend text");
    }

    /// Under an armed retry window, the window is when anything is retried:
    /// it is every retrying block's `retry_in` and the `Retry-After`, and the
    /// episode's start is `since`.
    #[test]
    fn the_retry_window_times_a_degraded_runtime() {
        let platform = PlatformHealth {
            episode: Some(RetryEpisode {
                since_ms: NOW - 600_000,
                retry_at_ms: NOW + 41_200,
                retries: 7,
                failed: false,
            }),
            degraded_since_ms: Some(NOW - 30_000),
            ..PlatformHealth::default()
        };
        let report = HealthReport::of(
            &states(&[("b", retrying(QUOTA, 0)), ("c", BlockInitState::Ready)]),
            &platform,
            NOW,
        );
        assert_eq!(
            body(&report),
            json!({
                "status": "degraded",
                "since": "2027-01-15T07:50:00.000Z",
                "retry_in": 42,
                "retries": 7,
                "unavailable": [{"block": "b", "class": "retrying", "retry_in": 42}],
            })
        );
    }

    /// Degraded with every block ready: the variables read failed after the
    /// runtime was published. Still a 503.
    #[test]
    fn a_runtime_degraded_after_publishing_is_degraded_with_no_block_listed() {
        let platform = PlatformHealth {
            degraded_since_ms: Some(NOW - 1_000),
            ..PlatformHealth::default()
        };
        let report = HealthReport::of(&states(&[("a", BlockInitState::Ready)]), &platform, NOW);
        assert_eq!(report.status, HealthStatus::Degraded);
        assert_eq!(report.http_status(), 503);
        assert_eq!(report.unavailable, Some(Vec::new()));
        assert_eq!(report.retry_in, Some(1));
    }

    /// Stale and degraded at once reports `stale` (the addendum's IP-C
    /// precedence note), with the blocks that do not serve still listed.
    #[test]
    fn stale_takes_precedence_over_degraded_and_keeps_the_list() {
        let platform = PlatformHealth {
            episode: Some(RetryEpisode {
                since_ms: NOW - 5_000,
                retry_at_ms: NOW + 1_000,
                retries: 2,
                failed: false,
            }),
            degraded_since_ms: Some(NOW - 2_000),
            stale_since_ms: Some(NOW - 5_000),
            ..PlatformHealth::default()
        };
        let report = HealthReport::of(&states(&[("b", retrying(QUOTA, 0))]), &platform, NOW);
        assert_eq!(report.http_status(), 503);
        assert_eq!(
            body(&report),
            json!({
                "status": "stale",
                "since": "2027-01-15T07:59:55.000Z",
                "retry_in": 1,
                "retries": 2,
                "unavailable": [{"block": "b", "class": "retrying", "retry_in": 1}],
            })
        );
    }

    /// A rebuild that failed permanently, with the runtime before it serving
    /// on: `failed`, over stale and degraded.
    #[test]
    fn a_permanent_rebuild_failure_is_failed() {
        let platform = PlatformHealth {
            episode: Some(RetryEpisode {
                since_ms: NOW - 3_000,
                retry_at_ms: NOW + 4_000,
                retries: 3,
                failed: true,
            }),
            stale_since_ms: Some(NOW - 9_000),
            ..PlatformHealth::default()
        };
        let report = HealthReport::of(&states(&[("a", BlockInitState::Ready)]), &platform, NOW);
        assert_eq!(report.http_status(), 503);
        assert_eq!(
            body(&report),
            json!({
                "status": "failed",
                "since": "2027-01-15T07:59:57.000Z",
                "retry_in": 4,
                "retries": 3,
                "unavailable": [],
            })
        );
    }

    /// A block whose `Init` failed permanently is `failed` and listed without
    /// a `retry_in`: its runtime never retries it.
    #[test]
    fn a_failed_block_is_failed() {
        let report = HealthReport::of(
            &states(&[
                (
                    "a",
                    BlockInitState::Failed {
                        reason: "no such table: x".into(),
                    },
                ),
                ("b", retrying(QUOTA, 1_000)),
            ]),
            &PlatformHealth::default(),
            NOW,
        );
        assert_eq!(report.http_status(), 503);
        assert_eq!(
            body(&report),
            json!({
                "status": "failed",
                "retry_in": 1,
                "unavailable": [
                    {"block": "a", "class": "failed"},
                    {"block": "b", "class": "retrying", "retry_in": 1},
                ],
            })
        );
    }

    /// Plan I2g: a block that stays retrying on the same failure across
    /// rebuilt runtimes carries the run's start and length, whatever detail
    /// differs between occurrences; a new failure starts a new run; a block
    /// that recovers loses its run.
    #[test]
    fn a_block_retrying_on_the_same_failure_across_runtimes_carries_its_run() {
        let first = RetryStreaks::default().carried(
            &states(&[("b", retrying(RESET, 0)), ("c", retrying(QUOTA, 0))]),
            NOW - 120_000,
        );
        let second = first.carried(
            &states(&[
                ("b", retrying(&format!("{RESET}: undefined"), 0)),
                ("c", BlockInitState::Ready),
            ]),
            NOW - 60_000,
        );
        let third = second.carried(
            &states(&[("b", retrying(RESET, 0)), ("c", retrying(QUOTA, 0))]),
            NOW,
        );
        let b = third.get("b").expect("b's run");
        assert_eq!((b.since_ms, b.runtimes), (NOW - 120_000, 3));
        let c = third.get("c").expect("c's new run");
        assert_eq!((c.since_ms, c.runtimes), (NOW, 1));

        let fourth = third.carried(&states(&[("b", retrying(QUOTA, 0))]), NOW + 60_000);
        let b = fourth.get("b").expect("b's new run");
        assert_eq!(
            (b.since_ms, b.runtimes),
            (NOW + 60_000, 1),
            "another failure"
        );
        assert!(fourth.get("c").is_none(), "c has no state, so no run");

        let report = HealthReport::of(
            &states(&[("b", retrying(RESET, 0))]),
            &PlatformHealth {
                retry_streaks: third,
                ..PlatformHealth::default()
            },
            NOW,
        );
        assert_eq!(
            body(&report)["unavailable"],
            json!([{
                "block": "b",
                "class": "retrying",
                "retry_in": 1,
                "since": "2027-01-15T07:58:00.000Z",
                "runtimes": 3,
            }])
        );
    }

    /// The window has ended and the rebuild it allows has not run yet (the
    /// first request after it owns the rebuild; others are served the cached
    /// runtime meanwhile): `retry_in` falls back to the slots' backoff, or 1,
    /// never `retry_at - now` underflowed; `since` and `retries` stay.
    #[test]
    fn an_elapsed_window_awaiting_its_rebuild_times_nothing() {
        let platform = PlatformHealth {
            episode: Some(RetryEpisode {
                since_ms: NOW - 90_000,
                retry_at_ms: NOW - 2_000,
                retries: 4,
                failed: false,
            }),
            degraded_since_ms: Some(NOW - 30_000),
            ..PlatformHealth::default()
        };
        let report = HealthReport::of(
            &states(&[("b", retrying(QUOTA, 0)), ("c", retrying(QUOTA, 2_500))]),
            &platform,
            NOW,
        );
        assert_eq!(
            body(&report),
            json!({
                "status": "degraded",
                "since": "2027-01-15T07:58:30.000Z",
                "retry_in": 3,
                "retries": 4,
                "unavailable": [
                    {"block": "b", "class": "retrying", "retry_in": 1},
                    {"block": "c", "class": "retrying", "retry_in": 3},
                ],
            })
        );
    }

    /// A run is newly persistent in exactly one runtime: the
    /// [`PERSISTENT_RUNTIMES`]th, when `carried` logs it.
    #[test]
    fn a_run_becomes_persistent_once_at_the_threshold() {
        let slots = states(&[("b", retrying(RESET, 0))]);
        let mut streaks = RetryStreaks::default();
        let mut flagged = Vec::new();
        for n in 1..=PERSISTENT_RUNTIMES + 3 {
            streaks = streaks.carried(&slots, NOW + u64::from(n));
            let run = streaks.get("b").expect("b's run");
            assert_eq!(run.runtimes, n);
            if run.is_newly_persistent() {
                flagged.push(n);
            }
        }
        assert_eq!(flagged, vec![PERSISTENT_RUNTIMES]);
    }

    /// A block mid-`Init` or not yet initialized does not serve either.
    #[test]
    fn an_unsettled_block_is_degraded() {
        let report = HealthReport::of(
            &states(&[
                ("a", BlockInitState::Initializing),
                ("b", BlockInitState::NotInitialized),
            ]),
            &PlatformHealth::default(),
            NOW,
        );
        assert_eq!(
            body(&report),
            json!({
                "status": "degraded",
                "retry_in": 1,
                "unavailable": [
                    {"block": "a", "class": "initializing"},
                    {"block": "b", "class": "not_initialized"},
                ],
            })
        );
    }

    /// A check that has the last one's outcome extends its run; the other
    /// outcome starts one. `checked` is always the last.
    #[test]
    fn a_backend_check_extends_or_starts_its_run() {
        let first = BackendCheck::after(None, false, NOW);
        assert_eq!(
            (first.answered, first.since_ms, first.checked_ms),
            (false, NOW, NOW)
        );
        let second = BackendCheck::after(Some(first), false, NOW + 2_000);
        assert_eq!((second.since_ms, second.checked_ms), (NOW, NOW + 2_000));
        let back = BackendCheck::after(Some(second), true, NOW + 5_000);
        assert_eq!(
            (back.answered, back.since_ms, back.checked_ms),
            (true, NOW + 5_000, NOW + 5_000)
        );
        assert!(second.is_failing() && !back.is_failing());
    }

    /// An observation older than the record's last changes nothing: it
    /// cannot move `checked` back, put it before `since`, or replace a newer
    /// outcome. One at the same instant is taken in order.
    #[test]
    fn an_older_observation_changes_nothing() {
        let ok = BackendCheck::after(None, true, NOW);
        assert_eq!(
            BackendCheck::after(Some(ok), false, NOW - 1),
            ok,
            "older failure"
        );
        let failing = BackendCheck::after(Some(ok), false, NOW + 1_000);
        assert_eq!(
            BackendCheck::after(Some(failing), false, NOW + 500),
            failing,
            "an older failure does not move checked back"
        );
        assert_eq!(
            BackendCheck::after(Some(failing), true, NOW),
            failing,
            "an older answer does not replace a newer failure"
        );
        let same_instant = BackendCheck::after(Some(failing), true, NOW + 1_000);
        assert!(!same_instant.is_failing());
        for check in [ok, failing, same_instant] {
            assert!(check.since_ms() <= check.checked_ms(), "{check:?}");
        }
    }

    /// The reported outage (FOLLOWUPS "health body not naming a failed
    /// backend check"): a runtime published degraded — its variables read
    /// answered, then a block's `Init` failed on the backend — that a failed
    /// check keeps serving. Its slots and episode are those of a block failing
    /// its own `Init`; `backend` is what tells the two apart.
    #[test]
    fn a_degraded_runtime_names_whether_its_backend_check_failed() {
        let platform = |answered: bool| PlatformHealth {
            episode: Some(RetryEpisode {
                since_ms: NOW - 10_000,
                retry_at_ms: NOW + 3_200,
                retries: 3,
                failed: false,
            }),
            degraded_since_ms: Some(NOW - 10_000),
            backend: Some(BackendCheck::after(
                Some(BackendCheck::after(None, answered, NOW - 7_000)),
                answered,
                NOW - 800,
            )),
            ..PlatformHealth::default()
        };
        let slots = states(&[("a", BlockInitState::Ready), ("b", retrying(QUOTA, 0))]);

        let outage = HealthReport::of(&slots, &platform(false), NOW);
        assert_eq!(outage.http_status(), 503);
        assert_eq!(
            body(&outage),
            json!({
                "status": "degraded",
                "since": "2027-01-15T07:59:50.000Z",
                "retry_in": 4,
                "retries": 3,
                "backend": {
                    "status": "failing",
                    "since": "2027-01-15T07:59:53.000Z",
                    "checked": "2027-01-15T07:59:59.200Z",
                },
                "unavailable": [{"block": "b", "class": "retrying", "retry_in": 4}],
            })
        );

        let own_fault = HealthReport::of(&slots, &platform(true), NOW);
        assert_eq!(own_fault.status, HealthStatus::Degraded);
        assert_eq!(body(&own_fault)["backend"]["status"], "ok");
    }

    /// A failed check makes the status at least `degraded`, with every block
    /// ready and nothing else to report: the routes that read the backend do
    /// not serve. Its run's start is `since` when nothing older is known.
    #[test]
    fn a_failing_backend_check_is_degraded_with_every_block_ready() {
        let platform = PlatformHealth {
            backend: Some(BackendCheck::after(None, false, NOW - 4_000)),
            ..PlatformHealth::default()
        };
        let report = HealthReport::of(&states(&[("a", BlockInitState::Ready)]), &platform, NOW);
        assert_eq!(report.http_status(), 503);
        assert_eq!(
            body(&report),
            json!({
                "status": "degraded",
                "since": "2027-01-15T07:59:56.000Z",
                "retry_in": 1,
                "backend": {
                    "status": "failing",
                    "since": "2027-01-15T07:59:56.000Z",
                    "checked": "2027-01-15T07:59:56.000Z",
                },
                "unavailable": [],
            })
        );
    }

    /// A healthy runtime whose last check answered is `{"status":"ok"}`, as
    /// on a platform that checks nothing: `ok` implies the check answered.
    /// Only the database being down is the backend failing: a block whose
    /// attempt saw contention, a spent call budget, or no database failure
    /// at all is the version's own state.
    #[test]
    fn only_a_database_outage_is_a_block_retrying_on_the_database() {
        use wafer_block::wire::database::{CALL_BUDGET_EXHAUSTED, DATABASE_CONTENDED};
        let retrying_with = |code: Option<&str>| BlockInitState::Retrying {
            failure: wafer_run::TransientFailure::new("x").classed(code),
            retry_in: Duration::ZERO,
        };
        assert!(retrying_on_the_database(&retrying_with(Some(
            DATABASE_UNAVAILABLE
        ))));
        for code in [
            Some(DATABASE_CONTENDED),
            Some(CALL_BUDGET_EXHAUSTED),
            Some("auth.invalid_token"),
            None,
        ] {
            assert!(!retrying_on_the_database(&retrying_with(code)), "{code:?}");
        }
        assert!(!retrying_on_the_database(&BlockInitState::Ready));
    }

    #[test]
    fn an_answering_backend_adds_nothing_to_the_ok_body() {
        let platform = PlatformHealth {
            backend: Some(BackendCheck::after(None, true, NOW - 1)),
            ..PlatformHealth::default()
        };
        let report = HealthReport::of(&states(&[("a", BlockInitState::Ready)]), &platform, NOW);
        assert_eq!(report.http_status(), 200);
        assert_eq!(body(&report), json!({"status": "ok"}));
    }

    /// A permanent failure outranks the failing check: `failed`, with the
    /// check still named.
    #[test]
    fn a_failed_episode_outranks_a_failing_backend_check() {
        let platform = PlatformHealth {
            episode: Some(RetryEpisode {
                since_ms: NOW - 3_000,
                retry_at_ms: NOW + 1_000,
                retries: 1,
                failed: true,
            }),
            backend: Some(BackendCheck::after(None, false, NOW - 3_000)),
            ..PlatformHealth::default()
        };
        let report = HealthReport::of(&states(&[("a", BlockInitState::Ready)]), &platform, NOW);
        assert_eq!(report.status, HealthStatus::Failed);
        assert_eq!(body(&report)["backend"]["status"], "failing");
    }

    /// The registered source is what the report reads; none is the default.
    #[test]
    fn the_registered_source_supplies_the_platform_health() {
        fn stale() -> PlatformHealth {
            PlatformHealth {
                stale_since_ms: Some(NOW - 1),
                ..PlatformHealth::default()
            }
        }
        let before = set_platform_health_source(None);
        assert_eq!(platform_health(), PlatformHealth::default());
        set_platform_health_source(Some(stale));
        assert_eq!(platform_health().stale_since_ms, Some(NOW - 1));
        set_platform_health_source(before);
    }
}
