//! Products block migrations. Applied from the block's `Init` lifecycle via
//! [`crate::migration_helper::lifecycle_init`].
//!
//! Mirrors the auth/files migration pattern. SQL is embedded via
//! `include_str!`. Backend selection reads the
//! `WAFER_RUN_SHARED__DATABASE__BACKEND` config key
//! (`sqlite` | `postgres`). Falls back to `sqlite` when the config block
//! is not registered.
//!
//! Application is gated by [`crate::migration_helper::apply_if_blessed`]:
//! the helper handles statement splitting + the `current_hash` /
//! `blessed_hash` / `IMPRESSPRESS_RUN_MIGRATIONS` gate, and stamps a row in
//! `impresspress__admin__block_settings` once applied.

const SQL_001_SQLITE: &str = include_str!("001_products_schema.sqlite.sql");
const SQL_001_POSTGRES: &str = include_str!("001_products_schema.postgres.sql");
const SQL_002_SQLITE: &str = include_str!("002_default_templates.sqlite.sql");
const SQL_002_POSTGRES: &str = include_str!("002_default_templates.postgres.sql");
const SQL_003_SQLITE: &str = include_str!("003_stripe_events.sqlite.sql");
const SQL_003_POSTGRES: &str = include_str!("003_stripe_events.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
/// Order here is the apply order.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_products_schema", SQL_001_SQLITE),
    ("002_default_templates", SQL_002_SQLITE),
    ("003_stripe_events", SQL_003_SQLITE),
];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`].
pub(crate) const POSTGRES_MIGRATIONS: &[&str] =
    &[SQL_001_POSTGRES, SQL_002_POSTGRES, SQL_003_POSTGRES];
