//! `tracing` / `tracing-subscriber` initialisation helper.
//!
//! Called once at startup to install a tracing subscriber. Supports
//! `text` and `json` formats. OpenTelemetry OTLP export is enabled by
//! the `otel` feature and auto-activates when
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is set.

#[cfg(feature = "otel")]
use anyhow::Context;
use anyhow::Result;
use tracing_subscriber::{fmt, EnvFilter};

/// Install a `tracing` subscriber for the running process.
///
/// # Errors
///
/// Returns an error if the optional OTLP exporter (enabled via the `otel`
/// feature + `OTEL_EXPORTER_OTLP_ENDPOINT`) fails to construct. Plain
/// text/JSON subscriber initialisation is infallible.
pub fn init_tracing(log_format: &str) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,wafer=debug,impresspress=debug"));

    #[cfg(feature = "otel")]
    {
        if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
            init_tracing_with_otel(log_format, filter)?;
            return Ok(());
        }
    }

    if log_format == "json" {
        fmt()
            .json()
            .with_env_filter(filter)
            .with_target(true)
            .with_thread_ids(false)
            .init();
    } else {
        fmt()
            .with_env_filter(filter)
            .with_target(true)
            .with_thread_ids(false)
            .init();
    }
    Ok(())
}

#[cfg(feature = "otel")]
fn init_tracing_with_otel(log_format: &str, filter: EnvFilter) -> Result<()> {
    use opentelemetry::trace::TracerProvider;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .context("create OTLP span exporter")?;

    let service_name = std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "impresspress".into());
    let provider = opentelemetry_sdk::trace::TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_resource(opentelemetry_sdk::Resource::new(vec![
            opentelemetry::KeyValue::new("service.name", service_name),
        ]))
        .build();

    let tracer = provider.tracer("impresspress");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    let fmt_layer: Box<dyn Layer<_> + Send + Sync> = if log_format == "json" {
        Box::new(fmt::layer().json().with_target(true).with_thread_ids(false))
    } else {
        Box::new(fmt::layer().with_target(true).with_thread_ids(false))
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    tracing::info!("OpenTelemetry tracing enabled");
    Ok(())
}
