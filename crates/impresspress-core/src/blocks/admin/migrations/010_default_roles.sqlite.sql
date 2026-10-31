-- The two system roles, `admin` and `user` (SQLite / D1). Mirrored to
-- 010_default_roles.postgres.sql. The reasoning lives beside the constant
-- that embeds this file, in `migrations/mod.rs`.
--
-- The guard is the WHOLE table, in one statement, exactly as the Init seed's
-- count was: a database holding any role gets nothing. The rows are what
-- that seed wrote: the descriptions it used, `permissions` `[]`, `is_system`
-- 1, fixed ids, and one stamp in the executor's `+00:00` form in both
-- timestamp columns.
INSERT INTO impresspress__admin__roles
    (id, name, description, permissions, is_system, created_at, updated_at)
SELECT seed.id, seed.name, seed.description, '[]', 1, seed.stamp, seed.stamp
FROM (
    SELECT 'seed-admin' AS id, 'admin' AS name,
           'Full access to all resources' AS description,
           strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00' AS stamp
    UNION ALL
    SELECT 'seed-user', 'user', 'Standard user access',
           strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'
) AS seed
WHERE NOT EXISTS (SELECT 1 FROM impresspress__admin__roles);
