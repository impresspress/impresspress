-- The sandbox's own llms.txt, as the seed bundle carried it, and what of it
-- is in the published folder. See the SQLite variant for both columns.
ALTER TABLE impresspress__dev__seed_info
    ADD COLUMN IF NOT EXISTS llms_text TEXT;
ALTER TABLE impresspress__dev__seed_info
    ADD COLUMN IF NOT EXISTS llms_published_sha256 TEXT;
