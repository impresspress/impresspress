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

use std::fmt;

use impresspress_core::log_line::LogLine;
use tracing::{
    field::{Field as TracingField, Visit},
    span, Event, Level, Metadata, Subscriber,
};
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

/// Collects an event's `message` and its remaining fields into one line:
/// `target message key=value key=value`.
#[derive(Default)]
struct LineVisitor {
    message: String,
    fields: Vec<String>,
}

impl LineVisitor {
    fn push(&mut self, name: &str, value: String) {
        // `tracing` names the macro's format-string argument `message`.
        if name == "message" {
            self.message = value;
        } else {
            self.fields.push(format!("{name}={value}"));
        }
    }

    fn finish(self, meta: &Metadata<'_>) -> String {
        let mut line = format!("{} {}", meta.target(), self.message);
        if !self.fields.is_empty() {
            line.push(' ');
            line.push_str(&self.fields.join(" "));
        }
        line
    }
}

impl Visit for LineVisitor {
    fn record_debug(&mut self, field: &TracingField, value: &dyn fmt::Debug) {
        self.push(field.name(), format!("{value:?}"));
    }

    /// Strings are recorded unquoted — `error = connection refused`, not
    /// `error = "connection refused"` — which is what the `%e` / `%err` sigil
    /// at nearly every call site in this workspace means.
    fn record_str(&mut self, field: &TracingField, value: &str) {
        self.push(field.name(), value.to_string());
    }
}

/// A `tracing` subscriber that writes each event to the browser console and
/// does nothing else.
///
/// Deliberately span-less: `new_span` hands back a constant id and
/// `enter`/`exit` are no-ops, because nothing in the browser build reads span
/// context and a real span registry would cost bundle size and a slab
/// allocation per span for output nobody consumes. Events carry their own
/// fields, which is what the workspace's `warn!(error = %e, …)` style puts the
/// information in.
///
/// The sink is a generic closure rather than a hardcoded `console::*` call so
/// the level routing and the line format are testable without a browser
/// console to read.
struct ConsoleTracing<F> {
    emit: F,
}

impl<F> Subscriber for ConsoleTracing<F>
where
    F: Fn(Level, String) + Send + Sync + 'static,
{
    /// Everything is enabled; filtering is the macros' compile-time
    /// `max_level_*` features and the call sites' own levels. A browser build
    /// has no `RUST_LOG` to read.
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        // Ids must be non-zero; one constant id is enough for a subscriber that
        // never distinguishes spans.
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = LineVisitor::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        (self.emit)(*meta.level(), visitor.finish(meta));
    }

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}
}

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
/// `impresspress-web`'s `initialize` does — and before any code that logs.
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
pub fn init_console_tracing() -> bool {
    tracing::subscriber::set_global_default(ConsoleTracing { emit: console_emit }).is_ok()
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::Level;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::ConsoleTracing;

    fn capture(f: impl FnOnce()) -> Vec<(Level, String)> {
        let recorded: Arc<Mutex<Vec<(Level, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&recorded);
        let subscriber = ConsoleTracing {
            emit: move |level, line| sink.lock().unwrap().push((level, line)),
        };
        tracing::subscriber::with_default(subscriber, f);
        let out = recorded.lock().unwrap().clone();
        out
    }

    /// **Fails on the pre-fix tree**, where the browser installed no subscriber
    /// at all: `tracing`'s default dispatcher drops every event, so a framework
    /// `warn!` — including the "malformed chunk, skipped" one on the LLM
    /// streaming path — reached nothing at all. The console is the browser's
    /// only log sink, so an event that does not reach it does not exist.
    #[wasm_bindgen_test]
    fn a_framework_warning_reaches_the_sink_with_its_fields() {
        let events = capture(|| {
            tracing::warn!(
                error = "bad json",
                payload = "{oops",
                "openai sse: decode failed"
            );
        });

        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, Level::WARN);
        assert!(
            events[0].1.contains("openai sse: decode failed"),
            "{events:?}"
        );
        assert!(events[0].1.contains("error=bad json"), "{events:?}");
        assert!(events[0].1.contains("payload={oops"), "{events:?}");
    }

    /// The level reaches the sink unchanged, so `console_emit` can route an
    /// error to `console.error` and a debug line to `console.debug` — without
    /// that, the browser's own level filter cannot tell framework noise from a
    /// framework failure.
    #[wasm_bindgen_test]
    fn every_level_is_carried_through_rather_than_flattened() {
        let events = capture(|| {
            tracing::error!("e");
            tracing::warn!("w");
            tracing::info!("i");
            tracing::debug!("d");
        });

        assert_eq!(
            events.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
            vec![Level::ERROR, Level::WARN, Level::INFO, Level::DEBUG],
            "{events:?}"
        );
    }

    /// A message with no fields is not padded with a trailing separator, and
    /// the target is present so a console line says which module produced it.
    #[wasm_bindgen_test]
    fn a_bare_message_carries_its_target_and_no_trailing_padding() {
        let events = capture(|| tracing::info!("plain"));

        assert_eq!(
            events[0].1,
            format!("{} plain", module_path!()),
            "{events:?}"
        );
    }
}
