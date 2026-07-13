//! Native platform services and bootstrap helpers for WAFER-based server
//! apps. Sibling to `impresspress-browser`; provides the same shape (factory
//! functions per service, lightweight bootstrap helpers, a runtime /
//! serve layer) so a consumer's entrypoint looks structurally identical
//! across both targets.
//!
//! The library contains zero impresspress-app-specific knowledge — app-level
//! schema work (reading/seeding `variables` tables, per-block config
//! JSON, ImpresspressBuilder composition) lives in the consumer's binary.

pub mod crypto;
pub mod database;
pub mod env;
pub mod hooks;
pub mod log_init;
pub mod logger;
pub mod network;
pub mod serve;
pub mod storage;

pub use crypto::make_jwt_crypto_service;
#[cfg(feature = "postgres")]
pub use database::make_postgres_database_service;
pub use database::{make_database_service, make_sqlite_database_service};
pub use env::{collect_app_env_vars, load_dotenv, InfraConfig};
pub use hooks::register_observability_hooks;
pub use log_init::init_tracing;
pub use logger::make_tracing_logger;
pub use network::make_fetch_network_service;
pub use serve::{register_http_listener, serve_until_shutdown};
pub use storage::{make_local_storage_service, make_storage_service};
#[cfg(feature = "s3")]
pub use storage::{make_s3_storage_service, S3Config};
