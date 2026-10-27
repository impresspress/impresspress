-- Initial messages schema (Postgres parity — untested).
-- Solobase deploys SQLite/D1 today; this file is included for parity with
-- the auth/files-migrations pattern. Validate before enabling Postgres for
-- the messages block.

-- Contexts ---------------------------------------------------------------
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

-- Entries ---------------------------------------------------------------
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
CREATE INDEX IF NOT EXISTS idx_messages_entries_context_id_created_at
    ON suppers_ai__messages__entries (context_id, created_at);
CREATE INDEX IF NOT EXISTS idx_messages_entries_context_id
    ON suppers_ai__messages__entries (context_id);
CREATE INDEX IF NOT EXISTS idx_messages_entries_context_id_kind
    ON suppers_ai__messages__entries (context_id, kind);
CREATE INDEX IF NOT EXISTS idx_messages_entries_kind
    ON suppers_ai__messages__entries (kind);
