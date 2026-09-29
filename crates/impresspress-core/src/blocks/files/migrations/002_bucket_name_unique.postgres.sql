-- Make `impresspress__files__buckets.name` unique, deleting rows that already
-- collide and keeping the earliest creator (Postgres parity — untested, see
-- 001). The reasoning lives beside the constant that embeds this file, in
-- `migrations/mod.rs`; a shipped migration is hash-addressed over its whole
-- text, so prose here cannot be corrected.

DELETE FROM impresspress__files__buckets
WHERE EXISTS (
    SELECT 1
    FROM impresspress__files__buckets AS earlier
    WHERE earlier.name = impresspress__files__buckets.name
      AND (
          earlier.created_at < impresspress__files__buckets.created_at
          OR (
              earlier.created_at = impresspress__files__buckets.created_at
              AND earlier.id < impresspress__files__buckets.id
          )
      )
);

CREATE UNIQUE INDEX IF NOT EXISTS impresspress__files__buckets_name_uniq
    ON impresspress__files__buckets (name);
