-- An account's role is a grant row in `impresspress__admin__user_roles`, and
-- nothing else: copy every inline `users.role` into a grant the account does
-- not already hold, then drop the column. Why, and why each statement is
-- shaped as it is, is in the 016 note in `migrations/mod.rs`.
--
-- The file runs once, recorded in the migration ledger. When it runs, 001's
-- `role` column is there, so a missing column means the database's record is
-- wrong, and the `DROP` fails rather than hides it.
INSERT INTO impresspress__admin__user_roles
    (id, user_id, role, assigned_at, assigned_by, created_at, updated_at)
SELECT 'ur_' || lower(hex(randomblob(16))), u.id, u.role, u.created_at, '', u.created_at, u.created_at
  FROM wafer_run__auth__users u
 WHERE u.role <> ''
   AND NOT EXISTS (
       SELECT 1 FROM impresspress__admin__user_roles r
        WHERE r.user_id = u.id AND r.role = u.role
   );
ALTER TABLE wafer_run__auth__users DROP COLUMN role;
