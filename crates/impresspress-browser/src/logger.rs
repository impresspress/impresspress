use wafer_core::interfaces::logger::service::{Field, LoggerService};
use web_sys::console;

pub struct ConsoleLogger;

// SAFETY: `ConsoleLogger` is a unit struct with no shared state.
// wasm32-unknown-unknown has no threads, so the `Send`/`Sync` bounds
// required by `Arc<dyn LoggerService>` are satisfied trivially — no
// cross-thread aliasing or data races are possible.
unsafe impl Send for ConsoleLogger {}
unsafe impl Sync for ConsoleLogger {}

fn format_message(msg: &str, fields: &[Field]) -> String {
    if fields.is_empty() {
        return msg.to_string();
    }
    let field_str: Vec<String> = fields
        .iter()
        .map(|f| format!("{}={}", f.key, f.value))
        .collect();
    format!("{} {}", msg, field_str.join(" "))
}

impl LoggerService for ConsoleLogger {
    fn debug(&self, msg: &str, fields: &[Field]) {
        console::log_1(&format_message(msg, fields).into());
    }

    fn info(&self, msg: &str, fields: &[Field]) {
        console::log_1(&format_message(msg, fields).into());
    }

    fn warn(&self, msg: &str, fields: &[Field]) {
        console::warn_1(&format_message(msg, fields).into());
    }

    fn error(&self, msg: &str, fields: &[Field]) {
        console::error_1(&format_message(msg, fields).into());
    }
}

pub fn make_console_logger(
) -> std::sync::Arc<dyn wafer_core::interfaces::logger::service::LoggerService> {
    std::sync::Arc::new(ConsoleLogger)
}
