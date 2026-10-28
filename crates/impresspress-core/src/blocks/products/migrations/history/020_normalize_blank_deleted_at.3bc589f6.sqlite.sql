-- Normalize `deleted_at` to its two-value invariant: SQL NULL for a live
-- product, an RFC3339 stamp for the instant a product was soft-deleted. The
-- empty string is neither, and a row carrying it now reads as DELETED
-- everywhere.
--
-- Two predicates define "live" for this table and both side with SQL:
-- `repo::products::live_filter` (`deleted_at IS NULL`, appended to every list
-- and count read) and `repo::products::is_deleted` (its per-record twin,
-- `IS NOT NULL`). The partial unique slug index from migration 005 is keyed
-- on `deleted_at IS NULL` too. Since `'' IS NOT NULL`, an un-normalized row
-- disappears from the public catalog, the admin product list and the
-- per-seller product cap the moment those predicates agree — a live product
-- going offline with no admin action and nothing in the logs to explain it.
--
-- The state was reachable historically. Until the product handlers began
-- refusing a body that names an `UNSETTABLE_FIELDS` column, all four
-- create/update paths forwarded the request body verbatim, so any client
-- sending `"deleted_at": ""` produced exactly such a row.
--
-- Why the repair is guarded
-- -------------------------
-- 005's index is partial: `WHERE slug <> '' AND deleted_at IS NULL`. A
-- `deleted_at = ''` row therefore sits OUTSIDE it and never stopped another
-- product from claiming its `(owner_kind, owner_id, slug)` key — and
-- `owner_kind` / `owner_id` default to 'platform' / '', so every platform
-- product shares one key space. Repairing such a row to NULL pulls it back
-- INTO the index, against a slug that may since have been taken.
--
-- That failure is not survivable. `migration_helper::apply_if_blessed`
-- tolerates only a duplicate `ALTER ... ADD COLUMN`, so a unique violation
-- propagates, the migration hash is never stamped, and every later boot
-- re-runs and re-fails. On Cloudflare `builder::strict_init_all_blocks`
-- turns a block Init failure into `Err` and `IMPRESSPRESS_RUN_MIGRATIONS` is
-- baked into the deployment, so every request 500s until someone hand-edits
-- D1. Natively the tolerant `init_all_blocks` only logs, but the engine
-- rolls the whole statement back, so not one row gets repaired. The guard
-- therefore skips the rows it cannot make safe rather than losing the ones
-- it can.
--
-- The guard reads the same before and after the update: a claimant matches
-- `deleted_at IS NULL OR deleted_at = ''` whether or not this statement has
-- already repaired it, so the result does not depend on the order the engine
-- visits rows in. It covers both collision shapes — a blank row against a
-- live one, and two blank rows against each other, where repairing both
-- would collide them with one another. Empty slugs are exempt because the
-- index is partial on `slug <> ''`: any number of them coexist.
--
-- What an operator does about a skipped row
-- -----------------------------------------
-- A migration cannot log, and no UI reaches a soft-deleted product yet (the
-- admin "Deleted" view and its Restore button are a follow-up), so a skipped
-- row has to be found and repaired directly, e.g. from the admin SQL
-- explorer:
--
--   SELECT id, owner_kind, owner_id, slug
--   FROM impresspress__products__products WHERE deleted_at = '';
--
-- Rename whichever product should not hold the slug, then clear the column on
-- that one row:
--
--   UPDATE impresspress__products__products
--   SET deleted_at = NULL WHERE id = '<product id>';
--
-- Re-running this migration is not the remedy: once applied its hash is
-- stamped and `apply_if_blessed` short-circuits for good.
--
-- RELEASE.md's "Upgrade Notes" carries the operator-facing version of all of
-- this, including the reason an existing deployment must be upgraded with
-- `--run-migrations` rather than without.
--
-- This is a ONE-TIME repair of historical rows, not a standing enforcement of
-- the invariant. No new `''` can be written: `soft_delete` writes
-- `now_rfc3339()`, `restore` writes SQL NULL, and every handler that forwards
-- a caller-supplied body strips `deleted_at` first — pinned end-to-end by
-- `no_handler_path_can_write_an_empty_deleted_at`. Re-running the statement
-- is harmless: after the first pass the only rows left for it to match are
-- the ones it deliberately skipped, and it re-checks the same guard before
-- touching them.
UPDATE impresspress__products__products
    SET deleted_at = NULL
    WHERE deleted_at = ''
      AND (
          slug = ''
          OR NOT EXISTS (
              SELECT 1
              FROM impresspress__products__products AS claimant
              WHERE claimant.id <> impresspress__products__products.id
                AND claimant.owner_kind = impresspress__products__products.owner_kind
                AND claimant.owner_id = impresspress__products__products.owner_id
                AND claimant.slug = impresspress__products__products.slug
                AND (claimant.deleted_at IS NULL OR claimant.deleted_at = '')
          )
      );
