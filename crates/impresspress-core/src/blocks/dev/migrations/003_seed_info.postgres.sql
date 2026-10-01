-- What the seed bundle said about this sandbox. See the SQLite variant for
-- why the row is seeded empty rather than inserted by code.
CREATE TABLE IF NOT EXISTS impresspress__dev__seed_info (
    singleton_id     INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    template         TEXT,
    suggested_prompt TEXT,
    guide_markdown   TEXT,
    imported_at      TEXT
);

INSERT INTO impresspress__dev__seed_info
    (singleton_id, template, suggested_prompt, guide_markdown, imported_at)
VALUES (1, NULL, NULL, NULL, NULL)
ON CONFLICT (singleton_id) DO NOTHING;
