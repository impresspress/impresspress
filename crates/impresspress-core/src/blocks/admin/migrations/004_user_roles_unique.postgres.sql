-- Make `impresspress__admin__user_roles` hold at most one grant per
-- (user_id, role), deleting rows that already repeat one and keeping the
-- earliest. The reasoning lives beside the constant that embeds this file, in
-- `migrations/mod.rs`; a shipped migration is hash-addressed over its whole
-- text, so prose here cannot be corrected.

DELETE FROM impresspress__admin__user_roles
WHERE EXISTS (
    SELECT 1
    FROM impresspress__admin__user_roles AS earlier
    WHERE earlier.user_id = impresspress__admin__user_roles.user_id
      AND earlier.role = impresspress__admin__user_roles.role
      AND (
          earlier.created_at < impresspress__admin__user_roles.created_at
          OR (
              earlier.created_at = impresspress__admin__user_roles.created_at
              AND earlier.id < impresspress__admin__user_roles.id
          )
      )
);

CREATE UNIQUE INDEX IF NOT EXISTS impresspress__admin__user_roles_user_role_uniq
    ON impresspress__admin__user_roles (user_id, role);
