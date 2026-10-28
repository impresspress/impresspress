-- The sandbox's own llms.txt, as the seed bundle carried it.
--
-- The static host serves that file at `/llms.txt` to a reader the service
-- worker does not control. Once the worker does, the runtime answers the
-- path, and it answers from the published site — so the publisher writes
-- this text there for as long as the site has no `llms.txt` of its own
-- (`publisher.rs`). It is recorded here, beside the guide, for the reason the
-- guide is: `seed::import` verified it against the manifest's hash once, on
-- the boot that seeded the instance.
--
-- Nullable and not back-filled: a row an import wrote before this column
-- existed describes a seed that carried no such file, and NULL is what makes
-- the publisher leave that instance's site as it is.
ALTER TABLE impresspress__dev__seed_info
    ADD COLUMN llms_text TEXT;
