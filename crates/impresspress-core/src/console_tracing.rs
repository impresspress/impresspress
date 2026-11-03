//! A `tracing` subscriber that hands each event to a console, as one line.
//!
//! Every crate under the block boundary — `impresspress-core`, `wafer-run`,
//! the platform adapters — logs with `tracing` macros. Native installs a
//! `tracing-subscriber` stack (`impresspress-native::log_init`); the two wasm
//! hosts have only a JavaScript console, and without a subscriber `tracing`'s
//! no-op default dispatcher drops every event before it reaches anything.
//! [`ConsoleTracing`] is that subscriber for both: the browser
//! (`impresspress-browser::logger::init_console_tracing`) and the Cloudflare
//! Worker (`impresspress-cloudflare::init_isolate`). Each host supplies the
//! sink — which `console` method a level goes to — and the most verbose
//! level it keeps.

use std::fmt;

use tracing::{
    field::{Field, Visit},
    level_filters::LevelFilter,
    span, Event, Level, Metadata, Subscriber,
};

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
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field.name(), format!("{value:?}"));
    }

    /// Strings are recorded unquoted — `error = connection refused`, not
    /// `error = "connection refused"` — which is what the `%e` / `%err` sigil
    /// at nearly every call site in this workspace means.
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field.name(), value.to_string());
    }
}

/// A `tracing` subscriber that writes each event at or above `max` to
/// `emit`, as `(level, line)`, and does nothing else.
///
/// Deliberately span-less: `new_span` hands back a constant id and
/// `enter`/`exit` are no-ops, because nothing on a wasm host reads span
/// context and a real span registry would cost bundle size and a slab
/// allocation per span for output nobody consumes. Events carry their own
/// fields, which is what the workspace's `warn!(error = %e, …)` style puts the
/// information in.
///
/// `max` is reported as the subscriber's [`max_level_hint`], so a call site
/// more verbose than it is disabled once, when `tracing` first registers it,
/// and costs a cached check afterwards — not a formatted line per call.
///
/// The sink is a closure rather than a hardcoded `console::*` call so the
/// level routing and the line format are testable without a console to read.
///
/// [`max_level_hint`]: Subscriber::max_level_hint
pub struct ConsoleTracing<F> {
    max: Level,
    emit: F,
}

impl<F> ConsoleTracing<F>
where
    F: Fn(Level, String) + Send + Sync + 'static,
{
    /// Write every event at `max` or above (`Level::WARN` keeps warnings and
    /// errors; `Level::TRACE` keeps everything) to `emit`.
    pub fn new(max: Level, emit: F) -> Self {
        Self { max, emit }
    }
}

impl<F> Subscriber for ConsoleTracing<F>
where
    F: Fn(Level, String) + Send + Sync + 'static,
{
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= self.max
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::from_level(self.max))
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::Level;

    use super::ConsoleTracing;

    fn capture(max: Level, f: impl FnOnce()) -> Vec<(Level, String)> {
        let recorded: Arc<Mutex<Vec<(Level, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&recorded);
        let subscriber = ConsoleTracing::new(max, move |level, line| {
            sink.lock().unwrap().push((level, line))
        });
        tracing::subscriber::with_default(subscriber, f);
        let out = recorded.lock().unwrap().clone();
        out
    }

    /// A framework warning reaches the sink with its message and fields: the
    /// console is a wasm host's only log sink, so an event that does not
    /// reach it does not exist.
    #[test]
    fn a_framework_warning_reaches_the_sink_with_its_fields() {
        let events = capture(Level::TRACE, || {
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

    /// The level reaches the sink unchanged, so a host can route an error to
    /// `console.error` and a debug line to `console.debug`.
    #[test]
    fn every_level_is_carried_through_rather_than_flattened() {
        let events = capture(Level::TRACE, || {
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

    /// Events more verbose than `max` never reach the sink.
    #[test]
    fn events_below_the_maximum_are_dropped() {
        let events = capture(Level::WARN, || {
            tracing::error!("e");
            tracing::warn!("w");
            tracing::info!("i");
            tracing::debug!("d");
        });

        assert_eq!(
            events.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
            vec![Level::ERROR, Level::WARN],
            "{events:?}"
        );
    }

    /// A message with no fields is not padded with a trailing separator, and
    /// the target is present so a console line says which module produced it.
    #[test]
    fn a_bare_message_carries_its_target_and_no_trailing_padding() {
        let events = capture(Level::TRACE, || tracing::info!("plain"));

        assert_eq!(
            events[0].1,
            format!("{} plain", module_path!()),
            "{events:?}"
        );
    }
}
