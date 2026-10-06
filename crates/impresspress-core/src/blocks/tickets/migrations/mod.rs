//! Tickets block migrations, applied through the standard migration gate.

const SQL_001_SQLITE: &str = include_str!("001_tickets_schema.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_001_POSTGRES: &str = include_str!("001_tickets_schema.postgres.sql");

pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[("001_tickets_schema", SQL_001_SQLITE)];

#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[(&str, &str)] = &[("001_tickets_schema", SQL_001_POSTGRES)];
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[(&str, &str)] = &[];
