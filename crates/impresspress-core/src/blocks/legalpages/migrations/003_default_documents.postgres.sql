-- The two default legal documents (PostgreSQL). Mirrors
-- 003_default_documents.sqlite.sql, which explains the whole-table guard,
-- the rows and the timestamp form; only the timestamp expression differs.
INSERT INTO impresspress__legalpages__documents
    (id, doc_type, title, content, status, version, created_by,
     published_at, created_at, updated_at)
SELECT seed.id, seed.doc_type, seed.title, seed.content, 'published', 1, 'system',
       seed.stamp, seed.stamp, seed.stamp
FROM (
    SELECT 'seed-terms' AS id, 'terms' AS doc_type, 'Terms of service' AS title,
           'These are the default terms of service. Please update them in the admin panel.
' AS content,
           to_char(now() AT TIME ZONE 'utc', 'YYYY-MM-DD"T"HH24:MI:SS') || '+00:00' AS stamp
    UNION ALL
    SELECT 'seed-privacy', 'privacy', 'Privacy policy',
           'This is the default privacy policy. Please update it in the admin panel.
',
           to_char(now() AT TIME ZONE 'utc', 'YYYY-MM-DD"T"HH24:MI:SS') || '+00:00'
) AS seed
WHERE NOT EXISTS (SELECT 1 FROM impresspress__legalpages__documents);
