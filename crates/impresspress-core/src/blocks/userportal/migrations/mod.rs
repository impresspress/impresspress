//! User portal block migrations. Applied from the block's `Init` lifecycle
//! via [`crate::migration_helper::lifecycle_init`].
//!
//! SQL files are embedded with `include_str!`. Backend dispatch + the
//! `current_hash` / `blessed_hash` / `IMPRESSPRESS_RUN_MIGRATIONS` gate live
//! in [`crate::migration_helper::apply_migrations`].

const SQL_001_SQLITE: &str = include_str!("001_userportal_schema.sqlite.sql");
const SQL_001_POSTGRES: &str = include_str!("001_userportal_schema.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[("001_userportal_schema", SQL_001_SQLITE)];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`].
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[SQL_001_POSTGRES];

#[cfg(test)]
mod tests {
    use super::{SQL_001_POSTGRES, SQL_001_SQLITE};

    #[test]
    fn sqlite_script_creates_buttons_table_and_sort_order_index() {
        assert!(
            SQL_001_SQLITE.contains("CREATE TABLE IF NOT EXISTS impresspress__userportal__buttons")
        );
        assert!(SQL_001_SQLITE.contains("impresspress__userportal__buttons_sort_order_idx"));
    }

    #[test]
    fn postgres_script_creates_buttons_table_and_sort_order_index() {
        assert!(SQL_001_POSTGRES
            .contains("CREATE TABLE IF NOT EXISTS impresspress__userportal__buttons"));
        assert!(SQL_001_POSTGRES.contains("impresspress__userportal__buttons_sort_order_idx"));
    }
}
