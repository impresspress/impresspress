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
    /// Probe window elapsed but the KV version GET itself failed
    /// (quota/transport). The last known runtime was served unchanged and the
    /// probe deadline was pushed out with backoff — no rebuild, no KV write.
    ProbeFailed,
    /// Version stamp moved, or a local write is pending: full rebuild.
    /// `build_ordinal` is this isolate's cumulative build count (1-based);
    /// `duration_ms` covers cache resolution through the completed build.
    Rebuilt {
        build_ordinal: u32,
        duration_ms: u64,
    },
    /// Nothing cached yet in this isolate: cold build. `build_ordinal` is
    /// this isolate's cumulative build count (1-based; always 1 on the first
    /// successful build because runtime construction is single-flight).
    /// `duration_ms` covers the initial version probe through the completed
    /// build.
    ColdBuilt {
        build_ordinal: u32,
        duration_ms: u64,
    },
}

impl CacheOutcome {
    /// Short machine-parseable label used in the `Server-Timing` header.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::ProbedFresh => "probed-fresh",
            Self::ProbeFailed => "probe-failed",
            Self::Rebuilt { .. } => "rebuilt",
            Self::ColdBuilt { .. } => "cold-built",
        }
    }

    /// This isolate's cumulative runtime-build count, if this outcome paid
    /// for a build; `None` for `Hit`/`ProbedFresh`.
    pub fn build_ordinal(self) -> Option<u32> {
        match self {
            Self::Hit | Self::ProbedFresh | Self::ProbeFailed => None,
            Self::Rebuilt { build_ordinal, .. } | Self::ColdBuilt { build_ordinal, .. } => {
                Some(build_ordinal)
            }
        }
    }

    /// Runtime-cache resolution duration when this request built a Wafer.
    pub fn build_duration_ms(self) -> Option<u64> {
        match self {
            Self::Hit | Self::ProbedFresh | Self::ProbeFailed => None,
            Self::Rebuilt { duration_ms, .. } | Self::ColdBuilt { duration_ms, .. } => {
                Some(duration_ms)
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
/// folded into the `desc` text instead (`"rebuilt (build 3)"`), while the
/// separately measured elapsed milliseconds are emitted as
/// `runtime-build;dur=<ms>`.
pub fn server_timing_header(outcome: CacheOutcome) -> String {
    match (outcome.build_ordinal(), outcome.build_duration_ms()) {
        (Some(n), Some(duration_ms)) => format!(
            "cache;desc=\"{} (build {n})\", runtime-build;dur={duration_ms}",
            outcome.as_str()
        ),
        _ => format!("cache;desc=\"{}\"", outcome.as_str()),
    }
}

/// Format a structured, grep-able metric line for `console_log!`. Used for
/// signals that occur off the response path — inside `ctx.wait_until`
/// closures, after the response has already been sent — which can never
/// reach a `Server-Timing` header.
///
/// Values are logfmt-quoted when they need it (see [`quote_value`]). Several
/// of the values this is called with are not token-shaped and never were: an
/// error message is a sentence, and a cron expression is five fields separated
/// by spaces. Emitted bare, the first space ends the field and everything
/// after it parses as further keyless junk — one corrupt line, every time,
/// for exactly the signals an operator reaches for when something is wrong.
pub fn metric_line(name: &str, fields: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(8 + name.len());
    out.push_str("metric=");
    out.push_str(name);
    for (k, v) in fields {
        out.push(' ');
        out.push_str(k);
        out.push('=');
        quote_value(v, &mut out);
    }
    out
}

/// Append one logfmt value: bare when it is a bare token, double-quoted with
/// `\` and `"` escaped otherwise.
///
/// Quoting on need rather than always keeps the common case — counts, ids,
/// booleans, cache labels — byte-identical to what this emitted before, so the
/// numeric fields stay as greppable as they were.
fn quote_value(value: &str, out: &mut String) {
    let needs_quoting = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\' || c == '=');
    if !needs_quoting {
        out.push_str(value);
        return;
    }
    out.push('"');
    for c in value.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        // A literal newline would split one metric line into two, the second
        // of which has no `metric=` prefix and would be read as an unrelated
        // log line. Escape the ASCII line breaks rather than emit them.
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
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
    fn probe_failed_is_a_no_build_outcome_with_its_own_label() {
        // A failed KV version probe that served the last known runtime must be
        // distinguishable from a successful probe in Server-Timing, or "KV is
        // down and we are serving stale" is invisible from the outside.
        assert_eq!(CacheOutcome::ProbeFailed.as_str(), "probe-failed");
        assert_eq!(CacheOutcome::ProbeFailed.build_ordinal(), None);
        assert_eq!(CacheOutcome::ProbeFailed.build_duration_ms(), None);
        assert_eq!(
            server_timing_header(CacheOutcome::ProbeFailed),
            r#"cache;desc="probe-failed""#
        );
    }

    #[test]
    fn header_with_build_folds_ordinal_into_desc_not_dur() {
        // `dur` is milliseconds per the Server-Timing spec — the build
        // ordinal is a count, not a duration, so it must never land there
        // (DevTools/RUM would render e.g. "rtbuild;dur=3" as a 3ms bar).
        let with_build = server_timing_header(CacheOutcome::Rebuilt {
            build_ordinal: 3,
            duration_ms: 147,
        });
        assert_eq!(
            with_build,
            r#"cache;desc="rebuilt (build 3)", runtime-build;dur=147"#
        );

        assert_eq!(
            server_timing_header(CacheOutcome::ColdBuilt {
                build_ordinal: 1,
                duration_ms: 82,
            }),
            r#"cache;desc="cold-built (build 1)", runtime-build;dur=82"#
        );
    }

    #[test]
    fn build_ordinal_none_for_hit_and_probed_fresh() {
        assert_eq!(CacheOutcome::Hit.build_ordinal(), None);
        assert_eq!(CacheOutcome::ProbedFresh.build_ordinal(), None);
        assert_eq!(
            CacheOutcome::Rebuilt {
                build_ordinal: 7,
                duration_ms: 10,
            }
            .build_ordinal(),
            Some(7)
        );
        assert_eq!(
            CacheOutcome::ColdBuilt {
                build_ordinal: 1,
                duration_ms: 42,
            }
            .build_duration_ms(),
            Some(42)
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

    /// The two values this is most often called with — a cron expression and
    /// an error message — both contain spaces. Bare, the first space would end
    /// the field and the rest of the value would parse as keyless junk, so
    /// every line carrying one would be unparseable.
    #[test]
    fn values_with_spaces_are_quoted_rather_than_splitting_the_line() {
        assert_eq!(
            metric_line(
                "auth_maintenance_sweep",
                &[("cron", "17 3 * * *"), ("sessions_deleted", "4")]
            ),
            r#"metric=auth_maintenance_sweep cron="17 3 * * *" sessions_deleted=4"#
        );
        assert_eq!(
            metric_line(
                "auth_maintenance_sweep_failed",
                &[("error", "D1_ERROR: no such table")]
            ),
            r#"metric=auth_maintenance_sweep_failed error="D1_ERROR: no such table""#
        );
    }

    #[test]
    fn quoting_escapes_what_would_otherwise_end_the_value_or_the_line() {
        assert_eq!(
            metric_line("t", &[("v", r#"say "hi" \ bye"#)]),
            r#"metric=t v="say \"hi\" \\ bye""#
        );
        assert_eq!(
            metric_line("t", &[("v", "line one\nline two")]),
            r#"metric=t v="line one\nline two""#
        );
        // An empty value is quoted too: `errors=` reads as a truncated line,
        // `errors=""` as the successful sweep it actually is.
        assert_eq!(metric_line("t", &[("errors", "")]), r#"metric=t errors="""#);
        // `=` inside a value would look like the start of the next field.
        assert_eq!(metric_line("t", &[("v", "a=b")]), r#"metric=t v="a=b""#);
    }
}
