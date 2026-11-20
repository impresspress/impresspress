//! Pure rules for the runtime's config-version probe: when an observed
//! version means "config changed", how long the probe window is, and how
//! long a saved admin change can take to reach every server.
//!
//! The Cloudflare runtime cache (`impresspress-cloudflare` `runtime_cache.rs`)
//! is the only consumer today. The rules live here, in plain functions, so
//! the host test suite exercises them: that crate's own unit tests are
//! wasm-only and are not run in CI.

use crate::config_vars::config_probe_interval_secs;
use crate::prepared_plan::UNBOUND_CONFIG_GENERATION;

/// What the probe reports when the config-version key cannot be READ (a KV
/// error, including a plan's KV read quota running out). A constant, so every
/// isolate that hits the error converges on the same value.
pub const VERSION_UNAVAILABLE: &str = "<kv-unavailable>";

/// Default probe window: a 30s floor plus up to 30s of jitter.
pub const DEFAULT_PROBE_FLOOR_MS: u64 = 30_000;
pub const DEFAULT_PROBE_JITTER_MS: u64 = 30_000;

/// After an UNREADABLE probe, look again this soon rather than waiting a
/// whole (possibly long) interval: KV usually recovers, and a change made
/// meanwhile should not wait for the next full window. The first retry; it
/// backs off from here ([`unreadable_retry_window_ms`]).
pub const RETRY_AFTER_UNREADABLE_MS: u64 = 30_000;

/// (base, jitter width) in ms for the next probe after an unreadable read,
/// given how long the outage has lasted (`outage_ms`, since the first failed
/// read in this isolate) and the normal probe window (floor, jitter).
///
/// Time-based, not count-based: many concurrent reads failing in the same
/// second do not escalate it, so a brief blip is re-checked soon. The delay
/// follows the outage's age, at least [`RETRY_AFTER_UNREADABLE_MS`], which
/// grows geometrically (each retry roughly doubles the elapsed time). Once the
/// outage is as long as the normal floor, this IS the normal window, jitter
/// included, so a retry never waits longer than an ordinary probe (and the
/// admin UI's propagation bound holds). With the default 30-60s window there is
/// nothing to back off to: the normal rate is already ~30s.
pub fn unreadable_retry_window_ms(outage_ms: u64, probe_window: (u64, u64)) -> (u64, u64) {
    let (floor, jitter) = probe_window;
    if outage_ms >= floor {
        return (floor, jitter.max(1));
    }
    let base = outage_ms
        .max(RETRY_AFTER_UNREADABLE_MS)
        .min(floor.max(RETRY_AFTER_UNREADABLE_MS));
    (base, (base / 2).min(jitter).max(1))
}

/// The isolate's outage start after a config-version read at `now`: the first
/// failure starts it, later failures keep it, any success clears it.
pub fn next_outage_start(previous: Option<u64>, now: u64, read_ok: bool) -> Option<u64> {
    if read_ok {
        None
    } else {
        Some(previous.unwrap_or(now))
    }
}

/// Upper bound on KV's own propagation of a new version stamp, used only for
/// what the admin UI tells people (Cloudflare documents ~60s).
pub const KV_PROPAGATION_ALLOWANCE_SECS: u64 = 60;

/// Did the config change? An UNREADABLE version is not a change.
///
/// Comparing [`VERSION_UNAVAILABLE`] like a real stamp never matches, so every
/// probe during a KV failure used to abandon the runtime for the slow dynamic
/// build, which cannot fit the Workers CPU budget: a KV outage became a site
/// outage. Holding the current runtime is the safe reading.
///
/// A runtime that was itself TAGGED with the sentinel (a dynamic build made
/// while KV was unreadable) does count as changed once a real stamp is read:
/// it cannot know what it missed, so it rebuilds once. Prepared runtimes are
/// tagged with their plan's generation instead, so on the normal path KV
/// recovery causes no rebuild at all.
pub fn config_version_changed(cached: &str, observed: &str) -> bool {
    observed != VERSION_UNAVAILABLE && cached != observed
}

/// Is a packaged prepared plan usable against the observed generation?
///
/// A BOUND plan matches its own generation. With KV unreadable there is
/// nothing to compare against, and the plan is trusted (it was current when
/// deployed). That is a deliberate availability-over-freshness trade: if an
/// admin changed config after the deploy AND KV is then unreadable, a cold
/// isolate serves the deploy-time config until KV answers again, instead of
/// every cold isolate falling into the dynamic build and failing. An unbound
/// plan is never trusted.
pub fn prepared_plan_usable(plan_generation: &str, observed_generation: &str) -> bool {
    plan_generation != UNBOUND_CONFIG_GENERATION
        && (observed_generation == VERSION_UNAVAILABLE || plan_generation == observed_generation)
}

/// (floor, jitter) in ms for an interval in seconds (already clamped).
/// Jitter is 10% of the floor but never less than the default 30s, so a
/// short setting is never probed more often, or more in lockstep, than
/// leaving the variable unset.
fn window_for_secs(secs: u64) -> (u64, u64) {
    let floor = secs * 1000;
    (floor, (floor / 10).max(DEFAULT_PROBE_JITTER_MS))
}

/// (floor, jitter) in ms for a configured interval value: the default 30s +
/// 0..30s when unset or unparsable.
pub fn probe_window_ms(interval: Option<&str>) -> (u64, u64) {
    match config_probe_interval_secs(interval) {
        Some(secs) => window_for_secs(secs),
        None => (DEFAULT_PROBE_FLOOR_MS, DEFAULT_PROBE_JITTER_MS),
    }
}

/// The longest a saved change can take to reach every server, in seconds:
/// one full jittered window plus KV's own propagation. None when the interval
/// is not configured (the admin UI then says nothing).
pub fn worst_case_propagation_secs(interval: Option<&str>) -> Option<u64> {
    let (floor, jitter) = window_for_secs(config_probe_interval_secs(interval)?);
    Some((floor + jitter).div_ceil(1000) + KV_PROPAGATION_ALLOWANCE_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_version_is_never_a_change() {
        assert!(!config_version_changed("v1", VERSION_UNAVAILABLE));
        assert!(!config_version_changed("v1", "v1"));
        assert!(config_version_changed("v1", "v2"));
        // The sentinel stored as a runtime's version still lets a real
        // stamp through once KV answers.
        assert!(config_version_changed(VERSION_UNAVAILABLE, "v2"));
    }

    #[test]
    fn a_bound_plan_is_trusted_while_kv_is_unreadable_an_unbound_one_never() {
        let v1 = "1".repeat(32);
        let v2 = "2".repeat(32);
        assert!(prepared_plan_usable(&v1, &v1));
        assert!(!prepared_plan_usable(&v1, &v2));
        assert!(prepared_plan_usable(&v1, VERSION_UNAVAILABLE));
        assert!(!prepared_plan_usable(
            UNBOUND_CONFIG_GENERATION,
            VERSION_UNAVAILABLE
        ));
        assert!(!prepared_plan_usable(
            UNBOUND_CONFIG_GENERATION,
            UNBOUND_CONFIG_GENERATION
        ));
    }

    #[test]
    fn probe_window_defaults_clamps_and_jitters() {
        assert_eq!(probe_window_ms(None), (30_000, 30_000));
        assert_eq!(probe_window_ms(Some("garbage")), (30_000, 30_000));
        assert_eq!(probe_window_ms(Some("600")), (600_000, 60_000));
        // A short setting keeps the default jitter: never more frequent or
        // more lockstep than unset.
        assert_eq!(probe_window_ms(Some("5")), (30_000, 30_000));
        assert_eq!(probe_window_ms(Some("30")), (30_000, 30_000));
        assert_eq!(probe_window_ms(Some("999999")), (3_600_000, 360_000));
    }

    #[test]
    fn unreadable_retries_follow_the_outage_age_then_the_normal_window() {
        let normal = (600_000, 60_000);
        // A fresh outage (or a burst of failures in the same second): 30s.
        assert_eq!(unreadable_retry_window_ms(0, normal), (30_000, 15_000));
        assert_eq!(unreadable_retry_window_ms(5_000, normal), (30_000, 15_000));
        // Grows with the outage, jitter never above the normal jitter.
        assert_eq!(
            unreadable_retry_window_ms(120_000, normal),
            (120_000, 60_000)
        );
        assert_eq!(
            unreadable_retry_window_ms(300_000, normal),
            (300_000, 60_000)
        );
        // At or past the floor: exactly the normal window, never longer.
        assert_eq!(unreadable_retry_window_ms(600_000, normal), normal);
        assert_eq!(unreadable_retry_window_ms(u64::MAX, normal), normal);
        // Default window: already ~30s, nothing to back off to.
        assert_eq!(
            unreadable_retry_window_ms(10_000, (30_000, 30_000)),
            (30_000, 15_000)
        );
        assert_eq!(
            unreadable_retry_window_ms(40_000, (30_000, 30_000)),
            (30_000, 30_000)
        );
    }

    #[test]
    fn the_outage_starts_on_the_first_failure_and_clears_on_success() {
        assert_eq!(next_outage_start(None, 100, false), Some(100));
        assert_eq!(
            next_outage_start(Some(100), 900, false),
            Some(100),
            "later failures keep the start"
        );
        assert_eq!(next_outage_start(Some(100), 900, true), None);
        assert_eq!(next_outage_start(None, 900, true), None);
    }

    #[test]
    fn worst_case_includes_jitter_and_kv_propagation() {
        assert_eq!(worst_case_propagation_secs(None), None);
        // 600s + 60s jitter + 60s KV = 720s, i.e. 12 minutes.
        assert_eq!(worst_case_propagation_secs(Some("600")), Some(720));
        assert_eq!(worst_case_propagation_secs(Some("30")), Some(120));
    }
}
