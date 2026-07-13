//! Vector block migrations. Delegated to `crate::migration_helper`.
//!
//! Backend selection mirrors `files/migrations/mod.rs`: read
//! `WAFER_RUN_SHARED__DATABASE__BACKEND` from the config snapshot, fall back
//! to `sqlite` when the config block is not registered. The actual apply +
//! gating + statement splitting lives in
//! [`crate::migration_helper::apply_if_blessed`].
//!
//! Scope: only the static `impresspress__vector__registry` catalog. Per-index
//! storage tables (`{prefixed}_meta`, `{prefixed}_fts`, vec0 virtual) are
//! materialized on demand by the upstream `wafer-run/vector` runtime block
//! via `vclient::create_index` — their names are user-supplied at runtime
//! and so cannot be expressed as a static SQL migration. See the SQL file
//! header for the long-form rationale.

const SQL_001_SQLITE: &str = include_str!("001_vector_schema.sqlite.sql");
const SQL_001_POSTGRES: &str = include_str!("001_vector_schema.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[("001_vector_schema", SQL_001_SQLITE)];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`].
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[SQL_001_POSTGRES];

#[cfg(test)]
mod tests {
    use super::{SQL_001_POSTGRES, SQL_001_SQLITE};

    /// The migration_helper statement splitter splits on bare `;` outside
    /// `--` line comments. Make sure every embedded statement parses into
    /// at least the table count we expect — protects against a stray
    /// `;` inside a comment / string literal silently dropping DDL.
    fn count_create_table(sql: &str) -> usize {
        sql.match_indices("CREATE TABLE IF NOT EXISTS ").count()
    }
    fn count_create_index(sql: &str) -> usize {
        sql.match_indices("CREATE INDEX IF NOT EXISTS ").count()
    }

    #[test]
    fn sqlite_script_has_expected_tables_and_indexes() {
        // 1 table: registry
        assert_eq!(count_create_table(SQL_001_SQLITE), 1);
        // 1 index: model lookup (PK already covers prefixed_name)
        assert_eq!(count_create_index(SQL_001_SQLITE), 1);
        assert!(SQL_001_SQLITE.contains("impresspress__vector__registry"));
        assert!(SQL_001_SQLITE.contains("idx_vector_registry_model"));
    }

    #[test]
    fn postgres_script_has_expected_tables_and_indexes() {
        assert_eq!(count_create_table(SQL_001_POSTGRES), 1);
        assert_eq!(count_create_index(SQL_001_POSTGRES), 1);
        assert!(SQL_001_POSTGRES.contains("impresspress__vector__registry"));
    }
}
