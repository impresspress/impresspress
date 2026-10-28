-- Initial messages schema (SQLite / D1).
--
-- Replaces the implicit `ensure_table` materialization (TEXT-only columns,
-- no indexes) for tables owned by `suppers-ai/messages`. CREATE TABLE IF
-- NOT EXISTS is a no-op against existing prod tables; the CREATE INDEX
-- statements below add the missing indexes so the hot list/filter paths
-- (context list by updated_at, A2A ListTasks by type+status, sibling
-- conversations via parent_id, entry pagination by context_id+created_at)
-- stop scanning the whole table.
--
-- Mirrored to 001_messages_schema.postgres.sql.

-- Contexts (conversations, tasks, notifications) -------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__messages__contexts (
    id            TEXT PRIMARY KEY,
    type          TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'active',
    title         TEXT NOT NULL DEFAULT '',
    sender_id     TEXT NOT NULL DEFAULT '',
    recipient_id  TEXT NOT NULL DEFAULT '',
    parent_id     TEXT,
    metadata      TEXT NOT NULL DEFAULT '{}',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
-- `list_contexts` filters by any combination of {type,status,sender_id,parent_id}
-- and always sorts by `updated_at DESC` (see service.rs::list_contexts). The
-- updated_at index backs the order-by; per-field indexes back the WHERE
-- predicates one at a time (SQLite picks the most selective).
CREATE INDEX IF NOT EXISTS idx_messages_contexts_updated_at
    ON suppers_ai__messages__contexts (updated_at);
CREATE INDEX IF NOT EXISTS idx_messages_contexts_type
    ON suppers_ai__messages__contexts (type);
CREATE INDEX IF NOT EXISTS idx_messages_contexts_status
    ON suppers_ai__messages__contexts (status);
CREATE INDEX IF NOT EXISTS idx_messages_contexts_sender_id
    ON suppers_ai__messages__contexts (sender_id);
CREATE INDEX IF NOT EXISTS idx_messages_contexts_parent_id
    ON suppers_ai__messages__contexts (parent_id);

-- Entries (messages, artifacts, notifications, status updates) ------------
CREATE TABLE IF NOT EXISTS suppers_ai__messages__entries (
    id            TEXT PRIMARY KEY,
    context_id    TEXT NOT NULL,
    kind          TEXT NOT NULL DEFAULT 'message',
    role          TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT '',
    sender_id     TEXT NOT NULL DEFAULT '',
    content       TEXT NOT NULL DEFAULT '',
    content_type  TEXT NOT NULL DEFAULT 'text/plain',
    metadata      TEXT NOT NULL DEFAULT '{}',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
-- `list_entries` always filters by `context_id` and sorts by `created_at ASC`
-- (chat replay order). The composite (context_id, created_at) index serves
-- both the WHERE and the ORDER BY in a single index seek. The single-column
-- context_id index backs `delete_by_filters(context_id=…)` in
-- `service::delete_context` (cascade-delete on context delete). The
-- (context_id, kind) composite backs filtered listing when `?kind=...` is
-- supplied (e.g. A2A artifact extraction in `build_task_response_with_history`).
CREATE INDEX IF NOT EXISTS idx_messages_entries_context_id_created_at
    ON suppers_ai__messages__entries (context_id, created_at);
CREATE INDEX IF NOT EXISTS idx_messages_entries_context_id
    ON suppers_ai__messages__entries (context_id);
CREATE INDEX IF NOT EXISTS idx_messages_entries_context_id_kind
    ON suppers_ai__messages__entries (context_id, kind);
CREATE INDEX IF NOT EXISTS idx_messages_entries_kind
    ON suppers_ai__messages__entries (kind);
