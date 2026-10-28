//! Tickets block migrations, applied through the standard migration gate.

const SQL_001_SQLITE: &str = include_str!("001_tickets_schema.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_001_POSTGRES: &str = include_str!("001_tickets_schema.postgres.sql");
const SQL_002_SQLITE: &str = include_str!("002_stamped_columns.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_002_POSTGRES: &str = include_str!("002_stamped_columns.postgres.sql");

pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_tickets_schema", SQL_001_SQLITE),
    ("002_stamped_columns", SQL_002_SQLITE),
];

#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[(&str, &str)] = &[
    ("001_tickets_schema", SQL_001_POSTGRES),
    ("002_stamped_columns", SQL_002_POSTGRES),
];
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[(&str, &str)] = &[];

/// This block's migrations as its `lifecycle(Init)` applies them, and as
/// [`crate::blocks::all_block_migrations`] lists them: both dialects' files
/// and both dialects' pre-ledger history (`crate::migration_history`).
pub(crate) const MIGRATIONS: crate::migration_helper::BlockMigrations =
    crate::migration_helper::BlockMigrations {
        block: "impresspress/tickets",
        sqlite: SQLITE_MIGRATIONS,
        postgres: POSTGRES_MIGRATIONS,
        sqlite_history: crate::migration_history::tickets::SQLITE,
        postgres_history: crate::migration_history::tickets::POSTGRES,
    };
