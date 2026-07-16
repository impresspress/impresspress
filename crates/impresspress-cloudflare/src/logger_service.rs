use std::{fmt::Write as _, sync::OnceLock};

use wafer_core::interfaces::logger::service::{Field, LoggerService};

/// LoggerService using CF Worker's console bindings.
///
/// Two things make this hot-path sensitive on Cloudflare: every request can
/// log several times, and `console.*` calls cross the JS/Rust boundary. This
/// implementation therefore (1) checks the configured minimum level *before*
/// touching `fields` at all, so a suppressed `debug()` call costs one enum
/// comparison and nothing else, and (2) formats surviving calls into a single
/// pre-sized `String` via `write!` instead of allocating one `String` per
/// field plus an intermediate `Vec`.
pub struct ConsoleLoggerService;

// Safety: wasm32-unknown-unknown is single-threaded.
unsafe impl Send for ConsoleLoggerService {}
unsafe impl Sync for ConsoleLoggerService {}

/// Log severity, ordered low-to-high so `level >= min_level()` is a plain
/// integer comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
enum LogLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl LogLevel {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "debug" | "trace" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Minimum level emitted when `IMPRESSPRESS_CF_LOG_LEVEL` isn't set at build
/// time. Debug builds keep `debug()` output; release (production deploy)
/// builds default to `info` so per-request debug logging doesn't pay
/// formatting cost on every warm request.
const DEFAULT_LEVEL: LogLevel = if cfg!(debug_assertions) {
    LogLevel::Debug
} else {
    LogLevel::Info
};

/// Resolve the minimum emitted level once per isolate.
///
/// `option_env!` is evaluated at compile time, so this reads whatever
/// `IMPRESSPRESS_CF_LOG_LEVEL` was set to in the environment that ran
/// `wasm-pack build`/`worker-build`, not a per-deployment Wrangler var. That
/// keeps this module self-contained (no constructor change, no `Env`
/// plumbing through `lib.rs`'s `make_*` wiring). Follow-up: thread a
/// per-deployment level through `ConsoleLoggerService::new(level)` once a
/// `make_logger_service` call site is free to take a parameter.
fn min_level() -> LogLevel {
    static LEVEL: OnceLock<LogLevel> = OnceLock::new();
    *LEVEL.get_or_init(|| {
        option_env!("IMPRESSPRESS_CF_LOG_LEVEL")
            .and_then(LogLevel::parse)
            .unwrap_or(DEFAULT_LEVEL)
    })
}

impl LoggerService for ConsoleLoggerService {
    fn debug(&self, msg: &str, fields: &[Field]) {
        if min_level() > LogLevel::Debug {
            return;
        }
        worker::console_debug!("[debug] {}{}", msg, Rendered(fields));
    }

    fn info(&self, msg: &str, fields: &[Field]) {
        if min_level() > LogLevel::Info {
            return;
        }
        worker::console_log!("[info] {}{}", msg, Rendered(fields));
    }

    fn warn(&self, msg: &str, fields: &[Field]) {
        if min_level() > LogLevel::Warn {
            return;
        }
        worker::console_warn!("[warn] {}{}", msg, Rendered(fields));
    }

    fn error(&self, msg: &str, fields: &[Field]) {
        if min_level() > LogLevel::Error {
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
