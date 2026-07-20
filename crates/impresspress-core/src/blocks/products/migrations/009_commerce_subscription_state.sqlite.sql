-- Authoritative lifecycle state for subscriptions sold through commerce
-- offers.
ALTER TABLE impresspress__products__purchases ADD COLUMN subscription_status TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN subscription_current_period_end TEXT;
ALTER TABLE impresspress__products__purchases ADD COLUMN subscription_cancel_at_period_end INTEGER NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__purchases ADD COLUMN subscription_canceled_at TEXT;
ALTER TABLE impresspress__products__purchases ADD COLUMN subscription_last_synced_at TEXT;

CREATE INDEX IF NOT EXISTS impresspress__products__purchases_subscription_status_idx
    ON impresspress__products__purchases (subscription_status, subscription_current_period_end);
