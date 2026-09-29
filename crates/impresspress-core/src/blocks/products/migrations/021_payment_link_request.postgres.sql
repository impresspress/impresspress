-- The Stripe request a Payment Link attempt has in flight, recorded before
-- the attempt is sent. A deactivation of a row that never recorded a link id
-- re-sends exactly these parameters under the same idempotency key to learn
-- which link Stripe minted, so the link can be deactivated at Stripe too.
ALTER TABLE impresspress__products__payment_links
    ADD COLUMN IF NOT EXISTS stripe_request TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__payment_links
    ADD COLUMN IF NOT EXISTS stripe_request_at TEXT NOT NULL DEFAULT '';
