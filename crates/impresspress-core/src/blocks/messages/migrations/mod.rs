//! Messages block migrations. The block's `lifecycle(Init)` runs these via
//! [`crate::migration_helper::lifecycle_init`], which dispatches the dialect +
//! gates the apply through [`crate::migration_helper::apply_migrations`].

const SQL_001_SQLITE: &str = include_str!("001_messages_schema.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_001_POSTGRES: &str = include_str!("001_messages_schema.postgres.sql");
const SQL_002_SQLITE: &str = include_str!("002_owner_id.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_002_POSTGRES: &str = include_str!("002_owner_id.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_messages_schema", SQL_001_SQLITE),
    ("002_owner_id", SQL_002_SQLITE),
];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`] one
/// for one. Selected at runtime by `apply_migrations` when the deployment's
/// `WAFER_RUN_SHARED__DATABASE__BACKEND` is `postgres`. Empty when the
/// `postgres` cargo feature is off — see `files::migrations`'s doc for the
/// rationale (Cloudflare/D1 never selects postgres; don't embed dead SQL).
#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[SQL_001_POSTGRES, SQL_002_POSTGRES];
#[cfg(not(feature = "postgres"))]
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[];

#[cfg(test)]
mod tests {
    #[cfg(feature = "postgres")]
    use super::{SQL_001_POSTGRES, SQL_002_POSTGRES};
    use super::{SQL_001_SQLITE, SQL_002_SQLITE};

    /// The migration_helper statement splitter splits on bare `;` outside
    /// `--` line comments. Make sure every embedded statement parses into
    /// at least the table count we expect — protects against a stray
    /// `;` inside a comment / string literal silently dropping DDL.
    // Match against the canonical DDL prefix, not bare "CREATE TABLE" — the
    // header comment in the SQL file mentions "CREATE TABLE IF NOT EXISTS"
    // descriptively, which would otherwise inflate the count.
    fn count_create_table(sql: &str) -> usize {
        sql.match_indices("CREATE TABLE IF NOT EXISTS ").count()
    }
    fn count_create_index(sql: &str) -> usize {
        sql.match_indices("CREATE INDEX IF NOT EXISTS ").count()
    }
    // Match against the "ALTER TABLE " DDL prefix with an "ADD COLUMN" body,
    // not a bare `contains("ADD COLUMN")` — the 002 header comment
    // descriptively mentions "ALTER ADD COLUMN" (no "TABLE"), so a naive
    // contains() check is trivially satisfied by prose and never actually
    // looks at the DDL. This scans each real `ALTER TABLE ...;` statement.
    fn count_alter_add_column(sql: &str) -> usize {
        sql.match_indices("ALTER TABLE ")
            .filter(|(idx, _)| {
                let stmt_end = sql[*idx..].find(';').map(|i| idx + i).unwrap_or(sql.len());
                sql[*idx..stmt_end].contains("ADD COLUMN")
            })
            .count()
    }

    #[test]
    fn sqlite_script_has_expected_tables_and_indexes() {
        // 2 tables: contexts + entries
        assert_eq!(count_create_table(SQL_001_SQLITE), 2);
        // 9 indexes: 5 on contexts (updated_at, type, status, sender_id,
        // parent_id) + 4 on entries (context_id+created_at, context_id,
        // context_id+kind, kind)
        assert_eq!(count_create_index(SQL_001_SQLITE), 9);
        // Spot-check a few key names so a rename here breaks the test.
        assert!(SQL_001_SQLITE.contains("impresspress__messages__contexts"));
        assert!(SQL_001_SQLITE.contains("impresspress__messages__entries"));
        assert!(SQL_001_SQLITE.contains("idx_messages_contexts_updated_at"));
        assert!(SQL_001_SQLITE.contains("idx_messages_entries_context_id_created_at"));
    }

    #[test]
    #[cfg(feature = "postgres")]
    fn postgres_script_has_expected_tables_and_indexes() {
        assert_eq!(count_create_table(SQL_001_POSTGRES), 2);
        assert_eq!(count_create_index(SQL_001_POSTGRES), 9);
        assert!(SQL_001_POSTGRES.contains("impresspress__messages__contexts"));
        assert!(SQL_001_POSTGRES.contains("impresspress__messages__entries"));
    }

    /// Shared assertions for the `002_owner_id` migration, run against
    /// whichever dialect's SQL the caller passes in.
    fn assert_owner_id_migration_adds_column_and_index(sql: &str) {
        // Exactly 2 ALTER TABLE ... ADD COLUMN statements: contexts +
        // entries. A dropped ALTER changes this count instead of being
        // masked by the header comment's prose.
        assert_eq!(count_alter_add_column(sql), 2);
        // Backfill #1: contexts.owner_id from historical sender_id.
        assert!(sql.contains("owner_id = sender_id"));
        // Backfill #2: entries.owner_id from the parent context's
        // owner_id, correlated on context_id. Deleting either backfill
        // statement drops one of these substrings.
        assert!(sql.contains("UPDATE impresspress__messages__entries"));
        assert!(sql.contains("c.owner_id"));
        assert!(sql.contains("context_id"));
        assert!(sql.contains("idx_messages_contexts_owner_id"));
        assert!(sql.contains("idx_messages_entries_owner_id"));
    }

    #[test]
    fn owner_id_migration_adds_column_and_index_sqlite() {
        assert_owner_id_migration_adds_column_and_index(SQL_002_SQLITE);
    }

    #[test]
    #[cfg(feature = "postgres")]
    fn owner_id_migration_adds_column_and_index_postgres() {
        assert_owner_id_migration_adds_column_and_index(SQL_002_POSTGRES);
    }
}
