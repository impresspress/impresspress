-- An account's role is a grant row in `impresspress__admin__user_roles`, and
-- nothing else: copy every inline `users.role` into a grant the account does
-- not already hold, then drop the column. Why, and why each statement is
-- shaped as it is, is in the 016 note in `migrations/mod.rs`.
--
-- The leading ADD COLUMN is what makes this file re-runnable. Every change
-- to the auth list re-runs it in full, and SQLite has no `DROP COLUMN IF
-- EXISTS`: on a database 016 already ran on, the ADD puts back an empty
-- column for the copy to find nothing in and the DROP to remove again. On a
-- database that still has 001's column, the ADD is a duplicate, which the
-- runner tolerates for an `ADD COLUMN` alone.
ALTER TABLE wafer_run__auth__users ADD COLUMN role TEXT NOT NULL DEFAULT '';
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
