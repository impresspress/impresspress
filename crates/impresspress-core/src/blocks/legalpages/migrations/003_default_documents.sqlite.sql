-- The two default legal documents (SQLite / D1). Mirrored to
-- 003_default_documents.postgres.sql.
--
-- Until this file, `LegalPagesBlock::lifecycle(Init)` seeded them: it counted
-- the table and published both documents when the count was 0. Cloudflare
-- runs every block's Init on every cold isolate, so that `COUNT(*)` read every
-- document version once per cold isolate, for the life of the deployment
-- (about 57% of one account's daily D1 reads). The migration ledger runs this
-- file once per database instead, and Init reads nothing.
--
-- The guard is the WHOLE table, in one statement, exactly as the Init seed's
-- count was: a database holding any document, in any status, gets nothing,
-- so a type an admin deleted is not re-seeded. One statement, because with
-- two guarded inserts the second would see the first's row and skip. The
-- difference from the Init seed: deleting every document no longer makes the
-- next boot seed them again, because this file has already run.
--
-- The rows are what the Init seed published: status `published`, version 1
-- (the first number of an empty type), `created_by` `system`, the titles of
-- `DocumentType::title()` and the same text. The ids are fixed, which the
-- executor allows (it validates no id format; products' 002 uses `default`).
-- The three timestamps take the form the executor writes, `…+00:00`, without
-- fractional seconds. The admin document list (`repo::documents::list_page`)
-- sorts these rows with the executor's by `updated_at` as text, and a `…Z`
-- stamp sorts after every `+00:00` stamp of the same second, so an edit made
-- in the seed's second would list below it.
INSERT INTO impresspress__legalpages__documents
    (id, doc_type, title, content, status, version, created_by,
     published_at, created_at, updated_at)
SELECT seed.id, seed.doc_type, seed.title, seed.content, 'published', 1, 'system',
       seed.stamp, seed.stamp, seed.stamp
FROM (
    SELECT 'seed-terms' AS id, 'terms' AS doc_type, 'Terms of service' AS title,
           'These are the default terms of service. Please update them in the admin panel.
' AS content,
           strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00' AS stamp
    UNION ALL
    SELECT 'seed-privacy', 'privacy', 'Privacy policy',
           'This is the default privacy policy. Please update it in the admin panel.
',
           strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'
) AS seed
WHERE NOT EXISTS (SELECT 1 FROM impresspress__legalpages__documents);
