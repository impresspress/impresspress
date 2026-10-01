//! Dev sandbox block migrations, applied through the standard migration gate.

const SQL_001_SQLITE: &str = include_str!("001_dev_schema.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_001_POSTGRES: &str = include_str!("001_dev_schema.postgres.sql");
const SQL_002_SQLITE: &str = include_str!("002_build_artifact_bytes.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_002_POSTGRES: &str = include_str!("002_build_artifact_bytes.postgres.sql");
const SQL_003_SQLITE: &str = include_str!("003_seed_info.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_003_POSTGRES: &str = include_str!("003_seed_info.postgres.sql");

pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_dev_schema", SQL_001_SQLITE),
    ("002_build_artifact_bytes", SQL_002_SQLITE),
    ("003_seed_info", SQL_003_SQLITE),
];

#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] =
    &[SQL_001_POSTGRES, SQL_002_POSTGRES, SQL_003_POSTGRES];
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[];
