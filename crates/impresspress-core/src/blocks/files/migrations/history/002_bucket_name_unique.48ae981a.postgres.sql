-- Make `impresspress__files__buckets.name` unique (Postgres parity — untested,
-- see 001).
--
-- A bucket name is not just a label: it IS the blob-namespace folder name in
-- `wafer-run/storage` (`store::create_folder(name)`, `store::put(name, key)`),
-- and `repo::buckets::find_owned` grants access on the `(name, created_by)`
-- pair. Without a unique index a second user could insert a row for a name
-- someone else already held — `StorageService::create_folder` is idempotent on
-- every backend, so nothing refused the duplicate — and that row gave them
-- `find_owned` access to the first owner's folder: list it, read every object
-- in it, overwrite them, and on `DELETE /b/storage/api/buckets/{name}` wipe
-- the folder outright.
--
-- With this index the metadata row is the atomic claim on the name, which is
-- why `storage::buckets::handle_create_bucket` now inserts the row BEFORE it
-- creates the folder and answers 409 when the insert is refused. Creating the
-- folder first cannot be made safe: the idempotent `create_folder` succeeds
-- against the existing folder, and the compensating `delete_folder` that runs
-- when the metadata insert fails would then delete the first owner's data.
--
-- Rows that already collide are resolved in favour of the earliest creator
-- (`created_at`, `id` as the tie-break so the result does not depend on row
-- order): the later duplicates are deleted, which is exactly the access they
-- should never have had. Their object-metadata rows are left alone — the
-- blobs they name are real, still in the folder, and still charged to whoever
-- uploaded them; only the second claim on the folder goes away.

DELETE FROM impresspress__files__buckets
WHERE EXISTS (
    SELECT 1
    FROM impresspress__files__buckets AS earlier
    WHERE earlier.name = impresspress__files__buckets.name
      AND (
          earlier.created_at < impresspress__files__buckets.created_at
          OR (
              earlier.created_at = impresspress__files__buckets.created_at
              AND earlier.id < impresspress__files__buckets.id
          )
      )
);

CREATE UNIQUE INDEX IF NOT EXISTS impresspress__files__buckets_name_uniq
    ON impresspress__files__buckets (name);
