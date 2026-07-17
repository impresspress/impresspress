//! Generic log-severity level + threshold ordering.
//!
//! Lives in `impresspress-core` (not `impresspress-cloudflare`) so the
//! parsing/threshold logic is host-testable: `impresspress-cloudflare` is
//! wasm32-only and excluded from `cargo test --workspace` — its R2/D1
//! service impls require `!Send` futures that don't compile on a native
//! target, so *nothing* in that crate (including plain unit tests with no
//! wasm dependency) can run via `cargo test -p impresspress-cloudflare`.
//! Follows the `cache_key`/`kv` extraction precedent: pure logic that a
//! wasm-only adapter would otherwise leave untested moves here.
//!
//! Consumed by `impresspress-cloudflare::logger_service::ConsoleLoggerService`,
//! which resolves the minimum emitted level once per logger construction
//! from the `IMPRESSPRESS_CF_LOG_LEVEL` worker var (read in
//! `impresspress-cloudflare/src/lib.rs::make_console_logger`), falling back
//! to a compile-time default when the var is unset or unparseable.

/// Log severity, ordered low-to-high so `level >= min_level` is a plain
/// integer comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl LogLevel {
    /// Parse a level name, case/whitespace-insensitive. `"trace"` maps to
    /// `Debug` (no separate trace tier). Returns `None` for anything else so
    /// callers can fall back to a default rather than silently picking one.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "debug" | "trace" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_levels_case_and_whitespace_insensitively() {
        assert_eq!(LogLevel::parse("Debug"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::parse("TRACE"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::parse(" info "), Some(LogLevel::Info));
        assert_eq!(LogLevel::parse("WARNING"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse("warn"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse("error"), Some(LogLevel::Error));
    }

    #[test]
    fn rejects_unknown_or_empty_level() {
        assert_eq!(LogLevel::parse("verbose"), None);
        assert_eq!(LogLevel::parse(""), None);
        assert_eq!(LogLevel::parse("   "), None);
    }

    #[test]
    fn orders_low_to_high_for_threshold_comparison() {
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }

    #[test]
    fn threshold_semantics_match_console_logger_service() {
        // ConsoleLoggerService suppresses a call at `level` when
        // `level < min_level` (see logger_service.rs's per-method guards).
        let min_level = LogLevel::Info;
        assert!(LogLevel::Debug < min_level, "debug suppressed at min=info");
        assert!(!(LogLevel::Info < min_level), "info emitted at min=info");
        assert!(!(LogLevel::Warn < min_level), "warn emitted at min=info");
        assert!(!(LogLevel::Error < min_level), "error emitted at min=info");
    }
}
