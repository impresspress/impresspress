//! LLM block migrations. Applied from the block's `Init` lifecycle.
//!
//! SQL files are embedded with `include_str!`. Backend dispatch + the
//! `current_hash` / `blessed_hash` / `IMPRESSPRESS_RUN_MIGRATIONS` gate live
//! in [`crate::migration_helper::apply_migrations`]. Replaces the implicit
//! `ensure_table` materialisation that previously created these tables on
//! first insert (TEXT-only columns, no indexes — see impresspress
//! `ensure-table-removal-in-progress`).

const SQL_001_SQLITE: &str = include_str!("001_llm_schema.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_001_POSTGRES: &str = include_str!("001_llm_schema.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
///
/// Application is gated by the shared migration-state gate
/// ([`crate::migration_helper::apply_if_blessed`]): idempotent across cold
/// starts, and schema changes require a redeploy that re-runs migrations
/// (native: `--run-migrations`; Cloudflare: the `/_deploy/init` funnel
/// applies them on every deploy).
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[("001_llm_schema", SQL_001_SQLITE)];

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

    fn split_statements(sql: &str) -> usize {
        // Inline mirror of `migration_helper::split_statements`'s
        // semicolon-on-newline split, filtering empty/comment-only chunks.
        // Kept here so the parser test guards regressions against the
        // committed SQL without depending on the helper's pub surface.
        sql.split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter(|s| {
                s.lines().any(|line| {
                    let l = line.trim();
                    !l.is_empty() && !l.starts_with("--")
                })
            })
            .count()
    }

    #[test]
    fn sqlite_sql_splits_into_expected_chunks() {
        // 2 tables + 3 indexes = 5 executable statements.
        assert_eq!(
            split_statements(SQL_001_SQLITE),
            5,
            "sqlite llm migration: expected 5 statements"
        );
    }

    #[test]
    #[cfg(feature = "postgres")]
    fn postgres_sql_splits_into_expected_chunks() {
        assert_eq!(
            split_statements(SQL_001_POSTGRES),
            5,
            "postgres llm migration: expected 5 statements"
        );
    }

    #[test]
    fn sqlite_creates_both_tables() {
        assert!(SQL_001_SQLITE.contains("impresspress__llm__settings"));
        assert!(SQL_001_SQLITE.contains("impresspress__llm__providers"));
    }

    #[test]
    fn sqlite_declares_required_indexes() {
        assert!(SQL_001_SQLITE.contains("impresspress__llm__settings_thread_id_idx"));
        assert!(SQL_001_SQLITE.contains("impresspress__llm__providers_name_uniq"));
        assert!(SQL_001_SQLITE.contains("impresspress__llm__providers_enabled_idx"));
    }

    #[test]
    #[cfg(feature = "postgres")]
    fn postgres_creates_both_tables() {
        assert!(SQL_001_POSTGRES.contains("impresspress__llm__settings"));
        assert!(SQL_001_POSTGRES.contains("impresspress__llm__providers"));
    }
}
