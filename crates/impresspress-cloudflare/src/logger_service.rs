use wafer_core::interfaces::logger::service::{Field, LoggerService};

/// LoggerService using CF Worker's console_log.
pub struct ConsoleLoggerService;

// Safety: wasm32-unknown-unknown is single-threaded.
unsafe impl Send for ConsoleLoggerService {}
unsafe impl Sync for ConsoleLoggerService {}

impl LoggerService for ConsoleLoggerService {
    fn debug(&self, msg: &str, fields: &[Field]) {
        worker::console_log!("[debug] {} {}", msg, format_fields(fields));
    }

    fn info(&self, msg: &str, fields: &[Field]) {
        worker::console_log!("[info] {} {}", msg, format_fields(fields));
    }

    fn warn(&self, msg: &str, fields: &[Field]) {
        worker::console_log!("[warn] {} {}", msg, format_fields(fields));
    }

    fn error(&self, msg: &str, fields: &[Field]) {
        worker::console_log!("[error] {} {}", msg, format_fields(fields));
    }
}

fn format_fields(fields: &[Field]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    fields
        .iter()
        .map(|f| format!("{}={}", f.key, f.value))
        .collect::<Vec<_>>()
        .join(" ")
}
