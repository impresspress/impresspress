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
-- stripping `INTERNAL_FIELDS`, all four create/update paths forwarded the
-- request body verbatim, so any client sending `"deleted_at": ""` produced
-- exactly such a row.
--
-- This is a ONE-TIME repair of historical rows, not an ongoing guard. No new
-- `''` can be written: `soft_delete` writes `now_rfc3339()`, `restore` writes
-- SQL NULL, and every handler that forwards a caller-supplied body strips
-- `deleted_at` first — pinned end-to-end by
-- `no_handler_path_can_write_an_empty_deleted_at`. Re-running the statement
-- is harmless; after the first pass it matches nothing.
UPDATE impresspress__products__products
    SET deleted_at = NULL
    WHERE deleted_at = '';
