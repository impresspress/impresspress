-- An account's role is a grant row in `impresspress__admin__user_roles`, and
-- nothing else. See `016_roles_are_grants.sqlite.sql` and the 016 note in
-- `migrations/mod.rs`; this file is the PostgreSQL dialect of the same
-- change. It runs once, when 001's `role` column is there, so neither
-- statement says `IF [NOT] EXISTS`.
INSERT INTO impresspress__admin__user_roles
    (id, user_id, role, assigned_at, assigned_by, created_at, updated_at)
SELECT 'ur_' || md5(random()::text || u.id), u.id, u.role, u.created_at, '', u.created_at, u.created_at
  FROM wafer_run__auth__users u
 WHERE u.role <> ''
   AND NOT EXISTS (
       SELECT 1 FROM impresspress__admin__user_roles r
        WHERE r.user_id = u.id AND r.role = u.role
   );
ALTER TABLE wafer_run__auth__users DROP COLUMN role;
