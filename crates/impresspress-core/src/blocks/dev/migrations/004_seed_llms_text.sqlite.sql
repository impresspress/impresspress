-- The sandbox's own llms.txt, as the seed bundle carried it, and what of it
-- is in the published folder.
--
-- The static host serves that file at `/llms.txt` to a reader the service
-- worker does not control. Once the worker does, the runtime answers the
-- path, and it answers from the published site — so the publisher writes
-- this text there for as long as the site has no `llms.txt` of its own
-- (`publisher.rs`). `llms_text` is recorded beside the guide for the reason
-- the guide is: `seed::import` verified it against the manifest's hash.
--
-- `llms_text` is nullable and not back-filled HERE: a row an import wrote
-- before this column existed has none, and a migration cannot fetch. The
-- boot that follows does (`seed::repair_llms`): a sandbox row with no text
-- fetches it from the bundle the origin now serves.
--
-- `llms_published_sha256` is the publisher's own record: the hash of the
-- sandbox text that is in the published folder at `llms.txt` right now, or
-- NULL when none is. It is what a publish compares against — never "the
-- text is recorded, so it must have been published", which is false for
-- exactly the row the boot repair fills in.
ALTER TABLE impresspress__dev__seed_info
    ADD COLUMN llms_text TEXT;
ALTER TABLE impresspress__dev__seed_info
    ADD COLUMN llms_published_sha256 TEXT;
