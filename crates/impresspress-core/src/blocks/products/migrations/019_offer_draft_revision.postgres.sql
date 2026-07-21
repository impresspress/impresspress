-- Fence draft child-row replacement against concurrent publication. The offer
-- row is the only CAS point, but variables/components live in child tables:
-- update_draft advances draft_revision and raises draft_updating before it
-- touches children and settles draft_updating after, while publish refuses an
-- in-progress draft and CASes draft->active on the exact revision it
-- validated. A publish can therefore never capture a half-replaced child set,
-- and a completed update always forces publish to re-read a consistent one.
ALTER TABLE impresspress__products__offers
    ADD COLUMN IF NOT EXISTS draft_revision BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__offers
    ADD COLUMN IF NOT EXISTS draft_updating INTEGER NOT NULL DEFAULT 0;
