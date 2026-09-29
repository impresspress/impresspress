use impresspress_core::{log_level::LogLevel, log_line::LogLine};
use wafer_core::interfaces::logger::service::{Field, LoggerService};

/// LoggerService using CF Worker's console bindings.
///
/// Two things make this hot-path sensitive on Cloudflare: every request can
/// log several times, and `console.*` calls cross the JS/Rust boundary. This
/// implementation therefore (1) checks the configured minimum level *before*
/// touching `fields` at all, so a suppressed `debug()` call costs one field
/// read plus one enum comparison and nothing else, and (2) formats surviving
/// calls straight into the console macro's formatter.
pub struct ConsoleLoggerService {
    min_level: LogLevel,
}

// No `unsafe impl Send/Sync` here: `LogLevel` is a plain `Copy` enum, so the
// compiler derives both. The pair that used to sit here claimed
// "wasm32-unknown-unknown is single-threaded" — true, but irrelevant to a type
// that is already `Send + Sync`, and an unnecessary `unsafe impl` teaches the
// next reader that the crate hands them out by habit. The three that remain
// (`database`, `network_service`, `storage`) wrap real JS handles and keep
// their SAFETY comments; new code uses `MaybeSend` and the lint allow instead.

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
    /// dashboard — see `services.rs::make_console_logger`), so an operator can
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
    fn debug(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        if LogLevel::Debug.is_suppressed(self.min_level) {
            return;
        }
        worker::console_debug!("[debug] {}", line(caller, msg, fields));
    }

    fn info(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        if LogLevel::Info.is_suppressed(self.min_level) {
            return;
        }
        worker::console_log!("[info] {}", line(caller, msg, fields));
    }

    fn warn(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        if LogLevel::Warn.is_suppressed(self.min_level) {
            return;
        }
        worker::console_warn!("[warn] {}", line(caller, msg, fields));
    }

    fn error(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        if LogLevel::Error.is_suppressed(self.min_level) {
            return;
        }
        worker::console_error!("[error] {}", line(caller, msg, fields));
    }
}

/// One record as `caller=… msg=… key=value…` (see
/// `impresspress_core::log_line`), written straight into the console
/// macro's formatter.
fn line<'a>(caller: Option<&'a str>, msg: &'a str, fields: &'a [Field]) -> LogLine<'a> {
    LogLine {
        caller,
        msg,
        fields,
    }
}
