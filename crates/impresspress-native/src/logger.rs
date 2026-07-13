//! Logger platform-service factory for native targets.

use std::sync::Arc;

use wafer_core::interfaces::logger::service::LoggerService;

/// Construct a LoggerService that emits via the `tracing` crate. Consumers
/// should call `init_tracing(format)` once at startup to install a
/// tracing subscriber (the logger alone does not install one).
pub fn make_tracing_logger() -> Arc<dyn LoggerService> {
    Arc::new(wafer_core::service_blocks::logger::TracingLogger)
}
