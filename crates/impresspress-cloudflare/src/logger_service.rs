use std::fmt::Write as _;

use impresspress_core::log_level::LogLevel;
use wafer_core::interfaces::logger::service::{Field, LoggerService};

/// LoggerService using CF Worker's console bindings.
///
/// Two things make this hot-path sensitive on Cloudflare: every request can
/// log several times, and `console.*` calls cross the JS/Rust boundary. This
/// implementation therefore (1) checks the configured minimum level *before*
/// touching `fields` at all, so a suppressed `debug()` call costs one field
/// read plus one enum comparison and nothing else, and (2) formats surviving
/// calls into a single pre-sized `String` via `write!` instead of allocating
/// one `String` per field plus an intermediate `Vec`.
pub struct ConsoleLoggerService {
    min_level: LogLevel,
}

// Safety: wasm32-unknown-unknown is single-threaded.
unsafe impl Send for ConsoleLoggerService {}
unsafe impl Sync for ConsoleLoggerService {}

/// Minimum level emitted when no runtime level is configured. Debug builds
/// keep `debug()` output; release (production deploy) builds default to
/// `info` so per-request debug logging doesn't pay formatting cost on every
/// warm request.
const DEFAULT_LEVEL: LogLevel = if cfg!(debug_assertions) {
    LogLevel::Debug
} else {
    LogLevel::Info
};

impl ConsoleLoggerService {
    /// Construct a logger with a runtime-configured minimum level.
    ///
    /// `level` is read at construction from the `IMPRESSPRESS_CF_LOG_LEVEL`
    /// worker var (`env.var`, set via `wrangler.toml` `[vars]` or the
    /// dashboard — see `lib.rs::make_console_logger`), so an operator can
    /// raise or lower verbosity per deployment without rebuilding. `None`
    /// (var unset) or an unparseable value falls back to [`DEFAULT_LEVEL`].
    /// Resolved once, at construction — the per-isolate runtime is built at
    /// most once per config-version change (`runtime_cache::get_or_build`),
    /// so this never runs per-request.
    pub fn new(level: Option<&str>) -> Self {
        Self {
            min_level: resolve_level(level),
        }
    }
}

/// Resolve a raw level string (the `IMPRESSPRESS_CF_LOG_LEVEL` worker var's
/// value) to a [`LogLevel`], falling back to [`DEFAULT_LEVEL`] when `raw` is
/// `None` or unparseable.
///
/// `pub(crate)` (not folded into `ConsoleLoggerService::new`) so
/// `lib.rs::run_inner` can resolve the exact same level to gate the
/// `Server-Timing` header without downcasting the type-erased
/// `Arc<dyn LoggerService>` the runtime holds.
pub(crate) fn resolve_level(raw: Option<&str>) -> LogLevel {
    raw.and_then(LogLevel::parse).unwrap_or(DEFAULT_LEVEL)
}

impl LoggerService for ConsoleLoggerService {
    fn debug(&self, msg: &str, fields: &[Field]) {
        if LogLevel::Debug.is_suppressed(self.min_level) {
            return;
        }
        worker::console_debug!("[debug] {}{}", msg, Rendered(fields));
    }

    fn info(&self, msg: &str, fields: &[Field]) {
        if LogLevel::Info.is_suppressed(self.min_level) {
            return;
        }
        worker::console_log!("[info] {}{}", msg, Rendered(fields));
    }

    fn warn(&self, msg: &str, fields: &[Field]) {
        if LogLevel::Warn.is_suppressed(self.min_level) {
            return;
        }
        worker::console_warn!("[warn] {}{}", msg, Rendered(fields));
    }

    fn error(&self, msg: &str, fields: &[Field]) {
        if LogLevel::Error.is_suppressed(self.min_level) {
            return;
        }
        worker::console_error!("[error] {}{}", msg, Rendered(fields));
    }
}

/// `Display` adapter that writes `" key=value key2=value2"` directly into the
/// formatter, avoiding the `Vec<String>` + `.join(" ")` intermediate
/// allocations of the previous implementation. Empty `fields` costs nothing
/// beyond the slice-length check.
struct Rendered<'a>(&'a [Field]);

impl std::fmt::Display for Rendered<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for field in self.0 {
            f.write_char(' ')?;
            f.write_str(&field.key)?;
            f.write_char('=')?;
            write!(f, "{}", field.value)?;
        }
        Ok(())
    }
}
