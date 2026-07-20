-- Stripe adds the buyer-selected shipping rate to Checkout's final total.
-- Persist it separately so order receipts remain itemized and reconciliation
-- can distinguish an allowed shipping charge from amount tampering.
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS shipping_cents BIGINT NOT NULL DEFAULT 0;
