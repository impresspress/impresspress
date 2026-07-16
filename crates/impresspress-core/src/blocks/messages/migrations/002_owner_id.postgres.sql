-- Mirror of 002_owner_id.sqlite.sql for PostgreSQL.
ALTER TABLE impresspress__messages__contexts ADD COLUMN IF NOT EXISTS owner_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__messages__entries  ADD COLUMN IF NOT EXISTS owner_id TEXT NOT NULL DEFAULT '';

UPDATE impresspress__messages__contexts SET owner_id = sender_id WHERE owner_id = '';
UPDATE impresspress__messages__entries e
   SET owner_id = c.owner_id
  FROM impresspress__messages__contexts c
 WHERE c.id = e.context_id AND e.owner_id = '';

CREATE INDEX IF NOT EXISTS idx_messages_contexts_owner_id
    ON impresspress__messages__contexts (owner_id);
CREATE INDEX IF NOT EXISTS idx_messages_entries_owner_id
    ON impresspress__messages__entries (owner_id);
