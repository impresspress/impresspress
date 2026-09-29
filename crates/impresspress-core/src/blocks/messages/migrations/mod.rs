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
    (OWNER_ID, SQL_002_SQLITE),
];

/// Basename of the `owner_id` column + backfill, named once so the migration
/// list and the test that slices it cannot drift apart.
pub(crate) const OWNER_ID: &str = "002_owner_id";

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
        // The two backfills are executed, not grepped, in
        // `owner_id_backfill_tests`.
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

#[cfg(test)]
mod owner_id_backfill_tests {
    //! What `002_owner_id` does to a deployment that already holds
    //! conversations — the only database its two backfill `UPDATE`s have
    //! anything to do on. Every other fixture applies 001 and 002 together
    //! against empty tables, where a backfill that assigned the wrong owner,
    //! or none, passes unnoticed; on a real upgrade it would lock every user
    //! out of their own history (`owner_id = ''` matches no caller) or, worse,
    //! hand an entry to someone else.
    //!
    //! The repair is driven through `apply_migrations`, the path an operator
    //! upgrading with `--run-migrations` takes, and the result is read back
    //! through the block's own routes, whose owner check is what the column
    //! exists for.

    use std::{collections::HashMap, sync::Arc};

    use serde_json::json;
    use wafer_core::clients::database as db;

    use super::{OWNER_ID, SQLITE_MIGRATIONS};
    use crate::{
        blocks::messages::{
            service::{CONTEXTS_TABLE, ENTRIES_TABLE},
            MessagesBlock,
        },
        migration_helper,
        test_support::{auth_msg, output_http_status, TestContext},
    };

    const MESSAGES: &str = "impresspress/messages";
    const AT: &str = "2026-01-01T00:00:00Z";

    /// A pre-002 row: 001's NOT NULL columns and nothing 002 adds.
    fn row(fields: serde_json::Value) -> HashMap<String, serde_json::Value> {
        let mut data = crate::util::json_map(fields);
        data.insert("created_at".to_string(), json!(AT));
        data.insert("updated_at".to_string(), json!(AT));
        data
    }

    async fn owner_of(ctx: &TestContext, table: &str, id: &str) -> String {
        db::get(ctx, table, id)
            .await
            .unwrap_or_else(|e| panic!("read {table}/{id}: {e}"))
            .data
            .get("owner_id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("{table}/{id} has no owner_id"))
            .to_string()
    }

    async fn status_as(ctx: &TestContext, path: &str, user: &str) -> u16 {
        output_http_status(
            ctx.dispatch_resolved(auth_msg("retrieve", path, user))
                .await,
        )
        .await
    }

    #[tokio::test]
    async fn migration_002_gives_existing_rows_their_owner() {
        let mut ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::messages::MessagesBlock::BLOCK_NAME);
        let before: Vec<&str> = SQLITE_MIGRATIONS[..SQLITE_MIGRATIONS
            .iter()
            .position(|(name, _)| *name == OWNER_ID)
            .expect("002 is wired into SQLITE_MIGRATIONS")]
            .iter()
            .map(|(_, sql)| *sql)
            .collect();
        migration_helper::apply_migrations(&ctx, MESSAGES, &before, &[])
            .await
            .expect("001 applies");

        for (id, sender) in [("ctx-alice", "alice"), ("ctx-bob", "bob")] {
            db::create(
                &ctx,
                CONTEXTS_TABLE,
                row(json!({ "id": id, "type": "conversation", "sender_id": sender })),
            )
            .await
            .expect("seed a pre-002 context");
        }
        // An entry's own `sender_id` is whoever spoke — here the assistant
        // in alice's thread. Its owner is the thread's, not its sender.
        for (id, context_id, sender) in [
            ("ent-alice", "ctx-alice", "assistant"),
            ("ent-bob", "ctx-bob", "bob"),
            ("ent-orphan", "ctx-gone", "carol"),
        ] {
            db::create(
                &ctx,
                ENTRIES_TABLE,
                row(json!({ "id": id, "context_id": context_id, "sender_id": sender })),
            )
            .await
            .expect("seed a pre-002 entry");
        }

        ctx.set_config(migration_helper::RUN_MIGRATIONS_KEY, "1");
        let all: Vec<&str> = SQLITE_MIGRATIONS.iter().map(|(_, sql)| *sql).collect();
        migration_helper::apply_migrations(&ctx, MESSAGES, &all, &[])
            .await
            .expect("002 applies to a database holding conversations");

        assert_eq!(owner_of(&ctx, CONTEXTS_TABLE, "ctx-alice").await, "alice");
        assert_eq!(owner_of(&ctx, CONTEXTS_TABLE, "ctx-bob").await, "bob");
        assert_eq!(
            owner_of(&ctx, ENTRIES_TABLE, "ent-alice").await,
            "alice",
            "an entry inherits its thread's owner, not its own sender"
        );
        assert_eq!(owner_of(&ctx, ENTRIES_TABLE, "ent-bob").await, "bob");
        assert_eq!(
            owner_of(&ctx, ENTRIES_TABLE, "ent-orphan").await,
            "",
            "an entry with no thread has no owner to inherit and stays unowned"
        );

        ctx.register_block(MESSAGES, Arc::new(MessagesBlock::new()));
        assert_eq!(
            status_as(&ctx, "/b/messages/api/contexts/ctx-alice", "alice").await,
            200,
            "the upgraded thread is still its author's"
        );
        assert_eq!(
            status_as(&ctx, "/b/messages/api/entries/ent-alice", "alice").await,
            200
        );
        assert_eq!(
            status_as(&ctx, "/b/messages/api/entries/ent-alice", "bob").await,
            404,
            "and nobody else's"
        );
    }
}
