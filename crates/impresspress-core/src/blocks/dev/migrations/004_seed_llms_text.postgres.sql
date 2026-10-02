-- The sandbox's own llms.txt, as the seed bundle carried it. See the SQLite
-- variant for why it is recorded and why it is not back-filled.
ALTER TABLE impresspress__dev__seed_info
    ADD COLUMN IF NOT EXISTS llms_text TEXT;
