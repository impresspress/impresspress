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
// 002 adds `providers.max_tokens_field`, the per-provider override for which
// field carries the output-token budget in a chat body.
//
// The spelling is otherwise a property of the declared protocol —
// `open_ai` sends `max_completion_tokens`, `open_ai_compatible` sends
// `max_tokens` — and that covers every endpoint but one. Azure OpenAI is
// configured as `open_ai_compatible`, yet its *reasoning* deployments accept
// only `max_completion_tokens`, so before this column an operator running one
// had no way to reach it and every chat turn came back 400. Nothing infers the
// value; the admin declares it (`ProviderConfig::max_tokens_field`), and
// `providers::openai::encode_chat_request_as` is the single place it beats the
// protocol's own.
//
// NULL is the ordinary state and means "follow the protocol", so existing rows
// need no backfill: `schema::row_to_config` reads a missing or empty value as
// `None`, which is what every provider configured before this migration was
// already doing.
const SQL_002_SQLITE: &str = include_str!("002_provider_max_tokens_field.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_002_POSTGRES: &str = include_str!("002_provider_max_tokens_field.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle_init` apply path.
///
/// Application is gated by the shared migration-state gate
/// ([`crate::migration_helper::apply_if_blessed`]): idempotent across cold
/// starts, and schema changes require a redeploy that re-runs migrations
/// (native: `--run-migrations`; Cloudflare: the `/_deploy/init` funnel
/// applies them on every deploy).
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_llm_schema", SQL_001_SQLITE),
    ("002_provider_max_tokens_field", SQL_002_SQLITE),
];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`]. Empty
/// when the `postgres` feature is off — see `files::migrations`'s doc for the
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

    /// 002 adds the budget-field column and nothing else, on both dialects.
    ///
    /// One statement each: the postgres file spells `IF NOT EXISTS` (which
    /// that dialect supports), SQLite relies on `apply_if_blessed`'s
    /// duplicate-column tolerance, which only covers `ALTER TABLE … ADD
    /// COLUMN`.
    #[test]
    fn migration_002_adds_only_the_max_tokens_field_column() {
        assert_eq!(split_statements(SQL_002_SQLITE), 1);
        assert!(SQL_002_SQLITE
            .contains("ALTER TABLE impresspress__llm__providers ADD COLUMN max_tokens_field TEXT"));
        #[cfg(feature = "postgres")]
        {
            assert_eq!(split_statements(SQL_002_POSTGRES), 1);
            assert!(SQL_002_POSTGRES.contains(
                "ALTER TABLE impresspress__llm__providers ADD COLUMN IF NOT EXISTS max_tokens_field TEXT"
            ));
        }
    }

    /// The column is nullable with no default: NULL is "follow the protocol",
    /// which is what every provider row created before 002 was already doing.
    /// A `NOT NULL DEFAULT` would hand each of them an override they never
    /// asked for.
    #[test]
    fn migration_002_leaves_existing_rows_without_an_override() {
        assert!(!SQL_002_SQLITE.contains("NOT NULL"));
        assert!(!SQL_002_SQLITE.contains("DEFAULT"));
        assert!(
            !SQL_002_SQLITE.to_ascii_uppercase().contains("UPDATE "),
            "no backfill: an absent value already means `None`"
        );
    }
}
