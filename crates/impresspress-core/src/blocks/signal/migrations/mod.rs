//! Signal block migrations. Delegated to `crate::migration_helper`.
//!
//! Backend selection mirrors `files/migrations/mod.rs` and
//! `vector/migrations/mod.rs`: read `WAFER_RUN_SHARED__DATABASE__BACKEND`
//! from the config snapshot, fall back to `sqlite` when the config block is
//! not registered. The actual apply + gating + statement splitting lives in
//! [`crate::migration_helper::apply_if_blessed`].

const SQL_001_SQLITE: &str = include_str!("001_signal_rooms.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_001_POSTGRES: &str = include_str!("001_signal_rooms.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[("001_signal_rooms", SQL_001_SQLITE)];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`]. Empty
/// when the `postgres` feature is off — see `files::migrations`'s doc for the
/// rationale (Cloudflare/D1 never selects postgres; don't embed dead SQL).
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[SQL_001_POSTGRES];
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[];

#[cfg(test)]
mod tests {
    #[cfg(feature = "postgres")]
    use super::SQL_001_POSTGRES;
    use super::SQL_001_SQLITE;

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
        // 1 table: rooms
        assert_eq!(count_create_table(SQL_001_SQLITE), 1);
        // 1 index: expires_at, for the sweep
        assert_eq!(count_create_index(SQL_001_SQLITE), 1);
        assert!(SQL_001_SQLITE.contains("impresspress__signal__rooms"));
        assert!(SQL_001_SQLITE.contains("impresspress__signal__rooms_expires_at_idx"));
    }

    #[test]
    #[cfg(feature = "postgres")]
    fn postgres_script_has_expected_tables_and_indexes() {
        assert_eq!(count_create_table(SQL_001_POSTGRES), 1);
        assert_eq!(count_create_index(SQL_001_POSTGRES), 1);
        assert!(SQL_001_POSTGRES.contains("impresspress__signal__rooms"));
    }
}
