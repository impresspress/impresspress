-- The two system roles, `admin` and `user` (PostgreSQL). Mirrors
-- 010_default_roles.sqlite.sql, which describes the whole-table guard and
-- the rows; only the timestamp expression differs.
INSERT INTO impresspress__admin__roles
    (id, name, description, permissions, is_system, created_at, updated_at)
SELECT seed.id, seed.name, seed.description, '[]', 1, seed.stamp, seed.stamp
FROM (
    SELECT 'seed-admin' AS id, 'admin' AS name,
           'Full access to all resources' AS description,
           to_char(now() AT TIME ZONE 'utc', 'YYYY-MM-DD"T"HH24:MI:SS') || '+00:00' AS stamp
    UNION ALL
    SELECT 'seed-user', 'user', 'Standard user access',
           to_char(now() AT TIME ZONE 'utc', 'YYYY-MM-DD"T"HH24:MI:SS') || '+00:00'
) AS seed
WHERE NOT EXISTS (SELECT 1 FROM impresspress__admin__roles);
