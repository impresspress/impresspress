-- Order Stripe refund and Connect capability projections by the source event.
-- Stripe does not guarantee webhook delivery order, so older deliveries must
-- never regress a terminal refund or re-enable a restricted seller account.
ALTER TABLE impresspress__products__refunds
    ADD COLUMN IF NOT EXISTS stripe_event_created BIGINT NOT NULL DEFAULT 0;

ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN IF NOT EXISTS stripe_event_created BIGINT NOT NULL DEFAULT 0;
