-- A reusable Stripe Payment Link is permanently scoped to the test or live
-- account in which it was created. Persist that immutable provider context.
ALTER TABLE impresspress__products__payment_links ADD COLUMN livemode INTEGER NOT NULL DEFAULT 0;
