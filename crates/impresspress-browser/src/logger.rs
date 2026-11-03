//! Console logging for the browser host: the block-facing
//! [`LoggerService`] and the framework-facing `tracing` bridge.
//!
//! Two audiences, one destination. A block calls `LoggerService` through its
//! context; every crate *underneath* the block boundary — `impresspress-core`,
//! `wafer-run`, this adapter — logs with `tracing` macros. On native those go
//! to the subscriber `impresspress-native::log_init` installs; in the browser
//! there was no subscriber at all, so every `tracing::warn!` in the framework
//! was discarded before it reached anything. [`init_console_tracing`] is that
//! missing half.

use impresspress_core::{console_tracing::ConsoleTracing, log_line::LogLine};
use tracing::Level;
use wafer_core::interfaces::logger::service::{Field, LoggerService};
use web_sys::console;

pub struct ConsoleLogger;

// SAFETY: `ConsoleLogger` is a unit struct with no shared state.
// wasm32-unknown-unknown has no threads, so the `Send`/`Sync` bounds
// required by `Arc<dyn LoggerService>` are satisfied trivially — no
// cross-thread aliasing or data races are possible.
unsafe impl Send for ConsoleLogger {}
unsafe impl Sync for ConsoleLogger {}

/// A block's record as one console line — see `impresspress_core::log_line`.
fn format_message(caller: Option<&str>, msg: &str, fields: &[Field]) -> String {
    LogLine {
        caller,
        msg,
        fields,
    }
    .to_string()
}

impl LoggerService for ConsoleLogger {
    fn debug(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        console::log_1(&format_message(caller, msg, fields).into());
    }

    fn info(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        console::log_1(&format_message(caller, msg, fields).into());
    }

    fn warn(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        console::warn_1(&format_message(caller, msg, fields).into());
    }

    fn error(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        console::error_1(&format_message(caller, msg, fields).into());
    }
}

pub fn make_console_logger(
) -> std::sync::Arc<dyn wafer_core::interfaces::logger::service::LoggerService> {
    std::sync::Arc::new(ConsoleLogger)
}

// ─── `tracing` → console bridge ──────────────────────────────────────────────
//
// The subscriber is `impresspress_core::console_tracing::ConsoleTracing`,
// shared with the Cloudflare Worker; this host supplies the sink.

/// Route one rendered event to the console method that matches its level, so
/// the browser's own level filter and the "Errors" tab work on framework logs.
fn console_emit(level: Level, line: String) {
    let value = line.into();
    match level {
        Level::ERROR => console::error_1(&value),
        Level::WARN => console::warn_1(&value),
        Level::INFO => console::info_1(&value),
        Level::DEBUG | Level::TRACE => console::debug_1(&value),
    }
}

/// Install the console subscriber as the process-wide `tracing` default.
///
/// Call once, as early as possible in the wasm entry point —
/// `boot::prepare_worker` does — and before any code that logs.
/// Without it every `tracing` event from `impresspress-core`, `wafer-run` and
/// this crate is dropped by `tracing`'s no-op default dispatcher, which is
/// what made "the malformed chunk is logged and skipped" mean "silently
/// skipped" in the browser: a short assistant message that looked complete,
/// with nothing in the console and no error to the caller.
///
/// Idempotent and never fatal: a second call (or a host that installed its own
/// subscriber first) leaves the existing one in place and returns `false`
/// rather than panicking, because failing to install a logger must not fail a
/// boot.
///
/// Every level is kept (`Level::TRACE`): filtering is the macros' compile-time
/// `max_level_*` features and the call sites' own levels, and the browser
/// console's own level filter hides what a reader does not want. A browser
/// build has no `RUST_LOG` to read.
pub fn init_console_tracing() -> bool {
    tracing::subscriber::set_global_default(ConsoleTracing::new(Level::TRACE, console_emit)).is_ok()
}
