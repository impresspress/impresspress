//! Pure formatting for the Cloudflare adapter's lightweight observability
//! signals — the 2026-07-16 Cloudflare audit follow-up ("Add Server-Timing/
//! metrics for runtime-cache hits, version probes, D1 primitive statements
//! per logical call, D1 rows read/written, runtime builds, body sizes, and
//! background log failures").
//!
//! Lives in `impresspress-core` (not `impresspress-cloudflare`) so it's
//! host-testable: `impresspress-cloudflare` is wasm32-only and excluded
//! from `cargo test --workspace` — its R2/D1 service impls require
//! `!Send` futures that don't compile on a native target, so nothing in
//! that crate can run via `cargo test -p impresspress-cloudflare`. Follows
//! the `cache_key`/`kv` extraction precedent: pure logic that a wasm-only
//! adapter would otherwise leave untested moves here.
//!
//! `impresspress-cloudflare::runtime_cache::get_or_build` computes
//! [`CacheOutcome`] for free from branches it already takes (no extra
//! probe, no extra allocation on the request hot path).
//! `impresspress-cloudflare::lib::run_inner` turns it into a `Server-Timing`
//! response header via [`server_timing_header`] — gated to the Cloudflare
//! console logger's Debug (dev) level, since an unconditional header would
//! disclose per-request cache/rebuild state, including the isolate build
//! counter (a signal for when a config bump landed), to every anonymous
//! production client — and formats off-response-path background failures
//! (audit-log batch persist, delayed config-version retry — both run inside
//! `ctx.wait_until` closures *after* the response has already been sent, so
//! they can never reach a response header) via [`metric_line`].
//!
//! FOLLOW-UP (flagged, not implemented here): per-request D1 primitive
//! statement count and rows read/written need a counter threaded through
//! `wafer-core`'s `DbExec`/`exec.rs` — a wafer-run change, out of scope for
//! this impresspress-cloudflare-local pass (see
//! `docs/CODE_REVIEW_2026-07-16_FINDINGS.md`, "Eliminate normal-request D1
//! schema introspection"). [`CacheOutcome`]'s per-isolate cumulative build
//! count is the tractable proxy used instead: a rebuild pays several D1
//! reads (block_settings, WRAP grants, ...), so a rising count on an
//! otherwise-idle isolate is a real cost signal reachable without that
//! plumbing.

/// Coarse outcome of one `runtime_cache::get_or_build` call — the cheapest
/// useful metrics signal for the two costs the audit named: whether this
/// request paid a KV version-probe, and whether it paid a full runtime
/// rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheOutcome {
    /// Fastest path: within the jittered probe window and not dirty — no
    /// KV read, no rebuild.
    Hit,
    /// Probe window elapsed (or an explicit dirty flag) but the KV version
    /// stamp still matched — one KV read, no rebuild.
    ProbedFresh,
    /// Version stamp moved, or a local write is pending: full rebuild.
    /// `build_ordinal` is this isolate's cumulative build count (1-based).
    Rebuilt { build_ordinal: u32 },
    /// Nothing cached yet in this isolate: cold build. `build_ordinal` is
    /// this isolate's cumulative build count (1-based; always 1 the first
    /// time, but a prior rebuild that raced and lost — see
    /// `runtime_cache`'s module doc on concurrent-first-request races —
    /// could make a later cold path start higher).
    ColdBuilt { build_ordinal: u32 },
}

impl CacheOutcome {
    /// Short machine-parseable label used in the `Server-Timing` header.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::ProbedFresh => "probed-fresh",
            Self::Rebuilt { .. } => "rebuilt",
            Self::ColdBuilt { .. } => "cold-built",
        }
    }

    /// This isolate's cumulative runtime-build count, if this outcome paid
    /// for a build; `None` for `Hit`/`ProbedFresh`.
    pub fn build_ordinal(self) -> Option<u32> {
        match self {
            Self::Hit | Self::ProbedFresh => None,
            Self::Rebuilt { build_ordinal } | Self::ColdBuilt { build_ordinal } => {
                Some(build_ordinal)
            }
        }
    }
}

/// Build the `Server-Timing` response-header value for one dispatched
/// request. Format: `<metric>;desc="<value>"` per the
/// [Server-Timing spec](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Server-Timing).
///
/// `dur` is defined by the spec as a *duration in milliseconds* — DevTools
/// and RUM tooling render it as a timing bar — so the per-isolate
/// build-ordinal counter (an integer count, not a time) must never be
/// passed as `dur`; that would render e.g. a 3rd rebuild as "3ms", which is
/// actively misleading. When this request paid for a build, the ordinal is
/// folded into the `desc` text instead (`"rebuilt (build 3)"`); a
/// Server-Timing entry is valid with just name+desc and no `dur`.
pub fn server_timing_header(outcome: CacheOutcome) -> String {
    match outcome.build_ordinal() {
        Some(n) => format!("cache;desc=\"{} (build {n})\"", outcome.as_str()),
        None => format!("cache;desc=\"{}\"", outcome.as_str()),
    }
}

/// Format a structured, grep-able metric line for `console_log!`. Used for
/// signals that occur off the response path — inside `ctx.wait_until`
/// closures, after the response has already been sent — which can never
/// reach a `Server-Timing` header.
pub fn metric_line(name: &str, fields: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(8 + name.len());
    out.push_str("metric=");
    out.push_str(name);
    for (k, v) in fields {
        out.push(' ');
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_without_build_is_single_entry() {
        assert_eq!(
            server_timing_header(CacheOutcome::Hit),
            r#"cache;desc="hit""#
        );
        assert_eq!(
            server_timing_header(CacheOutcome::ProbedFresh),
            r#"cache;desc="probed-fresh""#
        );
    }

    #[test]
    fn header_with_build_folds_ordinal_into_desc_not_dur() {
        // `dur` is milliseconds per the Server-Timing spec — the build
        // ordinal is a count, not a duration, so it must never land there
        // (DevTools/RUM would render e.g. "rtbuild;dur=3" as a 3ms bar).
        let with_build = server_timing_header(CacheOutcome::Rebuilt { build_ordinal: 3 });
        assert_eq!(with_build, r#"cache;desc="rebuilt (build 3)""#);
        assert!(!with_build.contains("dur="));

        assert_eq!(
            server_timing_header(CacheOutcome::ColdBuilt { build_ordinal: 1 }),
            r#"cache;desc="cold-built (build 1)""#
        );
    }

    #[test]
    fn build_ordinal_none_for_hit_and_probed_fresh() {
        assert_eq!(CacheOutcome::Hit.build_ordinal(), None);
        assert_eq!(CacheOutcome::ProbedFresh.build_ordinal(), None);
        assert_eq!(
            CacheOutcome::Rebuilt { build_ordinal: 7 }.build_ordinal(),
            Some(7)
        );
    }

    #[test]
    fn metric_line_formats_structured_fields() {
        assert_eq!(
            metric_line(
                "audit_log_persist_failed",
                &[("table", "requests"), ("rows", "3")]
            ),
            "metric=audit_log_persist_failed table=requests rows=3"
        );
    }

    #[test]
    fn metric_line_with_no_fields() {
        assert_eq!(
            metric_line("config_version_retry_failed", &[]),
            "metric=config_version_retry_failed"
        );
    }
}
