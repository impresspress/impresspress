-- Adds owner_id to messages contexts + entries (P0 #1 IDOR fix).
-- ALTER ADD COLUMN is idempotent under apply_if_blessed (tolerates the
-- duplicate-column error on re-run). Backfill existing rows: contexts from
-- their historical sender_id; entries from their parent context's owner.
ALTER TABLE impresspress__messages__contexts ADD COLUMN owner_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__messages__entries  ADD COLUMN owner_id TEXT NOT NULL DEFAULT '';

UPDATE impresspress__messages__contexts SET owner_id = sender_id WHERE owner_id = '';
UPDATE impresspress__messages__entries
   SET owner_id = (
       SELECT c.owner_id FROM impresspress__messages__contexts c
       WHERE c.id = impresspress__messages__entries.context_id
   )
 WHERE owner_id = ''
   AND EXISTS (
       SELECT 1 FROM impresspress__messages__contexts c
       WHERE c.id = impresspress__messages__entries.context_id
   );

CREATE INDEX IF NOT EXISTS idx_messages_contexts_owner_id
    ON impresspress__messages__contexts (owner_id);
CREATE INDEX IF NOT EXISTS idx_messages_entries_owner_id
    ON impresspress__messages__entries (owner_id);
