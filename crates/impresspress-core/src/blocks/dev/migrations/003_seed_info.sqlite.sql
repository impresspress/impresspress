-- The single-row record of what the seed bundle said about this sandbox:
-- which template seeded it, the prompt the workspace page suggests, and the
-- site-authoring guide `dev_read_reference` serves as `site_markdown`.
--
-- Seeded at rest with every column NULL, the way `runtime_state` is seeded
-- idle: `seed::import` UPDATEs the row on the boot that seeds the instance,
-- and a NULL `template` is how the row says the seed carried no `sandbox`
-- block (an exported bundle never does). One row, never inserted by code.
CREATE TABLE IF NOT EXISTS impresspress__dev__seed_info (
    singleton_id     INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    template         TEXT,
    suggested_prompt TEXT,
    guide_markdown   TEXT,
    imported_at      TEXT
);

INSERT OR IGNORE INTO impresspress__dev__seed_info
    (singleton_id, template, suggested_prompt, guide_markdown, imported_at)
VALUES (1, NULL, NULL, NULL, NULL);
